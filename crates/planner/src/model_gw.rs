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
//! ## No-network rule (this task)
//!
//! This module performs **no real network I/O** and introduces **no HTTP
//! client dependency**: the only transport is the [`crate::model_gw::Transport`] trait plus
//! the programmable [`crate::model_gw::MockTransport`] fault-injection double. Wiring a real
//! HTTPS client (reqwest or equivalent, with its `docs/LICENSES.md` entry)
//! is explicitly left to a follow-up task (see the crate docs note in
//! `lib.rs`).
//!
//! Blocking contract: [`crate::model_gw::Gateway::route`] drives a synchronous transport and
//! may block the calling thread on future real transports. Never call it
//! from an audio thread (AGENTS.md red line 2); it is a control-plane helper.

use std::collections::VecDeque;
use std::time::Duration;

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
        "upload field at index {index} is outside the audit whitelist [prompt, mir, meta]; nothing was sent (see DEC-011)"
    )]
    WhitelistViolation {
        /// Position of the first offending field (no value stored).
        index: usize,
    },
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
                "restrict upload fields to the whitelist [prompt, mir, meta]; PCM and key material must stay local (see DEC-011)"
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
    /// Whitelisted upload field names (subset of `[prompt, mir, meta]`).
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
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("model transport failed: {kind:?} (code {code:?}; see DEC-011)")]
pub struct TransportError {
    /// Failure class.
    pub kind: TransportKind,
    /// Taxonomy code derived from [`TransportKind::code`].
    pub code: ErrorCode,
}

impl TransportError {
    /// Build from a [`TransportKind`], deriving the [`synthlm_common::ipc::ErrorCode`].
    pub fn new(kind: TransportKind) -> Self {
        Self {
            kind,
            code: kind.code(),
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
}
