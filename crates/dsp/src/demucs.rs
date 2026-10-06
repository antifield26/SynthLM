//! Demucs adapter seam (DEC-009, C-dsp-toolchain §5).
//!
//! `TODO(TSK-205后续)`: this task ships the queue + cache layer only.
//! Model execution (ONNX sidecar / local batch), weight download, and RTF /
//! VRAM benchmarking land in the follow-up. This module therefore declares
//! the trait seam plus the lazy-download shape so the follow-up has a fixed
//! target — and every entry point fails loudly rather than half-working.
//!
//! Cost context (C-dsp-toolchain §5, 2026-10-05): a 3-minute song costs
//! ~4.5 min on CPU (official RTF ~1.5; M4 Pro single-specialist RTF 0.20,
//! full bag 0.49) versus ~47 s on M4 MPS / ~7 s on L4 GPU; VRAM ≥ 3 GB
//! (default working set ~7 GB, lower via `--segment` / `-d cpu`).

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
    /// lazy-download progress UI (`TODO(TSK-205后续)` to wire).
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
/// The follow-up (`TODO(TSK-205后续)`) implements this against the ONNX
/// sidecar / local batch runner: pick the model up (`ensure_model_available`
/// performs the declared lazy download), run the job's audio through it,
/// and return one [`ArtifactRef`] per stem written through the
/// content-addressed cache. The trait takes `&Job` (identity only) rather
/// than audio bytes so backends cannot accidentally pull PCM into the
/// control plane (AGENTS.md §8: raw audio stays out of queues/logs).
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
        // TODO(TSK-205后续): lazy-download weights (~166MB–1.26GB per
        // DemucsModel::approx_weight_bytes) into model_dir on first use,
        // with progress + checksum verification. This task must not download.
        Err(DspError::BackendNotWired {
            op: "ensure_model".to_owned(),
        })
    }

    fn separate(&self, _job: &Job) -> Result<Vec<ArtifactRef>, DspError> {
        // TODO(TSK-205后续): run ONNX sidecar / local batch on the job's
        // cached input, then put stems through ContentStore and return
        // ArtifactRefs. Never runs in the DAW process (L8).
        Err(DspError::BackendNotWired {
            op: "separate".to_owned(),
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
}
