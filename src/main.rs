// A windowed app on Windows, in debug builds too: no console window.
#![windows_subsystem = "windows"]

/// The label for a shortcut on egui's "command" modifier: `cmd_key!("F")` is
/// "⌘F" on macOS and "Ctrl+F" elsewhere.
macro_rules! cmd_key {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("\u{2318}", $key)
        } else {
            concat!("Ctrl+", $key)
        }
    };
}

mod app;
mod app_icon;
mod db;
mod doc;
mod editor;
mod fonts;
mod icons;
mod lock;
mod markdown;
mod model;
mod settings;
mod sync;
mod theme;
#[cfg(any(windows, target_os = "macos"))]
mod webfetch;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Notes")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(app_icon::icon_data(256)),
        ..Default::default()
    };
    eframe::run_native("CleanNotes", options, Box::new(|cc| Ok(app::App::start(cc))))
}
