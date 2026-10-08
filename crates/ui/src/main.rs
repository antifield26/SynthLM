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
//!
//! `--scale <f32>` (TSK-807) forces the egui pixels-per-point override, so
//! a HiDPI screenshot can be taken at any scale without touching the OS
//! display setting: the TSK-306/703 "150% needs a logout" blocker becomes
//! `synthlm-ui --scale 1.5` plus a screenshot. Without the switch, the
//! native monitor scale is used unchanged. The effective value is printed
//! on the first frame as `FIRST_FRAME PPP=<value> ZOOM=<value>` (see
//! [`synthlm_ui::SynthApp`]), which is the evidence to file next to the
//! screenshot.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Environment variable naming a plan JSON file to load instead of the
/// embedded demo plan.
const PLAN_PATH_ENV: &str = "SYNTHLM_UI_PLAN";

/// Command-line switch overriding the egui scale factor: `--scale <f32>`
/// (also accepted as `--scale=<f32>`).
///
/// The value is the requested `pixels_per_point`. egui derives it as
/// `zoom_factor * native_pixels_per_point`, and the switch sets the zoom
/// factor so that the product equals the requested value on any monitor:
/// `1.5` reproduces a 150% display (or a 150% UI on top of any native
/// scale) without an OS setting change and without a logout.
const SCALE_ARG: &str = "--scale";

/// Embedded demo plan (same shape the loader expects at runtime).
const DEMO_PLAN: &str = include_str!("../../../experiments/e2e-demo/demo-plan.json");

/// Load the startup plan report plus an optional whole-file error banner.
///
/// Override path wins when [`crate::main::PLAN_PATH_ENV`] names a readable file with
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

/// Parse the `--scale` switch out of `args` (program name included or not,
/// it is not inspected).
///
/// Returns `Ok(None)` when the switch is absent — the native monitor scale
/// then applies unchanged — and `Ok(Some(scale))` for a valid finite
/// `scale > 0.0`. Unknown arguments are ignored. Errors are explicit and
/// name the offending text, so a typo never silently falls back to 1.0.
///
/// # Errors
///
/// Fails when `--scale` has no following value, when the value does not
/// parse as `f32`, or when it is non-finite (`nan`/`inf`) or `<= 0.0`
/// (egui cannot lay out at such a scale).
fn parse_scale(args: &[String]) -> anyhow::Result<Option<f32>> {
    let mut requested: Option<f32> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let raw = if arg == SCALE_ARG {
            let Some(value) = args.get(index + 1) else {
                anyhow::bail!("{SCALE_ARG} requires a value, e.g. `{SCALE_ARG} 1.5`");
            };
            index += 2;
            value.as_str()
        } else if let Some(value) = arg
            .strip_prefix(SCALE_ARG)
            .and_then(|rest| rest.strip_prefix('='))
        {
            index += 1;
            value
        } else {
            index += 1;
            continue;
        };
        let value: f32 = raw.parse().map_err(|_| {
            anyhow::anyhow!("{SCALE_ARG} value '{raw}' is not a number (expected e.g. 1.5)")
        })?;
        if !value.is_finite() || value <= 0.0 {
            anyhow::bail!("{SCALE_ARG} value '{raw}' must be a finite number > 0 (e.g. 1.5)");
        }
        requested = Some(value);
    }
    Ok(requested)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scale_override = parse_scale(&args)?;

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
            if let Some(scale) = scale_override {
                // egui applies the new zoom factor at the start of the next
                // pass; the window is created before the app, so the first
                // frame already renders at the requested pixels-per-point.
                cc.egui_ctx.set_pixels_per_point(scale);
            }
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

#[cfg(test)]
mod tests {
    use super::parse_scale;

    /// Build the argument vector the way `main` sees it.
    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn scale_absent_keeps_native_scale() {
        assert_eq!(parse_scale(&args(&[])).ok(), Some(None));
        assert_eq!(parse_scale(&args(&["--verbose"])).ok(), Some(None));
    }

    #[test]
    fn scale_accepts_both_spellings() {
        assert_eq!(
            parse_scale(&args(&["--scale", "1.5"])).ok(),
            Some(Some(1.5))
        );
        assert_eq!(parse_scale(&args(&["--scale=1.5"])).ok(), Some(Some(1.5)));
        assert_eq!(parse_scale(&args(&["--scale", "2"])).ok(), Some(Some(2.0)));
    }

    #[test]
    fn scale_rejects_missing_unparseable_and_non_positive() {
        for bad in [
            vec!["--scale"],
            vec!["--scale", "big"],
            vec!["--scale", "0"],
            vec!["--scale", "-1.5"],
            vec!["--scale", "nan"],
            vec!["--scale", "inf"],
            vec!["--scale", "-inf"],
        ] {
            assert!(
                parse_scale(&args(&bad)).is_err(),
                "expected rejection for {bad:?}"
            );
        }
    }
}
