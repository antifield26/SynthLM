//! Columnar table index: the LanceDB-pattern backend (TSK-203).
//!
//! Models the LanceDB deployment shape: an embedded serverless table over a
//! columnar dataset with versioned writes and predicate pushdown for the
//! structured payload columns (DEC-014 "结构化走 payload 过滤"; DEC-027
//! versioned state). Sources: <https://crates.io/crates/lancedb> and
//! <https://github.com/lancedb/lancedb> (Apache-2.0, verified 2026-10-06).
//!
//! Prototype scope: storage is in-memory columns with exact cosine scan, and
//! [`LanceDbIndex::manifest`] exposes ids plus payloads only (never vector
//! bytes). Predicate pushdown is real: [`VectorIndex::search_filtered`] is
//! overridden to filter rows *before* scoring, unlike the default
//! rank-then-filter implementation. Binding the real crate (`lancedb
//! 0.39.0`, Apache-2.0, async Tokio API over Arrow/Lance files) is ⚠️需实测:
//! it drags a large dependency tree and on-disk format decisions (user-dir
//! layout per DEC-027) that this prototype deliberately defers. See
//! `crates/retrieval/BENCH.md` for the re-run condition.

use std::collections::HashMap;

use crate::{IndexError, Payload, ScoredHit, VectorIndex, index as idx};

/// Columnar-pattern vector index (LanceDB slot, exact-scan prototype).
#[derive(Debug, Default)]
pub struct LanceDbIndex {
    dim: usize,
    ids: Vec<u64>,
    vectors: Vec<f32>,
    payloads: Vec<Payload>,
    position: HashMap<u64, usize>,
    table_version: u64,
}

impl LanceDbIndex {
    /// Dataset revision, bumped once per successful [`VectorIndex::add`].
    ///
    /// Stands in for LanceDB's versioned-dataset semantics (DEC-027
    /// migration story: readers can pin a revision while writers append).
    #[must_use]
    pub fn table_version(&self) -> u64 {
        self.table_version
    }

    /// Id-plus-payload manifest of the table.
    ///
    /// Exposes entry ids and structured payloads for versioning/GC
    /// bookkeeping. Vector bytes are never included (privacy invariant, see
    /// [`VectorIndex`]).
    #[must_use]
    pub fn manifest(&self) -> Vec<(u64, Payload)> {
        self.ids
            .iter()
            .zip(self.payloads.iter())
            .map(|(id, payload)| (*id, payload.clone()))
            .collect()
    }
}

impl VectorIndex for LanceDbIndex {
    fn new(dim: usize) -> Result<Self, IndexError> {
        idx::check_dim(dim)?;
        Ok(Self {
            dim,
            ids: Vec::new(),
            vectors: Vec::new(),
            payloads: Vec::new(),
            position: HashMap::new(),
            table_version: 0,
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
        self.table_version += 1;
        Ok(())
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<ScoredHit>, IndexError> {
        idx::check_vector(self.dim, query)?;
        if self.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        let mut scored = Vec::with_capacity(self.ids.len());
        for (row, id) in self.ids.iter().enumerate() {
            let start = row * self.dim;
            let vector = &self.vectors[start..start + self.dim];
            scored.push(ScoredHit {
                id: *id,
                score: idx::cosine(query, vector),
            });
        }
        Ok(idx::select_top_k(scored, top_k))
    }

    fn search_filtered(
        &self,
        query: &[f32],
        top_k: usize,
        filter: &dyn Fn(&Payload) -> bool,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        idx::check_vector(self.dim, query)?;
        if self.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        // Predicate pushdown: score only rows whose payload matches.
        let mut scored = Vec::new();
        for (row, id) in self.ids.iter().enumerate() {
            if !filter(&self.payloads[row]) {
                continue;
            }
            let start = row * self.dim;
            let vector = &self.vectors[start..start + self.dim];
            scored.push(ScoredHit {
                id: *id,
                score: idx::cosine(query, vector),
            });
        }
        Ok(idx::select_top_k(scored, top_k))
    }

    fn payload_of(&self, id: u64) -> Option<&Payload> {
        self.position.get(&id).map(|row| &self.payloads[*row])
    }

    fn estimated_bytes(&self) -> u64 {
        let payloads: u64 = self.payloads.iter().map(Payload::estimated_bytes).sum();
        self.ids.len() as u64 * (8 + 32 + 8) + self.vectors.len() as u64 * 4 + payloads
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modality;

    fn payload(id: &str, lufs: Option<f32>) -> Payload {
        let mut payload = Payload::new(id, Modality::Audio);
        payload.lufs = lufs;
        payload
    }

    #[test]
    fn version_bumps_per_add_and_manifest_hides_vectors() -> Result<(), IndexError> {
        let mut index = LanceDbIndex::new(2)?;
        assert_eq!(index.table_version(), 0);
        index.add(1, &[1.0, 0.0], payload("a", Some(-14.0)))?;
        index.add(2, &[0.0, 1.0], payload("b", None))?;
        assert_eq!(index.table_version(), 2);
        let manifest = index.manifest();
        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest[0].1.preset_id, "a");
        Ok(())
    }

    #[test]
    fn filtered_search_prefilters_before_scoring() -> Result<(), IndexError> {
        let mut index = LanceDbIndex::new(2)?;
        // Best cosine match carries no LUFS; the filter must exclude it even
        // though it would rank first unfiltered.
        index.add(1, &[1.0, 0.0], payload("loud", None))?;
        index.add(2, &[0.9, 0.1], payload("ok", Some(-14.0)))?;
        let hits = index.search_filtered(&[1.0, 0.0], 5, &|p| p.lufs.is_some())?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 2);
        // Unfiltered still ranks the exact match first (control).
        let plain = index.search(&[1.0, 0.0], 2)?;
        assert_eq!(plain[0].id, 1);
        Ok(())
    }

    #[test]
    fn rejects_cross_space_vectors() -> Result<(), IndexError> {
        let mut index = LanceDbIndex::new(crate::CLAP_DIM)?;
        let short = vec![0.0_f32; 384];
        assert!(matches!(
            index.add(1, &short, Payload::new("st", Modality::Text)),
            Err(IndexError::DimMismatch {
                expected: crate::CLAP_DIM,
                got: 384
            })
        ));
        Ok(())
    }
}
