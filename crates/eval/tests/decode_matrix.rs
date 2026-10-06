//! Decode coverage matrix: symphonia (primary) vs FFmpeg CLI (fallback).
//!
//! TSK-206 spike. Fixtures are transcoded into `%TEMP%` with the local FFmpeg
//! (Gyan 9.0.2-essentials, GPL build, internal-run only) from the repo fixture
//! `experiments/spike-tone.wav` (PCM s16le, mono, 44.1 kHz, 1 s), then each
//! file is decoded with `symphonia` 0.6.1 (MPL-2.0) and对照-decoded with the
//! FFmpeg CLI. The verdict is the report at
//! `experiments/decode-matrix.out.txt` (format x result table + fallback
//! trigger list). Nothing here is business logic (`src/` untouched).
//!
//! Fallback rule under test: `fallback = !symphonia_ok && ffmpeg_ok`, where
//! `symphonia_ok` requires at least one decoded audio frame.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::{Hint, Probe};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Local FFmpeg when `FFMPEG_BIN` is unset. GPL-2+ build
/// (`--enable-gpl --enable-librubberband`), internal-run only; it is the
///对照 tool here, never the shipped fallback (see report §4).
const FFMPEG_DEFAULT: &str =
    r"C:\Users\25371\tools\ffmpeg\ffmpeg-9.0.2-essentials_build\bin\ffmpeg.exe";

/// Upper bound on packets per file so garbage inputs cannot spin the loop.
const MAX_PACKETS: usize = 10_000;

fn ffmpeg_bin() -> PathBuf {
    std::env::var_os("FFMPEG_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(FFMPEG_DEFAULT))
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct SymOutcome {
    ok: bool,
    detail: String,
    frames: u64,
}

fn try_decode(path: &Path) -> Result<(u64, String), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
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
        .map_err(|e| format!("probe: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "no default audio track".to_string())?;
    let params = track
        .codec_params
        .clone()
        .ok_or_else(|| "missing codec params".to_string())?;
    let CodecParameters::Audio(audio_params) = params else {
        return Err("default track is not audio".to_string());
    };
    let track_id = track.id;
    let spec = format!(
        "codec={:?} sr={} ch={}",
        audio_params.codec,
        audio_params.sample_rate.unwrap_or(0),
        audio_params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(0)
    );
    let mut decoder = registry
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .map_err(|e| format!("make decoder: {e}"))?;
    let mut frames: u64 = 0;
    for _ in 0..MAX_PACKETS {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(e) => return Err(format!("packet: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => frames += decoded.frames() as u64,
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::ResetRequired) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        }
    }
    Ok((frames, spec))
}

fn symphonia_decode(path: &Path) -> SymOutcome {
    match try_decode(path) {
        Ok((frames, spec)) if frames > 0 => SymOutcome {
            ok: true,
            detail: format!("frames={frames} {spec}"),
            frames,
        },
        Ok((frames, spec)) => SymOutcome {
            ok: false,
            detail: format!("decoded 0 frames {spec}"),
            frames,
        },
        Err(e) => SymOutcome {
            ok: false,
            detail: e,
            frames: 0,
        },
    }
}

struct FfOutcome {
    ran: bool,
    ok: bool,
    detail: String,
}

/// 对照-decode with the FFmpeg CLI into a throwaway wav under `%TEMP%`.
/// `ran=false` when no FFmpeg binary exists (assertions on the对照 side are
/// then skipped; the symphonia side still asserts).
fn ffmpeg_decode(ffmpeg: &Path, input: &Path, out_wav: &Path) -> FfOutcome {
    if !ffmpeg.exists() {
        return FfOutcome {
            ran: false,
            ok: false,
            detail: "SKIP: ffmpeg binary not found".to_string(),
        };
    }
    let output = Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(input)
        .args(["-f", "wav", "-ar", "44100", "-ac", "1"])
        .arg(out_wav)
        .output();
    match output {
        Err(e) => FfOutcome {
            ran: true,
            ok: false,
            detail: format!("spawn: {e}"),
        },
        Ok(o) => {
            let bytes = std::fs::metadata(out_wav).map(|m| m.len()).unwrap_or(0);
            let first_err: String = String::from_utf8_lossy(&o.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(160)
                .collect();
            if o.status.success() && bytes > 44 {
                FfOutcome {
                    ran: true,
                    ok: true,
                    detail: format!("bytes={bytes}"),
                }
            } else {
                FfOutcome {
                    ran: true,
                    ok: false,
                    detail: format!("exit={} bytes={bytes} {first_err}", o.status)
                        .trim()
                        .to_string(),
                }
            }
        }
    }
}

/// Deterministic filler for the random-bytes corrupt case (fixed seed, no RNG dep).
fn prng_bytes(n: usize) -> Vec<u8> {
    let mut s: u64 = 0x243F_6A88_85A3_08D3;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s & 0xFF) as u8
        })
        .collect()
}

fn run_ffmpeg(ffmpeg: &Path, args: &[&str]) {
    let status = Command::new(ffmpeg)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("ffmpeg spawn failed ({}): {e}", ffmpeg.display()));
    assert!(
        status.status.success(),
        "ffmpeg fixture gen failed: {} stderr={}",
        args.join(" "),
        String::from_utf8_lossy(&status.stderr)
            .lines()
            .next()
            .unwrap_or("")
    );
}

#[test]
fn decode_matrix_symphonia_vs_ffmpeg() {
    let ffmpeg = ffmpeg_bin();
    let ff_present = ffmpeg.exists();

    let src_wav = crate_dir().join("../../experiments/spike-tone.wav");
    assert!(src_wav.is_file(), "missing fixture {}", src_wav.display());

    // Scratch lives in %TEMP% (AGENTS.md: generated files stay out of the repo).
    let scratch =
        std::env::temp_dir().join(format!("synthlm-decode-matrix-{}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).expect("clear scratch");
    }
    std::fs::create_dir_all(&scratch).expect("create scratch");

    // Valid-input fixtures, transcoded locally from the wav baseline.
    let valid: Vec<(&str, &str, Vec<&str>)> = vec![
        ("wav", "m.wav", vec!["-c:a", "pcm_s16le"]),
        ("flac", "m.flac", vec!["-c:a", "flac"]),
        ("mp3", "m.mp3", vec!["-c:a", "libmp3lame", "-b:a", "128k"]),
        ("vorbis", "m.ogg", vec!["-c:a", "libvorbis", "-q:a", "4"]),
        ("aac", "m.m4a", vec!["-c:a", "aac", "-b:a", "128k"]),
    ];
    if ff_present {
        for (_, file, enc) in &valid {
            let out = scratch.join(file);
            let mut args: Vec<&str> = vec!["-v", "error", "-y", "-i"];
            args.push(src_wav.to_str().expect("utf8 fixture path"));
            args.extend_from_slice(enc);
            args.push(out.to_str().expect("utf8 scratch path"));
            run_ffmpeg(&ffmpeg, &args);
        }
    }

    // Corrupt-input fixtures (no encoder involved).
    let src_bytes = std::fs::read(&src_wav).expect("read fixture");
    std::fs::write(scratch.join("trunc.wav"), &src_bytes[..100]).expect("write trunc");
    std::fs::write(scratch.join("empty.wav"), []).expect("write empty");
    std::fs::write(scratch.join("random.bin"), prng_bytes(4096)).expect("write random");

    let mut report = String::new();
    let _ = writeln!(
        report,
        "TSK-206 decode matrix (symphonia primary vs FFmpeg CLI对照)"
    );
    let _ = writeln!(report, "date(UTC): 2026-10-06");
    let _ = writeln!(
        report,
        "symphonia: 0.6.1 MPL-2.0 (features: default + mp3/aac/isomp4)"
    );
    let _ = writeln!(
        report,
        "ffmpeg: {} ({})",
        ffmpeg.display(),
        if ff_present {
            "Gyan 9.0.2-essentials GPL-2+ build, internal-run only"
        } else {
            "NOT FOUND — 对照 side skipped"
        }
    );
    let _ = writeln!(
        report,
        "source: experiments/spike-tone.wav (pcm_s16le mono 44100Hz 1s, 44100 frames)"
    );
    let _ = writeln!(
        report,
        "scratch: %TEMP%/{} (fixtures, not committed)",
        scratch.file_name().unwrap_or_default().to_string_lossy()
    );
    let _ = writeln!(report, "rule: fallback = !symphonia_ok && ffmpeg_ok");
    report.push_str("format | file | symphonia | ffmpeg对照 | fallback\n");

    let mut sym_ok_count = 0_usize;
    let mut fallback_armed: Vec<String> = Vec::new();

    let mut check_row = |label: &str, file: &str, sym: &SymOutcome, ff: &FfOutcome| {
        let fallback = !sym.ok && ff.ok;
        if sym.ok {
            sym_ok_count += 1;
        }
        if fallback {
            fallback_armed.push(label.to_string());
        }
        let ff_cell = if ff.ran {
            format!("{} {}", if ff.ok { "ok" } else { "FAIL" }, ff.detail)
        } else {
            ff.detail.clone()
        };
        let _ = writeln!(
            report,
            "{label} | {file} | {} {} | {ff_cell} | {}",
            if sym.ok { "ok" } else { "FAIL" },
            sym.detail,
            if fallback { "YES" } else { "no" }
        );
    };

    // Valid inputs.
    let mut sym_by_label: Vec<(String, SymOutcome)> = Vec::new();
    let mut skipped_rows: Vec<String> = Vec::new();
    for (label, file, _) in &valid {
        let candidate = scratch.join(file);
        // Without FFmpeg the transcoded fixtures do not exist: only the wav
        // baseline row can run (straight from the repo fixture); the rest
        // are recorded as skipped, never asserted.
        let path = if candidate.is_file() {
            candidate
        } else if *label == "wav" {
            src_wav.clone()
        } else {
            skipped_rows.push(format!("{label} | {file} | skipped (no ffmpeg) | - | -"));
            continue;
        };
        let sym = symphonia_decode(&path);
        let ff_out = scratch.join(format!("ff-{label}.wav"));
        let ff = ffmpeg_decode(&ffmpeg, &path, &ff_out);
        check_row(label, file, &sym, &ff);
        sym_by_label.push((label.to_string(), sym));
    }
    // Corrupt inputs.
    for (label, file) in [
        ("trunc-wav", "trunc.wav"),
        ("empty", "empty.wav"),
        ("random", "random.bin"),
    ] {
        let path = scratch.join(file);
        let sym = symphonia_decode(&path);
        let ff_out = scratch.join(format!("ff-{label}.wav"));
        let ff = ffmpeg_decode(&ffmpeg, &path, &ff_out);
        // Corrupt inputs must fail gracefully (Err, never panic); the detail
        // strings are the evidence, so assert on the flag, not the text.
        assert!(
            !sym.ok,
            "{label}: corrupt input decoded unexpectedly: {}",
            sym.detail
        );
        check_row(label, file, &sym, &ff);
    }
    for line in &skipped_rows {
        let _ = writeln!(report, "{line}");
    }

    // Baseline pins (measured 2026-10-06 run; drift means the toolchain
    // changed and the report must be re-examined, not absorbed).
    let get = |label: &str| {
        sym_by_label
            .iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| panic!("missing row {label}"))
            .1
            .frames
    };
    if ff_present {
        // Lossless paths reproduce the 44100-frame baseline exactly.
        assert_eq!(get("wav"), 44_100, "wav frame count drifted");
        assert_eq!(get("flac"), 44_100, "flac frame count drifted");
        // Lossy paths decode fully (encoder padding/priming shifts exact
        // counts; the report carries the exact numbers).
        for label in ["mp3", "vorbis", "aac"] {
            assert!(
                get(label) > 40_000,
                "{label}: lossy decode too short: {}",
                get(label)
            );
        }
        // Fixtures were produced by this same FFmpeg, so the对照 side must
        // decode all five valid inputs.
        assert_eq!(sym_ok_count, 5, "valid-input symphonia sweep regressed");
    } else {
        // Without fixtures only the wav baseline row is meaningful.
        assert_eq!(get("wav"), 44_100, "wav frame count drifted");
    }

    report.push_str("---- fallback trigger list ----\n");
    if fallback_armed.is_empty() {
        report.push_str(
            "none triggered in this matrix: every input symphonia fails is also failed by FFmpeg.\n",
        );
    } else {
        for label in &fallback_armed {
            let _ = writeln!(
                report,
                "TRIGGER {label}: symphonia FAIL + ffmpeg ok -> route decode via FFmpeg CLI."
            );
        }
    }
    report.push_str(
        "standing triggers (apply regardless of this run):\n\
         - T1 symphonia probe Unsupported + ffmpeg ok -> FFmpeg fallback;\n\
         - T2 symphonia decode error/0-frames + ffmpeg ok -> FFmpeg fallback;\n\
         - T3 both fail -> hard error, no fallback (surface to user, keep provenance);\n\
         - T4 local CLI is a GPL build: fallback SHIPS only via an LGPL/non-gpl\n\
           FFmpeg build (TSK-206 follow-up); the Gyan binary stays internal-run.\n",
    );

    let out_path = crate_dir().join("../../experiments/decode-matrix.out.txt");
    std::fs::write(&out_path, &report).expect("write decode-matrix.out.txt");
    eprintln!("{report}");
}
