//! TSK-302 fixture corpus and DEC-013 gate tests.
//!
//! Corpora (48 fixtures total):
//!
//! - valid: 28 single-op plans built from real builtin-profile addresses
//!   (7 plugins × 3 TSK-201 pin idents + 7 macro paths). First-pass validity
//!   must clear 95% (DEC-013 reversal condition).
//! - fixable: 8 plans needing clamp / coercion / value-strip. Together with
//!   the valid set, acceptance after ≤ 2 repair rounds must clear 99%.
//! - unfixable: 12 plans whose single op is dropped (or whose snapshot is
//!   empty), ending `BLOCKED` with terminal taxonomy codes.
//!
//! Pin idents are the TSK-201 evidence anchors (`crates/profile` builtin
//! tests cite the `experiments/b-matrix-*.out.txt` lines); a typo or an
//! invented parameter name fails here instead of shipping.

use synthlm_planner::patch::{
    DEFAULT_MAX_REPAIR_ROUNDS, IdentPath, PatchErrorKind, PatchOp, PatchOpKind, PatchPlan,
    validate, validate_with_repair,
};
use synthlm_profile::builtins::load_builtin;
use synthlm_profile::schema::{Profile, UiHint};

/// DEC-013 reversal-condition monitor: first-round structured validity.
const FIRST_PASS_GATE: f64 = 0.95;
/// DEC-013 reversal-condition monitor: validity after ≤ 2 repair rounds.
const REPAIR_GATE: f64 = 0.99;

fn plan_of(path: &str, value: serde_json::Value, profile_name: &str) -> (PatchPlan, Profile) {
    let _ = profile_name;
    (
        PatchPlan {
            ops: vec![PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new(path),
                value,
            }],
            target_snapshot: "snap-fixture".to_owned(),
        },
        load_builtin(profile_name).expect("builtin profile must load"),
    )
}

fn profile_of(name: &str) -> Profile {
    load_builtin(name).expect("builtin profile must load")
}

/// Widget-appropriate scalar: toggles take `true`, selects take index `0`,
/// continuous takes `0.5`.
fn value_for(ui: UiHint) -> serde_json::Value {
    match ui {
        UiHint::Toggle => serde_json::Value::Bool(true),
        UiHint::Select => serde_json::json!(0),
        UiHint::Slider | UiHint::Knob | UiHint::Hidden => serde_json::json!(0.5),
    }
}

/// Build one single-op plan per ident with a widget-fitting value.
fn plans_for_idents(profile_name: &str, idents: &[&str]) -> Vec<(PatchPlan, Profile)> {
    let profile = profile_of(profile_name);
    idents
        .iter()
        .map(|ident| {
            let entry = profile
                .param_by_ident(ident)
                .expect("pin ident must exist in profile");
            (
                PatchPlan {
                    ops: vec![PatchOp {
                        op: PatchOpKind::Replace,
                        path: IdentPath::new(format!("param/{ident}")),
                        value: value_for(entry.ui),
                    }],
                    target_snapshot: "snap-fixture".to_owned(),
                },
                profile.clone(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Per-plugin pin抽查 (≥3 real ident hits each, TSK-201 anchors)
// ---------------------------------------------------------------------------

fn check_plugin_pins(profile_name: &str, idents: &[&str], macro_path: &str) {
    assert!(idents.len() >= 3, "{profile_name}: need ≥3 pin idents");
    let profile = profile_of(profile_name);
    for ident in idents {
        let entry = profile
            .param_by_ident(ident)
            .expect("pin ident must resolve");
        let plan = PatchPlan {
            ops: vec![PatchOp {
                op: PatchOpKind::Replace,
                path: IdentPath::new(format!("param/{ident}")),
                value: value_for(entry.ui),
            }],
            target_snapshot: "snap-fixture".to_owned(),
        };
        assert!(
            validate(&plan, &profile).is_empty(),
            "{profile_name}: pin {ident} must validate first-pass"
        );
    }
    let macro_key = macro_path
        .strip_prefix("macro/")
        .expect("macro path must carry the macro/ head");
    let macro_entry = profile
        .param_by_ident(macro_key)
        .or_else(|| profile.param_by_name_regex(macro_key))
        .expect("macro fixture must resolve to a profile entry");
    let macro_plan = PatchPlan {
        ops: vec![PatchOp {
            op: PatchOpKind::Replace,
            path: IdentPath::new(macro_path),
            value: value_for(macro_entry.ui),
        }],
        target_snapshot: "snap-fixture".to_owned(),
    };
    assert!(
        validate(&macro_plan, &profile).is_empty(),
        "{profile_name}: {macro_path} must validate first-pass"
    );
}

#[test]
fn reaeq_pins_validate() {
    // b-matrix-01-stock.out.txt lines 8, 44, 161.
    check_plugin_pins(
        "reaeq",
        &["0:_Freq_Low_Shelf", "4:_Gain_Band_2", "17:wet"],
        "macro/Global Gain",
    );
}

#[test]
fn ott_pins_validate() {
    // b-matrix-03-vst3.out.txt lines 8, 16, 24.
    check_plugin_pins("ott", &["0:0", "1:1", "2:2"], "macro/Depth");
}

#[test]
fn pro_q4_pins_validate() {
    // b-matrix-03-vst3.out.txt lines 189, 197, 277.
    check_plugin_pins("pro-q4", &["2:2", "3:3", "13:13"], "macro/Band 1 Gain");
}

#[test]
fn serum2_fx_pins_validate() {
    // b-matrix-03-vst3.out.txt lines 482, 490, 402.
    check_plugin_pins("serum2-fx", &["18:19", "19:20", "8:8"], "macro/Main Vol");
}

#[test]
fn vital_clap_pins_validate() {
    // b-matrix-04-clap.out.txt lines 14, 50, 56.
    check_plugin_pins("vital-clap", &["1:49", "6:54", "7:55"], "macro/Macro 1");
}

#[test]
fn js_general_dynamics_pins_validate() {
    // b-matrix-02-ident-env.out.txt lines 11-13 (stable semantic idents;
    // bare "4"/"8" are rejected as BareIndex by design).
    check_plugin_pins(
        "js-general-dynamics",
        &["11:wet", "10:bypass", "12:delta"],
        r"macro/Wet Mix \(dB\)",
    );
}

#[test]
fn reacontrolmidi_pins_validate() {
    // b-matrix-01-stock.out.txt lines 254, 200, 272.
    check_plugin_pins(
        "reacontrolmidi",
        &["8:_通道", "2:_Program", "10:_Snap_to_Scale"],
        "macro/Transpose",
    );
}

// ---------------------------------------------------------------------------
// Valid corpus (28): 21 pin idents + 7 macro paths as single-op plans
// ---------------------------------------------------------------------------

fn valid_corpus() -> Vec<(PatchPlan, Profile)> {
    let mut corpus = Vec::new();
    let pins: &[(&str, &[&str], &str)] = &[
        (
            "reaeq",
            &["0:_Freq_Low_Shelf", "4:_Gain_Band_2", "17:wet"],
            "macro/Global Gain",
        ),
        ("ott", &["0:0", "1:1", "2:2"], "macro/Depth"),
        ("pro-q4", &["2:2", "3:3", "13:13"], "macro/Band 1 Gain"),
        ("serum2-fx", &["18:19", "19:20", "8:8"], "macro/Main Vol"),
        ("vital-clap", &["1:49", "6:54", "7:55"], "macro/Macro 1"),
        (
            "js-general-dynamics",
            &["11:wet", "10:bypass", "12:delta"],
            r"macro/Wet Mix \(dB\)",
        ),
        (
            "reacontrolmidi",
            &["8:_通道", "2:_Program", "10:_Snap_to_Scale"],
            "macro/Transpose",
        ),
    ];
    for (name, idents, macro_path) in pins {
        corpus.extend(plans_for_idents(name, idents));
        let profile = profile_of(name);
        let macro_key = macro_path
            .strip_prefix("macro/")
            .expect("macro path must carry the macro/ head");
        let macro_entry = profile
            .param_by_ident(macro_key)
            .or_else(|| profile.param_by_name_regex(macro_key))
            .expect("macro fixture must resolve to a profile entry");
        corpus.push((
            PatchPlan {
                ops: vec![PatchOp {
                    op: PatchOpKind::Replace,
                    path: IdentPath::new(*macro_path),
                    value: value_for(macro_entry.ui),
                }],
                target_snapshot: "snap-fixture".to_owned(),
            },
            profile,
        ));
    }
    corpus
}

// ---------------------------------------------------------------------------
// Fixable corpus (8): clamp / coerce / strip, with expected repaired values
// ---------------------------------------------------------------------------

struct Fixable {
    plan: PatchPlan,
    profile: Profile,
    expected_value: serde_json::Value,
}

fn fixable_corpus() -> Vec<Fixable> {
    let reaeq = profile_of("reaeq");
    let pro_q4 = profile_of("pro-q4");
    vec![
        Fixable {
            // Clamp high.
            plan: plan_of("param/0:_Freq_Low_Shelf", serde_json::json!(1.5), "reaeq").0,
            profile: reaeq.clone(),
            expected_value: serde_json::json!(1.0),
        },
        Fixable {
            // Clamp low.
            plan: plan_of("param/4:_Gain_Band_2", serde_json::json!(-0.2), "reaeq").0,
            profile: reaeq.clone(),
            expected_value: serde_json::json!(0.0),
        },
        Fixable {
            // Numeric string.
            plan: plan_of("param/17:wet", serde_json::json!("0.75"), "reaeq").0,
            profile: reaeq.clone(),
            expected_value: serde_json::json!(0.75),
        },
        Fixable {
            // Chained: coerce round 1, clamp round 2.
            plan: plan_of("param/0:_Freq_Low_Shelf", serde_json::json!("2.5"), "reaeq").0,
            profile: reaeq.clone(),
            expected_value: serde_json::json!(1.0),
        },
        Fixable {
            // Toggle from 1.0.
            plan: plan_of("param/16:bypass", serde_json::json!(1.0), "reaeq").0,
            profile: reaeq.clone(),
            expected_value: serde_json::Value::Bool(true),
        },
        Fixable {
            // Toggle from bool-word.
            plan: plan_of("param/16:bypass", serde_json::json!("off"), "reaeq").0,
            profile: reaeq.clone(),
            expected_value: serde_json::Value::Bool(false),
        },
        Fixable {
            // Select integral float to int.
            plan: plan_of("param/5:5", serde_json::json!(2.0), "pro-q4").0,
            profile: pro_q4,
            expected_value: serde_json::json!(2),
        },
        Fixable {
            // Remove with stray value: nulled, op kept.
            plan: PatchPlan {
                ops: vec![PatchOp {
                    op: PatchOpKind::Remove,
                    path: IdentPath::new("param/17:wet"),
                    value: serde_json::json!(0.5),
                }],
                target_snapshot: "snap-fixture".to_owned(),
            },
            profile: reaeq,
            expected_value: serde_json::Value::Null,
        },
    ]
}

// ---------------------------------------------------------------------------
// Unfixable corpus (12): initial kind asserted, final state BLOCKED
// ---------------------------------------------------------------------------

struct Unfixable {
    plan: PatchPlan,
    profile: Profile,
    initial_kind: PatchErrorKind,
}

fn unfixable_corpus() -> Vec<Unfixable> {
    let reaeq = profile_of("reaeq");
    let pro_q4 = profile_of("pro-q4");
    let serum = profile_of("serum2-fx");
    vec![
        Unfixable {
            // RFC 6902 move: no target state, dropped.
            plan: PatchPlan {
                ops: vec![PatchOp {
                    op: PatchOpKind::Invalid,
                    path: IdentPath::new("param/17:wet"),
                    value: serde_json::json!(0.5),
                }],
                target_snapshot: "snap-fixture".to_owned(),
            },
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::InvalidOp,
        },
        Unfixable {
            // JSON-pointer style.
            plan: plan_of("param", serde_json::json!(0.5), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::BadPath,
        },
        Unfixable {
            // Bare numeric index.
            plan: plan_of("param/4", serde_json::json!(0.5), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::BareIndex,
        },
        Unfixable {
            // Unknown ident (FromIdent -1 equivalent).
            plan: plan_of("param/99:nope", serde_json::json!(0.5), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::UnresolvableIdent,
        },
        Unfixable {
            // Chunk-only state via single-param op.
            plan: plan_of(
                "param/Mod(ulation)? Matrix|Routing",
                serde_json::json!(1),
                "serum2-fx",
            )
            .0,
            profile: serum,
            initial_kind: PatchErrorKind::PresetOnlySingleParam,
        },
        Unfixable {
            // macro/ namespace hitting a non-macro entry.
            plan: plan_of("macro/Wet", serde_json::json!(0.5), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::RoleNotWhitelisted,
        },
        Unfixable {
            // Array value has no scalar reading.
            plan: plan_of("param/17:wet", serde_json::json!([0.5]), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::InvalidValueType,
        },
        Unfixable {
            // Bool cannot drive a slider.
            plan: plan_of("param/17:wet", serde_json::Value::Bool(true), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::TypeMismatch,
        },
        Unfixable {
            // Toggle rejects 5.0 (only 0.0/1.0 coerce).
            plan: plan_of("param/16:bypass", serde_json::json!(5.0), "reaeq").0,
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::TypeMismatch,
        },
        Unfixable {
            // Plan-level: no snapshot to apply against.
            plan: PatchPlan {
                ops: vec![PatchOp {
                    op: PatchOpKind::Replace,
                    path: IdentPath::new("param/17:wet"),
                    value: serde_json::json!(0.5),
                }],
                target_snapshot: String::new(),
            },
            profile: reaeq.clone(),
            initial_kind: PatchErrorKind::EmptySnapshot,
        },
        Unfixable {
            // Fractional select index is ambiguous: dropped, not rounded.
            plan: plan_of("param/5:5", serde_json::json!(2.5), "pro-q4").0,
            profile: pro_q4,
            initial_kind: PatchErrorKind::TypeMismatch,
        },
        Unfixable {
            // Null carries nothing to write.
            plan: plan_of("param/17:wet", serde_json::Value::Null, "reaeq").0,
            profile: reaeq,
            initial_kind: PatchErrorKind::MissingValue,
        },
    ]
}

// ---------------------------------------------------------------------------
// DEC-013 gates
// ---------------------------------------------------------------------------

#[test]
fn first_pass_rate_clears_95_percent() {
    let corpus = valid_corpus();
    assert!(corpus.len() >= 20, "need 20+ fixtures");
    let passed = corpus
        .iter()
        .filter(|(plan, profile)| validate(plan, profile).is_empty())
        .count();
    let rate = passed as f64 / corpus.len() as f64;
    assert!(
        rate >= FIRST_PASS_GATE,
        "first-pass {passed}/{} = {rate:.3} < {FIRST_PASS_GATE}",
        corpus.len()
    );
}

#[test]
fn repair_rate_clears_99_percent_within_two_rounds() {
    let valid = valid_corpus();
    let fixable = fixable_corpus();
    let denominator = valid.len() + fixable.len();
    let mut accepted = 0usize;
    for (plan, profile) in &valid {
        let outcome = validate_with_repair(plan, profile, DEFAULT_MAX_REPAIR_ROUNDS);
        assert!(outcome.rounds_used() <= DEFAULT_MAX_REPAIR_ROUNDS as usize);
        if outcome.accepted() {
            accepted += 1;
        }
    }
    for case in &fixable {
        let outcome = validate_with_repair(&case.plan, &case.profile, DEFAULT_MAX_REPAIR_ROUNDS);
        assert!(outcome.rounds_used() <= DEFAULT_MAX_REPAIR_ROUNDS as usize);
        if outcome.accepted() {
            accepted += 1;
            assert_eq!(outcome.plan.ops.len(), 1);
            assert_eq!(outcome.plan.ops[0].value, case.expected_value);
        }
    }
    let rate = accepted as f64 / denominator as f64;
    assert!(
        rate >= REPAIR_GATE,
        "repaired {accepted}/{denominator} = {rate:.3} < {REPAIR_GATE}"
    );
}

#[test]
fn unfixable_fixtures_stay_blocked() {
    let corpus = unfixable_corpus();
    assert_eq!(corpus.len(), 12);
    for case in &corpus {
        let initial = validate(&case.plan, &case.profile);
        assert!(
            initial.iter().any(|error| error.kind == case.initial_kind),
            "missing {:?} in {initial:?}",
            case.initial_kind
        );
        let outcome = validate_with_repair(&case.plan, &case.profile, DEFAULT_MAX_REPAIR_ROUNDS);
        assert!(
            !outcome.accepted(),
            "{:?} must stay BLOCKED",
            case.initial_kind
        );
        assert!(!outcome.residual_errors.is_empty());
        for error in &outcome.residual_errors {
            assert!(error.blocked(), "{error:?} must be BLOCKED");
            assert!(!error.code().retryable(), "{error:?} must never retry");
            assert!(!error.guidance().is_empty());
        }
    }
}
