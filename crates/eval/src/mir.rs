//! MIR v1 feature pipeline: fixed-params analysis feeding loudness-first scoring.
//!
//! Parameter freeze (DEC-012): 48 kHz sample rate, Hann window 2048,
//! hop 512, 80-band log-mel. Ordering (DEC-016): integrated LUFS is
//! measured first, the signal is gained to [`MirParams::target_lufs`]
//! (−14 LUFS), and only then are spectral features computed. Every
//! [`MirFeatures`] snapshot carries the [`MirParams`] it was produced
//! with so [`crate::score::compare`] can refuse cross-params comparisons.
//!
//! Selection rationale (docs/research/C-models-retrieval-eval.md §4,
//! verified 2026-10-06 against the local cargo registry manifests):
//! STFT via `realfft` (MIT, real-to-complex wrapper over `rustfft`,
//! MIT OR Apache-2.0) and loudness via `ebur128` (MIT, pure-Rust EBU R128
//! implementation covering TECH 3341/3342 including true peak). The mel
//! filterbank and the linear-energy spectral-flux transient envelope are
//! hand-written (no `ruststft` dependency) so the DEC-012 constants stay
//! literal in this file.

use crate::EvalError;
use ebur128::{EbuR128, Mode};
use realfft::RealFftPlanner;
use rustfft::num_complex::Complex;

/// Absolute gate (LUFS) below which a signal counts as unmeasurable silence.
///
/// Mirrors the −70 LUFS absolute gate of BS.1770 gating: anything quieter
/// cannot yield a finite normalisation gain and is rejected with
/// [`EvalError::SilenceOrTooQuiet`].
const ABS_GATE_LUFS: f64 = -70.0;

/// Floor applied before the log in log-mel, keeping silence at a finite
/// value instead of −infinity.
///
/// 1e-5 in (magnitude²) units sits ~180 dB below a full-scale bin-centred
/// tone yet above the f32 FFT rounding residue (~1e-6 energy for the
/// signals probed in `tests/golden_mir.rs`), so empty bands clamp to a
/// constant instead of amplifying fp noise through the logarithm.
const LOG_MEL_FLOOR: f32 = 1e-5;

/// Minimum gap between reported onset peaks, in STFT frames.
///
/// 3 frames ≈ 32 ms at hop 512 / 48 kHz, inside the ±20–50 ms alignment
/// tolerance window (research C §4.2).
pub const ONSET_MIN_GAP_FRAMES: usize = 3;

/// Matching tolerance for onset comparison, in STFT frames (≈ ±32 ms).
pub const ONSET_TOLERANCE_FRAMES: usize = 3;

/// Relative peak-picking threshold: a flux peak must exceed this fraction
/// of the utterance-global flux maximum to count as an onset.
pub const ONSET_REL_THRESHOLD: f32 = 0.25;

/// Floor for onset picking, relative to the utterance-peak mel-band energy.
///
/// Only peaks above −120 dB of the peak band energy count as onsets; below
/// is FFT rounding residue or inter-partial skirt beating (genuine
/// interference, but orders of magnitude below any real envelope event —
/// see the probe notes in `tests/golden_mir.rs`). An absolute floor cannot
/// work here because flux lives in signal-level energy units.
const ONSET_FLOOR_RATIO: f32 = 1e-6;

/// Frozen MIR v1 analysis parameters (DEC-012).
///
/// All fields participate in [`PartialEq`]: [`crate::score::compare`]
/// refuses to score feature pairs produced with different params, so any
/// change here (window, hop, mel bands, rate, frequency range, target)
/// automatically invalidates cross-version comparisons instead of silently
/// producing incomparable numbers. Schema changes must go through a
/// version bump (`v1` → `v2`), never an in-place edit of [`MirParams::v1`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MirParams {
    /// Sample rate in Hz. DEC-012 frozen value: 48000.
    pub sample_rate: u32,
    /// STFT window length in samples (Hann). DEC-012 frozen value: 2048.
    pub window: usize,
    /// STFT hop in samples. DEC-012 frozen value: 512.
    pub hop: usize,
    /// Mel filterbank band count. DEC-012 frozen value: 80.
    pub n_mel: usize,
    /// Filterbank low edge in Hz. DEC-012 implies full band: 0.0.
    pub f_min_hz: f32,
    /// Filterbank high edge in Hz (Nyquist at 48 kHz).
    pub f_max_hz: f32,
    /// Loudness normalisation target in LUFS. DEC-016: −14.
    pub target_lufs: f64,
}

impl MirParams {
    /// The frozen MIR v1 parameter set (DEC-012 + DEC-016).
    ///
    /// Do not alter the returned values; introduce a new constructor for a
    /// new schema version instead.
    pub const fn v1() -> Self {
        Self {
            sample_rate: 48_000,
            window: 2048,
            hop: 512,
            n_mel: 80,
            f_min_hz: 0.0,
            f_max_hz: 24_000.0,
            target_lufs: -14.0,
        }
    }

    /// Number of magnitude bins per STFT frame (`window / 2 + 1`).
    pub const fn n_bins(&self) -> usize {
        self.window / 2 + 1
    }
}

/// One analysed utterance: normalised-domain features plus loudness audit.
///
/// `integrated_lufs` is the pre-normalisation reading (diagnostic),
/// `gain_db` the applied ΔLUFS, and `normalized_lufs` / `normalized_dbtp`
/// are re-measured on the gained signal to close the loop (residual shows
/// the normalisation actually landed on target).
#[derive(Debug, Clone)]
pub struct MirFeatures {
    /// Parameters this snapshot was produced with (comparability key).
    pub params: MirParams,
    /// STFT frame count of the spectrograms below.
    pub n_frames: usize,
    /// Linear-magnitude spectrogram `[frame][bin]` of the normalised signal.
    pub magnitude: Vec<Vec<f32>>,
    /// Log-mel spectrogram `[frame][band]` of the normalised signal.
    pub log_mel: Vec<Vec<f32>>,
    /// Half-wave-rectified spectral flux on log-mel, per frame.
    pub flux: Vec<f32>,
    /// Onset frame indices picked from [`MirFeatures::flux`].
    pub onsets: Vec<usize>,
    /// Integrated LUFS of the raw input (pre-normalisation).
    pub integrated_lufs: f64,
    /// Applied gain in dB (ΔLUFS = target − raw integrated).
    pub gain_db: f64,
    /// Integrated LUFS re-measured after normalisation (≈ target).
    pub normalized_lufs: f64,
    /// True peak in dBTP re-measured after normalisation.
    pub normalized_dbtp: f64,
}

/// Full MIR v1 analysis: loudness-first, then spectral features.
///
/// 1. Validate samples (non-empty, at least one window, all finite).
/// 2. Measure integrated LUFS + true peak via `ebur128` (EBU R128).
/// 3. Gain to [`MirParams::target_lufs`] (DEC-016) and re-measure to close
///    the loop.
/// 4. STFT magnitude → 80-band log-mel → spectral flux → onset peaks, all
///    on the normalised signal.
///
/// Input is mono f32 at [`MirParams::sample_rate`]; multi-channel handling
/// (mix-down policy) is out of scope for v1 and must be decided by the
/// caller before calling this function.
pub fn analyze(samples: &[f32], params: &MirParams) -> Result<MirFeatures, EvalError> {
    if samples.len() < params.window {
        return Err(EvalError::TooShort {
            need: params.window,
            got: samples.len(),
        });
    }
    for s in samples {
        if !s.is_finite() {
            return Err(EvalError::InvalidSamples(
                "input contains NaN or infinite sample".to_string(),
            ));
        }
    }

    let (integrated_lufs, _) = measure_loudness(samples, params.sample_rate)?;
    let gain_db = params.target_lufs - integrated_lufs;
    let factor = 10.0_f64.powf(gain_db / 20.0);
    if !factor.is_finite() {
        return Err(EvalError::Loudness(format!(
            "non-finite normalisation gain from {integrated_lufs} LUFS"
        )));
    }
    let mut normalized = Vec::with_capacity(samples.len());
    for s in samples {
        let v = (*s as f64) * factor;
        if !v.is_finite() {
            return Err(EvalError::InvalidSamples(
                "normalised sample is not finite (input out of range)".to_string(),
            ));
        }
        normalized.push(v as f32);
    }
    let (normalized_lufs, normalized_dbtp) = measure_loudness(&normalized, params.sample_rate)?;

    let magnitude = stft_magnitude(&normalized, params)?;
    let filterbank = mel_filterbank(params);
    let energies = mel_energy(&magnitude, &filterbank);
    let log_mel = log_energies(&energies);
    let flux = spectral_flux(&energies);
    let peak_energy = energies.iter().flatten().fold(0.0_f32, |m, &v| m.max(v));
    let onsets = pick_onsets(&flux, peak_energy);

    Ok(MirFeatures {
        params: *params,
        n_frames: magnitude.len(),
        magnitude,
        log_mel,
        flux,
        onsets,
        integrated_lufs,
        gain_db,
        normalized_lufs,
        normalized_dbtp,
    })
}

/// Integrated LUFS plus true-peak dBTP of mono f32 samples at `sample_rate`.
///
/// Uses `ebur128` in `I | TRUE_PEAK` mode (EBU R128 / TECH 3341). Signals
/// with non-finite loudness or below the −70 LUFS absolute gate are
/// rejected — no finite normalisation gain exists for them.
fn measure_loudness(samples: &[f32], sample_rate: u32) -> Result<(f64, f64), EvalError> {
    let mut meter = EbuR128::new(1, sample_rate, Mode::I | Mode::TRUE_PEAK)
        .map_err(|e| EvalError::Loudness(e.to_string()))?;
    for chunk in samples.chunks(8192) {
        meter
            .add_frames_f32(chunk)
            .map_err(|e| EvalError::Loudness(e.to_string()))?;
    }
    let integrated = meter
        .loudness_global()
        .map_err(|e| EvalError::Loudness(e.to_string()))?;
    if !integrated.is_finite() {
        return Err(EvalError::SilenceOrTooQuiet {
            detail: "integrated loudness is not finite (digital silence?)".to_string(),
        });
    }
    if integrated < ABS_GATE_LUFS {
        return Err(EvalError::SilenceOrTooQuiet {
            detail: format!("{integrated:.1} LUFS below the -70 LUFS absolute gate"),
        });
    }
    let peak = meter
        .true_peak(0)
        .map_err(|e| EvalError::Loudness(e.to_string()))?;
    if peak.is_nan() || peak <= 0.0 {
        return Err(EvalError::Loudness(
            "non-positive true peak alongside finite loudness".to_string(),
        ));
    }
    Ok((integrated, 20.0 * peak.log10()))
}

/// Hann window of `n` samples: `0.5 * (1 − cos(2πi / (n−1)))`.
fn hann_window(n: usize) -> Vec<f32> {
    let denom = (n - 1) as f32;
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / denom).cos()))
        .collect()
}

/// Linear-magnitude STFT (`realfft` forward transform, Hann window).
///
/// Frames advance by [`MirParams::hop`]; trailing samples shorter than one
/// window are ignored (documented, deterministic). Output is
/// `[frame][bin]` with `window / 2 + 1` bins.
pub fn stft_magnitude(samples: &[f32], params: &MirParams) -> Result<Vec<Vec<f32>>, EvalError> {
    if samples.len() < params.window {
        return Err(EvalError::TooShort {
            need: params.window,
            got: samples.len(),
        });
    }
    let window = hann_window(params.window);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(params.window);
    let n_frames = (samples.len() - params.window) / params.hop + 1;
    let mut time = vec![0.0_f32; params.window];
    let mut spectrum = vec![
        Complex {
            re: 0.0_f32,
            im: 0.0_f32
        };
        params.n_bins()
    ];
    let mut frames = Vec::with_capacity(n_frames);
    for frame in 0..n_frames {
        let start = frame * params.hop;
        for (dst, (&s, &w)) in time.iter_mut().zip(
            samples[start..start + params.window]
                .iter()
                .zip(window.iter()),
        ) {
            *dst = s * w;
        }
        fft.process(&mut time, &mut spectrum)
            .map_err(|e| EvalError::Fft(e.to_string()))?;
        frames.push(spectrum.iter().map(|c| c.re.hypot(c.im)).collect());
    }
    Ok(frames)
}

/// Hertz ↔ mel conversion (HTK formula, `2595 * log10(1 + f/700)`).
fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

/// Inverse of [`hz_to_mel`].
fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0)
}

/// Triangular mel filterbank `[band][bin]` over [`MirParams::n_bins`] bins.
///
/// `n_mel + 2` points are spaced uniformly on the mel scale between
/// `f_min_hz` and `f_max_hz`; each band is a triangle peaking at its centre
/// point. Hand-written (no `ruststft` dependency) so the DEC-012 band count
/// stays a literal parameter of this function.
pub fn mel_filterbank(params: &MirParams) -> Vec<Vec<f32>> {
    let n_fft = params.window as f32;
    let rate = params.sample_rate as f32;
    let mel_low = hz_to_mel(params.f_min_hz);
    let mel_high = hz_to_mel(params.f_max_hz);
    let points: Vec<f32> = (0..params.n_mel + 2)
        .map(|m| mel_to_hz(mel_low + (mel_high - mel_low) * m as f32 / (params.n_mel + 1) as f32))
        .collect();
    let mut bank = Vec::with_capacity(params.n_mel);
    for m in 1..=params.n_mel {
        let (f_lo, f_c, f_hi) = (points[m - 1], points[m], points[m + 1]);
        let mut row = vec![0.0_f32; params.n_bins()];
        for (k, slot) in row.iter_mut().enumerate() {
            let freq = k as f32 * rate / n_fft;
            if freq >= f_lo && freq <= f_c && f_c > f_lo {
                *slot = (freq - f_lo) / (f_c - f_lo);
            } else if freq > f_c && freq <= f_hi && f_hi > f_c {
                *slot = (f_hi - freq) / (f_hi - f_c);
            }
        }
        bank.push(row);
    }
    bank
}

/// Log-mel spectrogram: natural log of each band energy, floored.
///
/// Output is `[frame][band]`. A uniform +x dB gain shifts every value by a
/// constant, so post-normalisation comparisons are gain-invariant by
/// construction (the +6 dB anti-cheat property, DEC-016/L5).
/// See [`mel_energy`] for the linear-domain intermediate.
pub fn apply_log_mel(magnitude: &[Vec<f32>], filterbank: &[Vec<f32>]) -> Vec<Vec<f32>> {
    log_energies(&mel_energy(magnitude, filterbank))
}

/// Natural log (floored) of linear mel-band energies, `[frame][band]`.
fn log_energies(energies: &[Vec<f32>]) -> Vec<Vec<f32>> {
    energies
        .iter()
        .map(|frame| {
            frame
                .iter()
                .map(|&energy| energy.max(LOG_MEL_FLOOR).ln())
                .collect()
        })
        .collect()
}

/// Linear mel-band energies `[frame][band]` (the pre-log intermediate).
///
/// Transient flux ([`spectral_flux`]) is computed on these, not on log-mel:
/// the logarithm amplifies skirt-beating residue in near-silent bands into
/// phantom onsets, while energy-domain flux keeps the residue ~1e-9 of peak
/// and genuine onsets near order unity.
pub fn mel_energy(magnitude: &[Vec<f32>], filterbank: &[Vec<f32>]) -> Vec<Vec<f32>> {
    magnitude
        .iter()
        .map(|frame| {
            filterbank
                .iter()
                .map(|weights| {
                    frame
                        .iter()
                        .zip(weights.iter())
                        .map(|(&m, &w)| m * m * w)
                        .sum()
                })
                .collect()
        })
        .collect()
}

/// Half-wave-rectified spectral flux on linear mel-band energy, per frame.
///
/// `flux[0]` is defined as 0.0; later frames hold the mean positive
/// band-wise energy difference to the previous frame, in (normalised
/// signal) energy units. Rising energy (note onsets, drum hits) yields
/// positive flux; steady or decaying regions yield ~0.
pub fn spectral_flux(mel_energy: &[Vec<f32>]) -> Vec<f32> {
    let mut flux = Vec::with_capacity(mel_energy.len());
    for (t, frame) in mel_energy.iter().enumerate() {
        if t == 0 {
            flux.push(0.0);
            continue;
        }
        let prev = &mel_energy[t - 1];
        let sum: f32 = frame
            .iter()
            .zip(prev.iter())
            .map(|(&cur, &old)| (cur - old).max(0.0))
            .sum();
        flux.push(sum / frame.len().max(1) as f32);
    }
    flux
}

/// Greedy local-maximum onset picking on an energy-flux envelope.
///
/// A frame is an onset when it exceeds both `ONSET_REL_THRESHOLD` times
/// the utterance-global maximum and `ONSET_FLOOR_RATIO` times the
/// utterance-peak band energy, is a local maximum, and keeps
/// `ONSET_MIN_GAP_FRAMES` distance from the previously kept peak. The
/// first and last frames are never reported (they lack two neighbours).
/// Deterministic: identical envelopes give identical onset lists.
///
/// Note: the relative threshold is deliberately global and simple.
/// Real-world recordings with one dominant hit desensitise quieter onsets;
/// adaptive (local-mean) thresholding is deferred to TSK-208 calibration.
pub fn pick_onsets(flux: &[f32], peak_energy: f32) -> Vec<usize> {
    let mut onsets = Vec::new();
    let max = flux.iter().fold(0.0_f32, |m, &v| m.max(v));
    if max.is_nan() || max <= 0.0 {
        return onsets;
    }
    let threshold = (max * ONSET_REL_THRESHOLD).max(peak_energy * ONSET_FLOOR_RATIO);
    let mut last_kept: Option<usize> = None;
    for i in 1..flux.len().saturating_sub(1) {
        if flux[i] <= threshold || flux[i] < flux[i - 1] || flux[i] < flux[i + 1] {
            continue;
        }
        if last_kept.is_some_and(|last| i - last < ONSET_MIN_GAP_FRAMES) {
            continue;
        }
        last_kept = Some(i);
        onsets.push(i);
    }
    onsets
}

/// Onset F1 between a reference and a candidate onset list.
///
/// Peaks match greedily (nearest unused reference within `tolerance`
/// frames). Both empty scores 1.0 (agree on "no onsets"); exactly one
/// empty scores 0.0.
pub fn onset_f1(reference: &[usize], candidate: &[usize], tolerance: usize) -> f32 {
    if reference.is_empty() && candidate.is_empty() {
        return 1.0;
    }
    if reference.is_empty() || candidate.is_empty() {
        return 0.0;
    }
    let mut used = vec![false; reference.len()];
    let mut hits = 0_usize;
    for &c in candidate {
        let mut best: Option<usize> = None;
        for (ri, &r) in reference.iter().enumerate() {
            if used[ri] {
                continue;
            }
            if r.abs_diff(c) > tolerance {
                continue;
            }
            let nearer = match best {
                None => true,
                Some(b) => r.abs_diff(c) < reference[b].abs_diff(c),
            };
            if nearer {
                best = Some(ri);
            }
        }
        if let Some(ri) = best {
            used[ri] = true;
            hits += 1;
        }
    }
    let precision = hits as f32 / candidate.len() as f32;
    let recall = hits as f32 / reference.len() as f32;
    if precision + recall <= 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_freezes_dec012_values() {
        let p = MirParams::v1();
        assert_eq!(p.sample_rate, 48_000);
        assert_eq!(p.window, 2048);
        assert_eq!(p.hop, 512);
        assert_eq!(p.n_mel, 80);
        assert_eq!(p.f_min_hz, 0.0);
        assert_eq!(p.f_max_hz, 24_000.0);
        assert_eq!(p.target_lufs, -14.0);
        assert_eq!(p.n_bins(), 1025);
    }

    #[test]
    fn hann_window_tapers_to_zero() {
        let w = hann_window(2048);
        assert_eq!(w.len(), 2048);
        assert!(w[0].abs() < 1e-6, "w[0]={}", w[0]);
        assert!((w[1023] - 1.0).abs() < 1e-3, "peak={}", w[1023]);
    }

    #[test]
    fn mel_filterbank_dims_and_energy() {
        let p = MirParams::v1();
        let bank = mel_filterbank(&p);
        assert_eq!(bank.len(), 80);
        for row in &bank {
            assert_eq!(row.len(), 1025);
            for v in row {
                assert!(v.is_finite() && *v >= 0.0);
            }
        }
        let total: f32 = bank.iter().flatten().sum();
        assert!(total > 0.0);
    }

    #[test]
    fn flux_of_flat_energies_is_zero() {
        let flat = vec![vec![2.5_f32; 80]; 8];
        let flux = spectral_flux(&flat);
        assert_eq!(flux.len(), 8);
        assert_eq!(flux[0], 0.0);
        for v in flux.iter().skip(1) {
            assert_eq!(*v, 0.0);
        }
        assert!(pick_onsets(&flux, 2.5).is_empty());
    }

    #[test]
    fn flux_tracks_energy_rises_only() {
        // One band jumps 1.0 -> 3.0 at frame 2 and holds: flux[2] sees the
        // rise averaged over bands, flux[3] sees no further rise.
        let frames = vec![
            vec![1.0_f32; 4],
            vec![1.0_f32; 4],
            vec![3.0, 1.0, 1.0, 1.0],
            vec![3.0, 1.0, 1.0, 1.0],
        ];
        let flux = spectral_flux(&frames);
        assert_eq!(flux.len(), 4);
        assert_eq!(flux[0], 0.0);
        assert_eq!(flux[1], 0.0);
        assert_eq!(flux[2], 0.5);
        assert_eq!(flux[3], 0.0);
        // Peak energy 3.0: floor is 3e-6, the 0.5 rise is a clear onset.
        assert_eq!(pick_onsets(&flux, 3.0), vec![2]);
    }

    #[test]
    fn onset_f1_edge_cases() {
        assert_eq!(onset_f1(&[], &[], 3), 1.0);
        assert_eq!(onset_f1(&[10], &[], 3), 0.0);
        assert_eq!(onset_f1(&[], &[10], 3), 0.0);
        assert_eq!(onset_f1(&[10, 50], &[11, 52], 3), 1.0);
        assert_eq!(onset_f1(&[10], &[50], 3), 0.0);
    }

    #[test]
    fn short_input_is_rejected() {
        let p = MirParams::v1();
        let tiny = vec![0.1_f32; 100];
        assert!(matches!(
            stft_magnitude(&tiny, &p),
            Err(EvalError::TooShort { .. })
        ));
        assert!(matches!(
            analyze(&tiny, &p),
            Err(EvalError::TooShort { .. })
        ));
        assert!(matches!(analyze(&[], &p), Err(EvalError::TooShort { .. })));
    }

    #[test]
    fn silence_is_rejected() {
        let p = MirParams::v1();
        let quiet = vec![0.0_f32; 8192];
        assert!(matches!(
            analyze(&quiet, &p),
            Err(EvalError::SilenceOrTooQuiet { .. })
        ));
    }

    #[test]
    fn non_finite_input_is_rejected() {
        let p = MirParams::v1();
        let mut bad = vec![0.1_f32; 4096];
        bad[7] = f32::NAN;
        assert!(matches!(
            analyze(&bad, &p),
            Err(EvalError::InvalidSamples(_))
        ));
    }
}
