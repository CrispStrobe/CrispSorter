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

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .to_path_buf()
    }

    /// Every `.rs` file under `dir`, except this one — it necessarily contains
    /// the very identifiers it forbids.
    fn rust_sources_in(dir: PathBuf) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir];
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

    /// The app crate only. Use where the invariant is about what *CrispSorter*
    /// does, as opposed to what exists anywhere in the workspace.
    fn rust_sources_except_self() -> Vec<PathBuf> {
        rust_sources_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
    }

    /// Every Rust source in the workspace, not just `src-tauri/`.
    ///
    /// The watermark guard used to scan the app crate alone, which quietly
    /// assumed synthesis could only ever live there. The workspace has nine
    /// members; a future generation path in one of them would have been
    /// invisible — the same "scoped to the wrong tree" failure that let the
    /// AIToolkit panels ship unaudited (docs/ai-act.md § 5). Widened
    /// 2026-08-02.
    fn workspace_rust_sources() -> Vec<PathBuf> {
        let root = repo_root();
        let mut out = rust_sources_except_self();
        out.extend(rust_sources_in(root.join("crisp-index-server/src")));
        out.extend(rust_sources_in(root.join("crisp-index-protocol/src")));
        let Ok(entries) = std::fs::read_dir(root.join("crates")) else { return out };
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                out.extend(rust_sources_in(entry.path().join("src")));
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
        for path in workspace_rust_sources() {
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
        let repo = repo_root();
        let mut offenders = Vec::new();

        // Every workflow, not a hardcoded pair. Naming `ci.yml` and
        // `release.yml` made the guard silently blind to a third workflow —
        // and adding one is not the kind of change anybody would think to
        // re-audit. Enumerated 2026-08-02.
        let mut workflows: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(repo.join(".github/workflows")) {
            for entry in entries.flatten() {
                let path = entry.path();
                let ext = path.extension().and_then(|e| e.to_str());
                if matches!(ext, Some("yml") | Some("yaml")) {
                    workflows.push(path);
                }
            }
        }
        assert!(
            !workflows.is_empty(),
            "no workflows found — this guard would pass by scanning nothing"
        );

        for path in &workflows {
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            for (n, line) in text.lines().enumerate() {
                // The feature is *described* in comments on purpose; only an
                // actual --features list turning it on is a problem.
                let enables = line.contains("--features") || line.contains("tauri_args");
                if enables && line.contains("images-crisplens-identify") {
                    offenders.push(format!("{name}:{}: {}", n + 1, line.trim()));
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

    /// Annex III(1)(b): inferring age or gender from a face is biometric
    /// **categorisation**, a different limb from identification and one the
    /// feature guard above does not cover.
    ///
    /// `crisplens_protocol::Face` carries `estimated_age` and
    /// `estimated_gender`, and it arrives with the *parent* feature
    /// `images-crisplens` — which is legitimately allowed to be enabled,
    /// because the rest of that surface (settings, auth, watchfolders,
    /// semantic search) identifies nobody. So the boundary that matters is not
    /// "is the feature on" but "does CrispSorter ever read those fields".
    ///
    /// Today it reads neither. Deserialising a struct that happens to have the
    /// columns is not categorisation; surfacing them would be. Pinned
    /// 2026-08-02 so that stays a decision rather than a diff nobody flagged.
    #[test]
    fn no_inferred_biometric_attribute_is_ever_read() {
        let mut offenders = Vec::new();
        for path in rust_sources_except_self() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for needle in ["estimated_age", "estimated_gender"] {
                if text.contains(needle) {
                    offenders.push(format!("{}: {needle}", path.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "CrispSorter reads an inferred biometric attribute (age/gender). \
             That is Annex III(1)(b) biometric categorisation, not the \
             identification limb the feature guard covers — read \
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

    /// The client calls that return model-generated content. Calling any of
    /// them makes the calling view an Art 50 surface.
    ///
    /// **Two clients, not one.** The first five are `AIToolkitClient` methods —
    /// a remote HTTP backend. `llmClient.query(` is the *local* path
    /// (`src/lib/llm/client.ts` → `POST /chat/completions`), and it is the one
    /// the app actually leans on: chat answers, batch metadata, the Settings
    /// benchmark. It was missing from this list until the 2026-08-02 audit,
    /// which meant `Chat.svelte` — the flagship generative surface — matched no
    /// needle and was reviewed by nobody. Its badge and gate were correct, but
    /// by hand: deleting either would have passed CI.
    ///
    /// That is the § 5 lesson recurring one layer in. The first version of this
    /// guard was scoped to the wrong *tree* (Rust, not Svelte); this one was
    /// scoped to the wrong *client* (remote, not local). When adding a
    /// generative call anywhere, add its needle here first.
    const GENERATIVE_CALLS: [&str; 6] = [
        ".chat(",
        ".translate(",
        ".vision(",         // captioning — generated text about an image
        ".generateImage(",  // synthetic image
        ".tts(",            // synthetic audio, marked by the backend in-band
        "llmClient.query(", // the local LLM — chat, batch metadata, benchmark
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
            let calls: Vec<&str> = GENERATIVE_CALLS
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
    /// `GENERATIVE_CALLS` above so the disclosure guard covers it.
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
             call to GENERATIVE_CALLS and mark the surface. See \
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
                    .map(|t| GENERATIVE_CALLS.iter().any(|n| t.contains(n)))
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

        // "At least one view matches" is a weak floor: it was satisfied by the
        // AIToolkit panels for the whole time Chat.svelte matched nothing. Name
        // the surface that must always be covered, so the needles cannot drift
        // off the app's primary generative path while the guard still reports
        // success on a secondary one.
        let chat = views
            .iter()
            .find(|p| p.ends_with("Chat.svelte"))
            .expect("Chat.svelte is in the component tree");
        let chat_text = std::fs::read_to_string(chat).expect("read Chat.svelte");
        assert!(
            GENERATIVE_CALLS.iter().any(|n| chat_text.contains(n)),
            "Chat.svelte matches no generative needle. It is the app's main \
             generative surface, so either the local client was renamed (update \
             GENERATIVE_CALLS) or chat moved — and until then the disclosure \
             guard is not looking at it. See docs/ai-act.md § 5."
        );
    }

    /// The disclosure guard reads `.svelte` files, because a badge belongs on a
    /// view. But `llmClient.query(` is reachable from plain `.ts` too, and a
    /// generative call in a module has no badge to carry — so the guard above
    /// cannot express the invariant for those callers.
    ///
    /// So they are pinned by name instead. `batch/store.svelte.ts` drives batch
    /// metadata inference and its output is disclosed where it is *rendered*,
    /// in `BatchReview.svelte`. A new module-level caller has no such story
    /// until somebody writes one, which is the point of failing here.
    #[test]
    fn no_unreviewed_module_generates_text() {
        let root = repo_root().join("src");
        let reviewed = ["store.svelte.ts"];
        let mut offenders = Vec::new();
        let mut stack = vec![root];
        let mut scanned = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("ts") {
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // client.ts *defines* the method; it is not a caller.
                if name == "client.ts" {
                    continue;
                }
                scanned += 1;
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                if text.contains("llmClient.query(") && !reviewed.contains(&name) {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(scanned > 5, "the .ts scan found {scanned} files — too few to be real");
        assert!(
            offenders.is_empty(),
            "a module generates text and no audit has said where that output is \
             disclosed. Add the disclosure at the view that renders it, then add \
             the module here. See docs/ai-act.md § 2b:\n  {}",
            offenders.join("\n  ")
        );
    }
}
