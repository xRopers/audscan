// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use audscan_gui::App;
use audscan_gui::app::Action;

fn main() -> eframe::Result {
    // `audscan-gui [FILE]`: FILE opens straight away.
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("audscan").with_inner_size([1360.0, 860.0]).with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "audscan",
        options,
        Box::new(move |_cc| {
            let mut app = App::new();
            if let Some(path) = arg {
                app.request(Action::OpenFile(path));
            }
            Ok(Box::new(app))
        }),
    )
}
