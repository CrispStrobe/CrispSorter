//! Intended-purpose acknowledgement — the notice that makes Art 25(1)(c) bite.
//!
//! # Why a gate rather than a line in the README
//!
//! The EU AI Act places obligations on providers (Art 16) and deployers
//! (Art 26) by operation of law. A user promising to comply transfers nothing
//! and discharges nothing, so a click-through is worthless as liability
//! transfer. What it *is* good for is **notice**: Art 3(12) lets the provider
//! define the intended purpose, and Art 25(1)(c) makes a deployer who
//! repurposes the system into a high-risk use the *provider* of a high-risk
//! system. That only works cleanly if the intended purpose was stated
//! unambiguously — which is what [`STATEMENT`] is for.
//!
//! Making acknowledgement a precondition for producing output means the output
//! itself carries the notice: an artifact exists ⟹ the statement was shown and
//! acknowledged. That is stronger than a consent record nobody can produce.
//!
//! # Where that inference stops — read this before relying on it
//!
//! CrispSorter is AGPL-3.0-or-later and its source is public, so **anyone can
//! build a copy with this gate removed**. "Output exists" therefore implies
//! acknowledgement *for builds we publish*, and proves nothing about an
//! arbitrary third-party build. The gate is good evidence about our own
//! distribution and not a proof about the world. That is exactly why the
//! acknowledgement is *also* recorded on disk with a version and a timestamp
//! (see [`Record`]) instead of relying on the inference alone.
//!
//! # What is gated, and what deliberately is not
//!
//! Gated in Rust, at the four commands that produce AI output or act on it:
//! `execute_batch` (applies suggestions, moving the user's files), `tts_speak`,
//! `translate_text`, `translate_docx`.
//!
//! **Chat is gated in the frontend, not here, and that is not an oversight.**
//! Chat completions never reach Rust: the `deep-chat` component talks to the
//! provider directly, and `Chat.svelte` invokes only `tts_speak` / `tts_stop` /
//! `asr_transcribe`. A Rust gate would therefore be decorative. The frontend
//! blocks the composer until `intended_purpose_status` reports acknowledged.
//! If chat ever moves behind a Rust command, gate it here too.
//!
//! Not gated: reading, indexing, searching, OCR, and every read-only command.
//! Blocking those would punish ordinary use to no benefit — the notice is about
//! what the system produces, not about opening it. `--version` and `doctor` in
//! particular must work on a fresh install so support questions stay answerable.

use std::path::{Path, PathBuf};

/// Bump when [`STATEMENT`] changes **materially**. A stale acknowledgement is
/// consent to a text nobody read, so a bump re-prompts. Do not bump for typos.
pub const STATEMENT_VERSION: u32 = 1;

const ENV: &str = "CRISPSORTER_ACCEPT_INTENDED_PURPOSE";
const FILE: &str = "intended-purpose-ack.json";

/// The intended purpose, and the exclusions that make repurposing identifiable.
///
/// Specific on purpose. "Comply with applicable law" is an exhortation nobody
/// can act on; naming the excluded uses gives both the user something concrete
/// and us a clean factual baseline if someone steps outside it.
pub const STATEMENT: &str = "\
CrispSorter is intended for organising, converting, transcribing, translating \
and searching documents and media that you are entitled to process — your own \
files, or files you have permission to handle.

It runs AI models locally. Their output is suggestions, not findings: titles, \
authors, dates, folder placements, transcripts, translations and chat answers \
can all be wrong, and you remain responsible for checking anything that \
matters.

It is NOT intended for, and must not be used for:
  · screening job applicants, or any employment or promotion decision;
  · assessing creditworthiness or eligibility for benefits or services;
  · evaluating students, exams, or admissions;
  · law-enforcement, migration, border or asylum purposes;
  · inferring identity, emotions or protected characteristics of people;
  · any use requiring certified accuracy or forensic evidential value.

Using it for those purposes changes its intended purpose. Under Article 25(1)(c) \
of Regulation (EU) 2024/1689 that makes YOU the provider of a high-risk AI \
system, with the full obligations of Chapter III — conformity assessment, risk \
management, logging, human oversight and registration. This software does not \
provide those, and no acknowledgement here transfers them to or from anyone.";

/// What is written to disk. Version + timestamp so the record says *which*
/// text was accepted and *when* — a bare boolean is not evidence of anything.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub version: u32,
    pub accepted_at_unix: i64,
    /// Free-form note about how it was accepted (`"gui"`, `"cli"`, `"env"`),
    /// so a support question can be answered without guessing.
    #[serde(default)]
    pub via: String,
}

fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

fn env_accepts() -> bool {
    match std::env::var(ENV) {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        }
        Err(_) => false,
    }
}

/// The stored acknowledgement, if any — regardless of whether it is current.
pub fn stored(data_dir: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(path_in(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the acknowledgement on record covers the *current* statement.
pub fn is_acknowledged(data_dir: &Path) -> bool {
    if env_accepts() {
        return true;
    }
    stored(data_dir).is_some_and(|r| r.version >= STATEMENT_VERSION)
}

/// Record an acknowledgement. `via` is for support, not for enforcement.
pub fn acknowledge(data_dir: &Path, via: &str) -> Result<Record, String> {
    let record = Record {
        version: STATEMENT_VERSION,
        accepted_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        via: via.to_owned(),
    };
    let json = serde_json::to_string_pretty(&record)
        .map_err(|e| format!("serialising the acknowledgement: {e}"))?;
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("creating {}: {e}", data_dir.display()))?;
    std::fs::write(path_in(data_dir), json)
        .map_err(|e| format!("writing {}: {e}", path_in(data_dir).display()))?;
    Ok(record)
}

/// The gate. Call at each choke point that produces or applies AI output.
///
/// The error names the operation and every way to acknowledge, because a
/// headless run that fails with an unactionable message is worse than no gate.
pub fn ensure(data_dir: &Path, operation: &str) -> Result<(), String> {
    if is_acknowledged(data_dir) {
        return Ok(());
    }
    Err(format!(
        "\"{operation}\" produces AI output, and this install has not yet \
         acknowledged what the software is intended for.\n\n{STATEMENT}\n\n\
         Acknowledge once by confirming the prompt in the app, by passing \
         --accept-intended-purpose on the CLI, or by setting {ENV}=1 for \
         unattended runs."
    ))
}

/// Forget the acknowledgement. Returns whether a record was actually removed.
///
/// Needed because a legal notice you cannot inspect or withdraw is not much of a
/// notice: an operator handing the machine on, or testing the first-run flow,
/// should not have to know the file name. Deleting the record does not undo
/// anything that was already produced — the gate only governs future output.
pub fn reset(data_dir: &Path) -> Result<bool, String> {
    let p = path_in(data_dir);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("removing {}: {e}", p.display())),
    }
}

/// What the UI needs to decide whether to show the acknowledgement prompt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
    pub acknowledged: bool,
    pub version: u32,
    /// The statement itself, so the dialog renders one source of truth rather
    /// than a copy in the frontend that drifts from the Rust constant.
    pub statement: String,
    pub accepted_at_unix: Option<i64>,
}

pub mod tauri_commands {
    use super::*;
    use tauri::State;

    #[tauri::command]
    pub async fn intended_purpose_status(
        state: State<'_, crate::AppState>,
    ) -> Result<Status, String> {
        let data_dir = state
            .data_dir
            .lock()
            .await
            .clone()
            .ok_or("data_dir not initialised")?;
        let rec = stored(&data_dir);
        Ok(Status {
            acknowledged: is_acknowledged(&data_dir),
            version: STATEMENT_VERSION,
            statement: STATEMENT.to_owned(),
            accepted_at_unix: rec.map(|r| r.accepted_at_unix),
        })
    }

    #[tauri::command]
    pub async fn intended_purpose_acknowledge(
        state: State<'_, crate::AppState>,
    ) -> Result<Status, String> {
        let data_dir = state
            .data_dir
            .lock()
            .await
            .clone()
            .ok_or("data_dir not initialised")?;
        let rec = acknowledge(&data_dir, "gui")?;
        Ok(Status {
            acknowledged: true,
            version: rec.version,
            statement: STATEMENT.to_owned(),
            accepted_at_unix: Some(rec.accepted_at_unix),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn a_fresh_install_is_not_acknowledged_and_the_gate_refuses() {
        let d = tmp();
        assert!(!is_acknowledged(d.path()));
        let err = ensure(d.path(), "translate").expect_err("must refuse");
        // The refusal has to carry the statement and a way out, or a headless
        // operator is stuck with an error they cannot action.
        assert!(err.contains("translate"), "names the operation: {err}");
        assert!(err.contains("NOT intended for"), "carries the statement");
        assert!(err.contains("--accept-intended-purpose"), "offers the CLI route");
        assert!(err.contains(ENV), "offers the unattended route");
    }

    #[test]
    fn acknowledging_records_version_and_time_and_opens_the_gate() {
        let d = tmp();
        let rec = acknowledge(d.path(), "cli").expect("write");
        assert_eq!(rec.version, STATEMENT_VERSION);
        assert!(rec.accepted_at_unix > 1_700_000_000, "a real timestamp");
        assert_eq!(rec.via, "cli");
        assert!(is_acknowledged(d.path()));
        assert!(ensure(d.path(), "translate").is_ok());
    }

    #[test]
    fn the_record_survives_a_restart() {
        let d = tmp();
        acknowledge(d.path(), "gui").unwrap();
        // Fresh read, no in-process state — the whole point of persisting.
        let rec = stored(d.path()).expect("record on disk");
        assert_eq!(rec.version, STATEMENT_VERSION);
        assert_eq!(rec.via, "gui");
    }

    #[test]
    fn a_stale_acknowledgement_does_not_cover_a_newer_statement() {
        let d = tmp();
        // Simulate a record written before the statement was revised.
        let old = Record { version: 0, accepted_at_unix: 1_700_000_001, via: "gui".into() };
        std::fs::write(path_in(d.path()), serde_json::to_string(&old).unwrap()).unwrap();
        assert!(
            !is_acknowledged(d.path()),
            "consent to a superseded text must not count — that is consent to \
             something nobody read"
        );
    }

    #[test]
    fn the_statement_names_the_exclusions_it_claims_to() {
        // The statement's value is its specificity; a future edit that softens
        // it into "comply with applicable law" should fail here.
        for needle in [
            "job applicants", "creditworthiness", "students", "law-enforcement",
            "emotions", "25(1)(c)",
        ] {
            assert!(STATEMENT.contains(needle), "statement lost {needle:?}");
        }
    }

    #[test]
    fn reset_removes_the_record_and_reports_whether_there_was_one() {
        let d = tmp();
        assert!(!reset(d.path()).unwrap(), "nothing to remove yet");
        acknowledge(d.path(), "cli").unwrap();
        assert!(is_acknowledged(d.path()));
        assert!(reset(d.path()).unwrap(), "removed");
        assert!(!is_acknowledged(d.path()), "back to unacknowledged");
        assert!(!reset(d.path()).unwrap(), "idempotent");
    }

    #[test]
    fn a_corrupt_record_is_treated_as_absent_rather_than_trusted() {
        let d = tmp();
        std::fs::write(path_in(d.path()), "{not json").unwrap();
        assert!(!is_acknowledged(d.path()), "unparseable must not open the gate");
    }
}
