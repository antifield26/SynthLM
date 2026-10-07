//! Error taxonomy for the stem queue / cache / GC layer.
//!
//! Library boundary, so errors use `thiserror` (AGENTS.md §4). There is no
//! secret-carrying variant by construction: details hold fingerprints,
//! key strings, and byte counts only — never keys, PCM, or absolute paths
//! (AGENTS.md §8; paths that reach errors are cache-relative).

use thiserror::Error;

/// Failures for job transitions, cache addressing, store I/O, and the
/// (not yet wired) Demucs backend.
#[derive(Debug, Error)]
pub enum DspError {
    /// A job state transition outside the legal matrix was requested.
    #[error("illegal job transition: {from} -> {to}")]
    IllegalTransition {
        /// State the job is currently in.
        from: String,
        /// State the caller requested.
        to: String,
    },

    /// `input_fingerprint` failed component validation (hex digest expected).
    #[error("invalid input fingerprint: {reason}")]
    InvalidFingerprint {
        /// Short, secret-free reason (length/charset, never the value).
        reason: String,
    },

    /// `params_hash` failed component validation (hex digest expected).
    #[error("invalid params hash: {reason}")]
    InvalidParamsHash {
        /// Short, secret-free reason (length/charset, never the value).
        reason: String,
    },

    /// A cache-key string failed shape validation (`kind/fp-ph`, no paths).
    #[error("invalid cache key: {reason}")]
    InvalidCacheKey {
        /// Short reason (shape/charset, never a filesystem path).
        reason: String,
    },

    /// Cache-root or artifact file I/O failure.
    #[error("cache io: {0}")]
    Io(#[from] std::io::Error),

    /// Access-order sidecar JSON failure.
    #[error("cache index json: {0}")]
    Json(#[from] serde_json::Error),

    /// Demucs execution requested before the backend is wired.
    ///
    /// Always carries the `TODO(TSK-205后续)` marker so stray callers are
    /// greppable; this task ships queue + cache only.
    #[error("demucs backend not wired (TODO(TSK-205后续)): {op}")]
    BackendNotWired {
        /// Operation that was attempted (e.g. `"separate"`, `"ensure_model"`).
        op: String,
    },

    /// Demucs model weights are not usable here (TSK-603).
    ///
    /// Terminal `BLOCKED`, never a silent fetch: the detail names the weight
    /// file (never an absolute path) plus the pinned fetch URL, or explains
    /// that the selected variant has no pinned manifest, or that the crate
    /// was built without the `onnx` feature. Raw audio is never involved
    /// (AGENTS.md §8).
    #[error("demucs model unavailable: {detail}")]
    ModelUnavailable {
        /// Short, secret-free reason (file label + pinned URL, or the
        /// missing-manifest / missing-feature explanation).
        detail: String,
    },

    /// A cached weight file disagrees with the pinned manifest (TSK-603).
    ///
    /// Stale or truncated cache: re-fetch the pinned revision, never run
    /// through it.
    #[error("demucs weight size mismatch: expected {expected} bytes, observed {observed}")]
    WeightMismatch {
        /// Pinned manifest size in bytes.
        expected: u64,
        /// Observed file size in bytes.
        observed: u64,
    },

    /// Separator input rejected before any inference (TSK-603).
    ///
    /// The backend takes explicit 44.1 kHz mono/stereo `f32` PCM only
    /// (upstream `infer.py` contract: other rates must be resampled by the
    /// caller); empty, ragged, or non-finite inputs fail here.
    #[error("unsupported separator input: {detail}")]
    UnsupportedInput {
        /// Short, secret-free reason (shape/rate, never audio content).
        detail: String,
    },

    /// ONNX session open or forward pass failed (TSK-603, `onnx` feature).
    ///
    /// Carries the weight file label and the failing stage only — never
    /// audio, never an absolute path (AGENTS.md §8).
    #[error("demucs inference failed: {detail}")]
    InferenceFailed {
        /// Short reason (stage + file label, never audio/paths).
        detail: String,
    },
}
