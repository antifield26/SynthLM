//! Multi-index retrieval with RRF fusion (DEC-014) and fused cosine dedup
//! (TSK-606, DEC-018).
//!
//! Text and audio embeddings live in different spaces, so they are stored in
//! two independent [`crate::VectorIndex`] objects (each enforcing its own
//! dimension) and merged at query time with Reciprocal Rank Fusion. No real
//! embeddings are needed for fusion itself: it operates purely on ranked id
//! lists, and the unit tests use hand-made rankings.
//!
//! Fused dedup ([`crate::fusion::dedup_by_cosine`]) mirrors the eval
//! `dedup_by_clap` greedy semantics (score-ordered, radius collapse,
//! non-finite threshold falls back to
//! [`crate::fusion::DEFAULT_FUSED_DEDUP_DISTANCE`]) in cosine-distance units,
//! so near-duplicate renders collapse while distinct timbres survive. The
//! radius value duplicates the eval default by value (DEC-022: `retrieval`
//! must not depend on `eval`, so the number is copied, not shared — the same
//! precedent as the documentary `CLAP_DIM` duplication).

use crate::{IndexError, MAX_DIM, Payload, ScoredHit, VectorIndex};

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

/// Default fused-dedup radius in cosine-distance units (`1 - cosine`).
///
/// Value-duplicates the eval `DEFAULT_CLAP_DEDUP_DISTANCE` (DEC-022 sibling
/// rule: `retrieval` must not depend on `eval`, so the number is copied, not
/// shared — the same precedent as the documentary `CLAP_DIM`). Calibrated so
/// near-duplicate renders (per-component drift around `1e-3`) collapse while
/// clearly separated timbres survive; see the `fused_dedup_*` tests in this
/// module.
pub const DEFAULT_FUSED_DEDUP_DISTANCE: f64 = 0.02;

/// CLAP-style cosine distance (`1 - cosine`) in `[0, 2]`.
///
/// Near-duplicate renders score near `0.0` (the measure is scale-invariant,
/// so a pure gain change barely moves it — the retrieval-side counterpart of
/// the TSK-601 `+6dB` gate); clearly separated timbres score well above
/// [`crate::fusion::DEFAULT_FUSED_DEDUP_DISTANCE`]. Zero-norm sides score a
/// cosine of `0.0` (distance `1.0`, never NaN), matching the single-index
/// convention shared by this crate's backends; the eval seam rejects
/// zero-norm inputs instead, so the two layers agree on every
/// finite-direction pair and differ only on degenerate silence (which never
/// reaches a fused shortlist as a usable direction).
///
/// # Errors
///
/// [`crate::IndexError::DimMismatch`] when the slices disagree in length;
/// [`crate::IndexError::InvalidDim`] for two empty slices;
/// [`crate::IndexError::NonFiniteVector`] on the first non-finite component
/// (`position` indexes the first offending slice — `a` is scanned before
/// `b`).
pub fn cosine_distance(a: &[f32], b: &[f32]) -> Result<f64, IndexError> {
    if a.len() != b.len() {
        return Err(IndexError::DimMismatch {
            expected: a.len(),
            got: b.len(),
        });
    }
    if a.is_empty() {
        return Err(IndexError::InvalidDim {
            got: 0,
            max: MAX_DIM,
        });
    }
    if let Some(position) = a.iter().position(|v| !v.is_finite()) {
        return Err(IndexError::NonFiniteVector { position });
    }
    if let Some(position) = b.iter().position(|v| !v.is_finite()) {
        return Err(IndexError::NonFiniteVector { position });
    }
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
    let cosine = if denom <= 0.0 {
        0.0
    } else {
        (dot / denom).clamp(-1.0, 1.0)
    };
    Ok(1.0 - cosine)
}

/// Dedup survivor selection by cosine distance with an explicit radius.
///
/// Greedy, mirroring the eval `dedup_by_clap` and planner
/// `diversify_with_threshold` semantics: rank by `scores` descending (stable,
/// so input order breaks ties), keep an entry only when its embedding is at
/// least `threshold` away from every already-kept one
/// ([`crate::fusion::cosine_distance`]), and return the survivors' indices in
/// score order. A non-finite or negative `threshold` falls back to
/// [`crate::fusion::DEFAULT_FUSED_DEDUP_DISTANCE`]; an empty pool yields an
/// empty survivor list. Pairwise cost is `O(n^2 * dim)`, sized for the
/// DEC-018 shortlist (3–5), not for index-scale search.
///
/// `scores` and `embeddings` run in parallel: `scores[i]` ranks
/// `embeddings[i]`. Fused RRF weights ([`crate::fusion::rrf_fuse`] output)
/// are the intended scores; see
/// [`crate::fusion::MultiIndex::search_fused_dedup`].
///
/// # Errors
///
/// [`crate::IndexError::DimMismatch`] when `scores` and `embeddings`
/// disagree in count (`expected`/`got` carry the two counts, not vector
/// dims), or when an embedding disagrees in width with the first one
/// (`expected` carries the first width); [`crate::IndexError::InvalidDim`]
/// for an empty embedding; [`crate::IndexError::NonFiniteVector`] on a
/// non-finite score (`position` is the score index) or a non-finite embedding
/// component (`position` is the component index within the offending
/// embedding).
pub fn dedup_by_cosine(
    scores: &[f64],
    embeddings: &[Vec<f32>],
    threshold: f64,
) -> Result<Vec<usize>, IndexError> {
    if scores.len() != embeddings.len() {
        return Err(IndexError::DimMismatch {
            expected: scores.len(),
            got: embeddings.len(),
        });
    }
    let width = embeddings.first().map_or(0, Vec::len);
    for (index, embedding) in embeddings.iter().enumerate() {
        if embedding.is_empty() {
            return Err(IndexError::InvalidDim {
                got: 0,
                max: MAX_DIM,
            });
        }
        if embedding.len() != width {
            return Err(IndexError::DimMismatch {
                expected: width,
                got: embedding.len(),
            });
        }
        if !scores[index].is_finite() {
            return Err(IndexError::NonFiniteVector { position: index });
        }
        if let Some(position) = embedding.iter().position(|v| !v.is_finite()) {
            return Err(IndexError::NonFiniteVector { position });
        }
    }
    let radius = if threshold.is_finite() && threshold >= 0.0 {
        threshold
    } else {
        DEFAULT_FUSED_DEDUP_DISTANCE
    };
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    let mut kept: Vec<usize> = Vec::new();
    for index in order {
        let mut too_close = false;
        for kept_index in &kept {
            let distance = cosine_distance(&embeddings[index], &embeddings[*kept_index])?;
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

    /// Queries both indexes, fuses with RRF, then dedups by cosine distance.
    ///
    /// Ranking scores for the dedup pass are the fused RRF weights, so the
    /// highest-fused entry wins each near-duplicate cluster (same greedy
    /// contract as the free `dedup_by_cosine` in this module). `embeddings` resolves
    /// the audio-side (CLAP-space) embedding for a fused id; ids it cannot
    /// resolve are always kept, so a partial lookup degrades to plain
    /// [`crate::fusion::MultiIndex::search_fused`] instead of dropping hits
    /// silently. Pairwise cost is `O(n^2 * dim)` over at most `top_k`
    /// entries: keep `top_k` at shortlist scale (DEC-018 3–5).
    ///
    /// # Errors
    ///
    /// Same as [`crate::fusion::MultiIndex::search_fused`], plus whatever
    /// the free `dedup_by_cosine` in this module rejects for the resolved
    /// embeddings (ragged widths, empty vectors, non-finite components).
    pub fn search_fused_dedup(
        &self,
        text_query: &[f32],
        audio_query: &[f32],
        top_k: usize,
        rrf_k: u64,
        threshold: f64,
        embeddings: &dyn Fn(u64) -> Option<Vec<f32>>,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        let fused = self.search_fused(text_query, audio_query, top_k, rrf_k)?;
        let resolved: Vec<Option<Vec<f32>>> = fused.iter().map(|hit| embeddings(hit.id)).collect();
        let mut positions: Vec<usize> = Vec::new();
        let mut scores: Vec<f64> = Vec::new();
        let mut vectors: Vec<Vec<f32>> = Vec::new();
        for (position, hit) in fused.iter().enumerate() {
            if let Some(vector) = &resolved[position] {
                positions.push(position);
                scores.push(f64::from(hit.score));
                vectors.push(vector.clone());
            }
        }
        let survivors = dedup_by_cosine(&scores, &vectors, threshold)?;
        let mut keep = vec![false; fused.len()];
        for survivor in survivors {
            if let Some(position) = positions.get(survivor) {
                keep[*position] = true;
            }
        }
        let mut out = Vec::new();
        for (position, hit) in fused.iter().enumerate() {
            if resolved[position].is_none() || keep[position] {
                out.push(*hit);
            }
        }
        Ok(out)
    }

    /// Fused search with dedup at the default radius.
    ///
    /// Shorthand for [`crate::fusion::MultiIndex::search_fused_dedup`] with
    /// the `DEFAULT_FUSED_DEDUP_DISTANCE` from this module.
    ///
    /// # Errors
    ///
    /// Same as [`crate::fusion::MultiIndex::search_fused_dedup`].
    pub fn search_fused_dedup_default(
        &self,
        text_query: &[f32],
        audio_query: &[f32],
        top_k: usize,
        rrf_k: u64,
        embeddings: &dyn Fn(u64) -> Option<Vec<f32>>,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        self.search_fused_dedup(
            text_query,
            audio_query,
            top_k,
            rrf_k,
            DEFAULT_FUSED_DEDUP_DISTANCE,
            embeddings,
        )
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

    // TSK-606 fused-behavior tests. The stub below is feature-independent
    // (no backend features required) so these run in every feature
    // combination; the backend-gated tests above keep covering the real
    // prototype indexes.
    struct FixedIndex {
        dim: usize,
        hits: Vec<ScoredHit>,
    }

    impl FixedIndex {
        fn with_hits(dim: usize, ids: &[u64]) -> Result<Self, IndexError> {
            if dim == 0 || dim > MAX_DIM {
                return Err(IndexError::InvalidDim {
                    got: dim,
                    max: MAX_DIM,
                });
            }
            let hits = ids
                .iter()
                .map(|id| ScoredHit {
                    id: *id,
                    score: 1.0,
                })
                .collect();
            Ok(Self { dim, hits })
        }
    }

    impl VectorIndex for FixedIndex {
        fn new(dim: usize) -> Result<Self, IndexError> {
            Self::with_hits(dim, &[])
        }
        fn dim(&self) -> usize {
            self.dim
        }
        fn len(&self) -> usize {
            self.hits.len()
        }
        fn add(&mut self, _id: u64, vector: &[f32], _payload: Payload) -> Result<(), IndexError> {
            if vector.len() != self.dim {
                return Err(IndexError::DimMismatch {
                    expected: self.dim,
                    got: vector.len(),
                });
            }
            if let Some(position) = vector.iter().position(|v| !v.is_finite()) {
                return Err(IndexError::NonFiniteVector { position });
            }
            Ok(())
        }
        fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<ScoredHit>, IndexError> {
            if query.len() != self.dim {
                return Err(IndexError::DimMismatch {
                    expected: self.dim,
                    got: query.len(),
                });
            }
            if let Some(position) = query.iter().position(|v| !v.is_finite()) {
                return Err(IndexError::NonFiniteVector { position });
            }
            let mut out = self.hits.clone();
            out.truncate(top_k);
            Ok(out)
        }
        fn payload_of(&self, _id: u64) -> Option<Payload> {
            None
        }
        fn estimated_bytes(&self) -> u64 {
            0
        }
    }

    #[test]
    fn dual_recall_fuses_with_stable_order() -> Result<(), IndexError> {
        // Heterogeneous dims (text 4 vs audio 3): one query per modality.
        let text = FixedIndex::with_hits(4, &[1, 2, 3])?;
        let audio = FixedIndex::with_hits(3, &[2, 3, 4])?;
        let multi = MultiIndex::new(Box::new(text), Box::new(audio));
        let first = multi.search_fused(&[1.0, 0.0, 0.0, 0.0], &[1.0, 0.0, 0.0], 4, RRF_K)?;
        let second = multi.search_fused(&[1.0, 0.0, 0.0, 0.0], &[1.0, 0.0, 0.0], 4, RRF_K)?;
        assert_eq!(first, second, "fusion must be deterministic");
        let ids: Vec<u64> = first.iter().map(|hit| hit.id).collect();
        // Id 2 (ranks 2+1) beats id 3 (ranks 3+2); both dual-list ids beat
        // the single-list ids 1 (text rank 1) and 4 (audio rank 3).
        assert_eq!(ids, vec![2, 3, 1, 4]);
        Ok(())
    }

    #[test]
    fn empty_side_degrades_to_single_side() -> Result<(), IndexError> {
        let text = FixedIndex::with_hits(2, &[1, 2])?;
        let empty = FixedIndex::with_hits(2, &[])?;
        let multi = MultiIndex::new(Box::new(text), Box::new(empty));
        let fused = multi.search_fused(&[1.0, 0.0], &[1.0, 0.0], 5, RRF_K)?;
        let ids: Vec<u64> = fused.iter().map(|hit| hit.id).collect();
        assert_eq!(ids, vec![1, 2], "empty audio side keeps the text ranking");
        // Reversed: empty text side, live audio side.
        let flipped = MultiIndex::new(
            Box::new(FixedIndex::with_hits(2, &[])?),
            Box::new(FixedIndex::with_hits(2, &[7])?),
        );
        let fused = flipped.search_fused(&[1.0, 0.0], &[1.0, 0.0], 5, RRF_K)?;
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].id, 7);
        // Both empty: empty fusion, no panic.
        let both = MultiIndex::new(
            Box::new(FixedIndex::with_hits(2, &[])?),
            Box::new(FixedIndex::with_hits(2, &[])?),
        );
        assert!(
            both.search_fused(&[1.0, 0.0], &[1.0, 0.0], 5, RRF_K)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn cosine_distance_pins_unit_cases() -> Result<(), IndexError> {
        assert_eq!(cosine_distance(&[1.0, 0.0], &[1.0, 0.0])?, 0.0);
        assert_eq!(cosine_distance(&[1.0, 0.0], &[0.0, 1.0])?, 1.0);
        assert_eq!(cosine_distance(&[1.0, 0.0], &[-1.0, 0.0])?, 2.0);
        Ok(())
    }

    #[test]
    fn cosine_distance_ignores_pure_gain() -> Result<(), IndexError> {
        // Retrieval-side counterpart of the TSK-601 +6dB gate: scaling must
        // barely move the direction.
        let base = [0.3, -0.5, 0.8, 0.1];
        let loud = [0.6, -1.0, 1.6, 0.2];
        let distance = cosine_distance(&base, &loud)?;
        assert!(
            distance <= 1e-6,
            "gain must not move the embedding: {distance}"
        );
        Ok(())
    }

    #[test]
    fn cosine_distance_validates_shapes() {
        assert!(matches!(
            cosine_distance(&[1.0, 0.0], &[1.0]),
            Err(IndexError::DimMismatch {
                expected: 2,
                got: 1
            })
        ));
        assert!(matches!(
            cosine_distance(&[], &[]),
            Err(IndexError::InvalidDim { .. })
        ));
        assert!(matches!(
            cosine_distance(&[f32::NAN, 0.0], &[1.0, 0.0]),
            Err(IndexError::NonFiniteVector { position: 0 })
        ));
        assert!(matches!(
            cosine_distance(&[1.0, 0.0], &[1.0, f32::INFINITY]),
            Err(IndexError::NonFiniteVector { position: 1 })
        ));
    }

    #[test]
    fn fused_dedup_collapses_near_duplicates() -> Result<(), IndexError> {
        // Per-component drift ~1e-3 (same fixture language as the eval
        // homogeneous test); highest score sits last to prove ranking wins.
        let base = [0.5_f32, 0.5, 0.5, 0.5];
        let drifted = |seed: f32| {
            base.iter()
                .enumerate()
                .map(|(i, s)| s + seed * 1e-3 * (i as f32 - 1.5))
                .collect::<Vec<f32>>()
        };
        let embeddings = vec![drifted(0.0), drifted(1.0), drifted(-1.0)];
        let scores = [0.70, 0.90, 0.80];
        assert_eq!(
            dedup_by_cosine(&scores, &embeddings, DEFAULT_FUSED_DEDUP_DISTANCE)?,
            vec![1]
        );
        Ok(())
    }

    #[test]
    fn fused_dedup_keeps_distant_timbres() -> Result<(), IndexError> {
        let embeddings = vec![
            vec![1.0_f32, 0.0, 0.0, 0.0],
            vec![0.0_f32, 1.0, 0.0, 0.0],
            vec![0.0_f32, 0.0, 1.0, 0.0],
        ];
        let scores = [0.9, 0.8, 0.7];
        assert_eq!(
            dedup_by_cosine(&scores, &embeddings, DEFAULT_FUSED_DEDUP_DISTANCE)?,
            vec![0, 1, 2]
        );
        Ok(())
    }

    #[test]
    fn fused_dedup_validates_and_falls_back() -> Result<(), IndexError> {
        let embedding = vec![1.0_f32, 0.0, 0.0, 0.0];
        let pair = [embedding.clone(), embedding];
        let scores = [0.9, 0.8];
        // Identical embeddings (distance 0.0) collapse under the default.
        let baseline = dedup_by_cosine(&scores, &pair, DEFAULT_FUSED_DEDUP_DISTANCE)?;
        assert_eq!(baseline, vec![0]);
        // A 0.0 radius disables dedup (0.0 is not below 0.0), mirroring the
        // planner zero-threshold contract.
        assert_eq!(dedup_by_cosine(&scores, &pair, 0.0)?, vec![0, 1]);
        // Non-finite / negative radii fall back to the default.
        assert_eq!(dedup_by_cosine(&scores, &pair, f64::NAN)?, baseline);
        assert_eq!(dedup_by_cosine(&scores, &pair, -1.0)?, baseline);
        // Shape violations surface instead of collapsing silently.
        assert!(matches!(
            dedup_by_cosine(&[0.5], &pair, 0.1),
            Err(IndexError::DimMismatch { .. })
        ));
        assert!(matches!(
            dedup_by_cosine(&scores, &[vec![1.0_f32; 4], vec![1.0_f32; 2]], 0.1),
            Err(IndexError::DimMismatch { .. })
        ));
        assert!(matches!(
            dedup_by_cosine(&scores, &[Vec::new(), vec![1.0_f32; 4]], 0.1),
            Err(IndexError::InvalidDim { .. })
        ));
        assert!(matches!(
            dedup_by_cosine(&[f64::NAN, 0.8], &pair, 0.1),
            Err(IndexError::NonFiniteVector { .. })
        ));
        // Empty pool is an empty survivor list, not an error.
        let empty_scores: [f64; 0] = [];
        let empty_embeddings: Vec<Vec<f32>> = Vec::new();
        assert_eq!(
            dedup_by_cosine(
                &empty_scores,
                &empty_embeddings,
                DEFAULT_FUSED_DEDUP_DISTANCE
            )?,
            Vec::<usize>::new()
        );
        Ok(())
    }

    #[test]
    fn fused_dedup_search_keeps_top_of_cluster() -> Result<(), IndexError> {
        // Text [1,2,3] vs audio [2,3,1]: fused order is [2,1,3,4]
        // (id 2: ranks 2+1; id 1: ranks 1+3; id 3: ranks 3+2).
        let multi = MultiIndex::new(
            Box::new(FixedIndex::with_hits(2, &[1, 2, 3])?),
            Box::new(FixedIndex::with_hits(2, &[2, 3, 1, 4])?),
        );
        let near = |seed: f32| vec![0.5 + seed * 1e-3, 0.5 - seed * 1e-3];
        let resolve = |id: u64| match id {
            1 => Some(near(0.0)),
            2 => Some(near(1.0)),
            3 => Some(vec![1.0, 0.0]),
            4 => Some(vec![0.0, 1.0]),
            _ => None,
        };
        let deduped = multi.search_fused_dedup(
            &[1.0, 0.0],
            &[1.0, 0.0],
            4,
            RRF_K,
            DEFAULT_FUSED_DEDUP_DISTANCE,
            &resolve,
        )?;
        let ids: Vec<u64> = deduped.iter().map(|hit| hit.id).collect();
        // Ids 1 and 2 are near-duplicates: the higher-fused id 2 wins.
        assert_eq!(ids, vec![2, 3, 4]);
        // Unresolvable ids are never dropped silently.
        let keep_all = multi.search_fused_dedup(
            &[1.0, 0.0],
            &[1.0, 0.0],
            4,
            RRF_K,
            DEFAULT_FUSED_DEDUP_DISTANCE,
            &|_id: u64| Option::<Vec<f32>>::None,
        )?;
        assert_eq!(keep_all.len(), 4);
        Ok(())
    }
}
