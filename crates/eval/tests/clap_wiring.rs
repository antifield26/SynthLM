//! TSK-601 acceptance: `clap_cos` is a real value, `+6dB` never scores
//! higher, and CLAP-distance dedup mirrors the planner candidate behavior
//! (homogeneous fixtures collapse to the top score, heterogeneous fixtures
//! all survive).
//!
//! Pure Rust + `synthlm-eval` only. No REAPER, no network, no weight files:
//! every embedding comes from [`synthlm_eval::SpectralClapEmbedder`].

use synthlm_eval::{
    ClapEmbedder, DEFAULT_CLAP_DEDUP_DISTANCE, MirParams, SpectralClapEmbedder, clap_distance,
    compare, dedup_by_clap_default, score_pair,
};

const SR: f32 = 48_000.0;
const N: usize = 96_000;

/// Sum of sine partials with f64 phase accumulation (same rationale as the
/// golden harness: f32 angles would amplitude-modulate the tone and fake
/// spectral flux).
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

fn signal_a() -> Vec<f32> {
    sine_mix(&[(445.3125, 0.5), (890.625, 0.25), (1335.9375, 0.125)])
}

fn signal_b_timbre() -> Vec<f32> {
    sine_mix(&[(445.3125, 0.5), (656.25, 0.3), (984.375, 0.2)])
}

#[test]
fn clap_cos_is_a_real_value_not_none() -> Result<(), synthlm_eval::EvalError> {
    let params = MirParams::v1();
    let backend = SpectralClapEmbedder;
    let a = signal_a();
    let b = signal_b_timbre();
    let identical = score_pair(&backend, &a, &a.clone(), &params)?;
    let timbre = score_pair(&backend, &a, &b, &params)?;
    eprintln!(
        "CLAP wiring: identical_cos={:?} timbre_cos={:?} radius={DEFAULT_CLAP_DEDUP_DISTANCE}",
        identical.clap_cos, timbre.clap_cos
    );
    let same = identical.clap_cos.expect("identical pair must embed");
    assert_eq!(same, 1.0);
    let cos = timbre.clap_cos.expect("timbre pair must embed");
    assert!(cos.is_finite() && cos < 1.0, "timbre cos={cos}");
    // The embedder-free path is unchanged (still `None`: it never sees audio).
    let plain = compare(
        &synthlm_eval::mir::analyze(&a, &params)?,
        &synthlm_eval::mir::analyze(&a.clone(), &params)?,
    )?;
    assert_eq!(plain.clap_cos, None);
    Ok(())
}

#[test]
fn clap_plus_6db_must_not_score_higher() -> Result<(), synthlm_eval::EvalError> {
    let params = MirParams::v1();
    let backend = SpectralClapEmbedder;
    let a = signal_a();
    let loud: Vec<f32> = a.iter().map(|s| s * 2.0).collect();
    let base = score_pair(&backend, &a, &a.clone(), &params)?;
    let scored = score_pair(&backend, &a, &loud, &params)?;
    let base_cos = base.clap_cos.expect("base must embed");
    let loud_cos = scored.clap_cos.expect("loud must embed");
    eprintln!(
        "CLAP +6dB: base_cos={base_cos} loud_cos={loud_cos} delta={}",
        scored.delta_lufs
    );
    assert!((f64::from(base_cos) - 1.0).abs() < 1e-9);
    assert!(
        (f64::from(loud_cos) - 1.0).abs() < 1e-6,
        "gain must vanish in CLAP space: {loud_cos}"
    );
    assert!(
        f64::from(loud_cos) <= f64::from(base_cos) + 1e-6,
        "loud must not beat identical: {loud_cos} vs {base_cos}"
    );
    // Spectral gate mirrors the golden anti-cheat assertion.
    assert!(scored.spec_l1 >= base.spec_l1);
    assert!(scored.mel_weighted >= base.mel_weighted);
    assert!(scored.transient_f1 <= base.transient_f1);
    Ok(())
}

#[test]
fn clap_dedup_matches_candidate_behavior() -> Result<(), synthlm_eval::EvalError> {
    let params = MirParams::v1();
    let backend = SpectralClapEmbedder;
    // Homogeneous: same bed plus per-sample drift around 1e-3 (planner
    // `homogeneous_fixtures_dedup_to_one` analog, top score last in order).
    let base = signal_a();
    let drifted = [0.0, 1.0, -1.0].map(|seed| {
        base.iter()
            .enumerate()
            .map(|(i, s)| s + seed * 1e-3 * ((i % 7) as f32 - 3.0))
            .collect::<Vec<f32>>()
    });
    let mut homo_embeddings = Vec::new();
    for variant in &drifted {
        homo_embeddings.push(backend.embed(variant, &params)?);
    }
    let homo_dist_01 = clap_distance(&homo_embeddings[0], &homo_embeddings[1])?;
    let survivors = dedup_by_clap_default(&[0.70, 0.90, 0.80], &homo_embeddings)?;
    eprintln!("CLAP dedup homogeneous: dist01={homo_dist_01:.3e} survivors={survivors:?}");
    assert!(homo_dist_01 < DEFAULT_CLAP_DEDUP_DISTANCE);
    assert_eq!(survivors, vec![1]);
    // Heterogeneous: clearly separated timbres (planner
    // `distant_candidates_all_survive` analog).
    let beds = [
        sine_mix(&[(445.3125, 0.5)]),
        sine_mix(&[(890.625, 0.5)]),
        sine_mix(&[(1335.9375, 0.5)]),
    ];
    let mut hetero_embeddings = Vec::new();
    for bed in &beds {
        hetero_embeddings.push(backend.embed(bed, &params)?);
    }
    let hetero_min = clap_distance(&hetero_embeddings[0], &hetero_embeddings[1])?
        .min(clap_distance(&hetero_embeddings[0], &hetero_embeddings[2])?)
        .min(clap_distance(&hetero_embeddings[1], &hetero_embeddings[2])?);
    let survivors = dedup_by_clap_default(&[0.9, 0.8, 0.7], &hetero_embeddings)?;
    eprintln!("CLAP dedup heterogeneous: min_dist={hetero_min:.6} survivors={survivors:?}");
    assert!(hetero_min > DEFAULT_CLAP_DEDUP_DISTANCE);
    assert_eq!(survivors, vec![0, 1, 2]);
    Ok(())
}
