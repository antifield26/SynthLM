//! Cache GC: watermark eviction + LRU + orphan reclamation (DEC-009, ARCH §7).
//!
//! Policy, in order:
//!
//! 1. **Orphan reclamation** — products on disk with no live
//!    [`ArtifactRef`](crate::artifact::ArtifactRef) (the caller passes the
//!    live-key set) are deleted first. This is how losing candidates and
//!    superseded runs leave the disk.
//! 2. **Watermark eviction** — while usage exceeds [`crate::gc::WATERMARK_BYTES`]
//!    (50 GiB), the least-recently-used products are evicted, oldest first.
//!    The watermark wins over liveness: if live products alone exceed it,
//!    LRU live products are evicted too (and reported), per the DEC-009
//!    reversal direction ("keep winners only" tightens retention).
//!
//! [`crate::gc::CacheMetrics`] (hit rate + watermark level) is the type later
//! observability work consumes (ARCH §9); this crate only computes it.

use std::collections::HashSet;

use crate::error::DspError;
use crate::key::CacheKey;
use crate::store::ContentStore;

/// GC watermark: 50 GiB (DEC-009 reversal condition: disk use above this
/// with hit rate < 50% tightens retention to winners only).
pub const WATERMARK_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// GC tuning. Only the watermark today; reserved for per-kind TTLs later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcPolicy {
    /// Evict LRU products while usage exceeds this many bytes.
    pub watermark_bytes: u64,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            watermark_bytes: WATERMARK_BYTES,
        }
    }
}

impl GcPolicy {
    /// Policy with an explicit watermark (tests inject tiny values; the
    /// real 50 GiB default is never exercised against the user directory).
    pub fn with_watermark_bytes(watermark_bytes: u64) -> Self {
        Self { watermark_bytes }
    }
}

/// Cache health snapshot for observability consumers (ARCH §9).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheMetrics {
    /// Store hits served.
    pub hits: u64,
    /// Store misses served.
    pub misses: u64,
    /// Cached product bytes at snapshot time.
    pub bytes_used: u64,
    /// Watermark the snapshot is measured against.
    pub watermark_bytes: u64,
}

impl CacheMetrics {
    /// Build a snapshot from store counters plus a usage reading.
    pub fn snapshot(hits: u64, misses: u64, bytes_used: u64, watermark_bytes: u64) -> Self {
        Self {
            hits,
            misses,
            bytes_used,
            watermark_bytes,
        }
    }

    /// Hit rate over all lookups, or `None` before the first lookup.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits.saturating_add(self.misses);
        if total == 0 {
            None
        } else {
            Some(self.hits as f64 / total as f64)
        }
    }

    /// Usage as a fraction of the watermark (`> 1.0` means GC must run).
    pub fn usage_ratio(&self) -> f64 {
        if self.watermark_bytes == 0 {
            return if self.bytes_used == 0 {
                0.0
            } else {
                f64::INFINITY
            };
        }
        self.bytes_used as f64 / self.watermark_bytes as f64
    }

    /// Whether usage is at or under the watermark.
    pub fn within_watermark(&self) -> bool {
        self.bytes_used <= self.watermark_bytes
    }
}

/// What one GC pass did (evidence for logs/tests, never PCM or paths).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Orphaned products deleted.
    pub orphans_removed: usize,
    /// Bytes reclaimed from orphans.
    pub orphans_reclaimed_bytes: u64,
    /// Products evicted to get under the watermark.
    pub watermark_evicted: usize,
    /// Bytes reclaimed by watermark eviction.
    pub watermark_reclaimed_bytes: u64,
    /// Disk usage before the pass.
    pub bytes_before: u64,
    /// Disk usage after the pass.
    pub bytes_after: u64,
}

impl GcReport {
    /// Total bytes reclaimed by the pass.
    pub fn reclaimed_bytes(&self) -> u64 {
        self.orphans_reclaimed_bytes
            .saturating_add(self.watermark_reclaimed_bytes)
    }
}

/// Run one GC pass against `store`.
///
/// `live` is the set of keys still referenced by project pointers
/// ([`ArtifactRef`](crate::artifact::ArtifactRef)); everything on disk
/// outside it is an orphan and goes first. Watermark eviction then walks
/// LRU-oldest-first until usage is at or under the policy watermark.
pub fn run_gc(
    store: &mut ContentStore,
    policy: &GcPolicy,
    live: &HashSet<CacheKey>,
) -> Result<GcReport, DspError> {
    let bytes_before = store.disk_usage()?;
    let mut report = GcReport {
        bytes_before,
        bytes_after: bytes_before,
        ..GcReport::default()
    };

    // Pass 1: orphan reclamation (unreferenced products are deleted).
    for orphan in store.orphans(live)? {
        let bytes = store.file_bytes(&orphan)?.unwrap_or(0);
        if store.remove(&orphan)? {
            report.orphans_removed += 1;
            report.orphans_reclaimed_bytes = report.orphans_reclaimed_bytes.saturating_add(bytes);
        }
    }

    // Pass 2: watermark eviction, LRU-oldest-first (watermark wins over liveness).
    let mut usage = store.disk_usage()?;
    if usage > policy.watermark_bytes {
        let candidates = store.lru_ordered(&store.keys_on_disk()?);
        for victim in candidates {
            if usage <= policy.watermark_bytes {
                break;
            }
            let bytes = store.file_bytes(&victim)?.unwrap_or(0);
            if store.remove(&victim)? {
                usage = usage.saturating_sub(bytes);
                report.watermark_evicted += 1;
                report.watermark_reclaimed_bytes =
                    report.watermark_reclaimed_bytes.saturating_add(bytes);
            }
        }
    }

    report.bytes_after = store.disk_usage()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactKind;
    use crate::job::JobKind;
    use crate::key::derive_cache_key;
    use crate::store::ContentStore;
    use std::path::PathBuf;

    /// Unique scratch directory for one test (same no-`tempfile` pattern as
    /// the store tests; the real user directory is never touched).
    fn unique_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("synthlm-dsp-{pid}-{tag}"))
    }

    fn remove_dir(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn key(fp: &str) -> CacheKey {
        derive_cache_key(JobKind::StemSeparation, fp, "0011223344556677").expect("valid digests")
    }

    #[test]
    fn watermark_constant_is_50gib() {
        assert_eq!(WATERMARK_BYTES, 50 * 1024 * 1024 * 1024);
        assert_eq!(GcPolicy::default().watermark_bytes, WATERMARK_BYTES);
    }

    #[test]
    fn orphan_reclamation_deletes_unreferenced_products() {
        let dir = unique_dir("gc-orphan");
        remove_dir(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let live_key = key("aa11aa11aa11aa11");
        let dead_key = key("bb22bb22bb22bb22");
        store
            .put(&live_key, ArtifactKind::Vocals, &[1u8; 64])
            .expect("put live");
        store
            .put(&dead_key, ArtifactKind::Drums, &[2u8; 64])
            .expect("put dead");

        let live: HashSet<CacheKey> = [live_key.clone()].into_iter().collect();
        let report = run_gc(&mut store, &GcPolicy::default(), &live).expect("gc");
        assert_eq!(report.orphans_removed, 1);
        assert_eq!(report.orphans_reclaimed_bytes, 64);
        assert_eq!(report.watermark_evicted, 0);
        assert!(store.contains(&live_key).expect("live survives"));
        assert!(!store.contains(&dead_key).expect("dead gone"));
        remove_dir(&dir);
    }

    #[test]
    fn watermark_triggers_lru_eviction_oldest_first() {
        let dir = unique_dir("gc-watermark");
        remove_dir(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let oldest = key("cc33cc33cc33cc33");
        let newest = key("dd44dd44dd44dd44");
        store
            .put(&oldest, ArtifactKind::Bass, &[3u8; 60])
            .expect("put oldest");
        store
            .put(&newest, ArtifactKind::Other, &[4u8; 60])
            .expect("put newest");
        // Both live: watermark (100 bytes) forces eviction of one LRU product.
        let live: HashSet<CacheKey> = [oldest.clone(), newest.clone()].into_iter().collect();
        let policy = GcPolicy::with_watermark_bytes(100);
        let report = run_gc(&mut store, &policy, &live).expect("gc");
        assert_eq!(report.orphans_removed, 0);
        assert_eq!(report.watermark_evicted, 1);
        assert_eq!(report.watermark_reclaimed_bytes, 60);
        assert!(report.bytes_after <= 100);
        assert!(!store.contains(&oldest).expect("oldest evicted"));
        assert!(store.contains(&newest).expect("newest kept"));
        remove_dir(&dir);
    }

    #[test]
    fn metrics_hit_rate_and_usage_ratio() {
        let m = CacheMetrics::snapshot(3, 1, 25 * 1024 * 1024 * 1024, WATERMARK_BYTES);
        assert_eq!(m.hit_rate(), Some(0.75));
        assert!((m.usage_ratio() - 0.5).abs() < f64::EPSILON);
        assert!(m.within_watermark());

        let empty = CacheMetrics::snapshot(0, 0, 0, WATERMARK_BYTES);
        assert_eq!(empty.hit_rate(), None);

        let over = CacheMetrics::snapshot(0, 4, WATERMARK_BYTES + 1, WATERMARK_BYTES);
        assert_eq!(over.hit_rate(), Some(0.0));
        assert!(over.usage_ratio() > 1.0);
        assert!(!over.within_watermark());
    }
}
