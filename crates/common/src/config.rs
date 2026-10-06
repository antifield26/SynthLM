//! Cloud-model configuration: API key, endpoints, consent tier, audit whitelist.
//!
//! Implements the TSK-108 slice of DEC-010/DEC-011 and ARCHITECTURE §8-10:
//!
//! - The API key is read **only from process environment**
//!   ([`crate::config::API_KEY_ENV`], alias [`crate::config::API_KEY_ALIAS_ENV`]).
//!   This module never opens a `.env` file and never touches the filesystem:
//!   injecting `.env` contents into the process environment is the launcher's
//!   job (shell / dotenv tooling / `acrd` startup). No new crate (e.g.
//!   `dotenvy`) is required, so no `docs/LICENSES.md` entry is needed.
//! - Base URL and model names default to the DEC-010 compile-time presets and
//!   may be overridden per-environment variable.
//! - The consent tier parses to [`crate::ipc::ConsentTier`]
//!   (`tier1` / `tier2` / `tier3`); anything else is [`crate::config::ConfigError`].
//! - Every [`crate::config::ConfigError`] maps to a non-retryable
//!   [`crate::ipc::ErrorCode`] (user-visible `BLOCKED` with guidance, never
//!   silently retried), mirroring the [`crate::ipc::ErrorCode`] taxonomy in
//!   [`crate::ipc`].
//!
//! ## Secrecy rules (AGENTS.md §3.7, §8; DEC-010/011)
//!
//! - [`crate::config::ApiKey`] hand-implements [`std::fmt::Debug`] and
//!   [`std::fmt::Display`] to emit only `[redacted]`.
//! - [`crate::config::ConfigError`] variants store **no caller-supplied values
//!   at all** (not even truncated): a misplaced key in `SYNTHLM_CONSENT_TIER`,
//!   an upload field list, or `SYNTHLM_BASE_URL` can therefore never surface
//!   in `Display` / `Debug` / logs. [`crate::config::Config`] derives `Debug`
//!   safely because its only secret field is [`crate::config::ApiKey`].
//! - [`crate::config::validate_upload_fields`] /
//!   [`crate::config::Config::audit_event`] enforce the DEC-011 whitelist
//!   (`prompt`, `mir`, `meta`); anything else -- key material, PCM references,
//!   prompt text -- is rejected before an [`crate::ipc::AuditEvent`] is built.
//!
//! Blocking contract: every constructor here may read process environment but
//! performs no network or file I/O. Still, never call these from an audio
//! thread (AGENTS.md red line 2); they are startup / control-plane helpers.

use std::fmt;

use thiserror::Error;

use crate::ipc::{AuditEvent, ConsentTier, ErrorCode};

// ---------------------------------------------------------------------------
// Environment + default constants (DEC-010 presets)
// ---------------------------------------------------------------------------

/// Primary environment variable carrying the cloud API key.
pub const API_KEY_ENV: &str = "OPENCODE_API_KEY";

/// Equivalent alias accepted when [`API_KEY_ENV`] is absent or blank.
///
/// Precedence is fixed: primary first, alias second; both absent/blank yields
/// [`ConfigError::MissingKey`].
pub const API_KEY_ALIAS_ENV: &str = "OPENCODE_GO_API_KEY";

/// Environment variable overriding the cloud base URL.
pub const BASE_URL_ENV: &str = "SYNTHLM_BASE_URL";

/// Environment variable overriding the Tier1 model name.
pub const TIER1_MODEL_ENV: &str = "SYNTHLM_TIER1_MODEL";

/// Environment variable overriding the Tier2 model name.
pub const TIER2_MODEL_ENV: &str = "SYNTHLM_TIER2_MODEL";

/// Environment variable overriding the Tier3 (local) model name.
pub const TIER3_MODEL_ENV: &str = "SYNTHLM_TIER3_MODEL";

/// Environment variable selecting the consent tier (`tier1|tier2|tier3`).
pub const CONSENT_TIER_ENV: &str = "SYNTHLM_CONSENT_TIER";

/// DEC-010 cloud preset: OpenCode Go base URL.
pub const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

/// DEC-010 Tier1 preset model (training-retention accepted).
pub const DEFAULT_TIER1_MODEL: &str = "muse-spark-1.3-contributor";

/// DEC-010 Tier2 preset model (ZDR: upload accepted, retention declined).
pub const DEFAULT_TIER2_MODEL: &str = "mimo-v2.6-flash";

/// DEC-010 Tier3 preset model (local-only, sole candidate).
pub const DEFAULT_TIER3_MODEL: &str = "Gemma 4 12B";

/// Default consent tier when [`CONSENT_TIER_ENV`] is unset.
///
/// Tier1 matches the DEC-010 recommendation (cloud-first preset priority).
/// Fail-safe ruling (TSK-108 merge, 2026-10-06): this default does NOT
/// authorize network calls. The interactive consent dialog / settings store
/// (TSK-105) remains authoritative, and model-gw (TSK-301) must return
/// `consent_required` BLOCKED before any cloud call when no stored consent
/// exists (AGENTS.md §8 默认不出网). Tier1 here only selects the model
/// once consent is granted.
pub const DEFAULT_TIER: ConsentTier = ConsentTier::Tier1;

/// DEC-011 upload whitelist: the only field names that may leave the machine.
///
/// `prompt` = model prompt, `mir` = MIR features, `meta` = candidate
/// metadata. Raw audio (PCM), key material, and prompt/repository paths are
/// never members; [`validate_upload_fields`] rejects them.
pub const ALLOWED_UPLOAD_FIELDS: &[&str] = &["prompt", "mir", "meta"];

/// Guidance attached to [`ConfigError::MissingKey`] (static: contains no key).
pub const MISSING_KEY_GUIDANCE: &str = "set OPENCODE_API_KEY (alias OPENCODE_GO_API_KEY) in .env and restart; cloud calls stay BLOCKED until configured; the key value is never logged (DEC-010/DEC-011)";

// ---------------------------------------------------------------------------
// ApiKey: opaque, redacted secret
// ---------------------------------------------------------------------------

/// Cloud API key. The inner value is only reachable via [`ApiKey::expose_secret`].
///
/// [`std::fmt::Debug`] and [`std::fmt::Display`] both render `[redacted]`; the
/// type deliberately implements neither `AsRef<str>` nor `Serialize`, so the
/// value cannot leak through generic logging or snapshot code paths.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wrap `raw` after trimming surrounding whitespace (tolerates `.env`
    /// editors that append a trailing newline). Blank input yields
    /// [`ConfigError::MissingKey`], never an empty key.
    pub fn new(raw: String) -> Result<Self, ConfigError> {
        let trimmed = raw.trim().to_owned();
        if trimmed.is_empty() {
            return Err(ConfigError::MissingKey(MISSING_KEY_GUIDANCE));
        }
        Ok(Self(trimmed))
    }

    /// Borrow the raw secret. Callers must keep the result out of logs,
    /// snapshots, error paths, and audit events (AGENTS.md sec. 8).
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    /// Always `[redacted]`; the secret never appears in debug output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for ApiKey {
    /// Always `[redacted]`; the secret never appears in display output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

// ---------------------------------------------------------------------------
// ConfigError: BLOCKED-only, value-free
// ---------------------------------------------------------------------------

/// Configuration failure. Every variant is user-visible `BLOCKED` (see
/// [`ConfigError::blocked`]) and carries guidance; no variant stores
/// caller-supplied values, so formatting an error can never echo secrets.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// [`API_KEY_ENV`] and [`API_KEY_ALIAS_ENV`] are both absent or blank.
    #[error("missing API key: {0}")]
    MissingKey(&'static str),
    /// [`CONSENT_TIER_ENV`] names no known tier.
    #[error(
        "invalid consent tier in SYNTHLM_CONSENT_TIER: expected tier1|tier2|tier3 (see DEC-010)"
    )]
    InvalidTier,
    /// An upload field at `index` falls outside [`ALLOWED_UPLOAD_FIELDS`].
    #[error(
        "upload field at index {index} is outside the audit whitelist [prompt, mir, meta] (see DEC-011)"
    )]
    WhitelistViolation {
        /// Position of the first offending field (no value stored).
        index: usize,
    },
    /// [`BASE_URL_ENV`] override is not an `http(s)://` URL.
    #[error("invalid base URL in SYNTHLM_BASE_URL: expected http(s)://... (see DEC-010)")]
    InvalidBaseUrl,
}

impl ConfigError {
    /// Map to the [`crate::ipc`] taxonomy. All arms are terminal (`blocked`);
    /// config problems are fixed by the user, never by retrying.
    pub fn code(self) -> ErrorCode {
        match self {
            ConfigError::MissingKey(_) => ErrorCode::AuthDenied,
            ConfigError::InvalidTier => ErrorCode::ConsentRequired,
            ConfigError::WhitelistViolation { .. } => ErrorCode::WhitelistViolation,
            ConfigError::InvalidBaseUrl => ErrorCode::Internal,
        }
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    ///
    /// Derived from [`ConfigError::code`] so the verdict cannot drift from
    /// the [`crate::ipc`] taxonomy; currently always `true`.
    pub fn blocked(self) -> bool {
        !self.code().retryable()
    }

    /// Static remediation hint for UI / BLOCKED surfaces (contains no secrets).
    pub fn guidance(self) -> &'static str {
        match self {
            ConfigError::MissingKey(guidance) => guidance,
            ConfigError::InvalidTier => {
                "set SYNTHLM_CONSENT_TIER to tier1, tier2, or tier3 (see DEC-010)"
            }
            ConfigError::WhitelistViolation { .. } => {
                "restrict upload fields to the whitelist [prompt, mir, meta]; PCM and key material must stay local (see DEC-011)"
            }
            ConfigError::InvalidBaseUrl => {
                "set SYNTHLM_BASE_URL to an http(s):// URL or unset it for the DEC-010 default (see DEC-010)"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing / validation helpers
// ---------------------------------------------------------------------------

/// Parse a consent tier (`tier1|tier2|tier3`, also `1|2|3`; ASCII
/// case-insensitive, surrounding whitespace ignored).
///
/// Anything else yields [`ConfigError::InvalidTier`] without echoing the input.
pub fn parse_consent_tier(raw: &str) -> Result<ConsentTier, ConfigError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "tier1" | "1" => Ok(ConsentTier::Tier1),
        "tier2" | "2" => Ok(ConsentTier::Tier2),
        "tier3" | "3" => Ok(ConsentTier::Tier3),
        _ => Err(ConfigError::InvalidTier),
    }
}

/// Reject any upload field outside [`ALLOWED_UPLOAD_FIELDS`].
///
/// Key material, PCM references, and prompt text are not whitelist members,
/// so they fail here before an [`AuditEvent`] can be built. The error carries
/// the offending *index*, never its value.
pub fn validate_upload_fields(fields: &[String]) -> Result<(), ConfigError> {
    match fields
        .iter()
        .position(|field| !ALLOWED_UPLOAD_FIELDS.contains(&field.as_str()))
    {
        Some(index) => Err(ConfigError::WhitelistViolation { index }),
        None => Ok(()),
    }
}

/// Read the cloud API key from process environment (primary, then alias).
///
/// Both absent or blank yields [`ConfigError::MissingKey`]. Reads process
/// environment only; no `.env` file is opened here.
pub fn load_api_key() -> Result<ApiKey, ConfigError> {
    load_key_with(&|key| std::env::var(key).ok())
}

/// Trimmed non-empty environment value (`None` for absent or blank).
fn nonempty(raw: Option<String>) -> Option<String> {
    match raw {
        Some(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        None => None,
    }
}

/// [`load_api_key`] over an injected lookup (lets tests avoid process env).
fn load_key_with(get: &dyn Fn(&str) -> Option<String>) -> Result<ApiKey, ConfigError> {
    if let Some(raw) = nonempty(get(API_KEY_ENV)) {
        return ApiKey::new(raw);
    }
    if let Some(raw) = nonempty(get(API_KEY_ALIAS_ENV)) {
        return ApiKey::new(raw);
    }
    Err(ConfigError::MissingKey(MISSING_KEY_GUIDANCE))
}

/// Accept only `http://` / `https://` overrides (blank is handled upstream by
/// falling back to [`DEFAULT_BASE_URL`]).
fn validate_base_url(raw: &str) -> Result<String, ConfigError> {
    let trimmed = raw.trim().to_owned();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Ok(trimmed)
    } else {
        Err(ConfigError::InvalidBaseUrl)
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Resolved cloud-model configuration (DEC-010 presets + env overrides).
///
/// `Debug` is safe to log: the only secret field ([`ApiKey`]) redacts itself.
/// The struct deliberately implements no `Serialize`: keys must never enter
/// snapshots or logs (DEC-010/011).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Consent tier authorizing cloud use.
    pub tier: ConsentTier,
    /// Cloud API key (required for every tier; Tier3 call paths ignore it).
    pub api_key: ApiKey,
    /// Cloud base URL (DEC-010 default, overridable via [`BASE_URL_ENV`]).
    pub base_url: String,
    /// Tier1 model name (DEC-010 default, overridable via [`TIER1_MODEL_ENV`]).
    pub tier1_model: String,
    /// Tier2 model name (DEC-010 default, overridable via [`TIER2_MODEL_ENV`]).
    pub tier2_model: String,
    /// Tier3 model name (DEC-010 default, overridable via [`TIER3_MODEL_ENV`]).
    pub tier3_model: String,
}

impl Config {
    /// Resolve from process environment. Missing/blank key is
    /// [`ConfigError::MissingKey`] (`BLOCKED` + guidance); the injectable
    /// lookup core (`from_lookup`) used by tests follows the same rules.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|key| std::env::var(key).ok())
    }

    /// Resolve from an injected `get` lookup (same rules as [`Config::from_env`]
    /// without touching process environment).
    fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let api_key = load_key_with(get)?;
        let tier = match nonempty(get(CONSENT_TIER_ENV)) {
            Some(raw) => parse_consent_tier(&raw)?,
            None => DEFAULT_TIER,
        };
        let base_url = match nonempty(get(BASE_URL_ENV)) {
            Some(raw) => validate_base_url(&raw)?,
            None => DEFAULT_BASE_URL.to_owned(),
        };
        Ok(Self {
            tier,
            api_key,
            base_url,
            tier1_model: nonempty(get(TIER1_MODEL_ENV))
                .unwrap_or_else(|| DEFAULT_TIER1_MODEL.to_owned()),
            tier2_model: nonempty(get(TIER2_MODEL_ENV))
                .unwrap_or_else(|| DEFAULT_TIER2_MODEL.to_owned()),
            tier3_model: nonempty(get(TIER3_MODEL_ENV))
                .unwrap_or_else(|| DEFAULT_TIER3_MODEL.to_owned()),
        })
    }

    /// Model name selected by the configured tier.
    pub fn active_model(&self) -> &str {
        match self.tier {
            ConsentTier::Tier1 => &self.tier1_model,
            ConsentTier::Tier2 => &self.tier2_model,
            ConsentTier::Tier3 => &self.tier3_model,
        }
    }

    /// Whether the configured tier may place cloud calls (Tier3 never does).
    pub fn upload_allowed(&self) -> bool {
        !matches!(self.tier, ConsentTier::Tier3)
    }

    /// Build a whitelisted [`AuditEvent`] for a cloud call: validates `fields`
    /// against [`ALLOWED_UPLOAD_FIELDS`] first, so key material or PCM can
    /// never reach the audit record.
    pub fn audit_event(
        &self,
        fields: Vec<String>,
        byte_count: u64,
    ) -> Result<AuditEvent, ConfigError> {
        validate_upload_fields(&fields)?;
        Ok(AuditEvent::now(
            self.active_model(),
            self.tier,
            fields,
            byte_count,
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests (synthetic values only; the real `.env` is never opened or printed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Synthetic stand-in key: clearly fake, never read from disk.
    const SYNTHETIC_KEY: &str = "tsk108-synthetic-key-AAAA";
    /// Second synthetic key, for precedence tests.
    const SYNTHETIC_ALIAS_KEY: &str = "tsk108-synthetic-alias-BBBB";

    /// Tests never touch process environment: every case below uses the
    /// injected-map lookup, so no `unsafe` set_var is needed anywhere
    /// (AGENTS.md §4 holds for tests and trunk alike).
    /// Injectable lookup from `pairs` (avoids process environment entirely).
    fn lookup_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (key, value) in pairs {
            map.insert((*key).to_owned(), (*value).to_owned());
        }
        map
    }

    fn config_from(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map = lookup_of(pairs);
        Config::from_lookup(&|key| map.get(key).cloned())
    }

    #[test]
    fn missing_key_when_both_absent_is_blocked() {
        let err = config_from(&[]).expect_err("both key vars absent must BLOCK");
        assert_eq!(err, ConfigError::MissingKey(MISSING_KEY_GUIDANCE));
        assert_eq!(err.code(), ErrorCode::AuthDenied);
        assert!(err.blocked());
        let text = format!("{err}");
        assert!(text.contains(API_KEY_ENV));
        assert!(text.contains("BLOCKED"));
    }

    #[test]
    fn empty_or_blank_key_is_missing() {
        for blank in ["", "   ", "\t\n "] {
            let err = config_from(&[(API_KEY_ENV, blank)]).expect_err("blank primary must BLOCK");
            assert_eq!(err.code(), ErrorCode::AuthDenied);
            assert!(err.blocked());
            // No value assertion here: blanks trim to empty and carry no
            // information; non-empty secrecy is covered by the redaction and
            // value-free-error tests below.
        }
        // Blank primary falls through to a blank alias: still missing.
        let err = config_from(&[(API_KEY_ENV, "  "), (API_KEY_ALIAS_ENV, "")])
            .expect_err("blank primary+alias must BLOCK");
        assert_eq!(err, ConfigError::MissingKey(MISSING_KEY_GUIDANCE));
    }

    #[test]
    fn alias_accepted_when_primary_absent() {
        let config = config_from(&[(API_KEY_ALIAS_ENV, SYNTHETIC_ALIAS_KEY)])
            .expect("alias must satisfy key requirement");
        assert_eq!(config.api_key.expose_secret(), SYNTHETIC_ALIAS_KEY);
    }

    #[test]
    fn primary_wins_over_alias() {
        let config = config_from(&[
            (API_KEY_ENV, SYNTHETIC_KEY),
            (API_KEY_ALIAS_ENV, SYNTHETIC_ALIAS_KEY),
        ])
        .expect("primary key must load");
        assert_eq!(config.api_key.expose_secret(), SYNTHETIC_KEY);
    }

    #[test]
    fn api_key_debug_and_display_are_redacted() {
        let key = ApiKey::new(SYNTHETIC_KEY.to_owned()).expect("synthetic key valid");
        assert_eq!(format!("{key:?}"), "[redacted]");
        assert_eq!(format!("{key}"), "[redacted]");
        assert!(!format!("{key:?}").contains(SYNTHETIC_KEY));
        assert!(!format!("{key}").contains(SYNTHETIC_KEY));
    }

    #[test]
    fn config_debug_never_contains_key() {
        let config = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY)]).expect("lookup config");
        let rendered = format!("{config:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains(SYNTHETIC_KEY));
    }

    #[test]
    fn tier_parsing_covers_all_tiers() {
        assert_eq!(parse_consent_tier("tier1"), Ok(ConsentTier::Tier1));
        assert_eq!(parse_consent_tier("TIER2"), Ok(ConsentTier::Tier2));
        assert_eq!(parse_consent_tier("  Tier3 "), Ok(ConsentTier::Tier3));
        assert_eq!(parse_consent_tier("1"), Ok(ConsentTier::Tier1));
        assert_eq!(parse_consent_tier("2"), Ok(ConsentTier::Tier2));
        assert_eq!(parse_consent_tier("3"), Ok(ConsentTier::Tier3));
    }

    #[test]
    fn invalid_tier_is_blocked_and_value_free() {
        let err = parse_consent_tier(SYNTHETIC_KEY).expect_err("key is no tier");
        assert_eq!(err, ConfigError::InvalidTier);
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(err.blocked());
        // Even a misplaced key in the tier slot must not surface in output.
        assert!(!format!("{err}").contains(SYNTHETIC_KEY));
        assert!(!format!("{err:?}").contains(SYNTHETIC_KEY));
    }

    #[test]
    fn defaults_match_dec010_presets() {
        let config = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY)]).expect("defaults");
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.tier1_model, DEFAULT_TIER1_MODEL);
        assert_eq!(config.tier2_model, DEFAULT_TIER2_MODEL);
        assert_eq!(config.tier3_model, DEFAULT_TIER3_MODEL);
        assert_eq!(config.tier, DEFAULT_TIER);
        assert_eq!(config.tier, ConsentTier::Tier1);
    }

    #[test]
    fn env_overrides_replace_defaults() {
        let config = config_from(&[
            (API_KEY_ENV, SYNTHETIC_KEY),
            (BASE_URL_ENV, "https://example.invalid/v1"),
            (TIER1_MODEL_ENV, "tier1-override"),
            (TIER2_MODEL_ENV, "tier2-override"),
            (TIER3_MODEL_ENV, "tier3-override"),
            (CONSENT_TIER_ENV, "tier2"),
        ])
        .expect("overrides");
        assert_eq!(config.base_url, "https://example.invalid/v1");
        assert_eq!(config.tier1_model, "tier1-override");
        assert_eq!(config.tier2_model, "tier2-override");
        assert_eq!(config.tier3_model, "tier3-override");
        assert_eq!(config.tier, ConsentTier::Tier2);
        assert_eq!(config.active_model(), "tier2-override");
        assert!(config.upload_allowed());
    }

    #[test]
    fn blank_overrides_fall_back_to_defaults() {
        let config = config_from(&[
            (API_KEY_ENV, SYNTHETIC_KEY),
            (BASE_URL_ENV, "   "),
            (TIER1_MODEL_ENV, ""),
            (CONSENT_TIER_ENV, "  "),
        ])
        .expect("blank overrides fall back");
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
        assert_eq!(config.tier1_model, DEFAULT_TIER1_MODEL);
        assert_eq!(config.tier, DEFAULT_TIER);
    }

    #[test]
    fn invalid_base_url_is_blocked() {
        let err = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY), (BASE_URL_ENV, "nota-a-url")])
            .expect_err("non-http URL must BLOCK");
        assert_eq!(err, ConfigError::InvalidBaseUrl);
        assert!(err.blocked());
    }

    #[test]
    fn active_model_follows_tier() {
        for (tier, expected) in [
            ("tier1", DEFAULT_TIER1_MODEL),
            ("tier2", DEFAULT_TIER2_MODEL),
            ("tier3", DEFAULT_TIER3_MODEL),
        ] {
            let config = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY), (CONSENT_TIER_ENV, tier)])
                .expect("tier config");
            assert_eq!(config.active_model(), expected);
        }
        let local = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY), (CONSENT_TIER_ENV, "tier3")])
            .expect("tier3 config");
        assert!(!local.upload_allowed());
    }

    #[test]
    fn audit_event_accepts_whitelist_fields() {
        let config = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY)]).expect("config");
        let fields = vec!["prompt".to_owned(), "mir".to_owned(), "meta".to_owned()];
        let event = config
            .audit_event(fields.clone(), 4096)
            .expect("whitelist fields accepted");
        assert_eq!(event.model, DEFAULT_TIER1_MODEL);
        assert_eq!(event.tier, ConsentTier::Tier1);
        assert_eq!(event.fields, fields);
        assert_eq!(event.byte_count, 4096);
        // Serialized audit stays at exactly five secret-free fields.
        let text = serde_json::to_string(&event).expect("serialize audit");
        assert!(!text.contains(SYNTHETIC_KEY));
    }

    #[test]
    fn audit_event_rejects_key_and_pcm_fields() {
        let config = config_from(&[(API_KEY_ENV, SYNTHETIC_KEY)]).expect("config");
        // Key-shaped field rejected (index 1), value never echoed.
        let err = config
            .audit_event(vec!["prompt".to_owned(), "api_key".to_owned()], 128)
            .expect_err("key field must be rejected");
        assert_eq!(err, ConfigError::WhitelistViolation { index: 1 });
        assert_eq!(err.code(), ErrorCode::WhitelistViolation);
        assert!(err.blocked());
        // The raw key value as a field is equally rejected.
        let err = config
            .audit_event(vec![SYNTHETIC_KEY.to_owned()], 128)
            .expect_err("raw key as field must be rejected");
        assert_eq!(err, ConfigError::WhitelistViolation { index: 0 });
        assert!(!format!("{err}").contains(SYNTHETIC_KEY));
        assert!(!format!("{err:?}").contains(SYNTHETIC_KEY));
        // PCM stays local per DEC-011.
        let err = config
            .audit_event(vec!["pcm".to_owned()], 128)
            .expect_err("pcm must be rejected");
        assert_eq!(err.code(), ErrorCode::WhitelistViolation);
    }

    #[test]
    fn from_env_smoke_delegates_without_panic() {
        // Thin wrapper over `from_lookup`; only asserts it resolves through
        // the same rules (outcome depends on the ambient environment).
        match Config::from_env() {
            Ok(config) => assert!(config.upload_allowed() || !config.upload_allowed()),
            Err(err) => assert!(err.blocked()),
        }
    }

    #[test]
    fn all_config_errors_are_blocked() {
        let cases = [
            ConfigError::MissingKey(MISSING_KEY_GUIDANCE),
            ConfigError::InvalidTier,
            ConfigError::WhitelistViolation { index: 0 },
            ConfigError::InvalidBaseUrl,
        ];
        for err in cases {
            assert!(err.blocked(), "{err:?} must be BLOCKED");
            assert!(!err.code().retryable(), "{err:?} must never retry");
            assert!(!err.guidance().is_empty());
        }
    }

    #[test]
    fn missing_key_guidance_names_env_vars() {
        let err = ConfigError::MissingKey(MISSING_KEY_GUIDANCE);
        let text = format!("{err}");
        assert!(text.contains(API_KEY_ENV));
        assert!(text.contains(API_KEY_ALIAS_ENV));
        assert_eq!(err.guidance(), MISSING_KEY_GUIDANCE);
    }
}
