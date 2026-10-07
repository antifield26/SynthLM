//! Main egui window (TSK-119 scaffold, TSK-506 candidate wiring).
//!
//! Layout: dark theme, waveform strip, 100 Hz status area, candidate cards
//! loaded from plan JSON, single-winner selection, apply/rollback
//! instruction previews, and an audition stub. The
//! [`crate::app::SynthApp`] pass clones the latest
//! [`crate::state::UiState`] under a short lock and paints from the clone,
//! so the UI never blocks the producer thread and never writes shared
//! state. Card data itself is owned by the app (parsed once at startup
//! from plan JSON, see [`crate::cards::parse_plan_text`]); the window only
//! renders it and previews instructions — execution belongs to the REAPER
//! side.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::cards::{ApplyPreview, ListedCandidate, PlanReport, RollbackPreview, Selection};
use crate::state::SharedState;

/// Environment variable bounding an unattended run (seconds).
///
/// Test/QA hook for screenshots and CPU sampling: when set to `N`, the
/// window closes itself `N` seconds after startup. Unset ⇒ run until the
/// user closes the window. This is a shutdown condition, not repaint
/// pacing (repaints stay purely event-driven, TSK-306 §2).
pub const RUN_SECS_ENV: &str = "SYNTHLM_UI_RUN_SECS";

/// Instruction preview currently shown in the preview pane.
#[derive(Clone, Debug, PartialEq)]
enum Preview {
    /// Apply instruction for the winner.
    Apply(ApplyPreview),
    /// Rollback instruction for the winner.
    Rollback(RollbackPreview),
}

impl Preview {
    /// Pane text for the current preview.
    fn render(&self) -> String {
        match self {
            Self::Apply(preview) => preview.render(),
            Self::Rollback(preview) => preview.render(),
        }
    }
}

/// Main window application.
///
/// Owns no audio or model handles; it only renders snapshots produced
/// externally (see [`crate::state::spawn_producer`]) plus the startup
/// plan report. Audition buttons stay disabled on purpose: playback has no
/// render pipeline behind it yet, and a fake play button would lie about
/// functionality.
pub struct SynthApp {
    state: SharedState,
    stop: Arc<AtomicBool>,
    t0: Instant,
    first_frame_logged: bool,
    run_secs: Option<u64>,
    plan: PlanReport,
    plan_error: Option<String>,
    selection: Selection,
    preview: Option<Preview>,
}

impl SynthApp {
    /// Build the app over externally produced `state` and a startup
    /// `plan` report.
    ///
    /// `stop` is flipped on [`crate::app::SynthApp`] exit so the producer
    /// thread (see [`crate::state::spawn_producer`]) terminates.
    /// `plan_error` carries the whole-file load failure, if any (rendered
    /// as a red banner; per-entry failures already live in `plan`).
    #[must_use]
    pub fn new(
        state: SharedState,
        stop: Arc<AtomicBool>,
        plan: PlanReport,
        plan_error: Option<String>,
    ) -> Self {
        let run_secs = std::env::var(RUN_SECS_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        Self {
            state,
            stop,
            t0: Instant::now(),
            first_frame_logged: false,
            run_secs,
            plan,
            plan_error,
            selection: Selection::new(),
            preview: None,
        }
    }

    /// Currently selected winner, resolved against the loaded roster.
    fn winner(&self) -> Option<&ListedCandidate> {
        self.selection.selected().and_then(|id| self.plan.find(id))
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
            egui::ScrollArea::vertical().show(ui, |ui| {
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
                self.cards_view(ui);
            });
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

impl SynthApp {
    /// Candidate cards, winner selection, apply/rollback previews, and the
    /// audition stub (TSK-506).
    fn cards_view(&mut self, ui: &mut egui::Ui) {
        ui.label("候选");

        if let Some(err) = self.plan_error.clone() {
            ui.colored_label(
                egui::Color32::RED,
                format!("计划加载失败：{err}，已显示空列表"),
            );
        }
        for rejected in self.plan.rejected().to_vec() {
            ui.colored_label(
                egui::Color32::RED,
                format!(
                    "条目 {} 构造失败：{}（已标红跳过）",
                    rejected.index(),
                    rejected.reason()
                ),
            );
        }

        if self.plan.candidates().is_empty() {
            ui.label("暂无候选 — 等待求解结果");
        } else {
            ui.label("试听按钮置灰原因：播放实现留空，仅显示引用，不伪装功能");
            for entry in self.plan.candidates().to_vec() {
                self.card_row(ui, &entry);
            }
            self.action_bar(ui);
            self.preview_pane(ui);
        }
    }

    /// One six-field card (DEC-019): the title carries the difference
    /// sentence; the audition button is disabled with its reason.
    fn card_row(&mut self, ui: &mut egui::Ui, entry: &ListedCandidate) {
        let id = entry.id().to_owned();
        let is_winner = self.selection.selected() == Some(id.as_str());
        ui.group(|ui| {
            ui.label(format!("候选 {id} ｜ {}", entry.card().diff()));
            ui.label(format!(
                "置信度 {:.0}% ｜ ΔLUFS {:+.1} ｜ 改动参数 {}",
                entry.card().confidence() * 100.0,
                entry.card().delta_lufs(),
                entry.card().changed()
            ));
            ui.horizontal(|ui| {
                ui.add_enabled(false, egui::Button::new("试听"));
                ui.label(format!("引用：{}", entry.card().audio_ref()));
            });
            ui.horizontal(|ui| {
                if is_winner {
                    ui.label("已胜出");
                    if ui.button("取消选择").clicked() {
                        self.selection.clear();
                        self.preview = None;
                    }
                } else if ui.button("选为胜出").clicked()
                    && self.selection.select_known(&id, self.plan.candidates())
                {
                    self.preview = None;
                }
            });
        });
    }

    /// Winner action bar: apply/rollback preview buttons (preview only;
    /// the REAPER side executes).
    fn action_bar(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        match self.winner().map(|entry| entry.id().to_owned()) {
            Some(id) => {
                ui.label(format!("胜出候选：{id}"));
                ui.horizontal(|ui| {
                    if ui.button("应用").clicked()
                        && let Some(winner) = self.winner()
                    {
                        self.preview = Some(Preview::Apply(ApplyPreview::for_winner(winner)));
                    }
                    if ui.button("回滚").clicked()
                        && let Some(winner) = self.winner()
                    {
                        self.preview = Some(Preview::Rollback(RollbackPreview::for_winner(winner)));
                    }
                });
            }
            None => {
                ui.label("胜出候选：未选择");
                ui.add_enabled(false, egui::Button::new("应用"));
                ui.add_enabled(false, egui::Button::new("回滚"));
                ui.label("请先选择胜出候选");
            }
        }
    }

    /// Instruction preview pane (DEC-008 transaction + DEC-020
    /// granularity, text only, never executed here).
    fn preview_pane(&self, ui: &mut egui::Ui) {
        if let Some(preview) = &self.preview {
            ui.separator();
            ui.label("指令预览（仅预览，不执行）");
            ui.monospace(preview.render());
        }
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
