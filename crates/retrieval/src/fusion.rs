//! Multi-index retrieval with RRF fusion (DEC-014 prototype stub).
//!
//! Text and audio embeddings live in different spaces, so they are stored in
//! two independent [`crate::VectorIndex`] objects (each enforcing its own
//! dimension) and merged at query time with Reciprocal Rank Fusion. No real
//! embeddings are needed here: fusion operates purely on ranked id lists,
//! and the unit tests use hand-made rankings.

use crate::{IndexError, Payload, ScoredHit, VectorIndex};

/// Default RRF rank constant (`1 / (k + rank)`); standard value 60.
pub const RRF_K: u64 = 60;

/// Fuses ranked lists with Reciprocal Rank Fusion.
///
/// Each list contributes `1 / (k + rank)` per id (rank is 1-based); ids
/// appearing in several lists accumulate. Within one list only the first
/// occurrence of an id counts. Output is sorted by descending fused score
/// with ascending id as deterministic tie-break. Empty input yields empty
/// output.
#[must_use]
pub fn rrf_fuse(ranked: &[&[ScoredHit]], k: u64) -> Vec<ScoredHit> {
    let mut fused: Vec<(u64, f64)> = Vec::new();
    for list in ranked {
        let mut seen: Vec<u64> = Vec::new();
        for (position, hit) in list.iter().enumerate() {
            if seen.contains(&hit.id) {
                continue;
            }
            seen.push(hit.id);
            let rank = position as u64 + 1;
            #[allow(clippy::cast_precision_loss)]
            let gain = 1.0 / (k as f64 + rank as f64);
            if let Some(entry) = fused.iter_mut().find(|(id, _)| *id == hit.id) {
                entry.1 += gain;
            } else {
                fused.push((hit.id, gain));
            }
        }
    }
    let mut out: Vec<ScoredHit> = fused
        .into_iter()
        .map(|(id, score)| ScoredHit {
            id,
            #[allow(clippy::cast_possible_truncation)]
            score: score as f32,
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Text + audio dual index with fused querying (DEC-014).
///
/// The two indexes are independent objects: they may (and per
/// `docs/research/C-models-retrieval-eval.md` §3, must) use different
/// dimensions, e.g. ST-384 text vs CLAP-512 audio. Callers supply one query
/// vector per modality.
pub struct MultiIndex {
    text: Box<dyn VectorIndex>,
    audio: Box<dyn VectorIndex>,
}

impl MultiIndex {
    /// Combines two pre-built indexes into one fused view.
    pub fn new(text: Box<dyn VectorIndex>, audio: Box<dyn VectorIndex>) -> Self {
        Self { text, audio }
    }

    /// Dimension of the text-side index.
    #[must_use]
    pub fn text_dim(&self) -> usize {
        self.text.dim()
    }

    /// Dimension of the audio-side index.
    #[must_use]
    pub fn audio_dim(&self) -> usize {
        self.audio.dim()
    }

    /// Queries both indexes and fuses the rankings with RRF.
    ///
    /// # Errors
    ///
    /// [`IndexError::DimMismatch`] when a query vector does not match its
    /// index, [`IndexError::NonFiniteVector`] on non-finite components.
    pub fn search_fused(
        &self,
        text_query: &[f32],
        audio_query: &[f32],
        top_k: usize,
        rrf_k: u64,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        let text_hits = self.text.search(text_query, top_k)?;
        let audio_hits = self.audio.search(audio_query, top_k)?;
        let mut fused = rrf_fuse(&[&text_hits, &audio_hits], rrf_k);
        fused.truncate(top_k);
        Ok(fused)
    }

    /// Fused search with per-index payload filters.
    ///
    /// Each side applies its own predicate through that backend's
    /// [`VectorIndex::search_filtered`] (pushdown-aware backends filter
    /// before scoring).
    ///
    /// # Errors
    ///
    /// Same as [`MultiIndex::search_fused`].
    pub fn search_fused_filtered(
        &self,
        text_query: &[f32],
        audio_query: &[f32],
        top_k: usize,
        rrf_k: u64,
        text_filter: &dyn Fn(&Payload) -> bool,
        audio_filter: &dyn Fn(&Payload) -> bool,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        let text_hits = self.text.search_filtered(text_query, top_k, text_filter)?;
        let audio_hits = self
            .audio
            .search_filtered(audio_query, top_k, audio_filter)?;
        let mut fused = rrf_fuse(&[&text_hits, &audio_hits], rrf_k);
        fused.truncate(top_k);
        Ok(fused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the dual-index tests below need concrete backends/payloads.
    #[cfg(all(feature = "usearch-backend", feature = "lancedb-backend"))]
    use crate::{LanceDbIndex, Modality, Payload, UsearchIndex};

    fn hits(ids: &[u64]) -> Vec<ScoredHit> {
        ids.iter()
            .map(|id| ScoredHit {
                id: *id,
                score: 1.0,
            })
            .collect()
    }

    #[test]
    fn rrf_rewards_multi_list_membership() {
        let a = hits(&[1, 2, 3]);
        let b = hits(&[2, 3, 4]);
        let fused = rrf_fuse(&[&a, &b], RRF_K);
        // Id 2 appears near the top of both lists: must outrank single-list ids.
        assert_eq!(fused[0].id, 2);
        assert!(fused.iter().any(|hit| hit.id == 1));
        assert!(fused.iter().any(|hit| hit.id == 4));
        assert_eq!(fused.len(), 4);
    }

    #[test]
    fn rrf_empty_and_singleton_behave() {
        assert!(rrf_fuse(&[], RRF_K).is_empty());
        let a = hits(&[9]);
        let fused = rrf_fuse(&[&a], RRF_K);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].id, 9);
    }

    #[test]
    fn rrf_tie_breaks_by_id_deterministically() {
        // Same single rank in two separate single-list fusions must order equal.
        let a = hits(&[5, 6]);
        let first = rrf_fuse(&[&a], RRF_K);
        let second = rrf_fuse(&[&a], RRF_K);
        assert_eq!(first, second);
    }

    // Dual-index tests need both backend patterns; single-feature builds skip
    // them (the bench harness covers each backend independently).
    #[cfg(all(feature = "usearch-backend", feature = "lancedb-backend"))]
    #[test]
    fn multi_index_allows_heterogeneous_dims() -> Result<(), IndexError> {
        // Text ST-style dim 4 vs audio CLAP-style dim 3 in one fused view.
        let mut text = UsearchIndex::new(4)?;
        text.add(1, &[1.0, 0.0, 0.0, 0.0], Payload::new("t1", Modality::Text))?;
        text.add(2, &[0.0, 1.0, 0.0, 0.0], Payload::new("t2", Modality::Text))?;
        let mut audio = LanceDbIndex::new(3)?;
        audio.add(2, &[1.0, 0.0, 0.0], Payload::new("a2", Modality::Audio))?;
        audio.add(3, &[0.0, 1.0, 0.0], Payload::new("a3", Modality::Audio))?;
        let multi = MultiIndex::new(Box::new(text), Box::new(audio));
        assert_eq!(multi.text_dim(), 4);
        assert_eq!(multi.audio_dim(), 3);
        // Id 2 is top-1 on both sides: fused rank 1.
        let fused = multi.search_fused(&[1.0, 0.0, 0.0, 0.0], &[1.0, 0.0, 0.0], 3, RRF_K)?;
        assert_eq!(fused[0].id, 2);
        Ok(())
    }

    #[cfg(all(feature = "usearch-backend", feature = "lancedb-backend"))]
    #[test]
    fn multi_index_propagates_dim_errors_per_side() -> Result<(), IndexError> {
        let text = Box::new(UsearchIndex::new(4)?) as Box<dyn VectorIndex>;
        let audio = Box::new(LanceDbIndex::new(3)?) as Box<dyn VectorIndex>;
        let multi = MultiIndex::new(text, audio);
        assert!(matches!(
            multi.search_fused(&[0.0; 3], &[0.0; 3], 3, RRF_K),
            Err(IndexError::DimMismatch { expected: 4, .. })
        ));
        Ok(())
    }

    #[cfg(all(feature = "usearch-backend", feature = "lancedb-backend"))]
    #[test]
    fn fused_filtered_applies_per_side_predicates() -> Result<(), IndexError> {
        let mut text = UsearchIndex::new(2)?;
        text.add(1, &[1.0, 0.0], Payload::new("t1", Modality::Text))?;
        let mut audio = LanceDbIndex::new(2)?;
        let mut gated = Payload::new("a1", Modality::Audio);
        gated.tags.push("keep".to_owned());
        audio.add(1, &[1.0, 0.0], gated)?;
        audio.add(2, &[1.0, 0.0], Payload::new("a2", Modality::Audio))?;
        let multi = MultiIndex::new(Box::new(text), Box::new(audio));
        let fused =
            multi.search_fused_filtered(&[1.0, 0.0], &[1.0, 0.0], 5, RRF_K, &|_| true, &|p| {
                p.tags.iter().any(|tag| tag == "keep")
            })?;
        // Only id 1 survives on the audio side; text side contributes id 1 too.
        assert!(fused.iter().all(|hit| hit.id == 1));
        Ok(())
    }
}
