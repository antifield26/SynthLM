//! TSK-304 gates: diversity dedup, direction coverage, and six-field cards.
//!
//! Wiring note: `crates/planner/src/lib.rs` does not declare `mod candidate`
//! yet (main session owns that one-line收口), so this file compiles
//! `candidate.rs` directly through the `#[path]` module below, with a small
//! `patch` re-export shim standing in for `crate::patch` until the root
//! declaration lands. TODO(TSK-304/wiring): once `pub mod candidate;` lands
//! in `lib.rs`, delete the shim plus the `#[path]` module and import from the
//! library instead:
//! `use synthlm_planner::candidate::{...};` plus
//! `use synthlm_planner::patch::{...};`. The assertions stay valid as-is.

// Temporary pre-wiring shim: `candidate.rs` imports `crate::patch::PatchPlan`,
// so this test crate exposes a `patch` module that re-exports the real library
// item instead of compiling `patch.rs` a second time inside this test binary
// (which would duplicate its unit tests and its lints). TODO(TSK-304/wiring):
// delete this shim together with the `#[path]` module below once
// `pub mod candidate;` lands in `lib.rs`.
mod patch {
    pub use synthlm_planner::patch::PatchPlan;
}

#[path = "../src/candidate.rs"]
mod candidate;

use candidate::{
    Candidate, CandidateCard, CandidateError, DEFAULT_DEDUP_DISTANCE, Direction, DiversifyOutcome,
    MAX_CANDIDATES, MIN_CANDIDATES, PoolEntry, ScoreSnapshot, count_changed_ops, diversify,
    diversify_with_threshold, param_distance,
};
use synthlm_planner::patch::{IdentPath, PatchOp, PatchOpKind, PatchPlan};

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

fn patch_with_ops(n_ops: usize) -> PatchPlan {
    let mut ops = Vec::with_capacity(n_ops);
    for i in 0..n_ops {
        ops.push(PatchOp {
            op: PatchOpKind::Replace,
            path: IdentPath::new(format!("param/0:lane{i}")),
            value: serde_json::json!(0.5),
        });
    }
    PatchPlan {
        ops,
        target_snapshot: "snap-t304".to_owned(),
    }
}

fn candidate_with(
    id: &str,
    diff_zh: &str,
    confidence: f64,
    delta_lufs: f64,
    n_ops: usize,
    score_total: f64,
) -> Candidate {
    let patch = patch_with_ops(n_ops);
    let changed = count_changed_ops(&patch);
    let score = ScoreSnapshot::new(score_total).expect("fixture score must be finite");
    Candidate::new(
        id.to_owned(),
        patch,
        diff_zh.to_owned(),
        confidence,
        delta_lufs,
        changed,
        score,
    )
    .expect("fixture candidate must validate")
}

fn entry_with(candidate: Candidate, params: Vec<f64>, direction: Direction) -> PoolEntry {
    PoolEntry::new(candidate, params, direction).expect("fixture entry must validate")
}

fn ids_of(outcome: &DiversifyOutcome) -> Vec<&str> {
    outcome.candidates().iter().map(Candidate::id).collect()
}

// ---------------------------------------------------------------------------
// Distance unit behavior (proxy contract until CLAP lands)
// ---------------------------------------------------------------------------

#[test]
fn param_distance_zero_for_identical_vectors() {
    let a = vec![0.5, 0.25, 0.75];
    assert_eq!(param_distance(&a, &a), 0.0);
    assert_eq!(param_distance(&a, &[0.5, 0.25, 0.75]), 0.0);
}

#[test]
fn param_distance_matches_pythagoras() {
    assert_eq!(param_distance(&[0.0, 0.0], &[3.0, 4.0]), 5.0);
}

#[test]
fn param_distance_reads_missing_lanes_as_zero() {
    assert_eq!(param_distance(&[1.0], &[1.0, 1.0]), 1.0);
    assert_eq!(param_distance(&[], &[2.0]), 2.0);
}

// ---------------------------------------------------------------------------
// Dedup: homogeneous fixtures collapse, highest score survives
// ---------------------------------------------------------------------------

#[test]
fn homogeneous_fixtures_dedup_to_one() {
    // Three near-duplicates (per-lane drift ~1e-3, far below the default
    // radius); the highest score sits last in input order to prove ranking
    // beats input order.
    let pool = vec![
        entry_with(
            candidate_with("dup-low", "低频收紧，整体更暗。", 0.7, -0.4, 2, 0.70),
            vec![0.5, 0.5, 0.5, 0.5],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("dup-high", "低频收紧，整体更暗。", 0.9, -0.5, 2, 0.90),
            vec![0.501, 0.499, 0.5, 0.501],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("dup-mid", "低频收紧，整体更暗。", 0.8, -0.45, 2, 0.80),
            vec![0.499, 0.501, 0.5, 0.499],
            Direction::Darker,
        ),
    ];
    let outcome = diversify(&pool, 3);
    assert_eq!(outcome.candidates().len(), 1);
    assert_eq!(ids_of(&outcome), vec!["dup-high"]);
    // Only one direction can be covered: the gap must be flagged, never
    // silently padded with the dropped near-duplicates.
    assert!(outcome.direction_gap());
    assert_eq!(outcome.missing_directions().len(), 2);
    assert!(outcome.missing_directions().contains(&Direction::Transient));
    assert!(outcome.missing_directions().contains(&Direction::Spatial));
}

#[test]
fn zero_threshold_disables_dedup() {
    let pool = vec![
        entry_with(
            candidate_with("same-a", "低频收紧，整体更暗。", 0.9, -0.5, 1, 0.9),
            vec![0.5, 0.5],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("same-b", "低频收紧，整体更暗。", 0.8, -0.5, 1, 0.8),
            vec![0.5, 0.5],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("same-c", "低频收紧，整体更暗。", 0.7, -0.5, 1, 0.7),
            vec![0.5, 0.5],
            Direction::Darker,
        ),
    ];
    // Identical vectors have distance 0.0, which is not below a 0.0 radius.
    let outcome = diversify_with_threshold(&pool, 3, 0.0);
    assert_eq!(outcome.candidates().len(), 3);
    assert_eq!(ids_of(&outcome), vec!["same-a", "same-b", "same-c"]);
}

#[test]
fn distant_candidates_all_survive() {
    let pool = vec![
        entry_with(
            candidate_with("far-a", "低频收紧，整体更暗。", 0.9, -0.5, 2, 0.9),
            vec![0.0, 0.0],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("far-b", "起音更锐，瞬态更突出。", 0.8, 0.2, 1, 0.8),
            vec![10.0, 10.0],
            Direction::Transient,
        ),
        entry_with(
            candidate_with("far-c", "声场展宽，纵深拉开。", 0.7, 0.1, 3, 0.7),
            vec![20.0, 20.0],
            Direction::Spatial,
        ),
    ];
    let outcome = diversify(&pool, 3);
    assert_eq!(outcome.candidates().len(), 3);
    assert!(!outcome.direction_gap());
    assert!(outcome.missing_directions().is_empty());
    // Survivors come back score-ordered.
    assert_eq!(ids_of(&outcome), vec!["far-a", "far-b", "far-c"]);
}

// ---------------------------------------------------------------------------
// Direction coverage: missing direction is flagged, never invented
// ---------------------------------------------------------------------------

#[test]
fn missing_direction_is_flagged_not_invented() {
    let pool = vec![
        entry_with(
            candidate_with("only-dark", "低频收紧，整体更暗。", 0.9, -0.4, 2, 0.9),
            vec![0.0, 0.0],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("only-trans", "起音更锐，瞬态更突出。", 0.8, 0.2, 1, 0.8),
            vec![5.0, 5.0],
            Direction::Transient,
        ),
    ];
    let outcome = diversify(&pool, 3);
    assert_eq!(outcome.candidates().len(), 2);
    assert!(outcome.direction_gap());
    assert_eq!(outcome.missing_directions(), &[Direction::Spatial]);
}

#[test]
fn empty_pool_reports_full_gap() {
    let outcome = diversify(&[], 3);
    assert!(outcome.candidates().is_empty());
    assert!(outcome.direction_gap());
    assert_eq!(outcome.missing_directions().len(), 3);
}

#[test]
fn direction_labels_cover_the_three_dec018_terms() {
    assert_eq!(Direction::all().len(), 3);
    assert_eq!(Direction::Darker.as_zh(), "更暗");
    assert_eq!(Direction::Transient.as_zh(), "瞬态");
    assert_eq!(Direction::Spatial.as_zh(), "空间");
}

// ---------------------------------------------------------------------------
// Shortlist bounds: k clamps into 3..=5
// ---------------------------------------------------------------------------

#[test]
fn shortlist_bounds_default_to_three_and_cap_at_five() {
    assert_eq!(MIN_CANDIDATES, 3);
    assert_eq!(MAX_CANDIDATES, 5);
    // The default radius is behavioral, not a magic literal: an explicit pass
    // of DEFAULT_DEDUP_DISTANCE must agree with diversify on the same pool.
    let pool = vec![
        entry_with(
            candidate_with("eq-a", "低频收紧，整体更暗。", 0.9, -0.5, 1, 0.9),
            vec![0.5, 0.5],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("eq-b", "低频收紧，整体更暗。", 0.8, -0.5, 1, 0.8),
            vec![0.501, 0.499],
            Direction::Darker,
        ),
    ];
    let plain = diversify(&pool, 3);
    let explicit = diversify_with_threshold(&pool, 3, DEFAULT_DEDUP_DISTANCE);
    assert_eq!(ids_of(&plain), ids_of(&explicit));
}

#[test]
fn oversized_k_caps_at_five() {
    let mut pool = Vec::new();
    for i in 0..6 {
        let direction = Direction::all()[i % 3];
        let score = 0.95 - f64::from(i as u32) * 0.01;
        pool.push(entry_with(
            candidate_with(
                &format!("cap-{i}"),
                "声场展宽，纵深拉开。",
                0.5,
                0.0,
                1,
                score,
            ),
            vec![f64::from(i as u32) * 10.0],
            direction,
        ));
    }
    let outcome = diversify(&pool, 100);
    assert_eq!(outcome.candidates().len(), 5);
    assert!(!outcome.direction_gap());
}

#[test]
fn undersized_k_clamps_to_three() {
    let mut pool = Vec::new();
    for i in 0..5 {
        let direction = Direction::all()[i % 3];
        let score = 0.9 - f64::from(i as u32) * 0.01;
        pool.push(entry_with(
            candidate_with(
                &format!("floor-{i}"),
                "起音更锐，瞬态更突出。",
                0.6,
                0.1,
                1,
                score,
            ),
            vec![f64::from(i as u32) * 10.0],
            direction,
        ));
    }
    let outcome = diversify(&pool, 0);
    assert_eq!(outcome.candidates().len(), 3);
}

#[test]
fn non_finite_threshold_falls_back_to_default() {
    let pool = vec![
        entry_with(
            candidate_with("fb-a", "低频收紧，整体更暗。", 0.9, -0.5, 1, 0.9),
            vec![0.5, 0.5],
            Direction::Darker,
        ),
        entry_with(
            candidate_with("fb-b", "低频收紧，整体更暗。", 0.8, -0.5, 1, 0.8),
            vec![0.501, 0.499],
            Direction::Darker,
        ),
    ];
    let with_default = diversify(&pool, 3);
    let with_nan = diversify_with_threshold(&pool, 3, f64::NAN);
    let with_negative = diversify_with_threshold(&pool, 3, -1.0);
    assert_eq!(with_nan.candidates().len(), with_default.candidates().len());
    assert_eq!(
        with_negative.candidates().len(),
        with_default.candidates().len()
    );
}

// ---------------------------------------------------------------------------
// Candidate validation: confidence range, non-empty Chinese sentence
// ---------------------------------------------------------------------------

#[test]
fn confidence_boundaries_accepted_beyond_rejected() {
    for good in [0.0, 0.25, 1.0] {
        candidate_with("ok", "低频收紧，整体更暗。", good, 0.0, 1, 0.5);
    }
    for bad in [-0.1, 1.1, 2.0] {
        let patch = patch_with_ops(1);
        let score = ScoreSnapshot::new(0.5).expect("fixture score must be finite");
        let err = Candidate::new(
            "bad".to_owned(),
            patch,
            "低频收紧，整体更暗。".to_owned(),
            bad,
            0.0,
            1,
            score,
        )
        .expect_err("out-of-range confidence must be rejected");
        assert_eq!(err, CandidateError::ConfidenceOutOfRange);
    }
}

#[test]
fn non_finite_confidence_rejected_before_range() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let patch = patch_with_ops(1);
        let score = ScoreSnapshot::new(0.5).expect("fixture score must be finite");
        let err = Candidate::new(
            "bad".to_owned(),
            patch,
            "低频收紧，整体更暗。".to_owned(),
            bad,
            0.0,
            1,
            score,
        )
        .expect_err("non-finite confidence must be rejected");
        assert_eq!(err, CandidateError::NonFiniteConfidence);
    }
}

#[test]
fn empty_id_and_diff_rejected() {
    let score = ScoreSnapshot::new(0.5).expect("fixture score must be finite");
    let err = Candidate::new(
        "   ".to_owned(),
        patch_with_ops(1),
        "低频收紧，整体更暗。".to_owned(),
        0.5,
        0.0,
        1,
        score,
    )
    .expect_err("blank id must be rejected");
    assert_eq!(err, CandidateError::EmptyId);

    for blank in ["", "   "] {
        let score = ScoreSnapshot::new(0.5).expect("fixture score must be finite");
        let err = Candidate::new(
            "c".to_owned(),
            patch_with_ops(1),
            blank.to_owned(),
            0.5,
            0.0,
            1,
            score,
        )
        .expect_err("blank diff must be rejected");
        assert_eq!(err, CandidateError::EmptyDiff);
    }
}

#[test]
fn chinese_diff_sentence_roundtrips() {
    let text = "高频滚降提前，整体更暗但保持清晰。";
    let got = candidate_with("zh", text, 0.8, -1.2, 2, 0.6);
    assert_eq!(got.diff_summary_zh(), text);
    assert_eq!(got.id(), "zh");
    assert_eq!(got.confidence(), 0.8);
    assert_eq!(got.delta_lufs(), -1.2);
    assert_eq!(got.changed_params(), 2);
    assert_eq!(got.score_snapshot().total(), 0.6);
    assert_eq!(got.patch_summary().ops.len(), 2);
}

#[test]
fn non_finite_lufs_and_score_rejected() {
    let score = ScoreSnapshot::new(0.5).expect("fixture score must be finite");
    let err = Candidate::new(
        "c".to_owned(),
        patch_with_ops(1),
        "低频收紧，整体更暗。".to_owned(),
        0.5,
        f64::NAN,
        1,
        score,
    )
    .expect_err("NaN delta_lufs must be rejected");
    assert_eq!(err, CandidateError::NonFiniteDeltaLufs);

    let err = ScoreSnapshot::new(f64::INFINITY).expect_err("infinite score must fail");
    assert_eq!(err, CandidateError::NonFiniteScore);
}

#[test]
fn pool_entry_rejects_empty_or_non_finite_vectors() {
    let good = candidate_with("c", "低频收紧，整体更暗。", 0.5, 0.0, 1, 0.5);
    let err = PoolEntry::new(good.clone(), vec![], Direction::Darker)
        .expect_err("empty vector must be rejected");
    assert_eq!(err, CandidateError::EmptyParamVector);

    let err = PoolEntry::new(good, vec![0.5, f64::NAN], Direction::Darker)
        .expect_err("NaN lane must be rejected");
    assert_eq!(err, CandidateError::NonFiniteParam);
}

#[test]
fn pool_entry_score_mirrors_snapshot_total() {
    let entry = entry_with(
        candidate_with("mirror", "起音更锐，瞬态更突出。", 0.6, 0.3, 1, 0.77),
        vec![0.1, 0.9],
        Direction::Transient,
    );
    assert_eq!(entry.score(), 0.77);
    assert_eq!(entry.direction(), Direction::Transient);
    assert_eq!(entry.params(), &[0.1, 0.9]);
    assert_eq!(entry.candidate().id(), "mirror");
}

// ---------------------------------------------------------------------------
// Cards: constructor-level six-field guarantee plus serialization density
// ---------------------------------------------------------------------------

#[test]
fn card_carries_exactly_six_fields() {
    let card = CandidateCard::new(
        "低频收紧，整体更暗。".to_owned(),
        0.82,
        -1.4,
        2,
        "preview-cand-07.wav".to_owned(),
        "apply-07".to_owned(),
    )
    .expect("fixture card must validate");
    assert_eq!(card.diff(), "低频收紧，整体更暗。");
    assert_eq!(card.confidence(), 0.82);
    assert_eq!(card.delta_lufs(), -1.4);
    assert_eq!(card.changed(), 2);
    assert_eq!(card.audio_ref(), "preview-cand-07.wav");
    assert_eq!(card.apply_token(), "apply-07");

    let value = serde_json::to_value(&card).expect("card must serialize");
    let object = value.as_object().expect("card must serialize to an object");
    assert_eq!(object.len(), 6, "DEC-019 fixes the card at six fields");
    for key in [
        "diff",
        "confidence",
        "delta_lufs",
        "changed",
        "audio_ref",
        "apply_token",
    ] {
        assert!(object.contains_key(key), "card must carry `{key}`");
    }
}

#[test]
fn card_rejects_each_bad_field() {
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

    let err = CandidateCard::new(
        String::new(),
        0.5,
        0.0,
        1,
        "preview-x.wav".to_owned(),
        "apply-x".to_owned(),
    )
    .expect_err("empty diff must be rejected");
    assert_eq!(err, CandidateError::EmptyDiff);

    for bad in [1.5, -0.5] {
        let err = CandidateCard::new(
            "低频收紧，整体更暗。".to_owned(),
            bad,
            0.0,
            1,
            "preview-x.wav".to_owned(),
            "apply-x".to_owned(),
        )
        .expect_err("out-of-range card confidence must be rejected");
        assert_eq!(err, CandidateError::ConfidenceOutOfRange);
    }
    let err = CandidateCard::new(
        "低频收紧，整体更暗。".to_owned(),
        f64::NAN,
        0.0,
        1,
        "preview-x.wav".to_owned(),
        "apply-x".to_owned(),
    )
    .expect_err("NaN card confidence must be rejected");
    assert_eq!(err, CandidateError::NonFiniteConfidence);

    let err = CandidateCard::new(
        "低频收紧，整体更暗。".to_owned(),
        0.5,
        f64::INFINITY,
        1,
        "preview-x.wav".to_owned(),
        "apply-x".to_owned(),
    )
    .expect_err("infinite card delta must be rejected");
    assert_eq!(err, CandidateError::NonFiniteDeltaLufs);

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

    let err = CandidateCard::new(
        "低频收紧，整体更暗。".to_owned(),
        0.5,
        0.0,
        1,
        "preview-x.wav".to_owned(),
        String::new(),
    )
    .expect_err("blank apply_token must be rejected");
    assert_eq!(err, CandidateError::EmptyApplyToken);
}

#[test]
fn card_from_candidate_mirrors_four_fields() {
    let got = candidate_with("src", "声场展宽，纵深拉开。", 0.66, 0.3, 3, 0.71);
    let card =
        CandidateCard::from_candidate(&got, "preview-src.wav".to_owned(), "apply-src".to_owned())
            .expect("derivation must succeed");
    assert_eq!(card.diff(), got.diff_summary_zh());
    assert_eq!(card.confidence(), got.confidence());
    assert_eq!(card.delta_lufs(), got.delta_lufs());
    assert_eq!(card.changed(), got.changed_params());

    let err = CandidateCard::from_candidate(&got, String::new(), "apply-src".to_owned())
        .expect_err("blank audio_ref must fail derivation");
    assert_eq!(err, CandidateError::EmptyAudioRef);
}

#[test]
fn candidate_serializes_with_seven_named_fields() {
    let got = candidate_with("ser", "低频收紧，整体更暗。", 0.5, -0.8, 2, 0.6);
    let value = serde_json::to_value(&got).expect("candidate must serialize");
    let object = value
        .as_object()
        .expect("candidate must serialize to an object");
    assert_eq!(object.len(), 7);
    for key in [
        "id",
        "patch_summary",
        "diff_summary_zh",
        "confidence",
        "delta_lufs",
        "changed_params",
        "score_snapshot",
    ] {
        assert!(object.contains_key(key), "candidate must carry `{key}`");
    }
    assert_eq!(object["changed_params"], serde_json::json!(2));
}
