//! Candidate vector surface (TSK-801, wires 3 and 4).
//!
//! Two capabilities that existed inside their own crates but were unreachable
//! from any product binary are wired here:
//!
//! - **retrieval** — candidate de-duplication by the same cosine primitive the
//!   RRF fusion path uses ([`synthlm_retrieval::cosine_distance`],
//!   default radius [`synthlm_retrieval::DEFAULT_FUSED_DEDUP_DISTANCE`]);
//! - **eval** — the CLAP-family audio distance (`clap_cos`), which the
//!   dispatcher used to hard-code to `None`.
//!
//! The default embedder is [`synthlm_eval::clap::SpectralClapEmbedder`] (the
//! always-compiled spectral fingerprint). The ONNX embedder stays behind the
//! non-default `onnx` feature and still reports BLOCKED until the mel
//! calibration spike lands — this module never fabricates a number.

use synthlm_eval::EvalError;
use synthlm_eval::MirParams;
use synthlm_eval::clap::SpectralClapEmbedder;
use synthlm_eval::score::clap_cosine;
use synthlm_retrieval::IndexError;
use synthlm_retrieval::{DEFAULT_FUSED_DEDUP_DISTANCE, cosine_distance};

/// Radius used when the caller does not override it (same value DEC-014/TSK-606
/// pinned for fused de-duplication).
pub const DEFAULT_DEDUP_RADIUS: f64 = DEFAULT_FUSED_DEDUP_DISTANCE;

/// Result of a de-duplication pass over candidate vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupOutcome {
    /// Indices of the candidates that survive, in input order.
    pub kept: Vec<usize>,
    /// Indices dropped as near-duplicates of an earlier survivor.
    pub collapsed: Vec<usize>,
}

impl DedupOutcome {
    /// How many candidates were collapsed into an earlier one.
    pub fn collapsed_count(&self) -> usize {
        self.collapsed.len()
    }
}

/// Greedy cosine de-duplication: a candidate is collapsed when its distance to
/// any earlier survivor is `<= radius`.
///
/// # Errors
///
/// [`synthlm_retrieval::IndexError`] from the shared primitive (dimension mismatch, non-finite
/// components, empty vector).
pub fn dedup_candidates(vectors: &[Vec<f32>], radius: f64) -> Result<DedupOutcome, IndexError> {
    let mut kept: Vec<usize> = Vec::new();
    let mut collapsed: Vec<usize> = Vec::new();
    for (index, vector) in vectors.iter().enumerate() {
        let mut duplicate = false;
        for &survivor in &kept {
            if cosine_distance(&vectors[survivor], vector)? <= radius {
                duplicate = true;
                break;
            }
        }
        if duplicate {
            collapsed.push(index);
        } else {
            kept.push(index);
        }
    }
    Ok(DedupOutcome { kept, collapsed })
}

/// Audio distance between a reference and a candidate render, in `-1.0..=1.0`.
///
/// # Errors
///
/// [`synthlm_eval::EvalError`] for empty/non-finite/silent input or a dimension mismatch
/// (propagated from the embedder; no silent fallback to a default value).
pub fn clap_cos(reference: &[f32], candidate: &[f32]) -> Result<f32, EvalError> {
    clap_cosine(
        &SpectralClapEmbedder,
        reference,
        candidate,
        &MirParams::v1(),
    )
}

/// Accepts a caller-supplied `clap_cos` only when it is finite and inside
/// `-1.0..=1.0`; anything else is rejected instead of being clamped, so a bad
/// producer cannot smuggle a fabricated score into the journal.
///
/// # Errors
///
/// Returns `Err(())`-style `None` on invalid input: callers map it to their own
/// error taxonomy. Kept as a pure predicate so both the dispatcher and tests
/// can reuse it.
pub fn accept_clap_cos(raw: f64) -> Option<f32> {
    if raw.is_finite() && (-1.0..=1.0).contains(&raw) {
        Some(raw as f32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    #[test]
    fn identical_candidates_collapse_and_distinct_ones_survive() {
        let a = sine(220.0, 4_800);
        let b = sine(3_000.0, 4_800);
        let vectors = vec![a.clone(), a, b];
        let outcome = dedup_candidates(&vectors, DEFAULT_DEDUP_RADIUS).expect("dedup");
        assert_eq!(outcome.kept, vec![0, 2]);
        assert_eq!(outcome.collapsed, vec![1]);
        assert_eq!(outcome.collapsed_count(), 1);
    }

    #[test]
    fn empty_input_yields_an_empty_outcome() {
        let outcome = dedup_candidates(&[], DEFAULT_DEDUP_RADIUS).expect("dedup");
        assert!(outcome.kept.is_empty());
        assert!(outcome.collapsed.is_empty());
    }

    #[test]
    fn non_finite_vectors_are_rejected_not_silently_kept() {
        let vectors = vec![vec![1.0_f32, 0.0], vec![f32::NAN, 0.0]];
        assert!(dedup_candidates(&vectors, DEFAULT_DEDUP_RADIUS).is_err());
    }

    #[test]
    fn clap_cos_is_one_for_identical_audio_and_rejects_silence() {
        let a = sine(440.0, 24_000);
        let identical = clap_cos(&a, &a).expect("identical rendering");
        assert!((identical - 1.0).abs() < 1e-5, "cos={identical}");
        assert!(clap_cos(&[], &a).is_err(), "empty reference is an error");
        assert!(
            clap_cos(&vec![0.0_f32; 1_024], &a).is_err(),
            "digital silence must not score"
        );
    }

    #[test]
    fn caller_supplied_cosines_are_range_checked() {
        assert_eq!(accept_clap_cos(0.75), Some(0.75));
        assert_eq!(accept_clap_cos(-1.0), Some(-1.0));
        assert_eq!(accept_clap_cos(1.0), Some(1.0));
        assert_eq!(accept_clap_cos(1.5), None);
        assert_eq!(accept_clap_cos(f64::NAN), None);
        assert_eq!(accept_clap_cos(f64::INFINITY), None);
    }
}
