//! TSK-705: second blind round (11 new stimuli) + recalibration.
//!
//! Extends the TSK-402 round (10 clips: B brightness, T transients, C cheat)
//! to 21 total (≥2×). New series reuse the same 2 s arpeggio, synth,
//! filters, and −14 LUFS discipline (see `blind_calib.rs` for the shared
//! recipe; small helpers are duplicated here so each harness stays a
//! hermetic single file, the `bench_10k`/`bench_10k_real` precedent):
//!
//! - B2 brightness (4): same arpeggio through RBJ lowpass at
//!   1000/2500/6000/14000 Hz (fresh cutoffs, no overlap with round 1).
//!   Ground truth dark→bright is cutoff-ascending; model predicts
//!   centroid-ascending.
//! - W richness (4): harmonic counts 4/8/16/32 at fixed 10 ms attack.
//!   Ground truth dull→rich is count-ascending; model predicts
//!   centroid-ascending (asserted in-test; counts were chosen so the
//!   ordering is strict).
//! - T2 transients (3): per-note attacks 10/40/120 ms. Ground truth
//!   dull→sharp is attack-descending; model predicts peak-flux-descending.
//!
//! Isolation mirrors round 1: `%TEMP%/synthlm-blind2/` holds ONLY
//! `clip11..clip21.wav`; the key (`%TEMP%/synthlm-blind2-key.json`,
//! permutation + ground truths) lives OUTSIDE it and is reported to the
//! main session only. The rater ranks within each series; Spearman(human,
//! model) per series plus pooled decides the DEC-016 weight verdict.

use std::collections::BTreeMap;
use std::path::PathBuf;

use synthlm_eval::mir::{analyze, mean_spectral_centroid, normalize_to_target};
use synthlm_eval::{EvalError, MirParams};

/// Blind permutation seed for round 2 (fixed, recorded in the key).
pub const BLIND2_SEED: u64 = 705_021_107;

/// Sample rate under test (matches [`MirParams::v1`]).
const SR: u32 = 48_000;

/// 2 s of mono audio at 48 kHz.
const N2S: usize = 96_000;

/// One arpeggio note length in samples (2 s / 3).
const NOTE_LEN: usize = 32_000;

/// Base per-note attack in ms (B2 + W series).
const BASE_ATTACK_MS: f32 = 10.0;

/// Per-note release in ms (all stimuli, click suppression).
const RELEASE_MS: f32 = 12.0;

/// Fundamental amplitude scale (peak-safe after −14 LUFS normalisation).
const FUND_AMP: f32 = 0.35;

/// B2-series lowpass cutoffs in Hz (ground truth dark→bright ascending).
const B2_CUTOFFS: [f32; 4] = [1_000.0, 2_500.0, 6_000.0, 14_000.0];

/// B2-series stimulus ids in ground-truth dark→bright order.
const B2_IDS: [&str; 4] = ["B1000", "B2500", "B6000", "B14000"];

/// W-series harmonic counts (ground truth dull→rich ascending).
const W_HARMS: [u32; 4] = [4, 8, 16, 32];

/// W-series stimulus ids (index-aligned with [`W_HARMS`]).
const W_IDS: [&str; 4] = ["W04", "W08", "W16", "W32"];

/// T2-series per-note attacks in ms.
const T2_ATTACKS: [f32; 3] = [10.0, 40.0, 120.0];

/// T2-series stimulus ids (index-aligned with [`T2_ATTACKS`]).
const T2_IDS: [&str; 3] = ["T010", "T040", "T120"];

/// T2-series ground truth dull→sharp (attack-descending).
const T2_DULL_TO_SHARP: [&str; 3] = ["T120", "T040", "T010"];

/// Arpeggio fundamentals in Hz (A4, C#5, E5, equal temperament).
const ARPEGGIO: [f64; 3] = [440.0, 554.365_261_172_99, 659.255_113_825_09];

/// Biquad `Q` (Butterworth, `1/√2`).
const BIQUAD_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Deterministic Fisher–Yates permutation of `0..n` (xorshift64*).
fn permute(n: usize, seed: u64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..n).collect();
    let mut s = if seed == 0 { 1 } else { seed };
    for i in (1..n).rev() {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let j = (s % ((i + 1) as u64)) as usize;
        idx.swap(i, j);
    }
    idx
}

/// Synthesise the 2 s arpeggio with a per-note linear `attack_ms` and
/// `n_harm` harmonics per note (`0.35/k`).
///
/// Deterministic f64 phases restarted per note; envelope is linear attack
/// then flat, with a 12 ms linear release at each note end. No RNG.
fn synth_arpeggio(attack_ms: f32, n_harm: u32) -> Vec<f32> {
    let attack = ((attack_ms / 1000.0 * SR as f32).round() as usize).max(1);
    let release = ((RELEASE_MS / 1000.0 * SR as f32).round() as usize).max(1);
    let mut out = vec![0.0_f32; N2S];
    for (note, &freq) in ARPEGGIO.iter().enumerate() {
        let base = note * NOTE_LEN;
        for pos in 0..NOTE_LEN {
            let env = if pos < attack {
                pos as f32 / attack as f32
            } else if pos >= NOTE_LEN - release {
                (NOTE_LEN - pos) as f32 / release as f32
            } else {
                1.0
            };
            let t = pos as f64 / f64::from(SR);
            let mut v = 0.0_f64;
            for k in 1..=n_harm {
                let f = freq * f64::from(k);
                let amp = f64::from(FUND_AMP) / f64::from(k);
                v += amp * (2.0 * std::f64::consts::PI * f * t).sin();
            }
            out[base + pos] += (v as f32) * env;
        }
    }
    out
}

/// One RBJ lowpass biquad pass (`Q = 1/√2`, direct form I, zero state).
fn lowpass_biquad(samples: &[f32], cutoff_hz: f32) -> Vec<f32> {
    let w0 = 2.0 * std::f64::consts::PI * f64::from(cutoff_hz) / f64::from(SR);
    let cosw = w0.cos();
    let sinw = w0.sin();
    let alpha = sinw / (2.0 * BIQUAD_Q);
    let b0 = (1.0 - cosw) / 2.0;
    let b1 = 1.0 - cosw;
    let b2 = (1.0 - cosw) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cosw;
    let a2 = 1.0 - alpha;
    let (b0, b1, b2, a1, a2) = (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0);
    let mut out = Vec::with_capacity(samples.len());
    let (mut x1, mut x2) = (0.0_f64, 0.0_f64);
    let (mut y1, mut y2) = (0.0_f64, 0.0_f64);
    for &s in samples {
        let x0 = f64::from(s);
        let y0 = b0 * x0 + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        out.push(y0 as f32);
        x2 = x1;
        x1 = x0;
        y2 = y1;
        y1 = y0;
    }
    out
}

/// Encode mono f32 (`-1.0..=1.0`) as 48 kHz 16-bit PCM WAV bytes.
fn wav_bytes_i16(samples: &[f32]) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let mut b = Vec::with_capacity(44 + data_len);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&SR.to_le_bytes());
    b.extend_from_slice(&(SR * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

/// Decode the [`wav_bytes_i16`] format back to mono f32 in `-1.0..=1.0`.
fn parse_wav_i16(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() < 44
        || &bytes[0..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
        || &bytes[12..16] != b"fmt "
    {
        return Err("not a 16-bit PCM WAV".to_string());
    }
    let data_len = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as usize;
    if bytes.len() < 44 + data_len {
        return Err("WAV data truncated".to_string());
    }
    let mut out = Vec::with_capacity(data_len / 2);
    let (chunks, _) = bytes[44..44 + data_len].as_chunks::<2>();
    for chunk in chunks {
        let v = i16::from_le_bytes(*chunk);
        out.push(f32::from(v) / 32767.0);
    }
    Ok(out)
}

/// Blind working directories for round 2: `(blind_dir, key_path)`.
///
/// `blind_dir` (`%TEMP%/synthlm-blind2/`) holds ONLY `clip11..clip21.wav`;
/// `key_path` (`%TEMP%/synthlm-blind2-key.json`) lives OUTSIDE it.
fn blind_paths() -> Result<(PathBuf, PathBuf), String> {
    let tmp = std::env::temp_dir();
    Ok((
        tmp.join("synthlm-blind2"),
        tmp.join("synthlm-blind2-key.json"),
    ))
}

/// Build the eleven stimuli in canonical (unblinded) order.
///
/// Returns `(ids, sample_vectors)` with ids
/// `[B1000,B2500,B6000,B14000,W04,W08,W16,W32,T010,T040,T120]`.
fn build_stimuli(params: &MirParams) -> Result<(Vec<String>, Vec<Vec<f32>>), EvalError> {
    let mut ids = Vec::with_capacity(11);
    let mut vecs = Vec::with_capacity(11);
    let base_raw = synth_arpeggio(BASE_ATTACK_MS, 16);
    let base = normalize_to_target(&base_raw, params)?;
    for (id, &cut) in B2_IDS.iter().zip(B2_CUTOFFS.iter()) {
        let filtered = lowpass_biquad(&base, cut);
        let normed = normalize_to_target(&filtered, params)?;
        ids.push(id.to_string());
        vecs.push(normed);
    }
    for (id, &nh) in W_IDS.iter().zip(W_HARMS.iter()) {
        let raw = synth_arpeggio(BASE_ATTACK_MS, nh);
        let normed = normalize_to_target(&raw, params)?;
        ids.push(id.to_string());
        vecs.push(normed);
    }
    for (id, &atk) in T2_IDS.iter().zip(T2_ATTACKS.iter()) {
        let raw = synth_arpeggio(atk, 16);
        let normed = normalize_to_target(&raw, params)?;
        ids.push(id.to_string());
        vecs.push(normed);
    }
    Ok((ids, vecs))
}

/// Peak spectral flux of a feature snapshot (sharpness rank signal).
fn peak_flux(flux: &[f32]) -> f32 {
    flux.iter().fold(0.0_f32, |m, &v| m.max(v))
}

#[test]
fn blind2_synth_is_reproducible() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let (ids_a, vecs_a) = build_stimuli(&params)?;
    let (ids_b, vecs_b) = build_stimuli(&params)?;
    assert_eq!(ids_a, ids_b);
    assert_eq!(vecs_a.len(), 11);
    for (a, b) in vecs_a.iter().zip(vecs_b.iter()) {
        assert_eq!(a, b);
        assert_eq!(wav_bytes_i16(a), wav_bytes_i16(b));
    }
    Ok(())
}

#[test]
fn blind2_generate_and_predict() -> Result<(), Box<dyn std::error::Error>> {
    let params = MirParams::v1();
    let (ids, vecs) = build_stimuli(&params)?;
    assert_eq!(ids.len(), 11);

    let mut centroid = BTreeMap::<String, f32>::new();
    let mut peak = BTreeMap::<String, f32>::new();
    let mut lufs_norm = BTreeMap::<String, f64>::new();
    let mut peak_abs = BTreeMap::<String, f32>::new();
    for (id, v) in ids.iter().zip(vecs.iter()) {
        let f = analyze(v, &params)?;
        centroid.insert(
            id.clone(),
            mean_spectral_centroid(&f.magnitude, params.sample_rate, params.window),
        );
        peak.insert(id.clone(), peak_flux(&f.flux));
        lufs_norm.insert(id.clone(), f.normalized_lufs);
        let mut m = 0.0_f32;
        for s in v.iter() {
            m = m.max(s.abs());
        }
        peak_abs.insert(id.clone(), m);
    }

    eprintln!("BLIND2 centroids (Hz) + peak flux:");
    for id in &ids {
        eprintln!("  {id} centroid={:.2} peak={:.6}", centroid[id], peak[id]);
    }

    // Disk-domain gates: every stimulus at −14 LUFS ±0.3, sample-safe.
    for id in &ids {
        assert!(
            (lufs_norm[id] - params.target_lufs).abs() < 0.3,
            "{id} norm_lufs={}",
            lufs_norm[id]
        );
        assert!(peak_abs[id] < 1.0, "{id} peak={}", peak_abs[id]);
    }
    // B2: centroid-ascending must equal cutoff-ascending (dark→bright).
    let mut b2_by_centroid = B2_IDS.to_vec();
    b2_by_centroid.sort_by(|a, b| {
        centroid[*a]
            .partial_cmp(&centroid[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(
        b2_by_centroid,
        B2_IDS.to_vec(),
        "B2 brightness order must follow cutoffs"
    );
    // W: centroid-ascending must equal harm-ascending (dull→rich).
    let mut w_by_centroid = W_IDS.to_vec();
    w_by_centroid.sort_by(|a, b| {
        centroid[*a]
            .partial_cmp(&centroid[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(
        w_by_centroid,
        W_IDS.to_vec(),
        "W richness order must follow harmonic counts, {centroid:?}"
    );
    // T2: peak-flux-descending must equal attack-ascending (sharp first).
    let mut t2_by_peak = T2_IDS.to_vec();
    t2_by_peak.sort_by(|a, b| {
        peak[*b]
            .partial_cmp(&peak[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(
        t2_by_peak,
        ["T010", "T040", "T120"],
        "T2 sharpness order must follow attacks 10>40>120"
    );
    let dull_first: Vec<&str> = t2_by_peak.iter().rev().copied().collect();
    assert_eq!(dull_first, T2_DULL_TO_SHARP.to_vec());

    // Model prediction rows (what the human ranking is scored against).
    eprintln!("BLIND2 model predictions:");
    eprintln!("  B2 dark→bright: {b2_by_centroid:?}");
    eprintln!("  W dull→rich: {w_by_centroid:?}");
    eprintln!("  T2 sharp-first: {t2_by_peak:?}");

    // Blind: permute and write ONLY the eleven wavs into the blind dir.
    let order = permute(ids.len(), BLIND2_SEED);
    let (blind_dir, key_path) = blind_paths().map_err(EvalError::InvalidSamples)?;
    std::fs::create_dir_all(&blind_dir)
        .map_err(|e: std::io::Error| EvalError::InvalidSamples(e.to_string()))?;
    if let Ok(entries) = std::fs::read_dir(&blind_dir) {
        for e in entries.flatten() {
            let p = e.path();
            let keep = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("clip") && n.ends_with(".wav") && n.len() == "clip00.wav".len()
            });
            if !keep {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let mut mapping = BTreeMap::<String, String>::new();
    for (clip_idx, &stim_idx) in order.iter().enumerate() {
        let name = format!("clip{:02}.wav", clip_idx + 11);
        let bytes = wav_bytes_i16(&vecs[stim_idx]);
        std::fs::write(blind_dir.join(&name), &bytes)
            .map_err(|e| EvalError::InvalidSamples(e.to_string()))?;
        mapping.insert(name, ids[stim_idx].clone());
    }

    // Round-trip: read back and re-score (file truth, quantisation-tolerant).
    let mut rt_centroid = BTreeMap::<String, f32>::new();
    let mut rt_peak = BTreeMap::<String, f32>::new();
    for (clip, stim) in mapping.iter() {
        let bytes = std::fs::read(blind_dir.join(clip))
            .map_err(|e| EvalError::InvalidSamples(e.to_string()))?;
        let samples = parse_wav_i16(&bytes).map_err(EvalError::InvalidSamples)?;
        assert_eq!(samples.len(), N2S, "{clip} length");
        let f = analyze(&samples, &params)?;
        rt_centroid.insert(
            stim.clone(),
            mean_spectral_centroid(&f.magnitude, params.sample_rate, params.window),
        );
        rt_peak.insert(stim.clone(), peak_flux(&f.flux));
    }
    let mut b2_rt = B2_IDS.to_vec();
    b2_rt.sort_by(|a, b| {
        rt_centroid[*a]
            .partial_cmp(&rt_centroid[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(b2_rt, B2_IDS.to_vec(), "round-trip B2 order");
    let mut w_rt = W_IDS.to_vec();
    w_rt.sort_by(|a, b| {
        rt_centroid[*a]
            .partial_cmp(&rt_centroid[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(w_rt, W_IDS.to_vec(), "round-trip W order");
    let mut t2_rt = T2_IDS.to_vec();
    t2_rt.sort_by(|a, b| {
        rt_peak[*b]
            .partial_cmp(&rt_peak[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(t2_rt, ["T010", "T040", "T120"], "round-trip T2 order");

    // Key (answer) OUTSIDE the blind dir; content also goes to the return.
    let mut map_json = String::from("{");
    for (i, (clip, stim)) in mapping.iter().enumerate() {
        if i > 0 {
            map_json.push(',');
        }
        map_json.push_str(&format!("\"{clip}\":\"{stim}\""));
    }
    map_json.push('}');
    let key = format!(
        "{{\"seed\":{BLIND2_SEED},\"sample_rate\":{SR},\"target_lufs\":-14.0,\"order\":[{0}],\"mapping\":{map_json},\"B2_dark_to_bright\":[\"B1000\",\"B2500\",\"B6000\",\"B14000\"],\"W_dull_to_rich\":[\"W04\",\"W08\",\"W16\",\"W32\"],\"T2_dull_to_sharp\":[\"T120\",\"T040\",\"T010\"]}}",
        order
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    std::fs::write(&key_path, key.as_bytes())
        .map_err(|e| EvalError::InvalidSamples(e.to_string()))?;
    eprintln!(
        "BLIND2 wrote {} clips -> {}",
        mapping.len(),
        blind_dir.display()
    );
    eprintln!("BLIND2 key (main session only) -> {}", key_path.display());
    eprintln!("BLIND2 mapping {mapping:?}");
    Ok(())
}
