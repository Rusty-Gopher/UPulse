//! UPulse — a clean, GPU-rendered control center for your Ubuntu system.
//!
//! Shows disks, memory, CPU, network, and running processes at a glance.

// Don't spawn a console window on Windows release builds (harmless on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod apps;
mod cleanup;
mod icons;
mod kernels;
mod metrics;
mod repos;
mod scan;
mod startup;
mod system;
mod theme;
mod updates;
mod util;

use app::App;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UPulse")
            .with_app_id("upulse")
            .with_inner_size([1040.0, 780.0])
            .with_min_inner_size([860.0, 580.0]),
        ..Default::default()
    };

    eframe::run_native("UPulse", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
