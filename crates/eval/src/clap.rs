//! Local CLAP backends: deterministic spectral proxy (wired) + ONNX seam.
//!
//! [`crate::score::ClapEmbedder`] is the trait seam; this module provides the
//! two backends behind it:
//!
//! - [`crate::clap::SpectralClapEmbedder`] (default, always compiled): a pure-Rust,
//!   dependency-free 512-dimensional embedding. It RMS-normalises the input
//!   first, so a pure gain change (the DEC-016 `+6dB` cheat) maps to the same
//!   direction and [`crate::score::cosine_similarity`] reports `~1.0`. No
//!   network, no audio leaves the process (AGENTS.md `§8`).
//! - `OnnxClapEmbedder` (opt-in `onnx` cargo feature, `ort`-backed): opens the
//!   pinned LAION-family ONNX audio encoder from the local weight cache and
//!   validates its graph contract. Full ONNX inference stays behind the HTSAT
//!   mel-preprocessing calibration spike (see [`crate::clap::CLAP_ONNX_URL`]);
//!   the seam never emits guessed numbers.
//!
//! End-to-end scoring with a real cosine goes through
//! [`crate::clap::score_pair`]; the bare [`crate::score::compare`] stays
//! embedder-free and keeps returning `clap_cos: None` (it never sees audio
//! samples). Candidate-distance dedup lives in
//! [`crate::clap::dedup_by_clap`], mirroring the planner `diversify` greedy
//! semantics (score-ordered, radius collapse, non-finite threshold falls back
//! to [`crate::clap::DEFAULT_CLAP_DEDUP_DISTANCE`]).
//!
//! All analysis runs off the DAW threads (L8); nothing here touches REAPER,
//! the audio thread, or the network. The only file reads are explicit local
//! weight-cache checks ([`crate::clap::verify_cached_weight`]), never audio
//! uploads.

use std::path::{Path, PathBuf};

use crate::EvalError;
use crate::mir::{MirParams, analyze};
use crate::score::{ClapEmbedder, Score, clap_cosine, compare_with_clap, cosine_similarity};

/// Canonical CLAP embedding width (LAION `projection_dim = 512`).
///
/// Matches the documentary `CLAP_DIM` in the retrieval crate without
/// depending on it (DEC-022: `eval` and `retrieval` are siblings, so the
/// constant is duplicated rather than shared).
pub const CLAP_DIM: usize = 512;

/// Fingerprint resample target in samples.
///
/// Embeddings compare variable-length inputs by first resampling them onto
/// this fixed grid (endpoint-pinned linear interpolation, the same
/// convention as [`crate::mir::resample_frames`]), so one second and two
/// seconds of the same timbre land in the same direction.
pub const CLAP_FINGERPRINT_SAMPLES: usize = 8192;

/// Fingerprint frame count (`CLAP_FRAMES * CLAP_BINS_PER_FRAME == CLAP_DIM`).
pub const CLAP_FRAMES: usize = 32;

/// DCT coefficients kept per fingerprint frame.
pub const CLAP_BINS_PER_FRAME: usize = 16;

/// Default dedup radius in CLAP cosine-distance units (`1 - cosine`).
///
/// Calibrated so near-duplicate renders (per-sample drift around `1e-3`)
/// collapse while clearly separated timbres survive; see the
/// `homogeneous_*` / `distant_*` tests in this module and
/// `tests/clap_wiring.rs`. Mirrors the role of the planner
/// `DEFAULT_DEDUP_DISTANCE`, but the units are cosine distance, not
/// parameter Euclidean distance, so the numbers are not interchangeable.
pub const DEFAULT_CLAP_DEDUP_DISTANCE: f64 = 0.02;

// ---------------------------------------------------------------------------
// Pinned ONNX weight manifest (LAION family, TSK-601)
// ---------------------------------------------------------------------------

/// ONNX audio-encoder repo (community export of the LAION encoder).
pub const CLAP_ONNX_REPO: &str = "lquint/clap-htsat-unfused-onnx";

/// Pinned repo revision (HF API, verified 2026-10-07).
pub const CLAP_ONNX_REV: &str = "b31e0c5b0737a45ca1b04b8d151bed48afd78fcc";

/// Weight file inside the repo.
pub const CLAP_ONNX_FILE: &str = "model.onnx";

/// Pinned download source (revision-qualified; re-verify with
/// `curl -sI -L <url>` and compare `X-Linked-Size` / `X-Linked-ETag`).
pub const CLAP_ONNX_URL: &str = "https://huggingface.co/lquint/clap-htsat-unfused-onnx/resolve/b31e0c5b0737a45ca1b04b8d151bed48afd78fcc/model.onnx";

/// Expected weight size in bytes (`X-Linked-Size`, HEAD 2026-10-07).
pub const CLAP_ONNX_BYTES: u64 = 119_654_416;

/// Expected content SHA256 (`X-Linked-ETag`, HEAD 2026-10-07).
pub const CLAP_ONNX_SHA256: &str =
    "0763d8c6d03fe1675a3905b96ae3ff9ebfe316e3c0af9b64f8658ece57f0d5a5";

/// Weight license (HF model page, verified 2026-10-07).
pub const CLAP_ONNX_LICENSE: &str = "Apache-2.0";

/// Export opset (export script + model card, verified 2026-10-07).
pub const CLAP_ONNX_OPSET: u32 = 18;

/// Base PyTorch model the export wraps.
pub const CLAP_ONNX_BASE_MODEL: &str = "laion/clap-htsat-unfused";

/// ONNX input holding `[batch, 1, time, 64]` mel features.
pub const CLAP_ONNX_INPUT_FEATURES: &str = "input_features";

/// ONNX input holding the `[batch, 1]` over-10-seconds flag.
pub const CLAP_ONNX_INPUT_IS_LONGER: &str = "is_longer";

/// ONNX output holding `[batch, 512]` L2-normalised embeddings.
pub const CLAP_ONNX_OUTPUT: &str = "embeddings";

/// Expected graph input count (mel features + length flag).
pub const CLAP_ONNX_EXPECTED_INPUTS: usize = 2;

/// Expected graph output count (one embedding tensor).
pub const CLAP_ONNX_EXPECTED_OUTPUTS: usize = 1;

/// Local weight-cache location (DEC-027 user directory, never the repo).
///
/// Windows resolves `%APPDATA%/SynthLM/models/clap/model.onnx`; other
/// platforms use `$XDG_CACHE_HOME/synthlm/clap/model.onnx`, falling back to
/// `$HOME/.cache/synthlm/clap/model.onnx`. Returns `None` when no base
/// directory resolves instead of guessing. No download happens here: a
/// missing file is a `None` path plus [`crate::EvalError::ClapUnavailable`]
/// at load time, never a silent fetch.
#[must_use]
pub fn default_weight_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|base| PathBuf::from(base).join("SynthLM/models/clap/model.onnx"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(cache) = std::env::var("XDG_CACHE_HOME") {
            if !cache.trim().is_empty() {
                return Some(PathBuf::from(cache).join("synthlm/clap/model.onnx"));
            }
        }
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".cache/synthlm/clap/model.onnx"))
    }
}

/// File-name-only label for weight errors (never an absolute path, `§8`).
fn weight_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<unnameable>")
        .to_owned()
}

/// Verify a cached weight file against the pinned manifest.
///
/// Returns the observed byte size when it equals
/// [`crate::clap::CLAP_ONNX_BYTES`]. A missing file is
/// [`crate::EvalError::ClapUnavailable`] (terminal `BLOCKED`: fetch the
/// pinned revision into [`crate::clap::default_weight_path`] and retry); a
/// size disagreement is [`crate::EvalError::ByteMismatch`] (stale or
/// truncated cache — re-fetch, never score through it).
///
/// # Errors
///
/// [`crate::EvalError::ClapUnavailable`] when the file cannot be read;
/// [`crate::EvalError::ByteMismatch`] when the size disagrees.
pub fn verify_cached_weight(path: &Path) -> Result<u64, EvalError> {
    let label = weight_label(path);
    let observed = std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|_| {
            EvalError::ClapUnavailable(format!(
                "clap weights not cached ({label}); fetch {CLAP_ONNX_URL} \
                 into the local cache and retry"
            ))
        })?;
    if observed != CLAP_ONNX_BYTES {
        return Err(EvalError::ByteMismatch {
            hint: CLAP_ONNX_BYTES,
            got: observed,
        });
    }
    Ok(observed)
}

// ---------------------------------------------------------------------------
// Spectral proxy backend (default wired embedder)
// ---------------------------------------------------------------------------

/// Deterministic spectral-fingerprint CLAP backend (TSK-601 wired default).
///
/// Pure Rust, no model file, no network: resamples the input to
/// [`crate::clap::CLAP_FINGERPRINT_SAMPLES`] samples, RMS-normalises (hence
/// gain-invariant: `+6dB` maps to the same direction), projects each of
/// [`crate::clap::CLAP_FRAMES`] frames through a 16-coefficient DCT, and
/// L2-normalises the concatenated 512-vector. Identical inputs embed
/// bit-identically; silent inputs fail with
/// [`crate::EvalError::SilenceOrTooQuiet`] rather than a bogus direction.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpectralClapEmbedder;

impl ClapEmbedder for SpectralClapEmbedder {
    fn embed(&self, samples: &[f32], _params: &MirParams) -> Result<Vec<f32>, EvalError> {
        if samples.is_empty() {
            return Err(EvalError::InvalidSamples(
                "cannot embed an empty signal".to_string(),
            ));
        }
        if samples.iter().any(|s| !s.is_finite()) {
            return Err(EvalError::InvalidSamples(
                "input contains NaN or infinite sample".to_string(),
            ));
        }
        let grid = resample_to(samples, CLAP_FINGERPRINT_SAMPLES);
        let mut energy = 0.0_f64;
        for s in &grid {
            let v = f64::from(*s);
            energy += v * v;
        }
        let rms = (energy / grid.len() as f64).sqrt();
        if !rms.is_finite() {
            return Err(EvalError::InvalidSamples(
                "non-finite fingerprint level (input out of range)".to_string(),
            ));
        }
        if rms <= 0.0 {
            return Err(EvalError::SilenceOrTooQuiet {
                detail: "fingerprint RMS is zero (digital silence?)".to_string(),
            });
        }
        let mut normed = Vec::with_capacity(grid.len());
        for s in &grid {
            normed.push(f64::from(*s) / rms);
        }
        let mut embedding = dct_fingerprint(&normed);
        let mut norm_sq = 0.0_f64;
        for v in &embedding {
            let d = f64::from(*v);
            norm_sq += d * d;
        }
        let norm = norm_sq.sqrt();
        if !norm.is_finite() || norm <= 0.0 {
            return Err(EvalError::InvalidSamples(
                "zero-norm fingerprint has no direction".to_string(),
            ));
        }
        for v in embedding.iter_mut() {
            *v = (f64::from(*v) / norm) as f32;
        }
        Ok(embedding)
    }

    fn embed_dim(&self) -> usize {
        CLAP_DIM
    }
}

/// Endpoint-pinned linear resample of a mono signal to `target` samples.
///
/// The source endpoints map onto the target endpoints, so identical inputs
/// stay identical and stationary signals keep a flat envelope (same
/// convention as [`crate::mir::resample_frames`]). Total: empty input or a
/// zero target yields an empty grid; a single source sample tiles.
fn resample_to(samples: &[f32], target: usize) -> Vec<f32> {
    if samples.is_empty() || target == 0 {
        return Vec::new();
    }
    if samples.len() == target {
        return samples.to_vec();
    }
    if samples.len() == 1 {
        return vec![samples[0]; target];
    }
    let last = samples.len() - 1;
    (0..target)
        .map(|i| {
            let pos = i as f64 * last as f64 / (target - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(last);
            let t = (pos - lo as f64) as f32;
            samples[lo] + (samples[hi] - samples[lo]) * t
        })
        .collect()
}

/// 32-frame × 16-coefficient DCT fingerprint (pre-normalisation energy).
///
/// `normed` must hold [`crate::clap::CLAP_FINGERPRINT_SAMPLES`] RMS-normalised
/// samples; output holds [`crate::clap::CLAP_DIM`] unnormalised coefficients
/// (the caller L2-normalises). Deterministic: identical inputs give
/// bit-identical outputs.
fn dct_fingerprint(normed: &[f64]) -> Vec<f32> {
    const FRAME: usize = CLAP_FINGERPRINT_SAMPLES / CLAP_FRAMES;
    let mut out = Vec::with_capacity(CLAP_DIM);
    for frame in 0..CLAP_FRAMES {
        let base = frame * FRAME;
        for k in 0..CLAP_BINS_PER_FRAME {
            let mut acc = 0.0_f64;
            for n in 0..FRAME {
                let angle = std::f64::consts::PI * k as f64 * (n as f64 + 0.5) / FRAME as f64;
                acc += normed[base + n] * angle.cos();
            }
            out.push(acc as f32);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Distance, dedup, end-to-end scoring
// ---------------------------------------------------------------------------

/// CLAP cosine distance (`1 - cosine`) in `[0, 2]`.
///
/// Near-duplicate renders score `~0.0`; clearly separated timbres score well
/// above [`crate::clap::DEFAULT_CLAP_DEDUP_DISTANCE`]. Validation (dimension
/// match, non-empty, zero-norm rejection) is inherited from
/// [`crate::score::cosine_similarity`].
///
/// # Errors
///
/// Whatever [`crate::score::cosine_similarity`] rejects.
pub fn clap_distance(a: &[f32], b: &[f32]) -> Result<f64, EvalError> {
    let cosine = cosine_similarity(a, b)?;
    Ok(1.0 - f64::from(cosine))
}

/// Dedup survivor selection by CLAP distance with an explicit radius.
///
/// Greedy, mirroring the planner `diversify_with_threshold` semantics: rank
/// by `scores` descending (stable, so input order breaks ties), keep a
/// candidate only when its embedding is at least `threshold` away from every
/// already-kept one ([`crate::clap::clap_distance`]), and return the
/// survivors' indices in score order. A non-finite or negative `threshold`
/// falls back to [`crate::clap::DEFAULT_CLAP_DEDUP_DISTANCE`]; an empty pool
/// yields an empty survivor list. Pairwise cost is `O(n^2 * dim)`, sized for
/// the DEC-018 shortlist (3–5), not for index-scale search.
///
/// # Errors
///
/// [`crate::EvalError::LengthMismatch`] when `scores` and `embeddings`
/// disagree in count; [`crate::EvalError::DimMismatch`] when an embedding
/// is not [`crate::clap::CLAP_DIM`] wide;
/// [`crate::EvalError::InvalidSamples`] on non-finite scores, non-finite
/// components, or zero-norm embeddings.
pub fn dedup_by_clap(
    scores: &[f64],
    embeddings: &[Vec<f32>],
    threshold: f64,
) -> Result<Vec<usize>, EvalError> {
    if scores.len() != embeddings.len() {
        return Err(EvalError::LengthMismatch {
            a: scores.len(),
            b: embeddings.len(),
        });
    }
    for (index, embedding) in embeddings.iter().enumerate() {
        if embedding.len() != CLAP_DIM {
            return Err(EvalError::DimMismatch {
                a: embedding.len(),
                b: CLAP_DIM,
            });
        }
        if embedding.iter().any(|v| !v.is_finite()) {
            return Err(EvalError::InvalidSamples(format!(
                "embedding {index} holds a non-finite component"
            )));
        }
        if !scores[index].is_finite() {
            return Err(EvalError::InvalidSamples(format!(
                "dedup score {index} must be finite"
            )));
        }
    }
    let radius = if threshold.is_finite() && threshold >= 0.0 {
        threshold
    } else {
        DEFAULT_CLAP_DEDUP_DISTANCE
    };
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    let mut kept: Vec<usize> = Vec::new();
    for index in order {
        let mut too_close = false;
        for kept_index in &kept {
            let distance = clap_distance(&embeddings[index], &embeddings[*kept_index])?;
            if distance < radius {
                too_close = true;
                break;
            }
        }
        if !too_close {
            kept.push(index);
        }
    }
    Ok(kept)
}

/// Dedup with the default radius
/// ([`crate::clap::DEFAULT_CLAP_DEDUP_DISTANCE`]).
///
/// # Errors
///
/// Same as [`crate::clap::dedup_by_clap`].
pub fn dedup_by_clap_default(
    scores: &[f64],
    embeddings: &[Vec<f32>],
) -> Result<Vec<usize>, EvalError> {
    dedup_by_clap(scores, embeddings, DEFAULT_CLAP_DEDUP_DISTANCE)
}

/// End-to-end score of two raw signals through an embedder backend.
///
/// Runs [`crate::mir::analyze`] on both sides (loudness-first, DEC-016),
/// threads [`crate::score::clap_cosine`] through
/// [`crate::score::compare_with_clap`], and returns a [`crate::score::Score`]
/// whose `clap_cos` is `Some` — the wired counterpart to the embedder-free
/// [`crate::score::compare`], which keeps returning `None`.
///
/// # Errors
///
/// Whatever [`crate::mir::analyze`] or the embedder rejects (too short,
/// silent, or unmeasurable inputs fail here, never as bogus similarities).
pub fn score_pair<E: ClapEmbedder>(
    embedder: &E,
    reference: &[f32],
    candidate: &[f32],
    params: &MirParams,
) -> Result<Score, EvalError> {
    let reference_features = analyze(reference, params)?;
    let candidate_features = analyze(candidate, params)?;
    let cosine = clap_cosine(embedder, reference, candidate, params)?;
    compare_with_clap(&reference_features, &candidate_features, cosine)
}

// ---------------------------------------------------------------------------
// ONNX seam (opt-in `onnx` feature, `ort`-backed)
// ---------------------------------------------------------------------------

/// ONNX audio-encoder backend behind the `onnx` cargo feature (TSK-601 seam).
///
/// `load` verifies the local cache entry against the pinned manifest
/// ([`crate::clap::verify_cached_weight`]) and opens the graph with `ort`
/// (`Session::builder`, `commit_from_file`, `inputs`/`outputs` per
/// `ort 2.0.0-rc.13` docs.rs, verified 2026-10-07), asserting the
/// two-inputs/one-output contract of [`crate::clap::CLAP_ONNX_FILE`].
/// A swapped or corrupt file fails here with
/// [`crate::EvalError::ClapUnavailable`] instead of scoring.
///
/// `embed` stays a terminal `BLOCKED` until the HTSAT 64-mel preprocessing
/// spike pins the exact `transformers` STFT constants (AGENTS.md `§3.4`
/// no-guess rule): the seam opens and validates the session today but never
/// emits uncalibrated numbers. Use [`crate::clap::SpectralClapEmbedder`]
/// until then.
///
/// `ort`'s internal `unsafe` (ONNX Runtime FFI) is a dependency-boundary
/// detail: this crate adds no `unsafe` of its own. The `onnx` feature also
/// pulls the prebuilt ONNX Runtime binaries at build time
/// (`ort` `download-binaries`); the default build stays offline.
#[cfg(feature = "onnx")]
#[derive(Debug)]
pub struct OnnxClapEmbedder {
    /// Open ONNX Runtime session (kept alive for the calibration spike that
    /// wires `embed`; read by [`crate::clap::OnnxClapEmbedder::io_counts`]).
    session: ort::session::Session,
    /// File name only (log-scrub rule: never an absolute path).
    file_label: String,
}

#[cfg(feature = "onnx")]
impl OnnxClapEmbedder {
    /// Open and contract-check a cached weight file.
    ///
    /// # Errors
    ///
    /// [`crate::EvalError::ClapUnavailable`] when the file is missing or the
    /// session fails to open; [`crate::EvalError::ByteMismatch`] on a stale
    /// cache; [`crate::EvalError::ClapUnavailable`] when the graph I/O is
    /// not the two-input/one-output CLAP audio-encoder contract.
    pub fn load(path: &Path) -> Result<Self, EvalError> {
        verify_cached_weight(path)?;
        let label = weight_label(path);
        let builder = ort::session::Session::builder().map_err(|_| {
            EvalError::ClapUnavailable(
                "onnx session builder failed (prebuilt runtime missing?)".to_string(),
            )
        })?;
        let mut builder = builder;
        let session = builder.commit_from_file(path).map_err(|_| {
            EvalError::ClapUnavailable(format!(
                "onnx session open failed ({label}); re-fetch the pinned \
                 revision or fix the local cache"
            ))
        })?;
        let backend = Self {
            session,
            file_label: label,
        };
        let (inputs, outputs) = backend.io_counts();
        if inputs != CLAP_ONNX_EXPECTED_INPUTS || outputs != CLAP_ONNX_EXPECTED_OUTPUTS {
            return Err(EvalError::ClapUnavailable(format!(
                "unexpected onnx graph I/O (inputs={inputs}, outputs={outputs}); \
                 want {CLAP_ONNX_EXPECTED_INPUTS} inputs and \
                 {CLAP_ONNX_EXPECTED_OUTPUTS} output ({})",
                backend.file_label,
            )));
        }
        Ok(backend)
    }

    /// Graph input/output counts (contract probe, also keeps the session
    /// field read outside inference).
    #[must_use]
    pub fn io_counts(&self) -> (usize, usize) {
        (self.session.inputs().len(), self.session.outputs().len())
    }

    /// Cached file name (provenance, never a path).
    #[must_use]
    pub fn file_label(&self) -> &str {
        &self.file_label
    }
}

#[cfg(feature = "onnx")]
impl ClapEmbedder for OnnxClapEmbedder {
    fn embed(&self, _samples: &[f32], _params: &MirParams) -> Result<Vec<f32>, EvalError> {
        Err(EvalError::ClapUnavailable(format!(
            "onnx inference uncalibrated ({}): HTSAT 64-mel preprocessing \
             needs the feature-extractor spike first; use SpectralClapEmbedder",
            self.file_label,
        )))
    }

    fn embed_dim(&self) -> usize {
        CLAP_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::compare;

    fn sine(len: usize, freq: f32, amp: f32) -> Vec<f32> {
        let step = 2.0 * std::f64::consts::PI * f64::from(freq) / 48_000.0;
        let mut phase = 0.0_f64;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push((f64::from(amp) * phase.sin()) as f32);
            phase += step;
        }
        out
    }

    fn l2_norm(v: &[f32]) -> f64 {
        v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt()
    }

    #[test]
    fn spectral_embed_is_512_unit_finite_and_deterministic() -> Result<(), EvalError> {
        let backend = SpectralClapEmbedder;
        assert_eq!(backend.embed_dim(), CLAP_DIM);
        assert_eq!(backend.embed_dim(), 512);
        let x = sine(48_000, 440.0, 0.4);
        let params = MirParams::v1();
        let first = backend.embed(&x, &params)?;
        let second = backend.embed(&x, &params)?;
        assert_eq!(first.len(), CLAP_DIM);
        assert_eq!(first, second, "identical inputs must embed bit-identically");
        for v in &first {
            assert!(v.is_finite(), "embedding must be finite");
        }
        assert!(
            (l2_norm(&first) - 1.0).abs() < 1e-5,
            "embedding must be L2-normalised: {}",
            l2_norm(&first)
        );
        Ok(())
    }

    #[test]
    fn spectral_embed_rejects_empty_nonfinite_and_silence() {
        let backend = SpectralClapEmbedder;
        let params = MirParams::v1();
        assert!(matches!(
            backend.embed(&[], &params),
            Err(EvalError::InvalidSamples(_))
        ));
        let mut bad = sine(4096, 440.0, 0.4);
        bad[7] = f32::NAN;
        assert!(matches!(
            backend.embed(&bad, &params),
            Err(EvalError::InvalidSamples(_))
        ));
        let quiet = vec![0.0_f32; 8192];
        assert!(matches!(
            backend.embed(&quiet, &params),
            Err(EvalError::SilenceOrTooQuiet { .. })
        ));
    }

    #[test]
    fn plus_6db_maps_to_the_same_direction() -> Result<(), EvalError> {
        let backend = SpectralClapEmbedder;
        let params = MirParams::v1();
        let base = sine(48_000, 440.0, 0.4);
        let loud: Vec<f32> = base.iter().map(|s| s * 2.0).collect();
        let distance = clap_distance(
            &backend.embed(&base, &params)?,
            &backend.embed(&loud, &params)?,
        )?;
        eprintln!("CLAP +6dB distance={distance:.3e}");
        assert!(
            distance <= 1e-6,
            "+6dB must not move the embedding: {distance}"
        );
        let scored = score_pair(&backend, &base, &loud, &params)?;
        let identical = score_pair(&backend, &base, &base.clone(), &params)?;
        assert_eq!(identical.clap_cos, Some(1.0));
        let loud_cos = scored.clap_cos.unwrap_or(-2.0);
        assert!(
            (f64::from(loud_cos) - 1.0).abs() < 1e-6,
            "loud clap_cos={loud_cos}"
        );
        // Spectral distances must not improve either (mirrors the golden gate).
        let plain = compare(&analyze(&base, &params)?, &analyze(&loud, &params)?)?;
        let base_plain = compare(&analyze(&base, &params)?, &analyze(&base.clone(), &params)?)?;
        assert!(plain.spec_l1 >= base_plain.spec_l1);
        assert!(plain.mel_weighted >= base_plain.mel_weighted);
        assert!(plain.transient_f1 <= base_plain.transient_f1);
        Ok(())
    }

    #[test]
    fn timbre_change_is_farther_than_gain_change() -> Result<(), EvalError> {
        let backend = SpectralClapEmbedder;
        let params = MirParams::v1();
        let base = sine(48_000, 440.0, 0.4);
        let other = sine(48_000, 880.0, 0.4);
        let embed_base = backend.embed(&base, &params)?;
        let embed_other = backend.embed(&other, &params)?;
        let timbre_distance = clap_distance(&embed_base, &embed_other)?;
        eprintln!("CLAP timbre distance={timbre_distance:.6}");
        assert!(
            timbre_distance > DEFAULT_CLAP_DEDUP_DISTANCE,
            "heterogeneous timbres must clear the dedup radius: {timbre_distance}"
        );
        let scored = score_pair(&backend, &base, &other, &params)?;
        let cos = scored.clap_cos.unwrap_or(2.0);
        assert!(cos < 1.0, "different timbres must not report unity: {cos}");
        assert!(cos.is_finite());
        Ok(())
    }

    #[test]
    fn homogeneous_embeddings_dedup_to_highest_score() -> Result<(), EvalError> {
        let backend = SpectralClapEmbedder;
        let params = MirParams::v1();
        let base = sine(48_000, 440.0, 0.4);
        // Per-sample drift around 1e-3, mirroring the planner homogeneous
        // fixtures (highest score last in input order to prove ranking wins).
        let drifted = |seed: f32| {
            base.iter()
                .enumerate()
                .map(|(i, s)| s + seed * 1e-3 * ((i % 7) as f32 - 3.0))
                .collect::<Vec<f32>>()
        };
        let variants = [drifted(0.0), drifted(1.0), drifted(-1.0)];
        let mut embeddings = Vec::new();
        for variant in &variants {
            embeddings.push(backend.embed(variant, &params)?);
        }
        let scores = [0.70, 0.90, 0.80];
        let survivors = dedup_by_clap_default(&scores, &embeddings)?;
        assert_eq!(
            survivors,
            vec![1],
            "only the top score survives: {survivors:?}"
        );
        Ok(())
    }

    #[test]
    fn distant_embeddings_all_survive() -> Result<(), EvalError> {
        let backend = SpectralClapEmbedder;
        let params = MirParams::v1();
        let fixtures = [
            sine(48_000, 440.0, 0.4),
            sine(48_000, 880.0, 0.4),
            sine(48_000, 220.0, 0.4),
        ];
        let mut embeddings = Vec::new();
        for fixture in &fixtures {
            embeddings.push(backend.embed(fixture, &params)?);
        }
        let scores = [0.9, 0.8, 0.7];
        let survivors = dedup_by_clap_default(&scores, &embeddings)?;
        assert_eq!(survivors, vec![0, 1, 2]);
        Ok(())
    }

    #[test]
    fn dedup_validates_shapes_and_falls_back_on_bad_radius() -> Result<(), EvalError> {
        let backend = SpectralClapEmbedder;
        let params = MirParams::v1();
        let embedding = backend.embed(&sine(48_000, 440.0, 0.4), &params)?;
        // Length disagreement between scores and embeddings.
        let duo = [embedding.clone(), embedding.clone()];
        assert!(matches!(
            dedup_by_clap(&[0.5], &duo, 0.1),
            Err(EvalError::LengthMismatch { .. })
        ));
        // Wrong embedding width.
        assert!(matches!(
            dedup_by_clap(&[0.5], &[vec![0.0_f32; 16]], 0.1),
            Err(EvalError::DimMismatch { .. })
        ));
        // Non-finite score.
        let single = [embedding.clone()];
        assert!(matches!(
            dedup_by_clap(&[f64::NAN], &single, 0.1),
            Err(EvalError::InvalidSamples(_))
        ));
        // Empty pool is an empty survivor list, not an error.
        let empty: Vec<Vec<f32>> = Vec::new();
        assert_eq!(dedup_by_clap_default(&[], &empty)?, Vec::<usize>::new());
        // Non-finite / negative radii fall back to the default.
        let pair = [embedding.clone(), embedding.clone()];
        let scores = [0.9, 0.8];
        let plain = dedup_by_clap_default(&scores, &pair)?;
        assert_eq!(dedup_by_clap(&scores, &pair, f64::NAN)?, plain);
        assert_eq!(dedup_by_clap(&scores, &pair, -1.0)?, plain);
        Ok(())
    }

    #[test]
    fn manifest_pins_source_size_and_hash() {
        assert_eq!(CLAP_ONNX_REPO, "lquint/clap-htsat-unfused-onnx");
        assert_eq!(CLAP_ONNX_REV.len(), 40);
        assert!(CLAP_ONNX_URL.contains(CLAP_ONNX_REV));
        assert!(CLAP_ONNX_URL.ends_with("/model.onnx"));
        assert_eq!(CLAP_ONNX_BYTES, 119_654_416);
        assert_eq!(CLAP_ONNX_SHA256.len(), 64);
        assert!(CLAP_ONNX_SHA256.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(CLAP_ONNX_LICENSE, "Apache-2.0");
        assert_eq!(CLAP_ONNX_OPSET, 18);
        assert_eq!(CLAP_ONNX_INPUT_FEATURES, "input_features");
        assert_eq!(CLAP_ONNX_INPUT_IS_LONGER, "is_longer");
        assert_eq!(CLAP_ONNX_OUTPUT, "embeddings");
        assert_eq!(CLAP_ONNX_EXPECTED_INPUTS, 2);
        assert_eq!(CLAP_ONNX_EXPECTED_OUTPUTS, 1);
        // Cache path ends at the pinned file name whenever it resolves.
        if let Some(path) = default_weight_path() {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some("model.onnx")
            );
        }
    }

    #[test]
    fn missing_cache_is_blocked_not_silent() {
        let absent = Path::new("definitely-absent-clap-weights.onnx");
        let err = verify_cached_weight(absent).expect_err("absent cache must fail");
        assert!(matches!(err, EvalError::ClapUnavailable(_)));
        assert!(err.blocked());
        let message = err.to_string();
        assert!(
            message.contains("definitely-absent-clap-weights.onnx"),
            "message keeps the file name: {message}"
        );
    }
}
