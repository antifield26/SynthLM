//! On-disk table index: the real-crate LanceDB backend (TSK-602).
//!
//! Binds `lancedb =0.39.0` (Apache-2.0; sources
//! <https://crates.io/crates/lancedb> and
//! <https://github.com/lancedb/lancedb>, verified 2026-10-07) behind the
//! non-default `lancedb-real` feature. Each [`RealLanceDbIndex`] owns one
//! LanceDB table in its own OS temp dir (removed on drop): an `id` column,
//! one `FixedSizeList<Float32>` vector column, and scalar payload columns
//! (`preset_id`, `modality`, `lufs`). Queries run unindexed (flat) cosine
//! search, so ranking and scores match the [`crate::LanceDbIndex`] pattern
//! exactly on unit vectors; the 10k harness in `tests/bench_10k_real.rs`
//! checks this parity explicitly.
//!
//! Design notes (DEC-014; `lancedb 0.39.0` docs.rs, verified 2026-10-07):
//!
//! * `add` only buffers rows in memory. The table is (re)materialized by
//!   [`RealLanceDbIndex::flush`], which runs automatically before every
//!   search; one flush is one LanceDB commit, so 10k single-row commits never
//!   happen. [`RealLanceDbIndex::table_version`] reports the live LanceDB
//!   dataset version of the last commit (buffered adds are uncommitted).
//! * The LanceDB API is async; a private current-thread Tokio runtime bridges
//!   it to the sync [`crate::VectorIndex`] trait. Never call these methods
//!   from inside another Tokio runtime worker (nested `block_on` panics):
//!   offload with `spawn_blocking` instead. This index never runs on the
//!   audio thread (AGENTS.md §4).
//! * Score convention: LanceDB returns cosine *distance* `d` in `[0, 2]` in
//!   the `_distance` column; hits report `1.0 - d`, i.e. cosine similarity
//!   in `[-1, 1]`, higher is better, like the prototypes.
//! * [`crate::VectorIndex::search_filtered`] cannot push an opaque Rust
//!   closure into the query engine, so it refills from widening flat queries
//!   (`top_k`, `2*top_k`, …) and filters client-side; the returned set is
//!   identical to prefilter-then-score. True server-side pushdown is
//!   available via [`RealLanceDbIndex::search_filtered_sql`] with a SQL
//!   predicate (e.g. `"lufs < -27.6"`; `NULL` rows never match comparisons,
//!   matching the `Option` closure semantics).
//! * Privacy: [`RealLanceDbIndex::manifest`] exposes ids plus payloads only,
//!   never vector bytes. The table dir path is never logged or printed (its
//!   `Debug` impl omits it: OS temp paths embed the user name, and absolute
//!   paths are banned from logs by AGENTS.md §8).
//! * Limitation: cosine distance is undefined for zero vectors (upstream
//!   never returns them); zero-norm queries surface as
//!   [`crate::IndexError::Backend`], while the prototypes score them `0.0`.
//!   The 10k harness uses unit vectors, so parity is unaffected.
//!
//! Build note: enabling this module requires a `protoc` binary (`PROTOC` env)
//! for `lance-encoding`'s build script, and `lancedb 0.39.0` only compiles
//! with its `remote` feature enabled (upstream gates `Error::Http` on
//! `remote` but uses it in `job.rs` unconditionally; verified 2026-10-07
//! against the registry source). Both are recorded in `BENCH.md`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use arrow_array::types::Float32Type;
use arrow_array::{FixedSizeListArray, Float32Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt as _;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::{IndexError, Modality, Payload, ScoredHit, VectorIndex, index as idx};

/// Table column holding entry ids.
const COL_ID: &str = "id";
/// Table column holding embedding vectors (`FixedSizeList<Float32>`).
const COL_VECTOR: &str = "vector";
/// Table column holding stable preset idents.
const COL_PRESET_ID: &str = "preset_id";
/// Table column holding the modality tag (`"text"` / `"audio"`).
const COL_MODALITY: &str = "modality";
/// Table column holding integrated loudness (nullable; SQL comparisons skip `NULL`).
const COL_LUFS: &str = "lufs";

/// Monotonic suffix for per-index table names (with the process id).
static TABLE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Maps any displayable backend failure to [`IndexError::Backend`].
fn backend_failure(message: impl std::fmt::Display) -> IndexError {
    IndexError::Backend {
        message: message.to_string(),
    }
}

/// Arrow schema of the per-index table for `dim`-wide vectors.
fn table_schema(dim: i32) -> std::sync::Arc<Schema> {
    std::sync::Arc::new(Schema::new(vec![
        Field::new(COL_ID, DataType::UInt64, false),
        Field::new(
            COL_VECTOR,
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("item", DataType::Float32, true)),
                dim,
            ),
            true,
        ),
        Field::new(COL_PRESET_ID, DataType::Utf8, false),
        Field::new(COL_MODALITY, DataType::Utf8, false),
        Field::new(COL_LUFS, DataType::Float32, true),
    ]))
}

/// Mutable half of [`RealLanceDbIndex`], behind a mutex so `search` (which
/// takes `&self`) can commit buffered rows before querying.
struct RealState {
    /// Owns the table storage; deliberately never read (the leading
    /// underscore only silences `dead_code`): dropping it removes the
    /// directory, so it must outlive `db`/`table` (fields drop in
    /// declaration order, hence it stays first).
    _dir: tempfile::TempDir,
    /// Open database rooted at the owned temp dir (see `_dir` above).
    db: lancedb::Connection,
    /// Live table once the first flush commits, `None` until then.
    table: Option<lancedb::Table>,
    /// Unique table name within [`RealState::db`].
    table_name: String,
    /// Rows buffered by [`VectorIndex::add`], not yet committed.
    pending_ids: Vec<u64>,
    /// Row-major buffered vectors (`pending_ids.len() * dim` entries).
    pending_vectors: Vec<f32>,
    /// Payloads of buffered rows, positional with `pending_ids`.
    pending_payloads: Vec<Payload>,
    /// Payloads of every entry (buffered and committed), for closure
    /// filters and [`RealLanceDbIndex::payload_of`].
    payloads: HashMap<u64, Payload>,
    /// Live LanceDB dataset version of the last commit (`0` pre-flush).
    table_version: u64,
}

/// Real-crate LanceDB vector index over a single embedding space.
///
/// Created with [`VectorIndex::new`]; rows added with [`VectorIndex::add`]
/// are buffered until [`RealLanceDbIndex::flush`] (called automatically by
/// every search) commits them to the on-disk table. See the module docs for
/// the sync-over-async bridging rules and the score convention.
pub struct RealLanceDbIndex {
    dim: usize,
    dim_i32: i32,
    runtime: tokio::runtime::Runtime,
    state: Mutex<RealState>,
}

impl std::fmt::Debug for RealLanceDbIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Omits the table dir (absolute OS temp path) and the runtime by
        // design; see the module docs.
        formatter
            .debug_struct("RealLanceDbIndex")
            .field("dim", &self.dim)
            .field("len", &self.len())
            .field("table_version", &self.table_version())
            .finish_non_exhaustive()
    }
}

impl RealLanceDbIndex {
    /// Locks the mutable state, mapping poisoning to [`IndexError::Backend`].
    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, RealState>, IndexError> {
        self.state.lock().map_err(|_| IndexError::Backend {
            message: "index mutex poisoned".to_owned(),
        })
    }

    /// Assembles the buffered rows into one [`RecordBatch`].
    ///
    /// Pure constructor: no I/O, so the caller may hold the state lock.
    fn pending_batch(
        state: &RealState,
        dim: usize,
        dim_i32: i32,
    ) -> Result<RecordBatch, IndexError> {
        let rows = state.pending_ids.len();
        let ids = UInt64Array::from_iter_values(state.pending_ids.iter().copied());
        let mut offsets = Vec::with_capacity(rows);
        for row in 0..rows {
            let start = row * dim;
            let cells: Vec<Option<f32>> = state.pending_vectors[start..start + dim]
                .iter()
                .map(|value| Some(*value))
                .collect();
            offsets.push(Some(cells));
        }
        let vectors =
            FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(offsets, dim_i32);
        let preset_ids = StringArray::from_iter_values(
            state.pending_payloads.iter().map(|p| p.preset_id.as_str()),
        );
        let modalities =
            StringArray::from_iter_values(state.pending_payloads.iter().map(
                |p| match p.modality {
                    Modality::Text => "text",
                    Modality::Audio => "audio",
                },
            ));
        let loudness = Float32Array::from_iter(state.pending_payloads.iter().map(|p| p.lufs));
        RecordBatch::try_new(
            table_schema(dim_i32),
            vec![
                std::sync::Arc::new(ids),
                std::sync::Arc::new(vectors),
                std::sync::Arc::new(preset_ids),
                std::sync::Arc::new(modalities),
                std::sync::Arc::new(loudness),
            ],
        )
        .map_err(backend_failure)
    }

    /// Commits buffered rows to the on-disk table (no-op when empty).
    ///
    /// Runs automatically before every search; call it explicitly to include
    /// the commit in build-time measurements (the 10k harness does this) or
    /// to observe the new [`RealLanceDbIndex::table_version`] immediately.
    pub fn flush(&self) -> Result<(), IndexError> {
        let mut state = self.lock_state()?;
        if state.pending_ids.is_empty() {
            return Ok(());
        }
        let batch = Self::pending_batch(&state, self.dim, self.dim_i32)?;
        if state.table.is_none() {
            let name = state.table_name.clone();
            let table = self
                .runtime
                .block_on(state.db.create_table(name, batch).execute())
                .map_err(backend_failure)?;
            state.table_version = self
                .runtime
                .block_on(table.version())
                .map_err(backend_failure)?;
            state.table = Some(table);
        } else {
            let table = state.table.as_ref().ok_or_else(|| IndexError::Backend {
                message: "table vanished after creation".to_owned(),
            })?;
            self.runtime
                .block_on(table.add(batch).execute())
                .map_err(backend_failure)?;
            state.table_version = self
                .runtime
                .block_on(table.version())
                .map_err(backend_failure)?;
        }
        state.pending_ids.clear();
        state.pending_vectors.clear();
        state.pending_payloads.clear();
        Ok(())
    }

    /// Runs one flat cosine query, optionally with a SQL prefilter.
    ///
    /// Returns up to `top_k` hits ordered by descending cosine similarity.
    fn search_inner(
        &self,
        query: &[f32],
        top_k: usize,
        predicate: Option<&str>,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        idx::check_vector(self.dim, query)?;
        if top_k == 0 {
            return Ok(Vec::new());
        }
        self.flush()?;
        let state = self.lock_state()?;
        if state.payloads.is_empty() {
            return Ok(Vec::new());
        }
        let Some(table) = state.table.as_ref() else {
            return Ok(Vec::new());
        };
        let mut request = table
            .query()
            .nearest_to(query)
            .map_err(backend_failure)?
            .distance_type(lancedb::DistanceType::Cosine)
            .limit(top_k);
        if let Some(sql) = predicate {
            request = request.only_if(sql);
        }
        let stream = self
            .runtime
            .block_on(request.execute())
            .map_err(backend_failure)?;
        let batches: Vec<RecordBatch> = self
            .runtime
            .block_on(stream.try_collect())
            .map_err(backend_failure)?;
        let mut scored = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name(COL_ID)
                .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
                .ok_or_else(|| IndexError::Backend {
                    message: "result lacks UInt64 `id` column".to_owned(),
                })?;
            let distances = batch
                .column_by_name("_distance")
                .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
                .ok_or_else(|| IndexError::Backend {
                    message: "result lacks Float32 `_distance` column".to_owned(),
                })?;
            for row in 0..batch.num_rows() {
                scored.push(ScoredHit {
                    id: ids.value(row),
                    // Cosine distance d = 1 - similarity.
                    score: 1.0 - distances.value(row),
                });
            }
        }
        Ok(idx::select_top_k(scored, top_k))
    }

    /// Dataset revision: the live LanceDB dataset version of the last commit
    /// (`0` before the first flush commits).
    ///
    /// Reads the real versioned-dataset counter (DEC-027 migration story),
    /// at commit rather than per-row granularity: buffered `add` calls do
    /// not bump it, one non-empty [`RealLanceDbIndex::flush`] commits once.
    #[must_use]
    pub fn table_version(&self) -> u64 {
        self.lock_state()
            .map(|state| state.table_version)
            .unwrap_or(0)
    }

    /// Id-plus-payload manifest of the table.
    ///
    /// Exposes entry ids and structured payloads ordered by ascending id.
    /// Vector bytes are never included (privacy invariant, see
    /// [`VectorIndex`]).
    #[must_use]
    pub fn manifest(&self) -> Vec<(u64, Payload)> {
        self.lock_state()
            .map(|state| {
                let mut entries: Vec<(u64, Payload)> = state
                    .payloads
                    .iter()
                    .map(|(id, payload)| (*id, payload.clone()))
                    .collect();
                entries.sort_by_key(|(id, _)| *id);
                entries
            })
            .unwrap_or_default()
    }

    /// Ranked search with a server-side SQL prefilter (predicate pushdown).
    ///
    /// Only rows matching `predicate` (e.g. `"lufs < -27.6"`) are scored;
    /// `NULL` payload fields never match comparisons, mirroring the
    /// `Option`-aware closure semantics of [`VectorIndex::search_filtered`].
    ///
    /// # Errors
    ///
    /// Same as [`VectorIndex::search`], plus [`IndexError::Backend`] when the
    /// predicate fails to parse or the query fails.
    pub fn search_filtered_sql(
        &self,
        query: &[f32],
        top_k: usize,
        predicate: &str,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        self.search_inner(query, top_k, Some(predicate))
    }
}

impl VectorIndex for RealLanceDbIndex {
    fn new(dim: usize) -> Result<Self, IndexError> {
        idx::check_dim(dim)?;
        let dim_i32 = i32::try_from(dim).map_err(|_| IndexError::Backend {
            message: "dimension exceeds i32 range".to_owned(),
        })?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(backend_failure)?;
        let dir = tempfile::TempDir::new().map_err(backend_failure)?;
        let path = dir.path().to_str().ok_or_else(|| IndexError::Backend {
            message: "temp table path is not UTF-8".to_owned(),
        })?;
        let db = runtime
            .block_on(lancedb::connect(path).execute())
            .map_err(backend_failure)?;
        let suffix = TABLE_SEQ.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            dim,
            dim_i32,
            runtime,
            state: Mutex::new(RealState {
                _dir: dir,
                db,
                table: None,
                table_name: format!("synthlm_{}_{}", std::process::id(), suffix),
                pending_ids: Vec::new(),
                pending_vectors: Vec::new(),
                pending_payloads: Vec::new(),
                payloads: HashMap::new(),
                table_version: 0,
            }),
        })
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn len(&self) -> usize {
        self.lock_state()
            .map(|state| state.payloads.len())
            .unwrap_or(0)
    }

    fn add(&mut self, id: u64, vector: &[f32], payload: Payload) -> Result<(), IndexError> {
        idx::check_vector(self.dim, vector)?;
        let mut state = self.lock_state()?;
        if state.payloads.contains_key(&id) {
            return Err(IndexError::DuplicateId { id });
        }
        state.payloads.insert(id, payload.clone());
        state.pending_ids.push(id);
        state.pending_vectors.extend_from_slice(vector);
        state.pending_payloads.push(payload);
        Ok(())
    }

    fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<ScoredHit>, IndexError> {
        self.search_inner(query, top_k, None)
    }

    fn search_filtered(
        &self,
        query: &[f32],
        top_k: usize,
        filter: &dyn Fn(&Payload) -> bool,
    ) -> Result<Vec<ScoredHit>, IndexError> {
        // An opaque Rust closure cannot be pushed into the query engine, so
        // refill client-side from widening flat queries. Flat search ranking
        // is a total order, hence every widened query is a prefix of the
        // global ranking and the kept set equals prefilter-then-score.
        idx::check_vector(self.dim, query)?;
        if top_k == 0 {
            return Ok(Vec::new());
        }
        self.flush()?;
        let len = self.len();
        if len == 0 {
            return Ok(Vec::new());
        }
        let mut width = top_k.min(len);
        loop {
            let hits = self.search_inner(query, width, None)?;
            let kept: Vec<ScoredHit> = {
                let state = self.lock_state()?;
                hits.into_iter()
                    .filter(|hit| state.payloads.get(&hit.id).is_some_and(filter))
                    .collect()
            };
            if kept.len() >= top_k || width >= len {
                return Ok(idx::select_top_k(kept, top_k));
            }
            width = width.saturating_mul(2).min(len);
        }
    }

    fn payload_of(&self, id: u64) -> Option<Payload> {
        self.lock_state().ok()?.payloads.get(&id).cloned()
    }

    fn estimated_bytes(&self) -> u64 {
        self.lock_state()
            .map(|state| {
                let payloads: u64 = state.payloads.values().map(Payload::estimated_bytes).sum();
                state.payloads.len() as u64 * (8 + 32 + 8)
                    + (state.pending_vectors.len() as u64) * 4
                    + payloads
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(id: &str, lufs: Option<f32>) -> Payload {
        let mut out = Payload::new(id, Modality::Audio);
        out.lufs = lufs;
        out
    }

    #[test]
    fn rejects_bad_dim_before_touching_disk() {
        assert!(matches!(
            RealLanceDbIndex::new(0),
            Err(IndexError::InvalidDim { got: 0, .. })
        ));
        assert!(matches!(
            RealLanceDbIndex::new(crate::MAX_DIM + 1),
            Err(IndexError::InvalidDim { .. })
        ));
    }

    #[test]
    fn roundtrip_self_hit_with_cosine_scores() -> Result<(), IndexError> {
        let mut index = RealLanceDbIndex::new(4)?;
        assert_eq!(index.table_version(), 0);
        index.add(1, &[1.0, 0.0, 0.0, 0.0], payload("a", Some(-14.0)))?;
        index.add(2, &[0.0, 1.0, 0.0, 0.0], payload("b", None))?;
        // Still buffered: nothing committed yet.
        assert_eq!(index.table_version(), 0);
        let hits = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, 1);
        assert!((hits[0].score - 1.0).abs() < 1e-6);
        assert!(hits[0].score > hits[1].score);
        assert_eq!(index.table_version(), 1);
        // Explicit flush with nothing buffered is a no-op (no version bump).
        index.flush()?;
        assert_eq!(index.table_version(), 1);
        assert_eq!(index.len(), 2);
        assert_eq!(
            index.payload_of(2).as_ref().map(|p| p.preset_id.as_str()),
            Some("b")
        );
        Ok(())
    }

    #[test]
    fn rejects_duplicates_and_bad_vectors() -> Result<(), IndexError> {
        let mut index = RealLanceDbIndex::new(2)?;
        index.add(7, &[1.0, 0.0], payload("a", None))?;
        assert_eq!(
            index.add(7, &[0.0, 1.0], payload("b", None)),
            Err(IndexError::DuplicateId { id: 7 })
        );
        assert!(matches!(
            index.add(8, &[1.0], payload("c", None)),
            Err(IndexError::DimMismatch {
                expected: 2,
                got: 1
            })
        ));
        assert!(matches!(
            index.add(8, &[1.0, f32::NAN], payload("c", None)),
            Err(IndexError::NonFiniteVector { position: 1 })
        ));
        assert!(matches!(
            index.search(&[1.0], 1),
            Err(IndexError::DimMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn closure_filter_refills_past_excluded_best() -> Result<(), IndexError> {
        let mut index = RealLanceDbIndex::new(2)?;
        index.add(1, &[1.0, 0.0], payload("loud", None))?;
        index.add(2, &[0.9, 0.1], payload("ok", Some(-14.0)))?;
        let hits = index.search_filtered(&[1.0, 0.0], 5, &|p| p.lufs.is_some())?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 2);
        Ok(())
    }

    #[test]
    fn sql_filter_pushes_lufs_predicate_down() -> Result<(), IndexError> {
        let mut index = RealLanceDbIndex::new(2)?;
        index.add(1, &[1.0, 0.0], payload("loud", Some(-6.0)))?;
        index.add(2, &[0.9, 0.1], payload("ok", Some(-30.0)))?;
        index.add(3, &[0.8, 0.2], payload("unknown", None))?;
        let hits = index.search_filtered_sql(&[1.0, 0.0], 5, "lufs < -27.6")?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 2);
        // Unknown (`NULL`) loudness never matches the comparison.
        let all = index.search_filtered_sql(&[1.0, 0.0], 5, "lufs IS NULL")?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, 3);
        Ok(())
    }

    #[test]
    fn manifest_hides_vectors_and_orders_by_id() -> Result<(), IndexError> {
        let mut index = RealLanceDbIndex::new(2)?;
        index.add(9, &[1.0, 0.0], payload("nine", None))?;
        index.add(4, &[0.0, 1.0], payload("four", None))?;
        let manifest = index.manifest();
        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest[0].0, 4);
        assert_eq!(manifest[1].1.preset_id, "nine");
        Ok(())
    }

    #[test]
    fn empty_index_and_zero_top_k_yield_empty() -> Result<(), IndexError> {
        let index = RealLanceDbIndex::new(2)?;
        assert!(index.is_empty());
        assert!(index.search(&[1.0, 0.0], 5)?.is_empty());
        assert!(index.search_filtered(&[1.0, 0.0], 5, &|_| true)?.is_empty());
        assert!(
            index
                .search_filtered_sql(&[1.0, 0.0], 5, "lufs < 0.0")?
                .is_empty()
        );
        let mut index = index;
        index.add(1, &[1.0, 0.0], payload("a", None))?;
        assert!(index.search(&[1.0, 0.0], 0)?.is_empty());
        Ok(())
    }
}
