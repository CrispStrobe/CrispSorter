//! Compliance invariants that are cheaper to enforce than to re-audit.
//!
//! Nothing here runs at runtime. These are source-level guards for properties
//! that an audit established once and that a later change could silently
//! reverse — the class of regression that no functional test notices because
//! the code still works, it just stops being compliant.
//!
//! See `docs/ai-act.md` for what each invariant is protecting and why.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Every `.rs` file under `src/`, except this one — it necessarily contains
    /// the very identifiers it forbids.
    fn rust_sources_except_self() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                    && path.file_name().and_then(|n| n.to_str()) != Some("compliance.rs")
                {
                    out.push(path);
                }
            }
        }
        out
    }

    /// AI Act Art 50(2): synthesised audio must carry a machine-readable mark.
    ///
    /// CrispASR gives us this for free *by default* — `Session::synthesize`
    /// watermarks, while `synthesize_raw` emits unmarked audio and refuses
    /// unless `accept_marking_responsibility()` was called first. So the
    /// compliant state is simply "we never reach for the raw path", and the
    /// failure mode is a future caller adding one line to silence that refusal
    /// without realising what the refusal was for.
    ///
    /// If you are here because this test failed: taking marking responsibility
    /// is a real legal position, not a build fix. Read `docs/ai-act.md` first,
    /// and if the decision is genuinely to emit unmarked audio, record who
    /// decided it and why — then update this test deliberately.
    #[test]
    fn tts_never_bypasses_the_synthetic_audio_watermark() {
        let mut offenders = Vec::new();
        for path in rust_sources_except_self() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for needle in ["synthesize_raw", "accept_marking_responsibility"] {
                if text.contains(needle) {
                    offenders.push(format!("{}: {needle}", path.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "unmarked speech synthesis reached the tree — see docs/ai-act.md \
             before changing this test:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// "Research only, must never ship" is a policy until something enforces it.
    ///
    /// `images-crisplens-identify` turns on 1:N face identification — asking
    /// CrispLens *who* is in a picture, by matching against a gallery of N
    /// identities. That is the Annex III(1) reading we do not want to argue
    /// with, so no build recipe may name the feature: not `default`, not a CI
    /// or release `--features` string, not a bundle config.
    ///
    /// Scanning the workflows is crude but it catches the realistic mistake —
    /// somebody appending the feature to a `--features` line while chasing a
    /// build error, months from now, without reading Cargo.toml's warning.
    #[test]
    fn no_build_recipe_enables_face_identification() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .to_path_buf();
        let mut offenders = Vec::new();

        for wf in ["ci.yml", "release.yml"] {
            let path = repo.join(".github/workflows").join(wf);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for (n, line) in text.lines().enumerate() {
                // The feature is *described* in comments on purpose; only an
                // actual --features list turning it on is a problem.
                let enables = line.contains("--features") || line.contains("tauri_args");
                if enables && line.contains("images-crisplens-identify") {
                    offenders.push(format!("{wf}:{}: {}", n + 1, line.trim()));
                }
            }
        }

        // And it must not be a default feature of the app.
        let manifest = std::fs::read_to_string(repo.join("src-tauri/Cargo.toml"))
            .expect("read src-tauri/Cargo.toml");
        for line in manifest.lines() {
            let l = line.trim();
            if l.starts_with("default") && l.contains("images-crisplens-identify") {
                offenders.push(format!("Cargo.toml default: {l}"));
            }
        }

        assert!(
            offenders.is_empty(),
            "a build recipe enables 1:N face identification — read \
             docs/ai-act.md before changing this test:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// The scan has to actually be looking at files, or the assertion above is
    /// vacuously true and would keep passing after a refactor moved `src/`.
    #[test]
    fn the_source_scan_finds_the_tree_it_is_meant_to_guard() {
        let sources = rust_sources_except_self();
        assert!(
            sources.len() > 50,
            "expected the crate's source tree, found {} files — the guard above \
             would pass for the wrong reason",
            sources.len()
        );
        assert!(
            sources.iter().any(|p| p.ends_with("asr/mod.rs")),
            "asr/mod.rs is where synthesis lives; if it is not in the scan the \
             guard is not guarding anything"
        );
    }
}
