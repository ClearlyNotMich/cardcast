//! cardcast: mirror the source monitor onto the card display and keep other windows off it.
// Release builds are a GUI program, so Windows opens no console window beside the settings window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod guard;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "command mode (issue #38) calls it; its tests run everywhere"
    )
)]
mod input;
mod mirror;
mod ui;

fn main() -> eframe::Result {
    ui::run()
}
