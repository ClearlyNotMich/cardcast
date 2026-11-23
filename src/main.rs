//! cardcast: mirror the source monitor onto the card display and keep other windows off it.
// Release builds are a GUI program, so Windows opens no console window beside the settings window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod guard;
mod mirror;
mod ui;

fn main() -> eframe::Result {
    ui::run()
}
