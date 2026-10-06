//! SynthLM shared kernel: cross-crate types and error conventions.
//!
//! Skeleton only (TSK-002). Business logic lands per `docs/TASK-INDEX.md`.

/// IPC protocol kernel: framing, handshake, error taxonomy, retry policy,
/// audit events, and local-socket transport (DEC-023, ARCHITECTURE §5).
pub mod ipc;

/// Cloud-model configuration: API key, endpoints, consent tier, and the
/// DEC-011 upload whitelist (DEC-010/011, ARCHITECTURE §8–§10).
pub mod config;

/// First-run consent store: three-tier authorization persistence, text prompt,
/// and the settings data API (DEC-010/011, ARCHITECTURE §7).
pub mod consent;

/// IPC endpoint permissions: least-privilege socket files (Unix) and the
/// same-user default-ACL behavioral assertion (Windows) (TSK-115, D-eng-eco §2).
pub mod perm;

/// Shared-memory bulk channel: threshold policy, PCM block handoff, and the
/// control-plane announce envelope (TSK-115, D-eng-eco §2).
pub mod shm;
