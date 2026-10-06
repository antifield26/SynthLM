//! `synthlm-ui`: standalone main window (TSK-119 scaffold).
//!
//! The window is an egui shell over snapshots pushed by external producer
//! threads (DEC-002, TSK-306: egui locked in with event-driven repaint).
//! See [`crate::state::UiState`] for the shared snapshot,
//! [`crate::state::spawn_producer`] for the 100 Hz push loop,
//! [`crate::fonts::install_cjk`] for the embedded CJK subset, and
//! [`crate::app::SynthApp`] for the read-only UI pass.

pub mod app;
pub mod fonts;
pub mod state;

pub use app::SynthApp;
pub use state::{Candidate, SharedState, UiState, spawn_producer};
