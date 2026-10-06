#![forbid(unsafe_code)]
// TSK-303 wiring: add `pub mod search;` to `crates/planner/src/lib.rs`.
// (Main session owns that line plus the `docs/LICENSES.md` / `Cargo.toml`
// follow-ups; this file stays dependency-free so the pre-wiring integration
// tests can `#[path]`-include it, see `crates/planner/tests/search_*.rs`.)

//! Two-stage local search over a mixed continuous/discrete parameter space.
//!
//! Implements the TSK-303 slice of `docs/research/C-models-retrieval-eval.md`
//! §5: a coarse global pass inside a 50–200 evaluation budget, then a local
//! polish pass.
//!
//! - Space ([`crate::search::SearchSpace`]): continuous dims as `[min, max]`
//!   ranges plus discrete dims as short candidate lists; the total defaults
//!   to at most [`crate::search::MAX_DIMS`] (8) dims.
//! - Cost ([`crate::search::Objective`]): `evaluate(params) -> f64`, smaller
//!   is better; the mock ([`crate::search::MockBowl`]) is a quadratic bowl
//!   plus a per-mismatch discrete penalty plus injectable uniform noise, and
//!   doubles as the mock render (each call counts one render+metric eval).
//! - Budget ([`crate::search::budget_secs`]): the research formula
//!   `T = N × (t_render + t_metric) / P` as a pure function (`t_metric` is
//!   folded into `t_render`; see §5.1, `t_render` dominates).
//! - Two stages ([`crate::search::two_stage_search`]): TPE-lite coarse
//!   search ([`crate::search::tpe_lite_search`]), then a hand-written
//!   Nelder-Mead refine over the continuous dims with the coarse discrete
//!   pick frozen ([`crate::search::nelder_mead_refine`]).
//!
//! ## Optimizer selection (verified 2026-10-06)
//!
//! Research §5.2 ranks `optimizer`'s `TpeSampler` first for mixed
//! discrete/continuous coarse search and the dedicated `cmaes` crate second
//! for continuous-only runs, and rules out `argmin`'s CMA-ES because PR #225
//! is still unmerged (`state: open`, `merged: false`,
//! <https://github.com/argmin-rs/argmin/pull/225>). License/version facts
//! re-checked today: `optimizer` 1.0.1 is `MIT`
//! (<https://crates.io/api/v1/crates/optimizer>,
//! <https://github.com/raimannma/rust-optimizer>), `cmaes` 0.2.2 is
//! `MIT OR Apache-2.0` (<https://crates.io/api/v1/crates/cmaes>,
//! <https://github.com/pengowen123/cmaes>) — both permissive, both eligible
//! for a later `docs/LICENSES.md` registration.
//!
//! Neither crate is wired here: this task forbids `Cargo.toml` edits, and
//! AGENTS.md red line 6 requires a `docs/LICENSES.md` entry for any new
//! dependency (likewise out of scope), so the coarse pass is a
//! zero-dependency hand-written TPE-lite. It mirrors the TPE shape
//! (`good`/`bad` split at the top quartile, per-dim `l(x)/g(x)` candidate
//! ranking) closely enough that swapping in `optimizer::TpeSampler` later is
//! mechanical; the pending-dependency table is in the TSK-303 return.
//!
//! Blocking contract: [`crate::search::Objective::evaluate`] stands in for a
//! render+metric round and may block the calling thread on a real renderer.
//! Never call search from an audio thread (AGENTS.md red line 2); this is a
//! control-plane helper.

use std::cmp::Ordering;

use thiserror::Error;

/// Default total-dimension cap (continuous + discrete dims, research §5.2:
/// simplex / surrogate methods stay healthy below ~10 dims).
pub const MAX_DIMS: usize = 8;

/// Single-run evaluation cap (research §5.1: one search is 50–200 evals).
pub const MAX_EVALS: u32 = 200;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Search-space / budget construction failure (value-free: counts only, so
/// formatting an error can never echo caller material).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SearchError {
    /// No dims at all (need at least one continuous or discrete dim).
    #[error("search space is empty: add at least one continuous or discrete dim")]
    EmptySpace,
    /// Total dims exceed the cap.
    #[error("too many dims: got {got}, cap is {max}")]
    TooManyDims {
        /// Requested total dims.
        got: usize,
        /// Cap in force ([`crate::search::MAX_DIMS`]).
        max: usize,
    },
    /// Continuous range is not finite `min < max` (`dim` is the 0-based
    /// continuous-dim index).
    #[error("bad range on continuous dim {dim}: need finite min < max")]
    BadRange {
        /// 0-based continuous-dim index.
        dim: usize,
    },
    /// Discrete dim has an empty candidate list (`dim` is the 0-based
    /// discrete-dim index).
    #[error("discrete dim {dim} has no candidates: keep a short non-empty list")]
    EmptyCandidates {
        /// 0-based discrete-dim index.
        dim: usize,
    },
    /// Coarse + refine budget is zero (nothing would ever be evaluated).
    #[error("zero budget: coarse + refine evals must be at least 1")]
    ZeroBudget,
    /// Coarse + refine budget exceeds the single-run cap.
    #[error("over budget: got {got} evals, cap is {max}")]
    OverBudget {
        /// Requested total evals.
        got: u32,
        /// Cap in force ([`crate::search::MAX_EVALS`]).
        max: u32,
    },
    /// Refine start-point length mismatches the continuous dims.
    #[error("start point length {got} mismatches continuous dims {want}")]
    StartMismatch {
        /// Length of the given start point.
        got: usize,
        /// Continuous dims of the space.
        want: usize,
    },
}

// ---------------------------------------------------------------------------
// Space
// ---------------------------------------------------------------------------

/// One continuous dim: a finite `[min, max]` range with `min < max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContDim {
    /// Range lower bound (inclusive).
    pub min: f64,
    /// Range upper bound (inclusive, above `min`).
    pub max: f64,
}

impl ContDim {
    /// Range width (`max - min`, always positive by construction).
    #[must_use]
    pub fn width(&self) -> f64 {
        self.max - self.min
    }

    /// Clip `value` into `[min, max]`.
    #[must_use]
    pub fn clamp(&self, value: f64) -> f64 {
        value.clamp(self.min, self.max)
    }

    /// Range midpoint (the default start / zero-budget answer).
    #[must_use]
    pub fn center(&self) -> f64 {
        0.5 * (self.min + self.max)
    }
}

/// One discrete dim: a short non-empty candidate list (preset shortlist).
#[derive(Clone, Debug, PartialEq)]
pub struct DiscreteDim {
    candidates: Vec<String>,
}

impl DiscreteDim {
    /// How many candidates this dim offers (always ≥ 1 by construction).
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether this dim offers no candidates (always `false` by
    /// construction; kept so `len` has its `is_empty` pair).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Candidate label at `index` (`None` when out of range).
    #[must_use]
    pub fn candidate(&self, index: usize) -> Option<&str> {
        self.candidates.get(index).map(String::as_str)
    }

    /// All candidate labels, in index order.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }
}

/// Mixed search space: continuous `[min, max]` dims plus discrete shortlist
/// dims, at most [`crate::search::MAX_DIMS`] in total.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchSpace {
    continuous: Vec<ContDim>,
    discrete: Vec<DiscreteDim>,
}

impl SearchSpace {
    /// Build a space, validating ranges, candidate lists, and the total-dim
    /// cap.
    ///
    /// # Errors
    ///
    /// Returns [`crate::search::SearchError::EmptySpace`] for zero dims,
    /// [`crate::search::SearchError::TooManyDims`] past
    /// [`crate::search::MAX_DIMS`],
    /// [`crate::search::SearchError::BadRange`] for a non-finite or
    /// non-ascending range, or
    /// [`crate::search::SearchError::EmptyCandidates`] for an empty list.
    pub fn new(
        continuous: Vec<(f64, f64)>,
        discrete: Vec<Vec<String>>,
    ) -> Result<Self, SearchError> {
        if continuous.is_empty() && discrete.is_empty() {
            return Err(SearchError::EmptySpace);
        }
        let total = continuous.len() + discrete.len();
        if total > MAX_DIMS {
            return Err(SearchError::TooManyDims {
                got: total,
                max: MAX_DIMS,
            });
        }
        let mut cont = Vec::with_capacity(continuous.len());
        for (dim, (min, max)) in continuous.into_iter().enumerate() {
            if !min.is_finite() || !max.is_finite() || max <= min {
                return Err(SearchError::BadRange { dim });
            }
            cont.push(ContDim { min, max });
        }
        let mut disc = Vec::with_capacity(discrete.len());
        for (dim, candidates) in discrete.into_iter().enumerate() {
            if candidates.is_empty() {
                return Err(SearchError::EmptyCandidates { dim });
            }
            disc.push(DiscreteDim { candidates });
        }
        Ok(Self {
            continuous: cont,
            discrete: disc,
        })
    }

    /// Total dims (continuous + discrete).
    #[must_use]
    pub fn dim(&self) -> usize {
        self.continuous.len() + self.discrete.len()
    }

    /// How many continuous dims.
    #[must_use]
    pub fn cont_len(&self) -> usize {
        self.continuous.len()
    }

    /// How many discrete dims.
    #[must_use]
    pub fn disc_len(&self) -> usize {
        self.discrete.len()
    }

    /// Whether the space has no dims (always `false` by construction).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.continuous.is_empty() && self.discrete.is_empty()
    }

    /// Continuous dims, in order.
    #[must_use]
    pub fn continuous(&self) -> &[ContDim] {
        &self.continuous
    }

    /// Discrete dims, in order.
    #[must_use]
    pub fn discrete(&self) -> &[DiscreteDim] {
        &self.discrete
    }
}

// ---------------------------------------------------------------------------
// Params + objective
// ---------------------------------------------------------------------------

/// One evaluated point: continuous values plus per-discrete-dim candidate
/// indices.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Continuous values (same order/length as the space dims).
    pub continuous: Vec<f64>,
    /// Candidate index per discrete dim.
    pub discrete: Vec<usize>,
}

impl Params {
    /// Space center: continuous midpoints, discrete index 0 (the default
    /// start / zero-budget answer).
    #[must_use]
    pub fn center(space: &SearchSpace) -> Self {
        Self {
            continuous: space.continuous().iter().map(ContDim::center).collect(),
            discrete: vec![0; space.disc_len()],
        }
    }
}

/// Cost function: one mock render+metric round. Smaller is better;
/// non-finite returns are treated as `+inf` by the search loops.
pub trait Objective {
    /// Evaluate `params` (counts one evaluation against the budget).
    fn evaluate(&mut self, params: &Params) -> f64;
}

/// Any `FnMut(&Params) -> f64` closure works as an
/// [`crate::search::Objective`] (handy for one-off bowls in tests).
impl<F> Objective for F
where
    F: FnMut(&Params) -> f64,
{
    fn evaluate(&mut self, params: &Params) -> f64 {
        self(params)
    }
}

/// Mock render+metric: quadratic bowl in the continuous dims, plus
/// `discrete_penalty` per mismatched discrete dim, plus injectable uniform
/// noise in `±noise_amp` (0.0 = deterministic). Counts every call, so the
/// counter doubles as the virtual clock (`evals × t_render`, never a real
/// sleep).
#[derive(Clone, Debug)]
pub struct MockBowl {
    target_c: Vec<f64>,
    target_d: Vec<usize>,
    discrete_penalty: f64,
    noise_amp: f64,
    rng: Rng,
    evals: u64,
}

impl MockBowl {
    /// Build a bowl with continuous optimum `target_c`, discrete optimum
    /// `target_d`, per-mismatch `discrete_penalty`, uniform noise amplitude
    /// `noise_amp`, and a seeded stream `seed`.
    #[must_use]
    pub fn new(
        target_c: Vec<f64>,
        target_d: Vec<usize>,
        discrete_penalty: f64,
        noise_amp: f64,
        seed: u64,
    ) -> Self {
        Self {
            target_c,
            target_d,
            discrete_penalty,
            noise_amp,
            rng: Rng::new(seed),
            evals: 0,
        }
    }

    /// Evaluations performed so far (the virtual-clock tick count).
    #[must_use]
    pub fn evals(&self) -> u64 {
        self.evals
    }

    /// Virtual seconds spent: `evals × t_render_secs` (no clock read, no
    /// sleep — the mock render is instant).
    #[must_use]
    pub fn virtual_secs(&self, t_render_secs: f64) -> f64 {
        self.evals as f64 * t_render_secs
    }

    /// Continuous optimum.
    #[must_use]
    pub fn target_c(&self) -> &[f64] {
        &self.target_c
    }

    /// Discrete optimum (candidate indices).
    #[must_use]
    pub fn target_d(&self) -> &[usize] {
        &self.target_d
    }
}

impl Objective for MockBowl {
    fn evaluate(&mut self, params: &Params) -> f64 {
        self.evals = self.evals.saturating_add(1);
        let mut value = 0.0;
        for (x, t) in params.continuous.iter().zip(self.target_c.iter()) {
            let d = x - t;
            value += d * d;
        }
        for (got, want) in params.discrete.iter().zip(self.target_d.iter()) {
            if got != want {
                value += self.discrete_penalty;
            }
        }
        if self.noise_amp > 0.0 {
            value += (self.rng.next_f64() * 2.0 - 1.0) * self.noise_amp;
        }
        value
    }
}

// ---------------------------------------------------------------------------
// Budget
// ---------------------------------------------------------------------------

/// Research §5.1 budget, serial DAW case (`P = 1`): `T = N × t_render`
/// seconds, where `t_render` already folds in `t_metric` (metric time is
/// ms–100ms vs seconds of render).
///
/// Pure function — no clock, no sleep. E.g. 50 evals at 3 s render is
/// minute-level:
///
/// ```rust
/// use synthlm_planner::search::budget_secs;
///
/// assert!((budget_secs(50, 3.0) - 150.0).abs() < 1e-9);
/// ```
#[must_use]
pub fn budget_secs(n_evals: u32, t_render_secs: f64) -> f64 {
    budget_secs_parallel(n_evals, t_render_secs, 1)
}

/// Research §5.1 budget, general case: `T = N × t_render / P`. A
/// `parallelism` of 0 is treated as 1 (serial) so the divisor can never be
/// zero.
#[must_use]
pub fn budget_secs_parallel(n_evals: u32, t_render_secs: f64, parallelism: u32) -> f64 {
    f64::from(n_evals) * t_render_secs / f64::from(parallelism.max(1))
}

// ---------------------------------------------------------------------------
// Config + outcomes
// ---------------------------------------------------------------------------

/// Two-stage budget split plus seed (one seed drives both stages, so a whole
/// run replays deterministically).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchConfig {
    /// Coarse (TPE-lite) evaluations.
    pub coarse_evals: u32,
    /// Refine (Nelder-Mead) evaluations.
    pub refine_evals: u32,
    /// Seed for the coarse sampler.
    pub seed: u64,
}

impl SearchConfig {
    /// Build a config (either stage may be 0 to skip it; the total must be
    /// 1–[`crate::search::MAX_EVALS`]).
    #[must_use]
    pub fn new(coarse_evals: u32, refine_evals: u32, seed: u64) -> Self {
        Self {
            coarse_evals,
            refine_evals,
            seed,
        }
    }

    /// Total evaluations (`coarse + refine`, saturating).
    #[must_use]
    pub fn total_evals(self) -> u32 {
        self.coarse_evals.saturating_add(self.refine_evals)
    }

    /// Check the total against the run cap.
    ///
    /// # Errors
    ///
    /// Returns [`crate::search::SearchError::ZeroBudget`] for a zero total
    /// or [`crate::search::SearchError::OverBudget`] past
    /// [`crate::search::MAX_EVALS`].
    pub fn validate(self) -> Result<(), SearchError> {
        let total = self.total_evals();
        if total == 0 {
            return Err(SearchError::ZeroBudget);
        }
        if total > MAX_EVALS {
            return Err(SearchError::OverBudget {
                got: total,
                max: MAX_EVALS,
            });
        }
        Ok(())
    }
}

impl Default for SearchConfig {
    /// 20 coarse + 40 refine = 60 evals (inside the 50–200 run band).
    fn default() -> Self {
        Self {
            coarse_evals: 20,
            refine_evals: 40,
            seed: 0x5EED_3033,
        }
    }
}

/// One stage outcome: best point, its value, and evals actually spent
/// (refine may stop early on a flat simplex, so this can be below budget).
#[derive(Clone, Debug, PartialEq)]
pub struct StageOutcome {
    /// Best point found.
    pub best: Params,
    /// Its value (smaller is better).
    pub value: f64,
    /// Evaluations actually spent.
    pub evals_used: u32,
}

/// Whole-run outcome: refined best plus the coarse value it started from
/// (`value <= coarse_value` always holds when refine ran at least one eval,
/// since the simplex is seeded at the coarse best).
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    /// Refined best point.
    pub best: Params,
    /// Its value (smaller is better).
    pub value: f64,
    /// Evaluations spent across both stages.
    pub evals_used: u32,
    /// Coarse-stage best value (pre-refine reference).
    pub coarse_value: f64,
}

// ---------------------------------------------------------------------------
// Stage 1: TPE-lite coarse search (zero-dependency; see module docs)
// ---------------------------------------------------------------------------

/// Coarse global pass: uniform startup, then model-based picks.
///
/// The first `min(max_evals, 8)` evals are uniform (at least 1, and the whole
/// budget when it is tiny); each later eval draws 8 candidates from the
/// top-quartile (`good`) marginals — continuous dims perturbed ±10 % of
/// range around a random `good` point, discrete dims by `good` majority vote
/// with 20 % uniform resample — and evaluates the one maximizing the
/// log-density ratio `log l(x) - log g(x)` (`l` = `good` model: per-dim
/// Gaussian / Laplace-smoothed categorical; `g` = background: uniform range
/// / all-seen smoothed categorical).
///
/// Deterministic in `seed`; spends exactly `max_evals` evals (or none when
/// `max_evals` is 0, returning the space center unevaluated).
///
/// # Errors
///
/// Returns [`crate::search::SearchError::EmptySpace`] for an empty space.
pub fn tpe_lite_search<O: Objective>(
    obj: &mut O,
    space: &SearchSpace,
    max_evals: u32,
    seed: u64,
) -> Result<StageOutcome, SearchError> {
    if space.is_empty() {
        return Err(SearchError::EmptySpace);
    }
    if max_evals == 0 {
        return Ok(StageOutcome {
            best: Params::center(space),
            value: f64::INFINITY,
            evals_used: 0,
        });
    }
    let mut rng = Rng::new(seed);
    let startup = max_evals.min(8).max(max_evals.min(2));
    let mut seen: Vec<(Params, f64)> = Vec::with_capacity(max_evals as usize);
    for _ in 0..startup {
        let p = sample_uniform(space, &mut rng);
        let v = eval_capped(obj, &p);
        seen.push((p, v));
    }
    let mut remaining = max_evals - startup;
    while remaining > 0 {
        let cand = propose(space, &seen, &mut rng);
        let v = eval_capped(obj, &cand);
        seen.push((cand, v));
        remaining -= 1;
    }
    let mut best_idx = 0;
    for (i, (_, v)) in seen.iter().enumerate().skip(1) {
        if *v < seen[best_idx].1 {
            best_idx = i;
        }
    }
    let (best, value) = seen[best_idx].clone();
    Ok(StageOutcome {
        best,
        value,
        evals_used: max_evals,
    })
}

// ---------------------------------------------------------------------------
// Stage 2: hand-written Nelder-Mead refine (continuous dims only)
// ---------------------------------------------------------------------------

/// Refine the continuous dims with Nelder-Mead from `start` (the coarse
/// best); `discrete` is frozen (missing entries default to index 0).
///
/// Standard coefficients (reflect 1.0 / expand 2.0 / contract ±0.5 / shrink
/// 0.5), all vertices clipped to the space, non-finite values capped to
/// `+inf`, early stop when the value spread drops below 1e-9. Never spends
/// more than `max_evals` (0 returns the clipped start unevaluated).
///
/// # Errors
///
/// Returns [`crate::search::SearchError::EmptySpace`] for a space with no
/// continuous dims, or
/// [`crate::search::SearchError::StartMismatch`] when `start` misses the
/// continuous-dim count.
///
/// # Panics
///
/// Panics when the continuous dims exceed [`crate::search::MAX_DIMS`] (the
/// simplex costs `n + 1` evals before it can take a single step, so wider
/// spaces are rejected loudly instead of burning the budget).
pub fn nelder_mead_refine<O: Objective>(
    obj: &mut O,
    space: &SearchSpace,
    discrete: &[usize],
    start: &[f64],
    max_evals: u32,
) -> Result<StageOutcome, SearchError> {
    let n = space.cont_len();
    if n == 0 {
        return Err(SearchError::EmptySpace);
    }
    assert!(
        n <= MAX_DIMS,
        "Nelder-Mead needs at most {MAX_DIMS} continuous dims, got {n}"
    );
    if start.len() != n {
        return Err(SearchError::StartMismatch {
            got: start.len(),
            want: n,
        });
    }
    let fixed: Vec<usize> = (0..space.disc_len())
        .map(|j| discrete.get(j).copied().unwrap_or(0))
        .collect();
    let eval1 = |x: &[f64], o: &mut O| eval_cont(o, space, &fixed, x);
    if max_evals == 0 {
        let mut p = Params {
            continuous: start.to_vec(),
            discrete: fixed,
        };
        clip_to_space(space, &mut p);
        return Ok(StageOutcome {
            best: p,
            value: f64::INFINITY,
            evals_used: 0,
        });
    }
    let mut verts = initial_simplex(space, start);
    let mut vals: Vec<f64> = Vec::with_capacity(verts.len());
    let mut evals: u32 = 0;
    for v in &verts {
        if evals >= max_evals {
            break;
        }
        vals.push(eval1(v, obj));
        evals += 1;
    }
    verts.truncate(vals.len());
    if verts.len() < 2 || evals >= max_evals {
        let b = argmin(&vals);
        let mut p = Params {
            continuous: verts[b].clone(),
            discrete: fixed,
        };
        clip_to_space(space, &mut p);
        return Ok(StageOutcome {
            best: p,
            value: vals[b],
            evals_used: evals,
        });
    }
    while evals < max_evals {
        sort_simplex(&mut verts, &mut vals);
        let spread = vals
            .iter()
            .fold(0.0, |m: f64, &v| m.max((v - vals[0]).abs()));
        if spread < 1e-9 {
            break;
        }
        let cent = centroid(&verts[..n]);
        let worst = verts[n].clone();
        let xr = comb(&cent, &worst, 1.0);
        let fr = eval1(&xr, obj);
        evals += 1;
        if fr < vals[0] {
            let xe = comb(&cent, &worst, 2.0);
            if evals >= max_evals {
                verts[n] = xr;
                vals[n] = fr;
                break;
            }
            let fe = eval1(&xe, obj);
            evals += 1;
            if fe < fr {
                verts[n] = xe;
                vals[n] = fe;
            } else {
                verts[n] = xr;
                vals[n] = fr;
            }
        } else if fr < vals[n - 1] {
            verts[n] = xr;
            vals[n] = fr;
        } else {
            let xc = if fr < vals[n] {
                comb(&cent, &worst, 0.5)
            } else {
                comb(&cent, &worst, -0.5)
            };
            if evals >= max_evals {
                break;
            }
            let fc = eval1(&xc, obj);
            evals += 1;
            if fc < vals[n].min(fr) {
                verts[n] = xc;
                vals[n] = fc;
            } else {
                let best = verts[0].clone();
                for (vtx, f) in verts.iter_mut().zip(vals.iter_mut()).skip(1) {
                    if evals >= max_evals {
                        break;
                    }
                    let xs: Vec<f64> = best
                        .iter()
                        .zip(vtx.iter())
                        .map(|(&b, &v)| b + 0.5 * (v - b))
                        .collect();
                    let fs = eval1(&xs, obj);
                    evals += 1;
                    *vtx = xs;
                    *f = fs;
                }
            }
        }
    }
    let b = argmin(&vals);
    let mut p = Params {
        continuous: verts[b].clone(),
        discrete: fixed,
    };
    clip_to_space(space, &mut p);
    Ok(StageOutcome {
        best: p,
        value: vals[b],
        evals_used: evals,
    })
}

// ---------------------------------------------------------------------------
// Two-stage driver
// ---------------------------------------------------------------------------

/// Coarse TPE-lite pass, then Nelder-Mead polish from the coarse best with
/// its discrete pick frozen. Either stage may be 0 (skipped); the total must
/// fit [`crate::search::SearchConfig::validate`].
///
/// # Errors
///
/// Returns [`crate::search::SearchError::EmptySpace`] for an empty space, or
/// the [`crate::search::SearchConfig::validate`] budget errors.
pub fn two_stage_search<O: Objective>(
    obj: &mut O,
    space: &SearchSpace,
    config: &SearchConfig,
) -> Result<SearchResult, SearchError> {
    if space.is_empty() {
        return Err(SearchError::EmptySpace);
    }
    config.validate()?;
    let coarse = if config.coarse_evals == 0 {
        StageOutcome {
            best: Params::center(space),
            value: f64::INFINITY,
            evals_used: 0,
        }
    } else {
        tpe_lite_search(obj, space, config.coarse_evals, config.seed)?
    };
    let refined = if config.refine_evals == 0 {
        coarse.clone()
    } else {
        nelder_mead_refine(
            obj,
            space,
            &coarse.best.discrete,
            &coarse.best.continuous,
            config.refine_evals,
        )?
    };
    let evals_used = coarse.evals_used.saturating_add(refined.evals_used);
    Ok(SearchResult {
        best: refined.best,
        value: refined.value,
        evals_used,
        coarse_value: coarse.value,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Deterministic xorshift64* stream (no `rand` dependency; tests replay by
/// seed).
#[derive(Clone, Debug)]
struct Rng(u64);

impl Rng {
    /// Build a stream (a zero seed is salted so the state can never stick).
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

    /// Uniform index in `0..n` (`n == 0` yields 0).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_f64() * n as f64) as usize
    }
}

/// Evaluate with non-finite results capped to `+inf` (a flat worst that
/// sorts and compares sanely).
fn eval_capped<O: Objective>(obj: &mut O, params: &Params) -> f64 {
    let v = obj.evaluate(params);
    if v.is_finite() { v } else { f64::INFINITY }
}

/// Evaluate a continuous point with `fixed` discrete indices (clipped to
/// the space first).
fn eval_cont<O: Objective>(obj: &mut O, space: &SearchSpace, fixed: &[usize], x: &[f64]) -> f64 {
    let mut p = Params {
        continuous: x.to_vec(),
        discrete: fixed.to_vec(),
    };
    clip_to_space(space, &mut p);
    eval_capped(obj, &p)
}

/// Clip continuous values into range and discrete indices into their lists
/// (lists are non-empty by construction).
fn clip_to_space(space: &SearchSpace, p: &mut Params) {
    for (v, dim) in p.continuous.iter_mut().zip(space.continuous().iter()) {
        *v = dim.clamp(*v);
    }
    for (i, dim) in p.discrete.iter_mut().zip(space.discrete().iter()) {
        if *i >= dim.len() {
            *i = dim.len() - 1;
        }
    }
}

/// Uniform sample over the whole space.
fn sample_uniform(space: &SearchSpace, rng: &mut Rng) -> Params {
    Params {
        continuous: space
            .continuous()
            .iter()
            .map(|dim| dim.min + rng.next_f64() * dim.width())
            .collect(),
        discrete: space
            .discrete()
            .iter()
            .map(|dim| rng.below(dim.len()))
            .collect(),
    }
}

/// Index of the smallest value (`vals` must be non-empty; values are capped
/// finite-or-`+inf` so `<` totally orders them).
fn argmin(vals: &[f64]) -> usize {
    let mut best = 0;
    for (i, &v) in vals.iter().enumerate().skip(1) {
        if v < vals[best] {
            best = i;
        }
    }
    best
}

/// TPE propose: top-quartile `good` set, 8 candidates from the `good`
/// marginals, keep the max log-density-ratio one.
fn propose(space: &SearchSpace, seen: &[(Params, f64)], rng: &mut Rng) -> Params {
    let mut order: Vec<usize> = (0..seen.len()).collect();
    order.sort_by(|&a, &b| seen[a].1.partial_cmp(&seen[b].1).unwrap_or(Ordering::Equal));
    let good_n = (seen.len() / 4).max(1).min(seen.len());
    let good: Vec<&Params> = order[..good_n].iter().map(|&i| &seen[i].0).collect();
    let mut cand = sample_from_good(space, &good, rng);
    let mut score = density_log_ratio(space, seen, &good, &cand);
    for _ in 1..8 {
        let next = sample_from_good(space, &good, rng);
        let next_score = density_log_ratio(space, seen, &good, &next);
        if next_score > score {
            cand = next;
            score = next_score;
        }
    }
    cand
}

/// Sample near the `good` set: continuous dims perturbed ±10 % of range
/// around a random `good` point (clipped); discrete dims by `good` majority
/// vote with 20 % uniform resample.
fn sample_from_good(space: &SearchSpace, good: &[&Params], rng: &mut Rng) -> Params {
    let anchor = good[rng.below(good.len())];
    let mut p = Params {
        continuous: anchor.continuous.clone(),
        discrete: anchor.discrete.clone(),
    };
    for (v, dim) in p.continuous.iter_mut().zip(space.continuous().iter()) {
        let w = 0.1 * dim.width();
        *v = dim.clamp(*v + (rng.next_f64() * 2.0 - 1.0) * w);
    }
    for (j, (slot, dim)) in p
        .discrete
        .iter_mut()
        .zip(space.discrete().iter())
        .enumerate()
    {
        if rng.next_f64() < 0.2 {
            *slot = rng.below(dim.len());
        } else {
            let mut counts = vec![0_usize; dim.len()];
            for g in good {
                if g.discrete[j] < counts.len() {
                    counts[g.discrete[j]] += 1;
                }
            }
            let mut top = 0;
            for (i, &c) in counts.iter().enumerate().skip(1) {
                if c > counts[top] {
                    top = i;
                }
            }
            *slot = top;
        }
    }
    p
}

/// Log-density ratio `log l(cand) - log g(cand)`: continuous dims score a
/// Gaussian around the `good` mean (std floored at 5 % of range) against a
/// uniform background; discrete dims score Laplace-smoothed `good`
/// frequency against all-seen frequency.
fn density_log_ratio(
    space: &SearchSpace,
    seen: &[(Params, f64)],
    good: &[&Params],
    cand: &Params,
) -> f64 {
    let mut score = 0.0;
    for (j, dim) in space.continuous().iter().enumerate() {
        let n = good.len() as f64;
        let mean = good.iter().map(|g| g.continuous[j]).sum::<f64>() / n;
        let var = good
            .iter()
            .map(|g| {
                let d = g.continuous[j] - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        let std = var.sqrt().max(0.05 * dim.width()).max(1e-12);
        let z = (cand.continuous[j] - mean) / std;
        score += -0.5 * z * z + dim.width().ln();
    }
    for (j, dim) in space.discrete().iter().enumerate() {
        let c = dim.len() as f64;
        let l = (good
            .iter()
            .filter(|g| g.discrete[j] == cand.discrete[j])
            .count() as f64
            + 1.0)
            / (good.len() as f64 + c);
        let g = (seen
            .iter()
            .filter(|(p, _)| p.discrete[j] == cand.discrete[j])
            .count() as f64
            + 1.0)
            / (seen.len() as f64 + c);
        score += (l / g).ln();
    }
    score
}

/// Simplex seed: clipped start plus one +10 %-of-range (≥1e-3) step per dim.
fn initial_simplex(space: &SearchSpace, start: &[f64]) -> Vec<Vec<f64>> {
    let mut v0 = start.to_vec();
    for (v, dim) in v0.iter_mut().zip(space.continuous().iter()) {
        *v = dim.clamp(*v);
    }
    let mut verts = vec![v0.clone()];
    verts.extend(space.continuous().iter().enumerate().map(|(j, dim)| {
        let mut v = v0.clone();
        let step = (0.1 * dim.width()).max(1e-3);
        v[j] = dim.clamp(v[j] + step);
        v
    }));
    verts
}

/// Sort vertices and values together, ascending by value.
fn sort_simplex(verts: &mut Vec<Vec<f64>>, vals: &mut Vec<f64>) {
    let mut order: Vec<usize> = (0..verts.len()).collect();
    order.sort_by(|&a, &b| vals[a].partial_cmp(&vals[b]).unwrap_or(Ordering::Equal));
    let mut sorted_v = Vec::with_capacity(verts.len());
    let mut sorted_f = Vec::with_capacity(vals.len());
    for &i in &order {
        sorted_v.push(verts[i].clone());
        sorted_f.push(vals[i]);
    }
    *verts = sorted_v;
    *vals = sorted_f;
}

/// Mean of `points` (the best-`n` centroid input).
fn centroid(points: &[Vec<f64>]) -> Vec<f64> {
    let n = points.len() as f64;
    let mut cent = vec![0.0; points[0].len()];
    for p in points {
        for (c, &x) in cent.iter_mut().zip(p.iter()) {
            *c += x;
        }
    }
    for c in cent.iter_mut() {
        *c /= n;
    }
    cent
}

/// `cent + t * (cent - worst)`: `t = 1.0` reflects, `2.0` expands, `±0.5`
/// contracts.
fn comb(cent: &[f64], worst: &[f64], t: f64) -> Vec<f64> {
    cent.iter()
        .zip(worst.iter())
        .map(|(&c, &w)| c + t * (c - w))
        .collect()
}
