//! Retrieval-layer error type (library boundary: `thiserror`, per `AGENTS.md` §4).

use thiserror::Error;

/// Errors raised by [`crate::VectorIndex`] implementations.
///
/// All variants are deterministic and carry the offending values; error
/// messages never include embedding bytes (privacy invariant, see
/// [`crate::VectorIndex`]).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IndexError {
    /// [`crate::VectorIndex::new`] got a dimension outside `1..=MAX_DIM`.
    #[error("invalid dimension {got}: expected 1..={max}")]
    InvalidDim {
        /// Requested dimension.
        got: usize,
        /// Upper bound ([`crate::MAX_DIM`]).
        max: usize,
    },
    /// A vector's length differs from the index dimension.
    ///
    /// Cross-space insertion is rejected here: CLAP-512 vectors and
    /// sentence-transformer vectors must never share one index
    /// (`docs/research/C-models-retrieval-eval.md` §3, verified 2026-10-05).
    #[error("dimension mismatch: index dim {expected}, got {got}")]
    DimMismatch {
        /// Dimension the index was built with.
        expected: usize,
        /// Length of the rejected vector.
        got: usize,
    },
    /// A vector contains a NaN or infinite component.
    ///
    /// Non-finite components would poison cosine scores (NaN ordering), so
    /// they are rejected at the boundary instead of clamped silently.
    #[error("vector contains non-finite component at position {position}")]
    NonFiniteVector {
        /// First offending position.
        position: usize,
    },
    /// Two entries share one id; ids must be unique per index.
    #[error("duplicate id {id}")]
    DuplicateId {
        /// The offending id.
        id: u64,
    },
    /// The real LanceDB backend failed (connect, commit, query, or Arrow
    /// conversion).
    ///
    /// Carries only the backend's display string, never embedding bytes
    /// (privacy invariant, see [`crate::VectorIndex`]). Only constructible
    /// with the `lancedb-real` feature enabled.
    #[error("lancedb backend error: {message}")]
    Backend {
        /// Backend display string (no vector data).
        message: String,
    },
}
