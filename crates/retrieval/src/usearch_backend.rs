//! Embedded in-process index: the USearch-pattern backend (TSK-203).
//!
//! Models the USearch deployment shape for a single-user DAW sidecar: an
//! embedded HNSW-style index owned by the process, no server, vectors in RAM
//! (`docs/research/C-models-retrieval-eval.md` §3.2, verified 2026-10-05).
//!
//! Prototype scope: search here is **exact** flat cosine scan, not
//! approximate HNSW. Latency numbers therefore read as an exact-search
//! ceiling; a real HNSW would trade a few recall points for lower latency at
//! 100k+ scale. Binding the real crate (`usearch 2.26.4`, Apache-2.0,
//! <https://github.com/unum-cloud/USearch>) is ⚠️需实测: the crate compiles
//! its C++ core via `cxx-build` and needs a C++ toolchain, which the 10k
//! bench machine lacks (no MSVC/cmake). See `crates/retrieval/BENCH.md` for
//! the re-run condition before locking the default backend.
//!
//! Privacy: in-memory only; [`UsearchIndex`] never writes vectors to disk.

use std::collections::HashMap;

use crate::{IndexError, Payload, ScoredHit, VectorIndex, index as idx};

/// Embedded-pattern vector index (USearch slot, exact-scan prototype).
#[derive(Debug, Default)]
pub struct UsearchIndex {
    dim: usize,
    ids: Vec<u64>,
    vectors: Vec<f32>,
    payloads: Vec<Payload>,
    position: HashMap<u64, usize>,
}

impl UsearchIndex {
    /// Scores every entry against `query`.
    fn scored_all(&self, query: &[f32]) -> Vec<ScoredHit> {
        let mut out = Vec::with_capacity(self.ids.len());
        for (row, id) in self.ids.iter().enumerate() {
            let start = row * self.dim;
            let vector = &self.vectors[start..start + self.dim];
            out.push(ScoredHit {
                id: *id,
                score: idx::cosine(query, vector),
            });
        }
        out
    }
}

impl VectorIndex for UsearchIndex {
    fn new(dim: usize) -> Result<Self, IndexError> {
        idx::check_dim(dim)?;
        Ok(Self {
            dim,
            ids: Vec::new(),
            vectors: Vec::new(),
            payloads: Vec::new(),
            position: HashMap::new(),
        })
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn add(&mut self, id: u64, vector: &[f32], payload: Payload) -> Result<(), IndexError> {
        idx::check_vector(self.dim, vector)?;
        if self.position.contains_key(&id) {
            return Err(IndexError::DuplicateId { id });
        }
        self.position.insert(id, self.ids.len());
        self.ids.push(id);
        self.vectors.extend_from_slice(vector);
        self.payloads.push(payload);
        Ok(())
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<ScoredHit>, IndexError> {
        idx::check_vector(self.dim, query)?;
        if self.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        Ok(idx::select_top_k(self.scored_all(query), top_k))
    }

    fn payload_of(&self, id: u64) -> Option<Payload> {
        self.position
            .get(&id)
            .map(|row| self.payloads[*row].clone())
    }

    fn estimated_bytes(&self) -> u64 {
        let payloads: u64 = self.payloads.iter().map(Payload::estimated_bytes).sum();
        self.ids.len() as u64 * (8 + 32) + self.vectors.len() as u64 * 4 + payloads
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modality;

    fn payload(id: &str) -> Payload {
        Payload::new(id, Modality::Audio)
    }

    #[test]
    fn rejects_duplicate_id() -> Result<(), IndexError> {
        let mut index = UsearchIndex::new(2)?;
        index.add(7, &[1.0, 0.0], payload("a"))?;
        assert_eq!(
            index.add(7, &[0.0, 1.0], payload("b")),
            Err(IndexError::DuplicateId { id: 7 })
        );
        Ok(())
    }

    #[test]
    fn ranks_by_cosine_and_returns_payload() -> Result<(), IndexError> {
        let mut index = UsearchIndex::new(2)?;
        index.add(1, &[1.0, 0.0], payload("a"))?;
        index.add(2, &[0.0, 1.0], payload("b"))?;
        let hits = index.search(&[1.0, 0.0], 2)?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, 1);
        assert!(hits[0].score > hits[1].score);
        assert_eq!(
            index.payload_of(2).as_ref().map(|p| p.preset_id.as_str()),
            Some("b")
        );
        assert!(index.payload_of(999).is_none());
        Ok(())
    }

    #[test]
    fn empty_index_and_zero_top_k_yield_empty() -> Result<(), IndexError> {
        let index = UsearchIndex::new(4)?;
        assert!(index.search(&[0.0; 4], 5)?.is_empty());
        let mut index = index;
        index.add(1, &[1.0, 0.0, 0.0, 0.0], payload("a"))?;
        assert!(index.search(&[1.0, 0.0, 0.0, 0.0], 0)?.is_empty());
        Ok(())
    }

    #[test]
    fn estimates_memory_monotonic_in_entries() -> Result<(), IndexError> {
        let mut index = UsearchIndex::new(8)?;
        let before = index.estimated_bytes();
        index.add(1, &[0.5; 8], payload("a"))?;
        assert!(index.estimated_bytes() > before);
        Ok(())
    }
}
