//! Model gateway: three-tier routing with failover, retry, and audit.
//!
//! Implements the TSK-301 slice of DEC-010/DEC-011 and ARCHITECTURE §8:
//!
//! - Tier value resolution ([`crate::model_gw::resolve_tier`]): stored [`synthlm_common::consent::ConsentStore`] tier >
//!   environment override > [`synthlm_common::config::DEFAULT_TIER`]. In the gateway path stored
//!   consent always exists (otherwise [`synthlm_common::consent::require_consent`] BLOCKED first), so
//!   the stored tier wins there; the environment/default legs serve
//!   diagnostics and settings-preview callers.
//! - Fail-safe gate: [`crate::model_gw::Gateway::route`] calls
//!   [`synthlm_common::consent::require_consent`](synthlm_common::consent::require_consent) before any
//!   transport use. [`synthlm_common::consent::ConsentState::Undecided`] yields `consent_required`
//!   BLOCKED with zero transport calls and zero audit events (AGENTS.md §8
//!   默认不出网).
//! - Consent ceiling: the stored tier is the highest privilege attempted.
//!   Stored Tier2 never escalates to Tier1 (that would send
//!   training-retention-accepted traffic the user declined); stored Tier3
//!   only touches the local endpoint. The chain is Tier1→Tier2→Tier3,
//!   Tier2→Tier3, or Tier3 alone.
//! - Model resolution ([`crate::model_gw::resolve_model`]): tier → DEC-010 compile-time preset
//!   model name + endpoint. Cloud tiers reference [`synthlm_common::config::DEFAULT_BASE_URL`];
//!   Tier3 resolves to [`crate::model_gw::Endpoint::Local`] with no URL (the concrete local
//!   endpoint is probed by TSK-305; no placeholder port is invented here).
//!   Resolution needs no API key, so Tier3 works keyless.
//! - Whitelist gate: every [`crate::model_gw::ModelRequest::new`] validates `fields` with
//!   [`validate_upload_fields`](synthlm_common::config::validate_upload_fields)
//!   before any transport use; a violation is `whitelist_violation` BLOCKED
//!   and nothing is sent.
//! - Key gate: any chain containing a cloud tier requires
//!   `cloud_key_present`; otherwise `auth_denied` BLOCKED before any attempt.
//!   The gateway only ever sees key *presence* (a `bool`): key material never
//!   enters requests, errors, or audit events.
//! - Failover ([`crate::model_gw::Gateway::route`]): per-tier attempts follow
//!   [`synthlm_common::ipc::RetryPolicy`] via [`should_retry`](synthlm_common::ipc::should_retry)
//!   (computed backoff delays are reported, never slept: the transport is
//!   synchronous and tests must stay instant); each tier owns a
//!   [`synthlm_common::ipc::CircuitBreaker`] (open tiers are skipped, half-open probes pass
//!   through). `Unauthorized` (HTTP 401) skips the remaining cloud tiers and
//!   jumps to local Tier3, since retrying the same key cloud-side cannot
//!   succeed. Exhaustion of the chain is BLOCKED carrying the last
//!   [`synthlm_common::ipc::ErrorCode`].
//! - Audit: one [`synthlm_common::ipc::AuditEvent`] per transport attempt (retries included),
//!   built after the whitelist gate, so every mock or future real call is
//!   recorded with exactly time / model / tier / field list / byte count and
//!   no key/PCM material.
//!
//! ## Transports (TSK-301 mock + TSK-116 real client)
//!
//! Two [`crate::model_gw::Transport`] implementations ship here:
//!
//! - [`crate::model_gw::MockTransport`]: programmable fault-injection double.
//!   Performs no network I/O; used by the gateway failover tests and any
//!   caller that must stay offline.
//! - [`crate::model_gw::HttpsTransport`]: real blocking HTTPS client (TSK-116,
//!   DEC-010/011) posting the responses-format body to
//!   `{base_url}/responses`. Only this transport touches the network, and
//!   only for cloud tiers ([`crate::model_gw::Endpoint::Cloud`]); a
//!   [`crate::model_gw::Endpoint::Local`] request is refused with
//!   [`crate::model_gw::TransportKind::LocalDown`] before any I/O.
//!
//! Blocking contract: [`crate::model_gw::Gateway::route`] drives a synchronous transport and
//! may block the calling thread on real transports. Never call it
//! from an audio thread (AGENTS.md red line 2); it is a control-plane helper.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use thiserror::Error;

use synthlm_common::config::{
    self, DEFAULT_BASE_URL, DEFAULT_TIER, DEFAULT_TIER1_MODEL, DEFAULT_TIER2_MODEL,
    DEFAULT_TIER3_MODEL,
};
use synthlm_common::consent::{ConsentState, require_consent};
use synthlm_common::ipc::{
    AuditEvent, CircuitBreaker, CircuitState, ConsentTier, ErrorCode, RetryPolicy, TimeoutConfig,
    should_retry,
};

/// Re-exported tier alias: the gateway speaks [`ConsentTier`] everywhere.
///
/// The task statement names this `Tier`; the canonical type lives in
/// [`synthlm_common::ipc`] so consent, config, and audit stay on one
/// taxonomy.
pub type Tier = ConsentTier;

// ---------------------------------------------------------------------------
// Tier resolution
// ---------------------------------------------------------------------------

/// Resolve the effective tier: stored consent > environment override >
/// [`synthlm_common::config::DEFAULT_TIER`] (Tier1, DEC-010 cloud-first preset).
///
/// Pure function: no environment or filesystem reads. `stored` is the tier
/// from the authoritative [`synthlm_common::consent::ConsentStore`]; `env_override` is an already
/// parsed `SYNTHLM_CONSENT_TIER` value (see [`parse_env_override`]).
/// `None` on both legs yields [`synthlm_common::config::DEFAULT_TIER`].
///
/// Note: the gateway path ([`crate::model_gw::Gateway::route`]) gates on [`synthlm_common::consent::require_consent`]
/// first, so an undecided state BLOCKEDs before this function matters there;
/// the environment/default legs serve diagnostics and settings-preview
/// callers (open question: whether an env override may ever narrow a stored
/// tier — currently it may not; stored always wins once decided).
pub fn resolve_tier(stored: Option<ConsentTier>, env_override: Option<ConsentTier>) -> ConsentTier {
    stored.or(env_override).unwrap_or(DEFAULT_TIER)
}

/// Parse an injected `SYNTHLM_CONSENT_TIER` value (`None` = unset).
///
/// Blank input behaves as unset (mirrors [`config`] blank-fallback rules);
/// an unknown name is [`GatewayError::InvalidTier`] (BLOCKED, value-free).
/// Takes the raw string instead of reading process environment so tests never
/// touch ambient state.
pub fn parse_env_override(raw: Option<&str>) -> Result<Option<ConsentTier>, GatewayError> {
    match raw {
        None => Ok(None),
        Some(text) if text.trim().is_empty() => Ok(None),
        Some(text) => config::parse_consent_tier(text)
            .map(Some)
            .map_err(|_| GatewayError::InvalidTier),
    }
}

// ---------------------------------------------------------------------------
// Model resolution
// ---------------------------------------------------------------------------

/// Where a resolved model lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Cloud `responses` endpoint at the DEC-010 base URL.
    Cloud {
        /// Base URL (DEC-010 preset, e.g. `https://opencode.ai/zen/go/v1`).
        base_url: &'static str,
    },
    /// Local OpenAI-compatible endpoint (address probed by TSK-305; no URL
    /// is invented here, so Tier3 resolution stays keyless and network-free).
    Local,
}

/// Tier → model name + endpoint (DEC-010 presets, no key required).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedModel {
    /// Tier this resolution is for.
    pub tier: ConsentTier,
    /// Preset model name (e.g. `"muse-spark-1.3-contributor"`).
    pub model: &'static str,
    /// Where the model lives.
    pub endpoint: Endpoint,
}

/// Resolve `tier` to its DEC-010 preset model and endpoint.
///
/// Tier1 → [`DEFAULT_TIER1_MODEL`] on [`Endpoint::Cloud`], Tier2 →
/// [`DEFAULT_TIER2_MODEL`] on [`Endpoint::Cloud`], Tier3 →
/// [`DEFAULT_TIER3_MODEL`] on [`crate::model_gw::Endpoint::Local`]. Deliberately independent
/// of [`config::Config`] (which requires an API key): Tier3 local inference
/// must resolve without any key present.
pub fn resolve_model(tier: ConsentTier) -> ResolvedModel {
    match tier {
        ConsentTier::Tier1 => ResolvedModel {
            tier,
            model: DEFAULT_TIER1_MODEL,
            endpoint: Endpoint::Cloud {
                base_url: DEFAULT_BASE_URL,
            },
        },
        ConsentTier::Tier2 => ResolvedModel {
            tier,
            model: DEFAULT_TIER2_MODEL,
            endpoint: Endpoint::Cloud {
                base_url: DEFAULT_BASE_URL,
            },
        },
        ConsentTier::Tier3 => ResolvedModel {
            tier,
            model: DEFAULT_TIER3_MODEL,
            endpoint: Endpoint::Local,
        },
    }
}

/// Whether `tier` may place cloud calls (Tier3 never does).
pub fn upload_allowed(tier: ConsentTier) -> bool {
    !matches!(tier, ConsentTier::Tier3)
}

// ---------------------------------------------------------------------------
// Gateway errors (BLOCKED taxonomy, value-free)
// ---------------------------------------------------------------------------

/// Model-gateway failure. Every variant is user-visible `BLOCKED` semantics
/// (see [`GatewayError::code`]); no variant stores caller-supplied values —
/// not field names, not prompt text, not key material — so formatting an
/// error can never echo secrets (AGENTS.md §8).
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum GatewayError {
    /// No stored consent: complete the first-run choice first.
    #[error(
        "no upload consent stored: complete the first-run choice (1/2/3) or change it in settings; cloud calls stay BLOCKED (see DEC-010)"
    )]
    ConsentRequired,
    /// `SYNTHLM_CONSENT_TIER` names no known tier.
    #[error(
        "invalid consent tier in SYNTHLM_CONSENT_TIER: expected tier1|tier2|tier3 (see DEC-010)"
    )]
    InvalidTier,
    /// An upload field at `index` falls outside the DEC-011 whitelist.
    #[error(
        "upload field at index {index} is outside the audit whitelist [prompt, mir, meta, audio_ref]; nothing was sent (see DEC-011)"
    )]
    WhitelistViolation {
        /// Position of the first offending field (no value stored).
        index: usize,
    },
    /// An audio-bearing request reached Tier3, which is text-only (DEC-010).
    #[error("tier3 is text-only: audio understanding routes to Tier1/Tier2 (see DEC-010)")]
    TierAudioUnsupported,
    /// A cloud tier is in the chain but no API key is configured.
    #[error(
        "missing API key: set OPENCODE_API_KEY (alias OPENCODE_GO_API_KEY) in .env and restart; cloud calls stay BLOCKED until configured (see DEC-010)"
    )]
    MissingKey,
    /// Every tier in the consent-ceiling chain failed (or every breaker is
    /// open). Carries the last transport [`synthlm_common::ipc::ErrorCode`]; when no attempt ran
    /// (all breakers open) this is [`ErrorCode::CloudUnavailable`].
    #[error(
        "all model tiers exhausted: last failure was {code:?}; check connectivity or switch to Tier3 local (see DEC-011)"
    )]
    AllTiersExhausted {
        /// Last transport failure class (drives retry verdicts downstream).
        code: ErrorCode,
    },
}

impl GatewayError {
    /// Map to the [`synthlm_common::ipc`] taxonomy. Consent, tier, whitelist,
    /// and key failures are terminal; exhaustion reuses the last transport
    /// code so downstream retry verdicts cannot drift from the taxonomy.
    pub fn code(self) -> ErrorCode {
        match self {
            GatewayError::ConsentRequired | GatewayError::InvalidTier => ErrorCode::ConsentRequired,
            GatewayError::WhitelistViolation { .. } => ErrorCode::WhitelistViolation,
            GatewayError::TierAudioUnsupported => ErrorCode::AudioCapabilityMissing,
            GatewayError::MissingKey => ErrorCode::AuthDenied,
            GatewayError::AllTiersExhausted { code } => code,
        }
    }

    /// Whether this failure is user-visible `BLOCKED`.
    ///
    /// Derived from [`GatewayError::code`] so the verdict cannot drift from
    /// the [`synthlm_common::ipc`] taxonomy.
    pub fn blocked(self) -> bool {
        !self.code().retryable()
    }

    /// Static remediation hint for UI / BLOCKED surfaces (contains no secrets).
    pub fn guidance(self) -> &'static str {
        match self {
            GatewayError::ConsentRequired => {
                "choose a tier in the first-run prompt (1/2/3) or in settings; Tier3 keeps everything local (see DEC-010)"
            }
            GatewayError::InvalidTier => {
                "set SYNTHLM_CONSENT_TIER to tier1, tier2, or tier3 (see DEC-010)"
            }
            GatewayError::WhitelistViolation { .. } => {
                "restrict upload fields to the whitelist [prompt, mir, meta, audio_ref]; PCM and key material must stay local (see DEC-011)"
            }
            GatewayError::TierAudioUnsupported => {
                "route audio understanding to Tier1 or Tier2; Tier3 local is text-only (see DEC-010)"
            }
            GatewayError::MissingKey => {
                "set OPENCODE_API_KEY (alias OPENCODE_GO_API_KEY) in .env and restart; the key value is never logged (see DEC-010)"
            }
            GatewayError::AllTiersExhausted { .. } => {
                "check cloud connectivity and the local endpoint, then retry; repeated exhaustion trips per-tier breakers (see DEC-011)"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request / response
// ---------------------------------------------------------------------------

/// Outbound model request: tier + resolved model + whitelisted fields.
///
/// Holds field *names* and a body *byte count* only — never prompt text,
/// PCM, paths, or key material — so requests, errors, and audit events built
/// from it are secret-free by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRequest {
    /// Tier this request targets.
    pub tier: ConsentTier,
    /// Preset model name for [`ModelRequest::tier`].
    pub model: &'static str,
    /// Where the model lives.
    pub endpoint: Endpoint,
    /// Whitelisted upload field names (subset of `[prompt, mir, meta, audio_ref]`;
    /// `audio_ref` is Tier1/Tier2-only, refused at Tier3).
    pub fields: Vec<String>,
    /// Request body size in bytes (synthetic in tests; never PCM).
    pub byte_count: u64,
}

impl ModelRequest {
    /// Build a request for `tier`, validating `fields` against the DEC-011
    /// whitelist *before* any transport use.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayError::WhitelistViolation`] (BLOCKED, index-only)
    /// when any field falls outside `[prompt, mir, meta]`.
    pub fn new(
        tier: ConsentTier,
        fields: Vec<String>,
        byte_count: u64,
    ) -> Result<Self, GatewayError> {
        config::validate_upload_fields(&fields).map_err(|_| GatewayError::WhitelistViolation {
            // Re-derive the index without touching the offending value.
            index: fields
                .iter()
                .position(|field| !config::ALLOWED_UPLOAD_FIELDS.contains(&field.as_str()))
                .unwrap_or(0),
        })?;
        Ok(Self::for_tier(tier, fields, byte_count))
    }

    /// Build a request for `tier` without re-validating (the caller already
    /// passed [`crate::model_gw::ModelRequest::new`] or an equivalent whitelist gate).
    fn for_tier(tier: ConsentTier, fields: Vec<String>, byte_count: u64) -> Self {
        let resolved = resolve_model(tier);
        Self {
            tier,
            model: resolved.model,
            endpoint: resolved.endpoint,
            fields,
            byte_count,
        }
    }
}

/// Inbound model response (synthetic in tests; real decoding lands with the
/// HTTP client task).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelResponse {
    /// Tier that served the response.
    pub tier: ConsentTier,
    /// Model that served the response.
    pub model: String,
    /// Transport-observed latency in milliseconds.
    pub latency_ms: u64,
}

// ---------------------------------------------------------------------------
// Transport trait + fault-injection mock
// ---------------------------------------------------------------------------

/// Transport failure class. Maps to [`synthlm_common::ipc::ErrorCode`] via
/// [`TransportKind::code`]; only transport-level and server-side classes
/// retry — `Unauthorized` (HTTP 401) is terminal like the rest of the auth
/// taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    /// Attempt hit the [`TimeoutConfig`] budget (HTTP timeout).
    Timeout,
    /// Connection failed / reset (socket closed).
    ConnectionFailed,
    /// HTTP 401: key rejected. Terminal; also skips remaining cloud tiers.
    Unauthorized,
    /// HTTP 429: rate limited (retried with backoff).
    RateLimited,
    /// HTTP 5xx: server error (retried with backoff).
    ServerError,
    /// Tier3 local endpoint unavailable (retried with backoff, then BLOCKED).
    LocalDown,
}

impl TransportKind {
    /// Map to the [`synthlm_common::ipc`] taxonomy.
    pub fn code(self) -> ErrorCode {
        match self {
            TransportKind::Timeout => ErrorCode::Timeout,
            TransportKind::ConnectionFailed => ErrorCode::TransportClosed,
            TransportKind::Unauthorized => ErrorCode::AuthDenied,
            TransportKind::RateLimited | TransportKind::ServerError | TransportKind::LocalDown => {
                ErrorCode::CloudUnavailable
            }
        }
    }
}

/// Transport failure: kind only, no bodies, no headers, no key material.
///
/// `retry_after_ms` carries an observed `Retry-After` delay (HTTP 429 only);
/// every other failure leaves it `None`. The gateway reports
/// [`synthlm_common::ipc::RetryPolicy`] backoff delays and never sleeps, so this
/// is observation-only in this task.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("model transport failed: {kind:?} (code {code:?}; see DEC-011)")]
pub struct TransportError {
    /// Failure class.
    pub kind: TransportKind,
    /// Taxonomy code derived from [`TransportKind::code`].
    pub code: ErrorCode,
    /// Observed `Retry-After` in milliseconds (HTTP 429 only, `None` when the
    /// header is absent or unparsable).
    pub retry_after_ms: Option<u64>,
}

impl TransportError {
    /// Build from a [`TransportKind`], deriving the [`synthlm_common::ipc::ErrorCode`].
    pub fn new(kind: TransportKind) -> Self {
        Self {
            kind,
            code: kind.code(),
            retry_after_ms: None,
        }
    }

    /// Build from a [`TransportKind`] with an observed `Retry-After` delay.
    pub fn with_retry_after(kind: TransportKind, retry_after_ms: Option<u64>) -> Self {
        Self {
            kind,
            code: kind.code(),
            retry_after_ms,
        }
    }

    /// Whether the gateway may retry this failure within the tier.
    pub fn retryable(self) -> bool {
        self.code.retryable()
    }
}

/// Model transport: one blocking request/response exchange.
///
/// Implementations must be synchronous and secret-free (no key/PCM logging).
/// The real HTTPS client lands in a follow-up task; this task ships only
/// [`crate::model_gw::MockTransport`]. `timeout` carries the [`TimeoutConfig`] budgets (the
/// mock records them for assertion but never sleeps).
pub trait Transport {
    /// Send `request` under `timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] on timeout, connection failure, HTTP
    /// 401/429/5xx, or local-endpoint outage.
    fn send(
        &mut self,
        request: &ModelRequest,
        timeout: &TimeoutConfig,
    ) -> Result<ModelResponse, TransportError>;
}

/// Programmed mock outcome for one [`crate::model_gw::MockTransport`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MockOutcome {
    /// Succeed with an echo response (serving tier/model copied from the
    /// request) after `latency_ms`.
    Succeed {
        /// Reported latency in milliseconds.
        latency_ms: u64,
    },
    /// Fail with this [`TransportKind`].
    Fail(TransportKind),
}

/// Fault-injection [`crate::model_gw::Transport`] double: replays a programmed failure
/// sequence, then falls back to success (or a sticky failure).
///
/// Every call records the requested tier in [`MockTransport::calls`] and the
/// presented timeout in [`MockTransport::timeouts_seen_ms`], so tests can
/// assert the exact failover path and budget propagation without any network.
#[derive(Debug)]
pub struct MockTransport {
    /// Queued per-call outcomes (front = next call).
    script: VecDeque<MockOutcome>,
    /// Outcome once `script` is exhausted.
    fallback: MockOutcome,
    /// Requested tier per call, in order (includes retries).
    calls: Vec<ConsentTier>,
    /// `cloud_hard_ms` presented per call, in order.
    timeouts_seen_ms: Vec<u64>,
}

impl MockTransport {
    /// Replay `script`, then succeed.
    pub fn new(script: Vec<MockOutcome>) -> Self {
        Self {
            script: script.into(),
            fallback: MockOutcome::Succeed { latency_ms: 0 },
            calls: Vec::new(),
            timeouts_seen_ms: Vec::new(),
        }
    }

    /// Always succeed (empty script, success fallback).
    pub fn all_ok() -> Self {
        Self::new(Vec::new())
    }

    /// Fail every call: connection failures cloud-side, local-endpoint
    /// outage on Tier3 (the mock sees the requested tier, so the sticky
    /// fallback degrades to a tier-appropriate kind once `script` exhausts).
    pub fn all_down() -> Self {
        Self {
            script: vec![
                MockOutcome::Fail(TransportKind::ConnectionFailed),
                MockOutcome::Fail(TransportKind::ConnectionFailed),
                MockOutcome::Fail(TransportKind::ConnectionFailed),
                MockOutcome::Fail(TransportKind::ServerError),
                MockOutcome::Fail(TransportKind::ServerError),
                MockOutcome::Fail(TransportKind::ServerError),
                MockOutcome::Fail(TransportKind::LocalDown),
                MockOutcome::Fail(TransportKind::LocalDown),
                MockOutcome::Fail(TransportKind::LocalDown),
            ]
            .into(),
            fallback: MockOutcome::Fail(TransportKind::ConnectionFailed),
            calls: Vec::new(),
            timeouts_seen_ms: Vec::new(),
        }
    }

    /// Tier1 down → Tier2 down → Tier3 ok under the default [`synthlm_common::ipc::RetryPolicy`]
    /// (3 attempts per tier): six programmed failures, then success.
    pub fn tier1_down_tier2_down_tier3_ok() -> Self {
        Self::new(vec![
            MockOutcome::Fail(TransportKind::ConnectionFailed),
            MockOutcome::Fail(TransportKind::ConnectionFailed),
            MockOutcome::Fail(TransportKind::ConnectionFailed),
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
        ])
    }

    /// Tiers attempted so far, in call order (includes retries).
    pub fn calls(&self) -> &[ConsentTier] {
        &self.calls
    }

    /// `cloud_hard_ms` presented per call, in order.
    pub fn timeouts_seen_ms(&self) -> &[u64] {
        &self.timeouts_seen_ms
    }

    /// Next programmed outcome (or the fallback once exhausted).
    fn next_outcome(&mut self) -> MockOutcome {
        self.script.pop_front().unwrap_or(self.fallback)
    }
}

impl Transport for MockTransport {
    fn send(
        &mut self,
        request: &ModelRequest,
        timeout: &TimeoutConfig,
    ) -> Result<ModelResponse, TransportError> {
        self.calls.push(request.tier);
        self.timeouts_seen_ms.push(timeout.cloud_hard_ms);
        match self.next_outcome() {
            MockOutcome::Succeed { latency_ms } => Ok(ModelResponse {
                tier: request.tier,
                model: request.model.to_owned(),
                latency_ms,
            }),
            MockOutcome::Fail(kind) => Err(TransportError::new(kind)),
        }
    }
}

// ---------------------------------------------------------------------------
// Real HTTPS transport (TSK-116)
// ---------------------------------------------------------------------------

/// Path suffix appended to the DEC-010 base URL (Tier1, responses API).
///
/// Verified live 2026-10-06 only insofar as the Go docs table assigns it;
/// Tier1 call itself remains unprobed (training-retained tier, no call
/// placed without explicit human approval).
const RESPONSES_PATH_SUFFIX: &str = "/responses";

/// Path suffix for Tier2 (chat-completions API).
///
/// Verified live 2026-10-06: `POST {base}/chat/completions` with
/// `{model, messages, max_tokens}` returns 2xx for `mimo-v2.6-flash`
/// **iff** the [`OPENCODE_SESSION_HEADER`] header is present (absent →
/// systematic 400; see `docs/research/C-live-endpoint.md`).
const CHAT_COMPLETIONS_PATH_SUFFIX: &str = "/chat/completions";

/// Session header required by the Go surface (abuse monitoring + routing).
///
/// Docs (`https://opencode.ai/docs/go/`, 2026-10-06): send a stable session
/// ID per conversation. A custom `User-Agent` correlated with 401s in two
/// live probes, so this client keeps the default UA and sends only this
/// header (see `docs/research/C-live-endpoint.md`).
pub const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// Cloud path suffix for `tier` (Go docs per-model endpoint table,
/// 2026-10-06; Tier3 never reaches a cloud transport).
fn cloud_path_suffix(tier: ConsentTier) -> &'static str {
    match tier {
        ConsentTier::Tier1 => RESPONSES_PATH_SUFFIX,
        ConsentTier::Tier2 => CHAT_COMPLETIONS_PATH_SUFFIX,
        ConsentTier::Tier3 => RESPONSES_PATH_SUFFIX,
    }
}

/// Build the minimal request body for `request` (tier-aware wire shape).
///
/// Tier1 sends `model` plus a synthetic `input` string naming the whitelisted
/// fields and the body byte count. Tier2 sends the same synthetic content as
/// a single `messages[0]` user turn plus `max_tokens` (shape verified live
/// 2026-10-06). [`crate::model_gw::ModelRequest`] carries no prompt
/// text, PCM, paths, or key material, so the wire body is secret-free by
/// construction; real prompt/MIR payload wiring lands with the caller that
/// owns that content.
///
/// Tier1 responses extras verified live 2026-10-06 (human-approved call):
/// 2xx + `output[]` envelope with the marker text back (131 output tokens
/// incl. reasoning). Sampling knobs/`stream` remain at server defaults.
fn request_body(request: &ModelRequest) -> serde_json::Value {
    let synthetic = format!(
        "synthetic fields=[{}] bytes={}",
        request.fields.join(","),
        request.byte_count,
    );
    match request.tier {
        ConsentTier::Tier2 => serde_json::json!({
            "model": request.model,
            "messages": [{"role": "user", "content": synthetic}],
            "max_tokens": 256,
        }),
        _ => serde_json::json!({
            "model": request.model,
            "input": synthetic,
        }),
    }
}

/// Map an HTTP status to its [`crate::model_gw::TransportKind`].
///
/// 401 is terminal (shared-key poisoning is handled by
/// [`crate::model_gw::Gateway::route`]); 429 and any other non-2xx are
/// retryable (TODO(TSK-116): 需对真端点验证 — 4xx beyond 401/429, e.g.
/// 400/404, may deserve a terminal class).
fn status_kind(status: reqwest::StatusCode) -> TransportKind {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        TransportKind::Unauthorized
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        TransportKind::RateLimited
    } else {
        TransportKind::ServerError
    }
}

/// Parse a `Retry-After` header value into milliseconds.
///
/// Accepts the delta-seconds form; the HTTP-date form is unhandled and yields
/// `None` (TODO(TSK-116): 需对真端点验证 — which form the live endpoint
/// emits).
fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?;
    let text = raw.to_str().ok()?;
    let secs: u64 = text.trim().parse().ok()?;
    secs.checked_mul(1000)
}

/// Whether `base_url` targets loopback (stubs, hermetic probes).
///
/// Proxy policy (DEC-011 / ARCH §5): cloud hosts honor the system proxy
/// (`http_proxy`/`https_proxy`); loopback never does. Without this split a
/// local proxy answers TCP-refused targets with 502 and the client would
/// mis-file `ConnectionFailed` as [`TransportKind::ServerError`].
fn is_loopback_base(base_url: &str) -> bool {
    let Some(rest) = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
}

/// Real blocking HTTPS transport for cloud tiers (TSK-116, DEC-010/011).
///
/// Client choice: `reqwest::blocking` matches the synchronous
/// [`crate::model_gw::Transport`] contract directly, so no async runtime is
/// introduced and [`crate::model_gw::Gateway::route`] keeps its call-thread-blocking
/// semantics. TLS comes from rustls (static, no system OpenSSL dependency).
///
/// Key custody: the API key arrives as an already-loaded caller value in
/// [`crate::model_gw::HttpsTransport::new`]. This module never reads process
/// environment, `.env` files, or any other key source; an empty key is
/// refused with [`crate::model_gw::TransportKind::Unauthorized`] before any I/O. The key is
/// redacted from [`std::fmt::Debug`], errors, and audit events.
///
/// Timeouts: each [`crate::model_gw::Transport::send`] applies
/// `timeout.cloud_hard_ms` (default 30 s per DEC-011) as the total request
/// budget; `timeout.cloud_p95_ms` (default 10 s) stays the observable foil
/// target surfaced via [`crate::model_gw::ModelResponse::latency_ms`]. Both knobs
/// remain configurable through [`synthlm_common::ipc::TimeoutConfig`].
#[derive(Clone)]
pub struct HttpsTransport {
    /// Bearer credential (caller-loaded; never logged).
    api_key: String,
    /// DEC-010 base URL (overridable for loopback stub tests).
    base_url: String,
    /// Stable conversation session ID for [`OPENCODE_SESSION_HEADER`];
    /// `None` sends no session header (loopback stubs, Tier3-adjacent paths).
    session_id: Option<String>,
    /// Blocking HTTP client (rustls TLS, no global timeout; per-request
    /// budgets come from [`synthlm_common::ipc::TimeoutConfig`]).
    client: reqwest::blocking::Client,
}

impl std::fmt::Debug for HttpsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsTransport")
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

impl HttpsTransport {
    /// Build a cloud transport: `api_key` is the caller-loaded credential
    /// (never read from environment or files here), `base_url` is the
    /// DEC-010 base URL (a loopback URL keeps tests hermetic).
    ///
    /// Proxy: non-loopback bases honor the system proxy so corporate egress
    /// keeps working; loopback bases always skip it (same host set as the
    /// private `is_loopback_base` helper).
    /// Tier3 local endpoints never construct this type.
    ///
    /// # Errors
    ///
    /// Returns [`TransportKind::ConnectionFailed`] when the HTTP client
    /// cannot be constructed.
    pub fn new(api_key: String, base_url: String) -> Result<Self, TransportError> {
        let use_system_proxy = !is_loopback_base(&base_url);
        Self::with_proxy_policy(api_key, base_url, use_system_proxy)
    }

    /// Hermetic transport that ignores `http_proxy`/`https_proxy`/`ALL_PROXY`.
    ///
    /// Use for fault-injection tests that must observe raw socket errors
    /// (connection refused, reset) rather than a local proxy's synthesized
    /// status codes.
    ///
    /// # Errors
    ///
    /// Returns [`TransportKind::ConnectionFailed`] when the HTTP client
    /// cannot be constructed.
    pub fn new_hermetic(api_key: String, base_url: String) -> Result<Self, TransportError> {
        Self::with_proxy_policy(api_key, base_url, false)
    }

    fn with_proxy_policy(
        api_key: String,
        base_url: String,
        use_system_proxy: bool,
    ) -> Result<Self, TransportError> {
        let builder = reqwest::blocking::Client::builder();
        let builder = if use_system_proxy {
            builder
        } else {
            builder.no_proxy()
        };
        let client = builder
            .build()
            .map_err(|_| TransportError::new(TransportKind::ConnectionFailed))?;
        Ok(Self {
            api_key,
            base_url,
            session_id: None,
            client,
        })
    }

    /// Attach the stable conversation session ID sent as
    /// [`OPENCODE_SESSION_HEADER`] (required by the Go surface; absent →
    /// systematic 400 as probed 2026-10-06).
    #[must_use]
    pub fn with_session_id(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Base URL this transport posts to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Full endpoint URL for `tier` (trailing slashes on the base are
    /// tolerated; path is tier-aware per [`cloud_path_suffix`]).
    fn endpoint_url(&self, tier: ConsentTier) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            cloud_path_suffix(tier)
        )
    }
}

impl Transport for HttpsTransport {
    fn send(
        &mut self,
        request: &ModelRequest,
        timeout: &TimeoutConfig,
    ) -> Result<ModelResponse, TransportError> {
        // Cloud-only guard: local tiers never touch this transport.
        if matches!(request.endpoint, Endpoint::Local) {
            return Err(TransportError::new(TransportKind::LocalDown));
        }
        // Empty caller key is a poisoned credential: refuse before any I/O.
        if request.model.is_empty() || self.api_key.is_empty() {
            return Err(TransportError::new(TransportKind::Unauthorized));
        }
        if self.base_url.trim().is_empty() {
            return Err(TransportError::new(TransportKind::ConnectionFailed));
        }
        let hard_ms = timeout.cloud_hard_ms.max(1);
        let started = Instant::now();
        let mut post = self
            .client
            .post(self.endpoint_url(request.tier))
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_millis(hard_ms))
            .json(&request_body(request));
        if let Some(session_id) = self.session_id.as_deref() {
            post = post.header(OPENCODE_SESSION_HEADER, session_id);
        }
        let outcome = post.send();
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match outcome {
            Err(err) if err.is_timeout() => Err(TransportError::new(TransportKind::Timeout)),
            Err(_) => Err(TransportError::new(TransportKind::ConnectionFailed)),
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    // Require a JSON body so shape drift surfaces as a
                    // retryable failure instead of silent success.
                    // Tier2 chat envelope verified live 2026-10-06
                    // (choices/message/content + reasoning_content ext).
                    match response.json::<serde_json::Value>() {
                        Ok(_) => Ok(ModelResponse {
                            tier: request.tier,
                            model: request.model.to_owned(),
                            latency_ms,
                        }),
                        Err(_) => Err(TransportError::new(TransportKind::ServerError)),
                    }
                } else {
                    let retry_after_ms = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        parse_retry_after_ms(response.headers())
                    } else {
                        None
                    };
                    Err(TransportError::with_retry_after(
                        status_kind(status),
                        retry_after_ms,
                    ))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Route parameters (all injected: no environment, clock, or I/O reads).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteParams {
    /// Raw `SYNTHLM_CONSENT_TIER` override (`None` = unset). Only consulted
    /// when no stored consent exists *and* the caller bypasses the gateway
    /// gate; [`crate::model_gw::Gateway::route`] BLOCKEDs on undecided consent first, so a
    /// stored tier always wins there (see [`crate::model_gw::resolve_tier`]).
    pub env_override_raw: Option<String>,
    /// Upload field names (must be a subset of `[prompt, mir, meta]`).
    pub fields: Vec<String>,
    /// Request body size in bytes (synthetic in tests; never PCM).
    pub byte_count: u64,
    /// Whether a cloud API key is configured. Checked only when the chain
    /// contains a cloud tier; Tier3-only routes never need it. Presence only:
    /// key material never enters the gateway.
    pub cloud_key_present: bool,
    /// Current time in milliseconds (drives breaker cooldowns and audit
    /// timestamps deterministically; production passes
    /// [`now_ms`](synthlm_common::ipc::now_ms)).
    pub now_ms: u64,
}

/// Successful route outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteResult {
    /// Serving response.
    pub response: ModelResponse,
    /// Tier attempted per transport call, in order (includes retries).
    pub attempted: Vec<ConsentTier>,
    /// One [`synthlm_common::ipc::AuditEvent`] per transport call, parallel to `attempted`.
    pub audits: Vec<AuditEvent>,
    /// Computed backoff delays ([`synthlm_common::ipc::RetryPolicy`]) that a blocking transport
    /// would sleep; reported, never slept.
    pub retry_delays: Vec<Duration>,
}

/// Three-tier model gateway: consent gate → whitelist gate → key gate →
/// Tier1→Tier2→Tier3 failover with per-tier retry and circuit breaking.
///
/// Owns one [`synthlm_common::ipc::CircuitBreaker`] per tier plus the [`synthlm_common::ipc::RetryPolicy`] and
/// [`TimeoutConfig`] in force. Time is injected per [`crate::model_gw::Gateway::route`] call
/// (`now_ms`), so breaker behavior is deterministic under test.
#[derive(Clone, Debug)]
pub struct Gateway {
    /// Backoff policy for retryable failures within a tier.
    retry: RetryPolicy,
    /// Timeout budgets presented to the transport.
    timeout: TimeoutConfig,
    /// Per-tier breakers, indexed by [`tier_index`].
    breakers: [CircuitBreaker; 3],
}

impl Default for Gateway {
    /// Default policy: [`RetryPolicy::default`] (3 attempts),
    /// [`TimeoutConfig::default`] (30 s cloud hard timeout), breakers tripping
    /// after 3 consecutive tier failures with a 30 s cooldown.
    fn default() -> Self {
        Self::new(3, 30_000)
    }
}

impl Gateway {
    /// Build with `failure_threshold` consecutive tier failures tripping a
    /// tier breaker and `cooldown_ms` of fast-fail before a half-open probe.
    /// Retry/timeout budgets stay at their defaults.
    pub fn new(failure_threshold: u32, cooldown_ms: u64) -> Self {
        Self {
            retry: RetryPolicy::default(),
            timeout: TimeoutConfig::default(),
            breakers: [
                CircuitBreaker::new(failure_threshold, cooldown_ms),
                CircuitBreaker::new(failure_threshold, cooldown_ms),
                CircuitBreaker::new(failure_threshold, cooldown_ms),
            ],
        }
    }

    /// Backoff policy in force.
    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry
    }

    /// Timeout budgets in force.
    pub fn timeouts(&self) -> TimeoutConfig {
        self.timeout
    }

    /// Current breaker state for `tier`.
    pub fn breaker_state(&self, tier: ConsentTier) -> CircuitState {
        self.breakers[tier_index(tier)].state()
    }

    /// Consecutive tier-route failures recorded for `tier`.
    pub fn consecutive_failures(&self, tier: ConsentTier) -> u32 {
        self.breakers[tier_index(tier)].consecutive_failures()
    }

    /// Route one request through the consent-ceiling chain.
    ///
    /// Gate order (all pre-attempt failures produce zero transport calls and
    /// zero audit events): consent → whitelist → key. Then each tier in
    /// `chain_for` order is attempted while its breaker
    /// [`can_attempt`](CircuitBreaker::can_attempt)s; attempts within a tier
    /// follow [`synthlm_common::ipc::RetryPolicy`] via [`should_retry`]. One [`synthlm_common::ipc::AuditEvent`] is
    /// recorded per transport attempt, including retries.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayError::ConsentRequired`] (undecided consent),
    /// [`GatewayError::InvalidTier`] (bad env override),
    /// [`GatewayError::WhitelistViolation`] (fields outside the whitelist),
    /// [`GatewayError::TierAudioUnsupported`] (audio at text-only Tier3),
    /// [`GatewayError::MissingKey`] (cloud tier without a key), or
    /// [`GatewayError::AllTiersExhausted`] (chain failed or all breakers
    /// open). Never call from an audio thread (blocking transport contract).
    pub fn route<T: Transport>(
        &mut self,
        state: &ConsentState,
        params: RouteParams,
        transport: &mut T,
    ) -> Result<RouteResult, GatewayError> {
        // 1. Fail-safe consent gate: undecided BLOCKEDs before anything else.
        let stored = require_consent(state).map_err(|_| GatewayError::ConsentRequired)?;
        // 2. Tier value resolution (stored always wins here; see resolve_tier).
        let env_override = parse_env_override(params.env_override_raw.as_deref())?;
        let start = resolve_tier(Some(stored), env_override);
        // 3. Whitelist gate: no request object exists before this passes.
        config::validate_upload_fields(&params.fields).map_err(|_| {
            GatewayError::WhitelistViolation {
                index: params
                    .fields
                    .iter()
                    .position(|field| !config::ALLOWED_UPLOAD_FIELDS.contains(&field.as_str()))
                    .unwrap_or(0),
            }
        })?;
        // 4. Key gate: only when the chain can touch the cloud.
        let chain = chain_for(start);
        if chain.iter().any(|tier| upload_allowed(*tier)) && !params.cloud_key_present {
            return Err(GatewayError::MissingKey);
        }

        let timestamp = i64::try_from(params.now_ms).unwrap_or(i64::MAX);
        let mut attempted: Vec<ConsentTier> = Vec::new();
        let mut audits: Vec<AuditEvent> = Vec::new();
        let mut retry_delays: Vec<Duration> = Vec::new();
        let mut last_code = ErrorCode::CloudUnavailable;
        let mut skip_cloud = false;

        for tier in chain {
            // Tier3 is text-only (DEC-010): an audio-bearing request that
            // exhausts into Tier3 fails closed instead of silently degrading
            // to a text-only understanding of an audio request.
            if tier == ConsentTier::Tier3
                && params
                    .fields
                    .iter()
                    .any(|field| field == config::AUDIO_FIELD)
            {
                return Err(GatewayError::TierAudioUnsupported);
            }
            // A 401 verdict earlier in this route poisons the shared key:
            // skip the remaining cloud tiers and jump to local Tier3.
            if skip_cloud && upload_allowed(tier) {
                continue;
            }
            let breaker = &mut self.breakers[tier_index(tier)];
            if !breaker.can_attempt(params.now_ms) {
                continue;
            }
            let request = ModelRequest::for_tier(tier, params.fields.clone(), params.byte_count);
            let mut attempts_used: u32 = 0;
            loop {
                attempts_used = attempts_used.saturating_add(1);
                attempted.push(tier);
                audits.push(AuditEvent::new(
                    request.model,
                    tier,
                    request.fields.clone(),
                    request.byte_count,
                    timestamp,
                ));
                match transport.send(&request, &self.timeout) {
                    Ok(response) => {
                        self.breakers[tier_index(tier)].record_success();
                        return Ok(RouteResult {
                            response,
                            attempted,
                            audits,
                            retry_delays,
                        });
                    }
                    Err(err) => {
                        last_code = err.code;
                        if err.kind == TransportKind::Unauthorized {
                            skip_cloud = true;
                        }
                        match should_retry(err.code, attempts_used, &self.retry) {
                            Some(delay) => {
                                retry_delays.push(delay);
                            }
                            None => {
                                self.breakers[tier_index(tier)].record_failure(params.now_ms);
                                break;
                            }
                        }
                    }
                }
            }
        }
        Err(GatewayError::AllTiersExhausted { code: last_code })
    }
}

/// Chain order for a consent-ceiling `start` tier.
fn chain_for(start: ConsentTier) -> Vec<ConsentTier> {
    match start {
        ConsentTier::Tier1 => vec![ConsentTier::Tier1, ConsentTier::Tier2, ConsentTier::Tier3],
        ConsentTier::Tier2 => vec![ConsentTier::Tier2, ConsentTier::Tier3],
        ConsentTier::Tier3 => vec![ConsentTier::Tier3],
    }
}

/// Breaker slot for `tier`.
fn tier_index(tier: ConsentTier) -> usize {
    match tier {
        ConsentTier::Tier1 => 0,
        ConsentTier::Tier2 => 1,
        ConsentTier::Tier3 => 2,
    }
}

// ---------------------------------------------------------------------------
// Tests (synthetic values only; no process env, no user dir, no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use synthlm_common::consent::ConsentStore;

    /// Stored-consent state for `tier` (synthetic timestamp; never persisted).
    fn decided(tier: ConsentTier) -> ConsentState {
        ConsentState::Decided(ConsentStore {
            tier,
            decided_at_unix: 1_789_000_000,
            version: synthlm_common::consent::CONFIG_VERSION,
        })
    }

    fn params(fields: &[&str]) -> RouteParams {
        RouteParams {
            env_override_raw: None,
            fields: fields.iter().map(|field| (*field).to_owned()).collect(),
            byte_count: 4096,
            cloud_key_present: true,
            now_ms: 1_789_000_000_000,
        }
    }

    #[test]
    fn tier_resolution_priority_stored_over_env_over_default() {
        assert_eq!(
            resolve_tier(Some(ConsentTier::Tier3), Some(ConsentTier::Tier1)),
            ConsentTier::Tier3
        );
        assert_eq!(
            resolve_tier(Some(ConsentTier::Tier2), None),
            ConsentTier::Tier2
        );
        assert_eq!(
            resolve_tier(None, Some(ConsentTier::Tier2)),
            ConsentTier::Tier2
        );
        assert_eq!(resolve_tier(None, None), DEFAULT_TIER);
        assert_eq!(resolve_tier(None, None), ConsentTier::Tier1);
    }

    #[test]
    fn env_override_parsing_matches_config_spellings() {
        assert_eq!(parse_env_override(None), Ok(None));
        assert_eq!(
            parse_env_override(Some("  ")),
            Ok(None),
            "blank behaves as unset"
        );
        assert_eq!(
            parse_env_override(Some("tier2")),
            Ok(Some(ConsentTier::Tier2))
        );
        assert_eq!(parse_env_override(Some("3")), Ok(Some(ConsentTier::Tier3)));
        assert_eq!(
            parse_env_override(Some("TIER1")),
            Ok(Some(ConsentTier::Tier1))
        );
        let err = parse_env_override(Some("tier9")).expect_err("unknown tier BLOCKED");
        assert_eq!(err, GatewayError::InvalidTier);
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(err.blocked());
    }

    #[test]
    fn resolve_model_matches_dec010_presets() {
        let tier1 = resolve_model(ConsentTier::Tier1);
        assert_eq!(tier1.model, DEFAULT_TIER1_MODEL);
        assert_eq!(
            tier1.endpoint,
            Endpoint::Cloud {
                base_url: DEFAULT_BASE_URL
            }
        );
        let tier2 = resolve_model(ConsentTier::Tier2);
        assert_eq!(tier2.model, DEFAULT_TIER2_MODEL);
        assert!(matches!(tier2.endpoint, Endpoint::Cloud { .. }));
        let tier3 = resolve_model(ConsentTier::Tier3);
        assert_eq!(tier3.model, DEFAULT_TIER3_MODEL);
        assert_eq!(tier3.endpoint, Endpoint::Local);
        assert!(upload_allowed(ConsentTier::Tier1));
        assert!(upload_allowed(ConsentTier::Tier2));
        assert!(!upload_allowed(ConsentTier::Tier3));
    }

    #[test]
    fn no_consent_blocks_before_any_call_or_audit() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let err = gateway
            .route(
                &ConsentState::Undecided,
                params(&["prompt"]),
                &mut transport,
            )
            .expect_err("undecided consent must BLOCK");
        assert_eq!(err, GatewayError::ConsentRequired);
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(err.blocked());
        assert!(!err.code().retryable());
        assert!(
            transport.calls().is_empty(),
            "nothing may leave the machine"
        );
        assert!(!err.guidance().is_empty());
    }

    #[test]
    fn whitelist_violation_blocks_before_any_call() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let err = gateway
            .route(
                &decided(ConsentTier::Tier1),
                params(&["prompt", "pcm"]),
                &mut transport,
            )
            .expect_err("pcm must BLOCK");
        assert_eq!(err, GatewayError::WhitelistViolation { index: 1 });
        assert_eq!(err.code(), ErrorCode::WhitelistViolation);
        assert!(err.blocked());
        assert!(transport.calls().is_empty());
        // Request-level constructor enforces the same gate value-free.
        let req_err = ModelRequest::new(ConsentTier::Tier1, vec!["api_key".to_owned()], 64)
            .expect_err("key-shaped field must BLOCK");
        assert_eq!(req_err, GatewayError::WhitelistViolation { index: 0 });
        assert!(!format!("{req_err}").contains("api_key"));
    }

    #[test]
    fn missing_key_blocks_cloud_chain_but_not_local() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let mut keyless = params(&["prompt"]);
        keyless.cloud_key_present = false;
        let err = gateway
            .route(&decided(ConsentTier::Tier1), keyless, &mut transport)
            .expect_err("cloud without key must BLOCK");
        assert_eq!(err, GatewayError::MissingKey);
        assert_eq!(err.code(), ErrorCode::AuthDenied);
        assert!(err.blocked());
        assert!(transport.calls().is_empty());

        // Tier3-only chains never need a key.
        let mut local_transport = MockTransport::all_ok();
        let mut local = params(&["mir"]);
        local.cloud_key_present = false;
        let ok = gateway
            .route(&decided(ConsentTier::Tier3), local, &mut local_transport)
            .expect("tier3 works keyless");
        assert_eq!(ok.response.tier, ConsentTier::Tier3);
        assert_eq!(ok.attempted, vec![ConsentTier::Tier3]);
    }

    #[test]
    fn tier3_audio_request_is_refused_before_transport() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let err = gateway
            .route(
                &decided(ConsentTier::Tier3),
                params(&["prompt", "audio_ref"]),
                &mut transport,
            )
            .expect_err("tier3 is text-only");
        assert_eq!(err, GatewayError::TierAudioUnsupported);
        assert_eq!(err.code(), ErrorCode::AudioCapabilityMissing);
        assert!(err.blocked());
        assert!(transport.calls().is_empty());
        assert!(!format!("{err}").contains("audio_ref"));
    }

    #[test]
    fn tier1_audio_request_flows_with_audio_field() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let outcome = gateway
            .route(
                &decided(ConsentTier::Tier1),
                params(&["prompt", "audio_ref"]),
                &mut transport,
            )
            .expect("tier1 serves audio");
        assert_eq!(outcome.response.tier, ConsentTier::Tier1);
        assert_eq!(outcome.attempted, vec![ConsentTier::Tier1]);
        assert!(
            outcome
                .audits
                .iter()
                .all(|audit| audit.fields.iter().any(|field| field == "audio_ref"))
        );
    }

    #[test]
    fn full_degrade_chain_tier1_down_tier2_down_tier3_ok() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::tier1_down_tier2_down_tier3_ok();
        let outcome = gateway
            .route(
                &decided(ConsentTier::Tier1),
                params(&["prompt", "mir", "meta"]),
                &mut transport,
            )
            .expect("tier3 must serve");
        assert_eq!(outcome.response.tier, ConsentTier::Tier3);
        assert_eq!(outcome.response.model, DEFAULT_TIER3_MODEL);
        // Default policy: 3 attempts per failing tier, then 1 serving call.
        assert_eq!(
            outcome.attempted,
            vec![
                ConsentTier::Tier1,
                ConsentTier::Tier1,
                ConsentTier::Tier1,
                ConsentTier::Tier2,
                ConsentTier::Tier2,
                ConsentTier::Tier2,
                ConsentTier::Tier3,
            ]
        );
        assert_eq!(transport.calls(), outcome.attempted.as_slice());
        // One audit per attempt, all whitelisted and key-free.
        assert_eq!(outcome.audits.len(), outcome.attempted.len());
        for (audit, tier) in outcome.audits.iter().zip(outcome.attempted.iter()) {
            assert_eq!(&audit.tier, tier);
            assert_eq!(audit.model, resolve_model(*tier).model);
            assert_eq!(audit.byte_count, 4096);
            for field in &audit.fields {
                assert!(
                    config::ALLOWED_UPLOAD_FIELDS.contains(&field.as_str()),
                    "audit field outside whitelist: {field}"
                );
            }
            let rendered = format!("{audit:?}");
            assert!(!rendered.contains("pcm"), "audit leaked audio: {rendered}");
            assert!(
                !rendered.to_ascii_lowercase().contains("key"),
                "audit leaked key material: {rendered}"
            );
        }
        // Computed backoff delays are reported (4 retries before serving).
        assert_eq!(outcome.retry_delays.len(), 4);
        // Default timeout budgets reach the transport untouched.
        assert!(
            transport
                .timeouts_seen_ms()
                .iter()
                .all(|ms| *ms == TimeoutConfig::default().cloud_hard_ms)
        );
    }

    #[test]
    fn all_down_blocks_after_full_chain() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_down();
        let err = gateway
            .route(
                &decided(ConsentTier::Tier1),
                params(&["prompt"]),
                &mut transport,
            )
            .expect_err("all tiers down must BLOCK");
        assert_eq!(
            err,
            GatewayError::AllTiersExhausted {
                code: ErrorCode::CloudUnavailable
            }
        );
        assert_eq!(
            transport.calls(),
            &[
                ConsentTier::Tier1,
                ConsentTier::Tier1,
                ConsentTier::Tier1,
                ConsentTier::Tier2,
                ConsentTier::Tier2,
                ConsentTier::Tier2,
                ConsentTier::Tier3,
                ConsentTier::Tier3,
                ConsentTier::Tier3,
            ]
        );
    }

    #[test]
    fn tier2_start_never_escalates_to_tier1() {
        let mut gateway = Gateway::default();
        // Tier2 down (3 attempts) → Tier3 ok; Tier1 must never be touched.
        let mut transport = MockTransport::new(vec![
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
        ]);
        let outcome = gateway
            .route(
                &decided(ConsentTier::Tier2),
                params(&["prompt"]),
                &mut transport,
            )
            .expect("tier3 fallback must serve");
        assert_eq!(outcome.response.tier, ConsentTier::Tier3);
        assert!(
            !transport.calls().contains(&ConsentTier::Tier1),
            "tier2 consent must never escalate to tier1: {:?}",
            transport.calls()
        );
        assert_eq!(
            transport.calls(),
            &[
                ConsentTier::Tier2,
                ConsentTier::Tier2,
                ConsentTier::Tier2,
                ConsentTier::Tier3,
            ]
        );
    }

    #[test]
    fn retryable_error_retries_within_tier_then_serves() {
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::new(vec![
            MockOutcome::Fail(TransportKind::Timeout),
            MockOutcome::Succeed { latency_ms: 12 },
        ]);
        let outcome = gateway
            .route(
                &decided(ConsentTier::Tier1),
                params(&["prompt"]),
                &mut transport,
            )
            .expect("retry must serve");
        assert_eq!(outcome.response.tier, ConsentTier::Tier1);
        assert_eq!(
            outcome.attempted,
            vec![ConsentTier::Tier1, ConsentTier::Tier1]
        );
        assert_eq!(outcome.audits.len(), 2, "retries are audited too");
        assert_eq!(outcome.retry_delays.len(), 1);
        assert_eq!(
            outcome.retry_delays[0],
            RetryPolicy::default().delay_for_attempt(0)
        );
    }

    #[test]
    fn unauthorized_skips_remaining_cloud_tiers_for_local() {
        let mut gateway = Gateway::default();
        let mut transport =
            MockTransport::new(vec![MockOutcome::Fail(TransportKind::Unauthorized)]);
        let outcome = gateway
            .route(
                &decided(ConsentTier::Tier1),
                params(&["prompt"]),
                &mut transport,
            )
            .expect("tier3 local needs no key and must serve");
        assert_eq!(outcome.response.tier, ConsentTier::Tier3);
        assert!(
            !transport.calls().contains(&ConsentTier::Tier2),
            "401 poisons the shared key: tier2 must be skipped: {:?}",
            transport.calls()
        );
        assert_eq!(transport.calls(), &[ConsentTier::Tier1, ConsentTier::Tier3]);
    }

    #[test]
    fn breakers_trip_and_recover_per_ipc_semantics() {
        let mut gateway = Gateway::new(2, 1_000);
        // Two all-down routes at t=0 trip every tier breaker.
        for _ in 0..2 {
            let mut down = MockTransport::all_down();
            let _ = gateway.route(
                &decided(ConsentTier::Tier1),
                RouteParams {
                    now_ms: 0,
                    ..params(&["prompt"])
                },
                &mut down,
            );
        }
        assert_eq!(
            gateway.breaker_state(ConsentTier::Tier1),
            CircuitState::Open
        );
        assert_eq!(gateway.consecutive_failures(ConsentTier::Tier1), 2);
        // Open breakers fast-fail: the next route attempts nothing.
        let mut skipped = MockTransport::all_ok();
        let err = gateway
            .route(
                &decided(ConsentTier::Tier1),
                RouteParams {
                    now_ms: 500,
                    ..params(&["prompt"])
                },
                &mut skipped,
            )
            .expect_err("open breakers must BLOCK without attempts");
        assert_eq!(
            err,
            GatewayError::AllTiersExhausted {
                code: ErrorCode::CloudUnavailable
            }
        );
        assert!(skipped.calls().is_empty());
        // After cooldown the breakers half-open: a probe passes, success closes.
        let mut probe = MockTransport::all_ok();
        let ok = gateway
            .route(
                &decided(ConsentTier::Tier1),
                RouteParams {
                    now_ms: 1_500,
                    ..params(&["prompt"])
                },
                &mut probe,
            )
            .expect("half-open probe must pass");
        assert_eq!(ok.response.tier, ConsentTier::Tier1);
        assert_eq!(
            gateway.breaker_state(ConsentTier::Tier1),
            CircuitState::Closed
        );
        assert_eq!(gateway.consecutive_failures(ConsentTier::Tier1), 0);
    }

    #[test]
    fn transport_kind_taxonomy_retryable_vs_blocked() {
        assert_eq!(TransportKind::Timeout.code(), ErrorCode::Timeout);
        assert_eq!(
            TransportKind::ConnectionFailed.code(),
            ErrorCode::TransportClosed
        );
        assert_eq!(TransportKind::Unauthorized.code(), ErrorCode::AuthDenied);
        assert_eq!(
            TransportKind::RateLimited.code(),
            ErrorCode::CloudUnavailable
        );
        assert_eq!(
            TransportKind::ServerError.code(),
            ErrorCode::CloudUnavailable
        );
        assert_eq!(TransportKind::LocalDown.code(), ErrorCode::CloudUnavailable);
        for retryable in [
            TransportKind::Timeout,
            TransportKind::ConnectionFailed,
            TransportKind::RateLimited,
            TransportKind::ServerError,
            TransportKind::LocalDown,
        ] {
            assert!(TransportError::new(retryable).retryable());
        }
        assert!(!TransportError::new(TransportKind::Unauthorized).retryable());
    }

    #[test]
    fn all_gateway_errors_expose_blocked_guidance() {
        let cases = [
            GatewayError::ConsentRequired,
            GatewayError::InvalidTier,
            GatewayError::WhitelistViolation { index: 0 },
            GatewayError::MissingKey,
            GatewayError::AllTiersExhausted {
                code: ErrorCode::CloudUnavailable,
            },
        ];
        for err in cases {
            assert_eq!(err.blocked(), !err.code().retryable());
            assert!(!err.guidance().is_empty());
        }
        // Exhaustion with a terminal last code is BLOCKED.
        let terminal = GatewayError::AllTiersExhausted {
            code: ErrorCode::AuthDenied,
        };
        assert!(terminal.blocked());
    }

    // ------------------------------------------------------------------
    // HttpsTransport loopback stub tests (TSK-116).
    //
    // Hermetic by construction: every stub binds `127.0.0.1` with an
    // ephemeral port, the credential is synthetic, and bodies carry field
    // names plus byte counts only (never prompt text, PCM, or key
    // material).
    // ------------------------------------------------------------------

    /// Synthetic credential for stub tests (never a real key).
    const STUB_KEY: &str = "synthetic-test-key-116";

    /// Canned stub reply for one accepted loopback connection.
    struct StubReply {
        status: u16,
        body: String,
        retry_after_secs: Option<u64>,
        delay_ms: u64,
    }

    impl StubReply {
        fn status(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_owned(),
                retry_after_secs: None,
                delay_ms: 0,
            }
        }
    }

    /// Captured wire request (loopback only).
    struct CapturedRequest {
        method: String,
        path: String,
        auth: String,
        content_type: String,
        session: String,
        body: String,
    }

    fn reason_for(status: u16) -> &'static str {
        match status {
            200 => "OK",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            _ => "Error",
        }
    }

    /// Serve exactly one connection on loopback with `reply`.
    ///
    /// Returns the base URL (with a trailing slash, pinning slash-tolerant
    /// joining) and the captured request.
    fn serve_once(reply: StubReply) -> (String, std::sync::mpsc::Receiver<CapturedRequest>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("stub binds loopback");
        let port = listener.local_addr().expect("stub reads its port").port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("stub accepts");
            // Read the head until CRLF CRLF (body may already be buffered).
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = stream.read(&mut chunk).expect("stub reads head");
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head_end = raw
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("stub parses head");
            let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
            let mut lines = head.lines();
            let request_line = lines.next().expect("stub reads request line").to_owned();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().expect("stub reads method").to_owned();
            let path = parts.next().expect("stub reads path").to_owned();
            let mut content_length = 0usize;
            let mut auth = String::new();
            let mut content_type = String::new();
            let mut session = String::new();
            for line in lines {
                if let Some((name, value)) = line.split_once(':') {
                    match name.trim().to_ascii_lowercase().as_str() {
                        "content-length" => {
                            content_length = value.trim().parse().expect("stub content-length");
                        }
                        "authorization" => {
                            auth = value.trim().to_owned();
                        }
                        "content-type" => {
                            content_type = value.trim().to_owned();
                        }
                        "x-opencode-session" => {
                            session = value.trim().to_owned();
                        }
                        _ => {}
                    }
                }
            }
            let mut body = raw[head_end + 4..].to_vec();
            while body.len() < content_length {
                let n = stream.read(&mut chunk).expect("stub reads body");
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(content_length);
            if reply.delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(reply.delay_ms));
            }
            let mut response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
                reply.status,
                reason_for(reply.status),
                reply.body.len(),
            );
            if let Some(secs) = reply.retry_after_secs {
                response.push_str(&format!("Retry-After: {secs}\r\n"));
            }
            response.push_str("Connection: close\r\n\r\n");
            response.push_str(&reply.body);
            // The client may have timed out already; a failed write is fine.
            let _ = stream.write_all(response.as_bytes());
            let _ = tx.send(CapturedRequest {
                method,
                path,
                auth,
                content_type,
                session,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        });
        (format!("http://127.0.0.1:{port}/"), rx)
    }

    fn cloud_request() -> ModelRequest {
        ModelRequest::new(
            ConsentTier::Tier1,
            vec!["prompt".to_owned(), "mir".to_owned()],
            64,
        )
        .expect("whitelisted fields build")
    }

    fn stub_transport(base_url: String) -> HttpsTransport {
        // Hermetic: stub tests must not observe a developer machine's proxy.
        HttpsTransport::new_hermetic(STUB_KEY.to_owned(), base_url).expect("stub transport builds")
    }

    fn recv_captured(rx: std::sync::mpsc::Receiver<CapturedRequest>) -> CapturedRequest {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("stub captures the request")
    }

    #[test]
    fn https_posts_responses_shape_with_model_and_fields() {
        let (base, rx) = serve_once(StubReply::status(
            200,
            r#"{"id":"resp_stub","model":"muse-spark-1.3-contributor","output":[]}"#,
        ));
        let mut transport = stub_transport(base);
        let response = transport
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect("stub 200 serves");
        assert_eq!(response.tier, ConsentTier::Tier1);
        assert_eq!(response.model, DEFAULT_TIER1_MODEL);

        let captured = recv_captured(rx);
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/responses");
        assert_eq!(captured.auth, format!("Bearer {STUB_KEY}"));
        assert!(
            captured.content_type.starts_with("application/json"),
            "unexpected content type: {}",
            captured.content_type
        );
        let wire: serde_json::Value =
            serde_json::from_str(&captured.body).expect("request body is JSON");
        assert_eq!(
            wire.get("model").and_then(|model| model.as_str()),
            Some(DEFAULT_TIER1_MODEL),
            "DEC-010 preset model must ride the wire"
        );
        let input = wire
            .get("input")
            .and_then(|input| input.as_str())
            .expect("responses input field");
        assert!(
            input.contains("prompt") && input.contains("mir"),
            "whitelisted fields must ride the wire: {input}"
        );
        assert!(
            !captured.body.contains(STUB_KEY),
            "key material must never appear in the body"
        );
        assert!(
            captured.session.is_empty(),
            "no session header unless explicitly attached"
        );
    }

    #[test]
    fn https_tier2_posts_chat_path_with_session_header() {
        let (base, rx) = serve_once(StubReply::status(
            200,
            r#"{"id":"chatcmpl-stub","object":"chat.completion","choices":[]}"#,
        ));
        let mut transport = stub_transport(base).with_session_id("stub-session-1".to_owned());
        let request = ModelRequest::new(ConsentTier::Tier2, vec!["prompt".to_owned()], 64)
            .expect("whitelisted fields build");
        transport
            .send(&request, &TimeoutConfig::default())
            .expect("stub 200 serves");
        let captured = recv_captured(rx);
        assert_eq!(captured.path, "/chat/completions");
        assert_eq!(captured.session, "stub-session-1");
        let wire: serde_json::Value =
            serde_json::from_str(&captured.body).expect("request body is JSON");
        assert_eq!(
            wire.get("model").and_then(|model| model.as_str()),
            Some(DEFAULT_TIER2_MODEL),
            "DEC-010 Tier2 preset model must ride the wire"
        );
        assert!(
            wire.get("messages")
                .and_then(|messages| messages.as_array())
                .is_some(),
            "Tier2 wire shape must carry messages"
        );
    }

    #[test]
    fn https_maps_401_to_terminal_auth_denied() {
        let (base, _) = serve_once(StubReply::status(401, r#"{"error":"unauthorized"}"#));
        let mut transport = stub_transport(base);
        let err = transport
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("401 must fail");
        assert_eq!(err.kind, TransportKind::Unauthorized);
        assert_eq!(err.code, ErrorCode::AuthDenied);
        assert!(!err.retryable());
        assert_eq!(err.retry_after_ms, None);
    }

    #[test]
    fn https_maps_429_with_and_without_retry_after() {
        let with_header = StubReply {
            retry_after_secs: Some(2),
            ..StubReply::status(429, r#"{"error":"rate limited"}"#)
        };
        let (base, _) = serve_once(with_header);
        let mut transport = stub_transport(base);
        let err = transport
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("429 must fail");
        assert_eq!(err.kind, TransportKind::RateLimited);
        assert_eq!(err.code, ErrorCode::CloudUnavailable);
        assert!(err.retryable());
        assert_eq!(err.retry_after_ms, Some(2000));

        let (bare_base, _) = serve_once(StubReply::status(429, r#"{"error":"rate limited"}"#));
        let mut bare = stub_transport(bare_base);
        let bare_err = bare
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("bare 429 must fail");
        assert_eq!(bare_err.kind, TransportKind::RateLimited);
        assert!(bare_err.retryable());
        assert_eq!(bare_err.retry_after_ms, None);
    }

    #[test]
    fn https_maps_500_to_retryable_server_error() {
        let (base, _) = serve_once(StubReply::status(500, r#"{"error":"boom"}"#));
        let mut transport = stub_transport(base);
        let err = transport
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("500 must fail");
        assert_eq!(err.kind, TransportKind::ServerError);
        assert_eq!(err.code, ErrorCode::CloudUnavailable);
        assert!(err.retryable());
    }

    #[test]
    fn https_rejects_non_json_success_as_server_error() {
        let (base, _) = serve_once(StubReply::status(200, "not json"));
        let mut transport = stub_transport(base);
        let err = transport
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("non-JSON 200 must fail");
        assert_eq!(err.kind, TransportKind::ServerError);
        assert!(err.retryable());
    }

    #[test]
    fn https_timeout_uses_per_call_hard_budget() {
        let delayed = StubReply {
            delay_ms: 1500,
            ..StubReply::status(200, r#"{"id":"resp_slow"}"#)
        };
        let (base, _) = serve_once(delayed);
        let mut transport = stub_transport(base);
        let timeout = TimeoutConfig::default().with_cloud_hard_ms(150);
        let err = transport
            .send(&cloud_request(), &timeout)
            .expect_err("stub delay past the hard budget must time out");
        assert_eq!(err.kind, TransportKind::Timeout);
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(err.retryable());
    }

    #[test]
    fn https_connection_refused_maps_to_retryable() {
        // Reserve then release a loopback port so nothing listens on it.
        // Hermetic client: a system proxy would synthesize 502/503 and the
        // kind would collapse to ServerError instead of ConnectionFailed.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe binds loopback");
        let port = probe.local_addr().expect("probe reads its port").port();
        drop(probe);
        let mut transport =
            HttpsTransport::new_hermetic(STUB_KEY.to_owned(), format!("http://127.0.0.1:{port}/"))
                .expect("hermetic transport builds");
        let err = transport
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("refused connection must fail");
        assert_eq!(err.kind, TransportKind::ConnectionFailed);
        assert_eq!(err.code, ErrorCode::TransportClosed);
        assert!(err.retryable());
    }

    #[test]
    fn loopback_bases_skip_system_proxy() {
        assert!(is_loopback_base("http://127.0.0.1:9/"));
        assert!(is_loopback_base("https://localhost:8080/v1"));
        assert!(is_loopback_base("http://[::1]:1/"));
        assert!(is_loopback_base("http://127.0.0.1"));
        assert!(!is_loopback_base("https://opencode.ai/zen/go/v1"));
        assert!(!is_loopback_base("http://10.0.0.2:8080/"));
        assert!(!is_loopback_base("not-a-url"));
    }

    #[test]
    fn https_refuses_local_tier_and_empty_key_without_io() {
        // Local tiers never touch this transport: no stub, no I/O.
        let mut transport =
            HttpsTransport::new(STUB_KEY.to_owned(), "http://127.0.0.1:9/".to_owned())
                .expect("transport builds");
        let local = ModelRequest::new(ConsentTier::Tier3, vec!["mir".to_owned()], 32)
            .expect("local request builds");
        let err = transport
            .send(&local, &TimeoutConfig::default())
            .expect_err("local must not use the cloud transport");
        assert_eq!(err.kind, TransportKind::LocalDown);

        // Empty caller keys are refused before any I/O (nothing listens).
        let mut keyless = HttpsTransport::new(String::new(), "http://127.0.0.1:9/".to_owned())
            .expect("transport builds");
        let key_err = keyless
            .send(&cloud_request(), &TimeoutConfig::default())
            .expect_err("empty key is refused");
        assert_eq!(key_err.kind, TransportKind::Unauthorized);
        assert!(!key_err.retryable());

        // Debug redacts the credential while keeping the endpoint visible.
        let rendered = format!("{transport:?}");
        assert!(rendered.contains("127.0.0.1"));
        assert!(
            !rendered.contains(STUB_KEY),
            "debug must redact the key: {rendered}"
        );
    }

    #[test]
    fn https_retry_after_parsing_covers_header_forms() {
        assert_eq!(
            parse_retry_after_ms(&reqwest::header::HeaderMap::new()),
            None
        );
        let mut seconds = reqwest::header::HeaderMap::new();
        seconds.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("2"),
        );
        assert_eq!(parse_retry_after_ms(&seconds), Some(2000));
        let mut garbage = reqwest::header::HeaderMap::new();
        garbage.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("not-a-number"),
        );
        assert_eq!(parse_retry_after_ms(&garbage), None);
    }

    #[test]
    fn https_endpoint_url_tolerates_trailing_slashes() {
        let transport = HttpsTransport::new(
            STUB_KEY.to_owned(),
            "https://opencode.ai/zen/go/v1/".to_owned(),
        )
        .expect("transport builds");
        assert_eq!(
            transport.endpoint_url(ConsentTier::Tier1),
            "https://opencode.ai/zen/go/v1/responses"
        );
        assert_eq!(
            transport.endpoint_url(ConsentTier::Tier2),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
    }
}
