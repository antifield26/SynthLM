//! Golden MIR v1 regression: fixed synthetic inputs → fixed score intervals.
//!
//! No external files are read; all signals are generated programmatically
//! below. Pinned intervals in this file come from the first measured run
//! (see comments on each test); tightening them further is TSK-208
//! calibration work, not drift to be silently absorbed.

use synthlm_eval::mir::{analyze, stft_magnitude};
use synthlm_eval::{EvalError, MirParams, compare};

const SR: f32 = 48_000.0;
const SECS: usize = 2;
const N: usize = 48_000 * SECS;

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
        "GOLDEN identical: spec_l1={:.3e} mel_l1={:.3e} f1={:.6} lufs_i={:.3} dbtp={:.3} delta={:.3}",
        score.spec_l1,
        score.mel_l1,
        score.transient_f1,
        score.lufs_i,
        score.true_peak,
        score.delta_lufs
    );
    assert_eq!(score.spec_l1, 0.0);
    assert_eq!(score.mel_l1, 0.0);
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
        "GOLDEN timbre: spec_l1={:.6} mel_l1={:.6} f1={:.6}",
        score.spec_l1, score.mel_l1, score.transient_f1
    );
    // Pinned 2026-10-06 (first green run): spec_l1=0.456140,
    // mel_l1=1.875628, f1=1.0 (both onset lists empty — stationary tones
    // carry no envelope events, so the timbre difference lives entirely in
    // the spectral distances).
    assert!(
        (0.40..=0.52).contains(&score.spec_l1),
        "spec_l1={}",
        score.spec_l1
    );
    assert!(
        (1.70..=2.05).contains(&score.mel_l1),
        "mel_l1={}",
        score.mel_l1
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
        "GOLDEN transient: spec_l1={:.6} mel_l1={:.6} f1={:.6} ref_onsets={:?} cand_onsets={:?}",
        score.spec_l1, score.mel_l1, score.transient_f1, fa.onsets, fb.onsets
    );
    // Pinned 2026-10-06 (first green run): spec_l1=0.004105,
    // mel_l1=0.054821, f1=0.0. The burst at t=0.5 s (sample 24000) is the
    // only envelope event: reference has no onsets, candidate has exactly
    // the burst frame.
    assert!(fa.onsets.is_empty(), "ref_onsets={:?}", fa.onsets);
    assert_eq!(fb.onsets.len(), 1, "cand_onsets={:?}", fb.onsets);
    assert!(
        (0.002..=0.008).contains(&score.spec_l1),
        "spec_l1={}",
        score.spec_l1
    );
    assert!(
        (0.03..=0.09).contains(&score.mel_l1),
        "mel_l1={}",
        score.mel_l1
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
    let score = compare(&fa, &fc)?;
    eprintln!(
        "GOLDEN +6dB: spec_l1={:.3e} mel_l1={:.3e} f1={:.6} delta={:.3} lufs_i={:.3} dbtp={:.3}",
        score.spec_l1,
        score.mel_l1,
        score.transient_f1,
        score.delta_lufs,
        score.lufs_i,
        score.true_peak
    );
    // Pinned 2026-10-06: normalised distances measured exactly 0.0 on the
    // first run (suspected cause: x2 amplitude scaling propagates exactly
    // through the linear loudness filter chain, landing both sides on
    // identical normalised samples — mechanism not formally proven, hence
    // the intervals below keep slack for toolchain float drift instead of
    // asserting exact zeros; the raw-vs-normalised ratio is the
    // load-bearing assert).
    assert!(score.spec_l1 <= 1e-6, "spec_l1={}", score.spec_l1);
    assert!(score.mel_l1 <= 1e-4, "mel_l1={}", score.mel_l1);
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
fn golden_rejects_incomparable() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let fa = analyze(&signal_a(), &params)?;
    let other = MirParams {
        n_mel: 40,
        ..MirParams::v1()
    };
    let fb = analyze(&signal_a(), &other)?;
    assert!(matches!(compare(&fa, &fb), Err(EvalError::ParamMismatch)));
    Ok(())
}
