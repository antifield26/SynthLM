//! Loudness-first multi-objective comparison (DEC-016, ARCHITECTURE §6).
//!
//! [`compare`] takes two [`MirFeatures`] snapshots
//! and returns a [`Score`]. Both snapshots must share the same
//! [`MirParams`]; mismatched params are rejected
//! with [`EvalError::ParamMismatch`](crate::EvalError) because their
//! numbers would be incomparable (DEC-012).
//!
//! Distance convention: `spec_l1` / `mel_l1` are distances (lower is more
//! similar), `transient_f1` / `clap_cos` are similarities (higher is more
//! similar). Weighting the objectives into one number is calibration work
//! and belongs to TSK-208, not here.

use crate::EvalError;
use crate::mir::{MirFeatures, MirParams, ONSET_TOLERANCE_FRAMES, onset_f1};

/// Multi-objective score of a candidate against a reference (ARCH §6).
///
/// Field order mirrors ARCHITECTURE §6 verbatim:
/// `spec_l1, mel_l1, clap_cos, transient_f1, lufs_i, true_peak, delta_lufs`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// Mean absolute difference of the linear-magnitude spectrograms
    /// (post-normalisation). Distance: lower is more similar.
    pub spec_l1: f32,
    /// Mean absolute difference of the log-mel spectrograms
    /// (post-normalisation). Distance: lower is more similar.
    pub mel_l1: f32,
    /// CLAP cosine similarity, when a CLAP backend is wired. `None` until
    /// then (see `TODO(TSK-202)` on [`ClapEmbedder`]).
    pub clap_cos: Option<f32>,
    /// Onset F1 between reference and candidate transient envelopes
    /// (tolerance ±[`ONSET_TOLERANCE_FRAMES`] frames). Similarity: higher
    /// is more similar; 1.0 is identical.
    pub transient_f1: f32,
    /// Candidate integrated LUFS re-measured after normalisation
    /// (�?�?4; residual shows the norm loop landed on target).
    pub lufs_i: f64,
    /// Candidate true peak in dBTP after normalisation. Values above �?
    /// flag a downstream limiting need; applying that penalty is TSK-208
    /// calibration (DEC-016).
    pub true_peak: f64,
    /// Applied gain in dB (ΔLUFS = target minus raw candidate integrated).
    pub delta_lufs: f64,
}

/// Future CLAP audio-text embedding backend (trait seam, no model wired).
///
/// Research (C §1.1) selects CLAP-512 (`projection_dim = 512`, ONNX via
/// `ort`, int8 CPU) as the only product-safe music-text embedding route.
/// Wiring it means implementing this trait against an ONNX session and
/// threading the cosine through [`compare_with_clap`].
///
/// TODO(TSK-202): wire ort/CLAP ONNX (ort int8 CPU session + projection_dim
/// 512 check + export-consistency measurement); perceptual weight calibration
/// belongs to TSK-208.
pub trait ClapEmbedder {
    /// Embed mono samples at the params rate into one embedding vector.
    fn embed(&self, samples: &[f32], params: &MirParams) -> Result<Vec<f32>, EvalError>;

    /// Expected embedding dimension (CLAP default 512).
    fn embed_dim(&self) -> usize {
        512
    }
}

/// Cosine similarity of two embedding vectors, clamped to [-1, 1].
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Result<f32, EvalError> {
    if a.len() != b.len() {
        return Err(EvalError::DimMismatch {
            a: a.len(),
            b: b.len(),
        });
    }
    if a.is_empty() {
        return Err(EvalError::InvalidSamples(
            "cannot take cosine of empty embeddings".to_string(),
        ));
    }
    let (mut dot, mut na, mut nb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += f64::from(x) * f64::from(y);
        na += f64::from(x) * f64::from(x);
        nb += f64::from(y) * f64::from(y);
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom.is_nan() || denom <= 0.0 {
        return Err(EvalError::InvalidSamples(
            "zero-norm embedding has no direction".to_string(),
        ));
    }
    Ok((dot / denom) as f32).map(|v| v.clamp(-1.0, 1.0))
}

/// CLAP similarity through an embedder backend (both sides embedded).
///
/// Both embeddings must additionally match the backend's declared
/// [`ClapEmbedder::embed_dim`]; a mismatch indicates a swapped or corrupt
/// model and is rejected rather than scored.
pub fn clap_cosine<E: ClapEmbedder>(
    embedder: &E,
    reference: &[f32],
    candidate: &[f32],
    params: &MirParams,
) -> Result<f32, EvalError> {
    let ea = embedder.embed(reference, params)?;
    let eb = embedder.embed(candidate, params)?;
    for e in [&ea, &eb] {
        if e.len() != embedder.embed_dim() {
            return Err(EvalError::DimMismatch {
                a: e.len(),
                b: embedder.embed_dim(),
            });
        }
    }
    cosine_similarity(&ea, &eb)
}

/// Compare a candidate against a reference and score it.
///
/// `spec_l1` / `mel_l1` are mean absolute differences over the
/// post-normalisation spectrograms; `transient_f1` compares onset lists at
/// ±[`ONSET_TOLERANCE_FRAMES`] frames; loudness fields report the
/// candidate's closed-loop normalisation audit. `clap_cos` is `None`
/// (model not wired; use [`compare_with_clap`] once it is).
pub fn compare(reference: &MirFeatures, candidate: &MirFeatures) -> Result<Score, EvalError> {
    compare_inner(reference, candidate, None)
}

/// Compare with a precomputed CLAP cosine (validates finiteness only).
///
/// The cosine itself must come from [`clap_cosine`] or an equivalent
/// backend check; NaN/infinite values are rejected.
pub fn compare_with_clap(
    reference: &MirFeatures,
    candidate: &MirFeatures,
    clap_cos: f32,
) -> Result<Score, EvalError> {
    if !clap_cos.is_finite() {
        return Err(EvalError::InvalidSamples(
            "CLAP cosine must be finite".to_string(),
        ));
    }
    compare_inner(reference, candidate, Some(clap_cos.clamp(-1.0, 1.0)))
}

fn compare_inner(
    reference: &MirFeatures,
    candidate: &MirFeatures,
    clap_cos: Option<f32>,
) -> Result<Score, EvalError> {
    if reference.params != candidate.params {
        return Err(EvalError::ParamMismatch);
    }
    if reference.n_frames != candidate.n_frames {
        return Err(EvalError::LengthMismatch {
            a: reference.n_frames,
            b: candidate.n_frames,
        });
    }
    Ok(Score {
        spec_l1: mean_abs_diff(&reference.magnitude, &candidate.magnitude),
        mel_l1: mean_abs_diff(&reference.log_mel, &candidate.log_mel),
        clap_cos,
        transient_f1: onset_f1(&reference.onsets, &candidate.onsets, ONSET_TOLERANCE_FRAMES),
        lufs_i: candidate.normalized_lufs,
        true_peak: candidate.normalized_dbtp,
        delta_lufs: candidate.gain_db,
    })
}

/// Mean absolute element-wise difference of two `[frame][bin]` grids.
///
/// Empty grids (no comparable cells) define a distance of 0.0. Accumulates
/// in f64 so long utterances do not lose precision to summation order.
fn mean_abs_diff(a: &[Vec<f32>], b: &[Vec<f32>]) -> f32 {
    let mut acc = 0.0_f64;
    let mut count = 0_usize;
    for (ra, rb) in a.iter().zip(b.iter()) {
        for (&x, &y) in ra.iter().zip(rb.iter()) {
            acc += f64::from((x - y).abs());
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        (acc / count as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{MirParams, analyze};

    fn sine(len: usize, freq: f32, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn cosine_basics() {
        let a = [1.0_f32, 0.0, 0.0];
        let b = [0.0_f32, 1.0, 0.0];
        let cos = cosine_similarity(&a, &a).unwrap_or(-2.0);
        assert_eq!(cos, 1.0);
        assert_eq!(cosine_similarity(&a, &b).unwrap_or(-2.0), 0.0);
        assert!(matches!(
            cosine_similarity(&a, &[1.0, 2.0]),
            Err(EvalError::DimMismatch { .. })
        ));
        assert!(matches!(
            cosine_similarity(&[], &[]),
            Err(EvalError::InvalidSamples(_))
        ));
        assert!(matches!(
            cosine_similarity(&[0.0, 0.0], &[0.0, 0.0]),
            Err(EvalError::InvalidSamples(_))
        ));
    }

    #[test]
    fn clap_cos_defaults_to_none() -> Result<(), EvalError> {
        let params = MirParams::v1();
        let x = sine(48000, 440.0, 0.4);
        let a = analyze(&x, &params)?;
        let score = compare(&a, &a)?;
        assert_eq!(score.clap_cos, None);
        let scored = compare_with_clap(&a, &a, 0.9)?;
        assert_eq!(scored.clap_cos, Some(0.9));
        assert!(compare_with_clap(&a, &a, f32::NAN).is_err());
        Ok(())
    }

    #[test]
    fn mismatched_params_are_rejected() -> Result<(), EvalError> {
        let params = MirParams::v1();
        let other = MirParams {
            window: 1024,
            ..MirParams::v1()
        };
        let x = sine(48000, 440.0, 0.4);
        let a = analyze(&x, &params)?;
        let b = analyze(&x, &other)?;
        assert!(matches!(compare(&a, &b), Err(EvalError::ParamMismatch)));
        Ok(())
    }

    #[test]
    fn mismatched_lengths_are_rejected() -> Result<(), EvalError> {
        let params = MirParams::v1();
        let short = analyze(&sine(48_000, 440.0, 0.4), &params)?;
        let long = analyze(&sine(96_000, 440.0, 0.4), &params)?;
        assert!(matches!(
            compare(&short, &long),
            Err(EvalError::LengthMismatch { .. })
        ));
        Ok(())
    }
}
