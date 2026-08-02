//! What this build can actually do — PLAN P36.5.
//!
//! ## The problem this replaces
//!
//! `src/lib/platform.ts` decided what to render by sniffing
//! `navigator.userAgent`. That was wrong in three ways at once:
//!
//! * It answered the wrong question. "Am I on iOS?" is a proxy for "was
//!   OCR compiled into this binary?", and the proxy had already come
//!   apart: the iOS release job passes **no** `--features`, so the OCR and
//!   Translate tabs rendered with nothing behind them. App Review
//!   Guideline 2.1 (App Completeness) is the most common rejection there
//!   is, and a dead tab is exactly what it names.
//! * It could not see the new axis at all. `desktop-mas` is desktop by
//!   every user-agent measure and yet has no sidecar providers, no
//!   Tesseract, no `lp`. A UA sniff has nothing to say about that.
//! * iPadOS reports a *Macintosh* user agent, so the sniff needed a
//!   `maxTouchPoints` heuristic to guess at its own question.
//!
//! ## What replaces it
//!
//! One command that reports `cfg!` flags. The frontend feeds the result
//! into the `requires:` mechanism `tabs.ts` already has for the AIToolkit
//! panels, so UI truth derives from what was compiled rather than from
//! what the browser claims to be. That is the only version of this that
//! cannot drift: adding a feature gate to a module and forgetting to
//! update the UI now shows up as a missing tab, not a broken one.
//!
//! ## Compile-time facts, with one deliberate exception
//!
//! Every field is a `cfg!` except [`ocr_tiers`], which goes through each
//! tier's own availability predicate. Those are mostly `cfg!` too, but
//! `ocrs` genuinely checks for its `.rten` model files on disk: it is
//! compiled unconditionally (pure Rust, no feature flag) and useless
//! without weights. A tier that is compiled in but cannot run is not a
//! capability the user has, and reporting it would put the OCR tab back
//! in exactly the dead state this exists to prevent.

use serde::{Deserialize, Serialize};

/// The build's capability report. Serialised to the frontend as-is.
///
/// Adding a field here is cheap and is the intended way to gate new UI:
/// add the `cfg!`, add a `build:<name>` flag below, then put
/// `requires: ['build:<name>']` on the tab or control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    /// `macos` / `windows` / `linux` / `ios` / `android`. Diagnostics and
    /// the few genuinely platform-shaped decisions (the macOS share
    /// sheet), not a stand-in for feature detection.
    pub platform: String,
    /// A touch-first build. Compiled from the target, not guessed from a
    /// user agent — which is the whole point.
    pub mobile: bool,
    /// The desktop surface (watchers, feeds, local model host) is present.
    pub desktop: bool,
    /// This build may spawn helper processes. False for `desktop-mas` and
    /// for mobile; the single flag behind every sandbox difference.
    pub sidecars: bool,
    /// Developer-only routes are compiled in. Never true in a release
    /// artifact — see `compliance.rs`.
    pub dev_tools: bool,

    // ── Inference ──────────────────────────────────────────────────────
    /// In-process LLM inference (mistral.rs). Independent of `sidecars`:
    /// this is linked into the binary, which is why a sandboxed build
    /// keeps local inference.
    pub local_llm: bool,
    /// The app can *start* llama.cpp / MLX / Ollama for the user. Without
    /// it those providers still work — they just have to be running
    /// already, and the UI must say so instead of offering a button.
    pub launch_local_servers: bool,

    // ── Document capabilities ──────────────────────────────────────────
    /// At least one OCR tier is compiled in *and* has its models.
    pub ocr: bool,
    /// Which tiers, most capable first. Surfaced so Settings can explain
    /// what will actually run rather than just that something might.
    pub ocr_tiers: Vec<String>,
    /// Scanned-PDF rasterisation (pdfium). Without it, PDF OCR falls back
    /// to whole-file Tesseract, which a sandboxed build also lacks.
    pub pdf_render: bool,
    /// Password decryption, xref repair and linearisation (zpdf).
    pub pdf_zpdf: bool,

    // ── Speech ─────────────────────────────────────────────────────────
    /// On-device speech recognition (CrispASR).
    pub asr: bool,
    /// Text-to-speech is reachable at all, by either path.
    pub tts: bool,
    /// Speech is synthesised in-process and watermarked (CrispASR) rather
    /// than by the platform synth. The sandboxed path — and the compliant
    /// one, since `say` / SAPI emit unmarked audio (AI Act Art 50(2)).
    pub tts_watermarked: bool,

    // ── Translation ────────────────────────────────────────────────────
    /// Format-preserving translation (SimAlign over source ↔ target runs).
    pub translate_align: bool,
    /// Offline neural MT, no network needed.
    pub translate_nmt: bool,

    // ── Storage ────────────────────────────────────────────────────────
    /// Native Rust Filen client, rather than the Python CLI.
    pub drive_filen_native: bool,
    /// Native Rust Internxt client, rather than the Python CLI.
    pub drive_internxt_native: bool,
    /// The Python-CLI drives can run here. Requires `sidecars`, and is
    /// redundant wherever the matching native client is on.
    pub drive_subprocess: bool,
    /// FUSE mounting. Impossible under App Sandbox regardless of build.
    pub fuse: bool,

    // ── Media ──────────────────────────────────────────────────────────
    /// In-process audio codec suite (glint) as a decode tier.
    pub audio_glint: bool,
    /// The ffmpeg shell-out tier for the container long tail.
    pub audio_ffmpeg: bool,

    // ── Platform integration ───────────────────────────────────────────
    /// Direct printing without leaving the app.
    pub direct_print: bool,
    /// A real system share sheet.
    pub share_sheet: bool,

    /// Flat `build:*` keys for the `requires:` mechanism in `tabs.ts`.
    /// Namespaced so they cannot collide with the `service:*` keys the
    /// AIToolkit backend contributes to the same capability set.
    pub flags: Vec<String>,
}

/// Which OCR tiers this build can actually run, most capable first.
///
/// "Can actually run" is the operative phrase, and it is why each tier is
/// asked its own predicate rather than read off a `cfg!` here. Today
/// CrispEmbed and PaddleOCR answer from their feature flag while `ocrs`
/// checks for its `.rten` files on disk — but the predicate is the tier's
/// to define, and a tier that later grows a runtime requirement should
/// start reporting it without this function changing.
///
/// On iOS, where the release job passes no `--features` at all, every one
/// of these is false. That is the fix: the OCR tab stops rendering rather
/// than rendering with nothing behind it.
fn ocr_tiers() -> Vec<String> {
    let mut tiers = Vec::new();
    if crate::extractors::ocr_crispembed::is_crispembed_ocr_available() {
        tiers.push("crispembed".to_string());
    }
    if crate::extractors::ocr_paddle::is_paddle_ocr_available() {
        tiers.push("paddle".to_string());
    }
    if crate::extractors::ocr_ocrs::is_ocrs_available() {
        tiers.push("ocrs".to_string());
    }
    // Already false without `sidecars` — the function reports the tier as
    // absent rather than merely uninstalled, because this build could not
    // run the binary either way.
    if crate::extractors::ocr::is_tesseract_installed() {
        tiers.push("tesseract".to_string());
    }
    tiers
}

/// Build the report for this binary, on this machine, right now.
pub fn capabilities() -> Capabilities {
    let sidecars = cfg!(feature = "sidecars");
    let asr = cfg!(feature = "crispasr");
    let tiers = ocr_tiers();

    // Local LLM inference is `mistralrs`, which arrives with `desktop` and
    // only on the three desktop targets it has a backend for.
    let local_llm = cfg!(all(
        feature = "desktop",
        any(target_os = "macos", target_os = "windows", target_os = "linux")
    ));

    let platform_caps = crate::platform_share::capabilities();

    let mut caps = Capabilities {
        platform: std::env::consts::OS.to_string(),
        mobile: cfg!(any(target_os = "ios", target_os = "android")),
        desktop: cfg!(feature = "desktop"),
        sidecars,
        dev_tools: cfg!(feature = "dev-tools"),

        local_llm,
        launch_local_servers: sidecars,

        ocr: !tiers.is_empty(),
        ocr_tiers: tiers,
        pdf_render: cfg!(feature = "pdf-render"),
        pdf_zpdf: cfg!(feature = "pdf-zpdf"),

        asr,
        // Either the platform synth (spawned) or CrispASR (linked in).
        tts: sidecars || asr,
        // Only CrispASR marks its output. When both are available the
        // spawned synth wins for latency, so this is specifically "the
        // path this build will take is the marked one".
        tts_watermarked: !sidecars && asr,

        translate_align: cfg!(feature = "translate-align"),
        translate_nmt: cfg!(feature = "translate-nmt"),

        drive_filen_native: cfg!(feature = "drive-filen-native"),
        drive_internxt_native: cfg!(feature = "drive-internxt-native"),
        drive_subprocess: sidecars,
        fuse: cfg!(feature = "fuse"),

        audio_glint: cfg!(feature = "audio-glint"),
        audio_ffmpeg: sidecars,

        direct_print: platform_caps.direct_print,
        share_sheet: platform_caps.system_share_sheet,

        flags: Vec::new(),
    };
    caps.flags = caps.derive_flags();
    caps
}

impl Capabilities {
    /// Flatten the booleans into `build:*` keys.
    ///
    /// Kept as a method over the already-built struct rather than assembled
    /// alongside it: one source of truth means a field and its flag cannot
    /// disagree, which is the failure this whole mechanism exists to stop.
    fn derive_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();
        let mut add = |name: &str, on: bool| {
            if on {
                flags.push(format!("build:{name}"));
            }
        };
        add("mobile", self.mobile);
        add("desktop", self.desktop);
        add("sidecars", self.sidecars);
        add("dev-tools", self.dev_tools);
        add("local-llm", self.local_llm);
        add("launch-local-servers", self.launch_local_servers);
        add("ocr", self.ocr);
        add("pdf-render", self.pdf_render);
        add("pdf-zpdf", self.pdf_zpdf);
        add("asr", self.asr);
        add("tts", self.tts);
        add("translate-align", self.translate_align);
        add("translate-nmt", self.translate_nmt);
        add("drive-filen", self.drive_filen_native || self.drive_subprocess);
        add(
            "drive-internxt",
            self.drive_internxt_native || self.drive_subprocess,
        );
        add("fuse", self.fuse);
        add("direct-print", self.direct_print);
        add("share-sheet", self.share_sheet);
        flags
    }
}

pub mod tauri_commands {
    use super::*;

    /// What this build can do. Called once on startup; the frontend keeps
    /// the result in a store and gates every conditional surface on it.
    #[tauri::command]
    pub async fn build_capabilities() -> Result<Capabilities, String> {
        Ok(capabilities())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_matches_this_build() {
        let c = capabilities();
        assert_eq!(c.platform, std::env::consts::OS);
        assert_eq!(c.sidecars, cfg!(feature = "sidecars"));
        assert_eq!(c.desktop, cfg!(feature = "desktop"));
        assert_eq!(c.mobile, cfg!(any(target_os = "ios", target_os = "android")));
    }

    /// The invariant the whole milestone rests on: `desktop-mas` differs
    /// from `desktop` by the ability to spawn, and by nothing else that
    /// matters to the user.
    #[test]
    fn spawning_and_only_spawning_follows_the_sidecars_flag() {
        let c = capabilities();
        assert_eq!(c.launch_local_servers, c.sidecars);
        assert_eq!(c.audio_ffmpeg, c.sidecars);
        assert_eq!(c.drive_subprocess, c.sidecars);
        if !c.sidecars {
            assert!(
                !c.ocr_tiers.iter().any(|t| t == "tesseract"),
                "Tesseract is a shell-out; a build that cannot spawn must not list it"
            );
            assert!(!c.direct_print, "printing is `lp` / the Print verb, both spawns");
        }
    }

    /// Local inference must survive the sandbox. If this ever fails, the
    /// premise of PLAN P36 has broken — mistral.rs is linked in, not
    /// spawned, and that is the finding the whole plan is sized around.
    #[test]
    fn local_inference_does_not_depend_on_spawning() {
        if cfg!(all(
            feature = "desktop",
            any(target_os = "macos", target_os = "windows", target_os = "linux")
        )) {
            assert!(
                capabilities().local_llm,
                "in-process LLM inference is compiled in and must be reported \
                 regardless of the `sidecars` flag"
            );
        }
    }

    /// A tab gated on a flag that is never emitted would be invisible on
    /// every build — the same dead-surface bug in the other direction.
    #[test]
    fn every_flag_is_derived_from_a_field_that_is_true() {
        let c = capabilities();
        for flag in &c.flags {
            assert!(flag.starts_with("build:"), "unnamespaced flag: {flag}");
        }
        assert_eq!(
            c.flags.iter().any(|f| f == "build:sidecars"),
            c.sidecars,
            "the flag and the field disagree"
        );
        assert_eq!(c.flags.iter().any(|f| f == "build:ocr"), c.ocr);
        // Flags are a set, not a bag — a duplicate would mean two fields
        // are both claiming the same UI gate.
        let mut sorted = c.flags.clone();
        sorted.sort();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "duplicate flag in {:?}", c.flags);
    }

    #[test]
    fn ocr_is_reported_only_when_a_tier_can_really_run() {
        let c = capabilities();
        assert_eq!(c.ocr, !c.ocr_tiers.is_empty());
    }

    /// Speech that this build cannot produce must not be advertised, and
    /// the marked path must be the one a sandboxed build takes.
    #[test]
    fn tts_reporting_tracks_the_path_that_will_actually_run() {
        let c = capabilities();
        assert_eq!(c.tts, c.sidecars || c.asr);
        assert_eq!(c.tts_watermarked, !c.sidecars && c.asr);
        if c.tts_watermarked {
            assert!(c.tts, "a watermarked path is still a path");
        }
    }
}
