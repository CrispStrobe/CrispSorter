//! HuggingFace file prefetcher that writes directly into the on-disk layout
//! that `hf_hub` (used by `fastembed`) reads from on cache hit.
//!
//! ## Why this exists
//!
//! `hf_hub 0.4.3` on Windows has a download path that, after the actual file
//! has been streamed to `…/blobs/<etag>`, tries to materialise it at
//! `…/snapshots/<commit>/<file>` via `symlink_or_rename`:
//!
//! 1. `std::os::windows::fs::symlink_file(rel_src, dst)` — fails for
//!    non-elevated users (Windows requires Developer Mode or admin).
//! 2. Fallback: `std::fs::rename(src, dst)`. The parent dir was created
//!    just before with `create_dir_all(...).ok()` — the `.ok()` swallows
//!    failures silently, after which `rename` blows up with
//!    `os error 3` (`Das System kann den angegebenen Pfad nicht finden`).
//!
//! Net effect: every model download via fastembed's native `try_new` path
//! fails on a default Windows install.
//!
//! ## Workaround
//!
//! We download the same files ourselves with `reqwest`, then place them
//! directly at the pointer path that `hf_hub::Cache::get(filename)` looks at:
//!
//!   `<cache>/models--<owner>--<name>/refs/<revision>` → `<commit_hash>`
//!   `<cache>/models--<owner>--<name>/snapshots/<commit_hash>/<file>`
//!
//! When fastembed then calls `ApiRepo::get(filename)`, the cache lookup
//! short-circuits the broken download flow.
//!
//! No symlinks, no rename gymnastics — just plain `create_dir_all` and
//! `tokio::fs::write`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use tokio::io::AsyncWriteExt;

/// Maximum number of redirects to follow on a single HF download.
const MAX_REDIRECTS: usize = 5;

/// Prefetch every file in `files` from `repo` (revision `main`) into the
/// `cache_dir` layout that `hf_hub::Cache` understands. Returns the map
/// `file → on-disk PathBuf`.
///
/// Files already present (size matches HF Content-Length) are skipped.
///
/// `progress_cb`, if provided, receives `(file, bytes_so_far, total_bytes)`
/// so the caller can drive a progress UI.
pub async fn prefetch_repo_files<F>(
    repo: &str,
    files: &[&str],
    cache_dir: &Path,
    mut progress_cb: F,
) -> Result<Vec<PathBuf>>
where
    F: FnMut(&str, u64, u64),
{
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("creating cache dir {}", cache_dir.display()))?;

    let safe_repo = format!("models--{}", repo.replace('/', "--"));
    let repo_dir = cache_dir.join(&safe_repo);
    let refs_dir = repo_dir.join("refs");
    std::fs::create_dir_all(&refs_dir)
        .with_context(|| format!("creating refs dir {}", refs_dir.display()))?;

    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none()) // we want to read x-repo-commit on redirects
        .build()
        .context("building reqwest client")?;

    let mut local_paths = Vec::with_capacity(files.len());
    let mut commit_hash: Option<String> = None;

    for file in files {
        let (resolved_url, file_commit, content_length) =
            resolve_file(&client, repo, "main", file).await.with_context(|| {
                format!("resolving https://huggingface.co/{repo}/resolve/main/{file}")
            })?;

        // Use the first non-empty commit hash; HF guarantees they are stable
        // across files in the same revision pull.
        if commit_hash.is_none() && !file_commit.is_empty() {
            commit_hash = Some(file_commit);
        }
        let commit = commit_hash.as_deref().unwrap_or("main");
        let snapshot_dir = repo_dir.join("snapshots").join(commit);

        let dst_path = snapshot_dir.join(file);
        if let Some(parent) = dst_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating snapshot subdir {}", parent.display()))?;
        }

        // Skip if already present at the right size.
        if let Ok(meta) = std::fs::metadata(&dst_path) {
            if meta.len() == content_length && content_length > 0 {
                progress_cb(file, content_length, content_length);
                local_paths.push(dst_path);
                continue;
            }
        }

        download_with_progress(&client, &resolved_url, &dst_path, content_length, |done, total| {
            progress_cb(file, done, total);
        })
        .await
        .with_context(|| format!("downloading {file}"))?;

        local_paths.push(dst_path);
    }

    // Write `refs/main` so hf-hub's cache lookup `Cache::get(filename)` resolves.
    if let Some(commit) = commit_hash {
        let ref_path = refs_dir.join("main");
        std::fs::write(&ref_path, commit.as_bytes())
            .with_context(|| format!("writing {}", ref_path.display()))?;
    }

    Ok(local_paths)
}

/// Resolve `https://huggingface.co/<repo>/resolve/<rev>/<file>` to its final
/// CDN URL by following redirects manually so we can read the
/// `X-Repo-Commit` header from the *first* response (only the HF endpoint
/// emits it; the CDN does not).
async fn resolve_file(
    client: &Client,
    repo: &str,
    revision: &str,
    file: &str,
) -> Result<(String, String, u64)> {
    let mut url = format!("https://huggingface.co/{repo}/resolve/{revision}/{file}");
    let mut commit_hash = String::new();
    let mut content_length: u64 = 0;

    for _ in 0..MAX_REDIRECTS {
        // HEAD with Accept-Encoding: identity so HF returns the real Content-Length.
        let resp = client
            .head(&url)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await?;

        if commit_hash.is_empty() {
            if let Some(c) = resp.headers().get("x-repo-commit") {
                if let Ok(s) = c.to_str() {
                    commit_hash = s.to_owned();
                }
            }
        }

        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect with no Location header"))?
                .to_owned();
            url = if loc.starts_with("http://") || loc.starts_with("https://") {
                loc
            } else if loc.starts_with('/') {
                let base = reqwest::Url::parse(&url)?;
                format!("{}://{}{}", base.scheme(), base.host_str().unwrap_or(""), loc)
            } else {
                let base = reqwest::Url::parse(&url)?;
                base.join(&loc)?.to_string()
            };
            continue;
        }

        if !status.is_success() {
            return Err(anyhow!("HEAD {url} returned HTTP {status}"));
        }

        if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
            if let Ok(s) = cl.to_str() {
                content_length = s.parse().unwrap_or(0);
            }
        }

        return Ok((url, commit_hash, content_length));
    }

    Err(anyhow!("too many redirects resolving {repo}/{file}"))
}

async fn download_with_progress<F>(
    client: &Client,
    url: &str,
    dst: &Path,
    expected_size: u64,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("GET {url} returned HTTP {}", resp.status()));
    }

    let total = resp
        .content_length()
        .unwrap_or(expected_size);

    let tmp = dst.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;

    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        progress(downloaded, total);
    }
    file.flush().await?;
    drop(file);

    // Atomic rename within the same directory — both paths exist there.
    std::fs::rename(&tmp, dst).with_context(|| {
        format!("renaming {} -> {}", tmp.display(), dst.display())
    })?;
    Ok(())
}
