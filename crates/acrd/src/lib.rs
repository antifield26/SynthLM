//! `synthlm-acrd`: SynthLM sidecar daemon library.
//!
//! Hosts planner/retrieval/eval/DSP off the DAW process (L8): the serving
//! daemon ([`daemon`]), the deterministic demo harness ([`demo`]), the
//! single-command live e2e orchestration ([`e2e`]), the true IPC dispatcher
//! ([`dispatch`]), and the background-task journal ([`task`]).
//! The `acrd` binary (`src/main.rs`) is a thin argument-dispatch shell over
//! this library.

/// Sidecar daemon: accept loop, handshake, dispatch serving, watchdog.
pub mod daemon;

/// Deterministic M3 demo harness (fixed-seed candidates, no model calls).
pub mod demo;

/// Single-command live M3 e2e orchestration (LiveTier2 only, no fallback).
pub mod e2e;

/// True IPC dispatcher: wire frames to the offline planner chain.
pub mod dispatch;

/// Background-task state machine plus append-only journal (WAL).
pub mod task;
