//! cardcast: mirror the source monitor onto the card display and keep other windows off it.
// Release builds are a GUI program, so Windows opens no console window beside the settings window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod guard;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Copy command (issue #39) and Linux DDC/CI (issue #46) call the rest; its tests run everywhere"
    )
)]
mod input;
mod mirror;
mod ui;

fn main() -> eframe::Result {
    // With arguments cardcast is in command mode: one input switch, no window (SPEC section 11).
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if !args.is_empty() {
        std::process::exit(input::command(&args));
    }
    ui::run()
}
