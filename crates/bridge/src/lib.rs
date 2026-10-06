//! SynthLM REAPER bridge: main-thread-only FX/param/undo/snapshot control plane.
//!
//! TSK-101 wires `reaper-medium` (primary) + `reaper-low` (Take-side /
//! raw-value fallback, `A01` §6) as git dependencies pinned to an exact rev
//! (see `crates/bridge/Cargo.toml` + `docs/LICENSES.md`). Depends on `common`
//! only besides the REAPER bindings (DEC-022: no model/analysis deps).
//!
//! [`container_addr`] holds the pure container-address layer (trait seam +
//! encode/decode + GUID anchoring + flatten-fallback stub); the live
//! main-thread adapter lands in TSK-102.

/// Container address recompute layer: encode/decode, GUID anchors, flatten stub.
pub mod container_addr;

/// Undo transaction guard + dirty marking + take refetch (DEC-008, TSK-102).
pub mod undo;

/// Explicit take snapshot + one-click whole-chain rollback (DEC-004/020, TSK-103).
pub mod snapshot;

/// REAPER extension entry: version log + harmless `%TEMP%` smoke action
/// (TSK-114). Control plane only; see module docs for the scaffold evidence.
pub mod extension;
