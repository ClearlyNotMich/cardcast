//! PNG snapshots of the settings window in its main states, from fake monitors and devices, so
//! agents and reviewers can see the UI. Rendered with wgpu on a software rasteriser (lavapipe);
//! `UPDATE_SNAPSHOTS=1 cargo test snapshot` rewrites them in `tests/snapshots/`, and
//! `UPDATE_README_MEDIA=1 cargo test readme_media` the README's media in `docs/media/`.

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
        layout_size: (size.0, size.1),
        refresh_hz: hz,
        primary: at == (0, 0),
        left: at.0,
        top: at.1,
        graphics_card: graphics_card.into(),
        container_id: String::new(),
        connector: String::new(),
    }
}

/// The real sender, as measured on it: a 1440p monitor left of the primary 500 Hz 1440p monitor,
/// the 360 Hz source monitor right of it, all on the RTX 4080, and the capture card's display on
/// the integrated graphics.
fn sender(status: Status) -> SettingsWindow {
    let mut window = super::tests::window("card");
    window.monitors = vec![
        monitor(
            "side",
            "Dell S2417DG",
            (2560, 1440),
            144,
            (-2560, 0),
            RTX_4080,
        ),
        monitor(
            "primary",
            "MSI MAG 272Q X50",
            (2560, 1440),
            500,
            (0, 0),
            RTX_4080,
        ),
        monitor(
            "source",
            "Dell AW2523HF",
            (1920, 1080),
            360,
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

/// Input switching on: the source monitor (DisplayPort 1, HDMI 1, HDMI 2) and the primary monitor
/// (DisplayPort 1, HDMI 1) have input lists, the others don't answer DDC/CI.
fn with_input_lists(mut window: SettingsWindow) -> SettingsWindow {
    window.settings.input_switching = true;
    window.input_lists = window
        .monitors
        .iter()
        .map(|monitor| Probe {
            device_path: monitor.device_path.clone(),
            id: input::MonitorId {
                serial: (monitor.device_path == "source").then(|| "4GZY5P3".into()),
                name: monitor.name.clone(),
            },
            inputs: match monitor.device_path.as_str() {
                "source" => Some(vec![0x0F, 0x11, 0x12]),
                "primary" => Some(vec![0x0F, 0x11]),
                _ => None,
            },
        })
        .collect();
    window
}

#[test]
fn snapshot_input_switch_button() {
    let mut window = with_input_lists(sender(Status::Running));
    window.frame_rate.shown = Some(60.0);
    snapshot("input_switch_button", window);
}

#[test]
fn snapshot_settings_cog_open() {
    let _device = one_device_at_a_time();
    let mut harness = harness(with_input_lists(sender(Status::Stopped)));
    harness.get_by_label("Settings").click();
    harness.run();
    harness.snapshot("settings_cog_open");
}

#[test]
fn snapshot_input_menu_ticks_the_current_input() {
    let _device = one_device_at_a_time();
    let mut window = with_input_lists(sender(Status::Stopped));
    window.current_input = Some(("source".into(), 0x0F));
    let mut harness = harness(window);
    harness.get_by_label("Inputs of Dell AW2523HF").click();
    harness.run();
    // The input button opens its own menu; egui keeps one menu open, so not the tile's role menu.
    assert!(harness.query_by_label("HDMI 2").is_some());
    harness.snapshot("input_menu");
}

#[test]
fn the_input_menu_reads_the_current_input_as_it_opens_and_switches() {
    let _device = one_device_at_a_time();
    let mut window = with_input_lists(sender(Status::Stopped));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let recording = opened.clone();
    window.input_thread = Some(
        input::InputThread::spawn(
            |_: &Monitor| None,
            move |monitor: &Monitor| {
                recording.lock().unwrap().push(monitor.device_path.clone());
                None
            },
            || {},
        )
        .unwrap(),
    );
    let mut harness = harness(window);
    let wait_for = |count: usize| {
        let start = Instant::now();
        while opened.lock().unwrap().len() < count {
            assert!(
                start.elapsed().as_secs() < 10,
                "{:?}",
                opened.lock().unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    harness.get_by_label("Inputs of Dell AW2523HF").click();
    harness.run();
    wait_for(1);
    harness.get_by_label("HDMI 1").click();
    harness.run();
    wait_for(2);
    assert_eq!(*opened.lock().unwrap(), ["source", "source"]);
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

/// The README's media in `docs/media/`: an animated hero of the running settings window and three
/// stills. Only `UPDATE_README_MEDIA=1 cargo test readme_media` writes them.
#[test]
fn readme_media() {
    // One loop is two trips of the dots (1.4 s each); 25 frames a second.
    const FRAMES: u16 = 70;
    const FPS: u16 = 25;
    let write = std::env::var_os("UPDATE_README_MEDIA").is_some();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/media");
    let _device = one_device_at_a_time();

    let mut window = sender(Status::Running);
    window.frame_rate.shown = Some(60.0);
    let mut live = harness(window);
    live.run();
    let mut frames = Vec::new();
    for frame in 0..FRAMES {
        live.input_mut().time = Some(f64::from(frame) / f64::from(FPS));
        live.step();
        frames.push(live.render().unwrap());
    }
    drop(live);
    // The dots move from frame to frame.
    assert_ne!(frames[0], frames[1]);
    if write {
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join("hero.png")).unwrap();
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(file),
            frames[0].width(),
            frames[0].height(),
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_compression(png::Compression::High);
        encoder.set_animated(u32::from(FRAMES), 0).unwrap();
        encoder.set_frame_delay(1, FPS).unwrap();
        encoder.set_blend_op(png::BlendOp::Over).unwrap();
        let mut writer = encoder.write_header().unwrap();
        let mut previous: Option<&[u8]> = None;
        for frame in &frames {
            // Each frame after the first keeps only the pixels that changed; the rest are
            // transparent, so the frame blends over the one before and compresses to little.
            let pixels: Vec<u8> = match previous {
                None => frame.as_raw().clone(),
                Some(previous) => frame
                    .as_raw()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(previous.as_chunks::<4>().0)
                    .flat_map(|(now, then)| if now == then { [0; 4] } else { *now })
                    .collect(),
            };
            writer.write_image_data(&pixels).unwrap();
            previous = Some(frame.as_raw());
        }
        writer.finish().unwrap();
    }

    let stills: [(&str, SettingsWindow, Option<&str>); 3] = [
        (
            "sound_output",
            {
                let mut window = sender(Status::Running);
                window.frame_rate.shown = Some(60.0);
                window.choosing_sound_output = Some(SoundOutput::Automatic);
                window
            },
            None,
        ),
        (
            "input_menu",
            {
                let mut window = with_input_lists(sender(Status::Running));
                window.frame_rate.shown = Some(60.0);
                window.current_input = Some(("source".into(), 0x0F));
                window
            },
            Some("Inputs of Dell AW2523HF"),
        ),
        (
            "settings_cog",
            with_input_lists(sender(Status::Stopped)),
            Some("Settings"),
        ),
    ];
    for (name, window, click) in stills {
        let mut harness = harness(window);
        if let Some(label) = click {
            harness.get_by_label(label).click();
        }
        harness.run();
        let image = harness.render().unwrap();
        if write {
            image.save(dir.join(format!("{name}.png"))).unwrap();
        }
    }
}
