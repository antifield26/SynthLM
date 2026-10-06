//! Main egui window (TSK-119).
//!
//! Layout: dark theme, waveform strip, 100 Hz status area, candidate
//! placeholder list with empty-state copy. The [`crate::app::SynthApp`]
//! pass clones the latest [`crate::state::UiState`] under a short lock and
//! paints from the clone, so the UI never blocks the producer thread and
//! never writes shared state.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::state::SharedState;

/// Environment variable bounding an unattended run (seconds).
///
/// Test/QA hook for screenshots and CPU sampling: when set to `N`, the
/// window closes itself `N` seconds after startup. Unset ⇒ run until the
/// user closes the window. This is a shutdown condition, not repaint
/// pacing (repaints stay purely event-driven, TSK-306 §2).
pub const RUN_SECS_ENV: &str = "SYNTHLM_UI_RUN_SECS";

/// Main window application.
///
/// Owns no audio or model handles; it only renders snapshots produced
/// externally (see [`crate::state::spawn_producer`]).
pub struct SynthApp {
    state: SharedState,
    stop: Arc<AtomicBool>,
    t0: Instant,
    first_frame_logged: bool,
    run_secs: Option<u64>,
    demo_clicks: u32,
}

impl SynthApp {
    /// Build the app over externally produced `state`.
    ///
    /// `stop` is flipped on [`crate::app::SynthApp`] exit so the producer
    /// thread (see [`crate::state::spawn_producer`]) terminates.
    #[must_use]
    pub fn new(state: SharedState, stop: Arc<AtomicBool>) -> Self {
        let run_secs = std::env::var(RUN_SECS_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        Self {
            state,
            stop,
            t0: Instant::now(),
            first_frame_logged: false,
            run_secs,
            demo_clicks: 0,
        }
    }
}

impl eframe::App for SynthApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            eprintln!(
                "FIRST_FRAME PPP={:.3} ZOOM={:.3}",
                ctx.pixels_per_point(),
                ctx.zoom_factor()
            );
        }

        let snapshot = match self.state.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => {
                ui.label("state unavailable (lock poisoned)");
                return;
            }
        };

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("SynthLM 主控");
            ui.label("独立状态窗 · 事件驱动重绘");
            ui.separator();

            ui.label("波形");
            waveform(ui, &snapshot.samples);

            ui.separator();
            ui.label("状态（100Hz）");
            ui.monospace(format!(
                "帧={} 电平={:.3}（{:.1} dB） 缩放={:.3}",
                snapshot.frame,
                snapshot.level,
                snapshot.level_db(),
                ctx.pixels_per_point()
            ));
            ui.label("生产者 10ms/推 · 事件驱动重绘");
            ui.label("滤波器 截止 Fc=440Hz");

            ui.separator();
            ui.label("候选");
            if snapshot.candidates.is_empty() {
                ui.label("暂无候选 — 等待求解结果");
            } else {
                for candidate in &snapshot.candidates {
                    ui.group(|ui| {
                        ui.label(format!("{} ｜ {}", candidate.id, candidate.diff_sentence));
                        ui.label(format!(
                            "置信度 {:.0}% ｜ ΔLUFS {:+.1} ｜ 改动参数 {}",
                            candidate.confidence * 100.0,
                            candidate.delta_lufs,
                            candidate.changed_params
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("试听").clicked() {
                                self.demo_clicks += 1;
                            }
                            if ui.button("应用").clicked() {
                                self.demo_clicks += 1;
                            }
                            if ui.button("回滚").clicked() {
                                self.demo_clicks += 1;
                            }
                        });
                    });
                }
            }
            ui.label(format!("演示点击 {} 次", self.demo_clicks));
        });

        if self
            .run_secs
            .is_some_and(|secs| self.t0.elapsed() >= Duration::from_secs(secs))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Paint the waveform strip from a read-only sample snapshot.
///
/// Newest sample is at the right edge; the strip is fixed dark fill with a
/// cyan polyline (same visual language as the TSK-306 prototype).
fn waveform(ui: &mut egui::Ui, samples: &std::collections::VecDeque<f32>) {
    let width = ui.available_width().max(100.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 200.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(16, 18, 24));
    let n = samples.len();
    if n < 2 {
        return;
    }
    let denom = (n - 1) as f32;
    let mut prev: Option<egui::Pos2> = None;
    for (i, v) in samples.iter().enumerate() {
        let x = rect.min.x + rect.width() * (i as f32 / denom);
        let y = rect.center().y - v.clamp(-1.0, 1.0) * rect.height() * 0.42;
        let point = egui::pos2(x, y);
        if let Some(q) = prev {
            painter.line_segment(
                [q, point],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(0, 220, 255)),
            );
        }
        prev = Some(point);
    }
}
