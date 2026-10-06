//! Loudness-first multi-objective comparison (DEC-016, ARCHITECTURE §6).
//!
//! [`compare`] takes two [`MirFeatures`] snapshots
//! and returns a [`Score`]. Both snapshots must share the same
//! [`MirParams`]; mismatched params are rejected
//! with [`EvalError::ParamMismatch`](crate::EvalError) because their
//! numbers would be incomparable (DEC-012, both STFT tiers included).
//!
//! Distance convention: `spec_l1` / `mel_l1` / `mel_low|mid|high` are
//! distances (lower is more similar), `transient_f1` / `clap_cos` are
//! similarities (higher is more similar). `mel_l1` stays the unweighted
//! mean for calibration reference; `mel_weighted` (default
//! [`BandWeights`]) is the ranking signal. Tuning the weights against
//! listening tests belongs to TSK-402, not here.

use crate::EvalError;
use crate::mir::{MirFeatures, MirParams, ONSET_TOLERANCE_FRAMES, mel_band_centers, onset_f1};

/// Mel-band centre (Hz) below which a band counts as LOW (TSK-204).
///
/// ~400 Hz sits above vowel-fundamental territory while keeping the
/// first-formant region in MID; a qualitative split point, calibration in
/// TSK-402.
pub const BAND_LOW_MAX_HZ: f32 = 400.0;

/// Mel-band centre (Hz) at/above which a band counts as HIGH (TSK-204).
///
/// ~4 kHz marks the presence-to-brilliance transition (sibilance, air);
/// qualitative, calibration in TSK-402.
pub const BAND_HIGH_MIN_HZ: f32 = 4000.0;

/// Initial LOW-band weight (TSK-204).
///
/// Body/fundamentals matter, but small low-end shifts are common across
/// renders, so LOW starts below MID. Initial value — TSK-402 listening
/// calibration may move it; do not tune by gut feel here.
pub const MEL_W_LOW: f32 = 0.30;

/// Initial MID-band weight (TSK-204).
///
/// Highest of the three as a starting point: formants, presence and the
/// ear's most sensitive octaves live here, so timbre cheats (e.g. a lone
/// bass boost, normalised away elsewhere) stay visible. TSK-402 calibrates.
pub const MEL_W_MID: f32 = 0.45;

/// Initial HIGH-band weight (TSK-204).
///
/// Air/sibilance and clipping hash live here; audible but usually less
/// decisive than MID for "same mix?" judgements. Initial value, TSK-402
/// calibrates.
pub const MEL_W_HIGH: f32 = 0.25;

/// Downstream-limiting alarm threshold in dBTP (DEC-016, TSK-208).
///
/// A candidate whose post-normalisation true peak exceeds −1 dBTP needs a
/// limiter downstream; [`Score::true_peak_alarm`] reports exactly the
/// `true_peak > −1` condition. The boundary itself (−1.0) does not alarm —
/// only a strict overshoot does.
pub const TRUE_PEAK_ALARM_DBTP: f64 = -1.0;

/// Low/mid/high mel-band weights with a tunable interface (TSK-204).
///
/// [`BandWeights::defaults`] returns the [`MEL_W_LOW`]/[`MEL_W_MID`]/
/// [`MEL_W_HIGH`] starting point used for [`Score::mel_weighted`];
/// [`BandWeights::uniform`] is the calibration baseline (all bands equal).
/// TSK-402 listening tests own the final numbers — this struct is the seam
/// they tune through, via [`Score::mel_weighted_with`], without changing
/// the stored per-band components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandWeights {
    /// Weight applied to [`Score::mel_low`].
    pub low: f32,
    /// Weight applied to [`Score::mel_mid`].
    pub mid: f32,
    /// Weight applied to [`Score::mel_high`].
    pub high: f32,
}

impl BandWeights {
    /// The TSK-204 starting weights (sum to 1.0).
    pub fn defaults() -> Self {
        Self {
            low: MEL_W_LOW,
            mid: MEL_W_MID,
            high: MEL_W_HIGH,
        }
    }

    /// Equal weights (calibration baseline: recovers the flat mean up to
    /// per-region cell-count rounding).
    pub fn uniform() -> Self {
        Self {
            low: 1.0 / 3.0,
            mid: 1.0 / 3.0,
            high: 1.0 / 3.0,
        }
    }

    /// Weighted combination of per-band distances.
    pub fn apply(&self, low: f32, mid: f32, high: f32) -> f32 {
        self.low * low + self.mid * mid + self.high * high
    }
}

impl Default for BandWeights {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Multi-objective score of a candidate against a reference (ARCH §6).
///
/// Field order mirrors ARCHITECTURE §6 verbatim for the first seven
/// fields (`spec_l1, mel_l1, clap_cos, transient_f1, lufs_i, true_peak,
/// delta_lufs`); the trailing `mel_low|mid|high|weighted` fields are the
/// TSK-204 band split of `mel_l1`, kept as components so TSK-402 can
/// re-weight without re-running analysis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// Mean absolute difference of the linear-magnitude spectrograms
    /// (post-normalisation, primary resolution). Distance: lower is similar.
    pub spec_l1: f32,
    /// Mean absolute difference of the two-tier-averaged log-mel
    /// spectrograms (post-normalisation, unweighted mean over all bands).
    /// Distance: lower is more similar. Kept for calibration reference;
    /// see `mel_weighted` for the ranking signal.
    pub mel_l1: f32,
    /// Mean absolute log-mel difference over LOW bands only (centre Hz <
    /// [`BAND_LOW_MAX_HZ`]). Distance.
    pub mel_low: f32,
    /// Mean absolute log-mel difference over MID bands only
    /// ([`BAND_LOW_MAX_HZ`] ≤ centre < [`BAND_HIGH_MIN_HZ`]). Distance.
    pub mel_mid: f32,
    /// Mean absolute log-mel difference over HIGH bands only (centre Hz ≥
    /// [`BAND_HIGH_MIN_HZ`]). Distance.
    pub mel_high: f32,
    /// Default-weighted band combination
    /// (`defaults().apply(mel_low, mel_mid, mel_high)`). Distance: the
    /// ranking signal for "same mix?" judgements until TSK-402 recalibrates
    /// the weights (then use [`Score::mel_weighted_with`]).
    pub mel_weighted: f32,
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

impl Score {
    /// Re-weight the stored band components with custom weights (TSK-402
    /// calibration seam). [`Score::mel_weighted`] is this with
    /// [`BandWeights::defaults`].
    pub fn mel_weighted_with(&self, weights: &BandWeights) -> f32 {
        weights.apply(self.mel_low, self.mel_mid, self.mel_high)
    }

    /// Downstream-limiting alarm (DEC-016, TSK-208).
    ///
    /// True when the candidate's post-normalisation true peak strictly
    /// exceeds [`TRUE_PEAK_ALARM_DBTP`] (−1 dBTP). Exactly −1.0 does not
    /// alarm; anything above does, including the +6 dBTP-class peaks of
    /// hard-clipped material gained back up by normalisation.
    pub fn true_peak_alarm(&self) -> bool {
        self.true_peak > TRUE_PEAK_ALARM_DBTP
    }
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
/// post-normalisation spectrograms (`mel_l1` on the two-tier-averaged
/// log-mel); `mel_low|mid|high` split `mel_l1` by band region and
/// `mel_weighted` combines them with [`BandWeights::defaults`];
/// `transient_f1` compares onset lists at
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
    let centers = mel_band_centers(&reference.params);
    let band = band_mel_l1(&reference.log_mel, &candidate.log_mel, &centers);
    Ok(Score {
        spec_l1: mean_abs_diff(&reference.magnitude, &candidate.magnitude),
        mel_l1: mean_abs_diff(&reference.log_mel, &candidate.log_mel),
        mel_low: band.0,
        mel_mid: band.1,
        mel_high: band.2,
        mel_weighted: BandWeights::defaults().apply(band.0, band.1, band.2),
        clap_cos,
        transient_f1: onset_f1(&reference.onsets, &candidate.onsets, ONSET_TOLERANCE_FRAMES),
        lufs_i: candidate.normalized_lufs,
        true_peak: candidate.normalized_dbtp,
        delta_lufs: candidate.gain_db,
    })
}

/// Per-band log-mel distances `(low, mid, high)` (TSK-204).
///
/// Bands are assigned by centre frequency from [`mel_band_centers`]:
/// centre < [`BAND_LOW_MAX_HZ`] is LOW, centre < [`BAND_HIGH_MIN_HZ`] is
/// MID, otherwise HIGH. Each region mean is over its own cells (same
/// convention as [`mean_abs_diff`]); an empty region scores 0.0.
fn band_mel_l1(a: &[Vec<f32>], b: &[Vec<f32>], centers: &[f32]) -> (f32, f32, f32) {
    let mut acc = [0.0_f64; 3];
    let mut count = [0_usize; 3];
    for (ra, rb) in a.iter().zip(b.iter()) {
        for ((&x, &y), &c) in ra.iter().zip(rb.iter()).zip(centers.iter()) {
            let region = if c < BAND_LOW_MAX_HZ {
                0
            } else if c < BAND_HIGH_MIN_HZ {
                1
            } else {
                2
            };
            acc[region] += f64::from((x - y).abs());
            count[region] += 1;
        }
    }
    let mean = |i: usize| {
        if count[i] == 0 {
            0.0
        } else {
            (acc[i] / count[i] as f64) as f32
        }
    };
    (mean(0), mean(1), mean(2))
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

    #[test]
    fn band_weights_default_to_tsk204_split() {
        let w = BandWeights::defaults();
        assert_eq!(w, BandWeights::default());
        assert!((w.low + w.mid + w.high - 1.0).abs() < 1e-6);
        assert_eq!(
            w,
            BandWeights {
                low: MEL_W_LOW,
                mid: MEL_W_MID,
                high: MEL_W_HIGH,
            }
        );
        assert!((w.apply(1.0, 2.0, 3.0) - (0.30 + 0.90 + 0.75)).abs() < 1e-5);
        let u = BandWeights::uniform();
        assert!((u.apply(3.0, 3.0, 3.0) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn identical_input_has_zero_bands_and_consistent_weight() -> Result<(), EvalError> {
        let params = MirParams::v1();
        let x = sine(48000, 440.0, 0.4);
        let a = analyze(&x, &params)?;
        let score = compare(&a, &a)?;
        assert_eq!(score.mel_low, 0.0);
        assert_eq!(score.mel_mid, 0.0);
        assert_eq!(score.mel_high, 0.0);
        assert_eq!(score.mel_weighted, 0.0);
        assert_eq!(score.mel_weighted_with(&BandWeights::uniform()), 0.0);
        Ok(())
    }

    #[test]
    fn timbre_change_spreads_across_bands() -> Result<(), EvalError> {
        // 440 Hz vs 880 Hz sines: different bands must light up, and the
        // stored weighted value must equal defaults applied to components.
        let params = MirParams::v1();
        let a = analyze(&sine(48000, 440.0, 0.4), &params)?;
        let b = analyze(&sine(48000, 880.0, 0.4), &params)?;
        let score = compare(&a, &b)?;
        assert!(score.mel_low > 0.0);
        assert!(score.mel_mid > 0.0);
        assert_eq!(
            score.mel_weighted,
            score.mel_weighted_with(&BandWeights::defaults())
        );
        Ok(())
    }

    fn score_with_peak(peak_dbtp: f64) -> Score {
        Score {
            spec_l1: 0.0,
            mel_l1: 0.0,
            mel_low: 0.0,
            mel_mid: 0.0,
            mel_high: 0.0,
            mel_weighted: 0.0,
            clap_cos: None,
            transient_f1: 1.0,
            lufs_i: -14.0,
            true_peak: peak_dbtp,
            delta_lufs: 0.0,
        }
    }

    #[test]
    fn true_peak_alarm_boundary() {
        assert_eq!(TRUE_PEAK_ALARM_DBTP, -1.0);
        // Exactly −1.0 does not alarm; only a strict overshoot does.
        assert!(!score_with_peak(-1.0).true_peak_alarm());
        assert!(!score_with_peak(-1.001).true_peak_alarm());
        assert!(!score_with_peak(-8.819).true_peak_alarm());
        // Slightly over, full scale, and clipped-hot peaks alarm.
        assert!(score_with_peak(-0.999).true_peak_alarm());
        assert!(score_with_peak(0.0).true_peak_alarm());
        assert!(score_with_peak(6.355).true_peak_alarm());
    }

    #[test]
    fn true_peak_alarm_fires_on_clipped_signal() -> Result<(), EvalError> {
        // End-to-end: hard-clipped material gained back up by loudness
        // normalisation must trip the alarm (mirrors the clipped golden
        // pin in `tests/golden_mir.rs`), while the honest bed stays quiet.
        let params = MirParams::v1();
        let bed = sine(48_000, 440.0, 0.1);
        let reference = analyze(&bed, &params)?;
        let mut hot = bed;
        for &pos in &[12_000, 24_000, 36_000] {
            hot[pos] += 1.2;
        }
        let clipped: Vec<f32> = hot.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
        let candidate = analyze(&clipped, &params)?;
        let score = compare(&reference, &candidate)?;
        assert!(score.true_peak > -1.0, "clipped dbtp={}", score.true_peak);
        assert!(score.true_peak_alarm());
        let calm = compare(&reference, &reference.clone())?;
        assert!(
            !calm.true_peak_alarm(),
            "honest bed must not alarm: dbtp={}",
            calm.true_peak
        );
        Ok(())
    }
}
