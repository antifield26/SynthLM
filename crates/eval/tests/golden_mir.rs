//! Golden MIR v1 regression: fixed synthetic inputs → fixed score intervals.
//!
//! No external files are read; all signals are generated programmatically
//! below. Pinned intervals in this file come from measured runs (see dates
//! on each test); tightening them further is TSK-208/TSK-402 calibration
//! work, not drift to be silently absorbed.
//!
//! TSK-204 regression set in this file:
//!
//! - `golden_plus_6db_must_not_score_higher` (overall gain invariance),
//! - `golden_lowband_boost_is_not_rewarded` (per-band +6 dB cheat),
//! - `golden_clipped_peak_warns_dbtp` (hard clip: known degradation +
//!   dBTP alarm),
//! - `golden_runs_are_deterministic` (triple-run zero-variance gate,
//!   manual equivalent of the render null-test harness),
//! - `golden_multires_ordering_smoke` (hop/window variants keep the
//!   identical < transient < timbre distance ordering).

use synthlm_eval::mir::{analyze, stft_magnitude};
use synthlm_eval::{EvalError, MirParams, compare};

const SR: f32 = 48_000.0;
const SECS: usize = 2;
const N: usize = 48_000 * SECS;

/// Anti-cheat gate (TSK-204, DEC-016/L5): a loudness/clipping manipulation
/// must never score *higher* — smaller spectral distance or larger transient
/// similarity — than the honest identical-input baseline.
macro_rules! assert_not_better {
    ($cand:expr, $base:expr) => {
        assert!(
            $cand.spec_l1 >= $base.spec_l1,
            "spec_l1 improved by manipulation: cand={} base={}",
            $cand.spec_l1,
            $base.spec_l1
        );
        assert!(
            $cand.mel_l1 >= $base.mel_l1,
            "mel_l1 improved by manipulation: cand={} base={}",
            $cand.mel_l1,
            $base.mel_l1
        );
        assert!(
            $cand.mel_weighted >= $base.mel_weighted,
            "mel_weighted improved by manipulation: cand={} base={}",
            $cand.mel_weighted,
            $base.mel_weighted
        );
        assert!(
            $cand.transient_f1 <= $base.transient_f1,
            "transient_f1 improved by manipulation: cand={} base={}",
            $cand.transient_f1,
            $base.transient_f1
        );
    };
}

/// Sum of sine partials `(freq_hz, peak_amp)`.
///
/// Phase is accumulated in f64: an f32 `sin(2πft)` with `t` in seconds
/// loses argument-reduction precision for large `2πft` (radians ~1e4 at the
/// end of a 2 s tone), which amplitude-modulates the tone by ~0.5% and
/// shows up as phantom spectral flux. Real sampled audio has no such
/// error; the generator must not inject it.
fn sine_mix(parts: &[(f32, f32)]) -> Vec<f32> {
    let mut out = vec![0.0_f32; N];
    for &(freq, amp) in parts {
        let step = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(SR);
        let mut phase = 0.0_f64;
        for slot in out.iter_mut() {
            *slot += (f64::from(amp) * phase.sin()) as f32;
            phase += step;
        }
    }
    out
}

/// Reference timbre: three harmonic partials.
///
/// Frequencies are exact DFT bin centres (bin width 23.4375 Hz at
/// 48 kHz / 2048): a bin-centred stationary tone has constant windowed
/// magnitude across frames, so the reference carries no scalloping ripple
/// and any detected onset is a genuine envelope event. Do not retune these
/// to "musical" pitches without re-pinning every interval below.
fn signal_a() -> Vec<f32> {
    sine_mix(&[(445.3125, 0.5), (890.625, 0.25), (1335.9375, 0.125)])
}

/// Different timbre: shared root, other harmonics (all bin-centred).
fn signal_b_timbre() -> Vec<f32> {
    sine_mix(&[(445.3125, 0.5), (656.25, 0.3), (984.375, 0.2)])
}

/// Reference plus one 5 ms Hann-windowed 2 kHz burst at t = 0.5 s.
fn signal_b_transient() -> Vec<f32> {
    let mut x = signal_a();
    let start = 24_000;
    let len = 240;
    for (k, slot) in x.iter_mut().skip(start).take(len).enumerate() {
        let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * k as f32 / (len - 1) as f32).cos());
        let t = (start + k) as f32 / SR;
        *slot += 0.6 * w * (2.0 * std::f32::consts::PI * 2_000.0 * t).sin();
    }
    x
}

/// Low-band +6 dB cheat: root partial doubled, upper partials untouched.
///
/// Models the "bass loudness" cheat — after LUFS normalisation the overall
/// gain vanishes but the tilted spectrum must remain visible as distance.
fn signal_low_boosted() -> Vec<f32> {
    sine_mix(&[(445.3125, 1.0), (890.625, 0.25), (1335.9375, 0.125)])
}

/// Hard-clipped hot-peak case: quiet bed plus full-scale impulses.
///
/// The bed is `signal_a` at −12 dB so LUFS normalisation gains the signal
/// back up; the three single-sample impulses (×1.2, then clamped to
/// ±1.0 = 0 dBFS flat tops) model clipped peaks. Post-normalisation they
/// sit well above full scale, so the dBTP alarm (`true_peak > −1`) must
/// fire while the spectral/onset distances report known degradation.
fn signal_clipped() -> Vec<f32> {
    let mut x: Vec<f32> = signal_a().iter().map(|s| s * 0.25).collect();
    for &pos in &[24_000, 48_000, 72_000] {
        x[pos] += 1.2;
    }
    x.iter().map(|s| s.clamp(-1.0, 1.0)).collect()
}

/// Mean absolute difference of two raw magnitude spectrograms (test-local).
fn raw_spec_l1(a: &[Vec<f32>], b: &[Vec<f32>]) -> f64 {
    let mut acc = 0.0_f64;
    let mut count = 0_usize;
    for (ra, rb) in a.iter().zip(b.iter()) {
        for (&x, &y) in ra.iter().zip(rb.iter()) {
            acc += f64::from((x - y).abs());
            count += 1;
        }
    }
    acc / count as f64
}

#[test]
fn golden_identical_input_scores_zero() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let a = signal_a();
    let fa = analyze(&a, &params)?;
    let fb = analyze(&a.clone(), &params)?;
    let score = compare(&fa, &fb)?;
    eprintln!(
        "GOLDEN identical: spec_l1={:.3e} mel_l1={:.3e} w={:.3e} low={:.3e} mid={:.3e} high={:.3e} f1={:.6} lufs_i={:.3} dbtp={:.3} delta={:.3}",
        score.spec_l1,
        score.mel_l1,
        score.mel_weighted,
        score.mel_low,
        score.mel_mid,
        score.mel_high,
        score.transient_f1,
        score.lufs_i,
        score.true_peak,
        score.delta_lufs
    );
    assert_eq!(score.spec_l1, 0.0);
    assert_eq!(score.mel_l1, 0.0);
    assert_eq!(score.mel_low, 0.0);
    assert_eq!(score.mel_mid, 0.0);
    assert_eq!(score.mel_high, 0.0);
    assert_eq!(score.mel_weighted, 0.0);
    assert_eq!(score.transient_f1, 1.0);
    assert_eq!(score.clap_cos, None);
    // Pinned 2026-10-06 (first green run on the dev machine; ebur128 +
    // realfft are deterministic for fixed inputs, so any drift means the
    // pipeline changed and must be re-examined, not absorbed):
    // lufs_i=-14.000, dbtp=-8.819, delta=-5.650.
    assert!(
        (score.lufs_i - params.target_lufs).abs() < 0.1,
        "lufs_i={}",
        score.lufs_i
    );
    assert!(
        (-9.0..=-8.6).contains(&score.true_peak),
        "dbtp={}",
        score.true_peak
    );
    assert!(
        (-5.8..=-5.5).contains(&score.delta_lufs),
        "delta={}",
        score.delta_lufs
    );
    Ok(())
}

#[test]
fn golden_timbre_pair_is_discriminative() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let fa = analyze(&signal_a(), &params)?;
    let fb = analyze(&signal_b_timbre(), &params)?;
    let score = compare(&fa, &fb)?;
    eprintln!(
        "GOLDEN timbre: spec_l1={:.6} mel_l1={:.6} w={:.6} low={:.6} mid={:.6} high={:.6} f1={:.6}",
        score.spec_l1,
        score.mel_l1,
        score.mel_weighted,
        score.mel_low,
        score.mel_mid,
        score.mel_high,
        score.transient_f1
    );
    // Pinned 2026-10-06 (TSK-204 two-tier average; source: first multires
    // run on the dev machine — spec is primary-resolution so its pin
    // carries over unchanged at spec_l1=0.456140, while mel moved
    // 1.875628 → 1.775870 as the 1024-tier dilutes the harmonic
    // differences with coarser bins; any drift means the pipeline changed
    // and must be re-examined, not absorbed):
    // spec_l1=0.456140, mel_l1=1.775870, w=1.949957,
    // low=0.077270, mid=4.281723, high≈0, f1=1.0 (both onset lists empty —
    // stationary tones carry no envelope events, so the timbre difference
    // lives entirely in the spectral distances, concentrated in MID where
    // the differing harmonics sit; HIGH is floor-clamped in both inputs
    // because neither signal carries energy above 4 kHz).
    assert!(
        (0.40..=0.52).contains(&score.spec_l1),
        "spec_l1={}",
        score.spec_l1
    );
    assert!(
        (1.65..=1.90).contains(&score.mel_l1),
        "mel_l1={}",
        score.mel_l1
    );
    assert!(
        (1.82..=2.08).contains(&score.mel_weighted),
        "mel_weighted={}",
        score.mel_weighted
    );
    assert!(
        (0.05..=0.12).contains(&score.mel_low),
        "mel_low={}",
        score.mel_low
    );
    assert!(
        (4.00..=4.60).contains(&score.mel_mid),
        "mel_mid={}",
        score.mel_mid
    );
    assert!(
        score.mel_high < 1e-6,
        "mel_high must stay floor-clamped: {}",
        score.mel_high
    );
    assert_eq!(score.transient_f1, 1.0);
    Ok(())
}

#[test]
fn golden_transient_burst_lowers_f1() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let fa = analyze(&signal_a(), &params)?;
    let fb = analyze(&signal_b_transient(), &params)?;
    let score = compare(&fa, &fb)?;
    eprintln!(
        "GOLDEN transient: spec_l1={:.6} mel_l1={:.6} w={:.6} f1={:.6} ref_onsets={:?} cand_onsets={:?}",
        score.spec_l1, score.mel_l1, score.mel_weighted, score.transient_f1, fa.onsets, fb.onsets
    );
    // Pinned 2026-10-06 (TSK-204; source: first multires run — flux and
    // magnitude stay primary-resolution, so spec_l1=0.004105 and the
    // single burst onset carry over unchanged; mel moved 0.054821 →
    // 0.037580 under tier averaging, and the weighted value tracks it):
    // spec_l1=0.004105, mel_l1=0.037580, w=0.042742, f1=0.0. The burst at
    // t=0.5 s (sample 24000) is the only envelope event: reference has no
    // onsets, candidate has exactly the burst frame.
    assert!(fa.onsets.is_empty(), "ref_onsets={:?}", fa.onsets);
    assert_eq!(fb.onsets.len(), 1, "cand_onsets={:?}", fb.onsets);
    assert!(
        (0.002..=0.008).contains(&score.spec_l1),
        "spec_l1={}",
        score.spec_l1
    );
    assert!(
        (0.025..=0.055).contains(&score.mel_l1),
        "mel_l1={}",
        score.mel_l1
    );
    assert!(
        (0.030..=0.060).contains(&score.mel_weighted),
        "mel_weighted={}",
        score.mel_weighted
    );
    assert_eq!(score.transient_f1, 0.0);
    Ok(())
}

#[test]
fn golden_plus_6db_must_not_score_higher() -> Result<(), EvalError> {
    // L5 anti-cheat (DEC-016): +6 dB of pure gain must vanish in the
    // normalised distances; only ΔLUFS reports the gain.
    let params = MirParams::v1();
    let a = signal_a();
    let loud: Vec<f32> = a.iter().map(|s| s * 2.0).collect();
    let fa = analyze(&a, &params)?;
    let fc = analyze(&loud, &params)?;
    let base = compare(&fa, &fa.clone())?;
    let score = compare(&fa, &fc)?;
    eprintln!(
        "GOLDEN +6dB: spec_l1={:.3e} mel_l1={:.3e} w={:.3e} f1={:.6} delta={:.3} lufs_i={:.3} dbtp={:.3}",
        score.spec_l1,
        score.mel_l1,
        score.mel_weighted,
        score.transient_f1,
        score.delta_lufs,
        score.lufs_i,
        score.true_peak
    );
    // The formal gate: pure gain must not score higher than identical.
    assert_not_better!(score, base);
    assert!(score.spec_l1 <= 1e-6, "spec_l1={}", score.spec_l1);
    assert!(score.mel_l1 <= 1e-4, "mel_l1={}", score.mel_l1);
    assert!(score.mel_weighted <= 1e-4, "w={}", score.mel_weighted);
    assert_eq!(score.transient_f1, 1.0);
    // The reported ΔLUFS must equal the reference gain minus the exact
    // +6.0206 dB of a ×2 amplitude scaling (ebur128 doubling check).
    let expected_delta = fa.gain_db - 6.0206;
    assert!(
        (score.delta_lufs - expected_delta).abs() < 0.15,
        "delta={} expected={expected_delta}",
        score.delta_lufs
    );
    // The normalisation must do the work: raw (unnormalised) spectral
    // distance has to dwarf the normalised one.
    let raw_a = stft_magnitude(&a, &params)?;
    let raw_c = stft_magnitude(&loud, &params)?;
    let raw = raw_spec_l1(&raw_a, &raw_c);
    eprintln!("GOLDEN +6dB: raw_spec_l1={raw:.6}");
    assert!(
        raw > 100.0 * f64::from(score.spec_l1.max(1e-9)),
        "raw={raw}"
    );
    Ok(())
}

#[test]
fn golden_lowband_boost_is_not_rewarded() -> Result<(), EvalError> {
    // Per-band +6 dB cheat (TSK-204): doubling the root partial raises raw
    // loudness, but after normalisation the tilted spectrum must still read
    // as distance — never as an improvement over the honest baseline.
    let params = MirParams::v1();
    let fa = analyze(&signal_a(), &params)?;
    let fb = analyze(&signal_low_boosted(), &params)?;
    let base = compare(&fa, &fa.clone())?;
    let score = compare(&fa, &fb)?;
    eprintln!(
        "GOLDEN lowboost: spec_l1={:.6} mel_l1={:.6} w={:.6} low={:.6} mid={:.6} high={:.6} f1={:.6} delta={:.3} dbtp={:.3}",
        score.spec_l1,
        score.mel_l1,
        score.mel_weighted,
        score.mel_low,
        score.mel_mid,
        score.mel_high,
        score.transient_f1,
        score.delta_lufs,
        score.true_peak
    );
    assert_not_better!(score, base);
    // Pinned 2026-10-06 (TSK-204; source: first multires run):
    // spec_l1=0.117193, mel_l1=0.167906, w=0.205353,
    // low=0.135559, mid=0.365967, high≈0, f1=1.0, delta=-10.692.
    // Read-off: doubling the root raises raw loudness ~5 dB (delta −10.69
    // vs the reference −5.65), normalisation then turns everything down, so
    // the cheat's visibility lives mostly in MID (upper partials sit ~5 dB
    // low) while HIGH stays floor-clamped in both inputs (no energy above
    // 4 kHz either side; 1e-6 slack covers toolchain float drift).
    assert!(
        (0.10..=0.14).contains(&score.spec_l1),
        "spec_l1={}",
        score.spec_l1
    );
    assert!(
        (0.14..=0.20).contains(&score.mel_l1),
        "mel_l1={}",
        score.mel_l1
    );
    assert!(
        (0.18..=0.24).contains(&score.mel_weighted),
        "mel_weighted={}",
        score.mel_weighted
    );
    assert!(
        (0.11..=0.17).contains(&score.mel_low),
        "mel_low={}",
        score.mel_low
    );
    assert!(
        (0.33..=0.41).contains(&score.mel_mid),
        "mel_mid={}",
        score.mel_mid
    );
    assert!(
        score.mel_high < 1e-6,
        "mel_high must stay floor-clamped: {}",
        score.mel_high
    );
    assert!(
        (-10.85..=-10.55).contains(&score.delta_lufs),
        "delta={}",
        score.delta_lufs
    );
    // A pure level tilt adds no envelope event: onsets stay empty on both
    // sides, so F1 must remain agreement.
    assert_eq!(score.transient_f1, 1.0);
    Ok(())
}

#[test]
fn golden_clipped_peak_warns_dbtp() -> Result<(), EvalError> {
    // Hard-clip case (TSK-204): known degradation that must (a) never score
    // higher than the honest baseline and (b) raise the dBTP alarm
    // (`true_peak > −1`, DEC-016 downstream-limiting flag).
    let params = MirParams::v1();
    let raw = signal_clipped();
    // The harness really does clip: flat tops at exactly ±1.0 (0 dBFS).
    let clipped_tops = raw.iter().filter(|s| s.abs() >= 1.0).count();
    assert!(
        clipped_tops >= 3,
        "expected ≥3 samples at 0 dBFS, got {clipped_tops}"
    );
    let fa = analyze(&signal_a(), &params)?;
    let fb = analyze(&raw, &params)?;
    let base = compare(&fa, &fa.clone())?;
    let score = compare(&fa, &fb)?;
    eprintln!(
        "GOLDEN clip: spec_l1={:.6} mel_l1={:.6} w={:.6} low={:.6} mid={:.6} high={:.6} f1={:.6} dbtp={:.3} delta={:.3} cand_onsets={:?}",
        score.spec_l1,
        score.mel_l1,
        score.mel_weighted,
        score.mel_low,
        score.mel_mid,
        score.mel_high,
        score.transient_f1,
        score.true_peak,
        score.delta_lufs,
        fb.onsets
    );
    assert_not_better!(score, base);
    // Pinned 2026-10-06 (TSK-204; source: first run on the dev machine):
    // spec_l1=0.070654, mel_l1=0.523018, w=0.465398,
    // low=0.372555, mid=0.423262, high=0.652655, f1=0.0,
    // dbtp=+6.355, delta=+6.355, cand_onsets=[45, 91, 138].
    // Read-off: the −12 dB bed is gained back up (+6.36 dB), so the 0 dBFS
    // flat tops land at +6.36 dBTP — dbtp equals delta here because the raw
    // peak is exactly full scale. Click energy is HF-heavy (high > mid),
    // and each impulse trips the flux picker, so F1 collapses to 0.
    assert!(
        (0.055..=0.090).contains(&score.spec_l1),
        "spec_l1={}",
        score.spec_l1
    );
    assert!(
        (0.45..=0.60).contains(&score.mel_l1),
        "mel_l1={}",
        score.mel_l1
    );
    assert!(
        (0.40..=0.53).contains(&score.mel_weighted),
        "mel_weighted={}",
        score.mel_weighted
    );
    assert!(
        score.mel_high > score.mel_mid,
        "clicks must read HF-heavy: high={} mid={}",
        score.mel_high,
        score.mel_mid
    );
    assert!(
        (5.90..=6.80).contains(&score.true_peak),
        "dbtp={}",
        score.true_peak
    );
    assert!(
        (6.10..=6.60).contains(&score.delta_lufs),
        "delta={}",
        score.delta_lufs
    );
    assert_eq!(score.transient_f1, 0.0);
    assert_eq!(fb.onsets.len(), 3, "cand_onsets={:?}", fb.onsets);
    // …but the peaks must alarm: normalisation gains the quiet bed back up
    // and the 0 dBFS flat tops land far above full scale.
    assert!(
        score.true_peak > -1.0,
        "dBTP alarm must fire on clipped peaks: dbtp={}",
        score.true_peak
    );
    Ok(())
}

#[test]
fn golden_runs_are_deterministic() -> Result<(), EvalError> {
    // Triple-run variance gate, manual equivalent (TSK-204, DEC-024): the
    // pipeline is a pure function, so three identical runs must have zero
    // variance — bit-identical features and bit-identical scores.
    let params = MirParams::v1();
    let a = signal_a();
    let fa = analyze(&a, &params)?;
    let fb = analyze(&a, &params)?;
    assert_eq!(fa.params, fb.params);
    assert_eq!(fa.n_frames, fb.n_frames);
    assert_eq!(fa.magnitude, fb.magnitude);
    assert_eq!(fa.log_mel, fb.log_mel);
    assert_eq!(fa.flux, fb.flux);
    assert_eq!(fa.onsets, fb.onsets);
    assert_eq!(fa.integrated_lufs, fb.integrated_lufs);
    assert_eq!(fa.gain_db, fb.gain_db);
    assert_eq!(fa.normalized_lufs, fb.normalized_lufs);
    assert_eq!(fa.normalized_dbtp, fb.normalized_dbtp);
    let mut specs = Vec::with_capacity(3);
    let mut mels = Vec::with_capacity(3);
    for _ in 0..3 {
        let f = analyze(&signal_b_timbre(), &params)?;
        let s = compare(&fa, &f)?;
        specs.push(s.spec_l1);
        mels.push(s.mel_l1);
    }
    eprintln!("GOLDEN determinism: specs={specs:?} mels={mels:?}");
    assert_eq!(specs[0], specs[1]);
    assert_eq!(specs[1], specs[2]);
    assert_eq!(mels[0], mels[1]);
    assert_eq!(mels[1], mels[2]);
    Ok(())
}

#[test]
fn golden_multires_ordering_smoke() -> Result<(), EvalError> {
    // Hop/window-variant smoke (TSK-204): under every resolution combo the
    // distance ordering identical < transient < timbre must hold and every
    // distance must be finite.
    let variants = [
        ("v1", MirParams::v1()),
        (
            "fine-tier2",
            MirParams {
                window2: 512,
                hop2: 128,
                ..MirParams::v1()
            },
        ),
        (
            "coarse-primary",
            MirParams {
                window: 4096,
                hop: 1024,
                ..MirParams::v1()
            },
        ),
    ];
    for (name, params) in variants {
        let fa = analyze(&signal_a(), &params)?;
        let f_timbre = analyze(&signal_b_timbre(), &params)?;
        let f_trans = analyze(&signal_b_transient(), &params)?;
        let d_id = compare(&fa, &fa.clone())?.mel_l1;
        let d_tr = compare(&fa, &f_trans)?.mel_l1;
        let d_ti = compare(&fa, &f_timbre)?.mel_l1;
        eprintln!("GOLDEN ordering [{name}]: id={d_id:.6} trans={d_tr:.6} timbre={d_ti:.6}");
        assert!(
            d_id.is_finite() && d_tr.is_finite() && d_ti.is_finite(),
            "[{name}]"
        );
        assert_eq!(d_id, 0.0, "[{name}] identical must score zero");
        assert!(d_tr > 1e-6, "[{name}] transient must differ: {d_tr}");
        // Smoke bounds from the 2026-10-06 run (v1: 0.038/1.776,
        // fine-tier2: 0.033/1.336, coarse-primary: 0.075/1.841): every
        // resolution must keep the burst small but nonzero and the timbre
        // pair clearly discriminative.
        assert!(d_tr < 0.15, "[{name}] transient too far: {d_tr}");
        assert!(d_ti > 1.0, "[{name}] timbre not discriminative: {d_ti}");
        assert!(
            d_tr < d_ti,
            "[{name}] ordering violated: trans={d_tr} timbre={d_ti}"
        );
    }
    Ok(())
}

#[test]
fn golden_rejects_incomparable() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let fa = analyze(&signal_a(), &params)?;
    let other = MirParams {
        n_mel: 40,
        ..MirParams::v1()
    };
    let fb = analyze(&signal_a(), &other)?;
    assert!(matches!(compare(&fa, &fb), Err(EvalError::ParamMismatch)));
    // The second tier joins the comparability key: same primary grid with
    // a different tier-2 hop is still incomparable.
    let tier_mismatch = MirParams {
        hop2: 128,
        ..MirParams::v1()
    };
    let fc = analyze(&signal_a(), &tier_mismatch)?;
    assert!(matches!(compare(&fa, &fc), Err(EvalError::ParamMismatch)));
    Ok(())
}
