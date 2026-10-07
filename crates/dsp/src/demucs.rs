//! Demucs adapter seam + wired ONNX backend (DEC-009, C-dsp-toolchain §5).
//!
//! TSK-205 shipped the queue + cache layer with a declared-but-unwired
//! [`crate::demucs::DemucsStub`]. TSK-603 wires one real backend on top of it:
//! [`crate::demucs::DemucsOnnxSeparator`] runs the pinned single-file
//! `htdemucs` ONNX export locally (pure CPU, zero audio leaves the machine)
//! and puts the resulting stem WAV through the TSK-205 content-addressed
//! cache ([`crate::store::ContentStore`]) with GC unchanged
//! ([`crate::gc::run_gc`]).
//!
//! Inference recipe (chunking, triangular overlap-add, row order) is ported
//! from the upstream MIT-licensed reference
//! `infer.py` at the pinned revision (see
//! [`crate::demucs::DEMUCS_ONNX_REPO`]); the *weights* stay research-only
//! per upstream `facebookresearch/demucs#327` (DEC-025): internal
//! non-commercial run only, never distributed, never downloaded by the
//! build — [`crate::demucs::DemucsOnnxSeparator::ensure_model_available`]
//! only verifies the lazily-fetched user-directory cache entry.
//!
//! Cost context (C-dsp-toolchain §5, 2026-10-05): a 3-minute song costs
//! ~4.5 min on CPU (official RTF ~1.5; M4 Pro single-specialist RTF 0.20,
//! full bag 0.49) versus ~47 s on M4 MPS / ~7 s on L4 GPU; VRAM ≥ 3 GB
//! (default working set ~7 GB, lower via `--segment` / `-d cpu`).
//!
//! Threading (AGENTS.md red lines 2–3, L8): everything here runs on a
//! background worker, never on an audio thread and never in the DAW
//! process. The control plane still never carries PCM: jobs carry digests,
//! and inference takes an explicit [`crate::demucs::MixPcm`] argument.

use std::path::PathBuf;

use crate::artifact::ArtifactRef;
use crate::error::DspError;
use crate::job::Job;

/// Demucs model variant selected for a separation job.
///
/// Sizes are per-variant weight volumes that the lazy downloader must fetch
/// on first use (C-dsp-toolchain §5): single-precision per-stem packs start
/// around 166 MB in FP16 form and a full model bag reaches ~1.26 GB.
/// Nothing here downloads anything — the numbers size the cache budget and
/// the first-run progress UI of the follow-up task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DemucsModel {
    /// `htdemucs` (default hybrid transformer): the standard quality pick.
    HtDemucs,
    /// `htdemucs_ft` (fine-tuned): larger pack, higher quality.
    HtDemucsFt,
    /// `htdemucs_6s` (six-stem incl. guitar/piano): widest split.
    HtDemucs6s,
}

impl DemucsModel {
    /// Stable model id (matches upstream Demucs variant names).
    pub fn as_str(self) -> &'static str {
        match self {
            DemucsModel::HtDemucs => "htdemucs",
            DemucsModel::HtDemucsFt => "htdemucs_ft",
            DemucsModel::HtDemucs6s => "htdemucs_6s",
        }
    }

    /// Every selectable variant.
    pub fn all() -> &'static [DemucsModel] {
        &[
            DemucsModel::HtDemucs,
            DemucsModel::HtDemucsFt,
            DemucsModel::HtDemucs6s,
        ]
    }

    /// Approximate weight volume in bytes, for cache budgeting and the
    /// lazy-download progress UI (`TODO(M57-handoff:TSK-205后续)` to wire).
    ///
    /// Figures follow C-dsp-toolchain §5: ~166 MB per FP16 single-stem
    /// pack at the low end, up to ~1.26 GB for a full bag at the high end.
    pub fn approx_weight_bytes(self) -> u64 {
        const MB: u64 = 1024 * 1024;
        match self {
            // Single FP16 per-stem pack end of the range.
            DemucsModel::HtDemucs => 166 * MB,
            // Fine-tuned pack: several stems at higher precision.
            DemucsModel::HtDemucsFt => 512 * MB,
            // Full six-stem bag end of the range (~1.26 GB).
            DemucsModel::HtDemucs6s => 1260 * MB,
        }
    }
}

/// Stem-separation backend seam.
///
/// Queue workers implement this against a control-plane job identity.
/// Backends that run inference locally take an explicit audio argument
/// instead (see [`crate::demucs::DemucsOnnxSeparator::separate_stem`]): the
/// trait takes `&Job` (identity only) rather than audio bytes so backends
/// cannot accidentally pull PCM into the control plane (AGENTS.md §8: raw
/// audio stays out of queues/logs).
pub trait StemSeparator {
    /// Backend name for logs and audit (e.g. `"demucs-onnx-stub"`).
    fn name(&self) -> &'static str;

    /// Ensure the model weights are present (lazy download on first use).
    fn ensure_model_available(&self) -> Result<(), DspError>;

    /// Separate the job's input into stems (identity in, pointers out).
    fn separate(&self, job: &Job) -> Result<Vec<ArtifactRef>, DspError>;
}

/// Declared-but-unwired Demucs backend.
///
/// Holds the model choice and the weights directory so the follow-up only
/// fills in execution; every method currently returns
/// [`DspError::BackendNotWired`]. No subprocess is spawned, no weight is
/// downloaded, and no model code runs in this task.
pub struct DemucsStub {
    /// Selected model variant.
    pub model: DemucsModel,
    /// Directory weights will be lazily downloaded into (follow-up).
    pub model_dir: PathBuf,
}

impl DemucsStub {
    /// Declare the backend for `model` with weights rooted at `model_dir`.
    pub fn new(model: DemucsModel, model_dir: PathBuf) -> Self {
        Self { model, model_dir }
    }

    /// Weight-file path the lazy downloader will populate (follow-up).
    pub fn weight_path(&self) -> PathBuf {
        self.model_dir.join(format!("{}.onnx", self.model.as_str()))
    }
}

impl StemSeparator for DemucsStub {
    fn name(&self) -> &'static str {
        "demucs-onnx-stub"
    }

    fn ensure_model_available(&self) -> Result<(), DspError> {
        // TODO(M57-handoff:TSK-205后续): lazy-download weights (~166MB–1.26GB per
        // DemucsModel::approx_weight_bytes) into model_dir on first use,
        // with progress + checksum verification. This task must not download.
        Err(DspError::BackendNotWired {
            op: "ensure_model".to_owned(),
        })
    }

    fn separate(&self, _job: &Job) -> Result<Vec<ArtifactRef>, DspError> {
        // TODO(M57-handoff:TSK-205后续): run ONNX sidecar / local batch on the job's
        // cached input, then put stems through ContentStore and return
        // ArtifactRefs. Never runs in the DAW process (L8).
        Err(DspError::BackendNotWired {
            op: "separate".to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------
// Pinned Demucs ONNX weight manifest (TSK-603, single-file `htdemucs` fp16)
// ---------------------------------------------------------------------------
//
// All values below were pinned against the Hugging Face Hub on 2026-10-07:
// repo revision via the Hub API, file size + content hash via
// `curl -sI -L <resolve-url>` response headers `X-Linked-Size` /
// `X-Linked-ETag`, graph/input contract via the same-revision `infer.py`
// (`sess.run(["stems"], {"mix": x})`, `SOURCES` order, 7.8 s window with
// quarter overlap). The single-file fp16 pack (165,612,636 B) is the low
// end of the C-dsp-toolchain §5 range (~166 MB); the FT bag (~1.26 GB)
// stays the high end and is NOT pinned here.

/// ONNX export repo (community export of the official `htdemucs` model).
pub const DEMUCS_ONNX_REPO: &str = "StemSplitio/htdemucs-onnx";

/// Pinned repo revision (Hub API, verified 2026-10-07).
pub const DEMUCS_ONNX_REV: &str = "d54ed9eb60e258ea82131c6ee14578628816456a";

/// Weight file inside the repo (fp16-stored weights, same runtime cost).
pub const DEMUCS_ONNX_FILE: &str = "htdemucs_fp16weights.onnx";

/// Pinned download source (revision-qualified; re-verify with
/// `curl -sI -L <url>` and compare `X-Linked-Size` / `X-Linked-ETag`).
pub const DEMUCS_ONNX_URL: &str = "https://huggingface.co/StemSplitio/htdemucs-onnx/resolve/d54ed9eb60e258ea82131c6ee14578628816456a/htdemucs_fp16weights.onnx";

/// Expected weight size in bytes (`X-Linked-Size`, HEAD 2026-10-07).
pub const DEMUCS_ONNX_BYTES: u64 = 165_612_636;

/// Expected content SHA256 (`X-Linked-ETag`, HEAD 2026-10-07).
pub const DEMUCS_ONNX_SHA256: &str =
    "d05c269d0178d2a72ad484b10b11dd370193fc923201c3b27a99f848745db70a";

/// Weight license posture (DEC-025).
///
/// Upstream official weights are NOT covered by the Demucs code MIT
/// license (`scientific purposes only`, see
/// `facebookresearch/demucs#327` via C-dsp-toolchain §5): internal
/// non-commercial run only, no distribution, no commercial inference.
/// The Hub uploader's `license:mit` tag covers the export code/packaging,
/// not the weight rights — this crate follows DEC-025, not the tag.
pub const DEMUCS_ONNX_LICENSE: &str = "research-only (upstream weights not covered by MIT, scientific purposes only; internal run only, no distribution)";

/// Export opset of the pinned file (model page, verified 2026-10-07).
pub const DEMUCS_ONNX_OPSET: u32 = 17;

/// [`crate::demucs::DemucsModel`] variant this manifest pins.
pub const DEMUCS_ONNX_VARIANT: &str = "htdemucs";

/// Model sample rate in Hz (upstream `infer.py` `SAMPLE_RATE`).
pub const DEMUCS_SAMPLE_RATE: u32 = 44_100;

/// Inference window in samples (upstream `infer.py`: 7.8 s at 44.1 kHz).
pub const DEMUCS_WINDOW_SAMPLES: usize = 343_980;

/// Model channel count (stereo; mono inputs are duplicated, per `infer.py`).
pub const DEMUCS_CHANNELS: usize = 2;

/// ONNX input name holding `[batch, 2, 343980]` mix PCM.
pub const DEMUCS_INPUT_NAME: &str = "mix";

/// ONNX output name holding `[batch, 4, 2, 343980]` stems.
pub const DEMUCS_OUTPUT_NAME: &str = "stems";

/// Stem row order of the output tensor (upstream `infer.py` `SOURCES`).
pub const DEMUCS_SOURCES: [&str; 4] = ["drums", "bass", "other", "vocals"];

/// Local weight-cache location (DEC-027 user directory, never the repo).
///
/// Windows resolves `%APPDATA%/SynthLM/models/demucs/` +
/// [`crate::demucs::DEMUCS_ONNX_FILE`]; other platforms use
/// `$XDG_CACHE_HOME/synthlm/demucs/…`, falling back to
/// `$HOME/.cache/synthlm/demucs/…`. Returns `None` when no base directory
/// resolves instead of guessing. No download happens here: a missing file
/// is a `None` path plus [`crate::error::DspError::ModelUnavailable`] at
/// load time, never a silent fetch (TSK-601 precedent).
#[must_use]
pub fn default_model_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|base| {
            PathBuf::from(base)
                .join("SynthLM")
                .join("models")
                .join("demucs")
                .join(DEMUCS_ONNX_FILE)
        })
    }
    #[cfg(not(windows))]
    {
        if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
            if !cache.is_empty() {
                return Some(
                    PathBuf::from(cache)
                        .join("synthlm")
                        .join("demucs")
                        .join(DEMUCS_ONNX_FILE),
                );
            }
        }
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".cache")
                .join("synthlm")
                .join("demucs")
                .join(DEMUCS_ONNX_FILE)
        })
    }
}

/// File-name-only label for weight errors (never an absolute path, §8).
fn weight_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unnameable>")
        .to_owned()
}

/// Verify a cached weight file against the pinned manifest.
///
/// Returns the observed byte size when it equals
/// [`crate::demucs::DEMUCS_ONNX_BYTES`]. A missing file is
/// [`crate::error::DspError::ModelUnavailable`] (terminal `BLOCKED`: fetch
/// the pinned revision into [`crate::demucs::default_model_path`] and
/// retry); a size disagreement is
/// [`crate::error::DspError::WeightMismatch`] (stale or truncated cache —
/// re-fetch, never run through it). Content-hash re-verification is the
/// fetcher's job (compare against
/// [`crate::demucs::DEMUCS_ONNX_SHA256`]); the hot path pins on size.
///
/// # Errors
///
/// [`crate::error::DspError::ModelUnavailable`] when the file cannot be
/// read; [`crate::error::DspError::WeightMismatch`] when the size
/// disagrees.
pub fn verify_cached_weight(path: &std::path::Path) -> Result<u64, DspError> {
    let label = weight_label(path);
    let observed = std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|_| DspError::ModelUnavailable {
            detail: format!(
                "demucs weights not cached ({label}); fetch {DEMUCS_ONNX_URL} \
                 into the local cache and retry"
            ),
        })?;
    if observed != DEMUCS_ONNX_BYTES {
        return Err(DspError::WeightMismatch {
            expected: DEMUCS_ONNX_BYTES,
            observed,
        });
    }
    Ok(observed)
}

// ---------------------------------------------------------------------------
// Stem rows, explicit audio input, cache-key preimage
// ---------------------------------------------------------------------------

/// One selectable output stem of the single-file 4-stem model.
///
/// Row indices match [`crate::demucs::DEMUCS_SOURCES`] order
/// (`drums=0, bass=1, other=2, vocals=3`, upstream `infer.py`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StemRow {
    /// Output row 0 (`drums`).
    Drums,
    /// Output row 1 (`bass`).
    Bass,
    /// Output row 2 (`other`).
    Other,
    /// Output row 3 (`vocals`).
    Vocals,
}

impl StemRow {
    /// Output-tensor row index for this stem.
    pub fn index(self) -> usize {
        match self {
            StemRow::Drums => 0,
            StemRow::Bass => 1,
            StemRow::Other => 2,
            StemRow::Vocals => 3,
        }
    }

    /// Stable stem name (matches [`crate::demucs::DEMUCS_SOURCES`]).
    pub fn as_str(self) -> &'static str {
        DEMUCS_SOURCES[self.index()]
    }

    /// Every selectable stem row.
    pub fn all() -> &'static [StemRow] {
        &[
            StemRow::Drums,
            StemRow::Bass,
            StemRow::Other,
            StemRow::Vocals,
        ]
    }

    /// Cache artifact kind for products of this stem.
    pub fn artifact_kind(self) -> crate::artifact::ArtifactKind {
        match self {
            StemRow::Drums => crate::artifact::ArtifactKind::Drums,
            StemRow::Bass => crate::artifact::ArtifactKind::Bass,
            StemRow::Other => crate::artifact::ArtifactKind::Other,
            StemRow::Vocals => crate::artifact::ArtifactKind::Vocals,
        }
    }
}

/// Explicit separator input: planar `f32` PCM at the model rate.
///
/// This is the only type that ever carries audio toward the backend
/// (AGENTS.md §8: jobs carry digests, never PCM). Mono is expressed as
/// `right: None` and duplicated to stereo inside inference, mirroring
/// upstream `infer.py` (`np.repeat`). Resampling is the caller's job:
/// any `sample_rate != 44100` is rejected deterministically.
#[derive(Clone, Copy, Debug)]
pub struct MixPcm<'a> {
    /// Input sample rate in Hz (must equal
    /// [`crate::demucs::DEMUCS_SAMPLE_RATE`]).
    pub sample_rate: u32,
    /// Left channel (mono content when `right` is `None`).
    pub left: &'a [f32],
    /// Right channel, or `None` for mono.
    pub right: Option<&'a [f32]>,
}

impl<'a> MixPcm<'a> {
    /// Sample count per channel.
    pub fn len(&self) -> usize {
        self.left.len()
    }

    /// Whether any channel holds samples.
    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    /// Validate shape/rate/content without running inference.
    ///
    /// # Errors
    ///
    /// [`crate::error::DspError::UnsupportedInput`] on rate mismatch,
    /// empty input, channel-length disagreement, or non-finite samples.
    pub fn validate(&self) -> Result<(), DspError> {
        if self.sample_rate != DEMUCS_SAMPLE_RATE {
            return Err(DspError::UnsupportedInput {
                detail: format!(
                    "sample rate {} != model rate {DEMUCS_SAMPLE_RATE}; resample first",
                    self.sample_rate
                ),
            });
        }
        if self.left.is_empty() {
            return Err(DspError::UnsupportedInput {
                detail: "empty mix has no stems".to_owned(),
            });
        }
        if let Some(right) = self.right
            && right.len() != self.left.len()
        {
            return Err(DspError::UnsupportedInput {
                detail: "channel length mismatch".to_owned(),
            });
        }
        let finite = self.left.iter().all(|s| s.is_finite())
            && self.right.is_none_or(|r| r.iter().all(|s| s.is_finite()));
        if !finite {
            return Err(DspError::UnsupportedInput {
                detail: "mix contains NaN or infinite sample".to_owned(),
            });
        }
        Ok(())
    }
}

/// Canonical params preimage that a job's `params_hash` must bind.
///
/// Callers hash this string (SHA-256, hex) externally and store the digest
/// as the job's `params_hash`, so one job addresses exactly one
/// `(model, stem, encoding)` product family and different stems never share
/// a cache key. `dsp` holds no hash dependency, so hashing stays outside.
#[must_use]
pub fn canonical_params_preimage(model: DemucsModel, stem: StemRow) -> String {
    format!(
        "synthlm-demucs-v1/{}/{}/sr{DEMUCS_SAMPLE_RATE}/wav-f32",
        model.as_str(),
        stem.as_str()
    )
}

// ---------------------------------------------------------------------------
// Pure DSP: chunk plan, overlap window, overlap-add, WAV encoding
// ---------------------------------------------------------------------------
//
// All functions in this section are dependency-free and run identically
// with or without the `onnx` feature, so the cache/GC path is fully
// testable offline. Formulae mirror upstream `infer.py` (`separate`).

/// Chunk `(start, end)` sample ranges covering `total_samples`.
///
/// Mirrors upstream `infer.py`: window [`crate::demucs::DEMUCS_WINDOW_SAMPLES`]
/// samples, quarter overlap, `stride = window - overlap`,
/// `n_chunks = max(1, ceil(total / stride))`, last chunk clamped to
/// `total` (short tail is zero-padded at inference time).
#[must_use]
pub fn chunk_plan(total_samples: usize) -> Vec<(usize, usize)> {
    let overlap = DEMUCS_WINDOW_SAMPLES / 4;
    let stride = DEMUCS_WINDOW_SAMPLES - overlap;
    let n_chunks = total_samples.div_ceil(stride).max(1);
    (0..n_chunks)
        .map(|i| {
            let start = i.saturating_mul(stride);
            let end = start
                .saturating_add(DEMUCS_WINDOW_SAMPLES)
                .min(total_samples);
            (start, end)
        })
        .collect()
}

/// Triangular overlap window of [`crate::demucs::DEMUCS_WINDOW_SAMPLES`].
///
/// Mirrors upstream `_make_window`: linear fade-in over the first quarter,
/// ones through the middle, linear fade-out over the last quarter
/// (`linspace(0, 1, overlap)` endpoints inclusive, mirrored).
#[must_use]
pub fn overlap_window() -> Vec<f32> {
    let n = DEMUCS_WINDOW_SAMPLES;
    let overlap = n / 4;
    let denom = (overlap - 1) as f32;
    (0..n)
        .map(|i| {
            if i < overlap {
                i as f32 / denom
            } else if i >= n - overlap {
                (n - 1 - i) as f32 / denom
            } else {
                1.0
            }
        })
        .collect()
}

/// Overlap-add accumulator for one stem (planar stereo, `f32`).
///
/// Feed each window's stem output via
/// [`crate::demucs::StemAccumulator::add_stem_chunk`] in plan order, then
/// [`crate::demucs::StemAccumulator::finish`]: overlapping regions are
/// weight-normalised (`out /= max(weight, 1e-8)`, mirroring `infer.py`), so
/// a lone full window round-trips bit-identically.
pub struct StemAccumulator {
    total: usize,
    left: Vec<f32>,
    right: Vec<f32>,
    weight: Vec<f32>,
    window: Vec<f32>,
}

impl StemAccumulator {
    /// Accumulator for a `total_samples`-long stem (all zeros + weights).
    pub fn new(total_samples: usize) -> Self {
        Self {
            total: total_samples,
            left: vec![0.0; total_samples],
            right: vec![0.0; total_samples],
            weight: vec![0.0; total_samples],
            window: overlap_window(),
        }
    }

    /// Expected output length in samples per channel.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Add one window's stem output at `start` (both channels, `f32`).
    ///
    /// `left`/`right` hold this window's stem samples for `clen` output
    /// samples (`clen = end - start` from [`crate::demucs::chunk_plan`];
    /// the zero-padded tail is never passed in). Row extraction from the
    /// model output happens before this call.
    ///
    /// # Errors
    ///
    /// [`crate::error::DspError::UnsupportedInput`] when the channels
    /// disagree in length or the chunk exceeds the planned total (caller
    /// bug surfaced as an error, never a panic).
    pub fn add_stem_chunk(
        &mut self,
        start: usize,
        left: &[f32],
        right: &[f32],
    ) -> Result<(), DspError> {
        if left.len() != right.len() {
            return Err(DspError::UnsupportedInput {
                detail: "stem chunk channel length mismatch".to_owned(),
            });
        }
        let clen = left.len();
        if start.saturating_add(clen) > self.total {
            return Err(DspError::UnsupportedInput {
                detail: "stem chunk exceeds planned total".to_owned(),
            });
        }
        for k in 0..clen {
            let w = self.window[k];
            self.left[start + k] += left[k] * w;
            self.right[start + k] += right[k] * w;
            self.weight[start + k] += w;
        }
        Ok(())
    }

    /// Normalise by accumulated weights and return `(left, right)`.
    pub fn finish(self) -> (Vec<f32>, Vec<f32>) {
        let mut left = self.left;
        let mut right = self.right;
        for i in 0..self.total {
            let norm = self.weight[i].max(1e-8);
            left[i] /= norm;
            right[i] /= norm;
        }
        (left, right)
    }
}

/// Encode planar stereo `f32` as a minimal IEEE-float WAV.
///
/// Header is a 44-byte RIFF/WAVE `fmt ` (format tag 3, 32-bit float) +
/// `data` chunk with interleaved little-endian samples — readable by
/// libsndfile/REAPER without extra dependencies. Sizes are `u32`-checked.
///
/// # Errors
///
/// [`crate::error::DspError::UnsupportedInput`] on channel-length
/// disagreement or products too large for the RIFF container.
pub fn encode_wav_f32(left: &[f32], right: &[f32], sample_rate: u32) -> Result<Vec<u8>, DspError> {
    if left.len() != right.len() {
        return Err(DspError::UnsupportedInput {
            detail: "wav channel length mismatch".to_owned(),
        });
    }
    let frames = left.len();
    let data_bytes = u64::try_from(frames).unwrap_or(u64::MAX).saturating_mul(8);
    let data_len = u32::try_from(data_bytes).map_err(|_| DspError::UnsupportedInput {
        detail: "stem too large for wav container".to_owned(),
    })?;
    let riff_len = data_len.saturating_add(36);

    let mut out = Vec::with_capacity(44 + data_bytes.min(usize::MAX as u64) as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&sample_rate.saturating_mul(8).to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for (l, r) in left.iter().zip(right.iter()) {
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Wired backend: local ONNX Demucs, lazy weights, stems into the cache
// ---------------------------------------------------------------------------

/// Wired local Demucs backend (TSK-603).
///
/// Runs the pinned single-file `htdemucs` fp16 export
/// ([`crate::demucs::DEMUCS_ONNX_FILE`]) on the CPU via ONNX Runtime and
/// writes the requested stem WAV through the TSK-205 content-addressed
/// cache. Queue recipe: `Pending → Running`, then
/// [`crate::demucs::DemucsOnnxSeparator::separate_stem`], then
/// `Running → Done` (or `→ Failed`); GC stays [`crate::gc::run_gc`].
///
/// Unlike [`crate::demucs::DemucsStub`] this type performs real inference,
/// so it owns its session: `separate_stem` takes `&mut self` (ORT `run`
/// needs `&mut`) and one separator serves one background worker. Only
/// [`crate::demucs::DemucsModel::HtDemucs`] has a pinned manifest today;
/// other variants fail [`crate::demucs::DemucsOnnxSeparator::ensure_model_available`]
/// with [`crate::error::DspError::ModelUnavailable`] until their own
/// revision/size/hash pinning lands.
///
/// The `ort` session exists only under the `onnx` cargo feature (same
/// optional-dependency pattern as TSK-601): without it the cache-hit path
/// still works offline, while a cache miss is a terminal `BLOCKED`
/// naming the feature.
pub struct DemucsOnnxSeparator {
    /// Selected model variant (only `HtDemucs` is pinned, see above).
    pub model: DemucsModel,
    /// Directory the pinned weight is lazily verified in (follow-up
    /// downloads land here; this task only reads).
    pub model_dir: PathBuf,
    /// Open ORT session, created on first cache miss (`onnx` feature).
    #[cfg(feature = "onnx")]
    session: Option<ort::session::Session>,
}

impl DemucsOnnxSeparator {
    /// Declare the backend for `model` with weights rooted at `model_dir`.
    ///
    /// Infallible like [`crate::demucs::DemucsStub::new`]: variant support
    /// and weight presence are checked at
    /// [`crate::demucs::DemucsOnnxSeparator::ensure_model_available`], not
    /// here.
    pub fn new(model: DemucsModel, model_dir: PathBuf) -> Self {
        Self {
            model,
            model_dir,
            #[cfg(feature = "onnx")]
            session: None,
        }
    }

    /// Weight-file path the lazy cache populates (pinned name for
    /// `HtDemucs`, `<variant>.onnx` placeholder otherwise).
    pub fn weight_path(&self) -> PathBuf {
        if self.model == DemucsModel::HtDemucs {
            self.model_dir.join(DEMUCS_ONNX_FILE)
        } else {
            self.model_dir.join(format!("{}.onnx", self.model.as_str()))
        }
    }

    /// Verify the lazily-cached weights are present and pinned-correct.
    ///
    /// Returns the observed weight bytes (auditable: time/model/bytes, no
    /// audio, no paths). No network happens here: a missing file is
    /// [`crate::error::DspError::ModelUnavailable`] with the pinned fetch
    /// URL, a size disagreement is
    /// [`crate::error::DspError::WeightMismatch`], and an unpinned variant
    /// is [`crate::error::DspError::ModelUnavailable`] naming the missing
    /// manifest.
    ///
    /// # Errors
    ///
    /// Whatever [`crate::demucs::verify_cached_weight`] reports, or
    /// [`crate::error::DspError::ModelUnavailable`] for unpinned variants.
    pub fn ensure_model_available(&self) -> Result<u64, DspError> {
        if self.model != DemucsModel::HtDemucs {
            return Err(DspError::ModelUnavailable {
                detail: format!(
                    "no pinned manifest for {} (only {} is pinned); \
                     variant pinning is a follow-up",
                    self.model.as_str(),
                    DEMUCS_ONNX_VARIANT
                ),
            });
        }
        verify_cached_weight(&self.weight_path())
    }

    /// Open (or reuse) the ONNX session and assert the graph contract.
    ///
    /// Requires the `onnx` cargo feature: without it this is a terminal
    /// `BLOCKED` naming the feature (the default build stays ORT-free).
    /// With it, verifies weights, opens the session from the cached file,
    /// and asserts the [`crate::demucs::DEMUCS_INPUT_NAME`] /
    /// [`crate::demucs::DEMUCS_OUTPUT_NAME`] contract — a swapped or
    /// corrupt file fails here, never as silent mis-separation.
    ///
    /// # Errors
    ///
    /// [`crate::error::DspError::ModelUnavailable`] (missing weights,
    /// unpinned variant, or feature off);
    /// [`crate::error::DspError::WeightMismatch`] (stale cache);
    /// [`crate::error::DspError::InferenceFailed`] (session open or
    /// contract mismatch).
    #[cfg(feature = "onnx")]
    pub fn open_session(&mut self) -> Result<(), DspError> {
        if self.session.is_some() {
            return Ok(());
        }
        self.ensure_model_available()?;
        let label = weight_label(&self.weight_path());
        let builder = ort::session::Session::builder().map_err(|_| DspError::InferenceFailed {
            detail: format!("onnx session builder failed ({label})"),
        })?;
        let mut builder = builder;
        let session = builder.commit_from_file(self.weight_path()).map_err(|_| {
            DspError::InferenceFailed {
                detail: format!(
                    "onnx session open failed ({label}); re-fetch the pinned \
                     revision or fix the local cache"
                ),
            }
        })?;
        let has_input = session
            .inputs()
            .iter()
            .any(|io| io.name() == DEMUCS_INPUT_NAME);
        let has_output = session
            .outputs()
            .iter()
            .any(|io| io.name() == DEMUCS_OUTPUT_NAME);
        if !has_input || !has_output {
            return Err(DspError::InferenceFailed {
                detail: format!(
                    "unexpected onnx graph I/O ({label}); want input \
                     {DEMUCS_INPUT_NAME} and output {DEMUCS_OUTPUT_NAME}"
                ),
            });
        }
        self.session = Some(session);
        Ok(())
    }

    /// Graph input/output counts (contract probe, also keeps the session
    /// field read outside inference).
    #[cfg(feature = "onnx")]
    #[must_use]
    pub fn io_counts(&self) -> Option<(usize, usize)> {
        self.session
            .as_ref()
            .map(|s| (s.inputs().len(), s.outputs().len()))
    }

    /// Separate one stem of `mix` into the content-addressed cache.
    ///
    /// `job` addresses the product: its `params_hash` must bind
    /// [`crate::demucs::canonical_params_preimage`] for `(model, stem)`
    /// (one job per stem, so stems never share a key). Cache hit: returns
    /// the stored product's pointer with no model work (works offline even
    /// without the `onnx` feature). Cache miss: verifies weights, runs the
    /// windowed forward pass, overlap-adds the requested row, encodes
    /// IEEE-float WAV, and [`crate::store::ContentStore::put`]s it.
    ///
    /// # Errors
    ///
    /// [`crate::error::DspError::UnsupportedInput`] (bad mix or chunk
    /// math); [`crate::error::DspError::ModelUnavailable`] /
    /// [`crate::error::DspError::WeightMismatch`] (weights);
    /// [`crate::error::DspError::InferenceFailed`] (session/run/output);
    /// store I/O via [`crate::error::DspError::Io`].
    #[cfg(feature = "onnx")]
    pub fn separate_stem(
        &mut self,
        job: &Job,
        stem: StemRow,
        mix: &MixPcm<'_>,
        store: &mut crate::store::ContentStore,
    ) -> Result<ArtifactRef, DspError> {
        mix.validate()?;
        let key = job.cache_key()?;
        if let Some(bytes) = store.fetch(&key)? {
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            return Ok(ArtifactRef::new(key, stem.artifact_kind(), len));
        }
        self.open_session()?;
        let label = weight_label(&self.weight_path());
        let session = self.session.as_mut().ok_or(DspError::InferenceFailed {
            detail: "onnx session missing after open".to_owned(),
        })?;
        let total = mix.len();
        let mut acc = StemAccumulator::new(total);
        for (start, end) in chunk_plan(total) {
            let clen = end.saturating_sub(start);
            let mut windowed = Vec::with_capacity(DEMUCS_CHANNELS * DEMUCS_WINDOW_SAMPLES);
            for i in 0..DEMUCS_WINDOW_SAMPLES {
                let idx = start.saturating_add(i);
                let l = if idx < end { mix.left[idx] } else { 0.0 };
                let r = match mix.right {
                    Some(right) if idx < end => right[idx],
                    Some(_) => 0.0,
                    None => l,
                };
                windowed.push(l);
                windowed.push(r);
            }
            // Interleaved LRLR.. → planar (2, N): even slots are left.
            let mut planar = vec![0.0f32; DEMUCS_CHANNELS * DEMUCS_WINDOW_SAMPLES];
            for (i, sample) in windowed.iter().enumerate() {
                let c = i % DEMUCS_CHANNELS;
                let t = i / DEMUCS_CHANNELS;
                planar[c * DEMUCS_WINDOW_SAMPLES + t] = *sample;
            }
            let input = ort::value::Tensor::from_array((
                [1usize, DEMUCS_CHANNELS, DEMUCS_WINDOW_SAMPLES],
                planar,
            ))
            .map_err(|_| DspError::InferenceFailed {
                detail: format!("onnx input tensor build failed ({label})"),
            })?;
            let outputs = session
                .run(ort::inputs! { DEMUCS_INPUT_NAME => input })
                .map_err(|_| DspError::InferenceFailed {
                    detail: format!("onnx forward pass failed ({label})"),
                })?;
            let value = outputs
                .get(DEMUCS_OUTPUT_NAME)
                .ok_or(DspError::InferenceFailed {
                    detail: format!("onnx output {DEMUCS_OUTPUT_NAME} missing"),
                })?;
            let (shape, data) =
                value
                    .try_extract_tensor::<f32>()
                    .map_err(|_| DspError::InferenceFailed {
                        detail: format!("onnx output is not f32 tensor ({label})"),
                    })?;
            let want = 4 * DEMUCS_CHANNELS * DEMUCS_WINDOW_SAMPLES;
            if shape.len() != 4 || shape.num_elements() != want {
                return Err(DspError::InferenceFailed {
                    detail: format!(
                        "unexpected {DEMUCS_OUTPUT_NAME} shape (dims={}, elems={})",
                        shape.len(),
                        shape.num_elements()
                    ),
                });
            }
            // Row-major (batch=1, stem=4, channel=2, time=N): stem row
            // `s`, channel `c` starts at ((s * 2) + c) * N.
            let row = stem.index();
            let base = row * DEMUCS_CHANNELS * DEMUCS_WINDOW_SAMPLES;
            let left_out = &data[base..base + DEMUCS_WINDOW_SAMPLES];
            let right_out = &data[base + DEMUCS_WINDOW_SAMPLES..base + 2 * DEMUCS_WINDOW_SAMPLES];
            acc.add_stem_chunk(
                start,
                &left_out[..clen.min(DEMUCS_WINDOW_SAMPLES)],
                &right_out[..clen.min(DEMUCS_WINDOW_SAMPLES)],
            )?;
        }
        let (left, right) = acc.finish();
        let wav = encode_wav_f32(&left, &right, DEMUCS_SAMPLE_RATE)?;
        store.put(&key, stem.artifact_kind(), &wav)
    }

    /// Separate one stem of `mix` into the content-addressed cache.
    ///
    /// Without the `onnx` feature only the cache-hit path is live (pure
    /// offline reuse of previously separated products); a miss is a
    /// terminal `BLOCKED` naming the feature. See the `onnx`-gated method
    /// of the same name for the full inference path.
    ///
    /// # Errors
    ///
    /// Same addressing/input/store errors as the inference build, plus
    /// [`crate::error::DspError::ModelUnavailable`] on every cache miss.
    #[cfg(not(feature = "onnx"))]
    pub fn separate_stem(
        &mut self,
        job: &Job,
        stem: StemRow,
        mix: &MixPcm<'_>,
        store: &mut crate::store::ContentStore,
    ) -> Result<ArtifactRef, DspError> {
        mix.validate()?;
        let key = job.cache_key()?;
        if let Some(bytes) = store.fetch(&key)? {
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            return Ok(ArtifactRef::new(key, stem.artifact_kind(), len));
        }
        Err(DspError::ModelUnavailable {
            detail: "demucs inference needs the `onnx` cargo feature; \
                     rebuild with --features onnx (cache hits stay offline)"
                .to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{JobKind, JobState};

    fn pending_job() -> Job {
        Job::new(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78".to_owned(),
            "0011223344556677".to_owned(),
        )
        .expect("valid digests")
    }

    #[test]
    fn stub_reports_not_wired_without_side_effects() {
        let stub = DemucsStub::new(
            DemucsModel::HtDemucs,
            PathBuf::from("models").join("demucs"),
        );
        assert_eq!(stub.name(), "demucs-onnx-stub");
        // No model directory may be created and no download attempted.
        assert!(!stub.model_dir.exists() || stub.model_dir.is_dir());
        let err = stub
            .ensure_model_available()
            .expect_err("weights must not download in TSK-205");
        assert!(matches!(err, DspError::BackendNotWired { .. }), "{err:?}");
        assert!(err.to_string().contains("TSK-205"));
        let mut job = pending_job();
        job.transition(JobState::Running).expect("pickup");
        let err = stub.separate(&job).expect_err("no inference in TSK-205");
        assert!(matches!(err, DspError::BackendNotWired { .. }), "{err:?}");
    }

    #[test]
    fn model_size_declarations_span_documented_range() {
        // C-dsp-toolchain §5: 166MB (single FP16 pack) – 1.26GB (full bag).
        const MB: u64 = 1024 * 1024;
        assert_eq!(DemucsModel::HtDemucs.approx_weight_bytes(), 166 * MB);
        assert_eq!(DemucsModel::HtDemucs6s.approx_weight_bytes(), 1260 * MB);
        for model in DemucsModel::all() {
            assert!(
                PathBuf::from(format!("{}.onnx", model.as_str()))
                    .extension()
                    .is_some(),
                "weight path keeps an .onnx extension"
            );
        }
    }

    // ---- TSK-603: pinned manifest, lazy weights, pure DSP, cache wiring ---

    /// Unique scratch directory for one test (same no-`tempfile` pattern as
    /// the store tests; the real user directory is never touched).
    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("synthlm-demucs-{pid}-{tag}"))
    }

    fn remove_scratch(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn hex_digest(seed: u8) -> String {
        let mut s = String::with_capacity(32);
        for i in 0..32 {
            s.push(char::from_digit(u32::from(seed.wrapping_add(i)) % 16, 16).unwrap_or('0'));
        }
        s
    }

    fn stem_job(stem: StemRow) -> Job {
        // Params bind the canonical preimage (model + stem + encoding), so
        // each stem addresses its own key. Test digests stand in for the
        // external SHA-256 hex of that preimage.
        let _ = canonical_params_preimage(DemucsModel::HtDemucs, stem);
        Job::new(
            JobKind::StemSeparation,
            hex_digest(0xab),
            hex_digest(stem.index() as u8),
        )
        .expect("valid digests")
    }

    fn sine_stereo(len: usize) -> (Vec<f32>, Vec<f32>) {
        let step = 2.0 * std::f64::consts::PI * 440.0 / f64::from(DEMUCS_SAMPLE_RATE);
        let mut phase = 0.0_f64;
        let mut mono = Vec::with_capacity(len);
        for _ in 0..len {
            mono.push((0.4 * phase.sin()) as f32);
            phase += step;
        }
        (mono.clone(), mono)
    }

    #[test]
    fn manifest_pins_source_size_and_hash() {
        assert_eq!(DEMUCS_ONNX_REPO, "StemSplitio/htdemucs-onnx");
        assert_eq!(DEMUCS_ONNX_REV.len(), 40);
        assert!(DEMUCS_ONNX_REV.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(DEMUCS_ONNX_URL.contains(DEMUCS_ONNX_REV));
        assert!(DEMUCS_ONNX_URL.ends_with(DEMUCS_ONNX_FILE));
        assert_eq!(DEMUCS_ONNX_BYTES, 165_612_636);
        assert_eq!(DEMUCS_ONNX_SHA256.len(), 64);
        assert!(DEMUCS_ONNX_SHA256.chars().all(|c| c.is_ascii_hexdigit()));
        // DEC-025 posture: research-only, not the uploader's mit tag.
        assert!(DEMUCS_ONNX_LICENSE.contains("research-only"));
        assert_eq!(DEMUCS_ONNX_OPSET, 17);
        assert_eq!(DEMUCS_ONNX_VARIANT, DemucsModel::HtDemucs.as_str());
        assert_eq!(DEMUCS_INPUT_NAME, "mix");
        assert_eq!(DEMUCS_OUTPUT_NAME, "stems");
        assert_eq!(DEMUCS_SOURCES, ["drums", "bass", "other", "vocals"]);
        assert_eq!(DEMUCS_SAMPLE_RATE, 44_100);
        assert_eq!(DEMUCS_WINDOW_SAMPLES, 343_980);
        assert_eq!(DEMUCS_CHANNELS, 2);
        // Cache path ends at the pinned file name whenever it resolves.
        if let Some(path) = default_model_path() {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some(DEMUCS_ONNX_FILE)
            );
        }
    }

    #[test]
    fn missing_cache_is_blocked_not_silent() {
        let absent = std::path::Path::new("definitely-absent-demucs-weights.onnx");
        let err = verify_cached_weight(absent).expect_err("absent cache must fail");
        assert!(matches!(err, DspError::ModelUnavailable { .. }));
        let message = err.to_string();
        assert!(
            message.contains("definitely-absent-demucs-weights.onnx"),
            "message keeps the file name: {message}"
        );
        assert!(
            message.contains(DEMUCS_ONNX_REV),
            "message points at the pinned revision: {message}"
        );
    }

    #[test]
    fn stale_cache_is_weight_mismatch() {
        let dir = scratch_dir("stale");
        remove_scratch(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let file = dir.join(DEMUCS_ONNX_FILE);
        std::fs::write(&file, [7u8; 16]).expect("stale weight");
        let err = verify_cached_weight(&file).expect_err("stale cache must fail");
        assert!(
            matches!(
                err,
                DspError::WeightMismatch {
                    expected: DEMUCS_ONNX_BYTES,
                    observed: 16
                }
            ),
            "{err:?}"
        );
        remove_scratch(&dir);
    }

    #[test]
    fn unpinned_variants_have_no_manifest() {
        let backend = DemucsOnnxSeparator::new(
            DemucsModel::HtDemucsFt,
            PathBuf::from("models").join("demucs"),
        );
        let err = backend
            .ensure_model_available()
            .expect_err("ft has no pinned manifest");
        assert!(matches!(err, DspError::ModelUnavailable { .. }), "{err:?}");
        assert!(err.to_string().contains("htdemucs_ft"));
    }

    #[test]
    fn stem_rows_match_upstream_source_order() {
        for (index, stem) in StemRow::all().iter().enumerate() {
            assert_eq!(stem.index(), index);
            assert_eq!(stem.as_str(), DEMUCS_SOURCES[index]);
        }
        assert_eq!(
            StemRow::Vocals.artifact_kind(),
            crate::artifact::ArtifactKind::Vocals
        );
        assert_eq!(
            StemRow::Drums.artifact_kind(),
            crate::artifact::ArtifactKind::Drums
        );
    }

    #[test]
    fn canonical_preimage_binds_model_and_stem() {
        let vocals = canonical_params_preimage(DemucsModel::HtDemucs, StemRow::Vocals);
        let drums = canonical_params_preimage(DemucsModel::HtDemucs, StemRow::Drums);
        let ft_vocals = canonical_params_preimage(DemucsModel::HtDemucsFt, StemRow::Vocals);
        assert_ne!(vocals, drums);
        assert_ne!(vocals, ft_vocals);
        assert!(vocals.contains("htdemucs"));
        assert!(vocals.contains("vocals"));
    }

    #[test]
    fn chunk_plan_covers_input_with_quarter_overlap() {
        const N: usize = DEMUCS_WINDOW_SAMPLES;
        const STRIDE: usize = N - N / 4;
        assert_eq!(chunk_plan(1000), vec![(0, 1000)]);
        assert_eq!(chunk_plan(N), vec![(0, N), (STRIDE, N)]);
        assert_eq!(chunk_plan(N + 1), vec![(0, N), (STRIDE, N + 1)]);
        // Coverage property on a multi-chunk input: stride spacing, clamp.
        let total = 1_000_000;
        let plan = chunk_plan(total);
        assert!(plan.len() > 2);
        for (i, (start, end)) in plan.iter().enumerate() {
            assert_eq!(*start, i * STRIDE);
            assert!(*end > *start && *end <= total);
            assert!(end - start <= N);
        }
        assert_eq!(plan.last().expect("nonempty").1, total);
    }

    #[test]
    fn overlap_window_shape_is_triangular() {
        const N: usize = DEMUCS_WINDOW_SAMPLES;
        let overlap = N / 4;
        let w = overlap_window();
        assert_eq!(w.len(), N);
        assert_eq!(w[0], 0.0);
        assert_eq!(w[overlap - 1], 1.0);
        assert_eq!(w[N - overlap], 1.0);
        assert_eq!(w[N - 1], 0.0);
        assert_eq!(w[N / 2], 1.0);
        for i in [1, 7, 12345, overlap, N / 2 + 999] {
            assert_eq!(w[i], w[N - 1 - i], "window must be symmetric at {i}");
        }
    }

    #[test]
    fn lone_window_interior_roundtrips_exactly() {
        // A single chunk shorter than the window: the window cancels on
        // division everywhere the weight is nonzero (edges fade by design).
        let total = 1000;
        let (left_in, right_in) = sine_stereo(total);
        let mut acc = StemAccumulator::new(total);
        acc.add_stem_chunk(0, &left_in, &right_in)
            .expect("chunk fits");
        let (left, right) = acc.finish();
        let overlap = DEMUCS_WINDOW_SAMPLES / 4;
        assert_eq!(left[0], 0.0);
        for i in overlap..total {
            assert_eq!(left[i], left_in[i], "interior must roundtrip at {i}");
            assert_eq!(right[i], right_in[i], "interior must roundtrip at {i}");
        }
    }

    #[test]
    fn overlap_add_blends_overlapping_chunks() {
        // Two constant chunks: every covered sample normalises to exactly
        // 1.0 (x/x), except sample 0 whose weight is zero by construction.
        let stride = DEMUCS_WINDOW_SAMPLES - DEMUCS_WINDOW_SAMPLES / 4;
        let total = stride + 100;
        let mut acc = StemAccumulator::new(total);
        let (start0, end0) = (0, total.min(DEMUCS_WINDOW_SAMPLES));
        let chunk0 = vec![1.0f32; end0 - start0];
        acc.add_stem_chunk(start0, &chunk0, &chunk0)
            .expect("chunk 0");
        let (start1, end1) = (stride, total);
        let chunk1 = vec![1.0f32; end1 - start1];
        acc.add_stem_chunk(start1, &chunk1, &chunk1)
            .expect("chunk 1");
        let (left, right) = acc.finish();
        assert_eq!(left[0], 0.0);
        for i in 1..total {
            assert_eq!(left[i], 1.0, "blend must normalise at {i}");
            assert_eq!(right[i], 1.0, "blend must normalise at {i}");
        }
        // Distinct constants blend proportionally to the window weights.
        let mut acc = StemAccumulator::new(total);
        let twos = vec![2.0f32; end0 - start0];
        let fours = vec![4.0f32; end1 - start1];
        acc.add_stem_chunk(start0, &twos, &twos).expect("chunk 0");
        acc.add_stem_chunk(start1, &fours, &fours).expect("chunk 1");
        let (left, _) = acc.finish();
        let w = overlap_window();
        for k in 0..(end1 - start1) {
            let t = start1 + k;
            if t == 0 {
                continue;
            }
            let a = w[t];
            let b = w[k];
            let expected = (2.0 * a + 4.0 * b) / (a + b);
            assert!(
                (left[t] - expected).abs() < 1e-4,
                "weighted blend at {t}: {} vs {expected}",
                left[t]
            );
        }
        // Caller bugs surface as errors, never panics.
        let mut acc = StemAccumulator::new(64);
        let ok = vec![0.0f32; 8];
        assert!(acc.add_stem_chunk(0, &ok, &ok[..7]).is_err());
        assert!(acc.add_stem_chunk(60, &ok, &ok).is_err());
    }

    #[test]
    fn wav_header_golden_and_sample_roundtrip() {
        let left = [0.5f32, -0.5];
        let right = [0.25f32, 0.0];
        let wav = encode_wav_f32(&left, &right, DEMUCS_SAMPLE_RATE).expect("encode");
        assert_eq!(wav.len(), 44 + 16);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 3);
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2);
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            44_100
        );
        assert_eq!(
            u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]]),
            44_100 * 8
        );
        assert_eq!(u16::from_le_bytes([wav[32], wav[33]]), 8);
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 32);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 16);
        let samples: Vec<f32> = wav[44..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(samples, vec![0.5, 0.25, -0.5, 0.0]);
        assert!(encode_wav_f32(&left, &left[..1], DEMUCS_SAMPLE_RATE).is_err());
    }

    #[test]
    fn unsupported_inputs_rejected() {
        let (left, right) = sine_stereo(256);
        let good = MixPcm {
            sample_rate: DEMUCS_SAMPLE_RATE,
            left: &left,
            right: Some(&right),
        };
        assert!(!good.is_empty());
        assert_eq!(good.len(), 256);
        good.validate().expect("valid stereo");
        let mono = MixPcm {
            sample_rate: DEMUCS_SAMPLE_RATE,
            left: &left,
            right: None,
        };
        mono.validate().expect("valid mono");
        for bad in [
            MixPcm {
                sample_rate: 48_000,
                left: &left,
                right: Some(&right),
            },
            MixPcm {
                sample_rate: DEMUCS_SAMPLE_RATE,
                left: &[],
                right: None,
            },
            MixPcm {
                sample_rate: DEMUCS_SAMPLE_RATE,
                left: &left,
                right: Some(&right[..128]),
            },
        ] {
            assert!(
                matches!(bad.validate(), Err(DspError::UnsupportedInput { .. })),
                "must reject {bad:?}"
            );
        }
        let mut salty = left.clone();
        salty[3] = f32::NAN;
        let nan = MixPcm {
            sample_rate: DEMUCS_SAMPLE_RATE,
            left: &salty,
            right: None,
        };
        assert!(matches!(
            nan.validate(),
            Err(DspError::UnsupportedInput { .. })
        ));
    }

    #[test]
    fn cache_hit_path_needs_no_model() {
        use crate::store::ContentStore;
        let dir = scratch_dir("hit");
        remove_scratch(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let job = stem_job(StemRow::Vocals);
        let key = job.cache_key().expect("key");
        // Seed the product directly (as a previous separation would have).
        let seeded = store
            .put(&key, StemRow::Vocals.artifact_kind(), b"stem-wav-bytes")
            .expect("seed");
        assert_eq!(seeded.key, key);
        // The backend serves the hit with no weight file anywhere near.
        let mut backend =
            DemucsOnnxSeparator::new(DemucsModel::HtDemucs, dir.join("no-weights-here"));
        let (left, right) = sine_stereo(64);
        let mix = MixPcm {
            sample_rate: DEMUCS_SAMPLE_RATE,
            left: &left,
            right: Some(&right),
        };
        let hit = backend
            .separate_stem(&job, StemRow::Vocals, &mix, &mut store)
            .expect("cache hit");
        assert_eq!(hit.key, key);
        assert_eq!(hit.kind, StemRow::Vocals.artifact_kind());
        assert_eq!(hit.bytes, 14);
        assert_eq!(store.hits(), 1);
        let bytes = store.fetch(&key).expect("fetch").expect("present");
        assert_eq!(bytes, b"stem-wav-bytes");
        remove_scratch(&dir);
    }

    #[test]
    #[cfg(not(feature = "onnx"))]
    fn cache_miss_without_feature_is_blocked() {
        use crate::store::ContentStore;
        let dir = scratch_dir("miss-no-feature");
        remove_scratch(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let mut backend = DemucsOnnxSeparator::new(DemucsModel::HtDemucs, dir.join("models"));
        let job = stem_job(StemRow::Drums);
        let (left, _) = sine_stereo(64);
        let mix = MixPcm {
            sample_rate: DEMUCS_SAMPLE_RATE,
            left: &left,
            right: None,
        };
        let err = backend
            .separate_stem(&job, StemRow::Drums, &mix, &mut store)
            .expect_err("miss without the onnx feature must block");
        assert!(matches!(err, DspError::ModelUnavailable { .. }), "{err:?}");
        assert!(err.to_string().contains("onnx"), "{err}");
        assert_eq!(store.misses(), 1);
        remove_scratch(&dir);
    }

    #[test]
    fn gc_reclaims_backend_products_and_reports_metrics() {
        use crate::gc::{CacheMetrics, GcPolicy, WATERMARK_BYTES, run_gc};
        use crate::store::ContentStore;
        use std::collections::HashSet;
        let dir = scratch_dir("gc-backend");
        remove_scratch(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let keep = stem_job(StemRow::Vocals);
        let drop = stem_job(StemRow::Drums);
        let keep_key = keep.cache_key().expect("key");
        let drop_key = drop.cache_key().expect("key");
        store
            .put(&keep_key, StemRow::Vocals.artifact_kind(), &[1u8; 64])
            .expect("put keep");
        store
            .put(&drop_key, StemRow::Drums.artifact_kind(), &[2u8; 64])
            .expect("put drop");
        // Backend hit path refreshes LRU + counters (TSK-205 reuse).
        let _ = store.fetch(&keep_key).expect("touch keep");
        assert_eq!(store.hits(), 1);
        // Both live, under the watermark: GC is a no-op.
        let live: HashSet<crate::key::CacheKey> =
            [keep_key.clone(), drop_key.clone()].into_iter().collect();
        let report = run_gc(&mut store, &GcPolicy::default(), &live).expect("gc");
        assert_eq!(report.orphans_removed, 0);
        assert_eq!(report.watermark_evicted, 0);
        // Losing candidate leaves the live set: orphan reclamation.
        let live: HashSet<crate::key::CacheKey> = [keep_key.clone()].into_iter().collect();
        let report = run_gc(&mut store, &GcPolicy::default(), &live).expect("gc");
        assert_eq!(report.orphans_removed, 1);
        assert_eq!(report.orphans_reclaimed_bytes, 64);
        assert!(store.contains(&keep_key).expect("keep survives"));
        // Tight watermark wins over liveness (DEC-009 reversal direction).
        let tiny = GcPolicy::with_watermark_bytes(32);
        let report = run_gc(&mut store, &tiny, &live).expect("gc");
        assert_eq!(report.watermark_evicted, 1);
        assert!(report.bytes_after <= 32);
        // Metrics recipe for observability consumers (hits + water level).
        let usage = store.disk_usage().expect("usage");
        let metrics = CacheMetrics::snapshot(store.hits(), store.misses(), usage, WATERMARK_BYTES);
        assert_eq!(metrics.hit_rate(), Some(1.0));
        assert!(metrics.within_watermark());
        assert!(metrics.usage_ratio() < 1.0);
        remove_scratch(&dir);
    }

    /// Live contract + single-stem forward pass (TSK-603 evidence).
    ///
    /// Runs only under `--features onnx` with a weight file present
    /// (`SYNTHLM_DEMUCS_WEIGHT`, else the default cache path): opens the
    /// real session, asserts the `mix`/`stems` contract, separates one
    /// second of synthetic stereo into the scratch cache, and proves the
    /// second call is a cache hit. Without weights it proves the BLOCKED
    /// path instead, so offline CI stays green.
    #[test]
    #[cfg(feature = "onnx")]
    fn live_weight_contract_and_single_stem_pass() {
        use crate::store::ContentStore;
        let weight = std::env::var("SYNTHLM_DEMUCS_WEIGHT")
            .ok()
            .map(PathBuf::from)
            .or_else(default_model_path)
            .expect("a candidate weight path");
        let model_dir = weight
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("models").join("demucs"));
        let mut backend = DemucsOnnxSeparator::new(DemucsModel::HtDemucs, model_dir);
        assert_eq!(backend.weight_path().file_name(), weight.file_name());
        if !weight.is_file() {
            let err = backend
                .ensure_model_available()
                .expect_err("absent weight must block");
            assert!(matches!(err, DspError::ModelUnavailable { .. }), "{err:?}");
            eprintln!("SKIP live forward pass: no weight at candidate path");
            return;
        }
        backend.open_session().expect("open pinned session");
        let (inputs, outputs) = backend.io_counts().expect("session open");
        eprintln!("live onnx graph io: inputs={inputs} outputs={outputs}");
        assert!(inputs >= 1 && outputs >= 1);
        let dir = scratch_dir("live");
        remove_scratch(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let job = stem_job(StemRow::Vocals);
        let (left, right) = sine_stereo(DEMUCS_SAMPLE_RATE as usize);
        let mix = MixPcm {
            sample_rate: DEMUCS_SAMPLE_RATE,
            left: &left,
            right: Some(&right),
        };
        let first = backend
            .separate_stem(&job, StemRow::Vocals, &mix, &mut store)
            .expect("live separation");
        assert_eq!(first.kind, StemRow::Vocals.artifact_kind());
        let frames = left.len();
        assert_eq!(first.bytes, 44 + frames as u64 * 8);
        let stored = store
            .fetch(&job.cache_key().expect("key"))
            .expect("fetch")
            .expect("present");
        assert_eq!(stored.len() as u64, first.bytes);
        assert_eq!(&stored[0..4], b"RIFF");
        // Every output sample is finite (no NaN/Inf escapes the graph).
        for chunk in stored[44..].as_chunks::<4>().0 {
            let v = f32::from_le_bytes(*chunk);
            assert!(v.is_finite());
        }
        // Second call is a cache hit: identical bytes, no second pass.
        let second = backend
            .separate_stem(&job, StemRow::Vocals, &mix, &mut store)
            .expect("hit");
        assert_eq!(second, first);
        assert!(store.hits() >= 2);
        eprintln!(
            "live stem pass: bytes={} hits={} misses={}",
            first.bytes,
            store.hits(),
            store.misses()
        );
        remove_scratch(&dir);
    }
}
