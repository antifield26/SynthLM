//! `synthlm-ui`: standalone main window (TSK-119 scaffold).
//!
//! The window is an egui shell over snapshots pushed by external producer
//! threads (DEC-002, TSK-306: egui locked in with event-driven repaint).
//! See [`crate::state::UiState`] for the shared snapshot,
//! [`crate::state::spawn_producer`] for the 100 Hz push loop,
//! [`crate::fonts::install_cjk`] for the embedded CJK subset,
//! [`crate::app::SynthApp`] for the read-only UI pass, and
//! [`crate::cards::parse_plan_text`] for the TSK-506 candidate cards with
//! single-winner selection plus apply/rollback instruction previews
//! (preview only; the REAPER side executes).

pub mod app;
pub mod cards;
pub mod fonts;
pub mod state;

pub use app::SynthApp;
pub use cards::{
    ApplyGranularity, ApplyPreview, Card, CardsError, ListedCandidate, ParamOp, PlanReport,
    RejectedEntry, RollbackPreview, Selection, parse_plan_text,
};
pub use state::{Candidate, SharedState, UiState, spawn_producer};
