//! Intent planning over explicit model backends (TSK-503, DEC-010/011/013).
//!
//! Replaces the demo-seeded plan path with a real Tier2-text-first flow while
//! keeping the seeded path available, explicitly labeled, and impossible to
//! confuse with a model result:
//!
//! - [`crate::planning::ModelBackend::MockSeeded`]: the pre-existing
//!   deterministic candidates (fixed `reaeq` skeleton, no transport, no
//!   network) carrying the `seeded-demo` watermark. The serving path and the
//!   `acrd demo` subcommand stay on this backend.
//! - [`crate::planning::ModelBackend::LiveTier2`]: consent gate → whitelist
//!   gate → key gate → Tier2 chat via
//!   [`crate::model_gw::Gateway::route`], then lenient text→Patch parsing,
//!   [`crate::patch::validate`] + [`crate::patch::validate_with_repair`]
//!   (at most [`crate::patch::DEFAULT_MAX_REPAIR_ROUNDS`] rounds, counts
//!   reported), then [`crate::candidate::diversify`] into candidates.
//!
//! Every failure is user-visible `BLOCKED` semantics (or the retryable
//! taxonomy verdict it wraps) and value-free: no variant stores intent text,
//! field names, key material, or model output, so formatting an error can
//! never echo secrets (AGENTS.md §8). A result that is not served by Tier2 is
//! never labeled as one: Tier3 fallback service, empty text, unparsable
//! text, and unrepairable residuals all return errors, never a plan stamped
//! with [`crate::planning::LIVE_BACKEND_LABEL`].
//!
//! Honesty boundaries (DEC-010/011):
//!
//! - Key custody: the live context carries key *presence* only (a `bool`),
//!   mirroring [`crate::model_gw::RouteParams`]; key material never enters
//!   this module.
//! - Prompt custody: `intent_text` selects the deterministic snapshot stem
//!   and stays local; only whitelisted field *names* plus a byte count ride
//!   the gateway request (the synthetic body owned by
//!   [`crate::model_gw`]). Rich prompt/MIR payload wiring lands with the
//!   caller that owns that content.
//! - Snapshot binding (DEC-004): plans solve against a deterministic
//!   `intent-<hash>` stem. The dispatch layer rebinds the surviving plan to
//!   the frozen snapshot id before journaling; ops are untouched by the
//!   rebind, so validation still holds and `patch.apply` re-validates
//!   anyway.
//!
//! Blocking contract: [`crate::planning::plan_for_intent`] drives a
//! synchronous transport through the gateway and may block the calling
//! thread on real transports. Never call it from an audio thread (AGENTS.md
//! red line 2); it is a control-plane helper.

use thiserror::Error;

use synthlm_common::consent::ConsentState;
use synthlm_common::ipc::{AuditEvent, ConsentTier, ErrorCode};
use synthlm_profile::schema::Profile;

use crate::candidate::{
    Candidate, Direction, PoolEntry, ScoreSnapshot, count_changed_ops, diversify,
};
use crate::model_gw::{Gateway, GatewayError, RouteParams, Transport};
use crate::patch::{DEFAULT_MAX_REPAIR_ROUNDS, PatchPlan, validate_with_repair};

// ---------------------------------------------------------------------------
// Backend labels (wire watermarks)
// ---------------------------------------------------------------------------

/// Wire `backend` label for the seeded path: contains both the `mock` marker
/// (asserted never erasable) and the `seeded-demo` watermark.
pub const MOCK_BACKEND_LABEL: &str = "mock-seeded-demo";

/// Wire `backend` label for the live Tier2 path. Only ever emitted on a
/// result actually served by Tier2 (see [`crate::planning::plan_for_intent`]).
pub const LIVE_BACKEND_LABEL: &str = "live-tier2";

/// Marker model id for seeded outcomes: no model ran.
pub const MOCK_SEEDED_MODEL: &str = "seeded-demo-no-model-call";

// ---------------------------------------------------------------------------
// Backend selection (explicit, type-level distinct)
// ---------------------------------------------------------------------------

/// Which planning backend may run. The caller constructs exactly one variant
/// — there is no default, no ambient flag, and no silent upgrade from seeded
/// to live — so a mock result can never be mistaken for a model result.
#[derive(Debug)]
pub enum ModelBackend<'a, T: Transport> {
    /// Deterministic seeded candidates: no consent needed, no key needed, no
    /// transport calls, no network. Carries [`crate::planning::MOCK_BACKEND_LABEL`].
    MockSeeded,
    /// Live Tier2 text path. Requires decided Tier1/Tier2 consent, a
    /// whitelisted field set, and a configured cloud key; the gateway
    /// enforces all three before any transport use. Carries
    /// [`crate::planning::LIVE_BACKEND_LABEL`] only when Tier2 serves.
    LiveTier2 {
        /// Gateway owning retry/timeout policy and per-tier breakers.
        gateway: &'a mut Gateway,
        /// Transport placing the call (real HTTPS or a loopback stub in
        /// tests; never a real endpoint outside human-authorized probes).
        transport: &'a mut T,
        /// Stored consent state (consent gate runs first: undecided BLOCKEDs
        /// with zero transport calls).
        consent: &'a ConsentState,
        /// Whitelisted upload field names (subset of
        /// `[prompt, mir, meta, audio_ref]`).
        fields: Vec<String>,
        /// Request body size in bytes (synthetic in tests; never PCM).
        byte_count: u64,
        /// Whether a cloud API key is configured (presence only; key
        /// material never enters this module).
        cloud_key_present: bool,
        /// Current time in milliseconds (drives breaker cooldowns and audit
        /// timestamps deterministically).
        now_ms: u64,
    },
}

/// Backend kind without transport lifetimes: what actually ran, recorded on
/// every [`crate::planning::PlanOutcome`] so the watermark cannot be dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelBackendKind {
    /// Seeded deterministic path (never a model result).
    MockSeeded,
    /// Live Tier2 path (only when Tier2 served).
    LiveTier2,
}

impl ModelBackendKind {
    /// Wire label for `plan.response` (`backend` field).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ModelBackendKind::MockSeeded => MOCK_BACKEND_LABEL,
            ModelBackendKind::LiveTier2 => LIVE_BACKEND_LABEL,
        }
    }

    /// Whether this kind is a live-model result.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, ModelBackendKind::LiveTier2)
    }
}

impl<T: Transport> ModelBackend<'_, T> {
    /// Backend kind (what will run / ran).
    #[must_use]
    pub fn kind(&self) -> ModelBackendKind {
        match self {
            ModelBackend::MockSeeded => ModelBackendKind::MockSeeded,
            ModelBackend::LiveTier2 { .. } => ModelBackendKind::LiveTier2,
        }
    }

    /// Whether this backend performs a live-model call.
    ///
    /// Type-level query: only [`crate::planning::ModelBackend::LiveTier2`]
    /// returns `true`, and constructing that variant requires the full live
    /// context (gateway, transport, consent), so mock code paths cannot drift
    /// into claiming a model result.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.kind().is_live()
    }

    /// Wire label for `plan.response` (`backend` field).
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.kind().label()
    }
}

// ---------------------------------------------------------------------------
// Errors (value-free, BLOCKED-first)
// ---------------------------------------------------------------------------

/// Intent-planning failure. Variants store no caller material — not intent
/// text, not field names, not model output, not key material — so formatting
/// an error can never echo secrets (AGENTS.md §8).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PlanningError {
    /// The gateway leg failed (consent / whitelist / key / exhaustion). The
    /// inner verdict is preserved, so terminal gates stay BLOCKED and
    /// retryable transport states stay retryable.
    #[error("model gateway leg failed: {0}")]
    Gateway(#[from] GatewayError),
    /// The route served, but not from Tier2 (consent-ceiling fallback to
    /// local Tier3, or a Tier1 tier serving a Tier2-labeled request). Never
    /// labeled as a model result; retryable per the exhaustion taxonomy
    /// (a later retry may reach Tier2).
    #[error(
        "no text-tier model served this request: refusing to label a fallback as a model result (see DEC-010)"
    )]
    NoLiveModel,
    /// The served text contained no Patch JSON (after fence-stripping and
    /// brace/bracket slicing). Terminal: retrying the same text cannot help.
    #[error(
        "model text carried no patch document: expected a JSON object with ops after fence-stripping (see DEC-013)"
    )]
    Unparsable,
    /// At least one parsed plan survived parsing but none survived repair:
    /// carries the first residual [`synthlm_common::ipc::ErrorCode`].
    /// Terminal (every [`crate::patch::PatchError`] is BLOCKED).
    #[error("patch residual after repair: first residual was {code:?} (see DEC-013)")]
    Unrepairable {
        /// First residual error class.
        code: ErrorCode,
    },
    /// No plan survived (nothing parsed, or repair dropped everything with
    /// no residuals to name). Terminal.
    #[error(
        "no applicable plan survived: every candidate text failed parsing or repair (see DEC-013)"
    )]
    EmptyPlan,
    /// Candidate assembly failed internally (constructor or pool invariant).
    #[error("candidate assembly failed: internal planning error (see ARCH §8)")]
    ChainFailed,
}

impl PlanningError {
    /// Map to the [`synthlm_common::ipc`] taxonomy.
    #[must_use]
    pub fn code(self) -> ErrorCode {
        match self {
            PlanningError::Gateway(inner) => inner.code(),
            PlanningError::NoLiveModel => ErrorCode::CloudUnavailable,
            PlanningError::Unparsable | PlanningError::EmptyPlan => ErrorCode::ProtocolViolation,
            PlanningError::Unrepairable { code } => code,
            PlanningError::ChainFailed => ErrorCode::Internal,
        }
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    #[must_use]
    pub fn blocked(self) -> bool {
        !self.code().retryable()
    }

    /// Whether this failure may enter backoff retry.
    #[must_use]
    pub fn retryable(self) -> bool {
        self.code().retryable()
    }

    /// Static remediation hint for UI / `BLOCKED` surfaces.
    #[must_use]
    pub fn guidance(self) -> &'static str {
        match self {
            PlanningError::Gateway(inner) => inner.guidance(),
            PlanningError::NoLiveModel => {
                "the text tier did not serve; check cloud connectivity and consent tier, then retry (Tier1 ignition needs separate approval, see DEC-010)"
            }
            PlanningError::Unparsable => {
                "the model did not return a patch document; retry once, then tighten the output schema (see DEC-013)"
            }
            PlanningError::Unrepairable { .. } => {
                "retarget ops to whitelisted idents (param/<ident> or macro/<name>) and resend (see DEC-013)"
            }
            PlanningError::EmptyPlan => {
                "every parsed op was illegal; the model must emit at least one whitelisted op (see DEC-013)"
            }
            PlanningError::ChainFailed => {
                "internal planning failure; retry once, then file the intent hash + profile state (see ARCH §8)"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// Successful intent plan: the surviving patch, diversified candidates, the
/// repair audit trail, gateway audits, and the backend watermark.
///
/// Fields are private so the backend watermark cannot be bypassed with a
/// struct literal: outside this module every instance comes from
/// [`crate::planning::plan_for_intent`], which stamps the kind that actually
/// ran.
#[derive(Clone, Debug)]
pub struct PlanOutcome {
    backend: ModelBackendKind,
    plan: PatchPlan,
    candidates: Vec<Candidate>,
    direction_gap: bool,
    repair_rounds: usize,
    removed_total: usize,
    replaced_total: usize,
    audits: Vec<AuditEvent>,
    serving_tier: Option<ConsentTier>,
    model: String,
}

impl PlanOutcome {
    /// Which backend ran (the unerased watermark).
    #[must_use]
    pub fn backend(&self) -> ModelBackendKind {
        self.backend
    }

    /// Wire `backend` label for `plan.response`.
    #[must_use]
    pub fn backend_label(&self) -> &'static str {
        self.backend.label()
    }

    /// The surviving (repaired, accepted) patch.
    #[must_use]
    pub fn plan(&self) -> &PatchPlan {
        &self.plan
    }

    /// Diversified candidates, score-ordered.
    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// True when survivors cover fewer than all three directions (callers
    /// surface the gap instead of guessing).
    #[must_use]
    pub fn direction_gap(&self) -> bool {
        self.direction_gap
    }

    /// Total repair rounds run across parsed plans (each at most
    /// [`crate::patch::DEFAULT_MAX_REPAIR_ROUNDS`]).
    #[must_use]
    pub fn repair_rounds(&self) -> usize {
        self.repair_rounds
    }

    /// Total ops dropped by repair.
    #[must_use]
    pub fn removed_total(&self) -> usize {
        self.removed_total
    }

    /// Total ops value-substituted by repair.
    #[must_use]
    pub fn replaced_total(&self) -> usize {
        self.replaced_total
    }

    /// Gateway audit events for the live call, parallel to transport
    /// attempts (empty on the seeded path, which places no calls).
    #[must_use]
    pub fn audits(&self) -> &[AuditEvent] {
        &self.audits
    }

    /// Tier that served the text (`None` on the seeded path: nothing ran).
    #[must_use]
    pub fn serving_tier(&self) -> Option<ConsentTier> {
        self.serving_tier
    }

    /// Serving model name, or the seeded no-model marker.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Plan for `intent_text` on `backend` against `profile`.
///
/// - [`crate::planning::ModelBackend::MockSeeded`]: deterministic skeleton
///   (the pre-existing three-op `reaeq` draft), dual-validated with repair,
///   diversified over the fixed well-separated lane grid. No consent, key,
///   transport, or network involved.
/// - [`crate::planning::ModelBackend::LiveTier2`]: gateway route
///   (consent → whitelist → key → Tier2 chat) recovers the model text; the
///   text is leniently parsed to patch documents (fence-stripped,
///   brace/bracket-sliced, snapshot-injected), each dual-validated with at
///   most [`crate::patch::DEFAULT_MAX_REPAIR_ROUNDS`] repair rounds, and
///   survivors are diversified into candidates. Any step failing BLOCKEDs
///   the whole call with its reason; a non-Tier2 serving tier, empty text,
///   unparsable text, or unrepairable residuals all error without ever
///   emitting [`crate::planning::LIVE_BACKEND_LABEL`].
///
/// # Errors
///
/// Returns [`crate::planning::PlanningError`] (BLOCKED-first, value-free;
/// see each variant). Never call from an audio thread (blocking transport
/// contract).
pub fn plan_for_intent<T: Transport>(
    backend: ModelBackend<'_, T>,
    profile: &Profile,
    intent_text: &str,
) -> Result<PlanOutcome, PlanningError> {
    let stem = intent_stem(intent_text);
    match backend {
        ModelBackend::MockSeeded => plan_mock_seeded(profile, &stem),
        ModelBackend::LiveTier2 {
            gateway,
            transport,
            consent,
            fields,
            byte_count,
            cloud_key_present,
            now_ms,
        } => plan_live_tier2(
            gateway,
            transport,
            consent,
            profile,
            &stem,
            fields,
            byte_count,
            cloud_key_present,
            now_ms,
        ),
    }
}

// ---------------------------------------------------------------------------
// MockSeeded path (deterministic, offline)
// ---------------------------------------------------------------------------

/// Run the seeded path: fixed skeleton → dual validation with repair →
/// fixed-grid diversification. Pure except for no clock reads at all.
fn plan_mock_seeded(profile: &Profile, stem: &str) -> Result<PlanOutcome, PlanningError> {
    let draft = seeded_draft_plan(stem);
    let repaired = validate_with_repair(&draft, profile, DEFAULT_MAX_REPAIR_ROUNDS);
    if !repaired.accepted() {
        let code = repaired
            .residual_errors
            .first()
            .map_or(ErrorCode::Internal, |err| err.code());
        return Err(PlanningError::Unrepairable { code });
    }
    if repaired.plan.ops.is_empty() {
        return Err(PlanningError::EmptyPlan);
    }
    let (pool_candidates, direction_gap) = mock_pool(&repaired.plan, stem)?;
    if pool_candidates.is_empty() {
        return Err(PlanningError::EmptyPlan);
    }
    let repair_rounds = repaired.rounds_used();
    let removed_total = repaired
        .reports
        .iter()
        .map(|report| report.removed.len())
        .sum();
    let replaced_total = repaired
        .reports
        .iter()
        .map(|report| report.replaced.len())
        .sum();
    Ok(PlanOutcome {
        backend: ModelBackendKind::MockSeeded,
        plan: repaired.plan,
        candidates: pool_candidates,
        direction_gap,
        repair_rounds,
        removed_total,
        replaced_total,
        audits: Vec::new(),
        serving_tier: None,
        model: MOCK_SEEDED_MODEL.to_owned(),
    })
}

/// The seeded draft plan: three `replace` ops on real `reaeq` slider idents
/// (the pre-existing deterministic skeleton, moved here from the dispatch
/// layer so both callers solve identically).
fn seeded_draft_plan(stem: &str) -> PatchPlan {
    use crate::patch::{IdentPath, PatchOp, PatchOpKind};
    PatchPlan {
        ops: vec![
            PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new("param/4:_Gain_Band_2"),
                value: serde_json::json!(0.6),
            },
            PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new("param/7:_Gain_Band_3"),
                value: serde_json::json!(0.4),
            },
            PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new("param/17:wet"),
                value: serde_json::json!(0.8),
            },
        ],
        target_snapshot: stem.to_owned(),
    }
}

/// Diversify the seeded plan over the fixed well-separated lane grid
/// (minimum pairwise distance ~0.28, outside the 0.2 dedup radius), so the
/// seeded chain deterministically yields a full 3-candidate shortlist with no
/// direction gap.
///
/// Returns the diversified candidates plus the gap flag.
fn mock_pool(plan: &PatchPlan, stem: &str) -> Result<(Vec<Candidate>, bool), PlanningError> {
    let changed = count_changed_ops(plan);
    let lanes: [Vec<f64>; 4] = [
        vec![0.6, 0.4],
        vec![0.2, 0.8],
        vec![0.85, 0.15],
        vec![0.4, 0.6],
    ];
    let directions = [
        Direction::Darker,
        Direction::Transient,
        Direction::Spatial,
        Direction::Darker,
    ];
    let diffs = [
        "低频更暗，整体更靠后。",
        "瞬态更紧，起振更清晰。",
        "声场更宽，混响感略增。",
        "低频进一步收紧，亮度略降。",
    ];
    let totals = [30.0, 20.0, 10.0, 5.0];
    let confidences = [0.8, 0.75, 0.7, 0.65];
    let mut pool = Vec::with_capacity(lanes.len());
    for (index, lane) in lanes.iter().enumerate() {
        let Some(total) = totals.get(index) else {
            return Err(PlanningError::ChainFailed);
        };
        let Some(confidence) = confidences.get(index) else {
            return Err(PlanningError::ChainFailed);
        };
        let Some(diff) = diffs.get(index) else {
            return Err(PlanningError::ChainFailed);
        };
        let Some(direction) = directions.get(index) else {
            return Err(PlanningError::ChainFailed);
        };
        let score = ScoreSnapshot::new(*total).map_err(|_| PlanningError::ChainFailed)?;
        let candidate = Candidate::new(
            format!("{stem}-c{}", index + 1),
            plan.clone(),
            (*diff).to_owned(),
            *confidence,
            0.0,
            changed,
            score,
        )
        .map_err(|_| PlanningError::ChainFailed)?;
        pool.push(
            PoolEntry::new(candidate, lane.clone(), *direction)
                .map_err(|_| PlanningError::ChainFailed)?,
        );
    }
    let diversified = diversify(&pool, 3);
    Ok((
        diversified.candidates().to_vec(),
        diversified.direction_gap(),
    ))
}

// ---------------------------------------------------------------------------
// LiveTier2 path (gateway → text → patch → diversify)
// ---------------------------------------------------------------------------

/// Run the live Tier2 path: gateway route, Tier2-service check, lenient
/// parse, per-plan repair (≤ 2 rounds), diversification.
#[allow(clippy::too_many_arguments)]
fn plan_live_tier2<T: Transport>(
    gateway: &mut Gateway,
    transport: &mut T,
    consent: &ConsentState,
    profile: &Profile,
    stem: &str,
    fields: Vec<String>,
    byte_count: u64,
    cloud_key_present: bool,
    now_ms: u64,
) -> Result<PlanOutcome, PlanningError> {
    let route = gateway.route(
        consent,
        RouteParams {
            env_override_raw: None,
            fields,
            byte_count,
            cloud_key_present,
            now_ms,
        },
        transport,
    )?;
    if route.response.tier != ConsentTier::Tier2 {
        return Err(PlanningError::NoLiveModel);
    }
    let docs = extract_plan_docs(&route.response.text);
    if docs.is_empty() {
        return Err(PlanningError::Unparsable);
    }
    let mut survivors: Vec<PatchPlan> = Vec::new();
    let mut repair_rounds = 0usize;
    let mut removed_total = 0usize;
    let mut replaced_total = 0usize;
    let mut first_code: Option<ErrorCode> = None;
    for doc in &docs {
        let candidate = inject_snapshot(doc, stem);
        let Some(candidate) = candidate else {
            if first_code.is_none() {
                first_code = Some(ErrorCode::ProtocolViolation);
            }
            continue;
        };
        let repaired = validate_with_repair(&candidate, profile, DEFAULT_MAX_REPAIR_ROUNDS);
        repair_rounds += repaired.rounds_used();
        removed_total += repaired
            .reports
            .iter()
            .map(|report| report.removed.len())
            .sum::<usize>();
        replaced_total += repaired
            .reports
            .iter()
            .map(|report| report.replaced.len())
            .sum::<usize>();
        if !repaired.accepted() {
            if first_code.is_none() {
                first_code = Some(
                    repaired
                        .residual_errors
                        .first()
                        .map_or(ErrorCode::Internal, |err| err.code()),
                );
            }
            continue;
        }
        if repaired.plan.ops.is_empty() {
            if first_code.is_none() {
                first_code = Some(ErrorCode::ProtocolViolation);
            }
            continue;
        }
        survivors.push(repaired.plan);
    }
    if survivors.is_empty() {
        if let Some(code) = first_code {
            return Err(PlanningError::Unrepairable { code });
        }
        return Err(PlanningError::EmptyPlan);
    }
    let primary = survivors[0].clone();
    let (candidates, direction_gap) = live_pool(&survivors, stem)?;
    if candidates.is_empty() {
        return Err(PlanningError::EmptyPlan);
    }
    Ok(PlanOutcome {
        backend: ModelBackendKind::LiveTier2,
        plan: primary,
        candidates,
        direction_gap,
        repair_rounds,
        removed_total,
        replaced_total,
        audits: route.audits,
        serving_tier: Some(ConsentTier::Tier2),
        model: route.response.model,
    })
}

/// Build one candidate per surviving plan. Lanes project op values to finite
/// numbers (bools as 1.0/0.0, numeric strings when parseable, else the
/// neutral midpoint); directions round-robin over the forced-coverage set so
/// [`crate::candidate::diversify`] can honestly report the resulting gap.
fn live_pool(plans: &[PatchPlan], stem: &str) -> Result<(Vec<Candidate>, bool), PlanningError> {
    let mut pool = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let lane: Vec<f64> = plan.ops.iter().map(|op| lane_value(&op.value)).collect();
        if lane.is_empty() {
            return Err(PlanningError::ChainFailed);
        }
        let direction = match index % 3 {
            0 => Direction::Darker,
            1 => Direction::Transient,
            _ => Direction::Spatial,
        };
        let score = ScoreSnapshot::new(0.0).map_err(|_| PlanningError::ChainFailed)?;
        let candidate = Candidate::new(
            format!("{stem}-c{}", index + 1),
            plan.clone(),
            format!("模型建议调整 {} 个参数。", plan.ops.len()),
            0.5,
            0.0,
            count_changed_ops(plan),
            score,
        )
        .map_err(|_| PlanningError::ChainFailed)?;
        pool.push(
            PoolEntry::new(candidate, lane, direction).map_err(|_| PlanningError::ChainFailed)?,
        );
    }
    let diversified = diversify(&pool, 3);
    Ok((
        diversified.candidates().to_vec(),
        diversified.direction_gap(),
    ))
}

/// Project one op value to a finite lane number (bools as 1.0/0.0, numeric
/// strings when parseable, otherwise the neutral midpoint; non-finite or
/// non-scalar as 0.0).
fn lane_value(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Number(number) => match number.as_f64() {
            Some(scalar) if scalar.is_finite() => scalar,
            _ => 0.0,
        },
        serde_json::Value::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        serde_json::Value::String(text) => match text.trim().parse::<f64>() {
            Ok(scalar) if scalar.is_finite() => scalar,
            _ => 0.5,
        },
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Lenient text → plan documents
// ---------------------------------------------------------------------------

/// Extract candidate JSON documents from model text, in order: the whole
/// trimmed text, a fenced-code-block interior, the first-brace/last-brace
/// slice, then the first-bracket/last-bracket slice (array-first when the
/// trimmed text opens with `[`). The first slice that yields at least one
/// plan-shaped document wins; anything else yields no documents (the caller
/// BLOCKEDs as unparsable, never guessing).
fn extract_plan_docs(text: &str) -> Vec<serde_json::Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut slices: Vec<&str> = vec![trimmed];
    if let Some(inner) = strip_code_fence(trimmed) {
        slices.push(inner);
    }
    let object_slice = slice_between(trimmed, '{', '}');
    let array_slice = slice_between(trimmed, '[', ']');
    if trimmed.starts_with('[') {
        if let Some(slice) = array_slice {
            slices.push(slice);
        }
        if let Some(slice) = object_slice {
            slices.push(slice);
        }
    } else {
        if let Some(slice) = object_slice {
            slices.push(slice);
        }
        if let Some(slice) = array_slice {
            slices.push(slice);
        }
    }
    for slice in slices {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(slice) {
            let docs = value_to_docs(&value);
            if !docs.is_empty() {
                return docs;
            }
        }
    }
    Vec::new()
}

/// Split a decoded document into plan-shaped members: a top-level array of
/// plans, a `{"patches": [...]}` envelope, or the single object itself.
/// Members that are not JSON objects are dropped (never guessed into plans).
fn value_to_docs(value: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter(|item| item.is_object())
            .cloned()
            .collect();
    }
    if let Some(items) = value.get("patches").and_then(|patches| patches.as_array()) {
        return items
            .iter()
            .filter(|item| item.is_object())
            .cloned()
            .collect();
    }
    if value.is_object() {
        return vec![value.clone()];
    }
    Vec::new()
}

/// Inject the deterministic snapshot stem into a plan-shaped document when
/// its `target_snapshot` is missing or empty. Returns `None` when the
/// document is not a plan object at all (wrong types, unknown fields) — the
/// caller counts that toward unparsable, never guessing field values.
fn inject_snapshot(doc: &serde_json::Value, stem: &str) -> Option<PatchPlan> {
    let mut object = doc.as_object()?.clone();
    let needs_inject = object
        .get("target_snapshot")
        .and_then(|snapshot| snapshot.as_str())
        .is_none_or(|snapshot| snapshot.is_empty());
    if needs_inject {
        object.insert(
            "target_snapshot".to_owned(),
            serde_json::Value::String(stem.to_owned()),
        );
    }
    serde_json::from_value::<PatchPlan>(serde_json::Value::Object(object)).ok()
}

/// Strip one fenced code block (``` or ```json … ```), returning the
/// interior. Returns `None` when the text is not fenced.
fn strip_code_fence(text: &str) -> Option<&str> {
    let body = text.strip_prefix("```")?;
    let after_open = body.find('\n').map_or(body, |index| &body[index + 1..]);
    let inner = after_open.strip_suffix("```").unwrap_or(after_open);
    Some(inner.trim())
}

/// Slice from the first `open` to the last `close` (inclusive). Returns
/// `None` when either delimiter is absent or the order is inverted.
fn slice_between(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    if end < start {
        return None;
    }
    text.get(start..=end)
}

/// Deterministic snapshot stem for an intent (`intent-<16-hex-fnv1a>`), so
/// the same intent always solves against the same stem label.
fn intent_stem(intent_text: &str) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in intent_text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("intent-{hash:016x}")
}

// ---------------------------------------------------------------------------
// Tests (synthetic values only; no process env, no user dir, no real network
// or keys: live calls run against 127.0.0.1 stubs or the mock transport)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use synthlm_common::consent::{CONFIG_VERSION, ConsentStore};

    use crate::model_gw::{HttpsTransport, MockOutcome, MockTransport, TransportKind};
    use crate::patch::validate;

    /// Synthetic credential for stub tests (never a real key).
    const STUB_KEY: &str = "synthetic-test-key-503";

    /// Stored-consent state for `tier` (synthetic timestamp; never persisted).
    fn decided(tier: ConsentTier) -> ConsentState {
        ConsentState::Decided(ConsentStore {
            tier,
            decided_at_unix: 1_789_000_000,
            version: CONFIG_VERSION,
        })
    }

    fn test_profile() -> Profile {
        synthlm_profile::builtins::load_builtin("reaeq").expect("reaeq builtin loads")
    }

    fn live_backend<'a, T: Transport>(
        gateway: &'a mut Gateway,
        transport: &'a mut T,
        consent: &'a ConsentState,
        key_present: bool,
    ) -> ModelBackend<'a, T> {
        ModelBackend::LiveTier2 {
            gateway,
            transport,
            consent,
            fields: vec!["prompt".to_owned()],
            byte_count: 64,
            cloud_key_present: key_present,
            now_ms: 1_789_000_000_000,
        }
    }

    /// Patch JSON needing repair: one numeric-string coercion plus one
    /// out-of-range clamp (both fixable in a single round), empty snapshot
    /// (stem-injected), wrapped in a prose + fenced envelope.
    fn stub_patch_content() -> String {
        let doc = serde_json::json!({
            "ops": [
                {"op": "replace", "path": "param/4:_Gain_Band_2", "value": 0.6},
                {"op": "replace", "path": "param/7:_Gain_Band_3", "value": "0.4"},
                {"op": "replace", "path": "param/17:wet", "value": 1.5},
            ],
            "target_snapshot": "",
        });
        let raw = serde_json::to_string(&doc).expect("stub doc serializes");
        format!(" patch suggestion below:\n```json\n{raw}\n```")
    }

    /// Serve exactly one loopback connection with a canned Tier2 chat
    /// envelope carrying `content`, then return the base URL.
    fn serve_chat_once(content: String) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("stub binds loopback");
        let port = listener.local_addr().expect("stub reads its port").port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("stub accepts");
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
            let mut content_length = 0usize;
            for line in head.lines().skip(1) {
                if let Some((name, value)) = line.split_once(':')
                    && name.trim().eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().expect("stub content-length");
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
            let envelope = serde_json::json!({
                "id": "chatcmpl-stub-503",
                "object": "chat.completion",
                "model": "mimo-v2.6-flash",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop",
                }],
            });
            let body_text = serde_json::to_string(&envelope).expect("stub envelope serializes");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_text}",
                body_text.len(),
            );
            let _ = stream.write_all(response.as_bytes());
        });
        format!("http://127.0.0.1:{port}/")
    }

    fn stub_transport(base_url: String) -> HttpsTransport {
        HttpsTransport::new_hermetic(STUB_KEY.to_owned(), base_url).expect("stub transport builds")
    }

    #[test]
    fn mock_backend_is_deterministic_and_watermarked() {
        let profile = test_profile();
        assert!(!ModelBackend::<'_, MockTransport>::MockSeeded.is_live());
        let first = plan_for_intent(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            "brighter highs",
        )
        .expect("mock plans");
        let second = plan_for_intent(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            "brighter highs",
        )
        .expect("mock replays");
        assert_eq!(first.plan, second.plan);
        assert_eq!(first.backend(), ModelBackendKind::MockSeeded);
        assert!(!first.backend().is_live());
        assert_eq!(first.backend_label(), MOCK_BACKEND_LABEL);
        assert!(first.backend_label().contains("mock"), "mock marker kept");
        assert!(
            first.backend_label().contains("seeded-demo"),
            "seeded-demo watermark kept"
        );
        assert_eq!(first.model(), MOCK_SEEDED_MODEL);
        assert_eq!(first.serving_tier(), None);
        assert!(first.audits().is_empty(), "mock places no calls");
        assert_eq!(first.repair_rounds(), 0);
        assert_eq!(first.plan.ops.len(), 3);
        assert!(
            validate(first.plan(), &profile).is_empty(),
            "mock patch legal"
        );
        assert_eq!(first.candidates().len(), 3);
        assert!(!first.direction_gap());
    }

    #[test]
    fn mock_backend_ignores_intent_but_stems_deterministically() {
        let profile = test_profile();
        let left = plan_for_intent(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            "intent-a",
        )
        .expect("mock plans");
        let right = plan_for_intent(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            "intent-b",
        )
        .expect("mock plans");
        assert_eq!(left.plan.ops, right.plan.ops, "same skeleton");
        assert_ne!(
            left.plan.target_snapshot, right.plan.target_snapshot,
            "stems differ per intent"
        );
    }

    #[test]
    fn live_backend_reports_is_live_before_any_call() {
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let backend = live_backend(&mut gateway, &mut transport, &consent, true);
        assert!(backend.is_live());
        assert_eq!(backend.label(), LIVE_BACKEND_LABEL);
    }

    #[test]
    fn live_stub_twice_yields_equal_legal_patches_with_repair_counts() {
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();

        let mut first_transport = stub_transport(serve_chat_once(stub_patch_content()));
        let first = plan_for_intent(
            live_backend(&mut gateway, &mut first_transport, &consent, true),
            &profile,
            "brighter highs",
        )
        .expect("stub plans");
        let mut second_transport = stub_transport(serve_chat_once(stub_patch_content()));
        let second = plan_for_intent(
            live_backend(&mut gateway, &mut second_transport, &consent, true),
            &profile,
            "brighter highs",
        )
        .expect("stub replays");

        assert_eq!(first.plan, second.plan, "same intent replays");
        assert_eq!(first.backend(), ModelBackendKind::LiveTier2);
        assert!(first.backend().is_live());
        assert_eq!(first.backend_label(), LIVE_BACKEND_LABEL);
        assert_eq!(first.serving_tier(), Some(ConsentTier::Tier2));
        assert!(!first.model().is_empty());
        assert!(
            validate(first.plan(), &profile).is_empty(),
            "live patch legal"
        );
        assert_eq!(first.plan.ops.len(), 3);
        assert_eq!(first.plan.ops[1].value, serde_json::json!(0.4));
        assert_eq!(first.plan.ops[2].value, serde_json::json!(1.0));
        assert_eq!(first.repair_rounds(), 1);
        assert_eq!(first.removed_total(), 0);
        assert_eq!(first.replaced_total(), 2);
        assert!(!first.candidates().is_empty());
        assert!(first.direction_gap(), "single-plan pool gaps honestly");

        assert!(!first.audits().is_empty(), "live call audited");
        for event in first.audits() {
            let value = serde_json::to_value(event).expect("audit serializes");
            let mut keys: Vec<&str> = value
                .as_object()
                .expect("audit object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec!["byte_count", "fields", "model", "tier", "ts_unix_ms"],
                "audit carries exactly five fields"
            );
            assert_eq!(
                event.fields,
                vec!["prompt".to_owned()],
                "whitelisted fields only"
            );
        }
    }

    #[test]
    fn live_mock_text_yields_legal_patch_without_network() {
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::new(vec![MockOutcome::SucceedWithText {
            latency_ms: 3,
            text: stub_patch_content(),
        }]);
        let outcome = plan_for_intent(
            live_backend(&mut gateway, &mut transport, &consent, true),
            &profile,
            "brighter highs",
        )
        .expect("mock text plans");
        assert_eq!(outcome.backend_label(), LIVE_BACKEND_LABEL);
        assert!(validate(outcome.plan(), &profile).is_empty());
        assert_eq!(outcome.plan.ops.len(), 3);
        assert_eq!(outcome.repair_rounds(), 1);
        assert_eq!(transport.calls().len(), 1);
    }

    #[test]
    fn live_blocked_without_consent_key_or_whitelist_with_zero_calls() {
        let profile = test_profile();

        // No consent: BLOCKED before any transport use.
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let err = plan_for_intent(
            live_backend(&mut gateway, &mut transport, &ConsentState::Undecided, true),
            &profile,
            "brighter highs",
        )
        .expect_err("undecided consent BLOCKEDs");
        assert_eq!(err, PlanningError::Gateway(GatewayError::ConsentRequired));
        assert!(err.clone().blocked());
        assert!(!err.clone().retryable());
        assert!(transport.calls().is_empty());
        assert!(!format!("{err}").to_lowercase().contains("live"));
        assert!(!err.guidance().is_empty());

        // No key on a cloud chain: BLOCKED before any transport use.
        let consent = decided(ConsentTier::Tier2);
        let mut keyless_transport = MockTransport::all_ok();
        let err = plan_for_intent(
            live_backend(&mut gateway, &mut keyless_transport, &consent, false),
            &profile,
            "brighter highs",
        )
        .expect_err("missing key BLOCKEDs");
        assert_eq!(err, PlanningError::Gateway(GatewayError::MissingKey));
        assert!(err.clone().blocked());
        assert!(keyless_transport.calls().is_empty());
        assert!(!format!("{err}").to_lowercase().contains("live"));

        // Whitelist violation: BLOCKED before any transport use.
        let mut wild_transport = MockTransport::all_ok();
        let err = plan_for_intent(
            ModelBackend::LiveTier2 {
                gateway: &mut gateway,
                transport: &mut wild_transport,
                consent: &consent,
                fields: vec!["pcm".to_owned()],
                byte_count: 64,
                cloud_key_present: true,
                now_ms: 1_789_000_000_000,
            },
            &profile,
            "brighter highs",
        )
        .expect_err("off-whitelist BLOCKEDs");
        assert!(matches!(
            err,
            PlanningError::Gateway(GatewayError::WhitelistViolation { .. })
        ));
        assert!(err.clone().blocked());
        assert!(wild_transport.calls().is_empty());
    }

    #[test]
    fn live_fallback_to_local_is_never_labeled_live() {
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::new(vec![
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
        ]);
        let err = plan_for_intent(
            live_backend(&mut gateway, &mut transport, &consent, true),
            &profile,
            "brighter highs",
        )
        .expect_err("Tier3 fallback must not read as a model result");
        assert_eq!(err, PlanningError::NoLiveModel);
        assert_eq!(err.clone().code(), ErrorCode::CloudUnavailable);
        assert!(!format!("{err}").contains(LIVE_BACKEND_LABEL));
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
    fn live_unparsable_text_is_blocked_not_live() {
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::new(vec![MockOutcome::SucceedWithText {
            latency_ms: 1,
            text: "no json here, just prose".to_owned(),
        }]);
        let err = plan_for_intent(
            live_backend(&mut gateway, &mut transport, &consent, true),
            &profile,
            "brighter highs",
        )
        .expect_err("prose BLOCKEDs");
        assert_eq!(err, PlanningError::Unparsable);
        assert!(err.clone().blocked());
        assert!(!format!("{err}").contains(LIVE_BACKEND_LABEL));
    }

    #[test]
    fn live_unrepairable_residuals_are_blocked_not_live() {
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let doc = serde_json::json!({
            "ops": [
                {"op": "replace", "path": "param/99:nope", "value": 0.5},
                {"op": "replace", "path": "param/also-nope", "value": true},
            ],
            "target_snapshot": "snap-x",
        });
        let mut transport = MockTransport::new(vec![MockOutcome::SucceedWithText {
            latency_ms: 1,
            text: serde_json::to_string(&doc).expect("doc serializes"),
        }]);
        let err = plan_for_intent(
            live_backend(&mut gateway, &mut transport, &consent, true),
            &profile,
            "brighter highs",
        )
        .expect_err("residuals BLOCKED");
        assert!(matches!(err, PlanningError::Unrepairable { .. }));
        assert!(err.clone().blocked());
        assert!(!format!("{err}").contains(LIVE_BACKEND_LABEL));
    }

    #[test]
    fn all_planning_errors_expose_blocked_guidance() {
        let gateway_err = PlanningError::Gateway(GatewayError::ConsentRequired);
        assert!(gateway_err.clone().blocked());
        assert!(!gateway_err.guidance().is_empty());
        for err in [
            PlanningError::NoLiveModel,
            PlanningError::Unparsable,
            PlanningError::Unrepairable {
                code: ErrorCode::WhitelistViolation,
            },
            PlanningError::EmptyPlan,
            PlanningError::ChainFailed,
        ] {
            assert_eq!(err.clone().blocked(), !err.clone().code().retryable());
            assert!(!format!("{err}").contains("OPENCODE"), "no key material");
            assert!(!err.guidance().is_empty());
        }
    }

    #[test]
    fn lenient_extraction_covers_fences_slices_and_envelopes() {
        let doc = r#"{"ops":[],"target_snapshot":"s"}"#;
        assert_eq!(extract_plan_docs(doc).len(), 1);
        assert_eq!(extract_plan_docs(&format!("```json\n{doc}\n```")).len(), 1);
        assert_eq!(
            extract_plan_docs(&format!("here you go {doc} bye")).len(),
            1
        );
        assert!(extract_plan_docs("just prose").is_empty());
        assert!(extract_plan_docs("   ").is_empty());
        let arr = format!("[{doc},{doc}]");
        assert_eq!(extract_plan_docs(&arr).len(), 2);
        let env = format!(r#"{{"patches":[{doc}]}}"#);
        assert_eq!(extract_plan_docs(&env).len(), 1);
        assert_eq!(strip_code_fence("```\n{}\n```"), Some("{}"));
        assert_eq!(strip_code_fence("{}"), None);
        assert_eq!(slice_between("a{b}c", '{', '}'), Some("{b}"));
        assert_eq!(slice_between("abc", '{', '}'), None);
        assert!(intent_stem("x").starts_with("intent-"));
        assert_eq!(intent_stem("x"), intent_stem("x"));
    }
}
