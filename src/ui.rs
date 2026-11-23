//! ui module. No unsafe and no Windows calls here.

mod settings;

use crate::audio::{self, AudioDevice};
use crate::guard::{self, Guard, Monitor};
use crate::mirror::{Mirror, StandInMirror};
use eframe::egui;
use settings::{Settings, Status};

/// Opens the settings window; returns when it is closed, which quits cardcast.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([460.0, 220.0]),
        ..Default::default()
    };
    eframe::run_native(
        "cardcast",
        options,
        Box::new(|creation| Ok(Box::new(SettingsWindow::new(&creation.egui_ctx)))),
    )
}

struct SettingsWindow {
    settings: Settings,
    monitors: Vec<Monitor>,
    playback_devices: Vec<AudioDevice>,
    status: Status,
    mirror: Box<dyn Mirror>,
    guard: Box<dyn Guard>,
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
        let context = context.clone();
        let mut guard = guard::new_guard(move || context.request_repaint());
        update_guard(&mut *guard, &settings);
        Self {
            settings,
            monitors,
            playback_devices,
            status,
            mirror: Box::new(StandInMirror::default()),
            guard,
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
        matches!(self.status, Status::Running | Status::WaitingForCardDisplay)
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
                        self.mirror.stop();
                        self.status = Status::Stopped;
                    }
                } else if ui.button("Start").clicked() {
                    self.status =
                        settings::start(&self.settings, &self.monitors, &mut *self.mirror);
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
