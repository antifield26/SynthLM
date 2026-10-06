//! Deterministic offline audition synthesis plus a minimal WAV writer.
//!
//! Renders the fixed 1-second 48 kHz mono variants the TSK-405 demo scores
//! with the eval pipeline (bright adds air partials, dark drops them, wide
//! detunes slightly for a chorus-like beating proxy). The writer emits plain
//! PCM-16 WAV so the runbook listen step needs no codec; both helpers are
//! pure offline computation plus one file write (never DAW or audio thread).

use std::fs;
use std::path::Path;

use super::DemoError;

/// Which fixed audition variant to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SynthKind {
    /// Reference bed: 220 + 440 + 660 Hz partials.
    Base,
    /// Bed plus 880 + 1760 Hz air partials (presence lift proxy).
    Bright,
    /// 220 Hz plus a quieter 440 Hz only (high-end rolloff proxy).
    Dark,
    /// Bed with the upper partials detuned by a few Hz (width proxy:
    /// beating shimmer; ReaEQ has no true stereo-width parameter, and the
    /// plan records that honestly).
    Wide,
}

/// Sample rate of every synthesized variant (matches MIR v1).
pub const SAMPLE_RATE: u32 = 48_000;

/// Length of every variant: exactly one second.
pub const NUM_SAMPLES: usize = 48_000;

/// Render one fixed variant (deterministic: no seed, no noise).
#[must_use]
pub fn render(kind: SynthKind) -> Vec<f32> {
    let mut out = Vec::with_capacity(NUM_SAMPLES);
    let mut index = 0usize;
    while index < NUM_SAMPLES {
        let time = index as f32 / SAMPLE_RATE as f32;
        let tau = std::f32::consts::TAU;
        let sample = match kind {
            SynthKind::Base => {
                0.30 * (tau * 220.0 * time).sin()
                    + 0.20 * (tau * 440.0 * time).sin()
                    + 0.10 * (tau * 660.0 * time).sin()
            }
            SynthKind::Bright => {
                0.30 * (tau * 220.0 * time).sin()
                    + 0.20 * (tau * 440.0 * time).sin()
                    + 0.10 * (tau * 660.0 * time).sin()
                    + 0.15 * (tau * 880.0 * time).sin()
                    + 0.10 * (tau * 1760.0 * time).sin()
            }
            SynthKind::Dark => {
                0.30 * (tau * 220.0 * time).sin() + 0.15 * (tau * 440.0 * time).sin()
            }
            SynthKind::Wide => {
                0.30 * (tau * 220.0 * time).sin()
                    + 0.20 * (tau * 443.0 * time).sin()
                    + 0.10 * (tau * 665.0 * time).sin()
            }
        };
        out.push(sample);
        index += 1;
    }
    out
}

/// Write mono PCM-16 WAV (44-byte header, little-endian).
///
/// # Errors
///
/// Returns [`super::DemoError::Io`] for empty input or a failed write.
pub fn write_wav_mono16(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), DemoError> {
    if samples.is_empty() {
        return Err(DemoError::Io(
            "refusing to write an empty preview".to_owned(),
        ));
    }
    let data_len = samples
        .len()
        .checked_mul(2)
        .ok_or_else(|| DemoError::Io("preview too large for a WAV data chunk".to_owned()))?;
    let data_u32 = u32::try_from(data_len)
        .map_err(|_| DemoError::Io("preview too large for a WAV data chunk".to_owned()))?;
    let chunk_len = data_len
        .checked_add(36)
        .ok_or_else(|| DemoError::Io("preview too large for a WAV RIFF chunk".to_owned()))?;
    let chunk_u32 = u32::try_from(chunk_len)
        .map_err(|_| DemoError::Io("preview too large for a WAV RIFF chunk".to_owned()))?;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&u32_value(chunk_u32));
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&u32_value(16));
    bytes.extend_from_slice(&u16_value(1));
    bytes.extend_from_slice(&u16_value(1));
    bytes.extend_from_slice(&u32_value(sample_rate));
    bytes.extend_from_slice(&u32_value(sample_rate.checked_mul(2).ok_or_else(|| {
        DemoError::Io("sample rate overflows the WAV byte rate".to_owned())
    })?));
    bytes.extend_from_slice(&u16_value(2));
    bytes.extend_from_slice(&u16_value(16));
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&u32_value(data_u32));
    for sample in samples {
        let quantized = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&quantized.to_le_bytes());
    }
    fs::write(path, &bytes).map_err(|err| DemoError::Io(format!("preview write failed: {err}")))?;
    Ok(())
}

/// Little-endian encoding of a `u32` WAV header field.
fn u32_value(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

/// Little-endian encoding of a `u16` WAV header field.
fn u16_value(value: u16) -> [u8; 2] {
    value.to_le_bytes()
}
