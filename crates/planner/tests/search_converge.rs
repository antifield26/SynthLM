//! TSK-303 convergence tests on the mock render: quadratic bowl within a
//! small budget, discrete shortlist picking the optimum, seed determinism,
//! and space/budget rejection.
//!
//! Pre-wiring note: same `#[path]` arrangement as `search_budget.rs` —
//! `crates/planner/src/lib.rs` does not yet declare `pub mod search` (main
//! session wires it); afterwards `use synthlm_planner::search::...` takes
//! over with identical assertions.

// Pre-wiring only: this copy of the module lives inside the test crate, so
// `pub` items this file does not touch read as dead. The canonical home is
// the lib (`pub mod search`), where exports never trip `dead_code`.
#[allow(dead_code)]
#[path = "../src/search.rs"]
mod search;

use search::{
    ContDim, DiscreteDim, MAX_DIMS, MAX_EVALS, MockBowl, Params, SearchConfig, SearchError,
    SearchSpace, StageOutcome, nelder_mead_refine, tpe_lite_search, two_stage_search,
};

fn bowl_space() -> SearchSpace {
    SearchSpace::new(vec![(-1.0, 1.0), (-1.0, 1.0)], vec![]).expect("2-D space")
}

#[test]
fn bowl_converges_within_30_evals() {
    let space = bowl_space();
    let mut mock = MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.0, 11);
    let config = SearchConfig::new(15, 15, 11);
    let out = two_stage_search(&mut mock, &space, &config).expect("search");
    assert!(
        out.evals_used <= 30,
        "must fit 30 evals, used {}",
        out.evals_used
    );
    assert_eq!(mock.evals(), u64::from(out.evals_used));
    assert!(
        out.value <= out.coarse_value,
        "refine must not regress: {} vs coarse {}",
        out.value,
        out.coarse_value
    );
    assert!(
        out.value < 1e-4,
        "2-D bowl must polish out, got {}",
        out.value
    );
    for (x, t) in out.best.continuous.iter().zip([0.3, -0.7]) {
        assert!((x - t).abs() < 1e-2, "near optimum, got {x}");
    }
}

#[test]
fn discrete_shortlist_picks_best() {
    let space = SearchSpace::new(
        vec![(0.0, 1.0)],
        vec![vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]],
    )
    .expect("mixed space");
    let mut mock = MockBowl::new(vec![0.7], vec![2], 5.0, 0.0, 5);
    let config = SearchConfig::new(20, 10, 5);
    let out = two_stage_search(&mut mock, &space, &config).expect("search");
    assert_eq!(out.best.discrete, vec![2], "shortlist must pick index 2");
    assert!(
        out.value < 1e-3,
        "must polish the continuous rest, got {}",
        out.value
    );
}

#[test]
fn same_seed_same_path() {
    let space = bowl_space();
    let run = |seed: u64| {
        let mut mock = MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.0, seed);
        two_stage_search(&mut mock, &space, &SearchConfig::new(12, 12, seed)).expect("search")
    };
    assert_eq!(run(21), run(21), "seeded runs must replay exactly");
    assert_ne!(
        run(21).value,
        two_stage_search(
            &mut MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.01, 21),
            &space,
            &SearchConfig::new(12, 12, 21),
        )
        .expect("noisy run")
        .value,
        "injected noise must actually perturb the path"
    );
}

#[test]
fn coarse_stage_spends_exactly_its_budget() {
    let space = bowl_space();
    let mut mock = MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.0, 2);
    let coarse: StageOutcome = tpe_lite_search(&mut mock, &space, 10, 2).expect("coarse");
    assert_eq!(coarse.evals_used, 10);
    assert_eq!(mock.evals(), 10);
    assert!(coarse.value.is_finite());
}

#[test]
fn refine_from_center_without_coarse() {
    // coarse = 0 starts Nelder-Mead at the space center.
    let space = bowl_space();
    let mut mock = MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.0, 9);
    let out = two_stage_search(&mut mock, &space, &SearchConfig::new(0, 40, 9)).expect("search");
    assert!(out.coarse_value.is_infinite(), "no coarse ran");
    assert!(
        out.value < 1e-4,
        "NM alone polishes a smooth bowl, got {}",
        out.value
    );
}

#[test]
fn nm_rejects_start_mismatch() {
    let space = bowl_space();
    let mut mock = MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.0, 1);
    let err = nelder_mead_refine(&mut mock, &space, &[], &[0.0], 10).expect_err("1 vs 2 dims");
    assert_eq!(err, SearchError::StartMismatch { got: 1, want: 2 });
}

#[test]
fn rejects_bad_space_and_config() {
    assert_eq!(MAX_DIMS, 8);
    assert_eq!(MAX_EVALS, 200);

    let nine = SearchSpace::new(vec![(0.0, 1.0); 9], vec![]).expect_err("9 dims");
    assert_eq!(
        nine,
        SearchError::TooManyDims {
            got: 9,
            max: MAX_DIMS
        }
    );
    assert_eq!(
        SearchSpace::new(vec![], vec![]).expect_err("empty"),
        SearchError::EmptySpace
    );
    assert_eq!(
        SearchSpace::new(vec![(2.0, 1.0)], vec![]).expect_err("min > max"),
        SearchError::BadRange { dim: 0 }
    );
    assert_eq!(
        SearchSpace::new(vec![(f64::NAN, 1.0)], vec![]).expect_err("NaN"),
        SearchError::BadRange { dim: 0 }
    );
    assert_eq!(
        SearchSpace::new(vec![(0.0, 1.0)], vec![vec![]]).expect_err("empty list"),
        SearchError::EmptyCandidates { dim: 0 }
    );

    let space = bowl_space();
    let mut mock = MockBowl::new(vec![0.3, -0.7], vec![], 0.0, 0.0, 1);
    assert_eq!(
        two_stage_search(&mut mock, &space, &SearchConfig::new(0, 0, 1)).expect_err("zero"),
        SearchError::ZeroBudget
    );
    assert_eq!(
        two_stage_search(&mut mock, &space, &SearchConfig::new(150, 100, 1))
            .expect_err("250 > 200"),
        SearchError::OverBudget {
            got: 250,
            max: MAX_EVALS
        }
    );
}

#[test]
fn accessors_and_center() {
    let space = SearchSpace::new(vec![(0.0, 2.0)], vec![vec!["x".to_owned(), "y".to_owned()]])
        .expect("mixed space");
    assert_eq!(space.dim(), 2);
    assert_eq!(space.cont_len(), 1);
    assert_eq!(space.disc_len(), 1);
    assert!(!space.is_empty());

    let dim: &ContDim = &space.continuous()[0];
    assert!((dim.width() - 2.0).abs() < 1e-12);
    assert!((dim.center() - 1.0).abs() < 1e-12);
    assert!((dim.clamp(9.0) - 2.0).abs() < 1e-12);

    let list: &DiscreteDim = &space.discrete()[0];
    assert_eq!(list.len(), 2);
    assert!(!list.is_empty());
    assert_eq!(list.candidate(1), Some("y"));
    assert_eq!(list.candidate(7), None);
    assert_eq!(list.candidates(), &["x".to_owned(), "y".to_owned()]);

    let center = Params::center(&space);
    assert_eq!(
        center,
        Params {
            continuous: vec![1.0],
            discrete: vec![0],
        }
    );

    let mock = MockBowl::new(vec![1.0], vec![1], 2.0, 0.0, 4);
    assert_eq!(mock.target_c(), &[1.0]);
    assert_eq!(mock.target_d(), &[1]);
    assert_eq!(mock.evals(), 0);
    assert!((mock.virtual_secs(5.0) - 0.0).abs() < 1e-12);

    let cfg = SearchConfig::new(20, 40, 8);
    assert_eq!(cfg.total_evals(), 60);
    assert!(cfg.validate().is_ok());
    assert!(SearchConfig::default().total_evals() <= MAX_EVALS);
}

#[test]
fn closure_objective_counts_calls() {
    let space = SearchSpace::new(vec![(0.0, 1.0)], vec![]).expect("1-D space");
    let mut calls = 0_u32;
    let mut obj = |p: &Params| {
        calls += 1;
        let d = p.continuous[0] - 0.4;
        d * d
    };
    let out: StageOutcome = tpe_lite_search(&mut obj, &space, 5, 6).expect("coarse");
    assert_eq!(out.evals_used, 5);
    assert_eq!(calls, 5);
    assert!(
        out.value < 0.05,
        "5 evals dent a 1-D bowl, got {}",
        out.value
    );
}
