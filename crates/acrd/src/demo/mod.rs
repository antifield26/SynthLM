//! M3 interactive end-to-end demo harness (TSK-405).
//!
//! Deterministic stand-in for the model planning step: a fixed intent
//! (brighter / darker / wider) maps to three hard-coded ReaEQ patch groups,
//! each double-checked by the planner patch validator, deduplicated by the
//! planner diversifier, auditioned through Rust-synthesized preview variants
//! scored with the eval pipeline, and exported as `demo-plan.json` plus a
//! temp-track-only `demo-apply.lua` (one independent undo block per
//! candidate, full rollback at the tail).
//!
//! Honesty contract: no model is called and the plan records the model id
//! `seeded-demo-no-model-call`. Nothing touches the network and no key is
//! read. The seed only drives a tiny deterministic jitter on the displayed
//! confidence; ranking always comes from the eval distances.
//!
//! Role grounding (ReaEQ whitelist, `crates/profile/profiles/reaeq.json`,
//! idents verified against `experiments/b-matrix-01-stock.out.txt`):
//! brighter lifts the high-shelf and presence gains, darker cuts them, and
//! wider dips the low-mid band while the macro trims loudness. ReaEQ owns no
//! true stereo-width parameter, so the wide candidate is an explicitly
//! labelled EQ proxy in both the plan and the Lua comments.

mod lua;
mod synth;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use synthlm_common::config;
use synthlm_common::ipc::{AuditEvent, ConsentTier};
use synthlm_eval::mir::{MirParams, analyze};
use synthlm_eval::score::compare;
use synthlm_planner::candidate::{
    Candidate, Direction, PoolEntry, ScoreSnapshot, count_changed_ops, diversify,
};
use synthlm_planner::patch::{IdentPath, PatchOp, PatchOpKind, PatchPlan, validate_with_repair};
use synthlm_profile::load_builtin;

use lua::LuaCandidate;
use synth::SynthKind;

/// Placeholder model id recorded in the plan and audit: no model ran.
pub const DEMO_MODEL_ID: &str = "seeded-demo-no-model-call";

/// Placeholder audit timestamp base in Unix ms (deterministic; the seed and
/// the per-candidate index are added on top, so replays are byte-identical).
pub const DEMO_AUDIT_TS_BASE_MS: i64 = 1_789_000_000_000;

/// Upload fields claimed by the placeholder audit (whitelist subset; Tier3
/// never uploads, so this claims intent only, never traffic).
const DEMO_AUDIT_FIELDS: [&str; 3] = ["prompt", "mir", "meta"];

/// Demo harness failure.
///
/// Application-layer error type: human-readable and secret-free by
/// construction (it never carries prompts, PCM, user paths, or key
/// material; the `Io` variant names the operation, not file content).
#[derive(Debug)]
pub enum DemoError {
    /// Command-line usage error (the binary prints usage, exit code 2).
    BadUsage(String),
    /// The ReaEQ builtin profile failed to load or validate.
    Profile(String),
    /// A fixed demo patch did not survive planner validation.
    PatchRejected(String),
    /// Diversification left a direction uncovered or collapsed a candidate.
    DiversifyGap(String),
    /// Eval analysis or comparison failed.
    Eval(String),
    /// A harness-internal invariant broke (constructor or lookup failure).
    Internal(String),
    /// Filesystem write failed.
    Io(String),
}

impl fmt::Display for DemoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DemoError::BadUsage(detail) => write!(f, "usage error: {detail}"),
            DemoError::Profile(detail) => write!(f, "profile error: {detail}"),
            DemoError::PatchRejected(detail) => write!(f, "patch rejected: {detail}"),
            DemoError::DiversifyGap(detail) => write!(f, "diversify gap: {detail}"),
            DemoError::Eval(detail) => write!(f, "eval error: {detail}"),
            DemoError::Internal(detail) => write!(f, "internal error: {detail}"),
            DemoError::Io(detail) => write!(f, "i/o error: {detail}"),
        }
    }
}

impl std::error::Error for DemoError {}

/// What one `crate::demo::run_demo` invocation produced (paths plus the
/// ranked order, for the runbook witness step and the binary summary line).
#[derive(Clone, Debug)]
pub struct DemoSummary {
    /// Seed driving the deterministic placeholder jitter.
    pub seed: u64,
    /// Output directory holding the three artifacts.
    pub out_dir: PathBuf,
    /// Written plan artifact.
    pub plan_path: PathBuf,
    /// Written Lua applier artifact.
    pub lua_path: PathBuf,
    /// Candidate ids in ranked (eval) order.
    pub ranked_ids: Vec<String>,
}

/// One fixed demo direction: patch, diversify lane, synth, and copy.
struct DemoSpec {
    /// Stable candidate id.
    id: &'static str,
    /// Forced-coverage lane. Brighter has no lane of its own in the
    /// planner taxonomy, so it rides the transient lane (high-frequency
    /// presence emphasis); darker and wider map 1:1.
    direction: Direction,
    /// One Chinese sentence of audible difference (UI copy contract).
    diff_zh: &'static str,
    /// `(param/<ident>, normalized value)` patch ops.
    ops: &'static [(&'static str, f64)],
    /// Diversify lane (pairwise distance above the default radius).
    lane: [f64; 3],
    /// Audition variant rendered for this candidate.
    synth: SynthKind,
    /// Base confidence before the seeded jitter.
    base_confidence: f64,
}

/// The three fixed demo specs (order is display order; ranking comes from
/// eval and may differ).
const SPECS: [DemoSpec; 3] = [
    DemoSpec {
        id: "demo-bright",
        direction: Direction::Transient,
        diff_zh: "高频搁架与临场频段提升，空气感增强，整体更亮。",
        ops: &[
            ("param/10:_Gain_High_Shelf_4", 0.75),
            ("param/7:_Gain_Band_3", 0.65),
        ],
        lane: [0.80, 0.70, 0.50],
        synth: SynthKind::Bright,
        base_confidence: 0.78,
    },
    DemoSpec {
        id: "demo-dark",
        direction: Direction::Darker,
        diff_zh: "高频搁架与临场频段衰减，毛刺收敛，整体更暗。",
        ops: &[
            ("param/10:_Gain_High_Shelf_4", 0.25),
            ("param/7:_Gain_Band_3", 0.30),
        ],
        lane: [0.20, 0.30, 0.50],
        synth: SynthKind::Dark,
        base_confidence: 0.76,
    },
    DemoSpec {
        id: "demo-wide",
        direction: Direction::Spatial,
        diff_zh: "中频微凹并点缀高频，声场左右拉开更宽（EQ 代理：ReaEQ 无真宽度参数）。",
        ops: &[
            ("param/4:_Gain_Band_2", 0.40),
            ("param/10:_Gain_High_Shelf_4", 0.60),
            ("param/15:_Global_Gain", 0.55),
        ],
        lane: [0.50, 0.50, 0.90],
        synth: SynthKind::Wide,
        base_confidence: 0.72,
    },
];

/// Deterministic xorshift64* stream (mirrors the planner search helper so
/// the demo stays dependency-free; a zero seed is salted).
struct Rng(u64);

impl Rng {
    /// Build a stream from the demo seed.
    fn new(seed: u64) -> Self {
        Self(seed | 0x9E37_79B9_7F4A_7C15)
    }

    /// Next `u64` (xorshift64*).
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Next `f64` in `[0, 1)` (top 53 bits, exactly representable).
    fn next_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;
        ((self.next_u64() >> 11) as f64) * SCALE
    }
}

/// Run the demo for `seed`, writing `demo-plan.json`, `demo-apply.lua`,
/// and three preview WAVs into `out_dir` (created when missing).
///
/// # Errors
///
/// Returns `crate::demo::DemoError` when the profile, patch validation,
/// diversification, eval scoring, or any file write fails.
pub fn run_demo(seed: u64, out_dir: &Path) -> Result<DemoSummary, DemoError> {
    let profile = load_builtin("reaeq")
        .map_err(|err| DemoError::Profile(format!("reaeq builtin failed: {err}")))?;
    fs::create_dir_all(out_dir)
        .map_err(|err| DemoError::Io(format!("cannot create output dir: {err}")))?;

    let snapshot_id = format!("demo-snapshot-seed-{seed}");
    let params = MirParams::v1();
    let reference = analyze(&synth::render(SynthKind::Base), &params)
        .map_err(|err| DemoError::Eval(format!("reference analysis failed: {err}")))?;

    let mut rng = Rng::new(seed);
    let mut pool: Vec<PoolEntry> = Vec::with_capacity(SPECS.len());
    let mut built: Vec<BuiltCandidate> = Vec::with_capacity(SPECS.len());
    for spec in &SPECS {
        let plan = build_plan(spec, &snapshot_id);
        let outcome = validate_with_repair(&plan, &profile, 2);
        if !outcome.accepted() {
            return Err(DemoError::PatchRejected(format!(
                "{}: {} residual error(s) after repair",
                spec.id,
                outcome.residual_errors.len()
            )));
        }
        let samples = synth::render(spec.synth);
        let features = analyze(&samples, &params)
            .map_err(|err| DemoError::Eval(format!("{} analysis failed: {err}", spec.id)))?;
        let score = compare(&reference, &features)
            .map_err(|err| DemoError::Eval(format!("{} compare failed: {err}", spec.id)))?;
        let total = 1.0 / (1.0 + f64::from(score.mel_weighted));
        if !total.is_finite() {
            return Err(DemoError::Eval(format!(
                "{} produced a non-finite ranking total",
                spec.id
            )));
        }
        let jitter = rng.next_f64();
        let confidence = (spec.base_confidence + (jitter - 0.5) * 0.02).clamp(0.0, 1.0);
        let patch = outcome.plan;
        let changed = count_changed_ops(&patch);
        let snapshot = ScoreSnapshot::new(total)
            .map_err(|err| DemoError::Internal(format!("{} score: {err}", spec.id)))?;
        let candidate = Candidate::new(
            spec.id.to_owned(),
            patch.clone(),
            spec.diff_zh.to_owned(),
            confidence,
            score.delta_lufs,
            changed,
            snapshot,
        )
        .map_err(|err| DemoError::Internal(format!("{} candidate: {err}", spec.id)))?;
        pool.push(
            PoolEntry::new(candidate, spec.lane.to_vec(), spec.direction)
                .map_err(|err| DemoError::Internal(format!("{} pool entry: {err}", spec.id)))?,
        );
        let preview_name = format!("demo-preview-{}.wav", spec.id);
        synth::write_wav_mono16(&out_dir.join(&preview_name), &samples, synth::SAMPLE_RATE)?;
        let plan_json = plan_to_json(&patch)?;
        let byte_count = serde_json::to_string(&plan_json)
            .map(|text| text.len() as u64)
            .map_err(|err| DemoError::Internal(format!("plan sizing failed: {err}")))?;
        built.push(BuiltCandidate {
            id: spec.id.to_owned(),
            mel_weighted: score.mel_weighted,
            delta_lufs: score.delta_lufs,
            confidence,
            total,
            audio_ref: preview_name,
            byte_count,
            plan_json,
            patch_ops: spec
                .ops
                .iter()
                .map(|(ident, value)| (ident_label(ident).to_owned(), *value))
                .collect(),
            diff_zh: spec.diff_zh.to_owned(),
        });
    }

    let diversified = diversify(&pool, 3);
    if diversified.direction_gap() {
        return Err(DemoError::DiversifyGap(format!(
            "missing directions: {:?}",
            diversified.missing_directions()
        )));
    }
    if diversified.candidates().len() != SPECS.len() {
        return Err(DemoError::DiversifyGap(format!(
            "expected {} survivors, got {}",
            SPECS.len(),
            diversified.candidates().len()
        )));
    }

    let mut ranked_json = Vec::with_capacity(SPECS.len());
    let mut ranked_lua = Vec::with_capacity(SPECS.len());
    let mut ranked_ids = Vec::with_capacity(SPECS.len());
    let mut audits = Vec::with_capacity(SPECS.len());
    for (index, survivor) in diversified.candidates().iter().enumerate() {
        let info = built
            .iter()
            .find(|entry| entry.id == survivor.id())
            .ok_or_else(|| {
                DemoError::Internal(format!("diversify returned unknown id {}", survivor.id()))
            })?;
        ranked_ids.push(info.id.clone());
        ranked_json.push(candidate_json(info, survivor, index));
        ranked_lua.push(LuaCandidate {
            id: info.id.clone(),
            diff_zh: info.diff_zh.clone(),
            params: info.patch_ops.clone(),
        });
        audits.push(audit_json(seed, index, info.byte_count));
    }

    let plan_doc = serde_json::json!({
        "harness": "synthlm-acrd demo (TSK-405)",
        "seed": seed,
        "intent": "更亮 / 更暗 / 更宽（固定意图，ReaEQ 白名单代理；宽为 EQ 代理）",
        "model": DEMO_MODEL_ID,
        "target_snapshot": snapshot_id,
        "candidates": ranked_json,
        "audit": audits,
        "notes": [
            "AI 规划步由固定种子确定性候选代替真实模型调用：诚实演示，非伪装智能。",
            "ranking 由 eval mel_weighted 距离导出（total = 1/(1+mel_weighted)）；confidence 仅叠加种子抖动。",
            "audit 为占位：Tier3 本地档，无任何网络调用；字段走白名单审计。",
        ],
    });
    let plan_text = serde_json::to_string_pretty(&plan_doc)
        .map_err(|err| DemoError::Internal(format!("plan serialization failed: {err}")))?;
    let plan_path = out_dir.join("demo-plan.json");
    fs::write(&plan_path, &plan_text)
        .map_err(|err| DemoError::Io(format!("plan write failed: {err}")))?;

    let lua_text = lua::render(seed, &snapshot_id, &ranked_lua);
    let lua_path = out_dir.join("demo-apply.lua");
    fs::write(&lua_path, &lua_text)
        .map_err(|err| DemoError::Io(format!("lua write failed: {err}")))?;

    Ok(DemoSummary {
        seed,
        out_dir: out_dir.to_owned(),
        plan_path,
        lua_path,
        ranked_ids,
    })
}

/// Planner-side data kept per candidate for artifact rendering.
struct BuiltCandidate {
    id: String,
    mel_weighted: f32,
    delta_lufs: f64,
    confidence: f64,
    total: f64,
    audio_ref: String,
    byte_count: u64,
    plan_json: serde_json::Value,
    patch_ops: Vec<(String, f64)>,
    diff_zh: String,
}

/// Build the [`PatchPlan`] for one spec (replace ops over ident paths).
fn build_plan(spec: &DemoSpec, snapshot_id: &str) -> PatchPlan {
    let mut ops = Vec::with_capacity(spec.ops.len());
    for (path, value) in spec.ops {
        ops.push(PatchOp {
            op: PatchOpKind::Replace,
            path: IdentPath::new((*path).to_owned()),
            value: serde_json::Number::from_f64(*value)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
        });
    }
    PatchPlan {
        ops,
        target_snapshot: snapshot_id.to_owned(),
    }
}

/// Strip the `param/` head for the Lua applier (which resolves idents).
fn ident_label(path: &str) -> &str {
    path.strip_prefix("param/").unwrap_or(path)
}

/// Render one [`PatchPlan`] as JSON (deterministic key order by construction).
fn plan_to_json(plan: &PatchPlan) -> Result<serde_json::Value, DemoError> {
    let mut ops = Vec::with_capacity(plan.ops.len());
    for op in &plan.ops {
        ops.push(serde_json::json!({
            "op": op.op.clone().as_str(),
            "path": op.path.raw(),
            "value": op.value,
        }));
    }
    Ok(serde_json::json!({
        "ops": ops,
        "target_snapshot": plan.target_snapshot,
    }))
}

/// Render one ranked candidate card for `demo-plan.json`.
fn candidate_json(info: &BuiltCandidate, survivor: &Candidate, index: usize) -> serde_json::Value {
    serde_json::json!({
        "id": info.id,
        "rank": index + 1,
        "direction": direction_of(survivor.id()),
        "diff_summary_zh": survivor.diff_summary_zh(),
        "confidence": info.confidence,
        "delta_lufs": info.delta_lufs,
        "changed_params": survivor.patch_summary().ops.len(),
        "score_total": info.total,
        "mel_weighted": info.mel_weighted,
        "audio_ref": info.audio_ref,
        "apply_token": format!("demo-apply-{}", info.id),
        "patch": info.plan_json,
    })
}

/// Map a stable demo id back to its planner direction label.
fn direction_of(id: &str) -> &'static str {
    match id {
        "demo-dark" => "darker",
        "demo-bright" => "transient",
        "demo-wide" => "spatial",
        _ => "unknown",
    }
}

/// Render one placeholder audit event (exactly the five IPC audit fields).
fn audit_json(seed: u64, index: usize, byte_count: u64) -> serde_json::Value {
    let fields: Vec<String> = DEMO_AUDIT_FIELDS
        .iter()
        .map(|field| (*field).to_owned())
        .collect();
    let _ = config::validate_upload_fields(&fields);
    let event = AuditEvent::new(
        DEMO_MODEL_ID,
        ConsentTier::Tier3,
        fields,
        byte_count,
        DEMO_AUDIT_TS_BASE_MS
            .saturating_add(seed as i64)
            .saturating_add(index as i64),
    );
    serde_json::json!({
        "ts_unix_ms": event.ts_unix_ms,
        "model": event.model,
        "tier": "tier3",
        "fields": event.fields,
        "byte_count": event.byte_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir per test (temp root + pid + tag; removed after).
    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "synthlm-acrd-demo-test-{}-{tag}",
            std::process::id()
        ))
    }

    fn read_plan(out_dir: &Path) -> serde_json::Value {
        let text = fs::read_to_string(out_dir.join("demo-plan.json")).expect("plan must exist");
        serde_json::from_str(&text).expect("plan must parse")
    }

    #[test]
    fn demo_is_deterministic_for_a_fixed_seed() {
        let first = scratch_dir("det-a");
        let second = scratch_dir("det-b");
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
        let left = run_demo(7, &first).expect("first run must succeed");
        let right = run_demo(7, &second).expect("second run must succeed");
        assert_eq!(left.ranked_ids, right.ranked_ids);
        let left_text =
            fs::read_to_string(first.join("demo-plan.json")).expect("left plan readable");
        let right_text =
            fs::read_to_string(second.join("demo-plan.json")).expect("right plan readable");
        assert_eq!(
            left_text, right_text,
            "same seed must give byte-identical plans"
        );
        let left_lua = fs::read_to_string(first.join("demo-apply.lua")).expect("left lua readable");
        let right_lua =
            fs::read_to_string(second.join("demo-apply.lua")).expect("right lua readable");
        assert_eq!(left_lua, right_lua, "same seed must give identical Lua");
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
    }

    #[test]
    fn demo_covers_all_three_directions() {
        let dir = scratch_dir("dirs");
        let _ = fs::remove_dir_all(&dir);
        let summary = run_demo(7, &dir).expect("demo must succeed");
        assert_eq!(summary.ranked_ids.len(), 3);
        let plan = read_plan(&dir);
        let candidates = plan["candidates"].as_array().expect("candidates array");
        assert_eq!(candidates.len(), 3);
        let mut directions: Vec<&str> = candidates
            .iter()
            .map(|card| card["direction"].as_str().expect("direction string"))
            .collect();
        directions.sort_unstable();
        assert_eq!(directions, vec!["darker", "spatial", "transient"]);
        for card in candidates {
            let diff = card["diff_summary_zh"].as_str().expect("diff string");
            assert!(!diff.trim().is_empty(), "diff sentence must be non-empty");
            let patch_ops = card["patch"]["ops"].as_array().expect("patch ops");
            assert!(!patch_ops.is_empty(), "patch must carry ops");
            for op in patch_ops {
                let value = op["value"].as_f64().expect("numeric value");
                assert!(
                    (0.0..=1.0).contains(&value),
                    "patch value {value} outside 0.0..=1.0"
                );
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn demo_audit_placeholders_are_complete() {
        let dir = scratch_dir("audit");
        let _ = fs::remove_dir_all(&dir);
        run_demo(7, &dir).expect("demo must succeed");
        let plan = read_plan(&dir);
        let audits = plan["audit"].as_array().expect("audit array");
        assert_eq!(audits.len(), 3);
        for (index, event) in audits.iter().enumerate() {
            let obj = event.as_object().expect("audit object");
            let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec!["byte_count", "fields", "model", "tier", "ts_unix_ms"],
                "audit must carry exactly the five IPC fields"
            );
            assert_eq!(event["model"].as_str(), Some(DEMO_MODEL_ID));
            assert_eq!(event["tier"].as_str(), Some("tier3"));
            let fields: Vec<&str> = event["fields"]
                .as_array()
                .expect("fields array")
                .iter()
                .map(|field| field.as_str().expect("field string"))
                .collect();
            assert_eq!(fields, vec!["prompt", "mir", "meta"]);
            assert!(
                event["byte_count"].as_u64().expect("byte count") > 0,
                "byte count must be positive"
            );
            assert_eq!(
                event["ts_unix_ms"].as_i64(),
                Some(DEMO_AUDIT_TS_BASE_MS + 7 + index as i64)
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn demo_writes_listenable_previews_and_lua() {
        let dir = scratch_dir("files");
        let _ = fs::remove_dir_all(&dir);
        let summary = run_demo(7, &dir).expect("demo must succeed");
        assert!(summary.plan_path.is_file());
        assert!(summary.lua_path.is_file());
        for id in &summary.ranked_ids {
            let wav = dir.join(format!("demo-preview-{id}.wav"));
            let bytes = fs::read(&wav).expect("preview must exist");
            assert!(
                bytes.len() > 44 + 4096,
                "preview {id} too small to be one second of audio"
            );
            assert_eq!(&bytes[0..4], b"RIFF", "preview {id} must be WAV");
        }
        let lua = fs::read_to_string(&summary.lua_path).expect("lua readable");
        for id in &summary.ranked_ids {
            assert!(
                lua.contains(id),
                "lua must apply every ranked candidate ({id})"
            );
        }
        assert!(lua.contains("Undo_BeginBlock2"));
        assert!(lua.contains("Undo_DoUndo2"));
        assert!(lua.contains("DeleteTrack"));
        let _ = fs::remove_dir_all(&dir);
    }
}
