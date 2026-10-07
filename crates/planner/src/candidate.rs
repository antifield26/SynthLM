//! Candidate diversity filtering and fixed six-field cards (TSK-304).
//!
//! Planner slice of DEC-018 (keep 3-5 candidates, deduplicate by
//! parameter/CLAP distance, force direction coverage over darker, transient,
//! and spatial, one Chinese sentence of difference per candidate) and DEC-019
//! (cards carry exactly six fields), matching the Candidate shape in
//! ARCHITECTURE section 6 and the L6 difference-summary rule.
//!
//! Wiring point: this module is intentionally NOT referenced from the crate
//! root yet. The main session connects it by adding one line,
//! `pub mod candidate;`, next to the existing `pub mod patch;` declaration in
//! `crates/planner/src/lib.rs`; do not edit `lib.rs` or `Cargo.toml` from
//! here. Until that lands, the TSK-304 integration test compiles this file
//! directly (see `crates/planner/tests/candidate_diversity.rs`), and that
//! test must switch to `synthlm_planner::candidate` imports once the root
//! declaration lands.
//!
//! The main (non-test) code below performs pure computation only: no I/O, no
//! clock, no panicking accessors, and no unsafe blocks. Never call it from an
//! audio thread anyway (AGENTS.md red line 2); this is a control-plane helper.
//!
//! TODO(TSK-304/CLAP): [`crate::candidate::param_distance`] is a parameter-space
//! Euclidean proxy. Replace it with CLAP512 cosine distance once the
//! retrieval/eval embedding reaches the planner input shape, and recalibrate
//! [`crate::candidate::DEFAULT_DEDUP_DISTANCE`] there (cosine and Euclidean
//! radii are not interchangeable).
//!
//! Preview reference format (TSK-606, DEC-019): [`crate::candidate::CandidateCard`]
//! carries `audio_ref` as a canonical derived file name,
//! `preview-<stem>.wav`, where `<stem>` is the candidate id sanitised to
//! `[0-9A-Za-z_-]` (see [`crate::candidate::audio_ref_for`]). The card holds
//! only the name; the WAV bytes behind it are content-addressed by the
//! renderer (hex fingerprint recorded in provenance/audit, never a card
//! field — DEC-019 fixes the card at six fields). Consumers such as the UI
//! resolve the name read-only via [`crate::candidate::CandidateCard::audio_ref`];
//! there is no setter and no path input, so a raw path can never become a
//! reference.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::patch::PatchPlan;

// ---------------------------------------------------------------------------
// Shortlist bounds (DEC-018: default 3, configurable up to 5)
// ---------------------------------------------------------------------------

/// Minimum shortlist size after diversification (DEC-018 default).
pub const MIN_CANDIDATES: usize = 3;

/// Maximum shortlist size after diversification (DEC-018 configurable cap).
pub const MAX_CANDIDATES: usize = 5;

/// Default dedup radius in parameter space for
/// [`crate::candidate::param_distance`].
///
/// Calibrated so near-duplicate fixtures (per-lane drift around 1e-3 over a
/// handful of normalized lanes) collapse while clearly separated candidates
/// (per-lane gaps around 1.0) survive. Recalibrate together with the
/// TODO(TSK-304/CLAP) replacement.
pub const DEFAULT_DEDUP_DISTANCE: f64 = 0.2;

// ---------------------------------------------------------------------------
// Errors (value-free, mirroring the patch policy)
// ---------------------------------------------------------------------------

/// Validation failure for [`crate::candidate::Candidate`],
/// [`crate::candidate::CandidateCard`],
/// [`crate::candidate::ScoreSnapshot`], and
/// [`crate::candidate::PoolEntry`] constructors.
///
/// Variants carry no caller material: formatting an error never echoes ids,
/// summaries, audio references, or tokens, mirroring the value-free policy of
/// [`crate::patch::PatchError`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CandidateError {
    /// Candidate id is empty or whitespace-only.
    #[error("candidate id must be non-empty")]
    EmptyId,
    /// Difference summary is empty or whitespace-only.
    #[error("difference summary must be a non-empty sentence")]
    EmptyDiff,
    /// Confidence is NaN or infinite (checked before the range).
    #[error("confidence must be finite")]
    NonFiniteConfidence,
    /// Finite confidence outside 0.0..=1.0.
    #[error("confidence must lie in 0.0..=1.0")]
    ConfidenceOutOfRange,
    /// `delta_lufs` is NaN or infinite.
    #[error("delta_lufs must be finite")]
    NonFiniteDeltaLufs,
    /// Score snapshot total is NaN or infinite.
    #[error("score total must be finite")]
    NonFiniteScore,
    /// Preview audio reference is empty or whitespace-only.
    #[error("audio reference must be non-empty")]
    EmptyAudioRef,
    /// Preview audio reference is not canonical: it must look like
    /// `preview-<id>.wav` with a non-empty `[0-9A-Za-z_-]` stem (see
    /// [`crate::candidate::audio_ref_for`]).
    #[error("audio reference must be preview-<id>.wav with a [0-9A-Za-z_-] stem")]
    InvalidAudioRef,
    /// Apply token is empty or whitespace-only.
    #[error("apply token must be non-empty")]
    EmptyApplyToken,
    /// Parameter vector is empty (distance would be undefined).
    #[error("parameter vector must be non-empty")]
    EmptyParamVector,
    /// Parameter vector holds NaN or infinite entries.
    #[error("parameter vector must hold only finite values")]
    NonFiniteParam,
}

// ---------------------------------------------------------------------------
// Score snapshot (planner-local so the DEC-022 edge planner->eval never forms)
// ---------------------------------------------------------------------------

/// Frozen evaluation score carried by a candidate (ARCHITECTURE section 6
/// score snapshot).
///
/// Today this is a single total so the planner stays decoupled from the eval
/// crate; it grows into the full multi-objective snapshot (spectral, mel,
/// CLAP, transient) once the eval wiring lands, without renaming any
/// [`crate::candidate::Candidate`] field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreSnapshot {
    total: f64,
}

impl ScoreSnapshot {
    /// Freeze a total score.
    ///
    /// # Errors
    ///
    /// Returns [`crate::candidate::CandidateError::NonFiniteScore`] for NaN or
    /// infinite input.
    pub fn new(total: f64) -> Result<Self, CandidateError> {
        if !total.is_finite() {
            return Err(CandidateError::NonFiniteScore);
        }
        Ok(Self { total })
    }

    /// The frozen total; higher ranks first in
    /// [`crate::candidate::diversify`].
    #[must_use]
    pub fn total(self) -> f64 {
        self.total
    }
}

// ---------------------------------------------------------------------------
// Candidate
// ---------------------------------------------------------------------------

/// One ranked hypothesis: patch plus presentation metadata (DEC-018,
/// ARCHITECTURE section 6, L6).
///
/// Fields are private so the confidence range and the non-empty difference
/// sentence cannot be bypassed with a struct literal: outside this module
/// every instance comes from [`crate::candidate::Candidate::new`]. A missing
/// field is a compile-time error because no field is optional and no builder
/// supplies defaults.
///
/// The Chinese single-sentence shape of `diff_summary_zh` is a model-prompt
/// contract, not a machine check: without new dependencies there is no script
/// table to verify CJK ranges, so construction only rejects empty or
/// whitespace-only text and the integration test pins representative Chinese
/// fixtures by convention.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    id: String,
    patch_summary: PatchPlan,
    diff_summary_zh: String,
    confidence: f64,
    delta_lufs: f64,
    changed_params: u32,
    score_snapshot: ScoreSnapshot,
}

impl Candidate {
    /// Build a candidate.
    ///
    /// Pass [`crate::candidate::count_changed_ops`] of `patch_summary` as
    /// `changed_params` so the card count stays consistent with the surviving
    /// ops; it is kept as an explicit argument (rather than derived) so
    /// archived JSON stays self-describing.
    ///
    /// # Errors
    ///
    /// Returns [`crate::candidate::CandidateError`] when the id or the
    /// difference sentence is empty, when `confidence` is non-finite or
    /// outside 0.0..=1.0, or when `delta_lufs` is non-finite.
    pub fn new(
        id: String,
        patch_summary: PatchPlan,
        diff_summary_zh: String,
        confidence: f64,
        delta_lufs: f64,
        changed_params: u32,
        score_snapshot: ScoreSnapshot,
    ) -> Result<Self, CandidateError> {
        if id.trim().is_empty() {
            return Err(CandidateError::EmptyId);
        }
        if diff_summary_zh.trim().is_empty() {
            return Err(CandidateError::EmptyDiff);
        }
        if !confidence.is_finite() {
            return Err(CandidateError::NonFiniteConfidence);
        }
        if !(0.0..=1.0).contains(&confidence) {
            return Err(CandidateError::ConfidenceOutOfRange);
        }
        if !delta_lufs.is_finite() {
            return Err(CandidateError::NonFiniteDeltaLufs);
        }
        Ok(Self {
            id,
            patch_summary,
            diff_summary_zh,
            confidence,
            delta_lufs,
            changed_params,
            score_snapshot,
        })
    }

    /// Stable candidate id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The patch this candidate would apply.
    #[must_use]
    pub fn patch_summary(&self) -> &PatchPlan {
        &self.patch_summary
    }

    /// One Chinese sentence describing the audible difference (L6).
    #[must_use]
    pub fn diff_summary_zh(&self) -> &str {
        &self.diff_summary_zh
    }

    /// Calibrated confidence in 0.0..=1.0 (DEC-021).
    #[must_use]
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Loudness delta in LUFS against the snapshot baseline (DEC-016
    /// reporting; the +6dB anti-cheat gate lives in eval, not here).
    #[must_use]
    pub fn delta_lufs(&self) -> f64 {
        self.delta_lufs
    }

    /// Number of changed parameters shown on the card.
    #[must_use]
    pub fn changed_params(&self) -> u32 {
        self.changed_params
    }

    /// Frozen evaluation score used for ranking.
    #[must_use]
    pub fn score_snapshot(&self) -> ScoreSnapshot {
        self.score_snapshot
    }
}

/// Count surviving ops as the card's changed-parameter number.
///
/// Saturates at [`u32::MAX`] for absurdly large plans instead of wrapping.
#[must_use]
pub fn count_changed_ops(patch: &PatchPlan) -> u32 {
    u32::try_from(patch.ops.len()).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// Direction (forced coverage set, DEC-018)
// ---------------------------------------------------------------------------

/// Forced-coverage timbre direction (DEC-018: darker, transient, spatial).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Darker / less bright.
    Darker,
    /// Transient shaping.
    Transient,
    /// Spatial / width-depth placement.
    Spatial,
}

impl Direction {
    /// All three directions in canonical order.
    #[must_use]
    pub fn all() -> [Direction; 3] {
        [Direction::Darker, Direction::Transient, Direction::Spatial]
    }

    /// Chinese label used in UI copy and audit strings.
    #[must_use]
    pub fn as_zh(self) -> &'static str {
        match self {
            Direction::Darker => "更暗",
            Direction::Transient => "瞬态",
            Direction::Spatial => "空间",
        }
    }
}

// ---------------------------------------------------------------------------
// CandidateCard (fixed six fields, DEC-019)
// ---------------------------------------------------------------------------

/// File-name prefix of every canonical preview reference (TSK-606).
///
/// A canonical reference is exactly
/// `AUDIO_REF_PREFIX + <stem> + AUDIO_REF_SUFFIX` with a non-empty
/// `[0-9A-Za-z_-]` stem; see [`crate::candidate::audio_ref_for`].
pub const AUDIO_REF_PREFIX: &str = "preview-";

/// File-name suffix of every canonical preview reference (TSK-606).
///
/// Previews are always WAV (the DEC-005 render line writes WAV); no other
/// container is accepted by [`crate::candidate::CandidateCard::new`].
pub const AUDIO_REF_SUFFIX: &str = ".wav";

/// Derives the canonical preview reference for a candidate id (TSK-606).
///
/// Deterministic: the same id always yields the same name (no timestamps, no
/// paths, no randomness), so identical re-renders hit the same derived file.
/// The id is trimmed, then every character outside `[0-9A-Za-z_-]` becomes
/// `_`, so separators (`/`, `\`), parent escapes (`..`), whitespace, and
/// non-ASCII can never reach the file name. The stem is never empty for a
/// non-blank id because the mapping is total per character.
///
/// # Errors
///
/// Returns [`crate::candidate::CandidateError::EmptyId`] when `candidate_id`
/// is empty or whitespace-only.
pub fn audio_ref_for(candidate_id: &str) -> Result<String, CandidateError> {
    if candidate_id.trim().is_empty() {
        return Err(CandidateError::EmptyId);
    }
    let stem: String = candidate_id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(format!("{AUDIO_REF_PREFIX}{stem}{AUDIO_REF_SUFFIX}"))
}

/// Reports whether `value` is a canonical preview reference (TSK-606).
///
/// Accepts exactly `preview-<stem>.wav` with a non-empty `[0-9A-Za-z_-]`
/// stem; matching is case-sensitive and the whole string must match (no
/// directories, no query strings, no second suffix).
#[must_use]
pub fn is_canonical_audio_ref(value: &str) -> bool {
    let Some(stem) = value
        .strip_prefix(AUDIO_REF_PREFIX)
        .and_then(|rest| rest.strip_suffix(AUDIO_REF_SUFFIX))
    else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Fixed six-field UI card (DEC-019): difference sentence, confidence,
/// loudness delta, changed-parameter count, preview audio reference, and the
/// apply-rollback token.
///
/// No field is optional, so a card with a missing field cannot be constructed
/// outside this module; use [`crate::candidate::CandidateCard::new`] or
/// [`crate::candidate::CandidateCard::from_candidate`]. Deserialization
/// additionally rejects objects with a missing or extra field (`serde`
/// `deny_unknown_fields` plus required fields), so a card that loses
/// `audio_ref` on the wire fails loudly instead of arriving half-formed.
///
/// `audio_ref` is the canonical derived preview name
/// (`preview-<id>.wav`, see [`crate::candidate::audio_ref_for`]): the card
/// carries the name only. The WAV bytes behind it are content-addressed by
/// the renderer — a lowercase-hex content fingerprint (8..=256 chars, the
/// same bounds convention as the dsp cache digests, cited by value so this
/// crate stays within its DEC-022 edge) is recorded in provenance/audit next
/// to the derived name, and the fingerprint — never a card field — is what
/// verifies the bytes before playback. The name itself carries only the id
/// stem: no PCM, no prompt text, no absolute path (AGENTS.md §8).
///
/// `apply_token` is the opaque token the bridge
/// exchanges to apply and roll back within one undo transaction (DEC-008,
/// DEC-020). Both handles are read-only for consumers such as the UI: the
/// only accessors are [`crate::candidate::CandidateCard::audio_ref`] and
/// [`crate::candidate::CandidateCard::apply_token`], and neither the planner
/// nor any other crate can rewrite a card in place.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateCard {
    diff: String,
    confidence: f64,
    delta_lufs: f64,
    changed: u32,
    audio_ref: String,
    apply_token: String,
}

impl CandidateCard {
    /// Build a card from its six fields.
    ///
    /// `audio_ref` must be canonical (`preview-<id>.wav`, see
    /// [`crate::candidate::audio_ref_for`]); derive it from the candidate id
    /// rather than hand-writing it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::candidate::CandidateError`] when `diff`,
    /// `audio_ref`, or `apply_token` is empty, when `audio_ref` is non-empty
    /// but not canonical ([`crate::candidate::CandidateError::InvalidAudioRef`]),
    /// when `confidence` is non-finite or outside 0.0..=1.0, or when
    /// `delta_lufs` is non-finite.
    pub fn new(
        diff: String,
        confidence: f64,
        delta_lufs: f64,
        changed: u32,
        audio_ref: String,
        apply_token: String,
    ) -> Result<Self, CandidateError> {
        if diff.trim().is_empty() {
            return Err(CandidateError::EmptyDiff);
        }
        if !confidence.is_finite() {
            return Err(CandidateError::NonFiniteConfidence);
        }
        if !(0.0..=1.0).contains(&confidence) {
            return Err(CandidateError::ConfidenceOutOfRange);
        }
        if !delta_lufs.is_finite() {
            return Err(CandidateError::NonFiniteDeltaLufs);
        }
        if audio_ref.trim().is_empty() {
            return Err(CandidateError::EmptyAudioRef);
        }
        if !is_canonical_audio_ref(&audio_ref) {
            return Err(CandidateError::InvalidAudioRef);
        }
        if apply_token.trim().is_empty() {
            return Err(CandidateError::EmptyApplyToken);
        }
        Ok(Self {
            diff,
            confidence,
            delta_lufs,
            changed,
            audio_ref,
            apply_token,
        })
    }

    /// Derive the four evaluation fields from `candidate` and attach the
    /// preview reference plus the apply token.
    ///
    /// Pass [`crate::candidate::audio_ref_for`] of `candidate.id()` as
    /// `audio_ref` so the name always matches the candidate it previews.
    ///
    /// # Errors
    ///
    /// Returns [`crate::candidate::CandidateError::EmptyAudioRef`],
    /// [`crate::candidate::CandidateError::InvalidAudioRef`], or
    /// [`crate::candidate::CandidateError::EmptyApplyToken`] when the
    /// attached handles are empty or off-shape; the four copied fields
    /// already passed [`crate::candidate::Candidate::new`].
    pub fn from_candidate(
        candidate: &Candidate,
        audio_ref: String,
        apply_token: String,
    ) -> Result<Self, CandidateError> {
        Self::new(
            candidate.diff_summary_zh.clone(),
            candidate.confidence,
            candidate.delta_lufs,
            candidate.changed_params,
            audio_ref,
            apply_token,
        )
    }

    /// Difference sentence (mirrors the candidate's Chinese summary).
    #[must_use]
    pub fn diff(&self) -> &str {
        &self.diff
    }

    /// Calibrated confidence in 0.0..=1.0.
    #[must_use]
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Loudness delta in LUFS.
    #[must_use]
    pub fn delta_lufs(&self) -> f64 {
        self.delta_lufs
    }

    /// Changed-parameter count.
    #[must_use]
    pub fn changed(&self) -> u32 {
        self.changed
    }

    /// Canonical preview audio reference (`preview-<id>.wav`).
    ///
    /// Read-only handle for consumers such as the UI: resolve it against the
    /// preview cache and play; never construct paths from it by hand.
    #[must_use]
    pub fn audio_ref(&self) -> &str {
        &self.audio_ref
    }

    /// Opaque apply-rollback token for the bridge undo transaction.
    #[must_use]
    pub fn apply_token(&self) -> &str {
        &self.apply_token
    }
}

// ---------------------------------------------------------------------------
// Diversification input and outcome
// ---------------------------------------------------------------------------

/// Diversification input: a candidate plus its position in parameter space
/// and its declared timbre direction.
///
/// `params` holds normalized control values today (one lane per whitelisted
/// role parameter, 0.0..=1.0 by the patch value rules); it becomes the
/// embedding lane once TODO(TSK-304/CLAP) lands. Only finiteness is enforced
/// so the future embedding shape fits without changing this constructor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolEntry {
    candidate: Candidate,
    params: Vec<f64>,
    direction: Direction,
}

impl PoolEntry {
    /// Wrap a candidate with its parameter vector and direction.
    ///
    /// # Errors
    ///
    /// Returns [`crate::candidate::CandidateError::EmptyParamVector`] for an
    /// empty vector, or [`crate::candidate::CandidateError::NonFiniteParam`]
    /// when any lane is NaN or infinite.
    pub fn new(
        candidate: Candidate,
        params: Vec<f64>,
        direction: Direction,
    ) -> Result<Self, CandidateError> {
        if params.is_empty() {
            return Err(CandidateError::EmptyParamVector);
        }
        for lane in &params {
            if !lane.is_finite() {
                return Err(CandidateError::NonFiniteParam);
            }
        }
        Ok(Self {
            candidate,
            params,
            direction,
        })
    }

    /// The wrapped candidate.
    #[must_use]
    pub fn candidate(&self) -> &Candidate {
        &self.candidate
    }

    /// Parameter-space position used by
    /// [`crate::candidate::param_distance`].
    #[must_use]
    pub fn params(&self) -> &[f64] {
        &self.params
    }

    /// Declared timbre direction for forced coverage.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Ranking score: the candidate's frozen total (higher ranks first).
    #[must_use]
    pub fn score(&self) -> f64 {
        self.candidate.score_snapshot.total
    }
}

/// Outcome of [`crate::candidate::diversify`]: the surviving shortlist plus
/// the coverage flag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiversifyOutcome {
    candidates: Vec<Candidate>,
    direction_gap: bool,
    missing_directions: Vec<Direction>,
}

impl DiversifyOutcome {
    /// Surviving candidates, score-ordered.
    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// True when the survivors cover fewer than all three directions. The
    /// survivors are still the highest-scoring ones (never padded with
    /// near-duplicates); callers surface the gap instead of guessing.
    #[must_use]
    pub fn direction_gap(&self) -> bool {
        self.direction_gap
    }

    /// Which directions have no representative among the survivors.
    #[must_use]
    pub fn missing_directions(&self) -> &[Direction] {
        &self.missing_directions
    }
}

// ---------------------------------------------------------------------------
// Distance and diversification
// ---------------------------------------------------------------------------

/// Euclidean distance in parameter space between two candidates.
///
/// Vectors of unequal length compare lane by lane with missing lanes read as
/// 0.0, so padded and unpadded encodings stay comparable. See the
/// TODO(TSK-304/CLAP) module note for the planned embedding replacement.
#[must_use]
pub fn param_distance(a: &[f64], b: &[f64]) -> f64 {
    let mut sum = 0.0;
    let mut index = 0;
    let len = a.len().max(b.len());
    while index < len {
        let x = match a.get(index) {
            Some(value) => *value,
            None => 0.0,
        };
        let y = match b.get(index) {
            Some(value) => *value,
            None => 0.0,
        };
        let delta = x - y;
        sum += delta * delta;
        index += 1;
    }
    sum.sqrt()
}

/// Diversify `pool` down to at most `k` candidates with the default radius
/// ([`crate::candidate::DEFAULT_DEDUP_DISTANCE`]).
#[must_use]
pub fn diversify(pool: &[PoolEntry], k: usize) -> DiversifyOutcome {
    diversify_with_threshold(pool, k, DEFAULT_DEDUP_DISTANCE)
}

/// Diversify `pool` down to at most `k` candidates (DEC-018).
///
/// Steps: rank by frozen score total (stable, so input order breaks ties);
/// greedily drop candidates closer than `threshold` to an already kept one
/// ([`crate::candidate::param_distance`]); keep one representative per
/// direction first, then fill by score up to the target; finally re-sort the
/// survivors by score. `k` is clamped into
/// `MIN_CANDIDATES..=MAX_CANDIDATES`, and a non-finite or negative threshold
/// falls back to [`crate::candidate::DEFAULT_DEDUP_DISTANCE`]. Survivors may
/// still be fewer than the target when deduplication collapses
/// near-duplicates.
///
/// `direction_gap` is set with the absent directions listed in
/// `missing_directions` whenever the survivors cover fewer than all three
/// directions, whether the pool never held them or deduplication removed the
/// last representative.
#[must_use]
pub fn diversify_with_threshold(pool: &[PoolEntry], k: usize, threshold: f64) -> DiversifyOutcome {
    let target = k.clamp(MIN_CANDIDATES, MAX_CANDIDATES);
    let radius = if threshold.is_finite() && threshold >= 0.0 {
        threshold
    } else {
        DEFAULT_DEDUP_DISTANCE
    };
    let mut order: Vec<usize> = (0..pool.len()).collect();
    order.sort_by(|a, b| pool[*b].score().total_cmp(&pool[*a].score()));
    let mut kept: Vec<usize> = Vec::new();
    for index in order {
        let mut too_close = false;
        for kept_index in &kept {
            if param_distance(pool[index].params(), pool[*kept_index].params()) < radius {
                too_close = true;
                break;
            }
        }
        if !too_close {
            kept.push(index);
        }
    }
    let mut picked: Vec<usize> = Vec::new();
    for direction in Direction::all() {
        if picked.len() >= target {
            break;
        }
        for index in &kept {
            if pool[*index].direction() == direction {
                picked.push(*index);
                break;
            }
        }
    }
    for index in &kept {
        if picked.len() >= target {
            break;
        }
        if !picked.contains(index) {
            picked.push(*index);
        }
    }
    picked.sort_by(|a, b| pool[*b].score().total_cmp(&pool[*a].score()));
    let mut missing_directions = Vec::new();
    for direction in Direction::all() {
        let mut covered = false;
        for index in &picked {
            if pool[*index].direction() == direction {
                covered = true;
                break;
            }
        }
        if !covered {
            missing_directions.push(direction);
        }
    }
    let direction_gap = !missing_directions.is_empty();
    let mut candidates = Vec::with_capacity(picked.len());
    for index in picked {
        candidates.push(pool[index].candidate().clone());
    }
    DiversifyOutcome {
        candidates,
        direction_gap,
        missing_directions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::PatchPlan;

    // NOTE: this module is also compiled by
    // `crates/planner/tests/candidate_diversity.rs` through a `#[path]`
    // include whose `patch` shim re-exports only `PatchPlan`, so the helpers
    // below build empty-op plans and never touch other `patch` items.
    fn patch_empty() -> PatchPlan {
        PatchPlan {
            ops: Vec::new(),
            target_snapshot: "snap-t606".to_owned(),
        }
    }

    fn candidate_with(id: &str) -> Candidate {
        let patch = patch_empty();
        let changed = count_changed_ops(&patch);
        let score = ScoreSnapshot::new(0.6).expect("fixture score must be finite");
        Candidate::new(
            id.to_owned(),
            patch,
            "低频收紧，整体更暗。".to_owned(),
            0.8,
            -1.2,
            changed,
            score,
        )
        .expect("fixture candidate must validate")
    }

    #[test]
    fn audio_ref_for_derives_preview_name() {
        assert_eq!(
            audio_ref_for("dup-low").expect("plain id must derive"),
            "preview-dup-low.wav"
        );
        // Surrounding whitespace is trimmed before derivation.
        assert_eq!(
            audio_ref_for("  spaced  ").expect("padded id must derive"),
            "preview-spaced.wav"
        );
        // Separators, dots, and non-ASCII become `_`, so no path escapes.
        assert_eq!(
            audio_ref_for("a/b\\c d.e候选").expect("hostile id must derive"),
            "preview-a_b_c_d_e__.wav"
        );
        // Derivation is total over non-blank ids: same id, same name.
        assert_eq!(
            audio_ref_for("demo-dark").expect("id must derive"),
            audio_ref_for("demo-dark").expect("derivation must be deterministic")
        );
    }

    #[test]
    fn audio_ref_for_rejects_blank_id() {
        for blank in ["", "   "] {
            let err = audio_ref_for(blank).expect_err("blank id must fail derivation");
            assert_eq!(err, CandidateError::EmptyId);
        }
    }

    #[test]
    fn canonical_shape_accepts_only_preview_wav() {
        for good in ["preview-a.wav", "preview-dup-low.wav", "preview-007_X.wav"] {
            assert!(is_canonical_audio_ref(good), "{good:?} must be canonical");
        }
        for bad in [
            "",
            "   ",
            "render/cand-07.wav",
            "demo-preview-x.wav",
            "preview-.wav",
            "preview-a.mp3",
            "preview-a/b.wav",
            "preview-a\\b.wav",
            "preview-..wav",
            "preview-候选.wav",
            "PREVIEW-a.wav",
            "preview-a.WAV",
            "preview-a.wav ",
            " preview-a.wav",
            "preview-a.wav.bak",
        ] {
            assert!(!is_canonical_audio_ref(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn card_new_enforces_audio_ref_shape() {
        let good = || {
            CandidateCard::new(
                "低频收紧，整体更暗。".to_owned(),
                0.5,
                0.0,
                1,
                "preview-x.wav".to_owned(),
                "apply-x".to_owned(),
            )
        };
        assert!(good().is_ok());

        // Blank keeps the historical error, not the shape error.
        let err = CandidateCard::new(
            "低频收紧，整体更暗。".to_owned(),
            0.5,
            0.0,
            1,
            "   ".to_owned(),
            "apply-x".to_owned(),
        )
        .expect_err("blank audio_ref must be rejected");
        assert_eq!(err, CandidateError::EmptyAudioRef);

        // Non-empty but off-shape is the new failure mode.
        for bad in [
            "render/cand-07.wav",
            "demo-preview-x.wav",
            "preview-.wav",
            "preview-a.mp3",
        ] {
            let err = CandidateCard::new(
                "低频收紧，整体更暗。".to_owned(),
                0.5,
                0.0,
                1,
                bad.to_owned(),
                "apply-x".to_owned(),
            )
            .expect_err("off-shape audio_ref must be rejected");
            assert_eq!(err, CandidateError::InvalidAudioRef, "{bad:?}");
        }
    }

    #[test]
    fn card_from_candidate_roundtrips_derived_ref() {
        let got = candidate_with("src-01");
        let derived = audio_ref_for(got.id()).expect("id must derive");
        let card = CandidateCard::from_candidate(&got, derived.clone(), "apply-src".to_owned())
            .expect("derivation must succeed");
        assert_eq!(card.audio_ref(), derived);
        assert_eq!(card.diff(), got.diff_summary_zh());

        let value = serde_json::to_value(&card).expect("card must serialize");
        let object = value.as_object().expect("card must serialize to an object");
        assert_eq!(object.len(), 6, "DEC-019 fixes the card at six fields");
        assert_eq!(object["audio_ref"], serde_json::json!(derived));
    }

    #[test]
    fn card_audio_ref_missing_field_fails_deserialization() {
        // A card that loses `audio_ref` on the wire must fail loudly.
        let without_ref = serde_json::json!({
            "diff": "低频收紧，整体更暗。",
            "confidence": 0.5,
            "delta_lufs": 0.0,
            "changed": 1,
            "apply_token": "apply-x"
        });
        let err = serde_json::from_value::<CandidateCard>(without_ref)
            .expect_err("missing audio_ref must fail");
        assert!(
            err.to_string().contains("audio_ref"),
            "error must name the missing field: {err}"
        );

        // An extra field fails too (shape lock, both directions).
        let with_extra = serde_json::json!({
            "diff": "低频收紧，整体更暗。",
            "confidence": 0.5,
            "delta_lufs": 0.0,
            "changed": 1,
            "audio_ref": "preview-x.wav",
            "apply_token": "apply-x",
            "fingerprint": "ab12cd34"
        });
        assert!(
            serde_json::from_value::<CandidateCard>(with_extra).is_err(),
            "seventh field must be rejected: the fingerprint stays out of the card"
        );
    }
}
