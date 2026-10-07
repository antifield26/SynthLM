//! Stem-separation background queue, content-addressed artifact cache, and GC.
//!
//! Implements DEC-009 and ARCHITECTURE §7–§8 for the DSP side:
//!
//! - background [`Job`] queue (`Pending`/`Running`/`Done`/`Failed` state
//!   machine; illegal transitions are rejected, never coerced);
//! - content-addressed [`CacheKey`] (`input_fingerprint` + `params_hash`,
//!   never a raw path) with artifacts stored under an external cache root
//!   ([`default_cache_root`]) while the project side keeps only the
//!   pointer type [`ArtifactRef`];
//! - GC ([`run_gc`]): 50 GiB watermark ([`WATERMARK_BYTES`]) + LRU
//!   (built-in access order, no atime reliance) + orphan reclamation
//!   (unreferenced artifacts are deleted), plus [`CacheMetrics`] for later
//!   observability consumers;
//! - Demucs backends ([`StemSeparator`] seam / [`DemucsStub`] not-wired
//!   marker / [`DemucsOnnxSeparator`] wired local ONNX backend): the wired
//!   backend verifies the lazily-cached pinned weights, runs the 7.8 s
//!   windowed forward pass on a background worker, and writes stem WAVs
//!   through the content-addressed cache (pure local CPU, zero audio
//!   leaves the machine).
//!
//! Dependency direction (DEC-022): `dsp` depends only on `common`
//! (single-direction `dsp` ← `common`); it never touches `bridge`/DAW code
//! (AGENTS.md red lines 2–3: no audio-thread work, no inference in the DAW
//! process — this crate is control-plane queue/cache bookkeeping only).

pub mod artifact;
pub mod demucs;
pub mod error;
pub mod gc;
pub mod job;
pub mod key;
pub mod store;

pub use artifact::{ArtifactKind, ArtifactRef};
pub use demucs::{
    DEMUCS_CHANNELS, DEMUCS_INPUT_NAME, DEMUCS_ONNX_BYTES, DEMUCS_ONNX_FILE, DEMUCS_ONNX_LICENSE,
    DEMUCS_ONNX_OPSET, DEMUCS_ONNX_REPO, DEMUCS_ONNX_REV, DEMUCS_ONNX_SHA256, DEMUCS_ONNX_URL,
    DEMUCS_ONNX_VARIANT, DEMUCS_OUTPUT_NAME, DEMUCS_SAMPLE_RATE, DEMUCS_SOURCES,
    DEMUCS_WINDOW_SAMPLES, DemucsModel, DemucsOnnxSeparator, DemucsStub, MixPcm, StemAccumulator,
    StemRow, StemSeparator, canonical_params_preimage, chunk_plan, default_model_path,
    encode_wav_f32, overlap_window, verify_cached_weight,
};
pub use error::DspError;
pub use gc::{CacheMetrics, GcPolicy, GcReport, WATERMARK_BYTES, run_gc};
pub use job::{Job, JobKind, JobState};
pub use key::CacheKey;
pub use store::{ContentStore, default_cache_root};
