//! Background-job state machine for stem separation (DEC-009, ARCH §8).
//!
//! Jobs are control-plane records only: they carry an `input_fingerprint`
//! and a `params_hash` (both hex digests), never audio bytes and never a
//! source path. The watchdog/journal mentioned in ARCH §8 can persist these
//! records; the legal transition matrix is the single source of truth here.

use serde::{Deserialize, Serialize};

use crate::error::DspError;

/// Minimum accepted digest length (hex chars): rejects truncated fingerprints.
pub const MIN_DIGEST_LEN: usize = 8;

/// Maximum accepted digest length (hex chars): bounds key + filename sizes.
pub const MAX_DIGEST_LEN: usize = 256;

/// What a background job computes. Only stem separation exists today; the
/// enum is non-exhaustive-by-convention (new kinds extend `all` + key
/// namespace together).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobKind {
    /// Demucs-style stem separation, run as an offline batch job (never on
    /// the realtime chain; C-dsp-toolchain §5: CPU minutes, GPU seconds).
    #[serde(rename = "stem_separation")]
    StemSeparation,
}

impl JobKind {
    /// Stable namespace string used inside cache keys and on disk.
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::StemSeparation => "stem_separation",
        }
    }

    /// Every job kind (for exhaustive tests and key-namespace coverage).
    pub fn all() -> &'static [JobKind] {
        &[JobKind::StemSeparation]
    }

    /// Parse a namespace string back (used when enumerating the cache).
    pub fn parse(s: &str) -> Option<JobKind> {
        match s {
            "stem_separation" => Some(JobKind::StemSeparation),
            _ => None,
        }
    }
}

/// Lifecycle state of a background [`Job`] (ARCH §8: pending/running/done/failed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobState {
    /// Queued, no worker has picked the job up.
    #[serde(rename = "pending")]
    Pending,
    /// A worker owns the job; only one worker may hold it at a time.
    #[serde(rename = "running")]
    Running,
    /// Worker finished; artifacts are addressable via the job's cache key.
    #[serde(rename = "done")]
    Done,
    /// Worker failed or the job was cancelled before completion.
    #[serde(rename = "failed")]
    Failed,
}

impl JobState {
    /// Stable wire string for this state.
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Pending => "pending",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    /// Every lifecycle state (for exhaustive transition tests).
    pub fn all() -> &'static [JobState] {
        &[
            JobState::Pending,
            JobState::Running,
            JobState::Done,
            JobState::Failed,
        ]
    }
}

/// Whether `from -> to` is a legal transition.
///
/// Legal matrix: `Pending → Running` (pickup), `Pending → Failed` (cancel),
/// `Running → Done` (success), `Running → Failed` (error). Everything else
/// — including `Pending → Done`, any edge out of `Done`/`Failed`, and
/// self-loops — is illegal and rejected by [`Job::transition`].
pub fn can_transition(from: JobState, to: JobState) -> bool {
    matches!(
        (from, to),
        (JobState::Pending, JobState::Running | JobState::Failed)
            | (JobState::Running, JobState::Done | JobState::Failed)
    )
}

/// Validate one hex-digest component (fingerprint or params hash).
///
/// Hex-only (plus bounded length) is what keeps raw paths out of keys:
/// `/`, `\`, `.`, `:` and friends can never validate, so a path can never
/// become a key component. `what` selects the error variant.
pub(crate) fn validate_digest(value: &str, what: DigestKind) -> Result<(), DspError> {
    let len_ok = value.len() >= MIN_DIGEST_LEN && value.len() <= MAX_DIGEST_LEN;
    let hex_ok = !value.is_empty() && value.chars().all(|c| c.is_ascii_hexdigit());
    if len_ok && hex_ok {
        return Ok(());
    }
    let reason = format!(
        "digest must be {MIN_DIGEST_LEN}..={MAX_DIGEST_LEN} hex chars, got len {} hex={hex_ok}",
        value.len()
    );
    Err(match what {
        DigestKind::Fingerprint => DspError::InvalidFingerprint { reason },
        DigestKind::ParamsHash => DspError::InvalidParamsHash { reason },
    })
}

/// Which digest component failed validation (selects the error variant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DigestKind {
    /// The audio input fingerprint.
    Fingerprint,
    /// The separation-params hash.
    ParamsHash,
}

/// A background stem-separation job: content identity + lifecycle state.
///
/// The job stores addressing material only. A finished job's products live
/// in the external content-addressed cache under
/// [`crate::key::CacheKey`]; callers derive the key from
/// [`Job::cache_key`] rather than persisting any path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// What the job computes.
    pub kind: JobKind,
    /// Hex digest of the input audio (e.g. MIR fingerprint), never a path.
    pub input_fingerprint: String,
    /// Hex digest of the canonical separation params, never a path.
    pub params_hash: String,
    /// Current lifecycle state.
    pub state: JobState,
}

impl Job {
    /// Build a `Pending` job, validating both digest components.
    pub fn new(
        kind: JobKind,
        input_fingerprint: String,
        params_hash: String,
    ) -> Result<Self, DspError> {
        validate_digest(&input_fingerprint, DigestKind::Fingerprint)?;
        validate_digest(&params_hash, DigestKind::ParamsHash)?;
        Ok(Self {
            kind,
            input_fingerprint,
            params_hash,
            state: JobState::Pending,
        })
    }

    /// Current lifecycle state.
    pub fn state(&self) -> JobState {
        self.state
    }

    /// Move the job to `to`, rejecting illegal transitions.
    ///
    /// Illegal requests return [`DspError::IllegalTransition`] and leave the
    /// job untouched (no partial state change is possible on a copy enum).
    pub fn transition(&mut self, to: JobState) -> Result<(), DspError> {
        if can_transition(self.state, to) {
            self.state = to;
            Ok(())
        } else {
            Err(DspError::IllegalTransition {
                from: self.state.as_str().to_owned(),
                to: to.as_str().to_owned(),
            })
        }
    }

    /// Content-addressed key for this job's products.
    pub fn cache_key(&self) -> Result<crate::key::CacheKey, DspError> {
        crate::key::derive_cache_key(self.kind, &self.input_fingerprint, &self.params_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digests() -> (String, String) {
        ("ab12cd34ef56ab78".to_owned(), "0011223344556677".to_owned())
    }

    #[test]
    fn new_job_starts_pending() {
        let (fp, ph) = digests();
        let job = Job::new(JobKind::StemSeparation, fp, ph).expect("valid digests");
        assert_eq!(job.state, JobState::Pending);
    }

    #[test]
    fn legal_lifecycle_pending_running_done() {
        let (fp, ph) = digests();
        let mut job = Job::new(JobKind::StemSeparation, fp, ph).expect("valid digests");
        job.transition(JobState::Running).expect("pickup");
        assert_eq!(job.state, JobState::Running);
        job.transition(JobState::Done).expect("success");
        assert_eq!(job.state, JobState::Done);
    }

    #[test]
    fn cancel_pending_to_failed_is_legal() {
        let (fp, ph) = digests();
        let mut job = Job::new(JobKind::StemSeparation, fp, ph).expect("valid digests");
        job.transition(JobState::Failed).expect("cancel");
        assert_eq!(job.state, JobState::Failed);
    }

    #[test]
    fn illegal_transitions_rejected_and_state_unchanged() {
        // Every (from, to) pair outside the legal matrix must be refused,
        // and the refusal must not move the job.
        let illegal: &[(JobState, JobState)] = &[
            (JobState::Pending, JobState::Pending),
            (JobState::Pending, JobState::Done),
            (JobState::Running, JobState::Pending),
            (JobState::Running, JobState::Running),
            (JobState::Done, JobState::Pending),
            (JobState::Done, JobState::Running),
            (JobState::Done, JobState::Done),
            (JobState::Done, JobState::Failed),
            (JobState::Failed, JobState::Pending),
            (JobState::Failed, JobState::Running),
            (JobState::Failed, JobState::Done),
            (JobState::Failed, JobState::Failed),
        ];
        assert_eq!(
            illegal.len(),
            12,
            "matrix must cover all 16 pairs minus 4 legal ones"
        );
        for (from, to) in illegal {
            let (fp, ph) = digests();
            let mut job = Job::new(JobKind::StemSeparation, fp, ph).expect("valid digests");
            job.state = *from;
            let err = job
                .transition(*to)
                .expect_err("illegal transition must fail");
            assert!(
                matches!(err, DspError::IllegalTransition { .. }),
                "wrong error for {from:?} -> {to:?}: {err:?}"
            );
            assert_eq!(job.state, *from, "failed transition must not move the job");
        }
    }

    #[test]
    fn non_hex_or_path_like_digests_rejected() {
        for bad in [
            "../../etc/passwd",
            "C:\\audio\\take.wav",
            "not-hex!!",
            "short",
            "",
        ] {
            let err = Job::new(
                JobKind::StemSeparation,
                bad.to_owned(),
                "0011223344556677".to_owned(),
            )
            .expect_err("bad fingerprint must fail");
            assert!(
                matches!(err, DspError::InvalidFingerprint { .. }),
                "wrong error for {bad:?}: {err:?}"
            );
        }
        let err = Job::new(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78".to_owned(),
            "has space in it!!".to_owned(),
        )
        .expect_err("bad params hash must fail");
        assert!(matches!(err, DspError::InvalidParamsHash { .. }), "{err:?}");
    }

    #[test]
    fn state_wire_strings_stable() {
        for state in JobState::all() {
            let wire = serde_json::to_value(state).expect("serialize state");
            assert_eq!(wire, serde_json::Value::String(state.as_str().to_owned()));
            let back: JobState = serde_json::from_value(wire).expect("deserialize state");
            assert_eq!(&back, state);
        }
    }
}
