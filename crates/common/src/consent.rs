//! First-run consent store: three-tier upload authorization + settings data API.
//!
//! Implements the TSK-105 slice of DEC-010/DEC-011 and ARCHITECTURE §7:
//!
//! - Three authorization tiers ([`crate::ipc::ConsentTier`]): Tier1 cloud with
//!   training-data retention (`muse-spark-1.3-contributor`), Tier2 cloud
//!   upload-only ZDR (`mimo-v2.6-flash`), Tier3 local-only (`Bonsai-2-27B`,
//!   sole candidate). No tier is assumed before the user chooses.
//! - Persistence lives in the user directory (`%APPDATA%/SynthLM` on Windows,
//!   `~/Library/Application Support/SynthLM` on macOS, `$XDG_CONFIG_HOME`
//!   or `~/.config` on Linux; see [`crate::consent::user_dir`]), gated by `config_version`
//!   migration (see [`crate::consent::CONFIG_VERSION`]).
//! - Fail-safe ruling (same as `config.rs` `DEFAULT_TIER` docs, 2026-10-06):
//!   without stored consent nothing may leave the machine. A missing file, an
//!   unreadable file, or an unknown `config_version` all load as
//!   [`crate::consent::ConsentState::Undecided`] (fail-closed), and [`crate::consent::require_consent`]
//!   turns `Undecided` into a `consent_required` BLOCKED error. The
//!   interactive consent dialog / settings store here is authoritative; the
//!   model gateway must gate on it before any cloud call (AGENTS.md §8).
//! - First-run dialog ships as text ([`crate::consent::first_run_prompt_text`]) because the UI
//!   engine is undecided (graphical rendering stays in TSK-306); input
//!   parsing ([`crate::consent::parse_first_run_choice`]) is a pure function so the future UI
//!   reuses the same text and the same parser.
//!
//! ## Secrecy rules (AGENTS.md §3.7, §8; DEC-010/011)
//!
//! The consent file stores only tier + timestamp + version: no keys, no PCM,
//! no prompts, no paths. [`crate::consent::ConsentError`] carries no caller-supplied values
//! (not even truncated input), so formatting an error can never echo secrets.
//!
//! Blocking contract: [`crate::consent::load_consent`] / [`crate::consent::load_from_path`] perform file I/O
//! and [`crate::consent::save_consent`] / [`crate::consent::save_to_path`] perform file I/O plus an atomic
//! rename. Never call these from an audio thread (AGENTS.md red line 2);
//! they are startup / control-plane helpers. [`crate::consent::require_consent`] and
//! [`crate::consent::parse_first_run_choice`] are pure and safe anywhere (still pointless on
//! an audio thread).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ipc::{ConsentTier, ErrorCode};

// ---------------------------------------------------------------------------
// Constants (DEC-010 presets, DEC-027 placement)
// ---------------------------------------------------------------------------

/// Current consent-file schema version (ARCHITECTURE §7 `config_version`).
///
/// Files stamped with any other version load as [`crate::consent::ConsentState::Undecided`]
/// (fail-closed); [`crate::consent::save_to_path`] always stamps this version.
pub const CONFIG_VERSION: u32 = 1;

/// Consent-file name inside the user directory.
pub const CONSENT_FILE_NAME: &str = "consent.json";

/// Application directory name on Windows and macOS.
pub const APP_DIR_NAME: &str = "SynthLM";

/// Application directory name under XDG config home (lowercase convention).
pub const APP_DIR_NAME_XDG: &str = "synthlm";

// ---------------------------------------------------------------------------
// ConsentStore + ConsentState
// ---------------------------------------------------------------------------

/// Persisted consent record: the tier the user chose, when, and under which
/// schema version.
///
/// The on-disk `version` field is serialized as `config_version` per
/// ARCHITECTURE §7. `Debug` is safe to log: the struct holds no secrets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentStore {
    /// Consent tier authorizing cloud use (Tier3 = local only, never upload).
    pub tier: ConsentTier,
    /// Unix epoch seconds when the tier was last chosen.
    pub decided_at_unix: i64,
    /// Schema version (serialized as `config_version`; see [`crate::consent::CONFIG_VERSION`]).
    #[serde(rename = "config_version")]
    pub version: u32,
}

impl ConsentStore {
    /// Build a store for `tier`, stamped with the current time and
    /// [`crate::consent::CONFIG_VERSION`].
    pub fn new(tier: ConsentTier) -> Self {
        Self {
            tier,
            decided_at_unix: now_unix(),
            version: CONFIG_VERSION,
        }
    }

    /// Switch tiers (settings page / consent dialog): updates the tier and
    /// re-stamps the decision time. The version stays at [`crate::consent::CONFIG_VERSION`].
    pub fn set_tier(&mut self, tier: ConsentTier) {
        self.tier = tier;
        self.decided_at_unix = now_unix();
    }
}

/// Load outcome: either a stored decision or the undecided first-run state.
///
/// Missing files, unreadable files, and unknown `config_version` values all
/// collapse to [`crate::consent::ConsentState::Undecided`] (fail-closed: no consent is ever
/// inferred). Use [`crate::consent::require_consent`] to gate cloud calls on this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentState {
    /// No usable stored choice yet: show [`crate::consent::first_run_prompt_text`] and parse
    /// the reply with [`crate::consent::parse_first_run_choice`].
    Undecided,
    /// A stored choice under [`crate::consent::CONFIG_VERSION`].
    Decided(ConsentStore),
}

impl ConsentState {
    /// Whether a stored choice exists.
    pub fn is_decided(self) -> bool {
        matches!(self, ConsentState::Decided(_))
    }

    /// The stored tier, if decided.
    pub fn tier(self) -> Option<ConsentTier> {
        match self {
            ConsentState::Decided(store) => Some(store.tier),
            ConsentState::Undecided => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ConsentError: BLOCKED-only, value-free
// ---------------------------------------------------------------------------

/// Consent failure. Every variant is user-visible `BLOCKED` (see
/// [`ConsentError::blocked`]) and carries guidance; no variant stores
/// caller-supplied values, so formatting an error can never echo secrets or
/// input. The wire mapping reuses [`crate::ipc::ErrorCode`]: no new error
/// classification is introduced here.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ConsentError {
    /// No stored consent (first run, or fail-closed migration reset).
    #[error(
        "no upload consent stored: complete the first-run choice (1/2/3) or change it in settings; cloud calls stay BLOCKED (see DEC-010)"
    )]
    ConsentRequired,
    /// The consent file could not be written (user dir missing / read-only).
    /// Carries only the [`std::io::ErrorKind`]: never a path (AGENTS.md §8).
    #[error(
        "consent store not writable: re-check user-dir permissions and retry; the previous choice stays in effect (see DEC-027)"
    )]
    StoreIo {
        /// OS error kind behind the failure (no path attached).
        kind: std::io::ErrorKind,
    },
}

impl ConsentError {
    /// Map to the [`crate::ipc`] taxonomy. Both arms are terminal
    /// (`blocked`); consent problems are fixed by the user, never by retrying.
    pub fn code(self) -> ErrorCode {
        match self {
            ConsentError::ConsentRequired => ErrorCode::ConsentRequired,
            ConsentError::StoreIo { .. } => ErrorCode::Internal,
        }
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    ///
    /// Derived from [`ConsentError::code`] so the verdict cannot drift from
    /// the [`crate::ipc`] taxonomy; currently always `true`.
    pub fn blocked(self) -> bool {
        !self.code().retryable()
    }

    /// Static remediation hint for UI / BLOCKED surfaces (contains no secrets).
    pub fn guidance(self) -> &'static str {
        match self {
            ConsentError::ConsentRequired => {
                "choose a tier in the first-run prompt (1/2/3) or in settings; Tier3 keeps everything local (see DEC-010)"
            }
            ConsentError::StoreIo { .. } => {
                "make the user directory writable, then re-apply the tier in settings (see DEC-027)"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Gate: require_consent
// ---------------------------------------------------------------------------

/// Gate cloud calls on stored consent (fail-safe ruling, 2026-10-06).
///
/// Returns the stored tier when decided; [`ConsentError::ConsentRequired`]
/// (`consent_required` BLOCKED, never retried) when [`crate::consent::ConsentState::Undecided`].
/// The model gateway must call this before any cloud request.
///
/// # Errors
///
/// Returns [`ConsentError::ConsentRequired`] when `state` is undecided.
pub fn require_consent(state: &ConsentState) -> Result<ConsentTier, ConsentError> {
    match *state {
        ConsentState::Decided(store) => Ok(store.tier),
        ConsentState::Undecided => Err(ConsentError::ConsentRequired),
    }
}

// ---------------------------------------------------------------------------
// First-run text prompt (UI engine undecided; graphics stay in TSK-306)
// ---------------------------------------------------------------------------

/// First-run consent text: the three tiers, their models and implications.
///
/// Pure text (no stdin reads: the caller owns input, so the future graphical
/// UI reuses this exact wording). Parse replies with [`crate::consent::parse_first_run_choice`].
pub fn first_run_prompt_text() -> &'static str {
    "SynthLM 首次启动：请选择云端授权档（DEC-010/DEC-011）。\
     此选择保存在用户目录，可随时在设置页更改；未选择前，任何云端调用一律 BLOCKED。\n\
     \n\
     1) Tier1 —— 接受训练保留（云端）\n\
     模型：muse-spark-1.3-contributor。提示词与特征可上传，服务方可能保留数据用于训练。\n\
     \n\
     2) Tier2 —— 仅接受上传（云端 ZDR）\n\
     模型：mimo-v2.6-flash。不接受训练保留，仅为本次推理上传必要字段。\n\
     \n\
     3) Tier3 —— 不上传（本地）\n\
     模型：Bonsai-2-27B（本地端点，仅文本）。一切不出网；需要本地模型端点可用。\n\
     \n\
     默认不出网：原始音频默认不出网；可上传字段仅限白名单 [prompt, mir, meta, audio_ref]（DEC-011；audio_ref 仅 Tier1/Tier2，Tier3 纯文本）。\n\
     \n\
     请输入 1 / 2 / 3（设置页与 SYNTHLM_CONSENT_TIER 亦接受 tier1/tier2/tier3 写法）。"
}

/// Parse a first-run reply into a tier.
///
/// Accepts `1`/`2`/`3` (plus the `tier1`/`tier2`/`tier3` spellings shared with
/// [`crate::config::parse_consent_tier`], ASCII case-insensitive, surrounding
/// whitespace ignored) so prompt input, settings, and `SYNTHLM_CONSENT_TIER`
/// stay in sync by construction. Pure function: no I/O.
///
/// # Errors
///
/// Anything else yields [`ConsentError::ConsentRequired`] (BLOCKED +
/// guidance) without echoing the input.
pub fn parse_first_run_choice(raw: &str) -> Result<ConsentTier, ConsentError> {
    crate::config::parse_consent_tier(raw).map_err(|_| ConsentError::ConsentRequired)
}

// ---------------------------------------------------------------------------
// User-directory placement (DEC-027; hand-written platform branches)
// ---------------------------------------------------------------------------

/// SynthLM user directory (DEC-027; ARCHITECTURE §7).
///
/// - Windows: `%APPDATA%/SynthLM`
/// - macOS: `~/Library/Application Support/SynthLM`
/// - Linux/other: `$XDG_CONFIG_HOME/synthlm`, else `~/.config/synthlm`
///
/// Hand-written from environment variables (no `directories` crate, so no new
/// dependency and no `docs/LICENSES.md` entry). Returns `None` when no base
/// directory resolves; callers treat that as fail-closed ([`crate::consent::ConsentState::Undecided`]
/// on load, [`ConsentError::StoreIo`] on save) and surface the guidance.
pub fn user_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        nonempty_var_os("APPDATA").map(|base| PathBuf::from(base).join(APP_DIR_NAME))
    }
    #[cfg(target_os = "macos")]
    {
        nonempty_var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(APP_DIR_NAME)
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if let Some(xdg) = nonempty_var_os("XDG_CONFIG_HOME") {
            Some(PathBuf::from(xdg).join(APP_DIR_NAME_XDG))
        } else {
            nonempty_var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config").join(APP_DIR_NAME_XDG))
        }
    }
}

/// Non-empty OS environment value (`None` for absent or empty).
fn nonempty_var_os(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key).filter(|value| !value.is_empty())
}

/// Consent-file path inside `dir` (injectable placement for tests and the
/// portable fallback; production passes [`crate::consent::user_dir`]).
pub fn consent_file_path_in(dir: &Path) -> PathBuf {
    dir.join(CONSENT_FILE_NAME)
}

/// Consent-file path inside [`crate::consent::user_dir`] (`None` when no base resolves).
pub fn consent_file_path() -> Option<PathBuf> {
    user_dir().map(|dir| consent_file_path_in(&dir))
}

// ---------------------------------------------------------------------------
// Load / save (fail-closed load, atomic save)
// ---------------------------------------------------------------------------

/// Wall-clock Unix seconds (0 when the clock is unavailable).
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|span| i64::try_from(span.as_secs()).ok())
        .unwrap_or(0)
}

/// Load consent from `path`, fail-closed.
///
/// Missing files, I/O errors, malformed JSON, a missing `config_version`, an
/// unknown tier, or any version other than [`crate::consent::CONFIG_VERSION`] all yield
/// [`crate::consent::ConsentState::Undecided`]: consent is never inferred. Pure filesystem
/// read of a secret-free file; never touches the real user directory unless
/// the caller passes the real path.
pub fn load_from_path(path: &Path) -> ConsentState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return ConsentState::Undecided,
    };
    let store: ConsentStore = match serde_json::from_str(&text) {
        Ok(store) => store,
        Err(_) => return ConsentState::Undecided,
    };
    if store.version != CONFIG_VERSION {
        return ConsentState::Undecided;
    }
    ConsentState::Decided(store)
}

/// Load consent from the real user directory ([`crate::consent::consent_file_path`]).
///
/// Fail-closed like [`crate::consent::load_from_path`]: an unresolvable directory also yields
/// [`crate::consent::ConsentState::Undecided`].
pub fn load_consent() -> ConsentState {
    match consent_file_path() {
        Some(path) => load_from_path(&path),
        None => ConsentState::Undecided,
    }
}

/// Save `store` to `path`, creating parent directories and writing atomically
/// (temp file + rename in the same directory).
///
/// The written file always stamps [`crate::consent::CONFIG_VERSION`], so a successful save
/// heals files from older schemas. Never touches the real user directory
/// unless the caller passes the real path.
///
/// # Errors
///
/// Returns [`ConsentError::StoreIo`] (BLOCKED + guidance, path-free) when
/// serialization, directory creation, the write, or the rename fails. The
/// previous file (if any) is left untouched on failure.
pub fn save_to_path(path: &Path, store: &ConsentStore) -> Result<(), ConsentError> {
    let io_err = |e: std::io::Error| ConsentError::StoreIo { kind: e.kind() };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    // Stamp the current schema on write so saved files always load back.
    let outgoing = ConsentStore {
        version: CONFIG_VERSION,
        ..*store
    };
    let text = serde_json::to_string_pretty(&outgoing).map_err(|_| ConsentError::StoreIo {
        kind: std::io::ErrorKind::InvalidData,
    })?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(io_err)?;
    std::fs::rename(&tmp, path).map_err(io_err)?;
    Ok(())
}

/// Save `store` to the real user directory ([`crate::consent::consent_file_path`]).
///
/// This is the settings-page write path: parse with [`crate::consent::parse_first_run_choice`],
/// build with [`crate::consent::ConsentStore::new`] (or mutate + [`crate::consent::save_consent`]), and the
/// next [`crate::consent::load_consent`] observes the new tier.
///
/// # Errors
///
/// Returns [`ConsentError::StoreIo`] when no user directory resolves or the
/// write fails; the previous choice stays in effect.
pub fn save_consent(store: &ConsentStore) -> Result<(), ConsentError> {
    match consent_file_path() {
        Some(path) => save_to_path(&path, store),
        None => Err(ConsentError::StoreIo {
            kind: std::io::ErrorKind::NotFound,
        }),
    }
}

// ---------------------------------------------------------------------------
// Explicit config_version migration (TSK-605; DEC-027, ARCHITECTURE §7)
// ---------------------------------------------------------------------------

/// How a stored `config_version` compares to [`crate::consent::CONFIG_VERSION`].
///
/// [`crate::consent::load_from_path`] stays fail-closed for any non-current
/// version; use [`crate::consent::migrate_store`] (or
/// [`crate::consent::migrate_json`]) when the caller explicitly wants the
/// migrate path instead of the undecided reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionClass {
    /// Older than [`crate::consent::CONFIG_VERSION`]: migratable.
    Older,
    /// Equal to [`crate::consent::CONFIG_VERSION`]: no migration needed.
    Current,
    /// Newer than [`crate::consent::CONFIG_VERSION`]: unknown, must be refused.
    Newer,
}

/// Classify a stored `config_version` against [`crate::consent::CONFIG_VERSION`].
pub fn classify_version(found: u32) -> VersionClass {
    if found < CONFIG_VERSION {
        VersionClass::Older
    } else if found == CONFIG_VERSION {
        VersionClass::Current
    } else {
        VersionClass::Newer
    }
}

/// Explicit migration failure. Variants carry no caller material (only the
/// refused numeric version), so formatting an error can never echo secrets,
/// prompts, or paths (AGENTS.md §8).
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum MigrateError {
    /// The record's `config_version` is newer than this build supports.
    /// Carries the refused version number only.
    #[error(
        "unknown config_version: newer than this build supports; refusing to migrate (see DEC-027)"
    )]
    UnknownVersion {
        /// The refused `config_version` value.
        found: u32,
    },
    /// The record is not valid JSON or misses required fields (a missing
    /// `config_version` or an unknown tier spelling, which is never guessed).
    #[error(
        "consent record is not valid JSON or misses required fields; refusing to guess (see DEC-027)"
    )]
    Corrupt,
}

impl MigrateError {
    /// Map to the [`crate::ipc`] taxonomy. Both arms are terminal
    /// (`blocked`); migration problems are fixed by the user (upgrade /
    /// re-choose), never by retrying.
    pub fn code(self) -> ErrorCode {
        match self {
            MigrateError::UnknownVersion { .. } | MigrateError::Corrupt => ErrorCode::Internal,
        }
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    ///
    /// Derived from [`MigrateError::code`] so the verdict cannot drift from
    /// the [`crate::ipc`] taxonomy; currently always `true`.
    pub fn blocked(self) -> bool {
        !self.code().retryable()
    }

    /// Static remediation hint for UI / BLOCKED surfaces (contains no secrets).
    pub fn guidance(self) -> &'static str {
        match self {
            MigrateError::UnknownVersion { .. } => {
                "the stored config is newer than this build; upgrade SynthLM, then retry (see DEC-027)"
            }
            MigrateError::Corrupt => {
                "remove the stored consent and re-choose a tier in the first-run prompt (1/2/3); Tier3 keeps everything local (see DEC-010)"
            }
        }
    }
}

/// Migrate one parsed [`crate::consent::ConsentStore`] to
/// [`crate::consent::CONFIG_VERSION`].
///
/// - [`crate::consent::VersionClass::Current`] passes through unchanged.
/// - `Older` preserves the stored tier and decision time and re-stamps the
///   current version (v0 shares the v1 field layout, so migration is a
///   re-stamp; future layouts add per-version transforms here).
/// - `Newer` is refused with [`crate::consent::MigrateError::UnknownVersion`]
///   (fail-closed: a newer schema is never guessed).
///
/// # Errors
///
/// Returns [`crate::consent::MigrateError::UnknownVersion`] when the record is
/// newer than this build.
pub fn migrate_store(store: &ConsentStore) -> Result<ConsentStore, MigrateError> {
    match classify_version(store.version) {
        VersionClass::Current => Ok(*store),
        VersionClass::Older => Ok(ConsentStore {
            tier: store.tier,
            decided_at_unix: store.decided_at_unix,
            version: CONFIG_VERSION,
        }),
        VersionClass::Newer => Err(MigrateError::UnknownVersion {
            found: store.version,
        }),
    }
}

/// Parse consent JSON text and migrate it to [`crate::consent::CONFIG_VERSION`].
///
/// This is the explicit counterpart to the fail-closed
/// [`crate::consent::load_from_path`]: malformed JSON, a missing
/// `config_version`, or an unknown tier spelling yields
/// [`crate::consent::MigrateError::Corrupt`] (never a guessed tier); a newer
/// version yields [`crate::consent::MigrateError::UnknownVersion`]. Pure
/// function: no I/O.
///
/// # Errors
///
/// Returns [`crate::consent::MigrateError::Corrupt`] for unparsable records
/// and [`crate::consent::MigrateError::UnknownVersion`] for newer versions.
pub fn migrate_json(text: &str) -> Result<ConsentStore, MigrateError> {
    let store: ConsentStore = serde_json::from_str(text).map_err(|_| MigrateError::Corrupt)?;
    migrate_store(&store)
}

// ---------------------------------------------------------------------------
// Unwritable user-dir fallback (TSK-605; DEC-027 reversal, ARCHITECTURE §7)
// ---------------------------------------------------------------------------

/// Project-relative fallback directory name (DEC-027 reversal: user dir not
/// writable → project-relative dir + explicit notice).
///
/// Joined onto the caller's project directory; the consent file keeps its
/// [`crate::consent::CONSENT_FILE_NAME`] name inside it.
pub const FALLBACK_DIR_NAME: &str = ".synthlm";

/// Opaque fingerprint of a directory path (FNV-1a 64, 16 lowercase hex chars).
///
/// Used in fallback notices so the failed location stays identifiable without
/// ever printing an absolute path (AGENTS.md §8). Deterministic for one path;
/// reveals nothing about the path contents.
pub fn path_fingerprint(path: &Path) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Whether `dir` can host state: creatable plus one probe byte round-trip.
///
/// Any failure (missing base, permission denied, a file blocking the path)
/// yields `false`. Creates `dir` when absent and removes the probe file
/// afterwards; the probe name carries only the pid.
pub fn dir_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".synthlm-write-probe-{}", std::process::id()));
    if std::fs::write(&probe, b"1").is_err() {
        return false;
    }
    let _ = std::fs::remove_file(&probe);
    true
}

/// Where consent state landed: the user dir, or the project-relative fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPlacement {
    /// Directory hosting [`crate::consent::CONSENT_FILE_NAME`].
    pub dir: PathBuf,
    /// Whether the project-relative fallback was used.
    pub fell_back: bool,
    /// Actionable notice when [`crate::consent::ResolvedPlacement::fell_back`] is set
    /// (relative names + fingerprint only, never an absolute path).
    pub notice: Option<String>,
}

impl ResolvedPlacement {
    /// Consent-file path inside the resolved directory.
    pub fn consent_path(&self) -> PathBuf {
        consent_file_path_in(&self.dir)
    }
}

/// Build the fallback notice for a failed user-dir base (relative names and
/// the opaque [`crate::consent::path_fingerprint`] only; never an absolute path).
fn fallback_notice(failed: Option<&Path>) -> String {
    let id = match failed {
        Some(path) => path_fingerprint(path),
        None => "unresolved".to_owned(),
    };
    format!(
        "user directory not writable (dir fingerprint {id}); fell back to project-relative `{FALLBACK_DIR_NAME}/`. Make the user directory writable, then re-apply the tier in settings so the choice persists there (see DEC-027)."
    )
}

/// Resolve the consent directory against an injected user-dir base.
///
/// A writable `base` wins; an unresolvable or unwritable base falls back to
/// `project_dir` joined with [`crate::consent::FALLBACK_DIR_NAME`], with an
/// actionable notice. Besides the [`crate::consent::dir_writable`] probe no
/// consent file is read or written here.
pub fn resolve_user_dir_in(base: Option<&Path>, project_dir: &Path) -> ResolvedPlacement {
    if let Some(dir) = base
        && dir_writable(dir)
    {
        return ResolvedPlacement {
            dir: dir.to_path_buf(),
            fell_back: false,
            notice: None,
        };
    }
    ResolvedPlacement {
        dir: project_dir.join(FALLBACK_DIR_NAME),
        fell_back: true,
        notice: Some(fallback_notice(base)),
    }
}

/// Resolve the consent directory against the real [`crate::consent::user_dir`].
///
/// Same fallback contract as [`crate::consent::resolve_user_dir_in`];
/// `project_dir` is the caller's project root (the fallback lives directly
/// beneath it).
pub fn resolve_user_dir(project_dir: &Path) -> ResolvedPlacement {
    let base = user_dir();
    resolve_user_dir_in(base.as_deref(), project_dir)
}

/// Outcome of a save that may have used the project-relative fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveOutcome {
    /// Whether the project-relative fallback received the file.
    pub fell_back: bool,
    /// Actionable notice when [`crate::consent::SaveOutcome::fell_back`] is set
    /// (relative names + fingerprint only, never an absolute path).
    pub notice: Option<String>,
}

/// Save `store` to `primary`, falling back to the project-relative dir on any
/// I/O failure.
///
/// The primary write keeps the [`crate::consent::save_to_path`] guarantee (a
/// failed save leaves the previous file untouched) before the fallback is
/// attempted; the fallback file keeps the [`crate::consent::CONSENT_FILE_NAME`]
/// name inside `project_dir` joined with [`crate::consent::FALLBACK_DIR_NAME`].
///
/// # Errors
///
/// Returns the fallback's [`ConsentError::StoreIo`] when both writes fail.
pub fn save_to_path_with_fallback(
    primary: &Path,
    project_dir: &Path,
    store: &ConsentStore,
) -> Result<SaveOutcome, ConsentError> {
    if save_to_path(primary, store).is_ok() {
        return Ok(SaveOutcome {
            fell_back: false,
            notice: None,
        });
    }
    let fallback_dir = project_dir.join(FALLBACK_DIR_NAME);
    save_to_path(&consent_file_path_in(&fallback_dir), store)?;
    Ok(SaveOutcome {
        fell_back: true,
        notice: Some(fallback_notice(primary.parent())),
    })
}

/// Save `store` to the real user directory, falling back to the
/// project-relative dir when the user dir is unresolvable or unwritable.
///
/// Settings-page write path with the DEC-027 reversal built in: prefer
/// [`crate::consent::save_consent`]'s placement, keep the choice (in the
/// fallback) instead of dropping it, and surface the notice.
///
/// # Errors
///
/// Returns [`ConsentError::StoreIo`] when both placements fail; the previous
/// choice stays in effect.
pub fn save_consent_with_fallback(
    project_dir: &Path,
    store: &ConsentStore,
) -> Result<SaveOutcome, ConsentError> {
    match consent_file_path() {
        Some(primary) => {
            save_to_path_with_fallback(&primary, &project_dir.join(FALLBACK_DIR_NAME), store)
        }
        None => {
            let fallback_dir = project_dir.join(FALLBACK_DIR_NAME);
            save_to_path(&consent_file_path_in(&fallback_dir), store)?;
            Ok(SaveOutcome {
                fell_back: true,
                notice: Some(fallback_notice(None)),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (synthetic values only; the real user directory is never touched)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch directory for one test (no `tempfile` dependency; the
    /// real user directory is never touched, per the task constraints).
    fn unique_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("synthlm-t105-{pid}-{tag}"))
    }

    fn remove_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tiers_switch_through_gate() {
        let mut store = ConsentStore::new(ConsentTier::Tier1);
        assert_eq!(store.tier, ConsentTier::Tier1);
        assert_eq!(store.version, CONFIG_VERSION);

        let state = ConsentState::Decided(store);
        assert!(state.is_decided());
        assert_eq!(state.tier(), Some(ConsentTier::Tier1));
        assert_eq!(require_consent(&state), Ok(ConsentTier::Tier1));

        // Settings-page switch Tier1 -> Tier2 -> Tier3 round-trips the gate.
        store.set_tier(ConsentTier::Tier2);
        assert_eq!(
            require_consent(&ConsentState::Decided(store)),
            Ok(ConsentTier::Tier2)
        );
        store.set_tier(ConsentTier::Tier3);
        assert_eq!(
            require_consent(&ConsentState::Decided(store)),
            Ok(ConsentTier::Tier3)
        );
        assert_eq!(store.version, CONFIG_VERSION);
    }

    #[test]
    fn persistence_roundtrip_in_injected_dir() {
        let dir = unique_dir("roundtrip");
        remove_dir(&dir);
        let path = consent_file_path_in(&dir);

        let mut store = ConsentStore::new(ConsentTier::Tier2);
        save_to_path(&path, &store).expect("save in scratch dir");
        let loaded = load_from_path(&path);
        assert_eq!(loaded, ConsentState::Decided(store));

        // Switching tiers persists: the next load observes the new tier.
        store.set_tier(ConsentTier::Tier1);
        save_to_path(&path, &store).expect("overwrite in scratch dir");
        let reloaded = load_from_path(&path);
        assert_eq!(reloaded.tier(), Some(ConsentTier::Tier1));
        assert_eq!(reloaded, ConsentState::Decided(store));

        // On-disk shape pins the ARCH §7 key: `config_version`, tier string.
        let text = std::fs::read_to_string(&path).expect("read back scratch file");
        assert!(text.contains("\"config_version\""), "on-disk key: {text}");
        assert!(text.contains("\"tier1\""), "tier wire string: {text}");

        remove_dir(&dir);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = unique_dir("nested").join("a").join("b");
        remove_dir(&dir);
        let path = consent_file_path_in(&dir);

        save_to_path(&path, &ConsentStore::new(ConsentTier::Tier3))
            .expect("nested parents created");
        assert_eq!(load_from_path(&path).tier(), Some(ConsentTier::Tier3));

        let top = unique_dir("nested");
        remove_dir(&top);
    }

    #[test]
    fn unknown_version_and_bad_files_fail_closed() {
        let dir = unique_dir("failclosed");
        remove_dir(&dir);
        let path = consent_file_path_in(&dir);

        // Missing file is first-run undecided.
        assert_eq!(load_from_path(&path), ConsentState::Undecided);

        std::fs::create_dir_all(&dir).expect("scratch dir");
        // Corrupt JSON never grants consent.
        std::fs::write(&path, "{not json").expect("scratch write");
        assert_eq!(load_from_path(&path), ConsentState::Undecided);
        // Unknown future schema resets to undecided (fail-closed migration).
        let future = serde_json::json!({
            "config_version": CONFIG_VERSION + 99,
            "tier": "tier1",
            "decided_at_unix": 1_789_000_000,
        });
        std::fs::write(&path, future.to_string()).expect("scratch write");
        assert_eq!(load_from_path(&path), ConsentState::Undecided);
        // Missing version key is equally undecided.
        let no_version = serde_json::json!({"tier": "tier1", "decided_at_unix": 1});
        std::fs::write(&path, no_version.to_string()).expect("scratch write");
        assert_eq!(load_from_path(&path), ConsentState::Undecided);
        // Unknown tier string is undecided, not a guessed tier.
        let bad_tier = serde_json::json!({
            "config_version": CONFIG_VERSION,
            "tier": "tier9",
            "decided_at_unix": 1,
        });
        std::fs::write(&path, bad_tier.to_string()).expect("scratch write");
        assert_eq!(load_from_path(&path), ConsentState::Undecided);
        // A current-version file written by an older schema heals on save.
        let stale = ConsentStore {
            tier: ConsentTier::Tier3,
            decided_at_unix: 1_789_000_000,
            version: 0,
        };
        save_to_path(&path, &stale).expect("save heals version");
        let healed = load_from_path(&path);
        assert_eq!(healed.tier(), Some(ConsentTier::Tier3));
        match healed {
            ConsentState::Decided(saved) => assert_eq!(saved.version, CONFIG_VERSION),
            ConsentState::Undecided => panic!("healed save must load"),
        }

        remove_dir(&unique_dir("failclosed"));
    }

    #[test]
    fn undecided_gate_is_consent_required_blocked() {
        let state = ConsentState::Undecided;
        assert!(!state.is_decided());
        assert_eq!(state.tier(), None);

        let err = require_consent(&state).expect_err("undecided must BLOCK");
        assert_eq!(err, ConsentError::ConsentRequired);
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(err.blocked());
        assert!(!err.code().retryable());
        assert!(!err.guidance().is_empty());
    }

    #[test]
    fn prompt_text_covers_all_tiers() {
        let text = first_run_prompt_text();
        for needle in [
            "muse-spark-1.3-contributor",
            "mimo-v2.6-flash",
            "Bonsai-2-27B",
            "prompt",
            "mir",
            "meta",
            "设置",
            "BLOCKED",
        ] {
            assert!(text.contains(needle), "prompt must mention {needle}");
        }
        for digit in ["1", "2", "3"] {
            assert!(text.contains(digit), "prompt must list choice {digit}");
        }
    }

    #[test]
    fn choice_parsing_accepts_all_tiers() {
        assert_eq!(parse_first_run_choice("1"), Ok(ConsentTier::Tier1));
        assert_eq!(parse_first_run_choice("2"), Ok(ConsentTier::Tier2));
        assert_eq!(parse_first_run_choice("3"), Ok(ConsentTier::Tier3));
        assert_eq!(parse_first_run_choice("  2  "), Ok(ConsentTier::Tier2));
        assert_eq!(parse_first_run_choice("TIER3"), Ok(ConsentTier::Tier3));
        assert_eq!(parse_first_run_choice("tier1"), Ok(ConsentTier::Tier1));
    }

    #[test]
    fn choice_parsing_rejects_garbage_without_echo() {
        const SYNTHETIC_SECRET: &str = "tsk105-synthetic-secret-CCCC";
        for raw in ["", "0", "4", "yes", "tier9", SYNTHETIC_SECRET] {
            let err = parse_first_run_choice(raw).expect_err("bad choice must BLOCK");
            assert_eq!(err, ConsentError::ConsentRequired);
            assert_eq!(err.code(), ErrorCode::ConsentRequired);
            assert!(err.blocked());
        }
        // Value-free error: a secret-shaped input never surfaces in any
        // rendering. (Single-char inputs like "0" are not checked for
        // echo: they legitimately occur inside the static guidance text
        // "1/2/3" / "DEC-010", the same rationale as the config.rs tier test,
        // which only asserts non-echo for the long synthetic key.)
        let err = parse_first_run_choice(SYNTHETIC_SECRET).expect_err("secret must BLOCK");
        assert!(!format!("{err}").contains(SYNTHETIC_SECRET));
        assert!(!format!("{err:?}").contains(SYNTHETIC_SECRET));
        assert!(!err.guidance().contains(SYNTHETIC_SECRET));
    }

    #[test]
    fn store_io_maps_to_blocked_internal() {
        let err = ConsentError::StoreIo {
            kind: std::io::ErrorKind::NotFound,
        };
        assert_eq!(err.code(), ErrorCode::Internal);
        assert!(err.blocked());
        assert!(!err.code().retryable());
        assert!(!err.guidance().is_empty());
    }

    #[test]
    fn consent_file_path_in_appends_filename() {
        let dir = Path::new("some-dir");
        assert_eq!(consent_file_path_in(dir), dir.join(CONSENT_FILE_NAME));
    }

    #[test]
    fn all_consent_errors_are_blocked() {
        for err in [
            ConsentError::ConsentRequired,
            ConsentError::StoreIo {
                kind: std::io::ErrorKind::PermissionDenied,
            },
        ] {
            assert!(err.blocked(), "{err:?} must be BLOCKED");
            assert!(!err.code().retryable(), "{err:?} must never retry");
            assert!(!err.guidance().is_empty());
        }
    }

    #[test]
    fn version_classification_covers_older_current_newer() {
        assert_eq!(classify_version(0), VersionClass::Older);
        assert_eq!(classify_version(CONFIG_VERSION), VersionClass::Current);
        assert_eq!(classify_version(CONFIG_VERSION + 1), VersionClass::Newer);
        assert_eq!(classify_version(CONFIG_VERSION + 99), VersionClass::Newer);
        assert_eq!(classify_version(u32::MAX), VersionClass::Newer);
    }

    #[test]
    fn migrate_matrix_equal_old_new() {
        // Equal: every tier passes through unchanged.
        for tier in [ConsentTier::Tier1, ConsentTier::Tier2, ConsentTier::Tier3] {
            let current = ConsentStore {
                tier,
                decided_at_unix: 1_789_000_000,
                version: CONFIG_VERSION,
            };
            assert_eq!(migrate_store(&current), Ok(current));
            let text = serde_json::to_string(&current).expect("serialize current");
            assert_eq!(migrate_json(&text), Ok(current));
        }
        // Old: tier + decision time preserved, version re-stamped.
        let old = ConsentStore {
            tier: ConsentTier::Tier2,
            decided_at_unix: 1_789_000_001,
            version: 0,
        };
        let migrated = migrate_store(&old).expect("v0 must migrate");
        assert_eq!(migrated.tier, ConsentTier::Tier2);
        assert_eq!(migrated.decided_at_unix, 1_789_000_001);
        assert_eq!(migrated.version, CONFIG_VERSION);
        let old_text = serde_json::json!({
            "config_version": 0,
            "tier": "tier2",
            "decided_at_unix": 1_789_000_001,
        })
        .to_string();
        assert_eq!(migrate_json(&old_text), Ok(migrated));
        // New: refused, BLOCKED, never guessed.
        for found in [CONFIG_VERSION + 1, CONFIG_VERSION + 99] {
            let future = ConsentStore {
                tier: ConsentTier::Tier1,
                decided_at_unix: 1,
                version: found,
            };
            let err = migrate_store(&future).expect_err("newer schema must be refused");
            assert_eq!(err, MigrateError::UnknownVersion { found });
            assert_eq!(err.code(), ErrorCode::Internal);
            assert!(err.blocked());
            assert!(!err.guidance().is_empty());
            let future_text = serde_json::json!({
                "config_version": found,
                "tier": "tier1",
                "decided_at_unix": 1,
            })
            .to_string();
            assert_eq!(
                migrate_json(&future_text),
                Err(MigrateError::UnknownVersion { found })
            );
        }
    }

    #[test]
    fn migrate_json_rejects_corrupt_without_echo() {
        const SYNTHETIC_SECRET: &str = "tsk605-synthetic-secret-DDDD";
        // Malformed JSON.
        let err = migrate_json("{not json").expect_err("malformed must fail");
        assert_eq!(err, MigrateError::Corrupt);
        // Missing config_version: never defaulted.
        let err = migrate_json(r#"{"tier":"tier1","decided_at_unix":1}"#)
            .expect_err("missing version must fail");
        assert_eq!(err, MigrateError::Corrupt);
        // Unknown tier spelling: never guessed, even when secret-shaped.
        let smuggled = format!(
            "{{\"config_version\":{CONFIG_VERSION},\"tier\":\"{SYNTHETIC_SECRET}\",\"decided_at_unix\":1}}"
        );
        let err = migrate_json(&smuggled).expect_err("unknown tier must fail");
        assert_eq!(err, MigrateError::Corrupt);
        assert!(!format!("{err}").contains(SYNTHETIC_SECRET));
        assert!(!format!("{err:?}").contains(SYNTHETIC_SECRET));
        for err in [
            MigrateError::Corrupt,
            MigrateError::UnknownVersion {
                found: CONFIG_VERSION + 1,
            },
        ] {
            assert!(err.blocked(), "{err:?} must be BLOCKED");
            assert!(!err.code().retryable(), "{err:?} must never retry");
            assert!(!err.guidance().is_empty());
        }
    }

    /// Unique scratch project dir for fallback tests (the real user directory
    /// is never touched).
    fn project_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("synthlm-t605-{pid}-{tag}"))
    }

    #[test]
    fn writable_base_wins_without_fallback() {
        let base = project_dir("base-ok");
        remove_dir(&base);
        let placement = resolve_user_dir_in(Some(base.as_path()), &project_dir("proj-unused"));
        assert!(!placement.fell_back);
        assert_eq!(placement.notice, None);
        assert_eq!(placement.dir, base);
        assert_eq!(placement.consent_path(), base.join(CONSENT_FILE_NAME));
        remove_dir(&base);
    }

    #[test]
    fn unresolved_base_falls_back_with_path_free_notice() {
        let project = project_dir("proj-fallback");
        remove_dir(&project);
        let placement = resolve_user_dir_in(None, &project);
        assert!(placement.fell_back);
        assert_eq!(placement.dir, project.join(FALLBACK_DIR_NAME));
        let notice = placement.notice.expect("fallback must explain itself");
        assert!(notice.contains(FALLBACK_DIR_NAME), "relative dir: {notice}");
        assert!(notice.contains("fingerprint"), "opaque id: {notice}");
        let project_text = project.to_string_lossy();
        assert!(
            !notice.contains(project_text.as_ref()),
            "no absolute path: {notice}"
        );
        remove_dir(&project);
    }

    #[test]
    fn unwritable_base_falls_back_to_project_relative() {
        // A regular file where a directory is expected: directory creation
        // fails on every OS, which simulates "user dir not writable" without
        // touching permissions or the real user directory.
        let scratch = project_dir("base-blocked");
        remove_dir(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let blocker = scratch.join("blocker");
        std::fs::write(&blocker, b"x").expect("blocker file");
        let project = project_dir("proj-blocked");
        remove_dir(&project);

        assert!(!dir_writable(&blocker));
        let placement = resolve_user_dir_in(Some(blocker.as_path()), &project);
        assert!(placement.fell_back);
        assert_eq!(placement.dir, project.join(FALLBACK_DIR_NAME));
        let notice = placement.notice.expect("fallback notice");
        let scratch_text = scratch.to_string_lossy();
        assert!(
            !notice.contains(scratch_text.as_ref()),
            "no absolute path: {notice}"
        );

        remove_dir(&scratch);
        remove_dir(&project);
    }

    #[test]
    fn dir_writable_probes_create_and_write() {
        let fresh = project_dir("writable").join("sub");
        remove_dir(&fresh);
        assert!(dir_writable(&fresh));
        assert!(fresh.is_dir());
        // A file path is never a writable dir.
        let file = project_dir("writable-file");
        remove_dir(&file);
        std::fs::write(&file, b"x").expect("scratch file");
        assert!(!dir_writable(&file));
        remove_dir(&project_dir("writable"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn fingerprint_is_stable_short_and_opaque() {
        let left = Path::new("some-dir");
        let fp = path_fingerprint(left);
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(path_fingerprint(left), fp, "deterministic");
        assert_ne!(
            path_fingerprint(left),
            path_fingerprint(Path::new("other-dir"))
        );
        assert!(!fp.contains("some-dir"));
    }

    #[test]
    fn save_prefers_primary_and_falls_back_only_on_failure() {
        let scratch = project_dir("save-fb");
        remove_dir(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        // Writable primary: no fallback.
        let primary = consent_file_path_in(&scratch.join("primary"));
        let outcome = save_to_path_with_fallback(
            &primary,
            &scratch.join("unused-project"),
            &ConsentStore::new(ConsentTier::Tier1),
        )
        .expect("primary save");
        assert!(!outcome.fell_back);
        assert_eq!(outcome.notice, None);
        assert_eq!(load_from_path(&primary).tier(), Some(ConsentTier::Tier1));

        // Blocked primary (file where the parent dir should be): fallback wins.
        let blocker = scratch.join("blocker");
        std::fs::write(&blocker, b"x").expect("blocker file");
        let bad_primary = blocker.join(CONSENT_FILE_NAME);
        let project = scratch.join("project");
        let outcome = save_to_path_with_fallback(
            &bad_primary,
            &project,
            &ConsentStore::new(ConsentTier::Tier2),
        )
        .expect("fallback save");
        assert!(outcome.fell_back);
        let notice = outcome.notice.expect("fallback notice");
        assert!(notice.contains(FALLBACK_DIR_NAME), "relative dir: {notice}");
        let scratch_text = scratch.to_string_lossy();
        assert!(
            !notice.contains(scratch_text.as_ref()),
            "no absolute path: {notice}"
        );
        let fallback_file = consent_file_path_in(&project.join(FALLBACK_DIR_NAME));
        assert_eq!(
            load_from_path(&fallback_file).tier(),
            Some(ConsentTier::Tier2)
        );

        // Both blocked: the error surfaces, nothing invented.
        let blocker2 = scratch.join("blocker2");
        std::fs::write(&blocker2, b"x").expect("blocker file");
        let err = save_to_path_with_fallback(
            &bad_primary,
            &blocker2.join("proj"),
            &ConsentStore::new(ConsentTier::Tier3),
        )
        .expect_err("double failure must BLOCK");
        assert!(err.blocked());

        remove_dir(&scratch);
    }
}
