//! TSK-303 budget-formula tests: research §5 `T = N × (t_render + t_metric) / P`.
//!
//! Pure arithmetic on a virtual clock: the mock counts evaluations and
//! converts counts × `t_render` to virtual seconds, so no test ever sleeps.
//!
//! Pre-wiring note: `crates/planner/src/lib.rs` does not yet declare
//! `pub mod search` (wired by the main session), so this file includes the
//! module by path. After wiring, `use synthlm_planner::search::...` takes
//! over; the assertions below are unchanged.

// Pre-wiring only: this copy of the module lives inside the test crate, so
// `pub` items this file does not touch read as dead. The canonical home is
// the lib (`pub mod search`), where exports never trip `dead_code`.
#[allow(dead_code)]
#[path = "../src/search.rs"]
mod search;

use search::{MockBowl, Objective, Params, SearchSpace, budget_secs, budget_secs_parallel};

#[test]
fn fifty_evals_at_three_seconds_is_minutes() {
    // §5.1 row 1: N=50 @3s ≈ 2.5min.
    let secs = budget_secs(50, 3.0);
    assert!(
        (secs - 150.0).abs() < 1e-9,
        "50x3s must be 150s, got {secs}"
    );
    assert!(
        (60.0..=600.0).contains(&secs),
        "150s must read as minute-level, got {secs}"
    );
}

#[test]
fn research_table_spots() {
    // §5.1: N=150 @5s ≈ 12.5min; N=300 @10s ≈ 50min.
    let mid = budget_secs(150, 5.0);
    assert!((mid - 750.0).abs() < 1e-9, "150x5s must be 750s, got {mid}");
    assert!((600.0..=900.0).contains(&mid));
    let big = budget_secs(300, 10.0);
    assert!(
        (big - 3000.0).abs() < 1e-9,
        "300x10s must be 3000s, got {big}"
    );
}

#[test]
fn parallelism_divides_and_zero_means_serial() {
    // §5.1 formula with P: 200x5s / 4 = 250s.
    let split = budget_secs_parallel(200, 5.0, 4);
    assert!((split - 250.0).abs() < 1e-9, "200x5s/4 must be 250s");
    // A zero parallelism degrades to serial instead of dividing by zero.
    let serial = budget_secs_parallel(50, 3.0, 0);
    assert!((serial - 150.0).abs() < 1e-12);
    // Serial helper equals P=1 exactly.
    let (a, b) = (budget_secs(50, 3.0), budget_secs_parallel(50, 3.0, 1));
    assert!((a - b).abs() < 1e-12);
}

#[test]
fn virtual_clock_counts_evals_without_sleeping() {
    let space = SearchSpace::new(vec![(0.0, 1.0)], vec![]).expect("1-D space");
    let mut mock = MockBowl::new(vec![0.5], vec![], 0.0, 0.0, 7);
    let before = std::time::Instant::now();
    for _ in 0..9 {
        mock.evaluate(&Params::center(&space));
    }
    let elapsed = before.elapsed();
    assert_eq!(mock.evals(), 9);
    assert!((mock.virtual_secs(3.0) - budget_secs(9, 3.0)).abs() < 1e-9);
    assert!(
        elapsed.as_secs() < 5,
        "virtual clock must not sleep: {elapsed:?}"
    );
}
