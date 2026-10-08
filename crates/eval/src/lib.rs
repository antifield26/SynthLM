//! SynthLM evaluation layer: LUFS-first multi-objective scoring.
//!
//! MIR v1 pipeline (TSK-202, multi-resolution + band weights in TSK-204):
//!
//! - [`mir::MirParams::v1`] freezes the DEC-012 analysis parameters
//!   (48 kHz, Hann 2048, hop 512, 80-band log-mel) plus the TSK-204 second
//!   tier (Hann 1024, hop 256) whose log-mel is resampled and averaged.
//! - [`mir::analyze`] runs decode-assumed-mono f32 samples through
//!   loudness measurement ([`ebur128`], EBU R128) first, applies gain to
//!   [`mir::MirParams::target_lufs`] (DEC-016, default −14 LUFS), then
//!   computes the STFT magnitude spectrogram ([`realfft`]/[`rustfft`]),
//!   two-tier-averaged 80-band log-mel, and a hand-written spectral-flux
//!   transient envelope (primary resolution).
//! - [`score::compare`] turns two [`mir::MirFeatures`] snapshots into a
//!   [`score::Score`] (ARCHITECTURE §6): unweighted `mel_l1` for
//!   calibration plus per-band `mel_low|mid|high` and default-weighted
//!   `mel_weighted` for ranking (weights tunable via
//!   [`score::BandWeights`], calibration in TSK-402). Features from
//!   different [`mir::MirParams`] are rejected as incomparable.
//! - CLAP cosine similarity is wired through
//!   [`clap::SpectralClapEmbedder`] (pure-local 512-dim fingerprint, TSK-601)
//!   with the ONNX audio encoder as an opt-in `onnx` feature seam
//!   (pinned manifest, local weight cache, no audio upload); end-to-end
//!   scoring goes through [`clap::score_pair`], candidate-distance dedup
//!   through [`clap::dedup_by_clap`].
//! - [`render::ingest_wav`] ingests 42230 true renders (TSK-504): wav file
//!   → mono f32 PCM → [`mir::analyze`], accepting only 48 kHz natively and
//!   44.1 kHz as resample-todo, with an explicit mono/stereo channel policy,
//!   TSK-208 codec-trim linkage, a three-render null-test gate
//!   ([`render::NullTestGate`]), and a fixed fallback decision table
//!   ([`render::FallbackPolicy`]). Render execution stays REAPER-side.
//!
//! All analysis runs off the DAW threads (L8); nothing here touches REAPER.

pub mod clap;
pub mod mir;
pub mod render;
pub mod score;

pub use clap::{
    CLAP_BINS_PER_FRAME, CLAP_DIM, CLAP_FINGERPRINT_SAMPLES, CLAP_FRAMES, CLAP_ONNX_BASE_MODEL,
    CLAP_ONNX_BYTES, CLAP_ONNX_EXPECTED_INPUTS, CLAP_ONNX_EXPECTED_OUTPUTS, CLAP_ONNX_FILE,
    CLAP_ONNX_INPUT_FEATURES, CLAP_ONNX_INPUT_IS_LONGER, CLAP_ONNX_LICENSE, CLAP_ONNX_OPSET,
    CLAP_ONNX_OUTPUT, CLAP_ONNX_REPO, CLAP_ONNX_REV, CLAP_ONNX_SHA256, CLAP_ONNX_URL,
    DEFAULT_CLAP_DEDUP_DISTANCE, SpectralClapEmbedder, clap_distance, dedup_by_clap,
    dedup_by_clap_default, default_weight_path, score_pair, verify_cached_weight,
};

pub use mir::{MirFeatures, MirParams, analyze};
pub use render::{
    ChannelHandling, CodecTrim, FallbackPolicy, FallbackRecord, NULL_TEST_MAX_MEL_L1,
    NULL_TEST_MAX_SPEC_L1, NULL_TEST_MIN_TRANSIENT_F1, NullTestGate, NullVerdict, RATE_44K1,
    RATE_48K, RateNote, RenderProduct, RenderStage, fnv1a_f32, ingest_wav, ingest_wav_trimmed,
};
pub use score::{BandWeights, ClapEmbedder, Score, compare};

/// Errors returned at the `synthlm-eval` library boundary.
///
/// The crate follows the workspace convention (AGENTS.md §4): library
/// boundaries use `thiserror`; no `unwrap`/`expect` on any main-code path.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// Input holds fewer samples than one analysis window.
    #[error("input too short: need at least {need} samples, got {got}")]
    TooShort {
        /// Minimum required sample count (one [`MirParams::window`]).
        need: usize,
        /// Actual sample count received.
        got: usize,
    },
    /// The signal carries no usable loudness (digital silence, or integrated
    /// loudness below the −70 LUFS absolute gate), so no finite
    /// normalisation gain exists.
    #[error("no usable loudness ({detail}); normalisation gain would not be finite")]
    SilenceOrTooQuiet {
        /// Machine-readable detail (e.g. measured LUFS or error source).
        detail: String,
    },
    /// Two feature snapshots have different frame counts and cannot be
    /// compared sample-by-sample.
    #[error("feature length mismatch: {a} vs {b} frames")]
    LengthMismatch {
        /// Frame count of the first operand.
        a: usize,
        /// Frame count of the second operand.
        b: usize,
    },
    /// Two embeddings (or feature vectors) have different dimensions.
    #[error("dimension mismatch: {a} vs {b}")]
    DimMismatch {
        /// Dimension of the first operand.
        a: usize,
        /// Dimension of the second operand.
        b: usize,
    },
    /// Two [`mir::MirFeatures`] snapshots were produced with different
    /// [`mir::MirParams`]; their scores would be incomparable (DEC-012), so the
    /// comparison is refused instead of silently rescoring.
    #[error("MIR params mismatch: cannot compare features from different MirParams")]
    ParamMismatch,
    /// The real FFT failed (buffer sizing is validated up front, so this
    /// indicates an internal inconsistency).
    #[error("FFT failed: {0}")]
    Fft(String),
    /// Loudness measurement through `ebur128` failed.
    #[error("loudness measurement failed: {0}")]
    Loudness(String),
    /// Input samples are unusable (empty, NaN/infinite, or zero-norm
    /// embedding where a direction is required).
    #[error("invalid samples: {0}")]
    InvalidSamples(String),
    /// A CLAP embedding was requested but the backend is unavailable
    /// (weights not cached, `onnx` feature disabled, or the ONNX inference
    /// contract still uncalibrated — see [`clap::SpectralClapEmbedder`]).
    #[error("CLAP model unavailable: {0}")]
    ClapUnavailable(String),
    /// A render file could not be read (missing file or OS error).
    ///
    /// `hint` carries only the file name, never an absolute path (log-scrub
    /// rule); `detail` is reduced to the OS error kind so no machine-local
    /// path leaks. Terminal `BLOCKED`: fix the path or re-render
    /// REAPER-side, then ingest again.
    #[error("render file unreadable ({hint}): {detail}")]
    RenderIo {
        /// File name only (no absolute path).
        hint: String,
        /// OS error kind (no path content).
        detail: String,
    },
    /// A render wav file is structurally unusable (bad magic, missing
    /// `fmt `/`data` chunk, truncated chunk, or unsupported encoding).
    ///
    /// Terminal `BLOCKED`: re-render as PCM int (8/16/24/32-bit) or 32-bit
    /// float wav REAPER-side.
    #[error("bad render wav ({hint}): {detail}")]
    BadWav {
        /// File name only (no absolute path).
        hint: String,
        /// Structural cause (no audio content, no absolute path).
        detail: String,
    },
    /// The render rate is outside the accepted set (48 kHz native,
    /// 44.1 kHz as resample-todo).
    ///
    /// Terminal `BLOCKED`: re-render at 48 kHz (or 44.1 kHz, accepting the
    /// resample-todo flag) REAPER-side. This crate never silently resamples.
    #[error(
        "unsupported render rate: got {got} Hz, accept only 48000 natively or 44100 as resample-todo"
    )]
    UnsupportedRate {
        /// Observed sample rate in Hz.
        got: u32,
    },
    /// The render has more than two channels.
    ///
    /// Terminal `BLOCKED`: pin the render to mono or stereo REAPER-side
    /// (`RENDER_CHANNELS`, or 40901-mono / 41223-stereo freeze) and ingest
    /// again.
    #[error("unsupported render channels: got {got}, accept only mono or stereo")]
    UnsupportedChannels {
        /// Observed channel count.
        got: usize,
    },
    /// The file size disagrees with the advisory `bytes_hint`.
    ///
    /// Terminal `BLOCKED`: the file changed under the caller (partial
    /// render, overwrite race) — re-render and re-ingest. A zero hint
    /// skips the check.
    #[error("render size mismatch: hint {hint} bytes vs {got} actual")]
    ByteMismatch {
        /// Advisory expected size in bytes.
        hint: u64,
        /// Observed size in bytes.
        got: u64,
    },
    /// The three-render null-test gate found variance over threshold
    /// (hash drift, spectral drift, onset drift, or length/param drift).
    ///
    /// Terminal `BLOCKED`: fix the render setup REAPER-side (leaked live
    /// content is the classic cause, A03 §8) or walk the
    /// [`crate::render::FallbackPolicy`] chain, then re-render and
    /// re-ingest.
    #[error("renders unstable across three takes: {detail}")]
    RenderUnstable {
        /// Gate reading (hashes + distances; no audio content, no paths).
        detail: String,
    },
}

impl EvalError {
    /// Whether this failure is a terminal `BLOCKED` verdict (never silently
    /// retried).
    ///
    /// Every variant currently returns `true`: the eval pipeline is a pure
    /// function of its input bytes, so retrying the same bytes cannot
    /// change the outcome. The only sanctioned retry path is a fresh
    /// REAPER-side render driven by the explicit
    /// [`crate::render::FallbackPolicy`] chain — never an in-eval retry of
    /// the same input.
    #[must_use]
    pub fn blocked(&self) -> bool {
        match self {
            Self::TooShort { .. }
            | Self::SilenceOrTooQuiet { .. }
            | Self::LengthMismatch { .. }
            | Self::DimMismatch { .. }
            | Self::ParamMismatch
            | Self::Fft(_)
            | Self::Loudness(_)
            | Self::InvalidSamples(_)
            | Self::ClapUnavailable(_)
            | Self::RenderIo { .. }
            | Self::BadWav { .. }
            | Self::UnsupportedRate { .. }
            | Self::UnsupportedChannels { .. }
            | Self::ByteMismatch { .. }
            | Self::RenderUnstable { .. } => true,
        }
    }
}
