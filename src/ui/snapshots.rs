//! PNG snapshots of the settings window in its main states, from fake monitors and devices, so
//! agents and reviewers can see the UI. Rendered with wgpu on a software rasteriser (lavapipe);
//! `UPDATE_SNAPSHOTS=1 cargo test snapshot` rewrites them in `tests/snapshots/`.

use super::*;
use crate::mirror::StandInMirror;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Vulkan loader 1.4.341 (Ubuntu 26.04 CI) frees a device in `vkDestroyDevice` while another
/// thread's device call walks the loader's device list, a SIGSEGV (fixed in
/// KhronosGroup/Vulkan-Loader#1866), so a test holds this while its harness owns a device.
fn one_device_at_a_time() -> MutexGuard<'static, ()> {
    static DEVICE: Mutex<()> = Mutex::new(());
    // A failed snapshot poisons it; the other snapshots should still run.
    DEVICE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sender's graphics card, and the processor's integrated graphics the card display is on.
const RTX_4080: &str = "NVIDIA GeForce RTX 4080";
const INTEGRATED: &str = "Integrated graphics";
/// The card display and its playback device share this container ID.
const CARD_CONTAINER: &str = "5D9736D1-BB58-4944-B8BA-CE1857311DC7";

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
        container_id: String::new(),
        connector: String::new(),
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
        Monitor {
            container_id: CARD_CONTAINER.into(),
            ..monitor(
                "card",
                "Elgato 1080p",
                (1920, 1080),
                60,
                (4480, 180),
                INTEGRATED,
            )
        },
    ];
    window.playback_devices = vec![
        AudioDevice {
            id: "speakers".into(),
            name: "Speakers (Realtek)".into(),
            description: "Speakers".into(),
            container_id: String::new(),
        },
        AudioDevice {
            id: "headset".into(),
            name: "Headset".into(),
            description: "Headphones".into(),
            container_id: String::new(),
        },
        // In Display EDID mode the card display reads "Elgato 1080p", its playback device "HD60 X".
        AudioDevice {
            id: "hdmi".into(),
            name: "HD60 X (Intel Display Audio)".into(),
            description: "HD60 X".into(),
            container_id: CARD_CONTAINER.into(),
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

/// Callers hold `one_device_at_a_time()` for as long as the harness lives.
fn harness(window: SettingsWindow) -> Harness<'static, SettingsWindow> {
    Harness::builder()
        .with_size([780.0, 780.0])
        .build_eframe(|creation| {
            look::apply(&creation.egui_ctx);
            window
        })
}

fn snapshot(name: &str, window: SettingsWindow) {
    let _device = one_device_at_a_time();
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

/// The one clash left to warn about: Windows made the card display the primary monitor.
#[test]
fn snapshot_card_display_is_primary() {
    let mut window = sender(Status::Stopped);
    for monitor in &mut window.monitors {
        monitor.primary = monitor.device_path == "card";
    }
    snapshot("card_display_is_primary", window);
}

/// The card display dropdown leaves out the source monitor and the primary monitor.
#[test]
fn snapshot_card_display_dropdown() {
    let _device = one_device_at_a_time();
    let mut harness = harness(sender(Status::Stopped));
    harness
        .get_by_role_and_label(egui::accesskit::Role::ComboBox, "Card display")
        .click();
    harness.run();
    harness.snapshot("card_display_dropdown");
}

/// A 120 Hz card display that gets about 71 frames a second.
#[test]
fn snapshot_slow_mirror() {
    let mut window = sender(Status::Running);
    window.monitors[3].refresh_hz = 120;
    let start = Instant::now();
    for second in 0..=4 {
        window.frame_rate.sample(
            71 * second,
            120,
            start + std::time::Duration::from_secs(second),
        );
    }
    window.status.slow_mirror = window
        .frame_rate
        .slow_warning(start + std::time::Duration::from_secs(4));
    assert!(window.status.slow_mirror.is_some());
    snapshot("slow_mirror", window);
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
fn snapshot_sound_output_automatic_not_found() {
    let mut window = sender(Status::Running);
    window.frame_rate.shown = Some(60.0);
    // No container ID, and the playback device is not named after the card display.
    window.monitors[3].container_id = String::new();
    let relay = super::tests::RecordingRelay::default();
    relay.1.borrow_mut().push(RelayReport::NoSoundOutput);
    window.relay = Box::new(relay);
    window.follow_reports();
    snapshot("sound_output_automatic_not_found", window);
}

#[test]
fn snapshot_sound_output_dialog() {
    let mut window = sender(Status::Running);
    window.frame_rate.shown = Some(60.0);
    window.choosing_sound_output = Some(SoundOutput::Automatic);
    snapshot("sound_output_dialog", window);
}

#[test]
fn snapshot_sound_output_no_audio() {
    let mut window = sender(Status::Running);
    window.frame_rate.shown = Some(60.0);
    window.settings.sound_output = SoundOutput::NoAudio;
    snapshot("sound_output_no_audio", window);
}

#[test]
fn snapshot_role_menu_opens_on_a_monitor_click() {
    let _device = one_device_at_a_time();
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
