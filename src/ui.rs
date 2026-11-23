//! ui module. No unsafe and no Windows calls here.

mod settings;

use crate::audio::{self, AudioDevice};
use crate::guard::{self, Guard, Monitor};
use crate::mirror::{self, Mirror};
use eframe::egui;
use settings::{Follow, Settings, Status};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Opens the settings window; returns when it is closed, which quits cardcast.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([460.0, 220.0]),
        ..Default::default()
    };
    eframe::run_native(
        "cardcast",
        options,
        Box::new(|creation| {
            let mut window = SettingsWindow::new(&creation.egui_ctx);
            window.launch(&creation.egui_ctx);
            Ok(Box::new(window))
        }),
    )
}

struct SettingsWindow {
    settings: Settings,
    monitors: Vec<Monitor>,
    playback_devices: Vec<AudioDevice>,
    status: Status,
    mirror: Box<dyn Mirror>,
    guard: Box<dyn Guard>,
    /// True from Start (pressed or at launch) until Stop is pressed, including while waiting for the card display.
    wanted: bool,
    displays_changed: Arc<AtomicBool>,
    _display_watcher: guard::DisplayWatcher,
}

impl SettingsWindow {
    fn new(context: &egui::Context) -> Self {
        let mut status = Status::Stopped;
        let monitors = guard::monitors().unwrap_or_else(|error| {
            status = Status::Error(error);
            Vec::new()
        });
        let playback_devices = audio::playback_devices().unwrap_or_else(|error| {
            status = Status::Error(error);
            Vec::new()
        });
        let settings = Settings::load().with_defaults(&monitors);
        let guard_context = context.clone();
        let mut guard = guard::new_guard(move || guard_context.request_repaint());
        update_guard(&mut *guard, &settings);
        let displays_changed = Arc::new(AtomicBool::new(false));
        let display_watcher = {
            let (context, displays_changed) = (context.clone(), displays_changed.clone());
            guard::DisplayWatcher::new(move || {
                displays_changed.store(true, Ordering::Relaxed);
                context.request_repaint();
            })
        };
        Self {
            settings,
            monitors,
            playback_devices,
            status,
            mirror: mirror::new_mirror(),
            guard,
            wanted: false,
            displays_changed,
            _display_watcher: display_watcher,
        }
    }

    /// Starts the mirror from saved settings; the window starts minimised if it runs.
    fn launch(&mut self, context: &egui::Context) {
        if settings::starts_on_launch(&self.settings) {
            self.start();
            if self.status == Status::Running {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
    }

    /// The one place the mirror starts: Start, launch and the card display coming back all call it.
    fn start(&mut self) {
        self.wanted = true;
        self.status = settings::start(&self.settings, &self.monitors, &mut *self.mirror);
    }

    /// The one place the mirror stops: Stop and the card display going away both call it.
    fn stop(&mut self, wanted: bool, status: Status) {
        self.mirror.stop();
        self.wanted = wanted;
        self.status = status;
    }

    /// Re-reads the monitors after a display change and follows the card display.
    fn follow_displays(&mut self) {
        let monitors = match guard::monitors() {
            Ok(monitors) => monitors,
            Err(error) => {
                self.status = Status::Error(error);
                return;
            }
        };
        let card = |monitors: &[Monitor]| {
            monitors
                .iter()
                .find(|monitor| self.settings.card_display.as_ref() == Some(&monitor.device_path))
                .cloned()
        };
        let follow = settings::follow_card_display(
            self.wanted,
            self.mirror.is_running(),
            card(&self.monitors).as_ref(),
            card(&monitors).as_ref(),
        );
        self.monitors = monitors;
        match follow {
            Follow::Start => self.start(),
            Follow::Stop => self.stop(true, Status::WaitingForCardDisplay),
            Follow::Nothing => {}
        }
    }

    fn monitor_dropdown(&mut self, ui: &mut egui::Ui, label: &str, choice: Choice) {
        let selected = match choice {
            Choice::SourceMonitor => &mut self.settings.source_monitor,
            Choice::CardDisplay => &mut self.settings.card_display,
            Choice::SendWindowsTo => &mut self.settings.send_windows_to,
        };
        let shown = self
            .monitors
            .iter()
            .find(|monitor| selected.as_deref() == Some(&monitor.device_path))
            .map_or_else(|| "(choose)".to_owned(), monitor_label);
        ui.label(label);
        egui::ComboBox::from_id_salt(label)
            .selected_text(shown)
            .width(300.0)
            .show_ui(ui, |ui| {
                for monitor in &self.monitors {
                    ui.selectable_value(
                        selected,
                        Some(monitor.device_path.clone()),
                        monitor_label(monitor),
                    );
                }
            });
        ui.end_row();
    }

    fn audio_dropdown(&mut self, ui: &mut egui::Ui) {
        let selected = &mut self.settings.audio_source;
        let shown = self
            .playback_devices
            .iter()
            .find(|device| selected.as_deref() == Some(&device.id))
            .map_or("(choose)", |device| &device.name);
        ui.label("Audio source");
        egui::ComboBox::from_id_salt("Audio source")
            .selected_text(shown)
            .width(300.0)
            .show_ui(ui, |ui| {
                for device in &self.playback_devices {
                    ui.selectable_value(selected, Some(device.id.clone()), &device.name);
                }
            });
        ui.end_row();
    }

    fn running(&self) -> bool {
        self.wanted && !matches!(self.status, Status::Error(_))
    }
}

fn update_guard(guard: &mut dyn Guard, settings: &Settings) {
    guard.update(
        settings.guard_enabled,
        settings.card_display.as_deref(),
        settings.send_windows_to.as_deref(),
    );
}

#[derive(Clone, Copy)]
enum Choice {
    SourceMonitor,
    CardDisplay,
    SendWindowsTo,
}

fn monitor_label(monitor: &Monitor) -> String {
    format!(
        "{} ({}×{}, {} Hz)",
        monitor.name, monitor.width, monitor.height, monitor.refresh_hz
    )
}

impl eframe::App for SettingsWindow {
    // Runs while minimised too, so the mirror follows the card display without the window open.
    fn logic(&mut self, _context: &egui::Context, _frame: &mut eframe::Frame) {
        if self.displays_changed.swap(false, Ordering::Relaxed) {
            self.follow_displays();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(report) = self.guard.take_report() {
            self.status = Status::Error(report);
        }
        let before = self.settings.clone();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::Grid::new("settings").num_columns(2).show(ui, |ui| {
                self.monitor_dropdown(ui, "Source monitor", Choice::SourceMonitor);
                self.monitor_dropdown(ui, "Card display", Choice::CardDisplay);
                self.monitor_dropdown(ui, "Send windows to", Choice::SendWindowsTo);
                self.audio_dropdown(ui);
            });
            ui.horizontal(|ui| {
                if self.running() {
                    if ui.button("Stop").clicked() {
                        self.stop(false, Status::Stopped);
                    }
                } else if ui.button("Start").clicked() {
                    self.start();
                }
                ui.checkbox(&mut self.settings.guard_enabled, "Guard");
            });
            ui.label(self.status.text());
        });
        if self.settings != before {
            update_guard(&mut *self.guard, &self.settings);
            if let Err(error) = self.settings.save() {
                self.status = Status::Error(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::StandInMirror;

    struct NoGuard;

    impl Guard for NoGuard {
        fn update(&mut self, _enabled: bool, _card: Option<&str>, _send_windows_to: Option<&str>) {}
        fn take_report(&mut self) -> Option<String> {
            None
        }
    }

    fn monitor(device_path: &str, primary: bool) -> Monitor {
        Monitor {
            device_path: device_path.into(),
            gdi_name: String::new(),
            name: device_path.into(),
            width: 1920,
            height: 1080,
            refresh_hz: 60,
            primary,
            left: 0,
            top: 0,
        }
    }

    fn window(card_display: &str) -> SettingsWindow {
        SettingsWindow {
            settings: Settings {
                source_monitor: Some("source".into()),
                card_display: Some(card_display.into()),
                ..Settings::default()
            },
            monitors: vec![monitor("source", true), monitor("card", false)],
            playback_devices: Vec::new(),
            status: Status::Stopped,
            mirror: Box::new(StandInMirror::default()),
            guard: Box::new(NoGuard),
            wanted: false,
            displays_changed: Arc::default(),
            _display_watcher: guard::DisplayWatcher::new(|| {}),
        }
    }

    /// Launches `window` and returns whether it asked to start minimised.
    fn launched_minimised(window: &mut SettingsWindow) -> bool {
        let context = egui::Context::default();
        // As in `run`: launch happens before the first pass.
        window.launch(&context);
        let mut output = context.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::Minimized(true))
    }

    #[test]
    fn saved_settings_start_the_mirror_minimised_at_launch() {
        let mut window = window("card");
        assert!(launched_minimised(&mut window));
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
    }

    #[test]
    fn launch_without_the_card_display_waits_with_the_window_shown() {
        let mut window = window("gone");
        assert!(!launched_minimised(&mut window));
        assert!(!window.mirror.is_running());
        assert!(window.running());
        assert_eq!(window.status.text(), "Waiting for card display");
    }
}
