//! TSK-402 first half: blind stimulus preparation + own prediction ordering.
//!
//! Scope (subagent, M): pure-Rust synthesis + `synthlm-eval` scoring only.
//! No REAPER, no network. Reuses [`synthlm_eval::mir::stft_magnitude`],
//! [`synthlm_eval::mir::mel_filterbank`] (via
//! [`synthlm_eval::mir::analyze`]), [`synthlm_eval::mir::spectral_centroid`]
//! / [`synthlm_eval::mir::mean_spectral_centroid`], and
//! [`synthlm_eval::mir::spectral_flux`] (via [`synthlm_eval::mir::analyze`]
//! + [`synthlm_eval::compare`]); no DSP is reimplemented here.
//!
//! Isolation: the blind directory (`%TEMP%/synthlm-blind/`) holds ONLY
//! `clip01..clip10.wav` (48 kHz mono 16-bit). The answer file (`key.json`,
//! permutation table + series membership) is written OUTSIDE that directory
//! (`%TEMP%/synthlm-blind-key.json`) and its content is reported to the main
//! session only — never placed where raters can see it.
//!
//! Stimuli (10):
//!
//! - Base phrase: 2 s three-note arpeggio (A4 440 Hz, C#5 554.365261 Hz,
//!   E5 659.255113 Hz, each 1/3 of 2 s), 16 harmonics at `0.35/k`, per-note
//!   linear attack + 12 ms release, deterministic f64 phases, gained to
//!   −14 LUFS via [`synthlm_eval::mir::normalize_to_target`].
//! - B series (brightness, 5): base (10 ms attack) through one RBJ biquad
//!   lowpass (`Q = 1/√2`) at 800/1500/3000/8000/20000 Hz, then re-gained to
//!   −14 LUFS. Ground truth dark→bright is cutoff-ascending.
//! - T series (transients, 3): same arpeggio synthesised with per-note
//!   linear attacks 5/20/80 ms, each gained to −14 LUFS. Ground truth
//!   dull→sharp is attack-descending (80/20/5).
//! - C pair (cheat, 2): base vs base × 2.0 (`+6.02 dB`, kept hot on disk at
//!   ≈ −8 LUFS); the eval pipeline normalises before scoring, so the hot
//!   copy must not score better.
//!
//! Blind permutation: Fisher–Yates with xorshift64* seeded by
//! [`BLIND_SEED`]; the seed is fixed and recorded in `key.json`.
//!
//! Own prediction: per clip, [`synthlm_eval::mir::analyze`] +
//! [`synthlm_eval::mir::mean_spectral_centroid`] (brightness) and peak/mean
//! flux from [`synthlm_eval::mir::MirFeatures::flux`] (sharpness), plus
//! [`synthlm_eval::compare`] `mel_weighted` distance + `transient_f1`
//! against the base for the audit table. B order is centroid-ascending,
//! T order is peak-flux-descending.

use std::collections::BTreeMap;
use std::path::PathBuf;

use synthlm_eval::mir::{analyze, mean_spectral_centroid, normalize_to_target};
use synthlm_eval::{EvalError, MirParams, compare};

/// Blind permutation seed (fixed, recorded in `key.json`).
pub const BLIND_SEED: u64 = 40_220_261_006;

/// Sample rate under test (matches [`MirParams::v1`]).
const SR: u32 = 48_000;

/// 2 s of mono audio at 48 kHz.
const N2S: usize = 96_000;

/// One arpeggio note length in samples (2 s / 3).
const NOTE_LEN: usize = 32_000;

/// Base per-note attack in ms (B series + C pair).
const BASE_ATTACK_MS: f32 = 10.0;

/// Per-note release in ms (all stimuli, click suppression).
const RELEASE_MS: f32 = 12.0;

/// Harmonic count per note (1..=16, amplitude `0.35/k`).
const N_HARM: u32 = 16;

/// Fundamental amplitude scale (peak-safe after −14 LUFS normalisation).
const FUND_AMP: f32 = 0.35;

/// B-series lowpass cutoffs in Hz (ground truth dark→bright ascending).
const B_CUTOFFS: [f32; 5] = [800.0, 1_500.0, 3_000.0, 8_000.0, 20_000.0];

/// B-series stimulus ids in ground-truth dark→bright order.
const B_IDS: [&str; 5] = ["B800", "B1500", "B3000", "B8000", "B20000"];

/// T-series per-note attacks in ms.
const T_ATTACKS: [f32; 3] = [5.0, 20.0, 80.0];

/// T-series stimulus ids (index-aligned with [`T_ATTACKS`]).
const T_IDS: [&str; 3] = ["T05", "T20", "T80"];

/// T-series ground truth dull→sharp (attack-descending).
const T_DULL_TO_SHARP: [&str; 3] = ["T80", "T20", "T05"];

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

/// Synthesise the 2 s arpeggio with a per-note linear `attack_ms`.
///
/// Deterministic f64 phases restarted per note (clear onsets); envelope is
/// linear attack then flat, with a [`RELEASE_MS`] linear release at each
/// note end. No RNG is involved.
fn synth_arpeggio(attack_ms: f32) -> Vec<f32> {
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
            for k in 1..=N_HARM {
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
///
/// Hand-written header (no new dependency, permissive-only); clamps and
/// rounds deterministically so identical samples give identical bytes.
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

/// Blind working directories: `(blind_dir, key_path)`.
///
/// `blind_dir` (`%TEMP%/synthlm-blind/`) holds ONLY the ten blind wavs;
/// `key_path` (`%TEMP%/synthlm-blind-key.json`) lives OUTSIDE it.
fn blind_paths() -> Result<(PathBuf, PathBuf), String> {
    let tmp = std::env::temp_dir();
    Ok((
        tmp.join("synthlm-blind"),
        tmp.join("synthlm-blind-key.json"),
    ))
}

/// Build the ten stimuli in canonical (unblinded) order.
///
/// Returns `(ids, sample_vectors)` with ids
/// `[B800,B1500,B3000,B8000,B20000,T05,T20,T80,C_base,C_plus6]`.
fn build_stimuli(params: &MirParams) -> Result<(Vec<String>, Vec<Vec<f32>>), EvalError> {
    let base_raw = synth_arpeggio(BASE_ATTACK_MS);
    let base = normalize_to_target(&base_raw, params)?;
    let mut ids = Vec::with_capacity(10);
    let mut vecs = Vec::with_capacity(10);
    for (id, &cut) in B_IDS.iter().zip(B_CUTOFFS.iter()) {
        let filtered = lowpass_biquad(&base, cut);
        let normed = normalize_to_target(&filtered, params)?;
        ids.push(id.to_string());
        vecs.push(normed);
    }
    for (id, &atk) in T_IDS.iter().zip(T_ATTACKS.iter()) {
        let raw = synth_arpeggio(atk);
        let normed = normalize_to_target(&raw, params)?;
        ids.push(id.to_string());
        vecs.push(normed);
    }
    ids.push("C_base".to_string());
    vecs.push(base.clone());
    let loud: Vec<f32> = base.iter().map(|s| s * 2.0).collect();
    ids.push("C_plus6".to_string());
    vecs.push(loud);
    Ok((ids, vecs))
}

/// Peak spectral flux of a feature snapshot (sharpness rank signal).
fn peak_flux(flux: &[f32]) -> f32 {
    flux.iter().fold(0.0_f32, |m, &v| m.max(v))
}

/// Mean spectral flux of a feature snapshot.
fn mean_flux(flux: &[f32]) -> f32 {
    if flux.is_empty() {
        return 0.0;
    }
    let mut acc = 0.0_f64;
    for &v in flux {
        acc += f64::from(v);
    }
    (acc / flux.len() as f64) as f32
}

#[test]
fn blind_synth_is_reproducible() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let (ids_a, vecs_a) = build_stimuli(&params)?;
    let (ids_b, vecs_b) = build_stimuli(&params)?;
    assert_eq!(ids_a, ids_b);
    assert_eq!(vecs_a.len(), 10);
    for (a, b) in vecs_a.iter().zip(vecs_b.iter()) {
        assert_eq!(a, b);
        assert_eq!(wav_bytes_i16(a), wav_bytes_i16(b));
    }
    // Spot-check: base bytes are stable across two encodings.
    assert_eq!(wav_bytes_i16(&vecs_a[8]), wav_bytes_i16(&vecs_b[8]));
    Ok(())
}

#[test]
fn blind_plus6db_not_better_after_normalisation() -> Result<(), EvalError> {
    let params = MirParams::v1();
    let (_, vecs) = build_stimuli(&params)?;
    let base = analyze(&vecs[8], &params)?;
    let loud = analyze(&vecs[9], &params)?;
    let same = compare(&base, &base.clone())?;
    let score = compare(&base, &loud)?;
    assert!(
        score.spec_l1 >= same.spec_l1,
        "spec_l1 improved: {} < {}",
        score.spec_l1,
        same.spec_l1
    );
    assert!(
        score.mel_l1 >= same.mel_l1,
        "mel_l1 improved: {} < {}",
        score.mel_l1,
        same.mel_l1
    );
    assert!(
        score.mel_weighted >= same.mel_weighted,
        "weighted improved: {} < {}",
        score.mel_weighted,
        same.mel_weighted
    );
    assert!(
        score.transient_f1 <= same.transient_f1,
        "f1 improved: {} > {}",
        score.transient_f1,
        same.transient_f1
    );
    Ok(())
}

#[test]
fn blind_generate_and_predict() -> Result<(), Box<dyn std::error::Error>> {
    let params = MirParams::v1();
    let (ids, vecs) = build_stimuli(&params)?;
    assert_eq!(ids.len(), 10);

    // Score every stimulus from the in-memory vectors (pre-write audit).
    let mut centroid = BTreeMap::<String, f32>::new();
    let mut peak = BTreeMap::<String, f32>::new();
    let mut mean = BTreeMap::<String, f32>::new();
    let mut weighted_vs_base = BTreeMap::<String, f32>::new();
    let mut f1_vs_base = BTreeMap::<String, f32>::new();
    let mut lufs_in = BTreeMap::<String, f64>::new();
    let mut lufs_norm = BTreeMap::<String, f64>::new();
    let mut dbtp = BTreeMap::<String, f64>::new();
    let mut peak_abs = BTreeMap::<String, f32>::new();
    let base_feat = analyze(&vecs[8], &params)?;
    for (id, v) in ids.iter().zip(vecs.iter()) {
        let f = analyze(v, &params)?;
        centroid.insert(
            id.clone(),
            mean_spectral_centroid(&f.magnitude, params.sample_rate, params.window),
        );
        peak.insert(id.clone(), peak_flux(&f.flux));
        mean.insert(id.clone(), mean_flux(&f.flux));
        let s = compare(&base_feat, &f)?;
        weighted_vs_base.insert(id.clone(), s.mel_weighted);
        f1_vs_base.insert(id.clone(), s.transient_f1);
        lufs_in.insert(id.clone(), f.integrated_lufs);
        lufs_norm.insert(id.clone(), f.normalized_lufs);
        dbtp.insert(id.clone(), f.normalized_dbtp);
        let mut m = 0.0_f32;
        for s in v.iter() {
            m = m.max(s.abs());
        }
        peak_abs.insert(id.clone(), m);
    }

    eprintln!("BLIND centroids (Hz):");
    for id in &ids {
        eprintln!("  {id} centroid={:.2}", centroid[id]);
    }
    eprintln!("BLIND peak/mean flux:");
    for id in &ids {
        eprintln!("  {id} peak={:.6} mean={:.6}", peak[id], mean[id]);
    }
    eprintln!("BLIND loudness audit (disk-domain):");
    for id in &ids {
        eprintln!(
            "  {id} in_lufs={:.3} norm_lufs={:.3} dbtp={:.3} peak={:.4}",
            lufs_in[id], lufs_norm[id], dbtp[id], peak_abs[id]
        );
    }
    eprintln!("BLIND mel_weighted + f1 vs C_base:");
    for id in &ids {
        eprintln!(
            "  {id} w={:.6} f1={:.3}",
            weighted_vs_base[id], f1_vs_base[id]
        );
    }

    // Own predictions (asserted so the test gates the ordering).
    // Disk-domain loudness gates: every normalised stimulus (all but the
    // hot cheat copy) must sit at −14 LUFS ±0.3 and stay sample-safe; the
    // hot copy must read ≈ +6 dB above base without hard-clipping the
    // 16-bit writer.
    for id in [
        "B800", "B1500", "B3000", "B8000", "B20000", "T05", "T20", "T80", "C_base",
    ] {
        assert!(
            (lufs_norm[id] - params.target_lufs).abs() < 0.3,
            "{id} norm_lufs={}",
            lufs_norm[id]
        );
        assert!(peak_abs[id] < 1.0, "{id} peak={}", peak_abs[id]);
    }
    assert!(
        (lufs_in["C_plus6"] - lufs_in["C_base"] - 6.0206).abs() < 0.15,
        "C_plus6 vs C_base delta: {} vs {}",
        lufs_in["C_plus6"],
        lufs_in["C_base"]
    );
    assert!(
        peak_abs["C_plus6"] < 1.0,
        "C_plus6 peak={}",
        peak_abs["C_plus6"]
    );
    // B: centroid-ascending must equal cutoff-ascending (dark→bright).
    let mut b_by_centroid = B_IDS.to_vec();
    b_by_centroid.sort_by(|a, b| {
        centroid[*a]
            .partial_cmp(&centroid[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(
        b_by_centroid,
        B_IDS.to_vec(),
        "B brightness order must follow cutoffs"
    );
    // T: peak-flux-descending must equal attack-ascending (sharp→dull
    // reversed: dull→sharp is 80/20/5, sharpest has the largest peak).
    let mut t_by_peak = T_IDS.to_vec();
    t_by_peak.sort_by(|a, b| {
        peak[*b]
            .partial_cmp(&peak[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(
        t_by_peak,
        ["T05", "T20", "T80"],
        "T sharpness order must follow attacks 5>20>80"
    );
    // The dull→sharp ground-truth row is the reverse of the sharp-first row.
    let dull_first: Vec<&str> = t_by_peak.iter().rev().copied().collect();
    assert_eq!(dull_first, T_DULL_TO_SHARP.to_vec());

    // Blind: permute and write ONLY the ten wavs into the blind dir.
    let order = permute(ids.len(), BLIND_SEED);
    let (blind_dir, key_path) = blind_paths().map_err(EvalError::InvalidSamples)?;
    std::fs::create_dir_all(&blind_dir)
        .map_err(|e: std::io::Error| EvalError::InvalidSamples(e.to_string()))?;
    // Keep the blind dir rater-clean: remove any stray non-clip files
    // (notably a misplaced key.json) before writing.
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
        let name = format!("clip{:02}.wav", clip_idx + 1);
        let bytes = wav_bytes_i16(&vecs[stim_idx]);
        std::fs::write(blind_dir.join(&name), &bytes)
            .map_err(|e| EvalError::InvalidSamples(e.to_string()))?;
        mapping.insert(name, ids[stim_idx].clone());
    }

    // Round-trip: read the blind files back and re-score (file truth).
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
    // 16-bit round-trip must preserve both orderings (quantisation-tolerant
    // strict monotonicity).
    let mut b_rt = B_IDS.to_vec();
    b_rt.sort_by(|a, b| {
        rt_centroid[*a]
            .partial_cmp(&rt_centroid[*b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(b_rt, B_IDS.to_vec(), "round-trip B order, {rt_centroid:?}");
    let mut t_rt = T_IDS.to_vec();
    t_rt.sort_by(|a, b| {
        rt_peak[*b]
            .partial_cmp(&rt_peak[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    assert_eq!(t_rt, ["T05", "T20", "T80"], "round-trip T order");

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
        "{{\"seed\":{BLIND_SEED},\"sample_rate\":{SR},\"target_lufs\":-14.0,\"order\":[{0}],\"mapping\":{map_json},\"B_dark_to_bright\":[\"B800\",\"B1500\",\"B3000\",\"B8000\",\"B20000\"],\"T_dull_to_sharp\":[\"T80\",\"T20\",\"T05\"],\"C_pair\":[\"C_base\",\"C_plus6\"]}}",
        order
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    std::fs::write(&key_path, key.as_bytes())
        .map_err(|e| EvalError::InvalidSamples(e.to_string()))?;
    eprintln!(
        "BLIND wrote {} clips -> {}",
        mapping.len(),
        blind_dir.display()
    );
    eprintln!("BLIND key (main session only) -> {}", key_path.display());
    eprintln!("BLIND mapping {mapping:?}");
    Ok(())
}
