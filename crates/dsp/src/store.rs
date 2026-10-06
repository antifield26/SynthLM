//! External content-addressed artifact store (DEC-009/027, ARCH §7).
//!
//! Layout under the cache root:
//!
//! ```text
//! <root>/<kind>/<fingerprint>-<params_hash>   artifact bytes
//! <root>/access_order.json                     LRU sidecar (oldest first)
//! ```
//!
//! The root defaults to [`default_cache_root`] (user directory, versioned
//! migration per DEC-027 is a later task) and is injectable so tests use a
//! scratch directory and never touch the real user directory. LRU uses the
//! built-in `access_order.json` sequence — never filesystem atime — so GC is
//! deterministic across platforms. All writes are atomic (`tmp` + rename).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::artifact::{ArtifactKind, ArtifactRef};
use crate::error::DspError;
use crate::key::CacheKey;

/// LRU sidecar filename inside the cache root.
pub const ORDER_FILE_NAME: &str = "access_order.json";

/// Resolve the default external cache root for stem products.
///
/// Windows: `%APPDATA%\SynthLM\cache\stems`; elsewhere:
/// `$XDG_CACHE_HOME/synthlm/stems` or `~/.cache/synthlm/stems`. Returns
/// `None` when no base directory is determinable (portable/permission-locked
/// setups) — callers then fall back to a project-relative directory with an
/// explicit notice (DEC-027 reversal condition).
pub fn default_cache_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|base| {
            PathBuf::from(base)
                .join("SynthLM")
                .join("cache")
                .join("stems")
        })
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("synthlm").join("stems"));
            }
        }
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".cache")
                .join("synthlm")
                .join("stems")
        })
    }
}

/// External content-addressed store for stem products.
///
/// Hit/miss counters feed [`crate::gc::CacheMetrics`]; the LRU sequence is
/// persisted in `access_order.json` on every mutation so GC order survives
/// restarts.
pub struct ContentStore {
    root: PathBuf,
    /// LRU sequence, oldest first (key strings).
    order: Vec<String>,
    /// Cache hits since open (for metrics).
    hits: u64,
    /// Cache misses since open (for metrics).
    misses: u64,
}

impl ContentStore {
    /// Open (creating) the store at `root`.
    pub fn open(root: PathBuf) -> Result<Self, DspError> {
        std::fs::create_dir_all(&root)?;
        let order = load_order(&root)?;
        Ok(Self {
            root,
            order,
            hits: 0,
            misses: 0,
        })
    }

    /// Cache root in use.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Hits served since open.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Misses served since open.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Resolve the on-disk path for `key` (root-joined validated relative path).
    fn artifact_path(&self, key: &CacheKey) -> Result<PathBuf, DspError> {
        Ok(self.root.join(key.relative_path()?))
    }

    /// Insert (or replace) product bytes for `key`, returning its project pointer.
    pub fn put(
        &mut self,
        key: &CacheKey,
        kind: ArtifactKind,
        bytes: &[u8],
    ) -> Result<ArtifactRef, DspError> {
        let path = self.artifact_path(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomic(&path, bytes)?;
        self.touch(key);
        persist_order(&self.root, &self.order)?;
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Ok(ArtifactRef::new(key.clone(), kind, len))
    }

    /// Fetch product bytes, recording a hit or miss and refreshing LRU order.
    pub fn fetch(&mut self, key: &CacheKey) -> Result<Option<Vec<u8>>, DspError> {
        let path = self.artifact_path(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => {
                self.hits = self.hits.saturating_add(1);
                self.touch(key);
                persist_order(&self.root, &self.order)?;
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.misses = self.misses.saturating_add(1);
                Ok(None)
            }
            Err(e) => Err(DspError::Io(e)),
        }
    }

    /// Whether `key` currently has a cached product.
    pub fn contains(&self, key: &CacheKey) -> Result<bool, DspError> {
        Ok(self.artifact_path(key)?.is_file())
    }

    /// Size of the cached product, or `None` when absent.
    pub fn file_bytes(&self, key: &CacheKey) -> Result<Option<u64>, DspError> {
        match std::fs::metadata(self.artifact_path(key)?) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DspError::Io(e)),
        }
    }

    /// Delete the cached product; returns `true` when a file was removed.
    pub fn remove(&mut self, key: &CacheKey) -> Result<bool, DspError> {
        let removed = match std::fs::remove_file(self.artifact_path(key)?) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(DspError::Io(e)),
        };
        let key_s = key.as_str().to_owned();
        if self.order.contains(&key_s) {
            self.order.retain(|k| *k != key_s);
            persist_order(&self.root, &self.order)?;
        }
        Ok(removed)
    }

    /// All keys currently on disk (parsed; unparseable files are skipped).
    pub fn keys_on_disk(&self) -> Result<Vec<CacheKey>, DspError> {
        let mut keys = Vec::new();
        let top = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
            Err(e) => return Err(DspError::Io(e)),
        };
        for entry in top {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let kind_name = entry.file_name();
            let Some(kind_s) = kind_name.to_str() else {
                continue;
            };
            for child in std::fs::read_dir(entry.path())? {
                let child = child?;
                if !child.file_type()?.is_file() {
                    continue;
                }
                let name = child.file_name();
                let Some(file) = name.to_str() else {
                    continue;
                };
                let candidate = format!("{kind_s}/{file}");
                if let Ok(key) = CacheKey::parse(&candidate) {
                    keys.push(key);
                }
            }
        }
        Ok(keys)
    }

    /// Total cached product bytes (excludes the `access_order.json` sidecar).
    pub fn disk_usage(&self) -> Result<u64, DspError> {
        let mut total = 0u64;
        for key in self.keys_on_disk()? {
            if let Some(bytes) = self.file_bytes(&key)? {
                total = total.saturating_add(bytes);
            }
        }
        Ok(total)
    }

    /// Order `keys` oldest-first by LRU sequence (unknown keys sort last).
    pub fn lru_ordered(&self, keys: &[CacheKey]) -> Vec<CacheKey> {
        let rank: HashMap<&str, usize> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i))
            .collect();
        let mut sorted = keys.to_vec();
        sorted.sort_by_key(|k| rank.get(k.as_str()).copied().unwrap_or(usize::MAX));
        sorted
    }

    /// Live-key membership helper for GC orphan detection.
    pub fn orphans(&self, live: &HashSet<CacheKey>) -> Result<Vec<CacheKey>, DspError> {
        let mut orphans: Vec<CacheKey> = self
            .keys_on_disk()?
            .into_iter()
            .filter(|k| !live.contains(k))
            .collect();
        orphans.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(orphans)
    }

    /// Mark `key` most-recently used.
    fn touch(&mut self, key: &CacheKey) {
        let key_s = key.as_str().to_owned();
        self.order.retain(|k| *k != key_s);
        self.order.push(key_s);
    }
}

/// Load the LRU sidecar; missing files yield an empty sequence, and a
/// corrupt sidecar self-heals to empty (cache stays usable; order rebuilds
/// as products are touched).
fn load_order(root: &Path) -> Result<Vec<String>, DspError> {
    let path = root.join(ORDER_FILE_NAME);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Vec<String>>(&bytes) {
            Ok(order) => Ok(order),
            Err(_) => Ok(Vec::new()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(DspError::Io(e)),
    }
}

/// Persist the LRU sidecar atomically.
fn persist_order(root: &Path, order: &[String]) -> Result<(), DspError> {
    let bytes = serde_json::to_vec(order)?;
    write_atomic(&root.join(ORDER_FILE_NAME), &bytes)?;
    Ok(())
}

/// Atomic file write: `tmp` + rename, so readers never see a half file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DspError> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobKind;
    use crate::key::derive_cache_key;

    /// Unique scratch directory for one test (mirrors `common::consent`
    /// tests: no `tempfile` dependency; the real user directory is never
    /// touched).
    pub(crate) fn unique_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("synthlm-dsp-{pid}-{tag}"))
    }

    pub(crate) fn remove_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn key(fp: &str, ph: &str) -> CacheKey {
        derive_cache_key(JobKind::StemSeparation, fp, ph).expect("valid digests")
    }

    #[test]
    fn put_fetch_roundtrip_in_injected_dir() {
        let dir = unique_dir("put-fetch");
        remove_dir(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let k = key("ab12cd34ef56ab78", "0011223344556677");

        assert_eq!(store.fetch(&k).expect("fetch miss"), None);
        assert_eq!(store.misses(), 1);

        let r = store
            .put(&k, ArtifactKind::Vocals, b"stem-bytes")
            .expect("put");
        assert_eq!(r.bytes, 10);
        assert!(store.contains(&k).expect("contains"));

        let back = store.fetch(&k).expect("fetch hit").expect("present");
        assert_eq!(back, b"stem-bytes");
        assert_eq!(store.hits(), 1);
        remove_dir(&dir);
    }

    #[test]
    fn lru_order_refreshes_on_access() {
        let dir = unique_dir("lru");
        remove_dir(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        let a = key("aa11aa11aa11aa11", "0011223344556677");
        let b = key("bb22bb22bb22bb22", "0011223344556677");
        store.put(&a, ArtifactKind::Drums, b"a").expect("put a");
        store.put(&b, ArtifactKind::Bass, b"b").expect("put b");
        assert_eq!(
            store.lru_ordered(&[b.clone(), a.clone()]),
            vec![a.clone(), b.clone()]
        );
        store.fetch(&a).expect("touch a");
        assert_eq!(store.lru_ordered(&[b.clone(), a.clone()]), vec![b, a]);
        remove_dir(&dir);
    }

    #[test]
    fn disk_usage_counts_products_not_sidecar() {
        let dir = unique_dir("usage");
        remove_dir(&dir);
        let mut store = ContentStore::open(dir.clone()).expect("open scratch store");
        assert_eq!(store.disk_usage().expect("empty usage"), 0);
        let k = key("cc33cc33cc33cc33", "0011223344556677");
        store
            .put(&k, ArtifactKind::Other, &[7u8; 100])
            .expect("put");
        assert_eq!(store.disk_usage().expect("usage"), 100);
        remove_dir(&dir);
    }
}
