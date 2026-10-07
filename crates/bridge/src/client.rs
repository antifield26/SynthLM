//! Bridge IPC client: pure frame builders, render-instruction queue, error mapping (TSK-502).
//!
//! Control-plane only (main thread, never the audio thread, `AGENTS.md` §3.2):
//! this module packs the [`crate::snapshot::Snapshot`] / [`crate::undo`]
//! outputs into [`synthlm_common::ipc`] wire frames, queues inbound render
//! instructions without executing them (render execution lives in REAPER via
//! `Main_OnCommand(42230)`; `acrd` only orchestrates, `ARCHITECTURE.md` §3
//! step 6), and maps `error` frames onto the existing
//! [`crate::undo::UndoError`] / [`crate::snapshot::SnapshotError`] taxonomy
//! with `retryable` / `BLOCKED` verdicts matching `ARCHITECTURE.md` §5.
//!
//! Transport is injected: [`crate::client::transmit`] / [`crate::client::receive`]
//! take any `Read + Write` stream, while [`crate::client::BridgeClient`] itself
//! stays transport-agnostic (pure builders + observable send/queue logs) so
//! unit tests run on loopback sockets or byte buffers, never on live REAPER
//! (DEC-024). Depends on `common` only besides the REAPER bindings (DEC-022:
//! no model/analysis deps); planner types are deliberately *not* imported —
//! patch ops travel as [`serde_json::Value`].

use std::collections::VecDeque;
use std::io::{Read, Write};

use serde_json::Value;
use synthlm_common::ipc::{
    EndpointRole, ErrorBody, ErrorCode, IpcError, MessageType, WireFrame, decode_frame_bytes,
    encode_frame, parse_error, read_frame,
};
use thiserror::Error;

use crate::snapshot::{Snapshot, SnapshotError};
use crate::undo::UndoError;

// ---------------------------------------------------------------------------
// Error type (library boundary: thiserror, AGENTS.md §4)
// ---------------------------------------------------------------------------

/// Bridge-side failure: local taxonomy plus remote `error`-frame verdicts.
///
/// `Remote` carries the peer [`synthlm_common::ipc::ErrorCode`] (the single
/// source of truth for retryable-vs-blocked, `ARCHITECTURE.md` §5) plus the
/// peer's short secret-free hint. Local variants reuse the existing
/// [`crate::undo::UndoError`] / [`crate::snapshot::SnapshotError`] taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BridgeError {
    /// A frame builder got an empty task id (frames correlate by task id).
    #[error("bridge got empty task id: frames are correlated by task id, never anonymous")]
    EmptyTaskId,

    /// A frame builder got an empty snapshot id.
    #[error("bridge got empty snapshot id: plans apply against a named frozen snapshot")]
    EmptySnapshotId,

    /// A score field was NaN or infinite (scores travel as finite numbers).
    #[error("bridge got non-finite score field: scores must be finite numbers")]
    BadScore,

    /// Patch ops were not a non-empty JSON array.
    #[error("bridge got empty or non-array patch ops: apply needs at least one op")]
    BadOps,

    /// Frame serialization failed (payload not representable on the wire).
    #[error("bridge frame encode failed: payload not serializable")]
    EncodeFailed,

    /// Local undo-transaction failure (existing taxonomy).
    #[error(transparent)]
    Undo(#[from] UndoError),

    /// Local snapshot failure (existing taxonomy).
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),

    /// Peer `error` frame: machine-readable failure class plus hint.
    #[error("bridge remote error [{code:?}]: {detail}")]
    Remote {
        /// Peer failure class (drives the retry verdict).
        code: ErrorCode,
        /// Peer hint (short, secret-free by wire contract).
        detail: String,
    },
}

impl BridgeError {
    /// Build a remote error from a wire code (detail is caller-supplied hint).
    #[must_use]
    pub fn from_code(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self::Remote {
            code,
            detail: detail.into(),
        }
    }

    /// Map a local [`synthlm_common::ipc::IpcError`] to a remote-style verdict.
    #[must_use]
    pub fn from_ipc(err: IpcError) -> Self {
        Self::Remote {
            code: err.code(),
            detail: "bridge transport failed (see ARCH §5)".to_owned(),
        }
    }

    /// Machine-readable failure class (reuses the
    /// [`synthlm_common::ipc::ErrorCode`] taxonomy so verdicts cannot drift).
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            BridgeError::EmptyTaskId
            | BridgeError::EmptySnapshotId
            | BridgeError::BadScore
            | BridgeError::BadOps => ErrorCode::ProtocolViolation,
            BridgeError::EncodeFailed => ErrorCode::Internal,
            BridgeError::Undo(_) | BridgeError::Snapshot(_) => ErrorCode::ProtocolViolation,
            BridgeError::Remote { code, .. } => *code,
        }
    }

    /// Whether this failure may enter backoff retry (`ARCHITECTURE.md` §5).
    #[must_use]
    pub fn retryable(&self) -> bool {
        self.code().retryable()
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    #[must_use]
    pub fn blocked(&self) -> bool {
        self.code().blocked()
    }

    /// Static remediation hint for UI / `BLOCKED` surfaces.
    #[must_use]
    pub fn guidance(&self) -> &'static str {
        match self.code() {
            ErrorCode::ConsentRequired => {
                "complete the first-run tier choice (1/2/3) or change it in settings; cloud calls stay BLOCKED (see DEC-010)"
            }
            ErrorCode::AuthDenied => {
                "set the API key in .env and restart; the key value is never logged (see DEC-010)"
            }
            ErrorCode::WhitelistViolation => {
                "restrict upload fields to the DEC-011 whitelist [prompt, mir, meta, audio_ref] (see DEC-011)"
            }
            ErrorCode::AudioCapabilityMissing => {
                "route audio understanding to Tier1 or Tier2; Tier3 local is text-only (see DEC-010)"
            }
            code if code.retryable() => {
                "back off and retry; escalate to the cached plan after the retry policy exhausts (see ARCH §5)"
            }
            _ => "fix the caller payload or protocol version, then resend (see ARCH §5)",
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound builders (pure: snapshot/undo output -> wire kind + body)
// ---------------------------------------------------------------------------

/// Task id carried by `body` (`""` when absent or not a string).
#[must_use]
pub fn task_id_of(body: &Value) -> String {
    body.get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Pack a frozen [`crate::snapshot::Snapshot`] into a `snapshot.submit` frame.
///
/// One solve correlates one task id with one snapshot id: both default to
/// `task_id` here (the caller may rename `snapshot_id` afterwards when a
/// solve fans out to several snapshots).
///
/// # Errors
///
/// [`crate::client::BridgeError::EmptyTaskId`] on an empty task id;
/// [`crate::client::BridgeError::Snapshot`] when the snapshot take GUID is
/// empty; [`crate::client::BridgeError::EncodeFailed`] when the snapshot is
/// not JSON-representable.
pub fn build_snapshot_submit(
    task_id: &str,
    snapshot: &Snapshot,
) -> Result<(MessageType, Value), BridgeError> {
    if task_id.is_empty() {
        return Err(BridgeError::EmptyTaskId);
    }
    if snapshot.take_guid.is_empty() {
        return Err(BridgeError::Snapshot(SnapshotError::EmptyTakeGuid));
    }
    let snapshot_value = serde_json::to_value(snapshot).map_err(|_| BridgeError::EncodeFailed)?;
    let params_len = u64::try_from(snapshot.params.len()).unwrap_or(u64::MAX);
    let chunk_bytes = u64::try_from(snapshot.chunk.len()).unwrap_or(u64::MAX);
    Ok((
        MessageType::SnapshotSubmit,
        serde_json::json!({
            "task_id": task_id,
            "snapshot_id": task_id,
            "take_guid": snapshot.take_guid,
            "params_len": params_len,
            "chunk_bytes": chunk_bytes,
            "snapshot": snapshot_value,
        }),
    ))
}

/// Pack a `plan.request` frame for `snapshot_id` under `task_id`.
///
/// `fields` names the upload fields the solve may use (subset of the DEC-011
/// whitelist; validated again acrd-side before anything leaves the machine).
///
/// # Errors
///
/// [`crate::client::BridgeError::EmptyTaskId`] / [`crate::client::BridgeError::EmptySnapshotId`]
/// on empty ids.
pub fn build_plan_request(
    task_id: &str,
    snapshot_id: &str,
    fields: &[String],
    byte_count: u64,
) -> Result<(MessageType, Value), BridgeError> {
    if task_id.is_empty() {
        return Err(BridgeError::EmptyTaskId);
    }
    if snapshot_id.is_empty() {
        return Err(BridgeError::EmptySnapshotId);
    }
    Ok((
        MessageType::PlanRequest,
        serde_json::json!({
            "task_id": task_id,
            "snapshot_id": snapshot_id,
            "fields": fields,
            "byte_count": byte_count,
        }),
    ))
}

/// Pack a `patch.apply` frame: `ops` is the JSON op array (planner-shaped,
/// carried opaquely so this crate never depends on planner types).
///
/// # Errors
///
/// [`crate::client::BridgeError::EmptyTaskId`] / [`crate::client::BridgeError::EmptySnapshotId`]
/// on empty ids; [`crate::client::BridgeError::BadOps`] when `ops` is not a
/// non-empty array.
pub fn build_patch_apply(
    task_id: &str,
    target_snapshot: &str,
    ops: &Value,
) -> Result<(MessageType, Value), BridgeError> {
    if task_id.is_empty() {
        return Err(BridgeError::EmptyTaskId);
    }
    if target_snapshot.is_empty() {
        return Err(BridgeError::EmptySnapshotId);
    }
    match ops.as_array() {
        Some(items) if !items.is_empty() => Ok((
            MessageType::PatchApply,
            serde_json::json!({
                "task_id": task_id,
                "target_snapshot": target_snapshot,
                "ops": ops,
            }),
        )),
        _ => Err(BridgeError::BadOps),
    }
}

/// Pack a `render.result` frame: the bridge reporting a REAPER-side render it
/// was instructed to run (see [`crate::client::PendingRenderQueue`]).
///
/// # Errors
///
/// [`crate::client::BridgeError::EmptyTaskId`] on an empty task id.
pub fn build_render_result(task_id: &str, ok: bool) -> Result<(MessageType, Value), BridgeError> {
    if task_id.is_empty() {
        return Err(BridgeError::EmptyTaskId);
    }
    Ok((
        MessageType::RenderResult,
        serde_json::json!({"task_id": task_id, "ok": ok}),
    ))
}

/// Score fields for a `score.report` frame (finite numbers only; the eval
/// weighting itself runs acrd-side via `synthlm-eval`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScoreFields {
    /// Mean spectral distance (post-normalisation).
    pub spec_l1: f64,
    /// Mean log-mel distance (post-normalisation).
    pub mel_l1: f64,
    /// LOW-band log-mel distance.
    pub mel_low: f64,
    /// MID-band log-mel distance.
    pub mel_mid: f64,
    /// HIGH-band log-mel distance.
    pub mel_high: f64,
    /// Onset F1 similarity.
    pub transient_f1: f64,
    /// Re-measured integrated LUFS.
    pub lufs_i: f64,
    /// Post-normalisation true peak (dBTP).
    pub true_peak: f64,
    /// Applied gain in dB.
    pub delta_lufs: f64,
}

impl ScoreFields {
    /// Whether every field is finite (wire precondition).
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.spec_l1.is_finite()
            && self.mel_l1.is_finite()
            && self.mel_low.is_finite()
            && self.mel_mid.is_finite()
            && self.mel_high.is_finite()
            && self.transient_f1.is_finite()
            && self.lufs_i.is_finite()
            && self.true_peak.is_finite()
            && self.delta_lufs.is_finite()
    }
}

/// Pack a `score.report` frame from finite score fields.
///
/// # Errors
///
/// [`crate::client::BridgeError::EmptyTaskId`] on an empty task id;
/// [`crate::client::BridgeError::BadScore`] on any non-finite field.
pub fn build_score_report(
    task_id: &str,
    scores: &ScoreFields,
) -> Result<(MessageType, Value), BridgeError> {
    if task_id.is_empty() {
        return Err(BridgeError::EmptyTaskId);
    }
    if !scores.is_finite() {
        return Err(BridgeError::BadScore);
    }
    Ok((
        MessageType::ScoreReport,
        serde_json::json!({
            "task_id": task_id,
            "spec_l1": scores.spec_l1,
            "mel_l1": scores.mel_l1,
            "mel_low": scores.mel_low,
            "mel_mid": scores.mel_mid,
            "mel_high": scores.mel_high,
            "transient_f1": scores.transient_f1,
            "lufs_i": scores.lufs_i,
            "true_peak": scores.true_peak,
            "delta_lufs": scores.delta_lufs,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Byte / stream transport (injected streams; never the audio thread)
// ---------------------------------------------------------------------------

/// Encode one length-prefixed frame for `kind` + `body`.
///
/// # Errors
///
/// [`crate::client::BridgeError::EncodeFailed`] when the payload exceeds the
/// wire cap or is not serializable.
pub fn frame_bytes(kind: MessageType, body: &Value) -> Result<Vec<u8>, BridgeError> {
    encode_frame(kind, body).map_err(|_| BridgeError::EncodeFailed)
}

/// Decode one complete frame from a byte slice (prefix + payload).
///
/// # Errors
///
/// [`crate::client::BridgeError::EncodeFailed`] on truncation, oversize, or
/// version mismatch (local framing failure, never a remote verdict).
pub fn parse_bytes(buf: &[u8]) -> Result<WireFrame, BridgeError> {
    decode_frame_bytes(buf).map_err(BridgeError::from_ipc)
}

/// Send one frame over an injected byte stream (loopback socket, pipe, or
/// test cursor — never the audio thread).
///
/// # Errors
///
/// [`crate::client::BridgeError::EncodeFailed`] on encode failure;
/// [`crate::client::BridgeError::Remote`] (`transport_closed`) on write
/// failure.
pub fn transmit(
    stream: &mut impl Write,
    kind: MessageType,
    body: &Value,
) -> Result<(), BridgeError> {
    let bytes = frame_bytes(kind, body)?;
    stream.write_all(&bytes).map_err(|_| {
        BridgeError::from_code(ErrorCode::TransportClosed, "bridge transmit failed")
    })?;
    Ok(())
}

/// Receive one frame from an injected byte stream.
///
/// # Errors
///
/// [`crate::client::BridgeError::Remote`] carrying the wire-mapped failure
/// class (clean EOF maps to `transport_closed`).
pub fn receive(stream: &mut impl Read) -> Result<WireFrame, BridgeError> {
    read_frame(stream).map_err(BridgeError::from_ipc)
}

// ---------------------------------------------------------------------------
// Inbound handling: render instructions queue, never execute
// ---------------------------------------------------------------------------

/// What an inbound frame means to the bridge client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundAction {
    /// A `render.request` instruction: queued for REAPER-side execution,
    /// never run here.
    RenderQueued {
        /// Correlated task id (`""` when the body carries none).
        task_id: String,
    },
    /// An `audit.event` acknowledgement worth noting in the client log.
    AuditNoted {
        /// Correlated task id (`""` when the body carries none).
        task_id: String,
    },
    /// A peer `error` envelope (retry verdict via
    /// [`crate::client::BridgeError::code`]).
    RemoteError(ErrorBody),
    /// Anything else: observed, no client state change.
    Ignored {
        /// Inbound message kind.
        kind: MessageType,
    },
}

/// Classify one inbound frame (pure: no queue or I/O side effects).
#[must_use]
pub fn handle_inbound(frame: &WireFrame) -> InboundAction {
    match frame.kind {
        MessageType::RenderRequest => InboundAction::RenderQueued {
            task_id: task_id_of(&frame.body),
        },
        MessageType::AuditEvent => InboundAction::AuditNoted {
            task_id: task_id_of(&frame.body),
        },
        MessageType::Error => match parse_error(frame) {
            Ok(body) => InboundAction::RemoteError(body),
            Err(_) => InboundAction::Ignored {
                kind: MessageType::Error,
            },
        },
        other => InboundAction::Ignored { kind: other },
    }
}

/// Queue of render instructions awaiting REAPER-side execution.
///
/// The bridge never renders: a queued frame is a to-do the main-thread REAPER
/// adapter drains via `Main_OnCommand(42230)` on its own schedule. The queue
/// is observable ([`crate::client::PendingRenderQueue::len`],
/// [`crate::client::PendingRenderQueue::queued_task_ids`]) so tests prove
/// instructions park here instead of running.
#[derive(Debug, Default)]
pub struct PendingRenderQueue {
    pending: VecDeque<WireFrame>,
}

impl PendingRenderQueue {
    /// Empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }

    /// Queue `frame` when it is a `render.request` instruction.
    ///
    /// Returns the correlated task id when queued, `None` otherwise (the
    /// frame is left alone: queueing never swallows non-render traffic).
    pub fn push_frame(&mut self, frame: &WireFrame) -> Option<String> {
        if frame.kind != MessageType::RenderRequest {
            return None;
        }
        let task_id = task_id_of(&frame.body);
        self.pending.push_back(frame.clone());
        Some(task_id)
    }

    /// Pop the oldest queued instruction (`None` when empty).
    #[must_use]
    pub fn pop_next(&mut self) -> Option<WireFrame> {
        self.pending.pop_front()
    }

    /// Queued instruction count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether no instruction is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Task ids of the queued instructions, in queue order.
    #[must_use]
    pub fn queued_task_ids(&self) -> Vec<String> {
        self.pending
            .iter()
            .map(|frame| task_id_of(&frame.body))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Bridge client (transport-agnostic facade)
// ---------------------------------------------------------------------------

/// Transport-agnostic bridge client: builders plus observable send/queue logs.
///
/// Send paths stay pure (builders return kind + body; the caller moves bytes
/// over its injected stream via [`crate::client::transmit`]); inbound frames
/// enter via [`crate::client::BridgeClient::observe`] and render instructions
/// park in the queue. Every hop records its task id so audit chains stay
/// associable end to end.
#[derive(Debug)]
pub struct BridgeClient {
    role: EndpointRole,
    sent: Vec<(String, MessageType)>,
    queue: PendingRenderQueue,
    noted_audits: Vec<String>,
}

impl BridgeClient {
    /// Build a client speaking as `role` (normally
    /// [`synthlm_common::ipc::EndpointRole::Bridge`]).
    #[must_use]
    pub fn new(role: EndpointRole) -> Self {
        Self {
            role,
            sent: Vec::new(),
            queue: PendingRenderQueue::new(),
            noted_audits: Vec::new(),
        }
    }

    /// The role this client handshakes as.
    #[must_use]
    pub fn role(&self) -> EndpointRole {
        self.role
    }

    /// Record an outbound frame (task correlation for the audit chain).
    pub fn note_sent(&mut self, task_id: &str, kind: MessageType) {
        self.sent.push((task_id.to_owned(), kind));
    }

    /// Outbound frames recorded so far (`(task_id, kind)` in send order).
    #[must_use]
    pub fn sent(&self) -> &[(String, MessageType)] {
        &self.sent
    }

    /// How many frames were sent.
    #[must_use]
    pub fn sent_count(&self) -> usize {
        self.sent.len()
    }

    /// How many sent frames had message kind `kind`.
    #[must_use]
    pub fn sent_for(&self, kind: MessageType) -> usize {
        self.sent.iter().filter(|(_, sent)| *sent == kind).count()
    }

    /// Observe one inbound frame: render instructions queue, audit acks are
    /// noted, everything else is classified without state change.
    pub fn observe(&mut self, frame: &WireFrame) -> InboundAction {
        let action = handle_inbound(frame);
        match &action {
            InboundAction::RenderQueued { .. } => {
                let _ = self.queue.push_frame(frame);
            }
            InboundAction::AuditNoted { task_id } => {
                self.noted_audits.push(task_id.clone());
            }
            InboundAction::RemoteError(_) | InboundAction::Ignored { .. } => {}
        }
        action
    }

    /// Queued render-instruction count (instructions park here; the bridge
    /// never executes them).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }

    /// Pop the oldest queued render instruction (`None` when empty).
    #[must_use]
    pub fn next_render(&mut self) -> Option<WireFrame> {
        self.queue.pop_next()
    }

    /// Task ids of the queued render instructions, in queue order.
    #[must_use]
    pub fn queued_task_ids(&self) -> Vec<String> {
        self.queue.queued_task_ids()
    }

    /// Task ids of the audit acknowledgements observed so far.
    #[must_use]
    pub fn noted_audits(&self) -> &[String] {
        &self.noted_audits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ParamEntry, RenderBackup};
    use synthlm_common::ipc::{ConsentTier, current_version};

    fn test_snapshot() -> Snapshot {
        Snapshot {
            take_guid: "{take-1}".to_owned(),
            params: vec![ParamEntry::new("cutoff".to_owned(), 0.5, 0.0, 1.0, 0.5).unwrap()],
            chunk: "chunk-body".to_owned(),
            take_fx_nch: Some(2.0),
            midi_bytes: None,
            midi_hash: None,
            render: RenderBackup::default(),
        }
    }

    fn finite_scores() -> ScoreFields {
        ScoreFields {
            spec_l1: 0.1,
            mel_l1: 0.2,
            mel_low: 0.05,
            mel_mid: 0.25,
            mel_high: 0.3,
            transient_f1: 0.9,
            lufs_i: -14.0,
            true_peak: -8.0,
            delta_lufs: 1.5,
        }
    }

    #[test]
    fn snapshot_submit_packs_take_guid_and_counts() {
        let (kind, body) = build_snapshot_submit("task-1", &test_snapshot()).unwrap();
        assert_eq!(kind, MessageType::SnapshotSubmit);
        assert_eq!(
            body.get("take_guid").and_then(Value::as_str),
            Some("{take-1}")
        );
        assert_eq!(body.get("task_id").and_then(Value::as_str), Some("task-1"));
        assert_eq!(
            body.get("snapshot_id").and_then(Value::as_str),
            Some("task-1")
        );
        assert_eq!(body.get("params_len").and_then(Value::as_u64), Some(1));
        assert!(body.get("snapshot").is_some());
    }

    #[test]
    fn builders_reject_empty_ids_and_bad_payloads() {
        assert_eq!(
            build_snapshot_submit("", &test_snapshot()).unwrap_err(),
            BridgeError::EmptyTaskId
        );
        let mut guidless = test_snapshot();
        guidless.take_guid.clear();
        assert_eq!(
            build_snapshot_submit("t", &guidless).unwrap_err(),
            BridgeError::Snapshot(SnapshotError::EmptyTakeGuid)
        );
        assert_eq!(
            build_plan_request("", "s", &[], 0).unwrap_err(),
            BridgeError::EmptyTaskId
        );
        assert_eq!(
            build_plan_request("t", "", &[], 0).unwrap_err(),
            BridgeError::EmptySnapshotId
        );
        assert_eq!(
            build_patch_apply("t", "s", &serde_json::json!([])).unwrap_err(),
            BridgeError::BadOps
        );
        assert_eq!(
            build_patch_apply("t", "s", &serde_json::json!({"op": 1})).unwrap_err(),
            BridgeError::BadOps
        );
        assert_eq!(
            build_render_result("", true).unwrap_err(),
            BridgeError::EmptyTaskId
        );
        let mut inf = finite_scores();
        inf.true_peak = f64::INFINITY;
        assert_eq!(
            build_score_report("t", &inf).unwrap_err(),
            BridgeError::BadScore
        );
    }

    #[test]
    fn plan_patch_render_score_builders_set_kinds() {
        let fields = vec!["prompt".to_owned(), "mir".to_owned()];
        let (kind, body) = build_plan_request("t", "snap-1", &fields, 128).unwrap();
        assert_eq!(kind, MessageType::PlanRequest);
        assert_eq!(body.get("fields").unwrap(), &serde_json::json!(fields));

        let ops = serde_json::json!([{"op": "replace", "path": "param/x", "value": 0.5}]);
        let (kind, body) = build_patch_apply("t", "snap-1", &ops).unwrap();
        assert_eq!(kind, MessageType::PatchApply);
        assert_eq!(body.get("ops").unwrap(), &ops);

        let (kind, body) = build_render_result("t", true).unwrap();
        assert_eq!(kind, MessageType::RenderResult);
        assert_eq!(body.get("ok").and_then(Value::as_bool), Some(true));

        let (kind, body) = build_score_report("t", &finite_scores()).unwrap();
        assert_eq!(kind, MessageType::ScoreReport);
        assert_eq!(body.get("task_id").and_then(Value::as_str), Some("t"));
    }

    #[test]
    fn frame_bytes_roundtrip_all_kinds() {
        for kind in MessageType::all() {
            let body = serde_json::json!({"task_id": "t"});
            let bytes = frame_bytes(*kind, &body).unwrap();
            let back = parse_bytes(&bytes).unwrap();
            assert_eq!(back.kind, *kind);
            assert_eq!(back.body, body);
            assert_eq!(back.ver, current_version());
        }
    }

    #[test]
    fn render_instructions_queue_without_executing() {
        let mut client = BridgeClient::new(EndpointRole::Bridge);
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::RenderRequest,
            body: serde_json::json!({"task_id": "t-render-1", "executor": "bridge/reaper"}),
        };
        match client.observe(&frame) {
            InboundAction::RenderQueued { task_id } => assert_eq!(task_id, "t-render-1"),
            other => panic!("must queue, got {other:?}"),
        }
        assert_eq!(client.pending_count(), 1);
        assert_eq!(client.queued_task_ids(), vec!["t-render-1".to_owned()]);
        // Non-render traffic never queues.
        let audit = WireFrame {
            ver: current_version(),
            kind: MessageType::AuditEvent,
            body: serde_json::json!({"task_id": "t-audit-1"}),
        };
        match client.observe(&audit) {
            InboundAction::AuditNoted { task_id } => assert_eq!(task_id, "t-audit-1"),
            other => panic!("must note audit, got {other:?}"),
        }
        assert_eq!(client.pending_count(), 1, "audit must not queue");
        assert_eq!(client.noted_audits(), &["t-audit-1".to_owned()]);
        // Draining returns the exact instruction for REAPER-side execution.
        let next = client.next_render().unwrap();
        assert_eq!(next.kind, MessageType::RenderRequest);
        assert!(client.next_render().is_none());
    }

    #[test]
    fn error_taxonomy_covers_all_wire_codes() {
        assert_eq!(ErrorCode::all().len(), 13);
        for code in ErrorCode::all() {
            let err = BridgeError::from_code(*code, "hint");
            assert_eq!(err.code(), *code);
            assert_eq!(err.retryable(), code.retryable());
            assert_eq!(err.blocked(), code.blocked());
            assert!(!err.guidance().is_empty());
            assert!(!format!("{err}").is_empty());
        }
        // Local taxonomy maps to BLOCKED protocol verdicts.
        for local in [
            BridgeError::EmptyTaskId,
            BridgeError::EmptySnapshotId,
            BridgeError::BadScore,
            BridgeError::BadOps,
            BridgeError::EncodeFailed,
            BridgeError::Undo(UndoError::EmptyOp),
            BridgeError::Snapshot(SnapshotError::EmptyTakeGuid),
        ] {
            assert!(local.blocked(), "{local:?} must be BLOCKED");
            assert!(!local.retryable());
        }
        // Retryable remote verdicts survive the trip through an error frame.
        for code in [
            ErrorCode::Timeout,
            ErrorCode::TransportClosed,
            ErrorCode::RenderUnstable,
            ErrorCode::CloudUnavailable,
        ] {
            let bytes = synthlm_common::ipc::error_frame(code, "retryable").expect("error frame");
            let frame = parse_bytes(&bytes).expect("decode error frame");
            match handle_inbound(&frame) {
                InboundAction::RemoteError(body) => {
                    assert_eq!(body.code, code);
                    let mapped = BridgeError::from_code(body.code, body.detail);
                    assert!(mapped.retryable(), "{code:?} must retry");
                }
                other => panic!("must classify error, got {other:?}"),
            }
        }
        // Terminal remote verdicts stay BLOCKED.
        for code in [
            ErrorCode::ConsentRequired,
            ErrorCode::AuthDenied,
            ErrorCode::WhitelistViolation,
            ErrorCode::AudioCapabilityMissing,
        ] {
            assert!(BridgeError::from_code(code, "hint").blocked());
        }
    }

    #[test]
    fn consent_tier_spellings_cover_gateway_tiers() {
        // The client never routes tiers itself, but it must speak the same
        // consent vocabulary as the acrd planner chain (Tier3 local-only).
        assert_eq!(format!("{:?}", ConsentTier::Tier3), "Tier3");
    }

    #[test]
    fn loopback_socket_carries_builder_frames() {
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("loopback addr");
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            receive(&mut conn).expect("server recv")
        });
        let (_, body) = build_snapshot_submit("t-loop-1", &test_snapshot()).unwrap();
        let mut client = TcpStream::connect(addr).expect("connect");
        transmit(&mut client, MessageType::SnapshotSubmit, &body).expect("send");
        let frame = server.join().expect("server thread");
        assert_eq!(frame.kind, MessageType::SnapshotSubmit);
        assert_eq!(task_id_of(&frame.body), "t-loop-1");

        // Client send log stays task-correlated end to end.
        let mut facade = BridgeClient::new(EndpointRole::Bridge);
        facade.note_sent("t-loop-1", MessageType::SnapshotSubmit);
        facade.note_sent("t-loop-1", MessageType::PlanRequest);
        assert_eq!(facade.sent_count(), 2);
        assert_eq!(facade.sent_for(MessageType::SnapshotSubmit), 1);
    }
}
