//! SynthLM planning layer: Patch Plan generation and validation.
//!
//! Ships the three-tier model gateway ([`model_gw`], TSK-301/TSK-116,
//! DEC-010/011): consent-gated routing with Tier1→Tier2→Tier3 failover,
//! retry, per-tier circuit breaking, and per-call audit. Transports are the
//! [`model_gw::Transport`] trait plus the [`model_gw::MockTransport`]
//! fault-injection double (no network) and the real blocking
//! [`model_gw::HttpsTransport`] responses client (cloud tiers only).
//! Cloud bases honor the system proxy; loopback/hermetic builds never do.

/// Candidate assembly: distance dedup with direction coverage plus the
/// six-field candidate card (TSK-304, DEC-018/019).
pub mod candidate;
/// Three-tier model gateway (TSK-301/TSK-116): tier resolution, failover
/// routing, mock + real HTTPS transports, and audit (DEC-010/011).
pub mod model_gw;
/// JSON Patch plan schema with two-phase validation and repair (TSK-302):
/// ident-shaped paths, profile-linked semantics, and a pure repair loop
/// (DEC-013).
pub mod patch;
/// Intent planning over explicit backends (TSK-503, DEC-010/011/013):
/// seeded deterministic candidates vs. live Tier2 text-first planning
/// (gateway route → lenient parse → dual validation with repair →
/// diversification), with unerased backend watermarks.
pub mod planning;
/// Derivative-free search: zero-dep TPE-lite coarse pass plus hand-written
/// Nelder-Mead refinement over a mock objective (TSK-303).
pub mod search;
