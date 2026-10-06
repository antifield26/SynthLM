//! External-producer state snapshot for the UI (TSK-119).
//!
//! The DAW-side (bridge) threads own the write side: they push a fresh
//! [`crate::state::UiState`] snapshot roughly every 10 ms and then poke the
//! UI via `request_repaint`. The egui pass only ever *reads* (see
//! [`crate::app::SynthApp`]); there is deliberately no timer-driven
//! `request_repaint_after` pacing (TSK-306 §2 footgun).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Points kept in the waveform strip.
pub const WAVEFORM_LEN: usize = 240;

/// Producer push interval: 10 ms ⇒ 100 Hz status cadence.
pub const PRODUCER_INTERVAL: Duration = Duration::from_millis(10);

/// Shared handle between producer threads and the UI pass.
pub type SharedState = Arc<Mutex<UiState>>;

/// One candidate placeholder row (DEC-019 fixed fields, content arrives later).
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Stable placeholder id (display only, never a live FX index).
    pub id: u32,
    /// One-sentence difference summary (placeholder until solver output).
    pub diff_sentence: String,
    /// Confidence 0.0–1.0 (placeholder).
    pub confidence: f32,
    /// Integrated-LUFS delta vs reference (placeholder).
    pub delta_lufs: f32,
    /// Number of changed parameters (placeholder).
    pub changed_params: u32,
}

/// Snapshot pushed by external producers, read by the UI pass.
///
/// All fields are plain data; the UI clones what it needs under a short
/// lock and then releases the mutex before painting, so the pass never
/// blocks the producer.
#[derive(Debug, Clone)]
pub struct UiState {
    /// Monotonic producer tick counter.
    pub frame: u64,
    /// Simulated clock in seconds (advanced by the producer, 10 ms/step).
    pub t_s: f32,
    /// Waveform phase in radians (advanced by the producer).
    pub phase: f32,
    /// Simulated level 0.0–1.0.
    pub level: f32,
    /// Waveform strip samples (newest at back, capped at
    /// [`crate::state::WAVEFORM_LEN`]).
    pub samples: VecDeque<f32>,
    /// Candidate placeholders (empty ⇒ empty-state copy, TSK-119).
    pub candidates: Vec<Candidate>,
}

impl UiState {
    /// Blank initial snapshot (zero ticks, empty candidate list).
    #[must_use]
    pub fn new() -> Self {
        Self {
            frame: 0,
            t_s: 0.0,
            phase: 0.0,
            level: 0.0,
            samples: VecDeque::with_capacity(WAVEFORM_LEN),
            candidates: Vec::new(),
        }
    }

    /// Level rendered as decibels (floored to avoid `log10(0)`).
    #[must_use]
    pub fn level_db(&self) -> f32 {
        20.0 * self.level.max(1e-3).log10()
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

/// Advance one producer tick (10 ms of simulated time).
///
/// Pure function of the previous snapshot: deterministic and unit-tested.
/// Called only by producer threads, never by the UI pass.
pub fn advance(state: &mut UiState) {
    state.frame = state.frame.wrapping_add(1);
    state.t_s += 0.01;
    state.phase += 0.12;
    let t = state.t_s;
    state.level = 0.75 + 0.25 * (2.0 * std::f32::consts::PI * 2.0 * t).sin();
    let v = (state.phase).sin();
    state.samples.push_back(v);
    while state.samples.len() > WAVEFORM_LEN {
        state.samples.pop_front();
    }
}

/// Spawn the 100 Hz state producer on a background thread (TSK-119 §3).
///
/// Each tick locks [`crate::state::SharedState`] briefly, applies
/// [`crate::state::advance`], and wakes the UI with `request_repaint`.
/// Repaints are therefore driven purely by data arrival; no
/// `request_repaint_after` timer pacing is used anywhere (TSK-306 §2).
/// The loop exits when `stop` is set or the state lock is poisoned.
pub fn spawn_producer(
    state: SharedState,
    ctx: egui::Context,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(PRODUCER_INTERVAL);
            let poisoned = match state.lock() {
                Ok(mut guard) => {
                    advance(&mut guard);
                    false
                }
                Err(_) => true,
            };
            if poisoned {
                break;
            }
            ctx.request_repaint();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_steps_ten_ms_and_caps_waveform() {
        let mut s = UiState::new();
        for _ in 0..(WAVEFORM_LEN + 50) {
            advance(&mut s);
        }
        assert_eq!(s.frame, (WAVEFORM_LEN + 50) as u64);
        assert!((s.t_s - 0.01 * (WAVEFORM_LEN + 50) as f32).abs() < 1e-3);
        assert_eq!(s.samples.len(), WAVEFORM_LEN);
        assert!((0.5..=1.0).contains(&s.level));
    }

    #[test]
    fn level_db_floor_is_finite() {
        let s = UiState::new();
        assert!(s.level_db().is_finite());
    }

    #[test]
    fn producer_thread_pushes_and_requests_repaint() {
        let state: SharedState = Arc::new(Mutex::new(UiState::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ctx = egui::Context::default();
        let handle = spawn_producer(Arc::clone(&state), ctx, Arc::clone(&stop));
        std::thread::sleep(Duration::from_millis(55));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let joined = handle.join();
        assert!(joined.is_ok());
        let frame = state.lock().map(|s| s.frame).unwrap_or(0);
        assert!(frame >= 2, "expected >=2 ticks in 55 ms, got {frame}");
    }

    /// Regression guard for the TSK-306 section 2 footgun: timer-based
    /// `request_repaint_after*` pacing must never reappear in this crate.
    /// Event-driven `request_repaint()` (data arrival) is the only
    /// allowed wake-up.
    #[test]
    fn no_timer_pacing_in_crate_sources() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src = manifest.join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src];
        // Built by parts so this very detector does not trip itself.
        let needle = ["request_repaint", "after"].join("_");
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir).unwrap_or_else(|_| {
                panic!("cannot list {}", dir.display());
            });
            for entry in entries {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("read rs");
                    for (i, line) in text.lines().enumerate() {
                        let code = line.split("//").next().unwrap_or("");
                        if code.contains(&needle) {
                            offenders.push(format!("{}:{}", path.display(), i + 1));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "timer pacing forbidden (TSK-306 section 2), found at: {offenders:?}"
        );
    }
}
