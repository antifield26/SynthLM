//! SynthLM retrieval layer: preset/audio embedding indexes.
//!
//! Implements TSK-203 (DEC-014): one [`VectorIndex`] trait over a single
//! embedding space, two feature-gated pattern-prototype backends
//! (`UsearchIndex` for the embedded USearch shape,
//! `LanceDbIndex` for the columnar LanceDB shape), and
//! [`MultiIndex`] for text/audio dual-index RRF merging. TSK-602 adds the
//! real-crate slot: `RealLanceDbIndex` (feature `lancedb-real`, off by
//! default) binds `lancedb =0.39.0` over on-disk tables with cosine flat
//! search and SQL prefiltering.
//!
//! Scope: prototypes are in-memory and exact-scan; no embedding bytes are
//! persisted anywhere (see [`VectorIndex`]). The real backend persists only
//! its own per-index table dir (vectors + scalar payload columns) and drops
//! it with the index. The 10k comparison harness
//! lives in `tests/bench_10k.rs` and its report in `BENCH.md`; the real-crate
//! 10k harness lives in `tests/bench_10k_real.rs` and writes no files
//! (its numbers are transcribed into `BENCH.md` by hand so `cargo test`
//! never dirties the tree).

#![warn(missing_docs)]

mod error;
mod fusion;
mod index;

#[cfg(feature = "usearch-backend")]
mod usearch_backend;

#[cfg(feature = "lancedb-backend")]
mod lancedb_backend;

#[cfg(feature = "lancedb-real")]
mod real_lancedb;

pub use error::IndexError;
pub use fusion::{MultiIndex, RRF_K, rrf_fuse};
pub use index::{CLAP_DIM, MAX_DIM, Modality, Payload, ScoredHit, VectorIndex};

#[cfg(feature = "usearch-backend")]
pub use usearch_backend::UsearchIndex;

#[cfg(feature = "lancedb-backend")]
pub use lancedb_backend::LanceDbIndex;

#[cfg(feature = "lancedb-real")]
pub use real_lancedb::RealLanceDbIndex;
