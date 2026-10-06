//! SynthLM shared kernel: cross-crate types and error conventions.
//!
//! Skeleton only (TSK-002). Business logic lands per `docs/TASK-INDEX.md`.

/// IPC protocol kernel: framing, handshake, error taxonomy, retry policy,
/// audit events, and local-socket transport (DEC-023, ARCHITECTURE §5).
pub mod ipc;
