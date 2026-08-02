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

    /// Every `.svelte` file under the frontend `src/`.
    fn svelte_sources() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .join("src");
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("svelte") {
                    out.push(path);
                }
            }
        }
        out
    }

    /// The methods on `AIToolkitClient` that return model-generated content.
    /// Calling any of them makes the calling view an Art 50 surface.
    const GENERATIVE_CLIENT_CALLS: [&str; 5] = [
        ".chat(",
        ".translate(",
        ".vision(",       // captioning — generated text about an image
        ".generateImage(", // synthetic image
        ".tts(",           // synthetic audio, NOT watermarked (remote backend)
    ];

    /// Art 50: a view that generates content must disclose it and must satisfy
    /// the intended-purpose gate.
    ///
    /// The Rust-only guard above cannot see this class of surface at all: the
    /// AIToolkit panels are TypeScript calling a remote HTTP backend, so no
    /// Rust identifier appears and no Rust command is involved. That is exactly
    /// how a fully wired image-generation and (unwatermarked) speech-synthesis
    /// surface shipped while `docs/ai-act.md` recorded both as absent — found
    /// in the 2026-08-01 audit.
    ///
    /// If you are here because this test failed: you added a generative call to
    /// a view that carries neither `AiGeneratedBadge` nor `IntendedPurposeGate`.
    /// Add both — do not add the file to an exemption list.
    #[test]
    fn every_generative_frontend_surface_discloses_and_gates() {
        let mut offenders = Vec::new();
        for path in svelte_sources() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let calls: Vec<&str> = GENERATIVE_CLIENT_CALLS
                .iter()
                .copied()
                .filter(|needle| text.contains(needle))
                .collect();
            if calls.is_empty() {
                continue;
            }
            let mut missing = Vec::new();
            if !text.contains("AiGeneratedBadge") {
                missing.push("AiGeneratedBadge");
            }
            if !text.contains("IntendedPurposeGate") {
                missing.push("IntendedPurposeGate");
            }
            if !missing.is_empty() {
                offenders.push(format!(
                    "{}: generates via {} but lacks {}",
                    path.display(),
                    calls.join(", "),
                    missing.join(" + ")
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "an undisclosed generative surface reached the frontend — see \
             docs/ai-act.md before changing this test:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Art 50(2): a marked artifact is only marked if the client actually uses
    /// the marked copy.
    ///
    /// The AIToolkit backend marks every generated image and returns the marked
    /// bytes in `b64_json`, keeping the provider's ORIGINAL (unmarked) `url`
    /// alongside it — explicitly "so the client never has to be trusted to mark
    /// on download". CrispSorter then did `img?.url ?? img?.b64_json`, which
    /// prefers the unmarked original and throws the marking away. The output was
    /// unmarked in the one place it counts: the file the user saves.
    ///
    /// Nothing about that reads as a compliance bug at review time — it looks
    /// like ordinary "prefer a URL over a data blob". So it is pinned: the image
    /// source must come from `markedImageSrc`, which encodes the preference once.
    #[test]
    fn generated_images_are_taken_from_the_marked_copy() {
        let mut offenders = Vec::new();
        for path in svelte_sources() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if !text.contains(".generateImage(") {
                continue;
            }
            if !text.contains("markedImageSrc") {
                offenders.push(format!(
                    "{}: generates images without markedImageSrc",
                    path.display()
                ));
            }
            // The specific shape that caused this: reaching for `.url` first.
            if text.contains("?.url ??") || text.contains(".url ?? ") {
                offenders.push(format!(
                    "{}: prefers the unmarked provider url over the marked b64_json",
                    path.display()
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "a generated image is being shown from the unmarked source — see \
             docs/ai-act.md § 5:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// A new backend endpoint is how the *next* unaudited generative surface
    /// arrives — the client gains a method, a view calls it, and nothing in the
    /// tree records whether its output is generated or merely rendered.
    ///
    /// So the client's endpoint list is pinned. Adding one fails here until
    /// somebody classifies it and, if it generates content, adds its call to
    /// `GENERATIVE_CLIENT_CALLS` above so the disclosure guard covers it.
    #[test]
    fn the_aitoolkit_client_exposes_no_unclassified_endpoint() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .join("src/lib/aitoolkit.ts");
        let text = std::fs::read_to_string(&path).expect("read src/lib/aitoolkit.ts");

        // Endpoints reviewed on 2026-08-01, and what each returns.
        let reviewed = [
            "/api/health",             // status — not content
            "/api/config",             // capability probe — not content
            "/api/auth/login",         // auth — not content
            "/api/providers",          // capability probe — not content
            "/api/extract",            // renders text already in the file
            "/api/ocr",                // renders pixels
            "/api/transcription/sync", // renders real audio
            "/api/chat/completions",   // GENERATES text
            "/api/translate/text",     // GENERATES text
            "/api/vision/analyze",     // GENERATES text (captioning)
            "/api/images/generate",    // GENERATES images
            "/api/tts/synthesize",     // GENERATES audio (no watermark)
        ];

        let mut unknown = Vec::new();
        for (idx, _) in text.match_indices("/api/") {
            let rest = &text[idx..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "/_-.".contains(c)))
                .unwrap_or(rest.len());
            let endpoint = &rest[..end];
            if !reviewed.contains(&endpoint) && !unknown.contains(&endpoint.to_string()) {
                unknown.push(endpoint.to_string());
            }
        }

        assert!(
            unknown.is_empty(),
            "the AIToolkit client reaches an endpoint no audit has classified. \
             Decide whether it returns generated content; if it does, add its \
             call to GENERATIVE_CLIENT_CALLS and mark the surface. See \
             docs/ai-act.md:\n  {}",
            unknown.join("\n  ")
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

        // Same for the frontend scan. A moved or renamed `src/` would make the
        // disclosure guard pass by finding nothing at all.
        let views = svelte_sources();
        assert!(
            views.len() > 15,
            "expected the frontend component tree, found {} .svelte files",
            views.len()
        );
        assert!(
            views
                .iter()
                .any(|p| p.ends_with("AIToolkitCapability.svelte")),
            "AIToolkitCapability.svelte is the surface that generates images and \
             unwatermarked speech; if it is not in the scan the guard is not \
             guarding anything"
        );
        assert!(
            views.iter().any(|p| {
                std::fs::read_to_string(p)
                    .map(|t| GENERATIVE_CLIENT_CALLS.iter().any(|n| t.contains(n)))
                    .unwrap_or(false)
            }),
            "no view matched any generative call — the needles have drifted from \
             the client's method names and the guard now passes vacuously"
        );

        // `generated_images_are_taken_from_the_marked_copy` only inspects files
        // containing `.generateImage(`. If that string stops appearing — renamed
        // method, moved call — the guard reviews nothing and reports success,
        // which is precisely the state that let the unmarked-url bug ship.
        assert!(
            views.iter().any(|p| {
                std::fs::read_to_string(p)
                    .map(|t| t.contains(".generateImage("))
                    .unwrap_or(false)
            }),
            "no view calls .generateImage() — the marked-image guard is inspecting \
             nothing; re-point it at wherever image generation moved"
        );
    }
}
