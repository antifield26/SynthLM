//! Project-side artifact pointers (DEC-009, DEC-027, ARCH §7).
//!
//! Large products (stems) live in the external content-addressed cache;
//! the project (`.rpp` `P_EXT:SYNTHLM_*`) stores only [`crate::artifact::ArtifactRef`] — a
//! key plus kind plus byte size. This keeps `.rpp` volume small and makes
//! cache entries relocatable: a pointer stays valid wherever the cache root
//! moves, because it names content, not a location.

use serde::{Deserialize, Serialize};

use crate::key::CacheKey;

/// Which stem (or stem set) a cached product holds.
///
/// Four-stem Demucs layout (`vocals`/`drums`/`bass`/`other`) is the default
/// vocabulary; six-stem variants map onto it in the Demucs adapter when it
/// is wired (`TODO(M57-handoff:TSK-205后续)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// Isolated vocal stem.
    #[serde(rename = "vocals")]
    Vocals,
    /// Isolated drum stem.
    #[serde(rename = "drums")]
    Drums,
    /// Isolated bass stem.
    #[serde(rename = "bass")]
    Bass,
    /// Residual mix minus the other stems.
    #[serde(rename = "other")]
    Other,
}

impl ArtifactKind {
    /// Stable string for this artifact kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Vocals => "vocals",
            ArtifactKind::Drums => "drums",
            ArtifactKind::Bass => "bass",
            ArtifactKind::Other => "other",
        }
    }

    /// Every artifact kind (for exhaustive tests).
    pub fn all() -> &'static [ArtifactKind] {
        &[
            ArtifactKind::Vocals,
            ArtifactKind::Drums,
            ArtifactKind::Bass,
            ArtifactKind::Other,
        ]
    }
}

/// Project-side pointer to one cached product (DEC-009: project stores pointers).
///
/// `{key, kind, bytes}` only: no audio, no absolute path (AGENTS.md §8).
/// `bytes` is the product size at insert time and lets GC account usage
/// without re-statting every file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Content-addressed key of the product set in the external cache.
    pub key: CacheKey,
    /// Which stem this pointer addresses.
    pub kind: ArtifactKind,
    /// Product size in bytes at insert time.
    pub bytes: u64,
}

impl ArtifactRef {
    /// Build a pointer to a cached product.
    pub fn new(key: CacheKey, kind: ArtifactKind, bytes: u64) -> Self {
        Self { key, kind, bytes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobKind;
    use crate::key::derive_cache_key;

    fn sample_ref() -> ArtifactRef {
        let key = derive_cache_key(
            JobKind::StemSeparation,
            "ab12cd34ef56ab78",
            "0011223344556677",
        )
        .expect("valid digests");
        ArtifactRef::new(key, ArtifactKind::Vocals, 1_048_576)
    }

    #[test]
    fn pointer_serialization_roundtrip() {
        // The project side persists pointers (e.g. base64 single-line
        // P_EXT); the JSON round-trip must be lossless.
        let r = sample_ref();
        let text = serde_json::to_string(&r).expect("serialize pointer");
        for banned in ["pcm", ".wav", ".mp3", "C:", "/home", "/tmp", "path"] {
            assert!(
                !text.to_lowercase().contains(banned),
                "pointer must not hold audio/paths ({banned}): {text}"
            );
        }
        let back: ArtifactRef = serde_json::from_str(&text).expect("deserialize pointer");
        assert_eq!(r, back);
    }

    #[test]
    fn pointer_holds_no_audio_bytes() {
        // Schema check: exactly {key, kind, bytes} — no slot for PCM.
        let value = serde_json::to_value(sample_ref()).expect("to value");
        let obj = value.as_object().expect("pointer is an object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["bytes", "key", "kind"]);
    }

    #[test]
    fn artifact_kind_wire_strings_stable() {
        for kind in ArtifactKind::all() {
            let wire = serde_json::to_value(kind).expect("serialize kind");
            assert_eq!(wire, serde_json::Value::String(kind.as_str().to_owned()));
        }
    }
}
