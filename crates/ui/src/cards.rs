//! Candidate cards: plan-JSON loading, single-winner selection, and
//! apply/rollback instruction previews (TSK-506).
//!
//! The main window renders one card per plan entry. Each entry must supply
//! the fixed six DEC-019 fields (difference sentence, confidence,
//! loudness delta, changed-parameter count, preview audio reference,
//! apply-rollback token); anything missing or out of range fails validation
//! and surfaces as a red row instead of a card, so a malformed plan can
//! never silently lose a field. The six-field shape mirrors
//! `synthlm_planner::candidate::CandidateCard` (private fields, validated
//! constructor, no optional fields), reimplemented here so this crate stays
//! self-contained: the UI must never link the planner's network transport
//! or its strict patch schema (preview stays lenient; the REAPER side
//! validates before executing).
//!
//! Red lines: this module is pure data (no I/O, no clock, no network, no
//! REAPER calls). [`crate::cards::ApplyPreview`] and
//! [`crate::cards::RollbackPreview`] only *describe* the DEC-008 undo
//! transaction and the parameter table; execution belongs to the REAPER
//! side (DEC-020). Errors are value-free: they name the bad field, never
//! the caller content.
//!
//! JSON shape: the same candidate entries as
//! `experiments/e2e-demo/demo-plan.json` (`id`, `diff_summary_zh`,
//! `confidence`, `delta_lufs`, `changed_params`, `audio_ref`,
//! `apply_token`, plus a `patch` object with `ops` and `target_snapshot`).
//! Extra fields (direction, rank, scores, audit entries) are ignored so
//! previews survive schema additions; required fields are all
//! non-optional, so a missing one rejects the entry.

use std::fmt::{Display, Formatter};

use serde_json::{Map, Value};

/// Single undo-transaction marker named by the UI preview (DEC-008).
const UNDO_BEGIN: &str = "Undo_BeginBlock2";

/// Matching close marker for [`crate::cards::UNDO_BEGIN`].
const UNDO_END: &str = "Undo_EndBlock2";

/// Provenance marker: applying must derive new files, never overwrite the
/// user project in place (AGENTS.md red line 5).
const PROVENANCE_MARK: &str = "P_EXT:SYNTHLM_*";

// ---------------------------------------------------------------------------
// Errors (value-free: variants name the field, never the content)
// ---------------------------------------------------------------------------

/// Validation or plan-shape failure for [`crate::cards::Card`],
/// [`crate::cards::ListedCandidate`], and
/// [`crate::cards::parse_plan_text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardsError {
    /// Candidate id missing, mistyped, or blank.
    EmptyId,
    /// Difference sentence missing, mistyped, or blank.
    EmptyDiff,
    /// Confidence missing, mistyped, or non-finite.
    NonFiniteConfidence,
    /// Finite confidence outside 0.0..=1.0.
    ConfidenceOutOfRange,
    /// Loudness delta missing, mistyped, or non-finite.
    NonFiniteDeltaLufs,
    /// Changed-parameter count missing, negative, fractional, or
    /// unrepresentable as `u32`.
    EmptyChanged,
    /// Preview audio reference missing, mistyped, or blank.
    EmptyAudioRef,
    /// Apply-rollback token missing, mistyped, or blank.
    EmptyApplyToken,
    /// A patch op name missing, mistyped, or blank.
    EmptyOp,
    /// A patch parameter path missing, mistyped, or blank.
    EmptyPath,
    /// A patch parameter value missing, mistyped, or non-finite.
    NonFiniteParamValue,
    /// Snapshot id missing, mistyped, or blank.
    EmptySnapshot,
    /// `patch` object (or its `ops` list) missing or misshaped.
    MissingPatch,
    /// Whole text is not JSON; carries the parser position only.
    InvalidJson {
        /// 1-based line from `serde_json`.
        line: usize,
        /// 1-based column from `serde_json`.
        column: usize,
    },
    /// Valid JSON but not a plan (top level not an object, or the
    /// candidate list not an array).
    NotAPlan,
}

impl Display for CardsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyId => write!(f, "候选缺少编号"),
            Self::EmptyDiff => write!(f, "差异句为空"),
            Self::NonFiniteConfidence => write!(f, "置信度不是数字"),
            Self::ConfidenceOutOfRange => write!(f, "置信度越界（需 0-1）"),
            Self::NonFiniteDeltaLufs => write!(f, "ΔLUFS 不是数字"),
            Self::EmptyChanged => write!(f, "改动参数数缺失"),
            Self::EmptyAudioRef => write!(f, "试听引用为空"),
            Self::EmptyApplyToken => write!(f, "应用令牌为空"),
            Self::EmptyOp => write!(f, "参数操作为空"),
            Self::EmptyPath => write!(f, "参数路径为空"),
            Self::NonFiniteParamValue => write!(f, "参数值不是数字"),
            Self::EmptySnapshot => write!(f, "快照标识为空"),
            Self::MissingPatch => write!(f, "缺少参数表"),
            Self::InvalidJson { line, column } => {
                write!(f, "计划不是合法 JSON（行 {line} 列 {column}）")
            }
            Self::NotAPlan => write!(f, "计划结构错误"),
        }
    }
}

impl std::error::Error for CardsError {}

// ---------------------------------------------------------------------------
// Card (fixed six fields, DEC-019)
// ---------------------------------------------------------------------------

/// Fixed six-field UI card (DEC-019): difference sentence, confidence,
/// loudness delta, changed-parameter count, preview audio reference, and
/// the apply-rollback token.
///
/// Fields are private so validation cannot be bypassed with a struct
/// literal: every instance comes from [`crate::cards::Card::new`]. A
/// missing field is a compile-time error because no field is optional and
/// no builder supplies defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    diff: String,
    confidence: f64,
    delta_lufs: f64,
    changed: u32,
    audio_ref: String,
    apply_token: String,
}

impl Card {
    /// Build a card from its six fields.
    ///
    /// # Errors
    ///
    /// Returns [`crate::cards::CardsError`] when `diff`, `audio_ref`, or
    /// `apply_token` is blank, when `confidence` is non-finite or outside
    /// 0.0..=1.0, or when `delta_lufs` is non-finite.
    pub fn new(
        diff: String,
        confidence: f64,
        delta_lufs: f64,
        changed: u32,
        audio_ref: String,
        apply_token: String,
    ) -> Result<Self, CardsError> {
        if diff.trim().is_empty() {
            return Err(CardsError::EmptyDiff);
        }
        if !confidence.is_finite() {
            return Err(CardsError::NonFiniteConfidence);
        }
        if !(0.0..=1.0).contains(&confidence) {
            return Err(CardsError::ConfidenceOutOfRange);
        }
        if !delta_lufs.is_finite() {
            return Err(CardsError::NonFiniteDeltaLufs);
        }
        if audio_ref.trim().is_empty() {
            return Err(CardsError::EmptyAudioRef);
        }
        if apply_token.trim().is_empty() {
            return Err(CardsError::EmptyApplyToken);
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

    /// Difference sentence (one Chinese sentence, L6).
    #[must_use]
    pub fn diff(&self) -> &str {
        &self.diff
    }

    /// Calibrated confidence in 0.0..=1.0.
    #[must_use]
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Loudness delta in LUFS against the snapshot baseline.
    #[must_use]
    pub fn delta_lufs(&self) -> f64 {
        self.delta_lufs
    }

    /// Changed-parameter count shown on the card.
    #[must_use]
    pub fn changed(&self) -> u32 {
        self.changed
    }

    /// Opaque preview audio reference (listed only; playback is a
    /// deliberately unimplemented stub, see the audition note in
    /// [`crate::app::SynthApp`]).
    #[must_use]
    pub fn audio_ref(&self) -> &str {
        &self.audio_ref
    }

    /// Opaque apply-rollback token exchanged with the REAPER side.
    #[must_use]
    pub fn apply_token(&self) -> &str {
        &self.apply_token
    }
}

// ---------------------------------------------------------------------------
// Patch ops (opaque preview rows; the REAPER side interprets them)
// ---------------------------------------------------------------------------

/// One whitelisted patch op carried for the apply preview table.
///
/// `op`/`path` stay opaque strings on purpose: the UI never interprets
/// semantics (no ident lookup, no FX indexing); the bridge validates
/// before executing.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamOp {
    op: String,
    path: String,
    value: f64,
}

impl ParamOp {
    /// Build one preview row.
    ///
    /// # Errors
    ///
    /// Returns [`crate::cards::CardsError::EmptyOp`],
    /// [`crate::cards::CardsError::EmptyPath`], or
    /// [`crate::cards::CardsError::NonFiniteParamValue`] for blank or
    /// non-finite input.
    pub fn new(op: String, path: String, value: f64) -> Result<Self, CardsError> {
        if op.trim().is_empty() {
            return Err(CardsError::EmptyOp);
        }
        if path.trim().is_empty() {
            return Err(CardsError::EmptyPath);
        }
        if !value.is_finite() {
            return Err(CardsError::NonFiniteParamValue);
        }
        Ok(Self { op, path, value })
    }

    /// Op name (opaque, e.g. `replace`).
    #[must_use]
    pub fn op(&self) -> &str {
        &self.op
    }

    /// Parameter path (opaque display string).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Target value.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }
}

// ---------------------------------------------------------------------------
// Listed candidate (card + identity + patch, one plan entry)
// ---------------------------------------------------------------------------

/// One validated plan entry: stable id, six-field card, patch rows, and
/// the snapshot the patch targets.
#[derive(Clone, Debug, PartialEq)]
pub struct ListedCandidate {
    id: String,
    card: Card,
    ops: Vec<ParamOp>,
    target_snapshot: String,
}

impl ListedCandidate {
    /// Build a listed candidate (all parts already validated).
    ///
    /// # Errors
    ///
    /// Returns [`crate::cards::CardsError::EmptyId`] for a blank id or
    /// [`crate::cards::CardsError::EmptySnapshot`] for a blank snapshot.
    pub fn new(
        id: String,
        card: Card,
        ops: Vec<ParamOp>,
        target_snapshot: String,
    ) -> Result<Self, CardsError> {
        if id.trim().is_empty() {
            return Err(CardsError::EmptyId);
        }
        if target_snapshot.trim().is_empty() {
            return Err(CardsError::EmptySnapshot);
        }
        Ok(Self {
            id,
            card,
            ops,
            target_snapshot,
        })
    }

    /// Build from a raw JSON value (one element of the plan's
    /// `candidates` array).
    ///
    /// # Errors
    ///
    /// Returns [`crate::cards::CardsError`] naming the first bad field;
    /// callers turn this into a red row instead of a card.
    pub fn from_value(item: &Value) -> Result<Self, CardsError> {
        let obj = item.as_object().ok_or(CardsError::NotAPlan)?;
        let id = field_string(obj, "id", CardsError::EmptyId)?;
        let diff = field_string(obj, "diff_summary_zh", CardsError::EmptyDiff)?;
        let confidence = field_f64(obj, "confidence", CardsError::NonFiniteConfidence)?;
        if !(0.0..=1.0).contains(&confidence) {
            return Err(CardsError::ConfidenceOutOfRange);
        }
        let delta_lufs = field_f64(obj, "delta_lufs", CardsError::NonFiniteDeltaLufs)?;
        let changed = field_changed(obj)?;
        let audio_ref = field_string(obj, "audio_ref", CardsError::EmptyAudioRef)?;
        let apply_token = field_string(obj, "apply_token", CardsError::EmptyApplyToken)?;
        let card = Card::new(
            diff,
            confidence,
            delta_lufs,
            changed,
            audio_ref,
            apply_token,
        )?;
        let patch = obj
            .get("patch")
            .and_then(Value::as_object)
            .ok_or(CardsError::MissingPatch)?;
        let target_snapshot = field_string(patch, "target_snapshot", CardsError::EmptySnapshot)?;
        let ops_value = patch
            .get("ops")
            .and_then(Value::as_array)
            .ok_or(CardsError::MissingPatch)?;
        let mut ops = Vec::with_capacity(ops_value.len());
        for raw in ops_value {
            let op_obj = raw.as_object().ok_or(CardsError::MissingPatch)?;
            let op = field_string(op_obj, "op", CardsError::EmptyOp)?;
            let path = field_string(op_obj, "path", CardsError::EmptyPath)?;
            let value = field_f64(op_obj, "value", CardsError::NonFiniteParamValue)?;
            ops.push(ParamOp::new(op, path, value)?);
        }
        Self::new(id, card, ops, target_snapshot)
    }

    /// Stable candidate id (selection key; never a live FX index).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The six-field card.
    #[must_use]
    pub fn card(&self) -> &Card {
        &self.card
    }

    /// Patch rows for the apply preview table.
    #[must_use]
    pub fn ops(&self) -> &[ParamOp] {
        &self.ops
    }

    /// Snapshot the patch targets.
    #[must_use]
    pub fn target_snapshot(&self) -> &str {
        &self.target_snapshot
    }
}

/// Extract a required non-blank string field.
fn field_string(
    obj: &Map<String, Value>,
    key: &str,
    err: CardsError,
) -> Result<String, CardsError> {
    match obj.get(key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        _ => Err(err),
    }
}

/// Extract a required finite number field.
fn field_f64(obj: &Map<String, Value>, key: &str, err: CardsError) -> Result<f64, CardsError> {
    match obj.get(key).and_then(Value::as_f64) {
        Some(value) if value.is_finite() => Ok(value),
        _ => Err(err),
    }
}

/// Extract the required changed-parameter count.
fn field_changed(obj: &Map<String, Value>) -> Result<u32, CardsError> {
    match obj.get("changed_params").and_then(Value::as_u64) {
        Some(count) => u32::try_from(count).map_err(|_| CardsError::EmptyChanged),
        None => Err(CardsError::EmptyChanged),
    }
}

// ---------------------------------------------------------------------------
// Plan loading (whole file + per-entry rejection)
// ---------------------------------------------------------------------------

/// One plan entry that failed validation (rendered as a red row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedEntry {
    index: usize,
    reason: CardsError,
}

impl RejectedEntry {
    /// Position inside the plan's `candidates` array.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Value-free reason for the rejection.
    #[must_use]
    pub fn reason(&self) -> &CardsError {
        &self.reason
    }
}

/// Outcome of [`crate::cards::parse_plan_text`]: renderable candidates
/// plus the rejected entries shown as red rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanReport {
    candidates: Vec<ListedCandidate>,
    rejected: Vec<RejectedEntry>,
}

impl PlanReport {
    /// Validated candidates in plan order.
    #[must_use]
    pub fn candidates(&self) -> &[ListedCandidate] {
        &self.candidates
    }

    /// Entries that failed validation (red rows).
    #[must_use]
    pub fn rejected(&self) -> &[RejectedEntry] {
        &self.rejected
    }

    /// True when nothing renderable and nothing rejected came back.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty() && self.rejected.is_empty()
    }

    /// Number of renderable candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Look up a candidate by its stable id.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&ListedCandidate> {
        self.candidates.iter().find(|entry| entry.id() == id)
    }
}

/// Parse plan JSON text into renderable cards plus red-row rejections.
///
/// Top-level shape failures (not JSON, not an object, `candidates` not an
/// array) return [`crate::cards::CardsError`]; per-entry failures land in
/// [`crate::cards::PlanReport::rejected`] so one bad entry never hides
/// the good ones.
///
/// # Errors
///
/// Returns [`crate::cards::CardsError::InvalidJson`] or
/// [`crate::cards::CardsError::NotAPlan`] for whole-file problems.
pub fn parse_plan_text(text: &str) -> Result<PlanReport, CardsError> {
    let root: Value = serde_json::from_str(text).map_err(|err| CardsError::InvalidJson {
        line: err.line(),
        column: err.column(),
    })?;
    let top = root.as_object().ok_or(CardsError::NotAPlan)?;
    let items = top
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or(CardsError::NotAPlan)?;
    let mut report = PlanReport {
        candidates: Vec::with_capacity(items.len()),
        rejected: Vec::new(),
    };
    for (index, item) in items.iter().enumerate() {
        match ListedCandidate::from_value(item) {
            Ok(entry) => report.candidates.push(entry),
            Err(reason) => report.rejected.push(RejectedEntry { index, reason }),
        }
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Selection (single winner by stable id)
// ---------------------------------------------------------------------------

/// Single-winner selection over the loaded roster.
///
/// Holds the winner's stable id (never a positional index, so plan
/// reorderings cannot silently retarget the apply command).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    winner: Option<String>,
}

impl Selection {
    /// Empty selection (no winner yet).
    #[must_use]
    pub fn new() -> Self {
        Self { winner: None }
    }

    /// Current winner id, if any.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.winner.as_deref()
    }

    /// True when no winner is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.winner.is_none()
    }

    /// Select `id` when it names a roster entry; otherwise refuse and keep
    /// the previous selection (no guessing).
    ///
    /// Returns true on success.
    pub fn select_known(&mut self, id: &str, roster: &[ListedCandidate]) -> bool {
        if roster.iter().any(|entry| entry.id() == id) {
            self.winner = Some(id.to_owned());
            true
        } else {
            false
        }
    }

    /// Clear the winner.
    pub fn clear(&mut self) {
        self.winner = None;
    }
}

// ---------------------------------------------------------------------------
// Apply / rollback instruction previews (DEC-008 + DEC-020, preview only)
// ---------------------------------------------------------------------------

/// Apply granularity (DEC-020): whole-chain snapshot by default; the
/// single-plugin / single-group options reuse the same single undo point
/// once the bridge exposes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApplyGranularity {
    /// Whole-chain snapshot applied as one undo point.
    #[default]
    WholeChain,
}

impl ApplyGranularity {
    /// Chinese label for preview copy.
    #[must_use]
    pub fn as_zh(self) -> &'static str {
        match self {
            Self::WholeChain => "整链快照",
        }
    }
}

/// Apply instruction preview for the winning candidate.
///
/// Data only: the undo-transaction lines, the stable undo description
/// (`SynthLM: apply <id> <N>params`, DEC-008), the provenance note, and
/// the parameter table. Nothing here touches REAPER; the REAPER side owns
/// execution (DEC-020).
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyPreview {
    candidate_id: String,
    apply_token: String,
    target_snapshot: String,
    granularity: ApplyGranularity,
    ops: Vec<ParamOp>,
}

impl ApplyPreview {
    /// Snapshot the winner's instruction fields into a preview.
    #[must_use]
    pub fn for_winner(winner: &ListedCandidate) -> Self {
        Self {
            candidate_id: winner.id().to_owned(),
            apply_token: winner.card().apply_token().to_owned(),
            target_snapshot: winner.target_snapshot().to_owned(),
            granularity: ApplyGranularity::WholeChain,
            ops: winner.ops().to_vec(),
        }
    }

    /// Candidate the preview applies to.
    #[must_use]
    pub fn candidate_id(&self) -> &str {
        &self.candidate_id
    }

    /// Apply granularity (always whole-chain for now).
    #[must_use]
    pub fn granularity(&self) -> ApplyGranularity {
        self.granularity
    }

    /// Stable undo description for the DEC-008 transaction block.
    #[must_use]
    pub fn undo_desc(&self) -> String {
        let count = u32::try_from(self.ops.len()).unwrap_or(u32::MAX);
        format!("SynthLM: apply {} {count}params", self.candidate_id)
    }

    /// Render the preview pane text (Chinese copy, ASCII structure).
    #[must_use]
    pub fn render(&self) -> String {
        let count = u32::try_from(self.ops.len()).unwrap_or(u32::MAX);
        let mut out = String::new();
        out.push_str("应用指令预览（仅预览，不执行）\n");
        out.push_str(&format!("候选 {}\n", self.candidate_id));
        out.push_str(&format!("事务 {UNDO_BEGIN} … {UNDO_END}（单事务）\n",));
        out.push_str(&format!("描述 {}\n", self.undo_desc()));
        out.push_str(&format!("粒度 {}（默认）\n", self.granularity.as_zh()));
        out.push_str(&format!("目标 {}\n", self.target_snapshot));
        out.push_str(&format!("令牌 {}\n", self.apply_token));
        out.push_str(&format!(
            "派生 新文件 + {PROVENANCE_MARK}（不覆盖用户工程）\n"
        ));
        out.push_str(&format!("参数表（{count}）：\n"));
        for op in &self.ops {
            out.push_str(&format!("  {} {} = {}\n", op.op(), op.path(), op.value()));
        }
        out.push_str("执行 REAPER 侧执行，UI 只产出指令\n");
        out
    }
}

/// Rollback instruction preview for the winning candidate.
///
/// Mirrors [`crate::cards::ApplyPreview`] with the whole-chain restore:
/// same single undo point, stable description
/// (`SynthLM: rollback <id>`), preview only, REAPER side executes.
#[derive(Clone, Debug, PartialEq)]
pub struct RollbackPreview {
    candidate_id: String,
    apply_token: String,
    target_snapshot: String,
}

impl RollbackPreview {
    /// Snapshot the winner's rollback fields into a preview.
    #[must_use]
    pub fn for_winner(winner: &ListedCandidate) -> Self {
        Self {
            candidate_id: winner.id().to_owned(),
            apply_token: winner.card().apply_token().to_owned(),
            target_snapshot: winner.target_snapshot().to_owned(),
        }
    }

    /// Candidate the preview rolls back.
    #[must_use]
    pub fn candidate_id(&self) -> &str {
        &self.candidate_id
    }

    /// Stable undo description for the DEC-008 transaction block.
    #[must_use]
    pub fn undo_desc(&self) -> String {
        format!("SynthLM: rollback {}", self.candidate_id)
    }

    /// Render the preview pane text (Chinese copy, ASCII structure).
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("回滚指令预览（仅预览，不执行）\n");
        out.push_str(&format!("候选 {}\n", self.candidate_id));
        out.push_str(&format!("事务 {UNDO_BEGIN} … {UNDO_END}（单事务）\n",));
        out.push_str(&format!("描述 {}\n", self.undo_desc()));
        out.push_str(&format!(
            "恢复 整链快照还原（目标 {}）\n",
            self.target_snapshot
        ));
        out.push_str(&format!("令牌 {}\n", self.apply_token));
        out.push_str("执行 REAPER 侧执行，UI 只产出指令\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real demo plan shape (TSK-506 wires exactly this file).
    const DEMO_PLAN: &str = include_str!("../../../experiments/e2e-demo/demo-plan.json");

    fn sample_entry(id: &str) -> ListedCandidate {
        let card = Card::new(
            "高频衰减，整体更暗。".to_owned(),
            0.75,
            -0.6,
            2,
            "demo-preview.wav".to_owned(),
            "demo-apply".to_owned(),
        )
        .expect("sample card");
        let ops = vec![
            ParamOp::new("replace".to_owned(), "param/1".to_owned(), 0.25).expect("sample op"),
        ];
        ListedCandidate::new(id.to_owned(), card, ops, "demo-snapshot".to_owned())
            .expect("sample entry")
    }

    #[test]
    fn loads_demo_plan_shape_with_three_cards() {
        let report = parse_plan_text(DEMO_PLAN).expect("demo plan parses");
        assert_eq!(report.len(), 3);
        assert!(report.rejected().is_empty());
        let first = report.candidates().first().expect("first card");
        assert_eq!(first.id(), "demo-wide");
        assert!(!first.card().diff().is_empty());
        assert!((0.0..=1.0).contains(&first.card().confidence()));
        assert_eq!(first.card().changed(), 3);
        assert_eq!(first.ops().len(), 3);
        assert!(!first.card().audio_ref().is_empty());
        assert!(!first.card().apply_token().is_empty());
    }

    #[test]
    fn missing_audio_ref_becomes_red_row_not_silent_drop() {
        let text = r#"{"candidates": [{
            "id": "broken", "diff_summary_zh": "更暗。",
            "confidence": 0.5, "delta_lufs": 0.0, "changed_params": 1,
            "apply_token": "tok",
            "patch": {"ops": [], "target_snapshot": "snap"}
        }]}"#;
        let report = parse_plan_text(text).expect("file shape parses");
        assert!(report.candidates().is_empty());
        assert_eq!(report.rejected().len(), 1);
        let rejected = report.rejected().first().expect("red row");
        assert_eq!(rejected.index(), 0);
        assert_eq!(*rejected.reason(), CardsError::EmptyAudioRef);
    }

    #[test]
    fn malformed_json_is_a_whole_file_error() {
        let err = parse_plan_text("{oops").expect_err("must fail");
        assert!(matches!(err, CardsError::InvalidJson { .. }));
    }

    #[test]
    fn non_object_top_level_is_not_a_plan() {
        let err = parse_plan_text("[1, 2]").expect_err("must fail");
        assert_eq!(err, CardsError::NotAPlan);
    }

    #[test]
    fn card_rejects_empty_diff_and_bad_confidence() {
        let empty = Card::new(
            "  ".to_owned(),
            0.5,
            0.0,
            1,
            "ref".to_owned(),
            "tok".to_owned(),
        );
        assert_eq!(empty, Err(CardsError::EmptyDiff));
        let over = Card::new(
            "更暗。".to_owned(),
            1.5,
            0.0,
            1,
            "ref".to_owned(),
            "tok".to_owned(),
        );
        assert_eq!(over, Err(CardsError::ConfidenceOutOfRange));
        let nan = Card::new(
            "更暗。".to_owned(),
            f64::NAN,
            0.0,
            1,
            "ref".to_owned(),
            "tok".to_owned(),
        );
        assert_eq!(nan, Err(CardsError::NonFiniteConfidence));
    }

    #[test]
    fn selection_holds_a_single_winner() {
        let roster = vec![sample_entry("aaa"), sample_entry("bbb")];
        let mut selection = Selection::new();
        assert!(selection.is_empty());
        assert!(selection.select_known("aaa", &roster));
        assert_eq!(selection.selected(), Some("aaa"));
        assert!(selection.select_known("bbb", &roster));
        assert_eq!(selection.selected(), Some("bbb"));
        assert!(!selection.select_known("zzz", &roster));
        assert_eq!(selection.selected(), Some("bbb"));
        selection.clear();
        assert!(selection.is_empty());
    }

    #[test]
    fn apply_preview_names_undo_transaction_and_params() {
        let winner = sample_entry("demo-dark");
        let preview = ApplyPreview::for_winner(&winner);
        assert_eq!(preview.undo_desc(), "SynthLM: apply demo-dark 1params");
        let text = preview.render();
        for needle in [
            UNDO_BEGIN,
            UNDO_END,
            "SynthLM: apply demo-dark 1params",
            "param/1",
            PROVENANCE_MARK,
            "REAPER",
        ] {
            assert!(text.contains(needle), "preview misses {needle}");
        }
    }

    #[test]
    fn rollback_preview_restores_whole_chain() {
        let winner = sample_entry("demo-dark");
        let preview = RollbackPreview::for_winner(&winner);
        assert_eq!(preview.undo_desc(), "SynthLM: rollback demo-dark");
        let text = preview.render();
        for needle in [UNDO_BEGIN, UNDO_END, "rollback demo-dark", "REAPER"] {
            assert!(text.contains(needle), "preview misses {needle}");
        }
    }
}
