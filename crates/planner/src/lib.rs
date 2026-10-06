//! SynthLM planning layer: Patch Plan generation and validation.
//!
//! Ships the three-tier model gateway ([`model_gw`], TSK-301, DEC-010/011):
//! consent-gated routing with Tier1→Tier2→Tier3 failover, retry, per-tier
//! circuit breaking, and per-call audit. No real network I/O lives here —
//! the only transport is the [`model_gw::Transport`] trait plus the
//! [`model_gw::MockTransport`] fault-injection double. A real HTTPS client
//! (and its `docs/LICENSES.md` entry) is a follow-up task.

/// Three-tier model gateway (TSK-301): tier resolution, failover routing,
/// mock transport, and audit (DEC-010/011).
pub mod model_gw;
