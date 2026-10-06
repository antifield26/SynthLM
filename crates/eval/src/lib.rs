//! SynthLM evaluation layer: LUFS-first multi-objective scoring.
//!
//! MIR v1 pipeline (TSK-202):
//!
//! - [`mir::MirParams::v1`] freezes the DEC-012 analysis parameters
//!   (48 kHz, Hann 2048, hop 512, 80-band log-mel).
//! - [`mir::analyze`] runs decode-assumed-mono f32 samples through
//!   loudness measurement ([`ebur128`], EBU R128) first, applies gain to
//!   [`mir::MirParams::target_lufs`] (DEC-016, default −14 LUFS), then
//!   computes the STFT magnitude spectrogram ([`realfft`]/[`rustfft`]),
//!   80-band log-mel, and a hand-written spectral-flux transient envelope.
//! - [`score::compare`] turns two [`mir::MirFeatures`] snapshots into a
//!   [`score::Score`] (ARCHITECTURE §6). Features from different
//!   [`mir::MirParams`] are rejected as incomparable.
//! - CLAP cosine similarity is a trait seam only
//!   ([`score::ClapEmbedder`]); no model is wired yet.
//!
//! All analysis runs off the DAW threads (L8); nothing here touches REAPER.

pub mod mir;
pub mod score;

pub use mir::{MirFeatures, MirParams, analyze};
pub use score::{ClapEmbedder, Score, compare};

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
    /// Two [`MirFeatures`] snapshots were produced with different
    /// [`MirParams`]; their scores would be incomparable (DEC-012), so the
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
    /// A CLAP embedding was requested but no model backend is wired
    /// (see `TODO(TSK-202)` on [`ClapEmbedder`]).
    #[error("CLAP model unavailable: {0}")]
    ClapUnavailable(String),
}
