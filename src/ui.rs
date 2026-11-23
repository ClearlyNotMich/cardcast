//! ui module. No unsafe and no Windows calls here.

mod settings;

use crate::audio::{self, AudioDevice, Relay, RelayReport};
use crate::guard::{self, Guard, Monitor};
use crate::mirror::{self, Ended, Mirror};
use eframe::egui;
use settings::{Follow, Settings, Status, StatusLine};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a restarting mirror is started again, as capture retries after access loss.
const RESTART_EVERY: Duration = Duration::from_secs(1);

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
    status: StatusLine,
    /// When a restarting mirror is next started.
    restart_at: Instant,
    mirror: Box<dyn Mirror>,
    relay: Box<dyn Relay>,
    guard: Box<dyn Guard>,
    /// True from Start (pressed or at launch) until Stop is pressed, including while waiting for the card display.
    wanted: bool,
    displays_changed: Arc<AtomicBool>,
    _display_watcher: guard::DisplayWatcher,
}

impl SettingsWindow {
    fn new(context: &egui::Context) -> Self {
        let mut status = StatusLine::new(Status::Stopped);
        let monitors = guard::monitors().unwrap_or_else(|error| {
            status.other = Some(error);
            Vec::new()
        });
        let playback_devices = audio::playback_devices().unwrap_or_else(|error| {
            status.other = Some(error);
            Vec::new()
        });
        let settings = Settings::load().with_defaults(&monitors);
        let relay_context = context.clone();
        let mirror_context = context.clone();
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
            restart_at: Instant::now(),
            mirror: mirror::new_mirror(move || mirror_context.request_repaint()),
            relay: audio::new_relay(move || relay_context.request_repaint()),
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
            if self.status.mirror == Status::Running {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
    }

    /// The one place the mirror starts: Start, launch and the card display coming back all call it.
    /// Audio starts with it.
    fn start(&mut self) {
        self.wanted = true;
        self.relay.stop();
        self.status = StatusLine::new(settings::start(
            &self.settings,
            &self.monitors,
            &mut *self.mirror,
        ));
        let card_display = self
            .monitors
            .iter()
            .find(|monitor| self.settings.card_display.as_ref() == Some(&monitor.device_path));
        if self.status.mirror == Status::Running
            && let (Some(audio_source), Some(card_display)) =
                (&self.settings.audio_source, card_display)
        {
            self.relay.start(audio_source, &card_display.name);
        }
    }

    /// The one place the mirror stops: Stop and the card display going away both call it. Audio stops with it.
    fn stop(&mut self, wanted: bool, status: Status) {
        self.mirror.stop();
        self.relay.stop();
        self.wanted = wanted;
        self.status = StatusLine::new(status);
    }

    /// Follows the mirror ending on its own, and starts a restarting mirror again on a timer.
    fn follow_mirror(&mut self, context: &egui::Context) {
        match self.mirror.take_end() {
            Some(Ended::Restart(reason)) => {
                self.status.mirror = Status::Restarting(reason);
                self.restart_at = Instant::now();
            }
            Some(Ended::Stop(reason)) => self.stop(true, Status::Error(reason)),
            None => {}
        }
        if matches!(self.status.mirror, Status::Restarting(_)) {
            let wait = self.restart_at.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                self.start();
                self.status.mirror = settings::restarted(self.status.mirror.clone());
                self.restart_at = Instant::now() + RESTART_EVERY;
            }
            if matches!(self.status.mirror, Status::Restarting(_)) {
                context.request_repaint_after(
                    self.restart_at.saturating_duration_since(Instant::now()),
                );
            }
        }
    }

    /// Takes the guard's and relay's latest reports into the status line.
    fn follow_reports(&mut self) {
        if let Some(report) = self.guard.take_report() {
            self.status.other = Some(report);
        }
        match self.relay.take_report() {
            Some(RelayReport::Failing(problem)) => self.status.audio = Some(problem),
            Some(RelayReport::Relaying) => self.status.audio = None,
            None => {}
        }
    }

    /// Re-reads the monitors after a display change and follows the card display.
    fn follow_displays(&mut self) {
        let monitors = match guard::monitors() {
            Ok(monitors) => monitors,
            Err(error) => {
                self.status.other = Some(error);
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
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if self.displays_changed.swap(false, Ordering::Relaxed) {
            self.follow_displays();
        }
        self.follow_mirror(context);
        self.follow_reports();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let before = self.settings.clone();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::Grid::new("settings").num_columns(2).show(ui, |ui| {
                self.monitor_dropdown(ui, "Source monitor", Choice::SourceMonitor);
                self.monitor_dropdown(ui, "Card display", Choice::CardDisplay);
                self.monitor_dropdown(ui, "Send windows to", Choice::SendWindowsTo);
                self.audio_dropdown(ui);
            });
            ui.horizontal(|ui| {
                if self.status.shows_stop() {
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
                self.status.other = Some(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::StandInMirror;

    /// Records what the settings window asked of the audio relay, and hands out `reports`.
    #[derive(Default, Clone)]
    struct RecordingRelay(
        std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        std::rc::Rc<std::cell::RefCell<Vec<RelayReport>>>,
    );

    impl audio::Relay for RecordingRelay {
        fn start(&mut self, audio_source: &str, card_display_name: &str) {
            self.0
                .borrow_mut()
                .push(format!("start {audio_source} to {card_display_name}"));
        }
        fn stop(&mut self) {
            self.0.borrow_mut().push("stop".into());
        }
        fn take_report(&mut self) -> Option<RelayReport> {
            self.1.borrow_mut().pop()
        }
    }

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
            status: StatusLine::new(Status::Stopped),
            restart_at: Instant::now(),
            mirror: Box::new(StandInMirror::default()),
            relay: Box::new(RecordingRelay::default()),
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
        assert!(window.status.shows_stop());
        assert_eq!(window.status.text(), "Waiting for card display");
    }

    fn with_relay(
        mut window: SettingsWindow,
        audio_source: Option<&str>,
    ) -> (SettingsWindow, RecordingRelay) {
        let relay = RecordingRelay::default();
        window.relay = Box::new(relay.clone());
        window.settings.audio_source = audio_source.map(str::to_owned);
        (window, relay)
    }

    #[test]
    fn audio_follows_start_and_stop() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        window.stop(false, Status::Stopped);
        assert_eq!(
            *relay.0.borrow(),
            ["stop", "start speakers to card", "stop"]
        );
    }

    #[test]
    fn audio_waits_with_the_mirror_for_the_card_display() {
        let (mut window, relay) = with_relay(window("gone"), Some("speakers"));
        window.start();
        assert_eq!(*relay.0.borrow(), ["stop"]);
    }

    #[test]
    fn no_audio_without_an_audio_source() {
        let (mut window, relay) = with_relay(window("card"), None);
        window.start();
        assert!(window.mirror.is_running());
        assert_eq!(*relay.0.borrow(), ["stop"]);
    }

    #[test]
    fn audio_problem_keeps_the_stop_button_and_clears_when_audio_recovers() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        relay.1.borrow_mut().push(RelayReport::Failing(
            "The audio source is not connected.".into(),
        ));
        window.follow_reports();
        assert!(window.status.shows_stop());
        assert_eq!(
            window.status.text(),
            "Running | The audio source is not connected."
        );
        relay.1.borrow_mut().push(RelayReport::Relaying);
        window.follow_reports();
        assert_eq!(window.status.text(), "Running");
    }

    fn mirror_ends(window: &mut SettingsWindow, end: Ended) {
        window.mirror = Box::new(StandInMirror {
            running: true,
            end: Some(end),
        });
    }

    #[test]
    fn mirror_that_lost_the_graphics_card_is_restarted() {
        let mut window = window("card");
        window.start();
        mirror_ends(&mut window, Ended::Restart("lost".into()));
        assert!(!window.mirror.is_running());
        window.follow_mirror(&egui::Context::default());
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
        assert!(window.status.shows_stop());
    }

    #[test]
    fn mirror_that_failed_for_good_stops_with_its_reason_and_audio() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        mirror_ends(&mut window, Ended::Stop("The mirror stopped.".into()));
        window.follow_mirror(&egui::Context::default());
        assert!(!window.mirror.is_running());
        assert_eq!(window.status.text(), "The mirror stopped.");
        assert!(!window.status.shows_stop());
        assert_eq!(relay.0.borrow().last().map(String::as_str), Some("stop"));
    }

    #[test]
    fn restart_waits_with_the_stop_button_for_an_unplugged_card_display() {
        let mut window = window("card");
        window.start();
        mirror_ends(&mut window, Ended::Restart("lost".into()));
        window
            .monitors
            .retain(|monitor| monitor.device_path != "card");
        window.follow_mirror(&egui::Context::default());
        assert_eq!(window.status.text(), "Waiting for card display");
        assert!(window.status.shows_stop());
    }
}
