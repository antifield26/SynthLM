//! Daemon-side artifact/stem cache surface (TSK-801, wire 1).
//!
//! Before this module `synthlm-dsp` was an orphan crate: the stem queue,
//! content-addressed store and GC existed and were tested, but no product
//! binary depended on them. `acrd` is that product surface now — the daemon
//! can report cache state and run the watermark GC over the same store the
//! DSP stage writes into.
//!
//! Everything here is control-plane and offline: no network, no PCM, no
//! absolute paths in output (callers format the root themselves).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use synthlm_dsp::{CacheKey, CacheMetrics, ContentStore, DspError, GcPolicy, GcReport, run_gc};

/// Cache state reported to the operator/UI.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheStatus {
    /// Store root as given by the caller (never logged by this module).
    pub root: PathBuf,
    /// Entries visible on disk.
    pub entries: usize,
    /// Bytes currently occupied by cache payloads.
    pub bytes_used: u64,
    /// Watermark the GC enforces.
    pub watermark_bytes: u64,
    /// Whether usage is inside the watermark.
    pub within_watermark: bool,
    /// Content-addressed lookups that hit since process start.
    pub hits: u64,
    /// Content-addressed lookups that missed since process start.
    pub misses: u64,
}

impl CacheStatus {
    /// Cache hit rate in `0.0..=1.0`, or `None` before any lookup.
    pub fn hit_rate(&self) -> Option<f64> {
        CacheMetrics::snapshot(
            self.hits,
            self.misses,
            self.bytes_used,
            self.watermark_bytes,
        )
        .hit_rate()
    }
}

/// Opens the store at `root` (creating the directory when missing) and reads
/// its state. Read-only with respect to cache *contents*.
///
/// # Errors
///
/// Propagates [`synthlm_dsp::DspError`] from the store (unreadable root, IO failures).
pub fn status(root: &Path) -> Result<CacheStatus, DspError> {
    std::fs::create_dir_all(root)?;
    let store = ContentStore::open(root.to_path_buf())?;
    let policy = GcPolicy::default();
    let bytes_used = store.disk_usage()?;
    let metrics = CacheMetrics::snapshot(
        store.hits(),
        store.misses(),
        bytes_used,
        policy.watermark_bytes,
    );
    Ok(CacheStatus {
        root: root.to_path_buf(),
        entries: store.keys_on_disk()?.len(),
        bytes_used,
        watermark_bytes: policy.watermark_bytes,
        within_watermark: metrics.within_watermark(),
        hits: store.hits(),
        misses: store.misses(),
    })
}

/// Runs one GC pass (orphan reclamation + watermark eviction) over `root`.
///
/// `live` is the caller's set of still-referenced keys; anything else is
/// treated as an orphan, exactly like the DSP stage's own call.
///
/// # Errors
///
/// Propagates [`synthlm_dsp::DspError`] from the store.
pub fn collect(root: &Path, live: &HashSet<CacheKey>) -> Result<GcReport, DspError> {
    std::fs::create_dir_all(root)?;
    let mut store = ContentStore::open(root.to_path_buf())?;
    run_gc(&mut store, &GcPolicy::default(), live)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("synthlm-acrd-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn empty_store_reports_zeroed_status_and_enforces_the_watermark() {
        let root = scratch("status");
        let s = status(&root).expect("status on an empty store");
        assert_eq!(s.entries, 0);
        assert_eq!(s.bytes_used, 0);
        assert_eq!(s.hits, 0);
        assert_eq!(s.misses, 0);
        assert_eq!(s.watermark_bytes, synthlm_dsp::WATERMARK_BYTES);
        assert!(s.within_watermark, "an empty store is inside the watermark");
        assert_eq!(s.hit_rate(), None, "no lookups yet => no rate");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn gc_on_an_empty_store_is_a_noop_and_reports_it() {
        let root = scratch("gc");
        let report = collect(&root, &HashSet::new()).expect("gc on an empty store");
        assert_eq!(report.reclaimed_bytes(), 0);
        assert_eq!(report.orphans_removed, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn status_creates_a_missing_root_instead_of_failing() {
        let root = scratch("create").join("nested").join("cache");
        assert!(!root.exists());
        let s = status(&root).expect("status must create the root");
        assert!(root.is_dir(), "store root exists after status");
        assert_eq!(s.entries, 0);
        let _ = std::fs::remove_dir_all(root.parent().and_then(Path::parent).unwrap_or(&root));
    }
}
