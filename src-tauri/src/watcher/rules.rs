//! P29.10 — Automation rule engine.
//!
//! User-configurable trigger-action rules evaluated by the folder watcher.
//! Rules are evaluated in priority order; by default the first matching
//! rule wins (set `match_all: true` to run all matches).
//!
//! This module builds on the existing `SortRule` / `WatchMode::AutoFile`
//! infrastructure (P26.2) but generalises it: triggers can match on
//! extension, doctype, tag, folder prefix, or file size; actions go
//! beyond moving files to include tagging, uploading, running OCR, and
//! sending notifications.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::path::PathBuf;

const RULES_FILE: &str = "automation_rules.json";

// ── Trigger types ────────────────────────────────────────────────────────

/// A single condition that must be true for a rule to fire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Trigger {
    /// File extension matches one of the listed patterns (case-insensitive).
    /// Example: `["pdf", "docx", "xlsx"]`
    Extension { patterns: Vec<String> },

    /// Document type (from P26.1 classifier) matches.
    Doctype { doctype: String },

    /// Document has a specific tag.
    Tag { tag: String },

    /// File path starts with a given prefix.
    FolderPrefix { prefix: String },

    /// File size is within a range (bytes).
    SizeRange { min: Option<u64>, max: Option<u64> },
}

/// How multiple triggers on a rule combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMode {
    /// All triggers must match (AND).
    All,
    /// Any trigger must match (OR).
    Any,
}

impl Default for TriggerMode {
    fn default() -> Self {
        Self::All
    }
}

// ── Action types ─────────────────────────────────────────────────────────

/// An action to perform when a rule fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Action {
    /// Ingest the file into the local index.
    Ingest,

    /// Add tags to the document.
    Tag { tags: Vec<String> },

    /// Move the file to a destination path template.
    /// Supports `{year}`, `{month}`, `{filename}`, `{ext}`, `{doctype}`.
    MoveTo { path_template: String },

    /// Upload the file to a registered cloud drive.
    UploadTo {
        drive_id: String,
        remote_path: String,
    },

    /// Run the OCR pipeline on the file.
    RunOcr {
        /// Optional pipeline name (e.g. "smart", "tesseract").
        /// `None` uses the default pipeline.
        pipeline: Option<String>,
    },

    /// Emit a notification (Tauri event or desktop notification).
    Notify { message: String },
}

// ── Rule ─────────────────────────────────────────────────────────────────

/// A complete automation rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRule {
    /// Unique rule name.
    pub name: String,

    /// Whether the rule is active.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Priority (lower = higher priority).  Rules are evaluated in
    /// ascending priority order.
    #[serde(default)]
    pub priority: i32,

    /// Triggers that must match for the rule to fire.
    pub triggers: Vec<Trigger>,

    /// How triggers combine (default: AND).
    #[serde(default)]
    pub trigger_mode: TriggerMode,

    /// Actions to perform (in order) when the rule fires.
    pub actions: Vec<Action>,
}

fn default_true() -> bool {
    true
}

// ── File metadata for matching ───────────────────────────────────────────

/// Metadata about a file being evaluated against rules.
pub struct FileContext<'a> {
    pub path: &'a Path,
    pub extension: &'a str,
    pub size: u64,
    pub doctype: Option<&'a str>,
    pub tags: &'a [String],
}

/// Runtime boundary for persisted automation rules.
///
/// The engine only evaluates rules and reports actions. It deliberately does
/// not execute filesystem, cloud, OCR, or notification side effects; callers
/// decide which actions are permitted and dispatch them explicitly.
#[derive(Debug, Clone)]
pub struct AutomationEngine {
    rules: Vec<AutomationRule>,
    match_all: bool,
}

impl AutomationEngine {
    pub fn new(rules: Vec<AutomationRule>, match_all: bool) -> Self {
        Self { rules, match_all }
    }

    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self::new(load_rules(data_dir)?, false))
    }

    pub fn rules(&self) -> &[AutomationRule] {
        &self.rules
    }

    pub fn match_all(&self) -> bool {
        self.match_all
    }

    /// Evaluate a filesystem path using metadata available to the watcher.
    /// Classifier-derived doctype and index-derived tags are intentionally
    /// absent here and can be supplied by a richer executor later.
    pub fn evaluate_path(&self, path: &Path) -> Vec<Action> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Vec::new();
        };
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let context = FileContext {
            path,
            extension,
            size: metadata.len(),
            doctype: None,
            tags: &[],
        };
        evaluate(&context, &self.rules, self.match_all)
    }
}

// ── Engine ───────────────────────────────────────────────────────────────

/// Evaluate a file against a set of rules and return the actions to
/// perform.  If `match_all` is false (default), only the first matching
/// rule's actions are returned.
pub fn evaluate(file: &FileContext<'_>, rules: &[AutomationRule], match_all: bool) -> Vec<Action> {
    let mut sorted_rules: Vec<&AutomationRule> = rules.iter().filter(|r| r.enabled).collect();
    sorted_rules.sort_by_key(|r| r.priority);

    let mut all_actions = Vec::new();

    for rule in sorted_rules {
        if rule_matches(file, rule) {
            all_actions.extend(rule.actions.iter().cloned());
            if !match_all {
                break;
            }
        }
    }

    all_actions
}

/// Check if a file matches a rule's triggers.
fn rule_matches(file: &FileContext<'_>, rule: &AutomationRule) -> bool {
    if rule.triggers.is_empty() {
        return false; // No triggers → never matches.
    }

    match rule.trigger_mode {
        TriggerMode::All => rule.triggers.iter().all(|t| trigger_matches(file, t)),
        TriggerMode::Any => rule.triggers.iter().any(|t| trigger_matches(file, t)),
    }
}

/// Check if a single trigger matches a file.
fn trigger_matches(file: &FileContext<'_>, trigger: &Trigger) -> bool {
    match trigger {
        Trigger::Extension { patterns } => {
            let ext_lower = file.extension.to_ascii_lowercase();
            patterns.iter().any(|p| p.to_ascii_lowercase() == ext_lower)
        }
        Trigger::Doctype { doctype } => file
            .doctype
            .map(|d| d.eq_ignore_ascii_case(doctype))
            .unwrap_or(false),
        Trigger::Tag { tag } => file.tags.iter().any(|t| t == tag),
        Trigger::FolderPrefix { prefix } => {
            let path_str = file.path.to_string_lossy();
            path_str.starts_with(prefix.as_str())
        }
        Trigger::SizeRange { min, max } => {
            if let Some(min_val) = min {
                if file.size < *min_val {
                    return false;
                }
            }
            if let Some(max_val) = max {
                if file.size > *max_val {
                    return false;
                }
            }
            true
        }
    }
}

/// Default automation rules (shipped disabled as examples).
pub fn default_rules() -> Vec<AutomationRule> {
    vec![
        AutomationRule {
            name: "Invoices to accounting folder".into(),
            enabled: false,
            priority: 10,
            triggers: vec![Trigger::Doctype {
                doctype: "invoice".into(),
            }],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::MoveTo {
                path_template: "Invoices/{year}/{month}/".into(),
            }],
        },
        AutomationRule {
            name: "Photos to cloud".into(),
            enabled: false,
            priority: 20,
            triggers: vec![
                Trigger::Extension {
                    patterns: vec!["jpg".into(), "png".into(), "heic".into()],
                },
                Trigger::SizeRange {
                    min: Some(1_000_000),
                    max: None,
                },
            ],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::UploadTo {
                drive_id: "gdrive-default".into(),
                remote_path: "/Photos/{year}/".into(),
            }],
        },
        AutomationRule {
            name: "OCR all scans".into(),
            enabled: false,
            priority: 30,
            triggers: vec![Trigger::FolderPrefix {
                prefix: "/Scans/".into(),
            }],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::RunOcr {
                pipeline: Some("smart".into()),
            }],
        },
    ]
}

/// Load persisted automation rules, falling back to the disabled examples on
/// a first run. The file contains no credentials or machine-specific paths.
pub fn load_rules(data_dir: &Path) -> anyhow::Result<Vec<AutomationRule>> {
    let path = data_dir.join(RULES_FILE);
    if !path.exists() {
        return Ok(default_rules());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Persist the complete rule set atomically so a crash cannot leave a partial
/// automation configuration behind.
pub fn save_rules(data_dir: &Path, rules: &[AutomationRule]) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(RULES_FILE);
    let partial = PathBuf::from(format!("{}.partial-{}", path.display(), std::process::id()));
    std::fs::write(&partial, serde_json::to_vec_pretty(rules)?)?;
    std::fs::rename(partial, path)?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_file<'a>(
        path: &'a str,
        ext: &'a str,
        size: u64,
        doctype: Option<&'a str>,
        tags: &'a [String],
    ) -> FileContext<'a> {
        // Leak a PathBuf so we can return a reference with 'a lifetime.
        // This is fine in tests.
        FileContext {
            path: Path::new(path),
            extension: ext,
            size,
            doctype,
            tags,
        }
    }

    #[test]
    fn engine_evaluates_path_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.PDF");
        std::fs::write(&path, b"pdf").unwrap();
        let engine = AutomationEngine::new(
            vec![AutomationRule {
                name: "ingest pdf".into(),
                enabled: true,
                priority: 0,
                triggers: vec![Trigger::Extension {
                    patterns: vec!["pdf".into()],
                }],
                trigger_mode: TriggerMode::All,
                actions: vec![Action::Ingest],
            }],
            false,
        );
        assert_eq!(engine.evaluate_path(&path), vec![Action::Ingest]);
        assert_eq!(engine.evaluate_path(&dir.path().join("missing.pdf")), Vec::new());
    }

    #[test]
    fn extension_trigger_case_insensitive() {
        let t = Trigger::Extension {
            patterns: vec!["PDF".into(), "docx".into()],
        };
        let f = make_file("/test/file.pdf", "pdf", 100, None, &[]);
        assert!(trigger_matches(&f, &t));

        let f2 = make_file("/test/file.txt", "txt", 100, None, &[]);
        assert!(!trigger_matches(&f2, &t));
    }

    #[test]
    fn doctype_trigger() {
        let t = Trigger::Doctype {
            doctype: "invoice".into(),
        };
        let f = make_file("/test/inv.pdf", "pdf", 100, Some("invoice"), &[]);
        assert!(trigger_matches(&f, &t));

        let f2 = make_file("/test/inv.pdf", "pdf", 100, Some("receipt"), &[]);
        assert!(!trigger_matches(&f2, &t));

        let f3 = make_file("/test/inv.pdf", "pdf", 100, None, &[]);
        assert!(!trigger_matches(&f3, &t));
    }

    #[test]
    fn tag_trigger() {
        let t = Trigger::Tag {
            tag: "important".into(),
        };
        let tags = vec!["important".into(), "urgent".into()];
        let f = make_file("/test/f.pdf", "pdf", 100, None, &tags);
        assert!(trigger_matches(&f, &t));

        let f2 = make_file("/test/f.pdf", "pdf", 100, None, &[]);
        assert!(!trigger_matches(&f2, &t));
    }

    #[test]
    fn folder_prefix_trigger() {
        let t = Trigger::FolderPrefix {
            prefix: "/Scans/".into(),
        };
        let f = make_file("/Scans/doc.pdf", "pdf", 100, None, &[]);
        assert!(trigger_matches(&f, &t));

        let f2 = make_file("/Documents/doc.pdf", "pdf", 100, None, &[]);
        assert!(!trigger_matches(&f2, &t));
    }

    #[test]
    fn size_range_trigger() {
        let t = Trigger::SizeRange {
            min: Some(1000),
            max: Some(5000),
        };
        let f = make_file("/f", "pdf", 2000, None, &[]);
        assert!(trigger_matches(&f, &t));

        let f2 = make_file("/f", "pdf", 500, None, &[]);
        assert!(!trigger_matches(&f2, &t));

        let f3 = make_file("/f", "pdf", 6000, None, &[]);
        assert!(!trigger_matches(&f3, &t));
    }

    #[test]
    fn size_range_open_ended() {
        let t = Trigger::SizeRange {
            min: Some(1_000_000),
            max: None,
        };
        let f = make_file("/f", "jpg", 5_000_000, None, &[]);
        assert!(trigger_matches(&f, &t));

        let f2 = make_file("/f", "jpg", 500, None, &[]);
        assert!(!trigger_matches(&f2, &t));
    }

    #[test]
    fn rule_and_mode_requires_all_triggers() {
        let rule = AutomationRule {
            name: "test".into(),
            enabled: true,
            priority: 0,
            triggers: vec![
                Trigger::Extension {
                    patterns: vec!["pdf".into()],
                },
                Trigger::SizeRange {
                    min: Some(100),
                    max: None,
                },
            ],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::Ingest],
        };

        // Matches both.
        let f = make_file("/f.pdf", "pdf", 200, None, &[]);
        assert!(rule_matches(&f, &rule));

        // Matches extension but not size.
        let f2 = make_file("/f.pdf", "pdf", 50, None, &[]);
        assert!(!rule_matches(&f2, &rule));
    }

    #[test]
    fn rule_or_mode_requires_any_trigger() {
        let rule = AutomationRule {
            name: "test".into(),
            enabled: true,
            priority: 0,
            triggers: vec![
                Trigger::Extension {
                    patterns: vec!["pdf".into()],
                },
                Trigger::Tag {
                    tag: "urgent".into(),
                },
            ],
            trigger_mode: TriggerMode::Any,
            actions: vec![Action::Ingest],
        };

        // Matches extension only.
        let f = make_file("/f.pdf", "pdf", 100, None, &[]);
        assert!(rule_matches(&f, &rule));

        // Matches tag only.
        let tags = vec!["urgent".into()];
        let f2 = make_file("/f.txt", "txt", 100, None, &tags);
        assert!(rule_matches(&f2, &rule));

        // Matches neither.
        let f3 = make_file("/f.txt", "txt", 100, None, &[]);
        assert!(!rule_matches(&f3, &rule));
    }

    #[test]
    fn evaluate_first_match_wins() {
        let rules = vec![
            AutomationRule {
                name: "low-priority".into(),
                enabled: true,
                priority: 100,
                triggers: vec![Trigger::Extension {
                    patterns: vec!["pdf".into()],
                }],
                trigger_mode: TriggerMode::All,
                actions: vec![Action::Notify {
                    message: "low".into(),
                }],
            },
            AutomationRule {
                name: "high-priority".into(),
                enabled: true,
                priority: 1,
                triggers: vec![Trigger::Extension {
                    patterns: vec!["pdf".into()],
                }],
                trigger_mode: TriggerMode::All,
                actions: vec![Action::Notify {
                    message: "high".into(),
                }],
            },
        ];

        let f = make_file("/f.pdf", "pdf", 100, None, &[]);
        let actions = evaluate(&f, &rules, false);
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            Action::Notify {
                message: "high".into()
            }
        );
    }

    #[test]
    fn evaluate_match_all_collects_all() {
        let rules = vec![
            AutomationRule {
                name: "r1".into(),
                enabled: true,
                priority: 1,
                triggers: vec![Trigger::Extension {
                    patterns: vec!["pdf".into()],
                }],
                trigger_mode: TriggerMode::All,
                actions: vec![Action::Ingest],
            },
            AutomationRule {
                name: "r2".into(),
                enabled: true,
                priority: 2,
                triggers: vec![Trigger::Extension {
                    patterns: vec!["pdf".into()],
                }],
                trigger_mode: TriggerMode::All,
                actions: vec![Action::Tag {
                    tags: vec!["auto".into()],
                }],
            },
        ];

        let f = make_file("/f.pdf", "pdf", 100, None, &[]);
        let actions = evaluate(&f, &rules, true);
        assert_eq!(actions.len(), 2);
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let rules = vec![AutomationRule {
            name: "disabled".into(),
            enabled: false,
            priority: 0,
            triggers: vec![Trigger::Extension {
                patterns: vec!["pdf".into()],
            }],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::Ingest],
        }];

        let f = make_file("/f.pdf", "pdf", 100, None, &[]);
        let actions = evaluate(&f, &rules, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn no_triggers_never_matches() {
        let rule = AutomationRule {
            name: "empty".into(),
            enabled: true,
            priority: 0,
            triggers: vec![],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::Ingest],
        };
        let f = make_file("/f.pdf", "pdf", 100, None, &[]);
        assert!(!rule_matches(&f, &rule));
    }

    #[test]
    fn no_match_falls_through() {
        let rules = vec![AutomationRule {
            name: "only-docx".into(),
            enabled: true,
            priority: 0,
            triggers: vec![Trigger::Extension {
                patterns: vec!["docx".into()],
            }],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::Ingest],
        }];

        let f = make_file("/f.pdf", "pdf", 100, None, &[]);
        let actions = evaluate(&f, &rules, false);
        assert!(actions.is_empty());
    }

    #[test]
    fn rule_serde_round_trips() {
        let rules = default_rules();
        let json = serde_json::to_string(&rules).unwrap();
        let back: Vec<AutomationRule> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), rules.len());
        assert_eq!(back[0].name, "Invoices to accounting folder");
    }

    #[test]
    fn action_serde_round_trips() {
        let actions = vec![
            Action::Ingest,
            Action::Tag {
                tags: vec!["a".into(), "b".into()],
            },
            Action::MoveTo {
                path_template: "Out/{year}/".into(),
            },
            Action::UploadTo {
                drive_id: "d1".into(),
                remote_path: "/cloud/".into(),
            },
            Action::RunOcr {
                pipeline: Some("smart".into()),
            },
            Action::Notify {
                message: "done!".into(),
            },
        ];
        let json = serde_json::to_string(&actions).unwrap();
        let back: Vec<Action> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, actions);
    }

    #[test]
    fn persisted_rules_round_trip_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let rules = vec![AutomationRule {
            name: "test-rule".into(),
            enabled: true,
            priority: 4,
            triggers: vec![Trigger::Extension { patterns: vec!["pdf".into()] }],
            trigger_mode: TriggerMode::All,
            actions: vec![Action::Ingest],
        }];
        save_rules(dir.path(), &rules).unwrap();
        assert_eq!(load_rules(dir.path()).unwrap(), rules);
        assert!(dir.path().join("automation_rules.json").exists());
    }

    #[test]
    fn missing_rules_loads_disabled_examples() {
        let dir = tempfile::tempdir().unwrap();
        let rules = load_rules(dir.path()).unwrap();
        assert!(!rules.is_empty());
        assert!(rules.iter().all(|rule| !rule.enabled));
    }
}
