#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adb;
mod app;
mod config;
mod pairing;
mod scrcpy;
mod updater;

use app::GScrcpyApp;
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title("gscrcpy - scrcpy 启动器"),
        ..Default::default()
    };
    eframe::run_native(
        "gscrcpy",
        options,
        Box::new(|cc| Ok(Box::new(GScrcpyApp::new(cc)))),
    )
}
