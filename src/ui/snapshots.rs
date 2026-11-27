//! PNG snapshots of the settings window in its main states, from fake monitors and devices, so
//! agents and reviewers can see the UI. Rendered with wgpu on a software rasteriser (lavapipe);
//! `UPDATE_SNAPSHOTS=1 cargo test snapshot` rewrites them in `tests/snapshots/`.

use super::*;
use crate::mirror::StandInMirror;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

/// The sender's graphics card, and the processor's integrated graphics the card display is on.
const RTX_4080: &str = "NVIDIA GeForce RTX 4080";
const INTEGRATED: &str = "Integrated graphics";

fn monitor(
    device_path: &str,
    name: &str,
    size: (u32, u32),
    hz: u32,
    at: (i32, i32),
    graphics_card: &str,
) -> Monitor {
    Monitor {
        device_path: device_path.into(),
        gdi_name: String::new(),
        name: name.into(),
        width: size.0,
        height: size.1,
        refresh_hz: hz,
        primary: at == (0, 0),
        left: at.0,
        top: at.1,
        graphics_card: graphics_card.into(),
    }
}

/// The real sender: a 1440p monitor left of the primary 1440p monitor, the 500 Hz source monitor
/// right of it, all on the RTX 4080, and the capture card's display on the integrated graphics.
fn sender(status: Status) -> SettingsWindow {
    let mut window = super::tests::window("card");
    window.monitors = vec![
        monitor(
            "side",
            "MSI MAG 272Q X50",
            (2560, 1440),
            144,
            (-2560, 0),
            RTX_4080,
        ),
        monitor(
            "primary",
            "Dell S2417DG",
            (2560, 1440),
            144,
            (0, 0),
            RTX_4080,
        ),
        monitor(
            "source",
            "Dell AW2523HF",
            (1920, 1080),
            500,
            (2560, 180),
            RTX_4080,
        ),
        monitor("card", "HD60 X", (1920, 1080), 60, (4480, 180), INTEGRATED),
    ];
    window.playback_devices = vec![
        AudioDevice {
            id: "speakers".into(),
            name: "Speakers (Realtek)".into(),
            description: "Speakers".into(),
        },
        AudioDevice {
            id: "headset".into(),
            name: "Headset".into(),
            description: "Headphones".into(),
        },
    ];
    window.settings.send_windows_to = Some("primary".into());
    window.settings.audio_source = Some("speakers".into());
    window.mirror = Box::new(StandInMirror {
        running: status == Status::Running,
        ..StandInMirror::default()
    });
    window.status = StatusLine::new(status);
    window
}

fn harness(window: SettingsWindow) -> Harness<'static, SettingsWindow> {
    Harness::builder()
        .with_size([780.0, 720.0])
        .build_eframe(|creation| {
            look::apply(&creation.egui_ctx);
            window
        })
}

fn snapshot(name: &str, window: SettingsWindow) {
    let mut harness = harness(window);
    harness.run();
    harness.snapshot(name);
}

#[test]
fn snapshot_stopped() {
    snapshot("stopped", sender(Status::Stopped));
}

#[test]
fn snapshot_starting() {
    snapshot("starting", sender(Status::Starting));
}

#[test]
fn snapshot_running() {
    let mut window = sender(Status::Running);
    window.frame_rate.shown = Some(60.0);
    snapshot("running", window);
}

#[test]
fn snapshot_waiting_for_card_display() {
    let mut window = sender(Status::WaitingForCardDisplay);
    window
        .monitors
        .retain(|monitor| monitor.device_path != "card");
    snapshot("waiting_for_card_display", window);
}

#[test]
fn snapshot_error() {
    let window = sender(Status::Error(
        "Could not start the mirror: the graphics driver is too old. Update it. (Direct3D 11.1)"
            .into(),
    ));
    snapshot("error", window);
}

#[test]
fn snapshot_invalid_roles() {
    let mut window = sender(Status::Stopped);
    window.settings.send_windows_to = Some("card".into());
    snapshot("invalid_roles", window);
}

#[test]
fn snapshot_guard_notice() {
    let mut window = sender(Status::Running);
    window.frame_rate.shown = Some(60.0);
    window.status.guard = Some(
        "\"Discord\" keeps going back to the card display; the guard stopped moving it.".into(),
    );
    snapshot("guard_notice", window);
}

#[test]
fn snapshot_role_menu_opens_on_a_monitor_click() {
    let mut harness = harness(sender(Status::Stopped));
    harness.get_by_label("Dell S2417DG").click();
    harness.run();
    harness.snapshot("role_menu");
}

#[test]
fn the_guard_switch_is_read_once_and_its_text_toggles_it() {
    let mut harness = Harness::new_ui_state(
        |ui, on: &mut bool| {
            live::switch(ui, on, "Guard", "Keeps windows off the card display");
        },
        true,
    );
    harness.run();
    // One node named "Guard": `get_by_label` fails on two.
    let switch = harness.get_by_label("Guard");
    // A click lands on the node's centre, which is on the text right of the 44-point track.
    assert!(switch.rect().center().x > switch.rect().left() + 44.0);
    switch.click();
    harness.run();
    assert!(!*harness.state());
}
