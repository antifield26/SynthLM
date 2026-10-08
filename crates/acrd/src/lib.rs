//! `synthlm-acrd`: SynthLM sidecar daemon library.
//!
//! Hosts planner/retrieval/eval/DSP off the DAW process (L8): the serving
//! daemon ([`crate::daemon`]), the deterministic demo harness ([`crate::demo`]), the
//! single-command live e2e orchestration ([`crate::e2e`]), the true IPC dispatcher
//! ([`crate::dispatch`]), and the background-task journal ([`crate::task`]).
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

/// Stem/artifact cache surface over `synthlm-dsp` (TSK-801 wire 1).
pub mod cache;

/// Candidate vector surface: retrieval de-duplication + eval audio distance
/// (TSK-801 wires 3 and 4).
pub mod vectors;
