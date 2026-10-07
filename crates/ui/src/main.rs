//! `synthlm-ui` binary: wires the 100 Hz producer to the egui window.
//!
//! Repaints are driven purely by producer data arrival
//! ([`synthlm_ui::spawn_producer`] calls `request_repaint` after each
//! push); no `request_repaint_after` timer pacing exists in this crate
//! (TSK-306 §2).
//!
//! Candidate cards come from plan JSON (TSK-506): the embedded
//! `experiments/e2e-demo/demo-plan.json` by default, or the file named by
//! the `SYNTHLM_UI_PLAN` environment variable when set. A whole-file load
//! failure renders as a red banner with an empty list (never a panic);
//! per-entry failures render as red rows (see
//! [`synthlm_ui::parse_plan_text`]).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Environment variable naming a plan JSON file to load instead of the
/// embedded demo plan.
const PLAN_PATH_ENV: &str = "SYNTHLM_UI_PLAN";

/// Embedded demo plan (same shape the loader expects at runtime).
const DEMO_PLAN: &str = include_str!("../../../experiments/e2e-demo/demo-plan.json");

/// Load the startup plan report plus an optional whole-file error banner.
///
/// Override path wins when [`PLAN_PATH_ENV`] names a readable file with
/// parseable content; any failure falls back to the embedded demo plan,
/// and only a failure of both leaves an empty report with a banner. No
/// path text ever enters the banner (absolute paths stay out of UI copy).
fn load_startup_plan() -> (synthlm_ui::PlanReport, Option<String>) {
    if let Ok(path) = std::env::var(PLAN_PATH_ENV) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if !text.trim().is_empty()
            && let Ok(report) = synthlm_ui::parse_plan_text(&text)
        {
            return (report, None);
        }
        let fallback = synthlm_ui::parse_plan_text(DEMO_PLAN).unwrap_or_default();
        return (
            fallback,
            Some("计划文件读取失败，已显示演示计划".to_owned()),
        );
    }
    match synthlm_ui::parse_plan_text(DEMO_PLAN) {
        Ok(report) => (report, None),
        Err(err) => (
            synthlm_ui::PlanReport::default(),
            Some(format!("计划加载失败：{err}，已显示空列表")),
        ),
    }
}

fn main() -> anyhow::Result<()> {
    let state: synthlm_ui::SharedState =
        Arc::new(std::sync::Mutex::new(synthlm_ui::UiState::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let producer_state = Arc::clone(&state);
    let producer_stop = Arc::clone(&stop);
    let handle_slot: Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let slot_writer = Arc::clone(&handle_slot);

    let (plan, plan_error) = load_startup_plan();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 560.0])
            .with_title("SynthLM"),
        ..Default::default()
    };
    let run_result = eframe::run_native(
        "SynthLM",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            synthlm_ui::fonts::install_cjk(&cc.egui_ctx);
            let handle =
                synthlm_ui::spawn_producer(producer_state, cc.egui_ctx.clone(), producer_stop);
            if let Ok(mut slot) = slot_writer.lock() {
                *slot = Some(handle);
            }
            Ok(
                Box::new(synthlm_ui::SynthApp::new(state, stop, plan, plan_error))
                    as Box<dyn eframe::App>,
            )
        }),
    );
    if let Err(report) = run_result {
        return Err(anyhow::anyhow!("eframe exited with error: {report}"));
    }

    let join_result = handle_slot
        .lock()
        .ok()
        .and_then(|mut slot| slot.take().map(std::thread::JoinHandle::join));
    if join_result.is_some_and(|result| result.is_err()) {
        return Err(anyhow::anyhow!("producer thread panicked"));
    }
    Ok(())
}
