//! [`VectorIndex`] trait plus shared vocabulary: [`Payload`], [`ScoredHit`].
//!
//! Design notes (DEC-014; `docs/research/C-models-retrieval-eval.md` §3):
//!
//! * One index holds exactly one embedding space: every vector added to or
//!   queried against an index must match [`VectorIndex::dim`], otherwise the
//!   call fails with [`crate::IndexError::DimMismatch`]. Text and audio
//!   indexes are separate objects (possibly with different dims) fused at
//!   query time via [`crate::fusion`].
//! * Structured metadata (preset ident, modality, tags, LUFS) travels in
//!   [`Payload`] and is the only thing payload filtering inspects. Raw
//!   embedding bytes are never serialized, logged, or written to disk by any
//!   implementation in this crate: prototype backends are in-memory only and
//!   [`Payload`] carries no vector data.

use crate::IndexError;

/// Maximum vector dimension accepted by [`crate::VectorIndex::new`].
///
/// 4096 comfortably covers CLAP-512 and sentence-transformer checkpoints
/// (384/768/1024) while rejecting corrupt/degenerate inputs early.
pub const MAX_DIM: usize = 4096;

/// Canonical audio-embedding dimension (LAION-CLAP `projection_dim = 512`).
///
/// Source: <https://huggingface.co/laion/clap-htsat-fused> and
/// `docs/research/C-models-retrieval-eval.md` §1.1 (verified 2026-10-05).
/// Documentary only: the trait enforces per-index consistency, not this value.
pub const CLAP_DIM: usize = 512;

/// Which embedding space a payload belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modality {
    /// Text-side embedding (preset description / label).
    Text,
    /// Audio-side embedding (CLAP).
    Audio,
}

/// Structured metadata stored alongside one vector.
///
/// Carries no embedding bytes: only stable idents and scalar/tag fields, so
/// payload snapshots and logs stay free of audio-derived content
/// (`AGENTS.md` §8).
#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    /// Stable preset ident (never a bare FX index, per `AGENTS.md` §4).
    pub preset_id: String,
    /// Embedding space this entry was produced from.
    pub modality: Modality,
    /// Free-form tags (e.g. `"bass"`, `"analog"`).
    pub tags: Vec<String>,
    /// Integrated loudness in LUFS, when known (structured filter field).
    pub lufs: Option<f32>,
}

impl Payload {
    /// Creates a payload with no tags and unknown loudness.
    pub fn new(preset_id: &str, modality: Modality) -> Self {
        Self {
            preset_id: preset_id.to_owned(),
            modality,
            tags: Vec::new(),
            lufs: None,
        }
    }

    /// Rough in-memory footprint of this payload in bytes (estimate used by
    /// [`VectorIndex::estimated_bytes`]).
    #[must_use]
    pub fn estimated_bytes(&self) -> u64 {
        let tags: usize = self.tags.iter().map(String::len).sum();
        (self.preset_id.len() + tags) as u64 + 32 + if self.lufs.is_some() { 4 } else { 0 }
    }
}

/// One ranked hit: higher `score` sorts first.
///
/// Scores are cosine similarities in `[-1, 1]` for the prototype backends
/// (exact search) and RRF fusion weights (see [`crate::rrf_fuse`]); both share
/// the "higher is better" convention.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoredHit {
    /// Entry id supplied at [`VectorIndex::add`] time.
    pub id: u64,
    /// Ranking score (higher is better).
    pub score: f32,
}

/// In-memory vector index over a single embedding space.
///
/// # Privacy invariant
///
/// Implementations keep raw embedding bytes in process memory only: `add`
/// copies the slice for search, and no method serializes vectors to disk,
/// logs, or snapshots. [`Payload`] (idents/tags/scalars) is the only
/// persisted/filtered surface.
pub trait VectorIndex {
    /// Creates an empty index for `dim`-wide vectors.
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidDim`] when `dim` is `0` or above
    /// [`MAX_DIM`].
    fn new(dim: usize) -> Result<Self, IndexError>
    where
        Self: Sized;

    /// Vector dimension every entry and query must match.
    fn dim(&self) -> usize;

    /// Number of entries currently stored.
    fn len(&self) -> usize;

    /// Whether the index holds no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Inserts `vector` under `id` with its [`Payload`].
    ///
    /// # Errors
    ///
    /// [`IndexError::DimMismatch`] when `vector.len() != self.dim()`,
    /// [`IndexError::NonFiniteVector`] on NaN/infinite components,
    /// [`IndexError::DuplicateId`] when `id` is already present.
    fn add(&mut self, id: u64, vector: &[f32], payload: Payload) -> Result<(), IndexError>;

    /// Returns up to `top_k` hits sorted by descending score.
    ///
    /// An empty index or `top_k == 0` yields `Ok(vec![])`; `top_k` above
    /// [`VectorIndex::len`] yields one hit per entry.
    ///
    /// # Errors
    ///
    /// [`IndexError::DimMismatch`] when `query.len() != self.dim()`,
    /// [`IndexError::NonFiniteVector`] on NaN/infinite query components.
    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<ScoredHit>, IndexError>;

    /// Ranked search restricted to entries passing `filter`.
    ///
    /// The default implementation ranks `top_k` first and filters after, so
    /// it can return fewer than `top_k` hits when many top hits are
    /// filtered out. Backends with predicate pushdown (the `LanceDbIndex`
    /// pattern) override this to filter *before* scoring
    /// and fill up to `top_k` from matching entries.
    ///
    /// # Errors
    ///
    /// Same as [`VectorIndex::search`].
    fn search_filtered(
        &self,
        query: &[f32],
        top_k: usize,
        filter: &dyn Fn(&Payload) -> bool,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        Ok(self
            .search(query, top_k)?
            .into_iter()
            .filter(|hit| self.payload_of(hit.id).is_some_and(filter))
            .collect())
    }

    /// Looks up the payload stored under `id`, if present.
    fn payload_of(&self, id: u64) -> Option<&Payload>;

    /// Rough in-memory footprint in bytes (vectors + payloads + index
    /// overhead). Estimates feed the 10k benchmark memory column; see each
    /// backend for its exact formula.
    fn estimated_bytes(&self) -> u64;
}

/// Validates `vector` against `expected` dim and finiteness.
///
/// Compiled only when a backend exists to serve (backends are the sole
/// non-test users; the shared validation unit tests ship with them).
#[cfg(any(feature = "usearch-backend", feature = "lancedb-backend"))]
pub(crate) fn check_vector(expected: usize, vector: &[f32]) -> Result<(), IndexError> {
    if vector.len() != expected {
        return Err(IndexError::DimMismatch {
            expected,
            got: vector.len(),
        });
    }
    if let Some(position) = vector.iter().position(|v| !v.is_finite()) {
        return Err(IndexError::NonFiniteVector { position });
    }
    Ok(())
}

/// Validates a constructor dimension.
///
/// See `check_vector` for why this is backend-gated.
#[cfg(any(feature = "usearch-backend", feature = "lancedb-backend"))]
pub(crate) fn check_dim(dim: usize) -> Result<(), IndexError> {
    if dim == 0 || dim > MAX_DIM {
        return Err(IndexError::InvalidDim {
            got: dim,
            max: MAX_DIM,
        });
    }
    Ok(())
}

/// Cosine similarity in `[-1, 1]`; zero norms score `0.0` (never NaN).
///
/// Callers must have validated equal lengths via `check_vector`.
///
/// See `check_vector` for why this is backend-gated.
#[cfg(any(feature = "usearch-backend", feature = "lancedb-backend"))]
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let x = f64::from(*x);
        let y = f64::from(*y);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom <= 0.0 {
        0.0
    } else {
        #[allow(clippy::cast_possible_truncation)]
        let score = dot / denom;
        score as f32
    }
}

/// Keeps the `top_k` highest `(id, score)` pairs, sorted descending by score
/// with ascending id as deterministic tie-break.
///
/// See `check_vector` for why this is backend-gated.
#[cfg(any(feature = "usearch-backend", feature = "lancedb-backend"))]
pub(crate) fn select_top_k(mut scored: Vec<ScoredHit>, top_k: usize) -> Vec<ScoredHit> {
    if top_k == 0 {
        scored.clear();
        return scored;
    }
    if scored.len() <= top_k {
        scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        return scored;
    }
    let pivot = top_k - 1;
    scored.select_nth_unstable_by(pivot, |a, b| {
        b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id))
    });
    scored.truncate(top_k);
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    scored
}

#[cfg(all(test, any(feature = "usearch-backend", feature = "lancedb-backend")))]
mod tests {
    use super::*;

    struct Stub {
        dim: usize,
    }

    impl VectorIndex for Stub {
        fn new(dim: usize) -> Result<Self, IndexError> {
            check_dim(dim)?;
            Ok(Self { dim })
        }
        fn dim(&self) -> usize {
            self.dim
        }
        fn len(&self) -> usize {
            0
        }
        fn add(&mut self, _id: u64, vector: &[f32], _payload: Payload) -> Result<(), IndexError> {
            check_vector(self.dim, vector)
        }
        fn search(&self, query: &[f32], _top_k: usize) -> Result<Vec<ScoredHit>, IndexError> {
            check_vector(self.dim, query)?;
            Ok(Vec::new())
        }
        fn payload_of(&self, _id: u64) -> Option<&Payload> {
            None
        }
        fn estimated_bytes(&self) -> u64 {
            0
        }
    }

    #[test]
    fn rejects_zero_and_oversize_dim() {
        assert!(matches!(
            Stub::new(0),
            Err(IndexError::InvalidDim { got: 0, .. })
        ));
        assert!(matches!(
            Stub::new(MAX_DIM + 1),
            Err(IndexError::InvalidDim { .. })
        ));
        assert!(Stub::new(512).is_ok());
    }

    #[test]
    fn rejects_dim_mismatch_and_non_finite() {
        let mut index = Stub { dim: 3 };
        let payload = Payload::new("p", Modality::Text);
        assert!(matches!(
            index.add(1, &[0.0, 0.0], payload.clone()),
            Err(IndexError::DimMismatch {
                expected: 3,
                got: 2
            })
        ));
        assert!(matches!(
            index.add(1, &[0.0, f32::NAN, 0.0], payload),
            Err(IndexError::NonFiniteVector { position: 1 })
        ));
        assert!(matches!(
            index.search(&[1.0, 2.0], 5),
            Err(IndexError::DimMismatch { .. })
        ));
    }

    #[test]
    fn cosine_zero_vector_scores_zero_without_nan() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert!(cosine(&[0.0, 0.0], &[0.0, 0.0]).is_finite());
    }
}
