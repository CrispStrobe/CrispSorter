//! Where CrispSorter's secrets actually live — a vault the user picks.
//!
//! Every credential the app holds (LLM API keys, the CrispLens session
//! cookie, cloud-backup bearer tokens, cloud-drive sessions, the proxy
//! password) used to go straight into whatever the `keyring` crate
//! considers the platform default. On macOS that is the **login
//! keychain**, and that turned out to be a bad place to be pinned to:
//!
//! * A keychain item remembers which binary created it. A rebuilt or
//!   re-signed app is, to macOS, a *different* application, so it is not
//!   on the item's ACL and every read raises the
//!   "CrispSorter wants to use your confidential information …" dialog.
//! * Clearing that dialog for good ("Always Allow") **modifies the ACL**,
//!   and modifying an ACL requires the keychain's own password. Users
//!   whose login-keychain password has drifted out of sync with their
//!   account password — an Apple ID reset does this — cannot supply it,
//!   so the dialog returns on every single read, forever.
//! * Nothing about the app actually requires that particular vault.
//!
//! So the vault is now a choice, not a constant. [`BackendChoice`] lists
//! what is on offer; the user picks one in Settings (or with
//! `crispsorter secrets backend`, or via `CRISPSORTER_SECRET_BACKEND`),
//! and every secret module in the app routes through [`Entry`], which
//! dispatches to whichever vault is active.
//!
//! # The choices
//!
//! | Choice | Where the bytes go | Prompts? |
//! |---|---|---|
//! | [`BackendChoice::OsDefault`] | macOS login keychain / Windows Credential Manager / Secret Service | macOS: yes, see above |
//! | [`BackendChoice::MacKeychain`] | a named `*.keychain-db` — a dedicated one the app creates, or an existing one you already know the password of | only when macOS needs that keychain's password |
//! | [`BackendChoice::File`] | AES-256-GCM envelope under the app data dir | never |
//! | [`BackendChoice::Session`] | nowhere; process memory only | never |
//!
//! # Defaults and compatibility
//!
//! Absent a stored choice the vault is [`BackendChoice::OsDefault`] —
//! byte-for-byte the old behaviour, so an existing install keeps reading
//! the secrets it already has. The chooser is offered, not imposed;
//! [`is_chosen`] tells the UI whether the user has ever answered.
//!
//! # The no-more-nagging rule
//!
//! When a vault read fails in a way that means *the user was asked and
//! could not or would not answer* — a cancelled macOS dialog, a failed
//! authentication, a request that could not be shown at all — we latch
//! [`Error::Denied`] for the rest of the process and stop calling the OS
//! entirely. One dialog per run, not one per secret. [`clear_denied`]
//! lifts the latch after the user has changed something.

pub mod file_vault;
#[cfg(target_os = "macos")]
pub mod mac_keychain;
pub mod memory;
pub mod os_keyring;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

/// Basename of the file under the app data dir that records the user's
/// choice. Deliberately *not* inside `settings.json`: the vault has to
/// be resolvable before the settings store is up, and by the CLI, which
/// never loads it at all.
pub const CONFIG_FILE: &str = "secret_backend.json";

/// Env override, evaluated before the config file. Accepts:
/// `os`, `session`, `file`, `file:device`, `file:passphrase`,
/// `keychain:<path>`, `keychain:<path>:prompt`.
pub const ENV_BACKEND: &str = "CRISPSORTER_SECRET_BACKEND";

/// Passphrase for `file:passphrase`, so headless CLI runs need no TTY.
pub const ENV_PASSPHRASE: &str = "CRISPSORTER_VAULT_PASSPHRASE";

// ── Errors ──────────────────────────────────────────────────────────

/// Failure modes of the vault layer.
///
/// Shaped like `keyring::Error` where the call sites already matched on
/// it (`NoEntry` above all), so routing the app through this type was a
/// mechanical change rather than a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Nothing is stored under that (service, account).
    NoEntry,
    /// The user was asked for permission and the answer was no — a
    /// cancelled dialog, a wrong password, or a prompt that could not be
    /// displayed. Latched process-wide; see the module docs.
    Denied(String),
    /// The vault exists but needs a passphrase before it can be read.
    Locked(String),
    /// The vault itself could not be reached or created.
    Backend(String),
    /// Anything else, with the platform's own words.
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoEntry => write!(f, "no secret stored under that account"),
            Error::Denied(s) => write!(
                f,
                "the system credential store denied access ({s}). \
                 CrispSorter will not ask again this run. \
                 Pick a different secret store in Settings › Security."
            ),
            Error::Locked(s) => write!(f, "the secret vault is locked ({s})"),
            Error::Backend(s) => write!(f, "secret store unavailable: {s}"),
            Error::Other(s) => write!(f, "secret store error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

// ── The vault trait ─────────────────────────────────────────────────

/// One place secrets can be kept. Implementations are cheap to call and
/// internally synchronised; the app holds a single `Arc<dyn Vault>` and
/// shares it across threads.
pub trait Vault: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, Error>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), Error>;
    fn delete(&self, service: &str, account: &str) -> Result<(), Error>;

    /// Stable identifier for logs and the Settings UI.
    fn kind(&self) -> &'static str;

    /// `false` while a passphrase-protected vault is still locked.
    fn is_unlocked(&self) -> bool {
        true
    }

    /// Supply a passphrase. Vaults that need none return `Ok(())`.
    fn unlock(&self, _passphrase: &str) -> Result<(), Error> {
        Ok(())
    }
}

// ── The choice ──────────────────────────────────────────────────────

/// How a named macOS keychain gets unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MacUnlock {
    /// CrispSorter created the keychain and keeps its random password in
    /// a `0600` file beside the app data. Unlocks silently; the user
    /// never sees a password dialog for it.
    #[default]
    AppManaged,
    /// The keychain is the user's own (a build keychain, say). macOS
    /// asks for its password when it needs it, and the user knows it.
    Prompt,
}

/// Where an encrypted-file vault's AES key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FileKeySource {
    /// A random 32-byte key in a `0600` file next to the vault.
    ///
    /// This protects the secrets from everything that copies *files*
    /// without copying permissions or the whole directory — cloud sync,
    /// Time Machine restores onto another account, a bug-report tarball,
    /// a settings export. It does not protect them from a process
    /// already running as this user, which can read the key file too.
    /// That is the same boundary the app had before any of this, minus
    /// the plaintext.
    #[default]
    Device,
    /// Argon2id over a passphrase the user types. Nothing on disk opens
    /// the vault, so a stolen copy of the directory is worthless — at
    /// the cost of typing the passphrase once per run.
    Passphrase,
}

/// The vault the user has chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BackendChoice {
    /// Whatever `keyring` calls the platform default. On macOS: the
    /// login keychain.
    OsDefault,
    /// A specific macOS keychain file.
    MacKeychain {
        path: PathBuf,
        #[serde(default)]
        unlock: MacUnlock,
    },
    /// An encrypted file under the app data dir.
    File {
        #[serde(default)]
        key: FileKeySource,
    },
    /// Memory only. Secrets last as long as the process does.
    Session,
}

impl Default for BackendChoice {
    fn default() -> Self {
        BackendChoice::OsDefault
    }
}

impl BackendChoice {
    /// Short stable id, matching [`Vault::kind`].
    pub fn kind(&self) -> &'static str {
        match self {
            BackendChoice::OsDefault => "os-default",
            BackendChoice::MacKeychain { .. } => "mac-keychain",
            BackendChoice::File { .. } => "file",
            BackendChoice::Session => "session",
        }
    }

    /// Parse the [`ENV_BACKEND`] syntax. `None` for anything unrecognised
    /// — an unreadable env var must not lock the user out of their
    /// secrets, so we fall through to the stored choice instead.
    pub fn parse_env(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (head, rest) = match raw.split_once(':') {
            Some((h, r)) => (h, Some(r)),
            None => (raw, None),
        };
        match head {
            "os" | "os-default" | "keyring" => Some(BackendChoice::OsDefault),
            "session" | "memory" | "none" => Some(BackendChoice::Session),
            "file" => Some(BackendChoice::File {
                key: match rest {
                    Some("passphrase") => FileKeySource::Passphrase,
                    _ => FileKeySource::Device,
                },
            }),
            "keychain" | "mac-keychain" => {
                // `keychain:/path/to/x.keychain-db[:prompt]`. Split from the
                // right so a path containing a colon still works.
                let rest = rest?;
                let (path, unlock) = match rest.rsplit_once(':') {
                    Some((p, "prompt")) => (p, MacUnlock::Prompt),
                    Some((p, "app-managed")) => (p, MacUnlock::AppManaged),
                    _ => (rest, MacUnlock::AppManaged),
                };
                if path.is_empty() {
                    return None;
                }
                Some(BackendChoice::MacKeychain {
                    path: PathBuf::from(path),
                    unlock,
                })
            }
            _ => None,
        }
    }
}

/// What [`load_choice`] found, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The active choice.
    pub choice: BackendChoice,
    /// `true` once the user has answered the chooser at least once.
    /// While `false` the app is on the compatibility default and the UI
    /// should offer the choice.
    pub chosen: bool,
    /// Set when [`ENV_BACKEND`] overrode the stored choice — the UI must
    /// then show the picker as read-only rather than lie about what is
    /// in effect.
    pub from_env: bool,
    /// `false` while a passphrase vault awaits its passphrase.
    pub unlocked: bool,
    /// `true` once a vault read was denied and the latch is up.
    pub denied: bool,
    /// Why the requested vault could not be built, if it could not be.
    /// [`Status::choice`] still reports what the user asked for; the
    /// store actually serving reads until it is fixed is an empty
    /// in-memory one. Never the OS default — answering "your store did
    /// not open" by quietly using the store the user rejected is how the
    /// macOS dialogs would come back.
    pub error: Option<String>,
}

// ── Persistence ─────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct StoredConfig {
    #[serde(flatten)]
    choice: BackendChoice,
}

/// Path of the choice file inside `data_dir`.
pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE)
}

/// Read the stored choice. `None` when the user has never chosen, or
/// when the file is unreadable/corrupt — a broken config must degrade to
/// the compatibility default, never to a hard failure.
pub fn load_choice(data_dir: &Path) -> Option<BackendChoice> {
    let raw = std::fs::read_to_string(config_path(data_dir)).ok()?;
    serde_json::from_str::<StoredConfig>(&raw)
        .ok()
        .map(|c| c.choice)
}

/// Persist the choice, creating `data_dir` if needed.
pub fn save_choice(data_dir: &Path, choice: &BackendChoice) -> Result<(), Error> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| Error::Backend(format!("creating {}: {e}", data_dir.display())))?;
    let body = serde_json::to_string_pretty(&StoredConfig {
        choice: choice.clone(),
    })
    .map_err(|e| Error::Other(e.to_string()))?;
    let path = config_path(data_dir);
    write_private(&path, body.as_bytes())
}

/// Write `bytes` to `path` atomically and owner-readable only.
///
/// Shared with [`file_vault`]: the vault envelope and the key file next
/// to it both need the temp-file-then-rename dance (a half-written vault
/// is an unopenable vault) and both need `0600`.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| Error::Backend(format!("creating {}: {e}", tmp.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Before the bytes, not after: a world-readable window, however
            // short, is still a window.
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        f.write_all(bytes)
            .map_err(|e| Error::Backend(format!("writing {}: {e}", tmp.display())))?;
        f.sync_all()
            .map_err(|e| Error::Backend(format!("syncing {}: {e}", tmp.display())))?;
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::Backend(format!("replacing {}: {e}", path.display())))?;
    Ok(())
}

// ── Process-wide active vault ───────────────────────────────────────

struct Slot {
    choice: BackendChoice,
    chosen: bool,
    from_env: bool,
    error: Option<String>,
    vault: Arc<dyn Vault>,
}

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
static SLOT: OnceLock<RwLock<Slot>> = OnceLock::new();
static DENIED: AtomicBool = AtomicBool::new(false);

/// Point the vault at a specific data dir. Call once, early, before any
/// secret is touched; later calls are ignored. The GUI passes Tauri's
/// `app_data_dir()`, the CLI its `--data-dir`. Skipping it altogether is
/// fine — [`default_app_data_dir`] is the same path in practice.
pub fn set_data_dir(dir: PathBuf) {
    let _ = DATA_DIR.set(dir);
}

fn data_dir() -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(default_app_data_dir)
}

/// The OS-conventional app data dir for CrispSorter, matching what
/// `tauri::path::app_data_dir()` returns.
///
/// The vault has to resolve this without a Tauri `AppHandle` — the CLI
/// has none, and even in the GUI the vault can be touched before setup
/// runs.
pub fn default_app_data_dir() -> PathBuf {
    const BUNDLE: &str = "com.crispstrobe.crispsorter";
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join("Library/Application Support").join(BUNDLE)
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default();
        appdata.join(BUNDLE)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
            });
        base.join(BUNDLE)
    }
}

/// Build the vault for `choice`, or say why it cannot be built.
fn build(choice: &BackendChoice, dir: &Path) -> Result<Arc<dyn Vault>, Error> {
    match choice {
        BackendChoice::OsDefault => Ok(Arc::new(os_keyring::OsKeyring)),
        BackendChoice::Session => Ok(Arc::new(memory::MemoryVault::default())),
        BackendChoice::File { key } => {
            Ok(Arc::new(file_vault::FileVault::open(dir, *key)?) as Arc<dyn Vault>)
        }
        #[cfg(target_os = "macos")]
        BackendChoice::MacKeychain { path, unlock } => {
            Ok(Arc::new(mac_keychain::MacKeychainVault::open(path, *unlock, dir)?)
                as Arc<dyn Vault>)
        }
        #[cfg(not(target_os = "macos"))]
        BackendChoice::MacKeychain { .. } => Err(Error::Backend(
            "named keychains exist only on macOS".to_string(),
        )),
    }
}

fn slot() -> &'static RwLock<Slot> {
    SLOT.get_or_init(|| {
        // Under `cargo test` the vault is memory-only and reads no config.
        // Otherwise a developer who has chosen the file vault would find
        // the suite creating and rewriting their *real* `secrets.vault`.
        #[cfg(test)]
        {
            return RwLock::new(Slot {
                choice: BackendChoice::Session,
                chosen: false,
                from_env: false,
                error: None,
                vault: Arc::new(memory::MemoryVault::default()),
            });
        }
        #[cfg(not(test))]
        {
            let dir = data_dir();
            let env = std::env::var(ENV_BACKEND)
                .ok()
                .as_deref()
                .and_then(BackendChoice::parse_env);
            let from_env = env.is_some();
            let stored = load_choice(&dir);
            let chosen = stored.is_some();
            let choice = env.or(stored).unwrap_or_default();

            // A vault we cannot build must not take the app's secrets with
            // it — but the fallback is memory, NOT the platform default.
            // Falling back to the OS store would answer "your chosen store
            // did not open" with "so we used the one you rejected", which
            // on macOS means the login-keychain dialogs are back. Memory
            // loses nothing: the unopened vault's contents stay on disk,
            // and `status().error` tells the UI what to say.
            match build(&choice, &dir) {
                Ok(vault) => RwLock::new(Slot {
                    choice,
                    chosen,
                    from_env,
                    error: None,
                    vault,
                }),
                Err(e) => RwLock::new(Slot {
                    choice,
                    chosen,
                    from_env,
                    error: Some(e.to_string()),
                    vault: Arc::new(memory::MemoryVault::default()),
                }),
            }
        }
    })
}

/// Swap the active vault without persisting anything.
///
/// Tests only. It exists so a module's own tests can drive its real
/// public API against an in-memory store — which previously required a
/// bespoke `keyring::CredentialBuilder` per module, because
/// `keyring::mock` stores the secret inside the `Credential` and so
/// loses it between two `Entry::new` calls for the same account.
#[cfg(test)]
pub fn install_for_tests(v: Arc<dyn Vault>) {
    let mut s = slot().write().expect("vault slot poisoned");
    s.vault = v;
}

/// The vault in force right now.
pub fn active() -> Arc<dyn Vault> {
    Arc::clone(&slot().read().expect("vault slot poisoned").vault)
}

/// Resolve the vault eagerly against `dir`. Cheap, idempotent, and the
/// only way the log line lands at startup rather than on first use.
pub fn init(dir: PathBuf) {
    set_data_dir(dir);
    let _ = slot();
}

/// Everything the Settings pane needs to render the picker.
pub fn status() -> Status {
    let s = slot().read().expect("vault slot poisoned");
    Status {
        choice: s.choice.clone(),
        chosen: s.chosen,
        from_env: s.from_env,
        unlocked: s.vault.is_unlocked(),
        denied: DENIED.load(Ordering::Relaxed),
        error: s.error.clone(),
    }
}

/// Has the user ever answered the chooser?
pub fn is_chosen() -> bool {
    slot().read().expect("vault slot poisoned").chosen
}

/// Switch vaults and persist the decision. Takes effect immediately —
/// no restart — because every secret module resolves [`active`] per
/// call rather than caching a handle.
///
/// Secrets already in the *old* vault stay there. Moving them is
/// [`migrate`]'s job, and is a separate decision: a user switching away
/// from a keychain they can no longer open does not want the switch to
/// fail because the copy-out failed.
pub fn select(choice: BackendChoice) -> Result<(), Error> {
    let dir = data_dir();
    let vault = build(&choice, &dir)?;
    save_choice(&dir, &choice)?;
    clear_denied();
    let mut s = slot().write().expect("vault slot poisoned");
    s.choice = choice;
    s.chosen = true;
    s.error = None;
    s.vault = vault;
    Ok(())
}

/// Unlock the active vault with `passphrase`.
pub fn unlock(passphrase: &str) -> Result<(), Error> {
    let v = active();
    v.unlock(passphrase)?;
    clear_denied();
    Ok(())
}

/// Copy `pairs` from the vault `from` describes into the active one.
///
/// Returns `(copied, skipped, failures)` — failures carry the
/// `(service, account)` and the reason, because a partial migration is
/// the normal outcome when the source is a keychain that prompts and
/// the user answers "Deny" halfway through.
#[allow(clippy::type_complexity)]
pub fn migrate(
    from: &BackendChoice,
    pairs: &[(String, String)],
) -> Result<(usize, usize, Vec<(String, String, String)>), Error> {
    let dir = data_dir();
    // An explicit "copy my keys across" is the user asking to be prompted,
    // so lower the latch first — otherwise a refusal earlier in the session
    // would make every row fail without the store ever being consulted. At
    // most one dialog follows: the first denial re-latches and breaks the
    // loop below.
    clear_denied();
    let source = build(from, &dir)?;
    let target = active();
    let (mut copied, mut skipped) = (0usize, 0usize);
    let mut failures = Vec::new();
    for (service, account) in pairs {
        match source.get(service, account) {
            Ok(Some(v)) => match target.set(service, account, &v) {
                Ok(()) => copied += 1,
                Err(e) => failures.push((service.clone(), account.clone(), e.to_string())),
            },
            Ok(None) => skipped += 1,
            // A denial mid-migration ends it: every remaining read would
            // raise the same dialog the user just dismissed.
            Err(e @ Error::Denied(_)) => {
                failures.push((service.clone(), account.clone(), e.to_string()));
                break;
            }
            Err(e) => failures.push((service.clone(), account.clone(), e.to_string())),
        }
    }
    Ok((copied, skipped, failures))
}

/// Every service name the app stores secrets under. The migration UI
/// needs this to build its candidate list; there is no vault API
/// anywhere that enumerates entries (macOS would prompt per row).
pub const SERVICES: &[&str] = &[
    "CrispSorter.LLM",
    "CrispSorter.CrispLens",
    "CrispSorter.CloudBackup",
    "CrispSorter.CloudDrive",
    "CrispSorter.CloudDrive.Auth",
    "CrispSorter.Proxy",
];

// ── The denial latch ────────────────────────────────────────────────

/// Is the latch up — i.e. did a read already get refused this run?
pub fn is_denied() -> bool {
    DENIED.load(Ordering::Relaxed)
}

/// Lower the latch. Called after the user changes vault, unlocks one, or
/// explicitly asks to try again.
pub fn clear_denied() {
    DENIED.store(false, Ordering::Relaxed);
}

/// Raise the latch. Backends call this the moment they see a refusal.
pub(crate) fn latch_denied() {
    DENIED.store(true, Ordering::Relaxed);
}

// ── keyring-shaped facade ───────────────────────────────────────────

/// A (service, account) handle, shaped like `keyring::Entry` so the six
/// secret modules could move across without restructuring.
///
/// Unlike `keyring::Entry` this binds nothing at construction: it
/// resolves [`active`] on every call, which is what makes switching
/// vaults take effect without a restart.
#[derive(Debug, Clone)]
pub struct Entry {
    service: String,
    account: String,
}

impl Entry {
    /// Infallible in practice; returns `Result` to match the call sites
    /// that already wrote `Entry::new(..)?`.
    pub fn new(service: &str, account: &str) -> Result<Self, Error> {
        Ok(Entry {
            service: service.to_string(),
            account: account.to_string(),
        })
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn set_password(&self, value: &str) -> Result<(), Error> {
        active().set(&self.service, &self.account, value)
    }

    /// `Err(Error::NoEntry)` when nothing is stored — the shape the old
    /// `match … Err(keyring::Error::NoEntry)` arms expect.
    pub fn get_password(&self) -> Result<String, Error> {
        match active().get(&self.service, &self.account)? {
            Some(v) => Ok(v),
            None => Err(Error::NoEntry),
        }
    }

    pub fn delete_credential(&self) -> Result<(), Error> {
        active().delete(&self.service, &self.account)
    }
}

// ── macOS keychain discovery ────────────────────────────────────────

/// One keychain the user could point the app at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeychainInfo {
    pub path: PathBuf,
    /// Filename without the `.keychain-db` / `.keychain` suffix.
    pub name: String,
    /// `true` for the login keychain — the one that prompts.
    pub is_login: bool,
    /// `true` for a keychain CrispSorter created for itself.
    pub is_ours: bool,
}

/// Keychains in the user's `~/Library/Keychains`.
///
/// Read by listing the directory rather than by asking the Security
/// framework for the search list: the search list is what
/// `SecKeychainCopySearchList` returns, which is both narrower (it omits
/// keychains that exist but are not in the list) and unexposed by the
/// `security-framework` crate. A directory listing is exactly the set a
/// user would see in Keychain Access's sidebar.
///
/// Empty on every non-macOS platform.
pub fn list_keychains() -> Vec<KeychainInfo> {
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
    #[cfg(target_os = "macos")]
    {
        let home = match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h),
            None => return Vec::new(),
        };
        let dir = home.join("Library/Keychains");
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<KeychainInfo> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                let file = path.file_name()?.to_str()?.to_string();
                let name = file
                    .strip_suffix(".keychain-db")
                    .or_else(|| file.strip_suffix(".keychain"))?
                    .to_string();
                Some(KeychainInfo {
                    is_login: name == "login",
                    is_ours: name == mac_keychain::DEFAULT_KEYCHAIN_NAME,
                    name,
                    path,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Where [`mac_keychain`] would put a keychain of its own.
pub fn suggested_keychain_path() -> Option<PathBuf> {
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
    #[cfg(target_os = "macos")]
    {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        Some(
            home.join("Library/Keychains")
                .join(format!("{}.keychain-db", mac_keychain::DEFAULT_KEYCHAIN_NAME)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_syntax_covers_every_backend() {
        assert_eq!(BackendChoice::parse_env("os"), Some(BackendChoice::OsDefault));
        assert_eq!(
            BackendChoice::parse_env("session"),
            Some(BackendChoice::Session)
        );
        assert_eq!(
            BackendChoice::parse_env("file"),
            Some(BackendChoice::File {
                key: FileKeySource::Device
            })
        );
        assert_eq!(
            BackendChoice::parse_env("file:passphrase"),
            Some(BackendChoice::File {
                key: FileKeySource::Passphrase
            })
        );
        assert_eq!(
            BackendChoice::parse_env("keychain:/tmp/x.keychain-db"),
            Some(BackendChoice::MacKeychain {
                path: PathBuf::from("/tmp/x.keychain-db"),
                unlock: MacUnlock::AppManaged,
            })
        );
        assert_eq!(
            BackendChoice::parse_env("keychain:/tmp/x.keychain-db:prompt"),
            Some(BackendChoice::MacKeychain {
                path: PathBuf::from("/tmp/x.keychain-db"),
                unlock: MacUnlock::Prompt,
            })
        );
    }

    #[test]
    fn unparseable_env_falls_through_rather_than_failing() {
        // The stored choice must win over a typo, not be shadowed by it.
        assert_eq!(BackendChoice::parse_env("nonsense"), None);
        assert_eq!(BackendChoice::parse_env("keychain:"), None);
        assert_eq!(BackendChoice::parse_env(""), None);
    }

    #[test]
    fn choice_round_trips_through_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_choice(dir.path()).is_none(), "nothing chosen yet");

        let choice = BackendChoice::File {
            key: FileKeySource::Passphrase,
        };
        save_choice(dir.path(), &choice).unwrap();
        assert_eq!(load_choice(dir.path()), Some(choice));
    }

    #[test]
    fn a_corrupt_config_reads_as_no_choice_not_as_an_error() {
        // Degrading to the compatibility default keeps a user who hand-edited
        // the file out of a state where no secret is reachable at all.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(config_path(dir.path()), b"{ not json").unwrap();
        assert!(load_choice(dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn private_writes_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret");
        write_private(&p, b"x").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "group/other bits must be clear");
    }

    #[test]
    fn default_choice_is_the_pre_existing_behaviour() {
        // Existing installs must keep reading the secrets they already
        // have; the chooser is an offer, not a migration.
        assert_eq!(BackendChoice::default(), BackendChoice::OsDefault);
    }
}
