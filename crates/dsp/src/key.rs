//! Content-addressed cache keys (DEC-009, ARCH §7).
//!
//! A key names products by *content identity* — the input audio fingerprint
//! plus the separation-params hash — and never by source path. Both
//! components are hex digests (validated in [`crate::job`]), so path
//! separators cannot survive validation and a raw path can never become a
//! key. The same `(kind, fingerprint, params_hash)` triple always yields the
//! same key, which is what makes cache hits possible across sessions.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::DspError;
use crate::job::{DigestKind, JobKind, validate_digest};

/// Content-addressed key for one cached product set.
///
/// Wire shape is `{kind}/{fingerprint}-{params_hash}` (e.g.
/// `stem_separation/ab12…-0011…`). [`CacheKey::relative_path`] maps it to a
/// two-level cache-relative path (`{kind}/{fingerprint}-{params_hash}`) for
/// directory sharding; absolute paths are never constructed from user or
/// project input — only by joining this validated relative path onto the
/// configured cache root.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey(String);

impl CacheKey {
    /// Key string (`{kind}/{fingerprint}-{params_hash}`).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse and validate a key string (used when enumerating the cache).
    pub fn parse(s: &str) -> Result<Self, DspError> {
        let invalid = |why: &str| DspError::InvalidCacheKey {
            reason: why.to_owned(),
        };
        let (kind_s, rest) = s
            .split_once('/')
            .ok_or_else(|| invalid("missing kind prefix"))?;
        let kind = JobKind::parse(kind_s).ok_or_else(|| invalid("unknown kind prefix"))?;
        let (fp, ph) = rest
            .split_once('-')
            .ok_or_else(|| invalid("missing fp-ph separator"))?;
        validate_digest(fp, DigestKind::Fingerprint)
            .map_err(|_| invalid("bad fingerprint part"))?;
        validate_digest(ph, DigestKind::ParamsHash).map_err(|_| invalid("bad params-hash part"))?;
        let _ = kind;
        Ok(Self(s.to_owned()))
    }

    /// Cache-relative path for this key (`{kind}/{fingerprint}-{params_hash}`).
    ///
    /// Safe to join onto the cache root: both components re-validate as hex,
    /// so no `..`, `/`, or `\` can pass through.
    pub fn relative_path(&self) -> Result<PathBuf, DspError> {
        let owned = self.0.clone();
        let parsed = CacheKey::parse(&owned)?;
        let (kind_s, rest) = parsed
            .0
            .split_once('/')
            .ok_or_else(|| DspError::InvalidCacheKey {
                reason: "missing kind prefix".to_owned(),
            })?;
        Ok(PathBuf::from(kind_s).join(rest))
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Derive the content-addressed key for one `(kind, fingerprint, params_hash)`.
///
/// Deterministic: the same triple always yields the same key (no timestamps,
/// no paths, no randomness), so identical re-requests hit the cache.
pub fn derive_cache_key(
    kind: JobKind,
    input_fingerprint: &str,
    params_hash: &str,
) -> Result<CacheKey, DspError> {
    validate_digest(input_fingerprint, DigestKind::Fingerprint)?;
    validate_digest(params_hash, DigestKind::ParamsHash)?;
    Ok(CacheKey(format!(
        "{}/{input_fingerprint}-{params_hash}",
        kind.as_str()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_input_same_key() {
        let a = derive_cache_key(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78",
            "0011223344556677",
        )
        .expect("valid digests");
        let b = derive_cache_key(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78",
            "0011223344556677",
        )
        .expect("valid digests");
        assert_eq!(a, b);
        assert_eq!(
            a.as_str(),
            "stem_separation/ab12cd34ef56ab78-0011223344556677"
        );
    }

    #[test]
    fn different_params_different_key() {
        let a = derive_cache_key(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78",
            "0011223344556677",
        )
        .expect("valid digests");
        let b = derive_cache_key(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78",
            "8899aabbccddeeff",
        )
        .expect("valid digests");
        assert_ne!(a, b);
    }

    #[test]
    fn raw_paths_never_become_keys() {
        for bad in [
            "../../etc/passwd",
            "C:\\audio\\take.wav",
            "ab12cd34/../evil!!",
            "",
        ] {
            assert!(
                derive_cache_key(JobKind::StemSeparation, bad, "0011223344556677").is_err(),
                "fingerprint {bad:?} must be rejected"
            );
            assert!(
                derive_cache_key(JobKind::StemSeparation, "ab12cd34ef56ab78", bad).is_err(),
                "params hash {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn key_roundtrip_parse_and_relative_path() {
        let key = derive_cache_key(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78",
            "0011223344556677",
        )
        .expect("valid digests");
        let back = CacheKey::parse(key.as_str()).expect("parse own key");
        assert_eq!(key, back);
        let rel = key.relative_path().expect("relative path");
        assert_eq!(
            rel,
            PathBuf::from("stem_separation").join("ab12cd34ef56ab78-0011223344556677")
        );
        // No parent escapes survive: exactly two components.
        assert_eq!(rel.components().count(), 2);
    }

    #[test]
    fn malformed_keys_rejected() {
        for bad in [
            "no-slash-at-all",
            "unknown_kind/ab12cd34ef56ab78-0011223344556677",
            "stem_separation/nodashhere",
            "stem_separation/short-0011223344556677",
        ] {
            assert!(CacheKey::parse(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
