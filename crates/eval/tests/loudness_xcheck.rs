//! Loudness calibration cross-check + AAC priming rule (TSK-208).
//!
//! 1. Cross-validation: `ebur128` (the production meter inside [`analyze`])
//!    vs `ebur128-stream` 0.2.0 (independent pure-Rust BS.1770-4
//!    implementation, MIT OR Apache-2.0) on a synthetic signal set (sine /
//!    noise / impulse bed / hot peak / −70 LUFS gate vicinity / digital
//!    silence). Gate: integrated-LUFS agreement within ±0.5 LU.
//! 2. TECH 3341 declaration: the official EBU reference vectors are
//!    licence-encumbered and are NOT vendored, downloaded, or reproduced
//!    here. Agreement on this synthetic set substitutes for the vector
//!    suite; anything stronger than "±0.5 LU on synthetic signals" would
//!    need the licensed vectors on a licensed bench (research doc
//!    `docs/research/C-dsp-toolchain.md` §4).
//! 3. AAC priming: a hermetic zero-priming simulation (always runs) plus a
//!    real FFmpeg-AAC → symphonia round trip (runs when the local FFmpeg
//!    binary exists, skips otherwise — same precedent as
//!    `tests/decode_matrix.rs`). The consumer rule under test is
//!    [`strip_codec_padding`] with an [`AAC_PRIMING_SAMPLES`] head trim plus
//!    truncation to the source frame count.

use std::path::{Path, PathBuf};
use std::process::Command;

use ebur128_stream::{AnalyzerBuilder, Channel, Mode};
use synthlm_eval::mir::{AAC_FRAME_SAMPLES, AAC_PRIMING_SAMPLES, analyze, strip_codec_padding};
use synthlm_eval::{EvalError, MirParams, compare};

/// Sample rate under test (matches [`MirParams::v1`]).
const SR: u32 = 48_000;

/// 2 s of mono audio at 48 kHz.
const N2S: usize = 96_000;

/// Local FFmpeg when `FFMPEG_BIN` is unset (same default as the decode
/// matrix; GPL build, internal-run only — here it only synthesises the AAC
/// fixture, the assertions run on symphonia-decoded samples).
const FFMPEG_DEFAULT: &str =
    r"C:\Users\25371\tools\ffmpeg\ffmpeg-9.0.2-essentials_build\bin\ffmpeg.exe";

/// Upper bound on packets per file so corrupt inputs cannot spin the loop.
const MAX_PACKETS: usize = 10_000;

/// Cross-check gate: maximum tolerated integrated-LUFS disagreement (LU).
const XCHECK_TOL_LU: f64 = 0.5;

/// Sine tone with f64 phase accumulation (no argument-reduction wobble).
fn sine(len: usize, freq: f32, amp: f32) -> Vec<f32> {
    let step = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(SR);
    let mut phase = 0.0_f64;
    let mut out = vec![0.0_f32; len];
    for slot in out.iter_mut() {
        *slot = (f64::from(amp) * phase.sin()) as f32;
        phase += step;
    }
    out
}

/// Three-partial master (bin-centred like the golden harness).
fn master_2s() -> Vec<f32> {
    let parts = [(445.3125_f32, 0.5_f32), (890.625, 0.25), (1335.9375, 0.125)];
    let mut out = vec![0.0_f32; N2S];
    for &(freq, amp) in &parts {
        let step = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(SR);
        let mut phase = 0.0_f64;
        for slot in out.iter_mut() {
            *slot += (f64::from(amp) * phase.sin()) as f32;
            phase += step;
        }
    }
    out
}

/// Deterministic uniform noise (fixed xorshift seed, no RNG dependency).
fn white_noise(len: usize, peak: f32) -> Vec<f32> {
    let mut s: u64 = 0x243F_6A88_85A3_08D3;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let u = (s >> 11) as f32 / (u64::MAX >> 11) as f32; // [0, 1)
            (u * 2.0 - 1.0) * peak
        })
        .collect()
}

/// Quiet sine bed plus sub-clipping impulses (transient-heavy content).
fn impulse_bed() -> Vec<f32> {
    let mut x: Vec<f32> = master_2s().iter().map(|s| s * 0.15).collect();
    for k in 0..8 {
        let pos = 6000 + k * 12_000;
        if pos < x.len() {
            x[pos] += 0.5;
        }
    }
    x
}

/// Integrated LUFS + true-peak dBTP via `ebur128-stream`.
///
/// `None` loudness means the signal cleared no gating block — the stream
/// analogue of [`EvalError::SilenceOrTooQuiet`].
fn stream_measure(samples: &[f32]) -> (Option<f64>, Option<f64>) {
    let mut analyzer = AnalyzerBuilder::new()
        .sample_rate(SR)
        .channels(&[Channel::Center])
        .modes(Mode::Integrated | Mode::TruePeak)
        .build()
        .expect("stream analyzer build (fixed mono config)");
    for chunk in samples.chunks(8192) {
        analyzer
            .push_planar::<f32>(&[chunk])
            .expect("stream push (finite mono input)");
    }
    let report = analyzer.finalize();
    (report.integrated_lufs(), report.true_peak_dbtp())
}

#[test]
fn xcheck_integrated_lufs_within_half_lu() {
    let params = MirParams::v1();
    assert_eq!(params.sample_rate, SR);
    let cases: Vec<(&str, Vec<f32>)> = vec![
        ("sine440", sine(N2S, 440.0, 0.4)),
        ("sine1k-hot", sine(N2S, 1000.0, 0.9)),
        ("noise", white_noise(N2S, 0.3)),
        ("impulse-bed", impulse_bed()),
        ("quiet-above-gate", sine(N2S, 440.0, 6e-4)),
    ];
    eprintln!(
        "XCHECK signal | ebur128 LUFS | stream LUFS | delta LU | ebur128 raw dBTP | stream dBTP | verdict"
    );
    for (name, sig) in &cases {
        let fa = analyze(sig, &params).unwrap_or_else(|e| panic!("{name}: analyze failed: {e}"));
        let (stream_lufs, stream_peak) = stream_measure(sig);
        let stream_lufs = stream_lufs.unwrap_or_else(|| panic!("{name}: stream found no loudness"));
        let delta = fa.integrated_lufs - stream_lufs;
        // `analyze` only exposes the post-normalisation peak; subtract the
        // applied gain to recover the raw-peak reading for the table.
        let ebur_raw_peak = fa.normalized_dbtp - fa.gain_db;
        let verdict = if delta.abs() <= XCHECK_TOL_LU {
            "PASS"
        } else {
            "FAIL"
        };
        eprintln!(
            "XCHECK {name} | {:.3} | {:.3} | {delta:+.3} | {ebur_raw_peak:.3} | {:.3} | {verdict}",
            fa.integrated_lufs,
            stream_lufs,
            stream_peak.unwrap_or(f64::NAN),
        );
        assert!(
            delta.abs() <= XCHECK_TOL_LU,
            "{name}: |Δ|={delta:.3} LU exceeds ±{XCHECK_TOL_LU}"
        );
    }

    // Chunk-size determinism of the stream side: one whole push must equal
    // chunked pushes (the crate's documented push-size invariance).
    let sig = &cases[0].1;
    let mut whole = AnalyzerBuilder::new()
        .sample_rate(SR)
        .channels(&[Channel::Center])
        .modes(Mode::Integrated | Mode::TruePeak)
        .build()
        .expect("build whole");
    whole
        .push_planar::<f32>(&[sig.as_slice()])
        .expect("push whole");
    let mut chunked = AnalyzerBuilder::new()
        .sample_rate(SR)
        .channels(&[Channel::Center])
        .modes(Mode::Integrated | Mode::TruePeak)
        .build()
        .expect("build chunked");
    for chunk in sig.chunks(1000) {
        chunked.push_planar::<f32>(&[chunk]).expect("push chunk");
    }
    let (r_whole, r_chunked) = (whole.finalize(), chunked.finalize());
    assert_eq!(r_whole.integrated_lufs(), r_chunked.integrated_lufs());
    assert_eq!(r_whole.true_peak_dbtp(), r_chunked.true_peak_dbtp());
}

#[test]
fn xcheck_silence_agrees_unmeasurable() {
    let params = MirParams::v1();
    // Digital silence: both sides must report "no usable loudness".
    let zeros = vec![0.0_f32; 8192];
    assert!(
        matches!(
            analyze(&zeros, &params),
            Err(EvalError::SilenceOrTooQuiet { .. })
        ),
        "ebur128 side must reject digital silence"
    );
    let (stream_lufs, _) = stream_measure(&zeros);
    assert!(
        stream_lufs.is_none(),
        "stream side must also find silence unmeasurable"
    );
    // Below the −70 LUFS absolute gate: both sides unmeasurable.
    let sub = sine(N2S, 440.0, 1e-4);
    let eval_out = analyze(&sub, &params);
    let (sub_stream, _) = stream_measure(&sub);
    eprintln!("XCHECK sub-gate: eval={eval_out:?} stream_lufs={sub_stream:?}");
    assert!(
        matches!(eval_out, Err(EvalError::SilenceOrTooQuiet { .. })),
        "sub-gate tone must be rejected by analyze"
    );
    assert!(
        sub_stream.is_none(),
        "sub-gate tone must be unmeasurable by the stream meter"
    );
}

#[test]
fn priming_trim_restores_zero_distance() {
    let params = MirParams::v1();
    let master = master_2s();
    // Simulated AAC decode output: 1024 zero priming samples, then the
    // master, then zero tail padding to the codec-frame multiple plus one
    // priming-length frame — the 44100 → 46080 matrix shape
    // (44100 + 1024 + 956) at 48 kHz scale.
    let padded_len =
        master.len().div_ceil(AAC_FRAME_SAMPLES) * AAC_FRAME_SAMPLES + AAC_PRIMING_SAMPLES;
    let mut sim = Vec::with_capacity(padded_len);
    sim.extend(std::iter::repeat_n(0.0_f32, AAC_PRIMING_SAMPLES));
    sim.extend_from_slice(&master);
    sim.resize(padded_len, 0.0);
    assert_eq!(padded_len % AAC_FRAME_SAMPLES, 0);
    eprintln!("PRIMING sim: master={} padded={}", master.len(), sim.len());

    let fa = analyze(&master, &params).expect("master analyzes");
    let fb_full = analyze(&sim, &params).expect("padded analyzes");
    // Different frame counts are incomparable by construction — this is why
    // the consumer must trim before scoring.
    assert!(matches!(
        compare(&fa, &fb_full),
        Err(EvalError::LengthMismatch { .. })
    ));

    // Sensitivity proof: truncating without the head trim aligns the wrong
    // samples and must show distance.
    let misaligned = analyze(&sim[..master.len()], &params).expect("misaligned analyzes");
    let s_mis = compare(&fa, &misaligned).expect("misaligned compares");
    eprintln!(
        "PRIMING misaligned: mel_l1={:.6} w={:.6} dLUFS={:+.3}",
        s_mis.mel_l1,
        s_mis.mel_weighted,
        misaligned.integrated_lufs - fa.integrated_lufs
    );

    // The rule: drop the priming head, truncate to the source count.
    let trimmed = strip_codec_padding(&sim, AAC_PRIMING_SAMPLES, master.len());
    assert_eq!(trimmed.len(), master.len());
    assert_eq!(trimmed, master.as_slice());
    let f_trim = analyze(trimmed, &params).expect("trimmed analyzes");
    let s_trim = compare(&fa, &f_trim).expect("trimmed compares");
    eprintln!(
        "PRIMING trimmed: mel_l1={:.3e} dLUFS={:+.3e}",
        s_trim.mel_l1,
        f_trim.integrated_lufs - fa.integrated_lufs
    );
    assert_eq!(s_trim.spec_l1, 0.0);
    assert_eq!(s_trim.mel_l1, 0.0);
    assert_eq!(s_trim.mel_weighted, 0.0);
    assert_eq!(s_trim.transient_f1, 1.0);
    assert!(
        s_mis.mel_l1 > 0.01,
        "test insensitive: misaligned mel_l1={}",
        s_mis.mel_l1
    );
}

fn ffmpeg_bin() -> PathBuf {
    std::env::var_os("FFMPEG_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(FFMPEG_DEFAULT))
}

/// Minimal mono PCM-16 WAV writer (test fixture synthesis only).
fn write_wav_pcm16(path: &Path, samples: &[f32], sample_rate: u32) {
    let n = samples.len() as u32;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + n * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(n * 2).to_le_bytes());
    for s in samples {
        let q = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        wav.extend_from_slice(&q.to_le_bytes());
    }
    std::fs::write(path, &wav).unwrap_or_else(|e| panic!("write wav {}: {e}", path.display()));
}

/// Decode any symphonia-readable file to mono f32 (multi-channel is mixed
/// down by averaging). Returns `(samples, sample_rate, channels)`.
fn symphonia_decode_mono(path: &Path) -> (Vec<f32>, u32, usize) {
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::codecs::registry::CodecRegistry;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::formats::TrackType;
    use symphonia::core::formats::probe::{Hint, Probe};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let mut probe = Probe::new();
    symphonia::default::register_enabled_formats(&mut probe);
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    let mut format = probe
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .unwrap_or_else(|e| panic!("probe {}: {e}", path.display()));
    let track = format
        .default_track(TrackType::Audio)
        .expect("default audio track");
    let track_id = track.id;
    let CodecParameters::Audio(audio_params) = track.codec_params.clone().expect("codec params")
    else {
        panic!("default track of {} is not audio", path.display());
    };
    let mut decoder = registry
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .unwrap_or_else(|e| panic!("make decoder {}: {e}", path.display()));
    let mut out: Vec<f32> = Vec::new();
    let (mut channels, mut rate) = (0_usize, 0_u32);
    for _ in 0..MAX_PACKETS {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(e) => panic!("packet {}: {e}", path.display()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::ResetRequired) => continue,
            Err(e) => panic!("decode {}: {e}", path.display()),
        };
        let nch = decoded.spec().channels().count().max(1);
        channels = nch;
        rate = decoded.spec().rate();
        let mut interleaved = vec![0.0_f32; decoded.frames() * nch];
        decoded.copy_to_slice_interleaved::<f32, _>(interleaved.as_mut_slice());
        for frame in interleaved.chunks(nch) {
            let sum: f32 = frame.iter().sum();
            out.push(sum / nch as f32);
        }
    }
    assert!(!out.is_empty(), "decoded 0 samples from {}", path.display());
    (out, rate, channels)
}

#[test]
fn aac_roundtrip_priming_rule() {
    let ffmpeg = ffmpeg_bin();
    if !ffmpeg.exists() {
        eprintln!("AAC SKIP: ffmpeg not found at {}", ffmpeg.display());
        return;
    }
    let params = MirParams::v1();
    let master = master_2s();
    let scratch = std::env::temp_dir().join(format!("synthlm-aac-priming-{}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).expect("clear scratch");
    }
    std::fs::create_dir_all(&scratch).expect("create scratch");
    let wav_path = scratch.join("master.wav");
    let m4a_path = scratch.join("master.m4a");
    write_wav_pcm16(&wav_path, &master, SR);

    let output = Command::new(&ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(&wav_path)
        .args(["-c:a", "aac", "-b:a", "128k"])
        .arg(&m4a_path)
        .output()
        .unwrap_or_else(|e| panic!("ffmpeg spawn: {e}"));
    assert!(
        output.status.success(),
        "ffmpeg encode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Both paths go through the same symphonia decoder; only the codec
    // differs, so any frame-count/loudness delta is the codec's.
    let (wav_pcm, wav_sr, wav_ch) = symphonia_decode_mono(&wav_path);
    let (aac_pcm, aac_sr, aac_ch) = symphonia_decode_mono(&m4a_path);
    assert_eq!((wav_sr, wav_ch), (SR, 1));
    assert_eq!(
        wav_pcm.len(),
        master.len(),
        "wav path must reproduce the source frame count"
    );
    assert_eq!((aac_sr, aac_ch), (SR, 1));
    eprintln!(
        "AAC struct: wav={} aac={} priming={} frame={}",
        wav_pcm.len(),
        aac_pcm.len(),
        AAC_PRIMING_SAMPLES,
        AAC_FRAME_SAMPLES
    );
    assert!(
        aac_pcm.len() > wav_pcm.len(),
        "AAC decode must carry priming/padding: aac={} wav={}",
        aac_pcm.len(),
        wav_pcm.len()
    );
    assert_eq!(
        aac_pcm.len() % AAC_FRAME_SAMPLES,
        0,
        "AAC decode length must be a codec-frame multiple: {}",
        aac_pcm.len()
    );

    let fa = analyze(&wav_pcm, &params).expect("wav path analyzes");
    let fb_full = analyze(&aac_pcm, &params).expect("aac path analyzes");
    eprintln!(
        "AAC raw: wav_int={:.3} aac_int={:.3} d_int={:+.3} wav_dbtp={:.3} aac_dbtp={:.3}",
        fa.integrated_lufs,
        fb_full.integrated_lufs,
        fb_full.integrated_lufs - fa.integrated_lufs,
        fa.normalized_dbtp,
        fb_full.normalized_dbtp
    );
    // Untrimmed, the two feature grids are incomparable by construction.
    assert!(matches!(
        compare(&fa, &fb_full),
        Err(EvalError::LengthMismatch { .. })
    ));

    // Sensitivity proof: truncating without the head trim aligns the wrong
    // samples and must read far.
    let misaligned = analyze(&aac_pcm[..wav_pcm.len()], &params).expect("misaligned analyzes");
    let s_mis = compare(&fa, &misaligned).expect("misaligned compares");

    // The rule: drop the priming head, truncate to the source count.
    let trimmed = strip_codec_padding(&aac_pcm, AAC_PRIMING_SAMPLES, wav_pcm.len());
    assert_eq!(
        trimmed.len(),
        wav_pcm.len(),
        "trimmed AAC must recover the source frame count"
    );
    let f_trim = analyze(trimmed, &params).expect("trimmed analyzes");
    let s_trim = compare(&fa, &f_trim).expect("trimmed compares");
    eprintln!(
        "AAC misaligned: mel_l1={:.6} w={:.6} dLUFS={:+.3}",
        s_mis.mel_l1,
        s_mis.mel_weighted,
        misaligned.integrated_lufs - fa.integrated_lufs
    );
    eprintln!(
        "AAC trimmed: mel_l1={:.6} w={:.6} dLUFS={:+.3} f1={:.3}",
        s_trim.mel_l1,
        s_trim.mel_weighted,
        f_trim.integrated_lufs - fa.integrated_lufs,
        s_trim.transient_f1
    );
    // Pinned 2026-10-06 (first green run, FFmpeg 9.0.2 native AAC 128k,
    // symphonia 0.6.1 decode): trimmed mel_l1=0.276, w=0.372, spec=0.0057,
    // dLUFS=+0.014 vs misaligned mel_l1=0.329, w=0.433, spec=0.0104.
    // A trim sweep (0/512/1024/1056/1088) confirmed 1024 gives the minimum
    // mel distance, i.e. the head really is the 1024-sample encoder delay.
    // Lossy coding leaves residue, so the trimmed distance is small but
    // nonzero; the rule's evidence is (a) exact frame-count recovery,
    // (b) directional improvement over the misaligned truncation in both
    // spectral domains, (c) LUFS agreement. Note the transient F1 reads 0.0
    // even trimmed: AAC frame-rate coding artifacts trip the flux picker
    // (candidate onsets [4, 35, 41, 46, 59] vs reference []) while the
    // stationary reference carries no envelope events — onset calibration
    // on lossy material belongs to TSK-402, not to this alignment rule.
    assert!(
        (0.20..=0.35).contains(&s_trim.mel_l1),
        "trimmed mel_l1={}",
        s_trim.mel_l1
    );
    assert!(
        (0.30..=0.45).contains(&s_trim.mel_weighted),
        "trimmed w={}",
        s_trim.mel_weighted
    );
    assert!(
        s_trim.mel_l1 < s_mis.mel_l1,
        "trim must beat misaligned: trim={} mis={}",
        s_trim.mel_l1,
        s_mis.mel_l1
    );
    assert!(
        s_trim.spec_l1 < s_mis.spec_l1,
        "trim must beat misaligned (linear domain): trim={} mis={}",
        s_trim.spec_l1,
        s_mis.spec_l1
    );
    assert!(
        (f_trim.integrated_lufs - fa.integrated_lufs).abs() < 0.5,
        "trimmed LUFS drift: {:+.3}",
        f_trim.integrated_lufs - fa.integrated_lufs
    );
}
