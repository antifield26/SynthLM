//! MIR v1 feature pipeline: fixed-params analysis feeding loudness-first scoring.
//!
//! Parameter freeze (DEC-012): 48 kHz sample rate, primary Hann window 2048,
//! hop 512, plus a second tier (window 1024, hop 256, TSK-204) whose log-mel
//! is resampled to the primary frame grid and averaged with the primary
//! log-mel. Ordering (DEC-016): integrated LUFS is
//! measured first, the signal is gained to [`MirParams::target_lufs`]
//! (−14 LUFS), and only then are spectral features computed. Every
//! [`MirFeatures`] snapshot carries the [`MirParams`] it was produced
//! with so [`crate::score::compare`] can refuse cross-params comparisons
//! (both tiers participate in the equality key).
//!
//! Selection rationale (docs/research/C-models-retrieval-eval.md §4,
//! verified 2026-10-06 against the local cargo registry manifests):
//! STFT via `realfft` (MIT, real-to-complex wrapper over `rustfft`,
//! MIT OR Apache-2.0) and loudness via `ebur128` (MIT, pure-Rust EBU R128
//! implementation covering TECH 3341/3342 including true peak). The mel
//! filterbank and the linear-energy spectral-flux transient envelope are
//! hand-written (no `ruststft` dependency) so the DEC-012 constants stay
//! literal in this file.
//!
//! Why two tiers (TSK-204): the 2048 window resolves harmonics while the
//! 1024 window localises transients; averaging their log-mels keeps one
//! comparable grid while reducing single-window scalloping/ripple bias.
//! The transient envelope ([`spectral_flux`]/onsets) intentionally stays
//! primary-resolution so onset golden pins do not move with the tier mix.

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

/// Default second-tier STFT window in samples (TSK-204).
///
/// Half the primary window: doubles time resolution for transient detail
/// while keeping enough bins (513) for a stable 80-band mel projection.
pub const TIER2_WINDOW: usize = 1024;

/// Default second-tier STFT hop in samples (TSK-204).
///
/// Quarter of the primary hop, matching the halved window's overlap ratio.
pub const TIER2_HOP: usize = 256;

/// Frozen MIR v1 analysis parameters (DEC-012, extended with the TSK-204
/// second resolution tier).
///
/// All fields participate in [`PartialEq`]: [`crate::score::compare`]
/// refuses to score feature pairs produced with different params, so any
/// change here (window, hop, second tier, mel bands, rate, frequency
/// range, target) automatically invalidates cross-version comparisons
/// instead of silently producing incomparable numbers. Schema changes must
/// go through a version bump (`v1` → `v2`), never an in-place edit of
/// [`MirParams::v1`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MirParams {
    /// Sample rate in Hz. DEC-012 frozen value: 48000.
    pub sample_rate: u32,
    /// Primary STFT window length in samples (Hann). DEC-012 value: 2048.
    pub window: usize,
    /// Primary STFT hop in samples. DEC-012 frozen value: 512.
    pub hop: usize,
    /// Second-tier STFT window in samples (Hann). TSK-204 value: 1024.
    pub window2: usize,
    /// Second-tier STFT hop in samples. TSK-204 frozen value: 256.
    pub hop2: usize,
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
    /// The frozen MIR v1 parameter set (DEC-012 + DEC-016 + TSK-204 tier 2).
    ///
    /// Do not alter the returned values; introduce a new constructor for a
    /// new schema version instead.
    pub const fn v1() -> Self {
        Self {
            sample_rate: 48_000,
            window: 2048,
            hop: 512,
            window2: TIER2_WINDOW,
            hop2: TIER2_HOP,
            n_mel: 80,
            f_min_hz: 0.0,
            f_max_hz: 24_000.0,
            target_lufs: -14.0,
        }
    }

    /// Number of magnitude bins per primary STFT frame (`window / 2 + 1`).
    pub const fn n_bins(&self) -> usize {
        self.window / 2 + 1
    }

    /// Number of magnitude bins per second-tier STFT frame.
    pub const fn n_bins2(&self) -> usize {
        self.window2 / 2 + 1
    }

    /// Longer of the two analysis windows; inputs shorter than this hold
    /// no full frame in at least one tier.
    pub const fn max_window(&self) -> usize {
        if self.window >= self.window2 {
            self.window
        } else {
            self.window2
        }
    }
}

/// One analysed utterance: normalised-domain features plus loudness audit.
///
/// `integrated_lufs` is the pre-normalisation reading (diagnostic),
/// `gain_db` the applied ΔLUFS, and `normalized_lufs` / `normalized_dbtp`
/// are re-measured on the gained signal to close the loop (residual shows
/// the normalisation actually landed on target). `log_mel` is the
/// two-tier average (TSK-204); `magnitude` and the transient envelope stay
/// primary-resolution.
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
/// 1. Validate samples (non-empty, at least one window in *both* tiers,
///    all finite).
/// 2. Measure integrated LUFS + true peak via `ebur128` (EBU R128).
/// 3. Gain to [`MirParams::target_lufs`] (DEC-016) and re-measure to close
///    the loop.
/// 4. Primary STFT magnitude → 80-band linear mel energies (transient flux
///    and onsets are derived here, primary-resolution), plus a second-tier
///    STFT (1024/256) whose log-mel is resampled to the primary frame grid
///    and averaged with the primary log-mel (TSK-204).
///
/// Input is mono f32 at [`MirParams::sample_rate`]; multi-channel handling
/// (mix-down policy) is out of scope for v1 and must be decided by the
/// caller before calling this function.
pub fn analyze(samples: &[f32], params: &MirParams) -> Result<MirFeatures, EvalError> {
    if samples.len() < params.max_window() {
        return Err(EvalError::TooShort {
            need: params.max_window(),
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
    let log_primary = log_energies(&energies);
    // Second tier (TSK-204): same mel count, finer time grid, resampled to
    // the primary frame count and averaged in the log domain.
    let tier2 = MirParams {
        window: params.window2,
        hop: params.hop2,
        ..*params
    };
    let magnitude2 = stft_magnitude(&normalized, &tier2)?;
    let filterbank2 = mel_filterbank(&tier2);
    let log_tier2 = log_energies(&mel_energy(&magnitude2, &filterbank2));
    let log_mel = average_grids(
        &log_primary,
        &resample_frames(&log_tier2, log_primary.len()),
    );
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

/// AAC encoder delay (priming) in samples, consumer-side assumption.
///
/// FFmpeg-native AAC prepends 1024 priming samples and pads the tail to a
/// 1024-sample codec-frame multiple: the TSK-206 matrix decoded 44100 source
/// frames as 46080 (`44100 + 1024 + 956`, see
/// `experiments/decode-matrix.out.txt`), and symphonia surfaces the priming
/// instead of stripping it, so the consumer must trim before `analyze`.
/// Encoders with a different delay (e.g. 2112) need their own `trim_head`.
pub const AAC_PRIMING_SAMPLES: usize = 1024;

/// AAC codec frame multiple in samples (tail padding granularity).
pub const AAC_FRAME_SAMPLES: usize = 1024;

/// Consumer-side codec priming/padding trim (TSK-208).
///
/// Returns the window starting `trim_head` samples in and holding at most
/// `target_len` samples (the source frame count): drop the head, then
/// truncate the tail. Out-of-range inputs clamp instead of panicking — a
/// `trim_head` past the end yields an empty slice, and a `target_len` past
/// the available tail yields what remains. Deterministic: identical inputs
/// give identical outputs.
///
/// The AAC rule is `strip_codec_padding(decoded, AAC_PRIMING_SAMPLES,
/// source_len)`; after the trim the samples must be bit-identical to the
/// source (lossless-wav simulation) or lossy-close (real AAC), which
/// `tests/loudness_xcheck.rs` asserts through `compare`.
pub fn strip_codec_padding(samples: &[f32], trim_head: usize, target_len: usize) -> &[f32] {
    let start = trim_head.min(samples.len());
    let end = start.saturating_add(target_len).min(samples.len());
    &samples[start..end]
}

/// Gain mono samples to [`MirParams::target_lufs`] (TSK-402 blind stimuli).
///
/// Measures integrated LUFS with the same `ebur128` meter as
/// [`crate::mir::analyze`], then applies the constant ΔLUFS gain
/// (`target − measured`). The returned signal re-measures at `target` up to
/// meter tolerance; silence/ungated inputs fail with
/// [`EvalError::SilenceOrTooQuiet`](crate::EvalError) or
/// [`EvalError::Loudness`](crate::EvalError) instead of producing a
/// non-finite gain. Deterministic: identical inputs give bit-identical
/// outputs for a fixed `ebur128` version.
pub fn normalize_to_target(samples: &[f32], params: &MirParams) -> Result<Vec<f32>, EvalError> {
    let (integrated, _) = measure_loudness(samples, params.sample_rate)?;
    let gain_db = params.target_lufs - integrated;
    let factor = 10.0_f64.powf(gain_db / 20.0);
    if !factor.is_finite() {
        return Err(EvalError::Loudness(format!(
            "non-finite normalisation gain from {integrated} LUFS"
        )));
    }
    let mut out = Vec::with_capacity(samples.len());
    for s in samples {
        let v = f64::from(*s) * factor;
        if !v.is_finite() {
            return Err(EvalError::InvalidSamples(
                "normalised sample is not finite (input out of range)".to_string(),
            ));
        }
        out.push(v as f32);
    }
    Ok(out)
}

/// Per-frame linear-magnitude spectral centroid in Hz (TSK-402 brightness).
///
/// Computed from a primary-resolution magnitude spectrogram as
/// `Σ(freq_k · mag_k) / Σ(mag_k)` with `freq_k = k · sample_rate / window`.
/// Zero-energy frames (silent) report `0.0` instead of dividing by zero.
/// Reuses the [`crate::mir::stft_magnitude`] output grid; no new transform is
/// introduced. Deterministic: identical inputs give bit-identical outputs.
pub fn spectral_centroid(magnitude: &[Vec<f32>], sample_rate: u32, window: usize) -> Vec<f32> {
    let rate = f64::from(sample_rate);
    let win = window as f64;
    magnitude
        .iter()
        .map(|frame| {
            let mut num = 0.0_f64;
            let mut den = 0.0_f64;
            for (k, &m) in frame.iter().enumerate() {
                let mag = f64::from(m);
                num += (k as f64 * rate / win) * mag;
                den += mag;
            }
            if den > 0.0 && den.is_finite() && num.is_finite() {
                (num / den) as f32
            } else {
                0.0
            }
        })
        .collect()
}

/// Mean spectral centroid in Hz over all frames (TSK-402 brightness rank).
///
/// Arithmetic mean of [`crate::mir::spectral_centroid`]; empty input reports
/// `0.0`. Brighter (less low-passed) stimuli score higher; the TSK-402 B
/// series ground truth is centroid-monotonic in cutoff order.
pub fn mean_spectral_centroid(magnitude: &[Vec<f32>], sample_rate: u32, window: usize) -> f32 {
    let per = spectral_centroid(magnitude, sample_rate, window);
    if per.is_empty() {
        return 0.0;
    }
    let mut acc = 0.0_f64;
    for v in &per {
        acc += f64::from(*v);
    }
    (acc / per.len() as f64) as f32
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

/// Resample a `[frame][band]` grid to `target_frames` rows (TSK-204).
///
/// Linear interpolation on the frame index maps source endpoints onto
/// target endpoints, so identical grids stay identical and stationary
/// signals keep a flat envelope. A single target frame holds the
/// per-band mean of the source (energy-preserving for the degenerate
/// one-frame case). Empty input or a zero target yields an empty grid;
/// every frame is assumed to hold the same band count (guaranteed by
/// [`mel_energy`]). Deterministic: identical inputs give bit-identical
/// outputs.
pub fn resample_frames(frames: &[Vec<f32>], target_frames: usize) -> Vec<Vec<f32>> {
    if frames.is_empty() || target_frames == 0 {
        return Vec::new();
    }
    if frames.len() == target_frames {
        return frames.to_vec();
    }
    let n_bands = frames[0].len();
    if target_frames == 1 {
        let mut acc = vec![0.0_f64; n_bands];
        for frame in frames {
            for (slot, &v) in acc.iter_mut().zip(frame.iter()) {
                *slot += f64::from(v);
            }
        }
        let denom = frames.len() as f64;
        return vec![acc.iter().map(|&s| (s / denom) as f32).collect()];
    }
    let src = frames.len();
    (0..target_frames)
        .map(|i| {
            let pos = i as f64 * (src - 1) as f64 / (target_frames - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(src - 1);
            let t = (pos - lo as f64) as f32;
            frames[lo]
                .iter()
                .zip(frames[hi].iter())
                .map(|(&a, &b)| a + (b - a) * t)
                .collect()
        })
        .collect()
}

/// Element-wise mean of two `[frame][band]` grids (TSK-204 tier fusion).
///
/// Callers must resample both grids to the same frame count first (see
/// [`resample_frames`]); rows/bands beyond the shorter grid are ignored by
/// construction of `zip`, so mismatched inputs degrade to truncation
/// rather than a panic. Both tiers share [`MirParams::n_mel`] bands, so in
/// the pipeline the average is exact.
pub fn average_grids(a: &[Vec<f32>], b: &[Vec<f32>]) -> Vec<Vec<f32>> {
    a.iter()
        .zip(b.iter())
        .map(|(ra, rb)| {
            ra.iter()
                .zip(rb.iter())
                .map(|(&x, &y)| 0.5 * (x + y))
                .collect()
        })
        .collect()
}

/// Centre frequency (Hz) of each mel band (TSK-204 band-split key).
///
/// Uses the same mel-uniform point layout as [`mel_filterbank`]: band `m`
/// (1-based in filterbank terms) peaks at interpolation point `m`, so the
/// returned vector has [`MirParams::n_mel`] entries. [`crate::score`]
/// maps these centres to low/mid/high regions for weighted scoring.
pub fn mel_band_centers(params: &MirParams) -> Vec<f32> {
    let mel_low = hz_to_mel(params.f_min_hz);
    let mel_high = hz_to_mel(params.f_max_hz);
    (1..=params.n_mel)
        .map(|m| mel_to_hz(mel_low + (mel_high - mel_low) * m as f32 / (params.n_mel + 1) as f32))
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
        assert_eq!(p.window2, TIER2_WINDOW);
        assert_eq!(p.hop2, TIER2_HOP);
        assert_eq!(p.window2, 1024);
        assert_eq!(p.hop2, 256);
        assert_eq!(p.n_mel, 80);
        assert_eq!(p.f_min_hz, 0.0);
        assert_eq!(p.f_max_hz, 24_000.0);
        assert_eq!(p.target_lufs, -14.0);
        assert_eq!(p.n_bins(), 1025);
        assert_eq!(p.n_bins2(), 513);
        assert_eq!(p.max_window(), 2048);
    }

    #[test]
    fn resample_frames_is_identity_and_mean() {
        let grid = vec![vec![1.0_f32, 2.0], vec![3.0, 4.0], vec![5.0, 6.0]];
        assert_eq!(resample_frames(&grid, 3), grid);
        assert!(resample_frames(&grid, 0).is_empty());
        assert!(resample_frames(&[], 4).is_empty());
        // Single target frame holds the per-band mean.
        let mean = resample_frames(&grid, 1);
        assert_eq!(mean.len(), 1);
        assert!((mean[0][0] - 3.0).abs() < 1e-6);
        assert!((mean[0][1] - 4.0).abs() < 1e-6);
        // Endpoints are pinned; midpoint interpolates linearly.
        let up = resample_frames(&grid, 5);
        assert_eq!(up.len(), 5);
        assert_eq!(up[0], grid[0]);
        assert_eq!(up[4], grid[2]);
        assert!((up[2][0] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn average_grids_means_elementwise() {
        let a = vec![vec![1.0_f32, 3.0]];
        let b = vec![vec![3.0_f32, 5.0]];
        assert_eq!(average_grids(&a, &b), vec![vec![2.0_f32, 4.0]]);
        assert_eq!(average_grids(&a, &a), a);
    }

    #[test]
    fn band_centers_span_full_range() {
        let p = MirParams::v1();
        let centers = mel_band_centers(&p);
        assert_eq!(centers.len(), 80);
        assert!(centers[0] > 0.0 && centers[0] < 400.0);
        assert!(centers[79] > 4000.0 && centers[79] < 24_000.0);
        for pair in centers.windows(2) {
            assert!(pair[0] < pair[1], "centres must rise monotonically");
        }
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
    fn strip_codec_padding_trims_head_and_tail() {
        let samples: Vec<f32> = (0..5000).map(|i| i as f32).collect();
        let trimmed = strip_codec_padding(&samples, 1024, 2048);
        assert_eq!(trimmed.len(), 2048);
        assert_eq!(trimmed[0], 1024.0);
        assert_eq!(trimmed[2047], 3071.0);
    }

    #[test]
    fn strip_codec_padding_clamps_out_of_range() {
        let samples: Vec<f32> = (0..100).map(|i| i as f32).collect();
        // Head past the end yields empty, never a panic.
        assert!(strip_codec_padding(&samples, 100, 50).is_empty());
        assert!(strip_codec_padding(&samples, 10_000, 50).is_empty());
        // Tail past the available samples yields what remains.
        let tail = strip_codec_padding(&samples, 90, 5000);
        assert_eq!(tail.len(), 10);
        assert_eq!(tail[0], 90.0);
        // Zero trim of the full length is the identity window.
        assert_eq!(strip_codec_padding(&samples, 0, 100), samples.as_slice());
        assert!(strip_codec_padding(&[], 1024, 44100).is_empty());
    }

    #[test]
    fn strip_codec_padding_covers_aac_matrix_shape() {
        // TSK-206 matrix shape: 44100 source frames decode as 46080
        // (1024 priming + 956 tail padding); the AAC rule recovers the
        // source window exactly.
        let decoded = vec![0.25_f32; 46_080];
        let trimmed = strip_codec_padding(&decoded, AAC_PRIMING_SAMPLES, 44_100);
        assert_eq!(AAC_PRIMING_SAMPLES, 1024);
        assert_eq!(AAC_FRAME_SAMPLES, 1024);
        assert_eq!(trimmed.len(), 44_100);
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
