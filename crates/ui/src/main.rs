//! `synthlm-ui` binary: wires the 100 Hz producer to the egui window.
//!
//! Repaints are driven purely by producer data arrival
//! ([`synthlm_ui::spawn_producer`] calls `request_repaint` after each
//! push); no `request_repaint_after` timer pacing exists in this crate
//! (TSK-306 §2).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn main() -> anyhow::Result<()> {
    let state: synthlm_ui::SharedState =
        Arc::new(std::sync::Mutex::new(synthlm_ui::UiState::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let producer_state = Arc::clone(&state);
    let producer_stop = Arc::clone(&stop);
    let handle_slot: Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let slot_writer = Arc::clone(&handle_slot);

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
            Ok(Box::new(synthlm_ui::SynthApp::new(state, stop)) as Box<dyn eframe::App>)
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
