//! acrd dispatch: true IPC wiring from wire frames to the planner chain (TSK-502).
//!
//! Replaces the [`crate::daemon::decide`] skeleton replies on the serving
//! path with real orchestration (control plane only, never the audio thread,
//! `AGENTS.md` §3.2; model inference, audio analysis, and render waiting never
//! enter the DAW process, L8):
//!
//! - `snapshot.submit` → the snapshot is recorded and its task is journaled
//!   (`Pending → Running → Done`) through [`crate::task::TaskLog`] (WAL to
//!   `journal.jsonl`); the reply is an `audit.event` acknowledgement.
//! - `plan.request` → the planner chain runs offline via
//!   [`synthlm_planner::planning::plan_for_intent`] on the explicitly
//!   selected backend (serving path: `MockSeeded` seeded candidates, no
//!   network; a wire `"live-tier2"` selection is BLOCKED by design — live
//!   planning needs local caller context and is never triggered remotely):
//!   profile whitelist ([`synthlm_profile::builtins`] `reaeq` factory
//!   profile) + [`synthlm_planner::patch`] validation with repair
//!   (≤ 2 rounds) + [`synthlm_planner::search`] mock-objective search +
//!   [`synthlm_planner::candidate::diversify`] +
//!   [`synthlm_planner::model_gw::Gateway`] routing over
//!   [`synthlm_planner::model_gw::MockTransport`] (the reply carries the
//!   `backend` watermark next to the unchanged `"mock"` object, marking
//!   `"mock"` sources honestly). Stored consent is Tier3 local-only, so the
//!   chain needs no key and nothing leaves the machine.
//! - `patch.apply` → the plan is re-validated against the same profile
//!   (dry-run snapshot-restore semantics: no DAW is touched here); the reply
//!   is a `patch.result` carrying `dry_run: true`.
//! - `render.request` → the render is *not* executed: the reply re-issues a
//!   `render.request` instruction frame naming `executor: "bridge/reaper"`
//!   with `status: "pending_bridge"`. The render task stays `Running` until a
//!   `render.result` closes it. Rendering executes REAPER-side
//!   (`Main_OnCommand(42230)` fixed-block path); `acrd` only orchestrates.
//! - `score.report` → reported score fields are validated (finite), ranked
//!   through [`synthlm_eval::score::BandWeights`] with the
//!   [`synthlm_eval::score::Score::true_peak_alarm`] check, and stored. The
//!   full [`synthlm_eval::score::compare`] over MIR features is the true-render
//!   hookup point (recorded as an open item in the task return, not claimed
//!   here).
//! - Unknown wire types fail frame parsing upstream and arrive as
//!   [`synthlm_common::ipc::IpcError::Json`], which gets an `error` reply with
//!   the connection kept open (existing [`crate::daemon::decide`] semantics).
//!
//! Every handler records a [`crate::dispatch::DispatchAudit`] tagged with the
//! request `task_id`, so each hop's audit event stays associable across the
//! whole `snapshot → plan → patch → render → score` chain. Concurrent
//! connections share one [`crate::dispatch::Dispatcher`] behind a mutex on
//! the serving path (see [`crate::daemon::serve_until_shutdown`]); the WAL
//! file leads memory on every mutation, so sequential chains are crash-safe
//! by the [`crate::task::TaskLog`] contract.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use synthlm_common::config::validate_upload_fields;
use synthlm_common::consent::{CONFIG_VERSION, ConsentState, ConsentStore};
use synthlm_common::ipc::{
    AuditEvent, ConsentTier, ErrorCode, IpcError, MessageType, WireFrame, now_ms,
};
use synthlm_eval::score::{BandWeights, Score};
use synthlm_planner::candidate::{Candidate, count_changed_ops};
use synthlm_planner::model_gw::{Gateway, MockTransport, RouteParams};
use synthlm_planner::patch::{DEFAULT_MAX_REPAIR_ROUNDS, PatchOp, PatchPlan, validate_with_repair};
use synthlm_planner::planning::{ModelBackend, PlanningError, plan_for_intent};
use synthlm_planner::search::{MockBowl, SearchConfig, SearchSpace, two_stage_search};
use synthlm_profile::builtins::load_builtin;
use synthlm_profile::schema::Profile;

use crate::daemon::{StepOutcome, error_body, hello_body};
use crate::task::{TaskLog, TaskState};

// ---------------------------------------------------------------------------
// Dispatch errors (value-free, BLOCKED taxonomy)
// ---------------------------------------------------------------------------

/// Dispatch failure. Variants carry static labels only (field names, op
/// positions) — never caller material — so formatting an error can never echo
/// secrets, prompts, PCM references, or paths (`AGENTS.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    /// A required object field is missing.
    MissingField(&'static str),
    /// A required id field is empty.
    EmptyField(&'static str),
    /// A field has the wrong JSON shape.
    BadJson(&'static str),
    /// Upload fields fall outside the DEC-011 whitelist.
    Whitelist,
    /// The repaired plan still carries residual errors (carries the first
    /// residual [`synthlm_common::ipc::ErrorCode`]).
    PatchResidual {
        /// First residual error class.
        code: ErrorCode,
    },
    /// Repair dropped every op (nothing applicable survives).
    PlanEmpty,
    /// The offline planner chain failed internally.
    ChainFailed,
    /// A live backend was selected over the wire. The serving path stays
    /// offline (`MockSeeded` only); live planning needs local caller
    /// context and is never triggered remotely.
    UnsupportedBackend,
    /// The model-gateway leg failed (carries the gateway error class).
    Gateway {
        /// Gateway failure class.
        code: ErrorCode,
    },
    /// The task journal refused a mutation.
    Store,
    /// A score field is missing or non-finite.
    BadScore,
    /// A referenced task or snapshot id is unknown (submit first).
    UnknownTask,
    /// A stage task is already `Done` (resending a completed stage).
    DuplicateTask,
    /// The factory profile failed to load.
    ProfileFailed,
}

impl DispatchError {
    /// Map to the [`synthlm_common::ipc`] taxonomy.
    #[must_use]
    pub fn code(self) -> ErrorCode {
        match self {
            DispatchError::MissingField(_)
            | DispatchError::EmptyField(_)
            | DispatchError::BadJson(_)
            | DispatchError::PlanEmpty
            | DispatchError::BadScore
            | DispatchError::UnknownTask
            | DispatchError::DuplicateTask
            | DispatchError::UnsupportedBackend => ErrorCode::ProtocolViolation,
            DispatchError::Whitelist => ErrorCode::WhitelistViolation,
            DispatchError::PatchResidual { code } | DispatchError::Gateway { code } => code,
            DispatchError::ChainFailed | DispatchError::Store | DispatchError::ProfileFailed => {
                ErrorCode::Internal
            }
        }
    }

    /// Whether this failure may enter backoff retry.
    #[must_use]
    pub fn retryable(self) -> bool {
        self.code().retryable()
    }

    /// Whether this failure is user-visible `BLOCKED` (never retried).
    #[must_use]
    pub fn blocked(self) -> bool {
        self.code().blocked()
    }

    /// Short secret-free detail for the wire `error` frame.
    #[must_use]
    pub fn detail(self) -> String {
        match self {
            DispatchError::MissingField(field) => format!("missing field {field}"),
            DispatchError::EmptyField(field) => format!("empty field {field}"),
            DispatchError::BadJson(what) => format!("malformed {what}"),
            DispatchError::Whitelist => "upload fields outside DEC-011 whitelist".to_owned(),
            DispatchError::PatchResidual { .. } => "patch residual after repair".to_owned(),
            DispatchError::PlanEmpty => "plan repaired to empty".to_owned(),
            DispatchError::ChainFailed => "offline planner chain failed".to_owned(),
            DispatchError::UnsupportedBackend => {
                "live planning is not served over IPC; the serving path stays offline".to_owned()
            }
            DispatchError::Gateway { .. } => "model gateway leg failed".to_owned(),
            DispatchError::Store => "task journal refused mutation".to_owned(),
            DispatchError::BadScore => "score field missing or non-finite".to_owned(),
            DispatchError::UnknownTask => "unknown task id; submit snapshot first".to_owned(),
            DispatchError::DuplicateTask => "stage task already done".to_owned(),
            DispatchError::ProfileFailed => "factory profile not loadable".to_owned(),
        }
    }

    /// Static remediation hint for UI / `BLOCKED` surfaces.
    #[must_use]
    pub fn guidance(self) -> &'static str {
        match self {
            DispatchError::MissingField(_)
            | DispatchError::EmptyField(_)
            | DispatchError::BadJson(_) => {
                "resend the frame with the documented object fields (see ARCH §5)"
            }
            DispatchError::Whitelist => {
                "restrict upload fields to [prompt, mir, meta, audio_ref]; PCM and key material stay local (see DEC-011)"
            }
            DispatchError::PatchResidual { .. } => {
                "retarget ops to whitelisted idents (param/<ident> or macro/<name>) and resend (see DEC-013)"
            }
            DispatchError::PlanEmpty => {
                "every op was illegal; send at least one whitelisted op (see DEC-013)"
            }
            DispatchError::ChainFailed | DispatchError::Store | DispatchError::ProfileFailed => {
                "internal dispatch failure; retry once, then file the journal + profile state (see ARCH §8)"
            }
            DispatchError::UnsupportedBackend => {
                "run live Tier2 planning in-process via the planning API with local consent, key presence, and session context; the wire path stays offline (see DEC-010)"
            }
            DispatchError::Gateway { .. } => {
                "check the local endpoint, then retry; repeated exhaustion trips per-tier breakers (see DEC-011)"
            }
            DispatchError::BadScore => "scores travel as finite numbers; resend with finite fields",
            DispatchError::UnknownTask => "submit snapshot.submit before plan/patch/render/score",
            DispatchError::DuplicateTask => {
                "that stage already completed; use a fresh task id to re-run"
            }
        }
    }
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dispatch failed [{:?}]: {}", self.code(), self.detail())
    }
}

impl std::error::Error for DispatchError {}

// ---------------------------------------------------------------------------
// Stored records
// ---------------------------------------------------------------------------

/// One audit hop tagged with its chain task id.
///
/// [`synthlm_common::ipc::AuditEvent`] itself stays at exactly five fields
/// (no task slot by construction); this wrapper carries the correlation so
/// every hop's event stays associable with its chain.
#[derive(Debug, Clone)]
pub struct DispatchAudit {
    /// Chain task id from the requesting frame.
    pub task_id: String,
    /// Dispatch stage (`"snapshot.submit"`, `"plan.request"`, …).
    pub stage: &'static str,
    /// The five-field audit event for this hop.
    pub event: AuditEvent,
}

/// A stored snapshot submission (journal task id = `snapshot_id`).
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    /// Chain task id from the requesting frame.
    pub task_id: String,
    /// Journal task id (`snapshot.submit` body `snapshot_id`).
    pub snapshot_id: String,
    /// Frozen take identity.
    pub take_guid: String,
    /// Parameter rows frozen at submit time.
    pub params_len: u64,
    /// Chunk size in bytes at submit time.
    pub chunk_bytes: u64,
}

/// A stored score report plus its eval-derived ranking scalar.
#[derive(Debug, Clone)]
pub struct ScoreRecord {
    /// Chain task id from the requesting frame.
    pub task_id: String,
    /// Mean spectral distance.
    pub spec_l1: f64,
    /// Mean log-mel distance.
    pub mel_l1: f64,
    /// LOW-band distance.
    pub mel_low: f64,
    /// MID-band distance.
    pub mel_mid: f64,
    /// HIGH-band distance.
    pub mel_high: f64,
    /// Default-weighted band combination (ranking signal).
    pub mel_weighted: f64,
    /// Onset F1 similarity.
    pub transient_f1: f64,
    /// Re-measured integrated LUFS.
    pub lufs_i: f64,
    /// Post-normalisation true peak (dBTP).
    pub true_peak: f64,
    /// Applied gain in dB.
    pub delta_lufs: f64,
    /// Downstream-limiting alarm (`true_peak > −1 dBTP`).
    pub alarm: bool,
}

/// A render instruction awaiting REAPER-side execution.
#[derive(Debug, Clone)]
pub struct RenderInstruction {
    /// Chain task id from the requesting frame.
    pub task_id: String,
    /// Candidate to render.
    pub candidate_id: String,
    /// Fixed executor label: rendering runs REAPER-side.
    pub executor: &'static str,
    /// Fixed status: `acrd` orchestrates, never waits.
    pub status: &'static str,
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Factory profile backing plan validation (evidence-pinned `reaeq`
/// built-in; the mock planner chain solves against real whitelist entries).
pub const DISPATCH_PROFILE_NAME: &str = "reaeq";

/// Executor label on render-instruction frames (rendering runs REAPER-side).
pub const RENDER_EXECUTOR: &str = "bridge/reaper";

/// Status on render-instruction frames (`acrd` orchestrates, never waits).
pub const RENDER_STATUS_PENDING: &str = "pending_bridge";

/// Default upload fields for `plan.request` when the caller names none.
pub const DEFAULT_PLAN_FIELDS: &[&str] = &["prompt", "mir", "meta"];

/// Mock marker for non-model audit hops (no model ran there).
pub const DISPATCH_MOCK_MODEL: &str = "dispatch-mock";

/// True IPC dispatcher: wire frames in, wire replies out, tasks in the WAL.
///
/// Open with [`crate::dispatch::Dispatcher::open`]; drive one frame at a time
/// with [`crate::dispatch::Dispatcher::dispatch_outcome`] (the serving path
/// shares one instance across connections behind a mutex).
pub struct Dispatcher {
    log: TaskLog,
    profile: Profile,
    profile_name: &'static str,
    audits: Vec<DispatchAudit>,
    snapshots: HashMap<String, SnapshotRecord>,
    scores: Vec<ScoreRecord>,
    renders: HashMap<String, RenderInstruction>,
}

impl Dispatcher {
    /// Open the journal in `state_dir` and load the factory profile.
    ///
    /// # Errors
    ///
    /// [`crate::dispatch::DispatchError::Store`] when the journal directory is
    /// not creatable; [`crate::dispatch::DispatchError::ProfileFailed`] when
    /// the factory profile does not validate.
    pub fn open(state_dir: &Path) -> Result<Self, DispatchError> {
        let log = TaskLog::open(state_dir).map_err(|_| DispatchError::Store)?;
        let profile =
            load_builtin(DISPATCH_PROFILE_NAME).map_err(|_| DispatchError::ProfileFailed)?;
        Ok(Self {
            log,
            profile,
            profile_name: DISPATCH_PROFILE_NAME,
            audits: Vec::new(),
            snapshots: HashMap::new(),
            scores: Vec::new(),
            renders: HashMap::new(),
        })
    }

    /// Factory profile in force (whitelist source for plan validation).
    #[must_use]
    pub fn profile_name(&self) -> &'static str {
        self.profile_name
    }

    /// Audit hops recorded so far, in handling order.
    #[must_use]
    pub fn audits(&self) -> &[DispatchAudit] {
        &self.audits
    }

    /// Audit hops for one chain task id, in handling order.
    #[must_use]
    pub fn audits_for(&self, task_id: &str) -> Vec<&DispatchAudit> {
        self.audits
            .iter()
            .filter(|audit| audit.task_id == task_id)
            .collect()
    }

    /// Snapshots stored so far.
    #[must_use]
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Score reports stored so far.
    #[must_use]
    pub fn score_count(&self) -> usize {
        self.scores.len()
    }

    /// Render instructions still pending bridge execution.
    #[must_use]
    pub fn render_pending_count(&self) -> usize {
        self.renders.len()
    }

    /// Decide the reply for one frame-read outcome (mirrors
    /// [`crate::daemon::decide`] error semantics: unknown wire types and
    /// malformed frames get an `error` reply with the connection kept open;
    /// only transport-level failures close).
    pub fn dispatch_outcome(&mut self, outcome: &Result<WireFrame, IpcError>) -> StepOutcome {
        match outcome {
            Ok(frame) => self.dispatch_frame(frame),
            Err(err) => match err {
                IpcError::TransportClosed
                | IpcError::Truncated { .. }
                | IpcError::Timeout
                | IpcError::Io(_) => StepOutcome::Close,
                IpcError::FrameTooLarge { .. } => StepOutcome::CloseWithReply(
                    MessageType::Error,
                    error_body(ErrorCode::FrameTooLarge, "frame exceeds size cap"),
                ),
                IpcError::VersionMismatch { .. } => StepOutcome::Reply(
                    MessageType::Error,
                    error_body(
                        ErrorCode::VersionMismatch,
                        "protocol major version mismatch",
                    ),
                ),
                IpcError::Json(_) | IpcError::Protocol(_) => StepOutcome::Reply(
                    MessageType::Error,
                    error_body(ErrorCode::ProtocolViolation, "malformed frame"),
                ),
                IpcError::Peer { code, .. } => {
                    StepOutcome::Reply(MessageType::Error, error_body(*code, "peer error echoed"))
                }
            },
        }
    }

    /// Dispatch one parsed frame to its handler.
    pub fn dispatch_frame(&mut self, frame: &WireFrame) -> StepOutcome {
        if frame.kind == MessageType::Error {
            return StepOutcome::Silent;
        }
        if frame.kind == MessageType::Hello {
            return StepOutcome::Reply(MessageType::Hello, hello_body());
        }
        let reply: Result<(MessageType, Value), DispatchError> = match frame.kind {
            MessageType::SnapshotSubmit => self.on_snapshot_submit(&frame.body),
            MessageType::PlanRequest => self.on_plan_request(&frame.body),
            MessageType::PatchApply => self.on_patch_apply(&frame.body),
            MessageType::RenderRequest => self.on_render_request(&frame.body),
            MessageType::RenderResult => self.on_render_result(&frame.body),
            MessageType::ScoreReport => self.on_score_report(&frame.body),
            MessageType::ConsentGet => Ok((
                MessageType::ConsentGet,
                serde_json::json!({"tier": "tier3", "dispatched": true}),
            )),
            MessageType::ConsentSet => self.on_consent_set(&frame.body),
            MessageType::AuditEvent => Ok((
                MessageType::AuditEvent,
                serde_json::json!({"ok": true, "noted": true}),
            )),
            MessageType::PlanResponse | MessageType::PatchResult => {
                Ok((frame.kind, serde_json::json!({"ok": true, "noted": true})))
            }
            MessageType::Hello | MessageType::Error => {
                return StepOutcome::Silent;
            }
        };
        match reply {
            Ok((kind, body)) => StepOutcome::Reply(kind, body),
            Err(err) => {
                StepOutcome::Reply(MessageType::Error, error_body(err.code(), &err.detail()))
            }
        }
    }

    /// `snapshot.submit`: record the snapshot, journal its task, ack via
    /// `audit.event`.
    fn on_snapshot_submit(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let task_id = get_id(body, "task_id")?;
        let snapshot_id = get_id(body, "snapshot_id")?;
        let take_guid = get_id(body, "take_guid")?;
        let params_len = body.get("params_len").and_then(Value::as_u64).unwrap_or(0);
        let chunk_bytes = body.get("chunk_bytes").and_then(Value::as_u64).unwrap_or(0);
        ensure_running(&mut self.log, &snapshot_id, "snapshot")?;
        self.push_audit(
            &task_id,
            "snapshot.submit",
            DISPATCH_MOCK_MODEL,
            ConsentTier::Tier3,
            vec!["meta".to_owned()],
            chunk_bytes,
        );
        self.snapshots.insert(
            snapshot_id.clone(),
            SnapshotRecord {
                task_id: task_id.clone(),
                snapshot_id: snapshot_id.clone(),
                take_guid,
                params_len,
                chunk_bytes,
            },
        );
        finish(&mut self.log, &snapshot_id, true)?;
        Ok((
            MessageType::AuditEvent,
            serde_json::json!({
                "ok": true, "stored": true, "task_id": task_id,
                "kind": "snapshot", "snapshot_id": snapshot_id,
            }),
        ))
    }

    /// `plan.request`: run the offline planner chain and answer `plan.response`.
    ///
    /// Backend selection is explicit via the optional wire `"backend"` field
    /// (absent means seeded). The serving path honors only
    /// [`ModelBackend::MockSeeded`](synthlm_planner::planning::ModelBackend);
    /// `"live-tier2"` is BLOCKED by design (see
    /// [`crate::dispatch::DispatchError::UnsupportedBackend`]).
    fn on_plan_request(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let task_id = get_id(body, "task_id")?;
        let snapshot_id = get_id(body, "snapshot_id")?;
        if !self.snapshots.contains_key(&snapshot_id) {
            return Err(DispatchError::UnknownTask);
        }
        let fields = get_fields(body)?;
        validate_upload_fields(&fields).map_err(|_| DispatchError::Whitelist)?;
        let byte_count = body.get("byte_count").and_then(Value::as_u64).unwrap_or(0);
        let intent_text = get_optional_string(body, "intent")?;
        let intent = intent_text.as_deref().unwrap_or("");
        if parse_backend(body)? == BackendSel::Live {
            return Err(DispatchError::UnsupportedBackend);
        }

        // 1. Seeded plan + candidates (deterministic; no transport, no
        // network). The backend is passed explicitly by this caller; the
        // planning layer stamps the watermark that the reply echoes.
        let outcome = plan_for_intent(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &self.profile,
            intent,
        )
        .map_err(|err| match err {
            PlanningError::Gateway(inner) => DispatchError::Gateway { code: inner.code() },
            PlanningError::Unrepairable { code } => DispatchError::PatchResidual { code },
            PlanningError::EmptyPlan => DispatchError::PlanEmpty,
            PlanningError::NoLiveModel | PlanningError::Unparsable | PlanningError::ChainFailed => {
                DispatchError::ChainFailed
            }
        })?;
        if outcome.plan().ops.is_empty() {
            return Err(DispatchError::PlanEmpty);
        }

        // 2. Mock-objective search (genuine optimizer run, synthetic bowl).
        let seed = fnv1a(&task_id);
        let space = SearchSpace::new(vec![(0.0, 1.0), (0.0, 1.0)], Vec::new())
            .map_err(|_| DispatchError::ChainFailed)?;
        let config = SearchConfig::new(12, 8, seed);
        let mut bowl = MockBowl::new(vec![0.6, 0.4], Vec::new(), 1.0, 0.0, seed);
        let search =
            two_stage_search(&mut bowl, &space, &config).map_err(|_| DispatchError::ChainFailed)?;

        // 3. Offline model-gateway leg (MockTransport: no network, Tier3 local).
        let decided_at = i64::try_from(now_ms().saturating_div(1000)).unwrap_or(0);
        let state = ConsentState::Decided(ConsentStore {
            tier: ConsentTier::Tier3,
            decided_at_unix: decided_at,
            version: CONFIG_VERSION,
        });
        let mut gateway = Gateway::new(3, 30_000);
        let mut transport = MockTransport::all_ok();
        let route = gateway
            .route(
                &state,
                RouteParams {
                    env_override_raw: None,
                    fields: fields.clone(),
                    byte_count,
                    cloud_key_present: false,
                    now_ms: now_ms(),
                },
                &mut transport,
            )
            .map_err(|err| DispatchError::Gateway { code: err.code() })?;
        for event in &route.audits {
            self.audits.push(DispatchAudit {
                task_id: task_id.clone(),
                stage: "plan.request",
                event: event.clone(),
            });
        }

        // 4. DEC-004 binding: re-stem the surviving plan onto the frozen
        // snapshot id (ops untouched, so validation still holds) and re-issue
        // candidates under this task's namespace.
        let mut plan = outcome.plan().clone();
        plan.target_snapshot = snapshot_id.clone();
        let changed = count_changed_ops(&plan);
        let mut candidates: Vec<Value> = Vec::with_capacity(outcome.candidates().len());
        for (index, candidate) in outcome.candidates().iter().enumerate() {
            let rebuilt = Candidate::new(
                format!("{task_id}-c{}", index + 1),
                plan.clone(),
                candidate.diff_summary_zh().to_owned(),
                candidate.confidence(),
                candidate.delta_lufs(),
                changed,
                candidate.score_snapshot(),
            )
            .map_err(|_| DispatchError::ChainFailed)?;
            candidates.push(serde_json::json!({
                "id": rebuilt.id(),
                "diff": rebuilt.diff_summary_zh(),
                "confidence": rebuilt.confidence(),
                "delta_lufs": rebuilt.delta_lufs(),
            }));
        }
        if candidates.is_empty() {
            return Err(DispatchError::PlanEmpty);
        }

        ensure_running(&mut self.log, &plan_task_id(&task_id), "plan")?;
        finish(&mut self.log, &plan_task_id(&task_id), true)?;

        let ops_value = serde_json::to_value(&plan.ops).map_err(|_| DispatchError::ChainFailed)?;
        let audits =
            audits_json(&self.audits_for(&task_id)).map_err(|_| DispatchError::ChainFailed)?;
        let best_lane = search.best.continuous.clone();
        Ok((
            MessageType::PlanResponse,
            serde_json::json!({
                "task_id": task_id,
                "target_snapshot": snapshot_id,
                "ops": ops_value,
                "candidates": candidates,
                "direction_gap": outcome.direction_gap(),
                "search": {
                    "evals_used": search.evals_used,
                    "coarse_value": search.coarse_value,
                    "best": best_lane,
                },
                "backend": outcome.backend_label(),
                "mock": {
                    "transport": "MockTransport",
                    "network": "none",
                    "search": "MockBowl",
                    "profile": self.profile_name,
                },
                "audits": audits,
            }),
        ))
    }

    /// `patch.apply`: re-validate the plan (dry-run, no DAW writes).
    fn on_patch_apply(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let task_id = get_id(body, "task_id")?;
        let target_snapshot = get_id(body, "target_snapshot")?;
        if !self.snapshots.contains_key(&target_snapshot) {
            return Err(DispatchError::UnknownTask);
        }
        let raw_ops = body.get("ops").ok_or(DispatchError::MissingField("ops"))?;
        let items = raw_ops
            .as_array()
            .ok_or(DispatchError::BadJson("ops array"))?;
        if items.is_empty() {
            return Err(DispatchError::BadJson("ops array"));
        }
        let mut ops = Vec::with_capacity(items.len());
        for item in items {
            let op: PatchOp = serde_json::from_value(item.clone())
                .map_err(|_| DispatchError::BadJson("patch op"))?;
            ops.push(op);
        }
        let incoming = PatchPlan {
            ops,
            target_snapshot: target_snapshot.clone(),
        };
        let outcome = validate_with_repair(&incoming, &self.profile, DEFAULT_MAX_REPAIR_ROUNDS);
        if !outcome.accepted() {
            let code = outcome
                .residual_errors
                .first()
                .map_or(ErrorCode::Internal, |err| err.code());
            return Err(DispatchError::PatchResidual { code });
        }
        let removed_total: usize = outcome
            .reports
            .iter()
            .map(|report| report.removed.len())
            .sum();
        let replaced_total: usize = outcome
            .reports
            .iter()
            .map(|report| report.replaced.len())
            .sum();
        let applied = u64::try_from(outcome.plan.ops.len()).unwrap_or(u64::MAX);
        let ops_bytes = u64::try_from(
            serde_json::to_string(&outcome.plan.ops)
                .map_err(|_| DispatchError::ChainFailed)?
                .len(),
        )
        .unwrap_or(u64::MAX);
        ensure_running(&mut self.log, &patch_task_id(&task_id), "patch")?;
        self.push_audit(
            &task_id,
            "patch.apply",
            DISPATCH_MOCK_MODEL,
            ConsentTier::Tier3,
            vec!["meta".to_owned()],
            ops_bytes,
        );
        finish(&mut self.log, &patch_task_id(&task_id), true)?;
        Ok((
            MessageType::PatchResult,
            serde_json::json!({
                "ok": true, "task_id": task_id, "target_snapshot": target_snapshot,
                "applied": applied, "dry_run": true,
                "removed_total": removed_total, "replaced_total": replaced_total,
                "rounds_used": outcome.rounds_used(),
            }),
        ))
    }

    /// `render.request`: journal a pending render and answer with the
    /// bridge-executed instruction frame (never render here).
    fn on_render_request(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let task_id = get_id(body, "task_id")?;
        let candidate_id = get_id(body, "candidate_id")?;
        let plan_key = plan_task_id(&task_id);
        let plan_done = self
            .log
            .get(&plan_key)
            .is_some_and(|task| task.state == TaskState::Done);
        if !plan_done {
            return Err(DispatchError::UnknownTask);
        }
        let render_key = render_task_id(&task_id, &candidate_id);
        ensure_running(&mut self.log, &render_key, "render")?;
        self.push_audit(
            &task_id,
            "render.request",
            DISPATCH_MOCK_MODEL,
            ConsentTier::Tier3,
            vec!["meta".to_owned()],
            0,
        );
        self.renders.insert(
            render_key,
            RenderInstruction {
                task_id: task_id.clone(),
                candidate_id: candidate_id.clone(),
                executor: RENDER_EXECUTOR,
                status: RENDER_STATUS_PENDING,
            },
        );
        Ok((
            MessageType::RenderRequest,
            serde_json::json!({
                "task_id": task_id,
                "candidate_id": candidate_id,
                "executor": RENDER_EXECUTOR,
                "status": RENDER_STATUS_PENDING,
                "instruction": "REAPER-side render via the fixed-block path (Main_OnCommand 42230); acrd orchestrates only and never waits on audio",
            }),
        ))
    }

    /// `render.result`: close the pending render task.
    fn on_render_result(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let task_id = get_id(body, "task_id")?;
        let ok = body
            .get("ok")
            .and_then(Value::as_bool)
            .ok_or(DispatchError::BadJson("ok flag"))?;
        let prefix = format!("{task_id}:render:");
        let mut target: Option<String> = None;
        for key in self.renders.keys() {
            if key.starts_with(prefix.as_str()) {
                target = Some(key.clone());
                break;
            }
        }
        let key = target.ok_or(DispatchError::UnknownTask)?;
        finish(&mut self.log, &key, ok)?;
        self.renders.remove(&key);
        self.push_audit(
            &task_id,
            "render.result",
            DISPATCH_MOCK_MODEL,
            ConsentTier::Tier3,
            vec!["meta".to_owned()],
            0,
        );
        Ok((
            MessageType::AuditEvent,
            serde_json::json!({
                "ok": true, "stored": true, "task_id": task_id,
                "kind": "render_result", "render_ok": ok,
            }),
        ))
    }

    /// `score.report`: validate finite fields, rank via `synthlm-eval`, store.
    fn on_score_report(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let task_id = get_id(body, "task_id")?;
        let spec_l1 = get_finite(body, "spec_l1")?;
        let mel_l1 = get_finite(body, "mel_l1")?;
        let mel_low = get_finite(body, "mel_low")?;
        let mel_mid = get_finite(body, "mel_mid")?;
        let mel_high = get_finite(body, "mel_high")?;
        let transient_f1 = get_finite(body, "transient_f1")?;
        let lufs_i = get_finite(body, "lufs_i")?;
        let true_peak = get_finite(body, "true_peak")?;
        let delta_lufs = get_finite(body, "delta_lufs")?;
        let weights = BandWeights::defaults();
        let score = Score {
            spec_l1: spec_l1 as f32,
            mel_l1: mel_l1 as f32,
            mel_low: mel_low as f32,
            mel_mid: mel_mid as f32,
            mel_high: mel_high as f32,
            mel_weighted: weights.apply(mel_low as f32, mel_mid as f32, mel_high as f32),
            clap_cos: None,
            transient_f1: transient_f1 as f32,
            lufs_i,
            true_peak,
            delta_lufs,
        };
        let weighted = f64::from(score.mel_weighted_with(&weights));
        let alarm = score.true_peak_alarm();
        ensure_running(&mut self.log, &score_task_id(&task_id), "score")?;
        self.push_audit(
            &task_id,
            "score.report",
            DISPATCH_MOCK_MODEL,
            ConsentTier::Tier3,
            vec!["mir".to_owned(), "meta".to_owned()],
            0,
        );
        self.scores.push(ScoreRecord {
            task_id: task_id.clone(),
            spec_l1,
            mel_l1,
            mel_low,
            mel_mid,
            mel_high,
            mel_weighted: weighted,
            transient_f1,
            lufs_i,
            true_peak,
            delta_lufs,
            alarm,
        });
        finish(&mut self.log, &score_task_id(&task_id), true)?;
        Ok((
            MessageType::ScoreReport,
            serde_json::json!({
                "ok": true, "stored": true, "task_id": task_id,
                "mel_weighted": weighted, "true_peak_alarm": alarm,
            }),
        ))
    }

    /// `consent.set`: accept a known tier spelling (the store itself lives
    /// outside this mock chain; this only validates the spelling).
    fn on_consent_set(&mut self, body: &Value) -> Result<(MessageType, Value), DispatchError> {
        let tier = body
            .get("tier")
            .and_then(Value::as_str)
            .ok_or(DispatchError::MissingField("tier"))?;
        match tier {
            "tier1" | "tier2" | "tier3" => Ok((
                MessageType::ConsentSet,
                serde_json::json!({"ok": true, "tier": tier, "dispatched": true}),
            )),
            _ => Err(DispatchError::BadJson("tier spelling")),
        }
    }

    /// Record one audit hop for `task_id`.
    fn push_audit(
        &mut self,
        task_id: &str,
        stage: &'static str,
        model: &str,
        tier: ConsentTier,
        fields: Vec<String>,
        byte_count: u64,
    ) {
        self.audits.push(DispatchAudit {
            task_id: task_id.to_owned(),
            stage,
            event: AuditEvent::now(model, tier, fields, byte_count),
        });
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Journal task id for the plan stage of `task_id`.
fn plan_task_id(task_id: &str) -> String {
    format!("{task_id}:plan")
}

/// Journal task id for the patch stage of `task_id`.
fn patch_task_id(task_id: &str) -> String {
    format!("{task_id}:patch")
}

/// Journal task id for one render of `candidate_id` under `task_id`.
fn render_task_id(task_id: &str, candidate_id: &str) -> String {
    format!("{task_id}:render:{candidate_id}")
}

/// Journal task id for the score stage of `task_id`.
fn score_task_id(task_id: &str) -> String {
    format!("{task_id}:score")
}

/// Read a required non-empty string field.
fn get_id(body: &Value, key: &'static str) -> Result<String, DispatchError> {
    let raw = body
        .get(key)
        .and_then(Value::as_str)
        .ok_or(DispatchError::MissingField(key))?;
    if raw.is_empty() {
        return Err(DispatchError::EmptyField(key));
    }
    Ok(raw.to_owned())
}

/// Read a required finite number field.
fn get_finite(body: &Value, key: &'static str) -> Result<f64, DispatchError> {
    let value = body
        .get(key)
        .and_then(Value::as_f64)
        .ok_or(DispatchError::BadScore)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(DispatchError::BadScore)
    }
}

/// Read the upload field list (defaults to [`crate::dispatch::DEFAULT_PLAN_FIELDS`]).
fn get_fields(body: &Value) -> Result<Vec<String>, DispatchError> {
    match body.get("fields") {
        None => Ok(DEFAULT_PLAN_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect()),
        Some(Value::Null) => Ok(DEFAULT_PLAN_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect()),
        Some(list) => {
            let items = list
                .as_array()
                .ok_or(DispatchError::BadJson("fields array"))?;
            let mut fields = Vec::with_capacity(items.len());
            for item in items {
                let field = item
                    .as_str()
                    .ok_or(DispatchError::BadJson("fields array"))?;
                fields.push(field.to_owned());
            }
            Ok(fields)
        }
    }
}

/// Deterministic FNV-1a seed from a task id (mock search replays by task).
fn fnv1a(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Backend selection for `plan.request` (explicit per call; absent means
/// seeded). The serving path honors only [`BackendSel::Mock`]; live is
/// BLOCKED by design, never triggered remotely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendSel {
    /// Deterministic seeded candidates (offline).
    Mock,
    /// Live Tier2 text path (refused over IPC).
    Live,
}

/// Read the optional wire `"backend"` selection.
///
/// Absent/`null` means [`BackendSel::Mock`]; known seeded spellings stay
/// mock; known live spellings select [`BackendSel::Live`] (refused later
/// with [`crate::dispatch::DispatchError::UnsupportedBackend`]); anything
/// else is malformed.
fn parse_backend(body: &Value) -> Result<BackendSel, DispatchError> {
    match body.get("backend") {
        None | Some(Value::Null) => Ok(BackendSel::Mock),
        Some(Value::String(name)) => match name.as_str() {
            "mock-seeded" | "mock" | "seeded-demo" => Ok(BackendSel::Mock),
            "live-tier2" | "live" => Ok(BackendSel::Live),
            _ => Err(DispatchError::BadJson("backend")),
        },
        Some(_) => Err(DispatchError::BadJson("backend")),
    }
}

/// Read an optional string field (`None` when absent or `null`).
fn get_optional_string(body: &Value, key: &'static str) -> Result<Option<String>, DispatchError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(DispatchError::BadJson(key)),
    }
}

/// Render audit hops as wire JSON (task correlation envelope around the
/// five-field events).
fn audits_json(audits: &[&DispatchAudit]) -> Result<Value, serde_json::Error> {
    let mut items = Vec::with_capacity(audits.len());
    for audit in audits {
        items.push(serde_json::json!({
            "task_id": audit.task_id,
            "stage": audit.stage,
            "event": serde_json::to_value(&audit.event)?,
        }));
    }
    Ok(Value::Array(items))
}

/// Open `id` as a running journal task (idempotent across `Pending`,
/// `Running`, and failed-then-retried states; completed stages refuse).
fn ensure_running(log: &mut TaskLog, id: &str, kind: &str) -> Result<(), DispatchError> {
    let state = log.get(id).map(|task| task.state);
    match state {
        None => {
            log.create_task(id.to_owned(), kind.to_owned())
                .map_err(|_| DispatchError::Store)?;
            log.transition(id, TaskState::Running)
                .map_err(|_| DispatchError::Store)?;
            Ok(())
        }
        Some(TaskState::Pending) => log
            .transition(id, TaskState::Running)
            .map_err(|_| DispatchError::Store),
        Some(TaskState::Running) => Ok(()),
        Some(TaskState::Failed) => {
            log.transition(id, TaskState::Pending)
                .map_err(|_| DispatchError::Store)?;
            log.transition(id, TaskState::Running)
                .map_err(|_| DispatchError::Store)?;
            Ok(())
        }
        Some(TaskState::Done) => Err(DispatchError::DuplicateTask),
    }
}

/// Close a running journal task (`Done` on success, `Failed` otherwise).
fn finish(log: &mut TaskLog, id: &str, ok: bool) -> Result<(), DispatchError> {
    let next = if ok {
        TaskState::Done
    } else {
        TaskState::Failed
    };
    log.transition(id, next).map_err(|_| DispatchError::Store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use synthlm_common::ipc::{EndpointRole, client_handshake, current_version};

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "synthlm-acrd-dispatch-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn open_test(tag: &str) -> (Dispatcher, std::path::PathBuf) {
        let dir = scratch_dir(tag);
        let dispatcher = Dispatcher::open(&dir).expect("open dispatcher");
        (dispatcher, dir)
    }

    fn submit_snapshot(dispatcher: &mut Dispatcher, task: &str, snap: &str) -> Value {
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::SnapshotSubmit,
            body: serde_json::json!({
                "task_id": task, "snapshot_id": snap,
                "take_guid": "{take-1}", "params_len": 2, "chunk_bytes": 64,
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::AuditEvent);
                body
            }
            other => panic!("snapshot must ack, got {other:?}"),
        }
    }

    fn request_plan(dispatcher: &mut Dispatcher, task: &str, snap: &str) -> Value {
        request_plan_with(dispatcher, task, snap, None)
    }

    fn request_plan_with(
        dispatcher: &mut Dispatcher,
        task: &str,
        snap: &str,
        intent: Option<&str>,
    ) -> Value {
        let mut body = serde_json::json!({"task_id": task, "snapshot_id": snap});
        if let Some(text) = intent {
            body["intent"] = serde_json::json!(text);
        }
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::PlanRequest,
            body,
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::PlanResponse);
                body
            }
            other => panic!("plan must answer, got {other:?}"),
        }
    }

    #[test]
    fn full_chain_snapshot_plan_patch_render_score() {
        let (mut dispatcher, dir) = open_test("chain");
        assert_eq!(dispatcher.profile_name(), "reaeq");
        let ack = submit_snapshot(&mut dispatcher, "chain-1", "snap-chain-1");
        assert_eq!(ack.get("stored").and_then(Value::as_bool), Some(true));
        assert_eq!(dispatcher.snapshot_count(), 1);

        let plan = request_plan(&mut dispatcher, "chain-1", "snap-chain-1");
        assert_eq!(
            plan.get("mock")
                .and_then(|mock| mock.get("transport"))
                .and_then(Value::as_str),
            Some("MockTransport")
        );
        assert_eq!(
            plan.get("mock")
                .and_then(|mock| mock.get("network"))
                .and_then(Value::as_str),
            Some("none")
        );
        let candidates = plan
            .get("candidates")
            .and_then(Value::as_array)
            .expect("candidates");
        assert!((3..=5).contains(&candidates.len()), "DEC-018 shortlist");
        assert_eq!(
            plan.get("direction_gap").and_then(Value::as_bool),
            Some(false)
        );
        let ops = plan.get("ops").and_then(Value::as_array).expect("ops");
        assert_eq!(ops.len(), 3);

        // patch.apply replays the answered ops (dry-run: no DAW touched).
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::PatchApply,
            body: serde_json::json!({
                "task_id": "chain-1", "target_snapshot": "snap-chain-1", "ops": ops,
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::PatchResult);
                assert_eq!(body.get("applied").and_then(Value::as_u64), Some(3));
                assert_eq!(body.get("dry_run").and_then(Value::as_bool), Some(true));
            }
            other => panic!("patch must answer, got {other:?}"),
        }

        // render.request answers with a bridge-executed instruction (pending).
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::RenderRequest,
            body: serde_json::json!({"task_id": "chain-1", "candidate_id": "chain-1-c1"}),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::RenderRequest);
                assert_eq!(
                    body.get("executor").and_then(Value::as_str),
                    Some("bridge/reaper")
                );
                assert_eq!(
                    body.get("status").and_then(Value::as_str),
                    Some("pending_bridge")
                );
            }
            other => panic!("render must instruct, got {other:?}"),
        }
        assert_eq!(dispatcher.render_pending_count(), 1);

        // render.result closes the pending render.
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::RenderResult,
            body: serde_json::json!({"task_id": "chain-1", "ok": true}),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, _) => assert_eq!(kind, MessageType::AuditEvent),
            other => panic!("render result must ack, got {other:?}"),
        }
        assert_eq!(dispatcher.render_pending_count(), 0);

        // score.report stores the eval-ranked report.
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::ScoreReport,
            body: serde_json::json!({
                "task_id": "chain-1", "spec_l1": 0.1, "mel_l1": 0.2,
                "mel_low": 0.05, "mel_mid": 0.25, "mel_high": 0.3,
                "transient_f1": 0.9, "lufs_i": -14.0,
                "true_peak": -8.0, "delta_lufs": 1.5,
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::ScoreReport);
                let weighted = body
                    .get("mel_weighted")
                    .and_then(Value::as_f64)
                    .expect("weighted");
                let expected = f64::from(BandWeights::defaults().apply(0.05, 0.25, 0.3));
                assert!((weighted - expected).abs() < 1e-6);
                assert_eq!(
                    body.get("true_peak_alarm").and_then(Value::as_bool),
                    Some(false)
                );
            }
            other => panic!("score must store, got {other:?}"),
        }
        assert_eq!(dispatcher.score_count(), 1);

        // Every hop audited under the same task id.
        let hops = dispatcher.audits_for("chain-1");
        assert!(hops.len() >= 5, "each stage audits, got {}", hops.len());
        let stages: Vec<&str> = hops.iter().map(|audit| audit.stage).collect();
        for stage in [
            "snapshot.submit",
            "plan.request",
            "patch.apply",
            "render.request",
            "score.report",
        ] {
            assert!(stages.contains(&stage), "missing audit hop {stage}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hot_peak_trips_the_true_peak_alarm() {
        let (mut dispatcher, dir) = open_test("alarm");
        submit_snapshot(&mut dispatcher, "alarm-1", "snap-alarm-1");
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::ScoreReport,
            body: serde_json::json!({
                "task_id": "alarm-1", "spec_l1": 0.4, "mel_l1": 0.5,
                "mel_low": 0.4, "mel_mid": 0.5, "mel_high": 0.6,
                "transient_f1": 0.7, "lufs_i": -14.0,
                "true_peak": -0.5, "delta_lufs": 6.0,
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(_, body) => {
                assert_eq!(
                    body.get("true_peak_alarm").and_then(Value::as_bool),
                    Some(true)
                );
            }
            other => panic!("score must store, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn error_frames_map_to_wire_errors_without_closing() {
        let (mut dispatcher, dir) = open_test("errors");

        // Plan before snapshot: unknown task, BLOCKED protocol violation.
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::PlanRequest,
            body: serde_json::json!({"task_id": "nope", "snapshot_id": "snap-nope"}),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(Value::as_str),
                    Some("protocol_violation")
                );
                assert_eq!(body.get("retryable").and_then(Value::as_bool), Some(false));
            }
            other => panic!("must error, got {other:?}"),
        }

        // Whitelist violation: PCM-ish field rejected before anything runs.
        submit_snapshot(&mut dispatcher, "wl-1", "snap-wl-1");
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::PlanRequest,
            body: serde_json::json!({
                "task_id": "wl-1", "snapshot_id": "snap-wl-1", "fields": ["pcm"],
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(Value::as_str),
                    Some("whitelist_violation")
                );
                assert_eq!(body.get("retryable").and_then(Value::as_bool), Some(false));
            }
            other => panic!("must whitelist-block, got {other:?}"),
        }

        // Bare-index op is repaired away (migration-removal branch), not fatal.
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::PatchApply,
            body: serde_json::json!({
                "task_id": "wl-1", "target_snapshot": "snap-wl-1",
                "ops": [
                    {"op": "replace", "path": "param/4", "value": 0.5},
                    {"op": "replace", "path": "param/4:_Gain_Band_2", "value": 0.5},
                ],
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::PatchResult);
                assert_eq!(body.get("applied").and_then(Value::as_u64), Some(1));
                assert_eq!(body.get("removed_total").and_then(Value::as_u64), Some(1));
            }
            other => panic!("bare index must repair, got {other:?}"),
        }

        // Non-finite score is refused.
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::ScoreReport,
            body: serde_json::json!({
                "task_id": "wl-1", "spec_l1": 0.1, "mel_l1": 0.2,
                "mel_low": 0.05, "mel_mid": 0.25, "mel_high": 0.3,
                "transient_f1": 0.9, "lufs_i": -14.0,
                "true_peak": -8.0,
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(Value::as_str),
                    Some("protocol_violation")
                );
            }
            other => panic!("missing score field must error, got {other:?}"),
        }

        // Unknown wire types fail parsing upstream and still get an error
        // reply with the connection kept open (existing daemon semantics).
        let payload = format!(
            "{{\"ver\":{{\"major\":{},\"minor\":{}}},\"type\":\"no.such.type\",\"body\":{{}}}}",
            synthlm_common::ipc::PROTOCOL_MAJOR,
            synthlm_common::ipc::PROTOCOL_MINOR
        );
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(payload.as_bytes());
        let err = synthlm_common::ipc::decode_frame_bytes(&bytes).expect_err("unknown type fails");
        match dispatcher.dispatch_outcome(&Err(err)) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(Value::as_str),
                    Some("protocol_violation")
                );
            }
            other => panic!("unknown type must error-open, got {other:?}"),
        }
        // Inbound error frames stay silent (no ping-pong).
        let error_frame = WireFrame {
            ver: current_version(),
            kind: MessageType::Error,
            body: serde_json::json!({"code": "timeout", "retryable": true, "detail": "x"}),
        };
        assert!(matches!(
            dispatcher.dispatch_frame(&error_frame),
            StepOutcome::Silent
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatch_error_taxonomy_is_blocked_except_gateway_retry() {
        // Terminal dispatch failures are always BLOCKED.
        for err in [
            DispatchError::MissingField("task_id"),
            DispatchError::EmptyField("task_id"),
            DispatchError::BadJson("ops array"),
            DispatchError::Whitelist,
            DispatchError::PlanEmpty,
            DispatchError::ChainFailed,
            DispatchError::Store,
            DispatchError::BadScore,
            DispatchError::UnknownTask,
            DispatchError::DuplicateTask,
            DispatchError::ProfileFailed,
        ] {
            assert!(err.blocked(), "{err:?} must be BLOCKED");
            assert!(!err.retryable());
            assert!(!err.guidance().is_empty());
            assert!(!format!("{err}").is_empty());
        }
        // Residual/gateway verdicts reuse the carried wire code.
        let retry = DispatchError::Gateway {
            code: ErrorCode::CloudUnavailable,
        };
        assert!(retry.retryable());
        assert!(!retry.blocked());
        let blocked = DispatchError::PatchResidual {
            code: ErrorCode::WhitelistViolation,
        };
        assert!(blocked.blocked());
        assert!(DispatchError::UnsupportedBackend.blocked());
        assert!(!DispatchError::UnsupportedBackend.guidance().is_empty());
    }

    #[test]
    fn plan_response_carries_backend_and_mock_watermark() {
        let (mut dispatcher, dir) = open_test("backend");
        submit_snapshot(&mut dispatcher, "be-1", "snap-be-1");
        let plan = request_plan(&mut dispatcher, "be-1", "snap-be-1");
        let backend = plan
            .get("backend")
            .and_then(Value::as_str)
            .expect("backend field");
        assert_eq!(backend, "mock-seeded-demo");
        assert!(backend.contains("mock"), "mock marker unerased");
        // Legacy mock object is unchanged.
        assert_eq!(
            plan.get("mock")
                .and_then(|mock| mock.get("transport"))
                .and_then(Value::as_str),
            Some("MockTransport")
        );
        assert_eq!(
            plan.get("mock")
                .and_then(|mock| mock.get("network"))
                .and_then(Value::as_str),
            Some("none")
        );
        // Same intent on fresh tasks replays identical ops under
        // task-namespaced candidate ids.
        submit_snapshot(&mut dispatcher, "be-2", "snap-be-2");
        submit_snapshot(&mut dispatcher, "be-3", "snap-be-3");
        let second = request_plan_with(&mut dispatcher, "be-2", "snap-be-2", Some("same-intent"));
        let third = request_plan_with(&mut dispatcher, "be-3", "snap-be-3", Some("same-intent"));
        assert_eq!(second.get("ops"), third.get("ops"));
        assert_eq!(
            second.get("backend").and_then(Value::as_str),
            Some("mock-seeded-demo")
        );
        let ids = |body: &Value| {
            body.get("candidates")
                .and_then(Value::as_array)
                .expect("candidates")
                .iter()
                .map(|card| {
                    card.get("id")
                        .and_then(Value::as_str)
                        .expect("card id")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_ne!(ids(&second), ids(&third), "task-namespaced ids");
        assert!(
            ids(&second).iter().all(|id| id.starts_with("be-2-c")),
            "ids namespaced by task, got {:?}",
            ids(&second)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_backend_over_wire_is_blocked_offline() {
        let (mut dispatcher, dir) = open_test("live-blocked");
        submit_snapshot(&mut dispatcher, "lv-1", "snap-lv-1");
        for backend in ["live-tier2", "live"] {
            let frame = WireFrame {
                ver: current_version(),
                kind: MessageType::PlanRequest,
                body: serde_json::json!({
                    "task_id": "lv-1", "snapshot_id": "snap-lv-1", "backend": backend,
                }),
            };
            match dispatcher.dispatch_frame(&frame) {
                StepOutcome::Reply(kind, body) => {
                    assert_eq!(kind, MessageType::Error);
                    assert_eq!(
                        body.get("code").and_then(Value::as_str),
                        Some("protocol_violation")
                    );
                    assert_eq!(body.get("retryable").and_then(Value::as_bool), Some(false));
                    assert!(
                        body.get("detail")
                            .and_then(Value::as_str)
                            .is_some_and(|detail| !detail.contains("live-tier2 success")),
                        "never labeled live: {:?}",
                        body.get("detail")
                    );
                }
                other => panic!("live over wire must BLOCK, got {other:?}"),
            }
        }
        // Unknown backend spellings are malformed, not silently mocked.
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::PlanRequest,
            body: serde_json::json!({
                "task_id": "lv-1", "snapshot_id": "snap-lv-1", "backend": "tier9",
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(Value::as_str),
                    Some("protocol_violation")
                );
            }
            other => panic!("unknown backend must error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_chain_roundtrip_over_loopback_with_bridge_builders() {
        use std::net::{TcpListener, TcpStream};
        use synthlm_bridge::client::{
            build_patch_apply, build_plan_request, build_render_result, build_score_report,
            build_snapshot_submit,
        };
        use synthlm_bridge::snapshot::{ParamEntry, RenderBackup, Snapshot};
        use synthlm_common::ipc::bind_endpoint;

        // Prefer the interprocess bus when the platform names it; the TCP
        // loopback below is the test-only fallback carrying identical frames.
        let _ = bind_endpoint("synthlm-t502-probe-unused");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("loopback addr");
        let dir = scratch_dir("loopback");
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut dispatcher = Dispatcher::open(&dir).expect("open dispatcher");
            let peer = synthlm_common::ipc::server_handshake(&mut conn, EndpointRole::Acrd)
                .expect("hello");
            assert_eq!(peer.role, EndpointRole::Bridge);
            // Read until the client goes away: bounding the loop by a frame
            // count would race the client's last read against our socket
            // drop (RST on Windows aborts the client's pending read).
            let mut kinds = Vec::new();
            loop {
                let outcome = synthlm_common::ipc::read_frame(&mut conn);
                let clean = matches!(outcome, Err(IpcError::TransportClosed));
                match dispatcher.dispatch_outcome(&outcome) {
                    StepOutcome::Reply(kind, body) => {
                        kinds.push(kind);
                        if synthlm_common::ipc::write_frame(&mut conn, kind, &body).is_err() {
                            break;
                        }
                    }
                    StepOutcome::Silent => {}
                    StepOutcome::Close | StepOutcome::CloseWithReply(_, _) => {
                        assert!(clean);
                        break;
                    }
                }
            }
            kinds
        });

        let mut client = TcpStream::connect(addr).expect("connect");
        client_handshake(&mut client, EndpointRole::Bridge).expect("handshake");
        let snapshot = Snapshot {
            take_guid: "{take-loop}".to_owned(),
            params: vec![ParamEntry::new("cutoff".to_owned(), 0.5, 0.0, 1.0, 0.5).unwrap()],
            chunk: "chunk".to_owned(),
            take_fx_nch: None,
            midi_bytes: None,
            midi_hash: None,
            render: RenderBackup::default(),
        };
        let (kind, body) = build_snapshot_submit("loop-1", &snapshot).unwrap();
        synthlm_common::ipc::write_frame(&mut client, kind, &body).unwrap();
        let reply = synthlm_common::ipc::read_frame(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::AuditEvent);

        let (kind, body) = build_plan_request(
            "loop-1",
            "loop-1",
            &["prompt".to_owned(), "mir".to_owned()],
            64,
        )
        .unwrap();
        // NOTE: bridge names snapshot_id = task_id; retarget to the stored id.
        let mut planned = body;
        planned["snapshot_id"] = serde_json::json!("loop-1");
        synthlm_common::ipc::write_frame(&mut client, kind, &planned).unwrap();
        let reply = synthlm_common::ipc::read_frame(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::PlanResponse);
        // Wire-level audit chain: every hop audit carries the chain task id.
        let audits = reply
            .body
            .get("audits")
            .and_then(Value::as_array)
            .expect("audits");
        assert!(!audits.is_empty());
        for audit in audits {
            assert_eq!(audit.get("task_id").and_then(Value::as_str), Some("loop-1"));
        }
        let ops = reply.body.get("ops").cloned().expect("ops");

        let (kind, body) = build_patch_apply("loop-1", "loop-1", &ops).unwrap();
        synthlm_common::ipc::write_frame(&mut client, kind, &body).unwrap();
        let reply = synthlm_common::ipc::read_frame(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::PatchResult);
        assert_eq!(
            reply.body.get("dry_run").and_then(Value::as_bool),
            Some(true)
        );

        // render.request is orchestration-only: a bridge-executed instruction.
        synthlm_common::ipc::write_frame(
            &mut client,
            MessageType::RenderRequest,
            &serde_json::json!({"task_id": "loop-1", "candidate_id": "loop-1-c1"}),
        )
        .unwrap();
        let reply = synthlm_common::ipc::read_frame(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::RenderRequest);
        assert_eq!(
            reply.body.get("executor").and_then(Value::as_str),
            Some("bridge/reaper")
        );
        assert_eq!(
            reply.body.get("status").and_then(Value::as_str),
            Some("pending_bridge")
        );

        // Unknown wire type mid-chain: error frame, connection stays up.
        let payload = format!(
            "{{\"ver\":{{\"major\":{},\"minor\":{}}},\"type\":\"no.such.type\",\"body\":{{}}}}",
            synthlm_common::ipc::PROTOCOL_MAJOR,
            synthlm_common::ipc::PROTOCOL_MINOR
        );
        let mut raw = (payload.len() as u32).to_le_bytes().to_vec();
        raw.extend_from_slice(payload.as_bytes());
        {
            use std::io::Write as _;
            client.write_all(&raw).expect("send unknown");
            client.flush().expect("flush");
        }
        let reply = synthlm_common::ipc::read_frame(&mut client).expect("recv error");
        assert_eq!(reply.kind, MessageType::Error);

        // render.result closes the pending render task.
        let (kind, body) = build_render_result("loop-1", true).unwrap();
        synthlm_common::ipc::write_frame(&mut client, kind, &body).unwrap();
        let reply = synthlm_common::ipc::read_frame(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::AuditEvent);

        let scores = synthlm_bridge::client::ScoreFields {
            spec_l1: 0.1,
            mel_l1: 0.2,
            mel_low: 0.05,
            mel_mid: 0.25,
            mel_high: 0.3,
            transient_f1: 0.9,
            lufs_i: -14.0,
            true_peak: -8.0,
            delta_lufs: 1.5,
        };
        let (kind, body) = build_score_report("loop-1", &scores).unwrap();
        synthlm_common::ipc::write_frame(&mut client, kind, &body).unwrap();
        let reply = synthlm_common::ipc::read_frame(&mut client).unwrap();
        assert_eq!(reply.kind, MessageType::ScoreReport);
        drop(client);

        let kinds = server.join().expect("server thread");
        assert!(kinds.contains(&MessageType::AuditEvent));
        assert!(kinds.contains(&MessageType::PlanResponse));
        assert!(kinds.contains(&MessageType::PatchResult));
        assert!(kinds.contains(&MessageType::RenderRequest));
        assert!(kinds.contains(&MessageType::Error));
        assert!(kinds.contains(&MessageType::ScoreReport));
    }

    #[test]
    fn kill_mid_chain_replays_journal_and_dedups_resubmission() {
        // Kill -9 model: drop the dispatcher mid-chain (a render left Running),
        // reopen on the same state dir, and prove the journal replayed: the old
        // completed stages refuse duplicate resubmission while fresh ids run.
        // (Stage payloads — snapshot rows, score numbers — are resupplied by
        // the bridge on reconnect; the journal owns task states, and the
        // DuplicateTask refusal below is the proof it survived the kill.)
        let dir = scratch_dir("kill");
        {
            let mut dispatcher = Dispatcher::open(&dir).expect("open dispatcher");
            submit_snapshot(&mut dispatcher, "kill-1", "snap-kill-1");
            let plan = request_plan(&mut dispatcher, "kill-1", "snap-kill-1");
            assert!(
                plan.get("ops")
                    .and_then(Value::as_array)
                    .is_some_and(|ops| !ops.is_empty())
            );
            // Leave a render Running: the crash happens before render.result.
            let frame = WireFrame {
                ver: current_version(),
                kind: MessageType::RenderRequest,
                body: serde_json::json!({"task_id": "kill-1", "candidate_id": "kill-1-c1"}),
            };
            match dispatcher.dispatch_frame(&frame) {
                StepOutcome::Reply(kind, _) => assert_eq!(kind, MessageType::RenderRequest),
                other => panic!("render must instruct, got {other:?}"),
            }
            assert_eq!(dispatcher.render_pending_count(), 1);
            drop(dispatcher); // simulated kill: no flush ceremony exists
        }
        // Restart: the journal replays (completed snapshot/plan tasks are still
        // Done at the TaskLog layer, so resubmission dedups instead of doubling).
        let mut dispatcher = Dispatcher::open(&dir).expect("reopen after kill");
        let frame = WireFrame {
            ver: current_version(),
            kind: MessageType::SnapshotSubmit,
            body: serde_json::json!({
                "task_id": "kill-1", "snapshot_id": "snap-kill-1",
                "take_guid": "{take-1}", "params_len": 2, "chunk_bytes": 64,
            }),
        };
        match dispatcher.dispatch_frame(&frame) {
            StepOutcome::Reply(kind, body) => {
                assert_eq!(kind, MessageType::Error);
                assert_eq!(
                    body.get("code").and_then(Value::as_str),
                    Some("protocol_violation")
                );
                assert_eq!(body.get("retryable").and_then(Value::as_bool), Some(false));
            }
            other => panic!("completed resubmit must dedup-refuse, got {other:?}"),
        }
        // A fresh chain on the reopened store runs end to end: replay, not loss.
        let ack = submit_snapshot(&mut dispatcher, "kill-2", "snap-kill-2");
        assert_eq!(ack.get("stored").and_then(Value::as_bool), Some(true));
        let plan = request_plan(&mut dispatcher, "kill-2", "snap-kill-2");
        assert!(
            plan.get("candidates")
                .and_then(Value::as_array)
                .is_some_and(|cards| (3..=5).contains(&cards.len()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
