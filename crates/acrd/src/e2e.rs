//! Single-command M3 end-to-end orchestration (TSK-505).
//!
//! Turns one fixed intent (default `更亮的混音`) into three ReaEQ candidates
//! through the live Tier2 text path, double-checks the surviving patch,
//! diversifies to a 3-candidate shortlist with non-empty Chinese difference
//! sentences, and exports `e2e-plan.json` plus a self-contained
//! `e2e-apply.lua`. The Lua applier owns the whole REAPER side inline: build
//! a temp track with ReaEQ, resolve idents, apply each candidate in its own
//! undo block, render every applied state with `Main_OnCommand(42230)` over
//! fixed bounds (non-target tracks muted, `RENDER_*` backed up and restored),
//! record audio-byte FNV-1a hashes (the `data` chunk only: REAPER stamps a
//! wall-clock origination time into `bext`, so file-byte hashes can never
//! match), roll everything back in reverse, assert
//! parameter restoration plus a settled-state render null-test, delete
//! the temp track, and leave zero residue.
//!
//! Honesty and gate contract (no silent fallback):
//!
//! - The only model path is
//!   [`synthlm_planner::planning::ModelBackend::LiveTier2`] (consent gate ->
//!   whitelist gate -> key gate -> Tier2 chat). A missing stored consent, a
//!   non-Tier2 stored tier, or a missing cloud key is `BLOCKED` with
//!   guidance; the call never degrades to
//!   [`synthlm_planner::planning::ModelBackend::MockSeeded`]. The seeded path
//!   stays exclusive to the explicit `acrd demo` harness
//!   ([`crate::demo::run_demo`]).
//! - [`crate::e2e::run_e2e_with_backend`] is the testable core: it accepts any
//!   explicitly constructed backend, so unit tests drive determinism with
//!   [`synthlm_planner::planning::ModelBackend::MockSeeded`] and gate
//!   rejections with stub transports, touching no network, no key material,
//!   and no real consent file.
//! - [`crate::e2e::run_e2e_live`] is the CLI path: it loads the real stored
//!   consent and the real key presence, builds the real
//!   [`synthlm_planner::model_gw::HttpsTransport`], and requires a Tier2
//!   serving tier. Tier1 is refused up front so this command never places
//!   training-retention traffic while looking for a Tier2 result.
//! - Render execution stays REAPER-side Lua (L8): this module only formats
//!   text and writes files, exactly like the [`crate::demo`] harness.
//!
//! Blocking contract: [`crate::e2e::run_e2e_live`] performs synchronous model
//! transport through the gateway and may block the calling thread. Never call
//! it from an audio thread (AGENTS.md red line 2); it is a control-plane
//! helper.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use synthlm_common::config::{DEFAULT_BASE_URL, load_api_key};
use synthlm_common::consent::{ConsentState, load_consent};
use synthlm_common::ipc::{ConsentTier, now_ms};
use synthlm_planner::candidate::Candidate;
use synthlm_planner::model_gw::{Gateway, HttpsTransport, Transport};
use synthlm_planner::patch::{PatchPlan, validate};
use synthlm_planner::planning::{
    LIVE_BACKEND_LABEL, MOCK_BACKEND_LABEL, ModelBackend, plan_for_intent,
};
use synthlm_profile::schema::Profile;

/// Default output directory for `acrd e2e` (relative to the workspace root,
/// mirroring the `demo` harness default).
pub const DEFAULT_E2E_OUT_DIR: &str = "experiments/e2e";

/// Default fixed intent when `--intent` is omitted (brighter mix).
pub const DEFAULT_INTENT_ZH: &str = "更亮的混音";

/// Upload field names for the live leg (DEC-011 whitelist subset; names plus
/// a byte count only, never prompt text or PCM).
const E2E_UPLOAD_FIELDS: [&str; 3] = ["prompt", "mir", "meta"];

/// Shortlist size the e2e contract promises (matches DEC-018 default).
const E2E_SHORTLIST: usize = 3;

/// E2E orchestration failure.
///
/// Application-layer error type: human-readable and secret-free by
/// construction. Variants carry counts, indices, and static guidance only --
/// never intent text, key material, absolute paths, or model output
/// (AGENTS.md §8).
#[derive(Debug)]
pub enum E2eError {
    /// Command-line usage error (the binary prints usage, exit code 2).
    BadUsage(String),
    /// The ReaEQ builtin profile failed to load or validate.
    Profile(String),
    /// No stored Tier2 consent authorizes this call.
    Consent(String),
    /// No cloud API key is configured.
    Key(String),
    /// The stored tier is decided but is not the Tier2 this command needs.
    Tier(String),
    /// The planning leg failed (gateway BLOCKED verdict or unparsable /
    /// unrepairable model text; never labeled as a model result).
    Planning(String),
    /// The post-repair second validation still reports residuals.
    PatchResidual(String),
    /// Diversification did not yield exactly three survivors.
    CandidateShortfall(String),
    /// Three survivors do not cover all three directions.
    DirectionGap(String),
    /// A surviving candidate carries an empty difference sentence.
    EmptyDiff(String),
    /// A surviving op cannot be applied by the Lua applier (non-`param/`
    /// address or non-numeric value).
    NonNumericOp(String),
    /// A harness-internal invariant broke.
    Internal(String),
    /// Filesystem write failed.
    Io(String),
}

impl fmt::Display for E2eError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            E2eError::BadUsage(detail) => write!(f, "usage error: {detail}"),
            E2eError::Profile(detail) => write!(f, "profile error: {detail}"),
            E2eError::Consent(detail) => write!(f, "BLOCKED (consent): {detail}"),
            E2eError::Key(detail) => write!(f, "BLOCKED (key): {detail}"),
            E2eError::Tier(detail) => write!(f, "BLOCKED (tier): {detail}"),
            E2eError::Planning(detail) => write!(f, "planning failed: {detail}"),
            E2eError::PatchResidual(detail) => write!(f, "patch rejected: {detail}"),
            E2eError::CandidateShortfall(detail) => write!(f, "candidate shortfall: {detail}"),
            E2eError::DirectionGap(detail) => write!(f, "direction gap: {detail}"),
            E2eError::EmptyDiff(detail) => write!(f, "empty difference: {detail}"),
            E2eError::NonNumericOp(detail) => write!(f, "non-numeric op: {detail}"),
            E2eError::Internal(detail) => write!(f, "internal error: {detail}"),
            E2eError::Io(detail) => write!(f, "i/o error: {detail}"),
        }
    }
}

impl std::error::Error for E2eError {}

/// What one e2e invocation produced (paths plus the ranked order, for the
/// driver script assertions and the binary summary line).
#[derive(Clone, Debug)]
pub struct E2eSummary {
    /// Seed recorded in the plan and baked into the Lua header.
    pub seed: u64,
    /// Intent text the plan was solved for (kept in the plan artifact, never
    /// echoed into log lines; the Lua side only carries its hash).
    pub intent: String,
    /// Backend wire label that actually ran (`live-tier2` on the CLI path).
    pub backend: String,
    /// Serving model name, or the seeded no-model marker on mock runs.
    pub model: String,
    /// Output directory holding the artifacts.
    pub out_dir: PathBuf,
    /// Written plan artifact.
    pub plan_path: PathBuf,
    /// Written Lua applier artifact.
    pub lua_path: PathBuf,
    /// Candidate ids in diversified order.
    pub ranked_ids: Vec<String>,
    /// Repair rounds run across parsed plans.
    pub repair_rounds: usize,
    /// Total ops dropped by repair.
    pub removed_total: usize,
    /// Total ops value-substituted by repair.
    pub replaced_total: usize,
}

/// One diversified candidate as the Lua applier needs it.
#[derive(Clone, Debug)]
struct LuaCandidate {
    /// Stable id (undo-block label suffix and render-pattern stem).
    id: String,
    /// Audible-difference sentence, shown in comments only.
    diff_zh: String,
    /// `(ident without the param/ head, normalized value)` in order.
    params: Vec<(String, f64)>,
}

/// Run the e2e core over an explicitly constructed `backend`.
///
/// Pure orchestration except for the backend leg itself plus the two artifact
/// writes: plan with [`synthlm_planner::planning::plan_for_intent`], second
/// validation with [`synthlm_planner::patch::validate`], 3-candidate plus
/// direction-coverage plus non-empty-diff assertions, Lua-appliable op
/// projection, then `e2e-plan.json` + `e2e-apply.lua` into `out_dir` (created
/// when missing). Nothing is written when planning or any assertion fails.
///
/// # Errors
///
/// Returns [`crate::e2e::E2eError`] when the profile, the planning leg, the
/// second validation, the shortlist assertions, the op projection, or any
/// file write fails.
pub fn run_e2e_with_backend<T: Transport>(
    backend: ModelBackend<'_, T>,
    profile: &Profile,
    intent_text: &str,
    seed: u64,
    out_dir: &Path,
) -> Result<E2eSummary, E2eError> {
    let outcome = plan_for_intent(backend, profile, intent_text).map_err(|err| {
        let verdict = if err.clone().blocked() {
            "BLOCKED"
        } else {
            "RETRYABLE"
        };
        let guidance = err.clone().guidance();
        E2eError::Planning(format!("{verdict}: {err} | guidance: {guidance}"))
    })?;

    // Second check (双校验): the repaired plan must validate clean on its
    // own; only the residual class travels onward (value-free taxonomy code).
    let residuals = validate(outcome.plan(), profile);
    if !residuals.is_empty() {
        let first = residuals
            .first()
            .map_or(synthlm_common::ipc::ErrorCode::Internal, |err| err.code());
        return Err(E2eError::PatchResidual(format!(
            "second validation found {} residual(s); first: {first:?}",
            residuals.len()
        )));
    }

    if outcome.candidates().len() != E2E_SHORTLIST {
        return Err(E2eError::CandidateShortfall(format!(
            "expected {E2E_SHORTLIST} survivors, got {}",
            outcome.candidates().len()
        )));
    }
    if outcome.direction_gap() {
        return Err(E2eError::DirectionGap(
            "survivors do not cover darker/transient/spatial; refusing to guess".to_owned(),
        ));
    }

    let backend_label = outcome.backend_label();
    let mut ranked_ids = Vec::with_capacity(E2E_SHORTLIST);
    let mut ranked_json = Vec::with_capacity(E2E_SHORTLIST);
    let mut ranked_lua = Vec::with_capacity(E2E_SHORTLIST);
    let mut render_names = Vec::with_capacity(E2E_SHORTLIST + 4);
    render_names.push("e2e-render-baseline-a.wav".to_owned());
    render_names.push("e2e-render-baseline-b.wav".to_owned());
    for (index, candidate) in outcome.candidates().iter().enumerate() {
        if candidate.diff_summary_zh().trim().is_empty() {
            return Err(E2eError::EmptyDiff(format!(
                "candidate at rank {} carries no difference sentence",
                index + 1
            )));
        }
        let params = project_lua_params(candidate, index)?;
        ranked_ids.push(candidate.id().to_owned());
        render_names.push(format!("e2e-render-{}.wav", file_stem_of(candidate.id())));
        ranked_json.push(candidate_json(
            candidate,
            &params,
            direction_label(backend_label, candidate.id()),
            index,
            &render_names[render_names.len() - 1],
        ));
        ranked_lua.push(LuaCandidate {
            id: candidate.id().to_owned(),
            diff_zh: candidate.diff_summary_zh().to_owned(),
            params,
        });
    }
    render_names.push("e2e-render-restored-a.wav".to_owned());
    render_names.push("e2e-render-restored-b.wav".to_owned());

    let audits: Vec<serde_json::Value> = outcome
        .audits()
        .iter()
        .map(|event| {
            serde_json::json!({
                "ts_unix_ms": event.ts_unix_ms,
                "model": event.model,
                "tier": event.tier,
                "fields": event.fields,
                "byte_count": event.byte_count,
            })
        })
        .collect();

    let plan_doc = serde_json::json!({
        "harness": "synthlm-acrd e2e (TSK-505)",
        "seed": seed,
        "intent": intent_text,
        "intent_hash": format!("{:016x}", fnv1a64(intent_text)),
        "backend": backend_label,
        "model": outcome.model(),
        "target_snapshot": outcome.plan().target_snapshot,
        "repair": {
            "rounds": outcome.repair_rounds(),
            "removed": outcome.removed_total(),
            "replaced": outcome.replaced_total(),
            "second_check": "clean",
        },
        "candidates": ranked_json,
        "renders": render_names,
        "audit": audits,
        "notes": [
            "LiveTier2 路径：consent/Key 缺失即 BLOCKED 指引，绝不回退到 seeded 占位；seeded 路径只属于显式 `acrd demo` 模式。",
            "Patch 双校验：plan_for_intent 内修复 + 写盘前 validate 复核；修复计数如实记录（rounds/removed/replaced）。",
            "Lua 内嵌执行渲染（Main_OnCommand 42230，固定 bounds，非相关轨 mute，RENDER_* 备份恢复），不另起新进程链；FNV 记录 + 标签核对回滚与显式复原 + 参数复原断言 + 稳态对稳态渲染 null-test（基线/复原各渲染两次）。",
            "审计事件恰为 IPC 五字段（time/model/tier/fields/byte_count）；mock 路径零调用即 audit 为空。",
        ],
    });

    fs::create_dir_all(out_dir)
        .map_err(|err| E2eError::Io(format!("cannot create output dir: {err}")))?;
    let plan_text = serde_json::to_string_pretty(&plan_doc)
        .map_err(|err| E2eError::Internal(format!("plan serialization failed: {err}")))?;
    let plan_path = out_dir.join("e2e-plan.json");
    fs::write(&plan_path, &plan_text)
        .map_err(|err| E2eError::Io(format!("plan write failed: {err}")))?;

    let lua_text = render_lua(
        seed,
        &outcome.plan().target_snapshot,
        backend_label,
        outcome.model(),
        &format!("{:016x}", fnv1a64(intent_text)),
        &ranked_lua,
    );
    let lua_path = out_dir.join("e2e-apply.lua");
    fs::write(&lua_path, &lua_text)
        .map_err(|err| E2eError::Io(format!("lua write failed: {err}")))?;

    Ok(E2eSummary {
        seed,
        intent: intent_text.to_owned(),
        backend: backend_label.to_owned(),
        model: outcome.model().to_owned(),
        out_dir: out_dir.to_owned(),
        plan_path,
        lua_path,
        ranked_ids,
        repair_rounds: outcome.repair_rounds(),
        removed_total: outcome.removed_total(),
        replaced_total: outcome.replaced_total(),
    })
}

/// Run the CLI live path: real stored consent plus real key presence into the
/// Tier2 text backend, then [`crate::e2e::run_e2e_with_backend`].
///
/// Gate order: stored consent must be decided Tier2 (Tier1 would risk
/// training-retention traffic on the Tier1-first chain, Tier3 is text-local
/// only) and a cloud key must be configured. Any missing piece is `BLOCKED`
/// with remediation guidance and places zero transport calls.
///
/// # Errors
///
/// Returns [`crate::e2e::E2eError`] for missing/unsuitable consent, a missing
/// key, transport-client construction failure, or anything
/// [`crate::e2e::run_e2e_with_backend`] reports.
pub fn run_e2e_live(intent_text: &str, seed: u64, out_dir: &Path) -> Result<E2eSummary, E2eError> {
    let profile = synthlm_profile::load_builtin("reaeq")
        .map_err(|err| E2eError::Profile(format!("reaeq builtin failed: {err}")))?;
    let consent = load_consent();
    let stored = match &consent {
        ConsentState::Decided(store) => store.tier,
        ConsentState::Undecided => {
            return Err(E2eError::Consent(
                "no upload consent stored: complete the first-run choice (1/2/3) or change it in \
                 settings; e2e needs a stored Tier2 choice and cloud calls stay BLOCKED until \
                 then (see DEC-010)"
                    .to_owned(),
            ));
        }
    };
    if stored != ConsentTier::Tier2 {
        return Err(E2eError::Tier(format!(
            "e2e needs a stored Tier2 choice, stored tier is {}: Tier1 would place \
             training-retention traffic on the Tier1-first chain and Tier3 is text-local only; \
             switch the tier in settings, then retry (see DEC-010)",
            tier_label(stored)
        )));
    }
    let api_key = load_api_key().map_err(|_| {
        E2eError::Key(
            "missing API key: set OPENCODE_API_KEY (alias OPENCODE_GO_API_KEY) in .env and \
             restart; e2e cloud calls stay BLOCKED until configured (see DEC-010)"
                .to_owned(),
        )
    })?;
    let mut gateway = Gateway::default();
    let mut transport = HttpsTransport::new(
        api_key.expose_secret().to_owned(),
        DEFAULT_BASE_URL.to_owned(),
    )
    .map_err(|err| E2eError::Internal(format!("transport client failed: {err}")))?
    .with_session_id(format!("synthlm-acrd-e2e-{seed}"));
    let fields: Vec<String> = E2E_UPLOAD_FIELDS
        .iter()
        .map(|field| (*field).to_owned())
        .collect();
    run_e2e_with_backend(
        ModelBackend::LiveTier2 {
            gateway: &mut gateway,
            transport: &mut transport,
            consent: &consent,
            fields,
            byte_count: u64::try_from(intent_text.len()).unwrap_or(u64::MAX),
            cloud_key_present: true,
            now_ms: now_ms(),
        },
        &profile,
        intent_text,
        seed,
        out_dir,
    )
}

/// Human label for a stored tier (static vocabulary, value-free).
fn tier_label(tier: ConsentTier) -> &'static str {
    match tier {
        ConsentTier::Tier1 => "tier1",
        ConsentTier::Tier2 => "tier2",
        ConsentTier::Tier3 => "tier3",
    }
}

/// FNV-1a (64-bit) over the intent bytes: hex stem for log-safe references.
///
/// The Lua side and `.out` lines carry this hash instead of the intent text,
/// so prompt wording never enters logs (AGENTS.md §8); the plan artifact
/// keeps the text under the demo-harness precedent.
fn fnv1a64(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// File-safe stem of a candidate id (the `{stem}-cN` ids only carry hex and
/// dashes, so this is the id itself; anything else is sanitized).
fn file_stem_of(id: &str) -> String {
    let mut stem = String::with_capacity(id.len());
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            stem.push(ch);
        } else {
            stem.push('_');
        }
    }
    if stem.is_empty() {
        stem.push_str("candidate");
    }
    stem
}

/// Direction label for a diversified candidate id.
///
/// Both planner backends name survivors `{stem}-cN`: the seeded pool covers
/// darker/transient/spatial/darker over `c1..=c4`, the live pool
/// round-robins darker/transient/spatial over the surviving plans, and
/// [`synthlm_planner::candidate::diversify`] preserves ids in score order, so
/// the suffix recovers the lane honestly. Anything else reports `unknown`
/// instead of guessing.
fn direction_label(backend_label: &str, id: &str) -> &'static str {
    const MOCK_LANES: [&str; 4] = ["darker", "transient", "spatial", "darker"];
    const LIVE_LANES: [&str; 3] = ["darker", "transient", "spatial"];
    let suffix = id.rsplit('-').next().unwrap_or("");
    let index: usize = suffix
        .strip_prefix('c')
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0);
    if index == 0 {
        return "unknown";
    }
    if backend_label == MOCK_BACKEND_LABEL {
        MOCK_LANES
            .get(index.wrapping_sub(1))
            .copied()
            .unwrap_or("unknown")
    } else if backend_label == LIVE_BACKEND_LABEL {
        LIVE_LANES
            .get(index.wrapping_sub(1) % LIVE_LANES.len())
            .copied()
            .unwrap_or("unknown")
    } else {
        "unknown"
    }
}

/// Project one candidate patch to Lua-appliable `(ident, value)` pairs.
///
/// The applier speaks `TrackFX_SetParam` over `param/` idents with finite
/// normalized numbers only: `macro/` addresses and non-numeric values are
/// refused with the candidate and op indices (never values or prompt text).
fn project_lua_params(candidate: &Candidate, index: usize) -> Result<Vec<(String, f64)>, E2eError> {
    let plan: &PatchPlan = candidate.patch_summary();
    if plan.ops.is_empty() {
        return Err(E2eError::NonNumericOp(format!(
            "candidate at rank {} carries no ops",
            index + 1
        )));
    }
    let mut params = Vec::with_capacity(plan.ops.len());
    for (op_index, op) in plan.ops.iter().enumerate() {
        let tail = op.path.raw().strip_prefix("param/").ok_or_else(|| {
            E2eError::NonNumericOp(format!(
                "candidate at rank {} op {op_index}: not a param/ address",
                index + 1
            ))
        })?;
        let value = op.value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
            E2eError::NonNumericOp(format!(
                "candidate at rank {} op {op_index}: non-numeric value",
                index + 1
            ))
        })?;
        params.push((tail.to_owned(), value));
    }
    Ok(params)
}

/// Render one ranked candidate card for `e2e-plan.json`.
fn candidate_json(
    candidate: &Candidate,
    params: &[(String, f64)],
    direction: &str,
    index: usize,
    render_name: &str,
) -> serde_json::Value {
    let mut ops = Vec::with_capacity(candidate.patch_summary().ops.len());
    for op in &candidate.patch_summary().ops {
        ops.push(serde_json::json!({
            "op": op.op.clone().as_str(),
            "path": op.path.raw(),
            "value": op.value,
        }));
    }
    let lua_params: Vec<serde_json::Value> = params
        .iter()
        .map(|(ident, value)| serde_json::json!({"ident": ident, "value": value}))
        .collect();
    serde_json::json!({
        "id": candidate.id(),
        "rank": index + 1,
        "direction": direction,
        "diff_summary_zh": candidate.diff_summary_zh(),
        "confidence": candidate.confidence(),
        "delta_lufs": candidate.delta_lufs(),
        "changed_params": candidate.changed_params(),
        "audio_ref": render_name,
        "apply_token": format!("e2e-apply-{}", candidate.id()),
        "lua_params": lua_params,
        "patch": {
            "ops": ops,
            "target_snapshot": candidate.patch_summary().target_snapshot,
        },
    })
}

/// Render `e2e-apply.lua`: header table plus the fixed applier body.
///
/// The output is deterministic in its inputs (no timestamps, no absolute
/// paths: renders and logs resolve against the script's own directory at run
/// time). Every ReaScript call below mirrors repo evidence cited in the
/// emitted comments (`experiments/b-matrix-*.out.txt`,
/// `experiments/render-line-02.out.txt`, 2026-10-06).
fn render_lua(
    seed: u64,
    snapshot: &str,
    backend_label: &str,
    model: &str,
    intent_hash: &str,
    candidates: &[LuaCandidate],
) -> String {
    let mut out = String::new();
    out.push_str("-- e2e-apply.lua — generated by `acrd e2e --seed ");
    out.push_str(&seed.to_string());
    out.push_str("` (TSK-505 single-command M3).\n");
    out.push_str("-- backend=");
    out.push_str(backend_label);
    out.push_str(" model=");
    out.push_str(model);
    out.push_str(" （seeded 占位只属于 `acrd demo`；本文件由 e2e 编排按所标 backend 生成）。\n");
    out.push_str("-- 安全：只操作本脚本新建的临时轨；从不调用存盘；结束删除临时轨。\n");
    out.push_str("-- Undo 语义：\n");
    out.push_str("--   * 每个候选一个独立 undo 块 \"SynthLM e2e: apply <id>\"\n");
    out.push_str(
        "--     （Undo_BeginBlock2/EndBlock2，extraflags -1 == UNDO_STATE_ALL，DEC-008）。\n",
    );
    out.push_str("--   * 回滚按 Undo_CanUndo2 标签逆序弹出我们自己的应用块：相同参数的空块可能被宿主合并，\n");
    out.push_str(
        "--     标签不符即停（绝不深入到建轨/插音频的点）；未被 undo 覆盖的参数由独立块\n",
    );
    out.push_str(
        "--     \"SynthLM e2e: restore snapshot\" 显式复原，随后断言每个参数回到初值（容差 1e-9）。\n",
    );
    out.push_str("--   * 渲染（基线×2 + 每候选 + 复原×2）不产生 undo 点；null-test 以稳态对稳态\n");
    out.push_str(
        "--     （baseline-b vs restored-b）为准，首渲沉降只记录不判（settleset/restorestable 行）。\n",
    );
    out.push_str(
        "--   * 清理块 \"SynthLM e2e: cleanup temp track\" 包裹删轨；删轨后轨数回到 ntr0。\n",
    );
    out.push_str("-- 运行：空工程下 reaper.exe -nonewinst <本文件绝对路径>；日志见同目录 e2e-apply.out.txt。\n");
    out.push_str("local SEED = ");
    out.push_str(&seed.to_string());
    out.push_str("\nlocal INTENT_HASH = \"");
    out.push_str(&lua_escape(intent_hash));
    out.push_str("\"\nlocal SNAPSHOT = \"");
    out.push_str(&lua_escape(snapshot));
    out.push_str("\"\nlocal CANDIDATES = {\n");
    for candidate in candidates {
        out.push_str("  { id = \"");
        out.push_str(&lua_escape(&candidate.id));
        out.push_str("\", diff = \"");
        out.push_str(&lua_escape(&candidate.diff_zh));
        out.push_str("\", params = { ");
        for (index, (ident, value)) in candidate.params.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str("{ \"");
            out.push_str(&lua_escape(ident));
            out.push_str("\", ");
            out.push_str(&format_lua_number(*value));
            out.push_str(" }");
        }
        out.push_str(" } },\n");
    }
    out.push_str("}\n");
    out.push_str(SCRIPT_BODY);
    out
}

/// Escape a string for a double-quoted Lua literal.
fn lua_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Format a normalized parameter value as a Lua number literal.
fn format_lua_number(value: f64) -> String {
    format!("{value:.6}")
}

/// Fixed applier body: resolve → reference audio → backup → baseline render
/// → 3 apply+render blocks → reverse rollback → restore asserts →
/// restored render + null-test → render-settings restore → cleanup.
const SCRIPT_BODY: &str = r#"local TOL = 1e-9
local base = debug.getinfo(1, "S").source:match("@?(.*[/\\])")
local out_path = base .. "e2e-apply.out.txt"
local lines = {}
local function log(s) lines[#lines+1] = s end
local function flush()
  local f = io.open(out_path, "w")
  if f then f:write(table.concat(lines, "\n") .. "\n") f:close() end
end
local function fmt_val(v) return string.format("%.9f", v) end
local function fnv_audio(path)
  -- 只哈希 data 块音频字节：REAPER 渲染在 bext 块写 origination time（精确到秒），
  -- 同状态连渲的文件字节 FNV 必然随挂钟秒变化（真机 2026-10-07：两次仅差 bext 内
  -- "15-18-15"/"15-18-16" 一字节），音频字节全等。null-test 必须看音频哈希。
  local f = io.open(path, "rb")
  if not f then return nil, 0 end
  local buf = f:read("a")
  f:close()
  if not buf or #buf < 20 then return nil, 0 end
  if buf:sub(1, 4) ~= "RIFF" or buf:sub(9, 12) ~= "WAVE" then return nil, 0 end
  local pos = 13
  while pos + 8 <= #buf do
    local id = buf:sub(pos, pos + 3)
    local size = string.unpack("<I4", buf, pos + 4)
    local payload = pos + 8
    if id == "data" then
      local h, n = 0x811C9DC5, 0
      local last = math.min(payload + size - 1, #buf)
      for i = payload, last do
        h = ((h ~ buf:byte(i)) * 0x01000193) & 0xFFFFFFFF
        n = n + 1
      end
      if n == 0 then return nil, 0 end
      return string.format("%08x", h), n
    end
    pos = payload + size + (size % 2)
  end
  return nil, 0
end
local function run()
  reaper.PreventUIRefresh(1)
  local ntr0 = reaper.CountTracks(0)
  log("seed=" .. tostring(SEED) .. " intent_hash=" .. INTENT_HASH .. " snapshot=" .. SNAPSHOT .. " tracks_before=" .. tostring(ntr0))
  reaper.InsertTrackAtIndex(ntr0, false)
  local tr = reaper.GetTrack(0, ntr0)
  assert(tr, "temp track missing")
  -- ReaEQ：先查后建（instantiate 0 仅查询，-1000 新建；见 b-matrix-01-stock.lua:26）。
  local fx = reaper.TrackFX_AddByName(tr, "ReaEQ", false, 0)
  local fx_how = "query"
  if fx < 0 then fx = reaper.TrackFX_AddByName(tr, "ReaEQ", false, -1000) fx_how = "add" end
  log("fx=" .. tostring(fx) .. " via=" .. fx_how)
  assert(fx >= 0, "ReaEQ unavailable")
  -- ident→序号：优先 FromIdent（实测 bogus 返回 -1，见 b-matrix-02-ident-env.out.txt:7），
  -- 回退枚举 GetParamIdent 精确匹配；缺 API 时直接走枚举分支。
  local has_from_ident = type(reaper.TrackFX_GetParamFromIdent) == "function"
  local function resolve(ident)
    if has_from_ident then
      local idx = reaper.TrackFX_GetParamFromIdent(tr, fx, ident)
      if type(idx) == "number" and idx >= 0 then return idx, "from_ident" end
    end
    local n = reaper.TrackFX_GetNumParams(tr, fx)
    for i = 0, n - 1 do
      local ok, nm = reaper.TrackFX_GetParamIdent(tr, fx, i)
      if ok and nm == ident then return i, "enumerate" end
    end
    return nil, "missing"
  end
  -- 快照全部涉及参数的初值（只读，不开 undo 块）。
  local initial = {}
  for _, c in ipairs(CANDIDATES) do
    for _, p in ipairs(c.params) do
      if initial[p[1]] == nil then
        local idx, how = resolve(p[1])
        assert(idx ~= nil, "unresolvable ident " .. p[1])
        initial[p[1]] = { idx = idx, value = reaper.TrackFX_GetParam(tr, fx, idx), how = how }
        log("resolve|" .. p[1] .. "=" .. tostring(idx) .. "|" .. how .. "|init=" .. fmt_val(initial[p[1]].value))
      end
    end
  end
  -- 参考音频：脚本目录上一级的 spike-tone.wav（只读 fixture，
  -- experiments/spike-tone.wav，44.1kHz/1s/88244 字节）；缺失即 FATAL，不猜测。
  local ref_path = base .. "../spike-tone.wav"
  local probe = io.open(ref_path, "rb")
  assert(probe ~= nil, "reference audio missing: run from experiments/e2e so ../spike-tone.wav resolves")
  probe:close()
  reaper.SetOnlyTrackSelected(tr)
  reaper.InsertMedia(ref_path, 0)
  local nitems = 0
  for i = 0, reaper.CountMediaItems(0) - 1 do
    if reaper.GetMediaItem_Track(reaper.GetMediaItem(0, i)) == tr then nitems = nitems + 1 end
  end
  log("refitems=" .. tostring(nitems))
  assert(nitems == 1, "reference item missing on temp track")
  -- RENDER_* 全量备份（键表见 render-line-02.lua:23-24）+ loop 备份。
  local NUMKEYS = {"RENDER_SETTINGS","RENDER_BOUNDSFLAG","RENDER_CHANNELS","RENDER_SRATE","RENDER_STARTPOS","RENDER_ENDPOS","RENDER_TAILFLAG","RENDER_TAILMS","RENDER_ADDTOPROJ","RENDER_DITHER"}
  local STRKEYS = {"RENDER_FILE","RENDER_PATTERN"}
  local saved_n, saved_s = {}, {}
  for _, k in ipairs(NUMKEYS) do saved_n[k] = reaper.GetSetProjectInfo(0, k, 0, false) end
  for _, k in ipairs(STRKEYS) do local _, v = reaper.GetSetProjectInfo_String(0, k, "", false) saved_s[k] = v end
  local _, loop0s, loop0e = reaper.GetSet_LoopTimeRange(false, false, 0, 0, false)
  -- 非相关轨全 mute（记住初值），临时轨解 mute（render-line-02.lua:32-41 模式）。
  local mutes = {}
  for i = 0, reaper.CountTracks(0) - 1 do
    local t = reaper.GetTrack(0, i)
    if t ~= tr then
      mutes[i] = reaper.GetMediaTrackInfo_Value(t, "B_MUTE")
      reaper.SetMediaTrackInfo_Value(t, "B_MUTE", 1)
    end
  end
  reaper.SetMediaTrackInfo_Value(tr, "B_MUTE", 0)
  -- 固定 bounds：loop 0..1（参考音频恰 1s）+ BOUNDSFLAG 2（render-line-02.lua:58-59 证据）；
  -- 加到工程关闭，dither 归零（确定性），渲染落脚本同目录。
  reaper.GetSet_LoopTimeRange(true, false, 0, 1, false)
  reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 2, true)
  reaper.GetSetProjectInfo(0, "RENDER_ADDTOPROJ", 0, true)
  reaper.GetSetProjectInfo(0, "RENDER_DITHER", 0, true)
  reaper.GetSetProjectInfo_String(0, "RENDER_FILE", base, true)
  log("rendersetup|srate=" .. tostring(reaper.GetSetProjectInfo(0, "RENDER_SRATE", 0, false)) .. "|channels=" .. tostring(reaper.GetSetProjectInfo(0, "RENDER_CHANNELS", 0, false)) .. "|bounds=loop0-1")
  flush()
  local function render_once(pat)
    reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", pat, true)
    reaper.Main_OnCommand(42230, 0)
    local path = base .. pat .. ".wav"
    local hash, bytes = fnv_audio(path)
    assert(hash ~= nil and bytes > 0, "render missing audio " .. pat)
    return hash, bytes
  end
  -- 基线渲染两次（初值状态；若首渲存在沉降效应，取第二次稳态为参照）。
  local base_a = render_once("e2e-render-baseline-a")
  log("render|baseline_a|fnv=" .. base_a)
  local base_b = render_once("e2e-render-baseline-b")
  log("render|baseline_b|fnv=" .. base_b)
  log("settleset|stable=" .. tostring(base_a == base_b) .. "|ref=" .. base_b)
  flush()
  -- 依次应用 + 逐候选渲染：每候选独立 undo 块（对应 Rust 侧 bridge UndoBlock 一求解一 undo 点，DEC-008）。
  -- 注意渲染是累积语义：候选 N 的渲染 = 前 N 个候选依次应用后的状态。
  for _, c in ipairs(CANDIDATES) do
    log("loop|enter|" .. c.id)
    flush()
    reaper.Undo_BeginBlock2(0)
    for _, p in ipairs(c.params) do
      reaper.TrackFX_SetParam(tr, fx, initial[p[1]].idx, p[2])
    end
    reaper.Undo_EndBlock2(0, "SynthLM e2e: apply " .. c.id, -1)
    local parts = {}
    for _, p in ipairs(c.params) do
      parts[#parts+1] = p[1] .. "=" .. fmt_val(reaper.TrackFX_GetParam(tr, fx, initial[p[1]].idx))
    end
    log("applied|" .. c.id .. "|" .. table.concat(parts, " "))
    flush()
    local hash = render_once("e2e-render-" .. c.id)
    log("render|" .. c.id .. "|fnv=" .. hash)
    flush()
  end
  -- 全部回滚：按 Undo_CanUndo2 标签逆序弹出我们自己的应用块。相同参数的空块
  -- 可能被宿主合并而不占弹出位，标签不符即停，绝不深入到建轨/插音频的点；
  -- 未被 undo 覆盖的参数随后由 restore 块显式复原（covered-by-restore 行如实记录）。
  assert(type(reaper.Undo_CanUndo2) == "function", "Undo_CanUndo2 unavailable")
  local popped, stopped = 0, false
  for i = #CANDIDATES, 1, -1 do
    if stopped then
      log("undo|" .. CANDIDATES[i].id .. "|covered-by-restore")
    else
      local want = "SynthLM e2e: apply " .. CANDIDATES[i].id
      local top = reaper.Undo_CanUndo2(0)
      if top ~= want then
        log("undostop|want=" .. want .. "|top=" .. tostring(top))
        log("undo|" .. CANDIDATES[i].id .. "|covered-by-restore")
        stopped = true
      else
        reaper.Undo_DoUndo2(0)
        popped = popped + 1
        log("undo|" .. CANDIDATES[i].id .. "|popped")
      end
    end
  end
  log("undone_blocks=" .. tostring(popped))
  -- 显式复原：把仍偏离初值的参数一次性设回（独立 restore 块），再断言初值。
  reaper.Undo_BeginBlock2(0)
  local need_restore = false
  for ident, snap in pairs(initial) do
    local v = reaper.TrackFX_GetParam(tr, fx, snap.idx)
    if math.abs(v - snap.value) >= TOL then
      reaper.TrackFX_SetParam(tr, fx, snap.idx, snap.value)
      need_restore = true
    end
  end
  reaper.Undo_EndBlock2(0, "SynthLM e2e: restore snapshot", -1)
  log("explicit_restore=" .. tostring(need_restore))
  local restored = true
  for ident, snap in pairs(initial) do
    local v = reaper.TrackFX_GetParam(tr, fx, snap.idx)
    local ok = math.abs(v - snap.value) < TOL
    restored = restored and ok
    log("restored|" .. ident .. "=" .. fmt_val(v) .. "|expect=" .. fmt_val(snap.value) .. "|ok=" .. tostring(ok))
  end
  log("restored_all=" .. tostring(restored))
  assert(restored, "rollback mismatch")
  flush()
  -- 复原渲染两次 + null-test 自比对：以稳态对稳态（baseline-b vs restored-b）为准
  -- （render-line-02 iso 三连等证据）；失配即 FATAL，不静默。
  local rest_a = render_once("e2e-render-restored-a")
  log("render|restored_a|fnv=" .. rest_a)
  local rest_b = render_once("e2e-render-restored-b")
  log("render|restored_b|fnv=" .. rest_b)
  log("restorestable|stable=" .. tostring(rest_a == rest_b))
  local match = (base_b == rest_b)
  log("nulltest|ref=" .. base_b .. "|restored=" .. rest_b .. "|match=" .. tostring(match))
  assert(match, "render null-test mismatch")
  flush()
  -- 恢复：RENDER_*、loop、mute。
  for _, k in ipairs(NUMKEYS) do reaper.GetSetProjectInfo(0, k, saved_n[k], true) end
  for _, k in ipairs(STRKEYS) do reaper.GetSetProjectInfo_String(0, k, saved_s[k], true) end
  reaper.GetSet_LoopTimeRange(true, false, loop0s, loop0e, false)
  for i, m in pairs(mutes) do
    local t = reaper.GetTrack(0, i)
    if t then reaper.SetMediaTrackInfo_Value(t, "B_MUTE", m) end
  end
  log("restore|ok=1")
  -- 清理：删临时轨（独立 cleanup 块），断言轨数归零。
  reaper.Undo_BeginBlock2(0)
  reaper.DeleteTrack(tr)
  reaper.Undo_EndBlock2(0, "SynthLM e2e: cleanup temp track", -1)
  local ntr1 = reaper.CountTracks(0)
  log("tracks_after=" .. tostring(ntr1) .. "|tracks_zero=" .. tostring(ntr1 == ntr0))
  assert(ntr1 == ntr0, "temp track leaked")
  reaper.PreventUIRefresh(-1)
  reaper.UpdateArrange()
  log("e2e_apply_ok=true")
end
local ok, err = xpcall(run, debug.traceback)
if not ok then
  log("FATAL=" .. tostring(err))
  pcall(function() reaper.PreventUIRefresh(-1) end)
end
flush()
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use synthlm_common::consent::{CONFIG_VERSION, ConsentStore};
    use synthlm_planner::model_gw::{MockOutcome, MockTransport, TransportKind};

    /// Unique scratch dir per test (temp root + pid + tag; removed after).
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "synthlm-acrd-e2e-test-{}-{tag}",
            std::process::id()
        ))
    }

    fn test_profile() -> Profile {
        synthlm_profile::load_builtin("reaeq").expect("reaeq builtin loads")
    }

    fn decided(tier: ConsentTier) -> ConsentState {
        ConsentState::Decided(ConsentStore {
            tier,
            decided_at_unix: 1_789_000_000,
            version: CONFIG_VERSION,
        })
    }

    fn live_backend_of<'a, T: Transport>(
        gateway: &'a mut Gateway,
        transport: &'a mut T,
        consent: &'a ConsentState,
        fields: Vec<String>,
        key_present: bool,
    ) -> ModelBackend<'a, T> {
        ModelBackend::LiveTier2 {
            gateway,
            transport,
            consent,
            fields,
            byte_count: 64,
            cloud_key_present: key_present,
            now_ms: 1_789_000_000_000,
        }
    }

    /// Three well-separated stub patch documents (lanes pairwise > 0.9 apart,
    /// so [`synthlm_planner::candidate::diversify`] keeps all three): the
    /// first needs a numeric-string coercion, the third an out-of-range
    /// clamp, covering the repair counters offline.
    fn stub_three_doc_text() -> String {
        let docs = serde_json::json!([
            {"ops": [
                {"op": "replace", "path": "param/4:_Gain_Band_2", "value": 0.15},
                {"op": "replace", "path": "param/7:_Gain_Band_3", "value": "0.2"},
            ]},
            {"ops": [
                {"op": "replace", "path": "param/4:_Gain_Band_2", "value": 0.85},
                {"op": "replace", "path": "param/7:_Gain_Band_3", "value": 0.8},
            ]},
            {"ops": [
                {"op": "replace", "path": "param/4:_Gain_Band_2", "value": 0.15},
                {"op": "replace", "path": "param/7:_Gain_Band_3", "value": 0.9},
                {"op": "replace", "path": "param/17:wet", "value": 1.5},
            ]},
        ]);
        let raw = serde_json::to_string(&docs).expect("stub docs serialize");
        format!("three patch suggestions below:\n```json\n{raw}\n```")
    }

    fn mock_text_transport(text: String) -> MockTransport {
        MockTransport::new(vec![MockOutcome::SucceedWithText {
            latency_ms: 3,
            text,
        }])
    }

    #[test]
    fn mock_seeded_is_deterministic_for_a_fixed_seed() {
        let first = scratch_dir("det-a");
        let second = scratch_dir("det-b");
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
        let profile = test_profile();
        let left = run_e2e_with_backend(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &first,
        )
        .expect("first run must succeed");
        let right = run_e2e_with_backend(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &second,
        )
        .expect("second run must succeed");
        assert_eq!(left.ranked_ids, right.ranked_ids);
        assert_eq!(left.backend, MOCK_BACKEND_LABEL);
        let left_text =
            fs::read_to_string(first.join("e2e-plan.json")).expect("left plan readable");
        let right_text =
            fs::read_to_string(second.join("e2e-plan.json")).expect("right plan readable");
        assert_eq!(
            left_text, right_text,
            "same seed must give byte-identical plans"
        );
        let left_lua = fs::read_to_string(first.join("e2e-apply.lua")).expect("left lua readable");
        let right_lua =
            fs::read_to_string(second.join("e2e-apply.lua")).expect("right lua readable");
        assert_eq!(left_lua, right_lua, "same seed must give identical Lua");
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
    }

    #[test]
    fn mock_seeded_yields_three_labeled_candidates_with_repair_counts() {
        let dir = scratch_dir("mock3");
        let _ = fs::remove_dir_all(&dir);
        let profile = test_profile();
        let summary = run_e2e_with_backend(
            ModelBackend::<'_, MockTransport>::MockSeeded,
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &dir,
        )
        .expect("mock e2e must succeed");
        assert_eq!(summary.ranked_ids.len(), E2E_SHORTLIST);
        assert_eq!(summary.repair_rounds, 0);
        let text = fs::read_to_string(dir.join("e2e-plan.json")).expect("plan readable");
        let plan: serde_json::Value = serde_json::from_str(&text).expect("plan parses");
        assert_eq!(plan["backend"].as_str(), Some(MOCK_BACKEND_LABEL));
        let candidates = plan["candidates"].as_array().expect("candidates array");
        assert_eq!(candidates.len(), E2E_SHORTLIST);
        let mut directions: Vec<&str> = candidates
            .iter()
            .map(|card| card["direction"].as_str().expect("direction string"))
            .collect();
        directions.sort_unstable();
        assert_eq!(directions, vec!["darker", "spatial", "transient"]);
        for card in candidates {
            let diff = card["diff_summary_zh"].as_str().expect("diff string");
            assert!(!diff.trim().is_empty(), "diff sentence must be non-empty");
            assert!(
                card["audio_ref"]
                    .as_str()
                    .expect("audio ref")
                    .ends_with(".wav"),
                "audio_ref must name the REAPER-side render target"
            );
            let lua_params = card["lua_params"].as_array().expect("lua params");
            assert!(!lua_params.is_empty(), "lua params must be non-empty");
        }
        assert_eq!(
            plan["audit"].as_array().expect("audit array").len(),
            0,
            "mock places no calls, so audit stays empty"
        );
        assert_eq!(
            plan["repair"]["second_check"].as_str(),
            Some("clean"),
            "second validation must record clean"
        );
        let lua = fs::read_to_string(dir.join("e2e-apply.lua")).expect("lua readable");
        for id in &summary.ranked_ids {
            assert!(lua.contains(id), "lua must apply every candidate ({id})");
        }
        for marker in [
            "Undo_BeginBlock2",
            "Undo_CanUndo2",
            "Undo_DoUndo2",
            "Main_OnCommand(42230, 0)",
            "DeleteTrack",
            "e2e_apply_ok=true",
            "undone_blocks=",
            "explicit_restore=",
            "settleset|",
            "restorestable|",
            "nulltest|",
            "RENDER_BOUNDSFLAG",
        ] {
            assert!(lua.contains(marker), "lua must contain {marker}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stub_live_text_yields_three_candidates_with_repair_counts() {
        let dir = scratch_dir("stub3");
        let _ = fs::remove_dir_all(&dir);
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let mut transport = mock_text_transport(stub_three_doc_text());
        let summary = run_e2e_with_backend(
            live_backend_of(
                &mut gateway,
                &mut transport,
                &consent,
                vec!["prompt".to_owned()],
                true,
            ),
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &dir,
        )
        .expect("stub text must plan");
        assert_eq!(summary.backend, LIVE_BACKEND_LABEL);
        assert_eq!(summary.ranked_ids.len(), E2E_SHORTLIST);
        assert_eq!(summary.repair_rounds, 2, "two docs need one round each");
        assert_eq!(summary.removed_total, 0);
        assert_eq!(summary.replaced_total, 2, "string coerce + clamp");
        let text = fs::read_to_string(dir.join("e2e-plan.json")).expect("plan readable");
        let plan: serde_json::Value = serde_json::from_str(&text).expect("plan parses");
        assert_eq!(plan["backend"].as_str(), Some(LIVE_BACKEND_LABEL));
        let audits = plan["audit"].as_array().expect("audit array");
        assert_eq!(audits.len(), 1, "one stub attempt audits once");
        let mut keys: Vec<&str> = audits[0]
            .as_object()
            .expect("audit object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["byte_count", "fields", "model", "tier", "ts_unix_ms"],
            "audit carries exactly the five IPC fields"
        );
        assert_eq!(transport.calls().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_gate_rejections_place_zero_calls_and_write_nothing() {
        let profile = test_profile();

        // No consent: BLOCKED before any transport use, nothing written.
        let dir = scratch_dir("gate-consent");
        let _ = fs::remove_dir_all(&dir);
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::all_ok();
        let err = run_e2e_with_backend(
            live_backend_of(
                &mut gateway,
                &mut transport,
                &ConsentState::Undecided,
                vec!["prompt".to_owned()],
                true,
            ),
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &dir,
        )
        .expect_err("undecided consent BLOCKEDs");
        assert!(format!("{err}").contains("BLOCKED"));
        assert!(!format!("{err}").to_lowercase().contains("live-tier2"));
        assert!(transport.calls().is_empty());
        assert!(!dir.exists(), "failed planning must write nothing");

        // No key: BLOCKED before any transport use.
        let consent = decided(ConsentTier::Tier2);
        let mut keyless = MockTransport::all_ok();
        let err = run_e2e_with_backend(
            live_backend_of(
                &mut gateway,
                &mut keyless,
                &consent,
                vec!["prompt".to_owned()],
                false,
            ),
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &dir,
        )
        .expect_err("missing key BLOCKEDs");
        assert!(format!("{err}").contains("BLOCKED"));
        assert!(keyless.calls().is_empty());
        assert!(!dir.exists());

        // Whitelist violation: BLOCKED before any transport use.
        let mut wild = MockTransport::all_ok();
        let err = run_e2e_with_backend(
            live_backend_of(
                &mut gateway,
                &mut wild,
                &consent,
                vec!["pcm".to_owned()],
                true,
            ),
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &dir,
        )
        .expect_err("off-whitelist BLOCKEDs");
        assert!(format!("{err}").contains("BLOCKED"));
        assert!(wild.calls().is_empty());
        assert!(!dir.exists());
    }

    #[test]
    fn live_transport_exhaustion_is_blocked_not_live() {
        let dir = scratch_dir("gate-exhaust");
        let _ = fs::remove_dir_all(&dir);
        let profile = test_profile();
        let consent = decided(ConsentTier::Tier2);
        let mut gateway = Gateway::default();
        let mut transport = MockTransport::new(vec![
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
            MockOutcome::Fail(TransportKind::ServerError),
        ]);
        let err = run_e2e_with_backend(
            live_backend_of(
                &mut gateway,
                &mut transport,
                &consent,
                vec!["prompt".to_owned()],
                true,
            ),
            &profile,
            DEFAULT_INTENT_ZH,
            7,
            &dir,
        )
        .expect_err("exhaustion must not read as a model result");
        assert!(
            format!("{err}").contains("RETRYABLE"),
            "tier fallback exhaustion stays retryable per the taxonomy, got: {err}"
        );
        assert!(!format!("{err}").contains(LIVE_BACKEND_LABEL));
        assert!(!dir.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn direction_labels_cover_both_backends_and_unknowns() {
        assert_eq!(
            direction_label(MOCK_BACKEND_LABEL, "intent-abc-c1"),
            "darker"
        );
        assert_eq!(
            direction_label(MOCK_BACKEND_LABEL, "intent-abc-c2"),
            "transient"
        );
        assert_eq!(
            direction_label(MOCK_BACKEND_LABEL, "intent-abc-c3"),
            "spatial"
        );
        assert_eq!(
            direction_label(MOCK_BACKEND_LABEL, "intent-abc-c4"),
            "darker"
        );
        assert_eq!(
            direction_label(MOCK_BACKEND_LABEL, "intent-abc-c9"),
            "unknown"
        );
        assert_eq!(
            direction_label(LIVE_BACKEND_LABEL, "intent-abc-c1"),
            "darker"
        );
        assert_eq!(
            direction_label(LIVE_BACKEND_LABEL, "intent-abc-c2"),
            "transient"
        );
        assert_eq!(
            direction_label(LIVE_BACKEND_LABEL, "intent-abc-c3"),
            "spatial"
        );
        assert_eq!(direction_label(LIVE_BACKEND_LABEL, "weird-id"), "unknown");
        assert_eq!(direction_label("nope", "intent-abc-c1"), "unknown");
    }

    #[test]
    fn lua_projection_refuses_macro_and_non_numeric_ops() {
        use synthlm_planner::patch::{IdentPath, PatchOp, PatchOpKind};
        let plan = PatchPlan {
            ops: vec![
                PatchOp {
                    op: PatchOpKind::Replace,
                    path: IdentPath::new("macro/something"),
                    value: serde_json::json!(0.5),
                },
                PatchOp {
                    op: PatchOpKind::Replace,
                    path: IdentPath::new("param/4:_Gain_Band_2"),
                    value: serde_json::json!(true),
                },
            ],
            target_snapshot: "snap-x".to_owned(),
        };
        let candidate = Candidate::new(
            "x-c1".to_owned(),
            plan,
            "差异句。".to_owned(),
            0.5,
            0.0,
            2,
            synthlm_planner::candidate::ScoreSnapshot::new(1.0).expect("score"),
        )
        .expect("candidate builds");
        let err = project_lua_params(&candidate, 0).expect_err("macro must refuse");
        assert!(format!("{err}").contains("param/"));
    }
}
