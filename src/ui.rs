//! ui module. No unsafe and no Windows calls here.

mod live;
mod look;
mod settings;
#[cfg(all(test, target_os = "linux"))]
mod snapshots;

use crate::audio::{self, AudioDevice, Relay, RelayReport, SoundOutput};
use crate::guard::{self, Guard, Monitor};
use crate::input::{self, InputThread, Probe};
use crate::mirror::{self, Ended, Mirror};
use eframe::egui::{self, RichText};
use look::{FrameRate, Role};
use settings::{Follow, Settings, Status, StatusLine};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Opens the settings window; returns when it is closed, which quits cardcast.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([780.0, 780.0])
            .with_min_inner_size([560.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "cardcast",
        options,
        Box::new(|creation| {
            look::apply(&creation.egui_ctx);
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
    /// The sound output dialog is open with this choice, not yet saved.
    choosing_sound_output: Option<SoundOutput>,
    status: StatusLine,
    /// When a restarting mirror is next started.
    restart_at: Instant,
    /// Starts since the mirror last showed the picture; sets the restart backoff.
    attempts: u32,
    /// Launch minimises the window once the mirror it started shows the picture.
    minimise_when_running: bool,
    mirror: Box<dyn Mirror>,
    relay: Box<dyn Relay>,
    guard: Box<dyn Guard>,
    displays_changed: Arc<AtomicBool>,
    /// `None` if it could not start; the status line says so.
    _display_watcher: Option<guard::DisplayWatcher>,
    frame_rate: FrameRate,
    /// Height the live layout takes beyond `look::LAYOUT_HEIGHT`, so the window has no empty band.
    layout_extra: f32,
    /// Started when input switching is first switched on and kept, so two never talk to one
    /// monitor; while off it gets no jobs, so nothing is sent over DDC/CI.
    input_thread: Option<InputThread>,
    start_input_thread: Box<dyn Fn() -> Result<InputThread, String>>,
    /// The input lists last read, in the monitor list's order.
    input_lists: Vec<Probe>,
    /// The current input of the monitor whose input menu opened last: its device path and the value.
    current_input: Option<(String, u8)>,
}

impl SettingsWindow {
    fn new(context: &egui::Context) -> Self {
        let mut status = StatusLine::new(Status::Stopped);
        status.displays = not_built_yet();
        let monitors = guard::monitors().unwrap_or_else(|error| {
            status.other = Some(error);
            Vec::new()
        });
        let playback_devices = audio::playback_devices().unwrap_or_else(|error| {
            status.other = Some(error);
            Vec::new()
        });
        let mut settings = Settings::load();
        let cleared = settings.clear_role_clashes();
        let mut settings = settings.with_defaults(&monitors);
        let followed = settings::follow_card_display(&mut settings, &monitors);
        if (cleared.is_some() || followed)
            && let Err(error) = settings.save()
        {
            status.other = Some(error);
        }
        status.other = settings::notices(status.other, cleared);
        let relay_context = context.clone();
        let mirror_context = context.clone();
        let guard_context = context.clone();
        let input_context = context.clone();
        let mut guard = guard::new_guard(move || guard_context.request_repaint());
        update_guard(&mut *guard, &settings);
        let displays_changed = Arc::new(AtomicBool::new(false));
        let display_watcher = {
            let (context, displays_changed) = (context.clone(), displays_changed.clone());
            guard::DisplayWatcher::new(move || {
                displays_changed.store(true, Ordering::Relaxed);
                context.request_repaint();
            })
            .map_err(|error| {
                status.displays = settings::notices(status.displays.take(), Some(error))
            })
            .ok()
        };
        Self {
            settings,
            monitors,
            playback_devices,
            choosing_sound_output: None,
            status,
            restart_at: Instant::now(),
            attempts: 0,
            minimise_when_running: false,
            mirror: mirror::new_mirror(move || mirror_context.request_repaint()),
            relay: audio::new_relay(move || relay_context.request_repaint()),
            guard,
            displays_changed,
            _display_watcher: display_watcher,
            frame_rate: FrameRate::default(),
            layout_extra: 0.0,
            input_thread: None,
            start_input_thread: Box::new(move || {
                let context = input_context.clone();
                input::new_input_thread(move || context.request_repaint())
            }),
            input_lists: Vec::new(),
            current_input: None,
        }
    }

    /// Starts the mirror from saved settings; the window minimises once it runs.
    fn launch(&mut self, context: &egui::Context) {
        self.follow_input_switching(None, false);
        if settings::starts_on_launch(&self.settings) {
            // A problem found while loading outlives the start, which clears the status line's.
            let loaded = self.status.other.take();
            self.minimise_when_running = true;
            self.start();
            self.follow_mirror(context);
            self.status.other = settings::notices(self.status.other.take(), loaded);
        }
    }

    /// The one place the mirror starts: Start, launch, a restart, a changed choice and a monitor
    /// coming back all call it. It starts in the background, so the settings window never waits
    /// on the graphics card. A running mirror and audio stop first.
    fn start(&mut self) {
        self.mirror.stop();
        self.relay.stop();
        self.attempts = self.attempts.saturating_add(1);
        self.restart_at = Instant::now() + settings::restart_delay(self.attempts);
        self.status.set_mirror(settings::start(
            &self.settings,
            &self.monitors,
            &mut *self.mirror,
        ));
        self.follow_started();
    }

    /// Start and a dropdown change: the user changed something, so the restart backoff starts over.
    fn start_by_user(&mut self) {
        self.attempts = 0;
        self.start();
    }

    /// A starting or restarting mirror that shows the picture runs: audio starts with it, and
    /// the restart backoff starts over.
    fn follow_started(&mut self) {
        if !matches!(self.status.mirror, Status::Starting | Status::Restarting(_))
            || !self.mirror.shows_picture()
        {
            return;
        }
        self.status.mirror = Status::Running;
        self.attempts = 0;
        self.start_audio();
    }

    fn card_display(&self) -> Option<&Monitor> {
        self.monitors
            .iter()
            .find(|monitor| self.settings.card_display.as_ref() == Some(&monitor.device_path))
    }

    /// Starts the relay for the running mirror, unless the sound output is No audio.
    fn start_audio(&mut self) {
        let Some(card_display) = self.card_display() else {
            return;
        };
        let card_display = (card_display.container_id.clone(), card_display.name.clone());
        if let (Some(audio_source), false) = (
            &self.settings.audio_source,
            self.settings.sound_output == SoundOutput::NoAudio,
        ) {
            self.relay.start(
                audio_source,
                &self.settings.sound_output,
                &card_display.0,
                &card_display.1,
            );
        }
    }

    /// A new sound output restarts only the audio; the mirror never waits on it.
    fn restart_audio(&mut self) {
        self.relay.stop();
        self.status.audio = None;
        self.status.choose_sound_output = false;
        if self.status.mirror == Status::Running {
            self.start_audio();
        }
    }

    /// The one place the mirror stops: Stop, an error and the card display going away all call it.
    /// Audio stops with it. `status` decides whether it starts again by itself.
    fn stop(&mut self, status: Status) {
        self.mirror.stop();
        self.relay.stop();
        self.attempts = 0;
        self.status.set_mirror(status);
    }

    /// Follows the mirror starting, failing to start or ending on its own, and starts a
    /// restarting mirror again when its backoff timer is due.
    fn follow_mirror(&mut self, context: &egui::Context) {
        match self.mirror.take_end() {
            Some(Ended::Restart(reason)) => {
                self.status.mirror = Status::Restarting(reason);
                self.restart_at = Instant::now() + settings::restart_delay(self.attempts);
            }
            Some(Ended::Stop(reason)) => self.stop(Status::Error(reason)),
            None => {}
        }
        self.follow_started();
        if self.minimise_when_running && self.status.mirror != Status::Starting {
            self.minimise_when_running = false;
            if self.status.mirror == Status::Running {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
        // A restart that is starting in the background waits for its result, not the timer.
        if matches!(self.status.mirror, Status::Restarting(_)) && !self.mirror.is_running() {
            if self.restart_at <= Instant::now() {
                let restarting = self.status.mirror.clone();
                self.start();
                // It keeps saying why it restarts until it shows the picture.
                self.status.mirror = match settings::restarted(self.status.mirror.clone()) {
                    Status::Starting => restarting,
                    status => status,
                };
            }
            if matches!(self.status.mirror, Status::Restarting(_)) && !self.mirror.is_running() {
                context.request_repaint_after(
                    self.restart_at.saturating_duration_since(Instant::now()),
                );
            }
        }
    }

    /// Takes the guard's and relay's latest reports into the status line.
    fn follow_reports(&mut self) {
        if let Some(report) = self.guard.take_report() {
            self.status.follow_guard(report);
        }
        match self.relay.take_report() {
            Some(RelayReport::Failing(problem)) => {
                self.status.audio = Some(problem);
                self.status.choose_sound_output = false;
            }
            Some(RelayReport::NoSoundOutput) => {
                self.status.audio = Some(audio::NO_SOUND_OUTPUT.into());
                self.status.choose_sound_output = true;
            }
            Some(RelayReport::Relaying) => {
                self.status.audio = None;
                self.status.choose_sound_output = false;
            }
            None => {}
        }
        while let Some(report) = self
            .input_thread
            .as_ref()
            .filter(|_| self.settings.input_switching)
            .and_then(InputThread::take_report)
        {
            match report {
                input::Report::Probed(lists) => {
                    self.status.input = input::probed_notice(&lists).map(str::to_owned);
                    self.input_lists = lists;
                }
                input::Report::Current { device_path, input } => {
                    self.current_input = input.map(|input| (device_path, input));
                }
                input::Report::Switched(Ok(current)) => {
                    self.status.input = None;
                    self.current_input = Some(current);
                }
                input::Report::Switched(Err(problem)) => self.status.input = Some(problem),
            }
        }
    }

    /// Starts and stops the input thread with the toggle, and has it read the input lists when
    /// [`input::reads_input_lists`] says so. `was_on` is `None` at launch.
    fn follow_input_switching(&mut self, was_on: Option<bool>, displays_changed: bool) {
        let on = self.settings.input_switching;
        if !on {
            if let Some(thread) = &mut self.input_thread {
                thread.cancel();
            }
            self.input_lists.clear();
            self.current_input = None;
            self.status.input = None;
            return;
        }
        if !input::reads_input_lists(was_on, on, displays_changed) {
            return;
        }
        if self.input_thread.is_none() {
            match (self.start_input_thread)() {
                Ok(thread) => self.input_thread = Some(thread),
                Err(problem) => {
                    self.status.input = Some(problem);
                    return;
                }
            }
        }
        self.send_input_job(input::Job::Probe(self.monitors.clone()));
    }

    /// Hands a job to the input thread; none while input switching is off.
    fn send_input_job(&mut self, job: input::Job) {
        if let Some(thread) = self
            .input_thread
            .as_ref()
            .filter(|_| self.settings.input_switching)
        {
            if matches!(job, input::Job::Read(_)) {
                self.current_input = None;
            }
            thread.send(job);
        }
    }

    /// What every pass does, even while minimised: follow display changes, the mirror and reports.
    fn follow(&mut self, context: &egui::Context) {
        // The display watcher is the one listener for display and device changes; the guard follows it too.
        if self.displays_changed.swap(false, Ordering::Relaxed) {
            self.guard.follow_displays();
            self.follow_displays();
        }
        self.follow_mirror(context);
        self.follow_reports();
    }

    /// Re-reads the monitors after a display change and follows the source monitor and card display.
    fn follow_displays(&mut self) {
        // A card display's sound output comes and goes with it.
        self.take_playback_devices(audio::playback_devices());
        match guard::monitors() {
            Ok(monitors) => self.follow_monitors(monitors),
            Err(error) => self.status.other = Some(error),
        }
    }

    fn follow_monitors(&mut self, monitors: Vec<Monitor>) {
        let chosen = |settings: &Settings, monitors: &'_ [Monitor]| {
            [&settings.source_monitor, &settings.card_display].map(|chosen| {
                monitors
                    .iter()
                    .find(|monitor| chosen.as_ref() == Some(&monitor.device_path))
                    .cloned()
            })
        };
        let before = chosen(&self.settings, &self.monitors);
        if settings::follow_card_display(&mut self.settings, &monitors) {
            update_guard(&mut *self.guard, &self.settings);
            if let Err(error) = self.settings.save() {
                self.status.other = Some(error);
            }
        }
        let now = chosen(&self.settings, &monitors);
        let follow = settings::follow_displays(
            self.status.shows_stop(),
            self.mirror.is_running() || matches!(self.status.mirror, Status::Restarting(_)),
            before.each_ref().map(Option::as_ref),
            now.each_ref().map(Option::as_ref),
        );
        self.monitors = monitors;
        match follow {
            Follow::Start => self.start(),
            Follow::Stop(status) => self.stop(status),
            Follow::Nothing => {}
        }
        self.follow_input_switching(Some(self.settings.input_switching), true);
    }

    /// Applies changed choices at once: the guard takes them, and a mirror that runs or will
    /// start by itself restarts on the new monitors and audio source.
    fn follow_settings(&mut self, before: &Settings) {
        update_guard(&mut *self.guard, &self.settings);
        if settings::restarts_on_change(self.status.shows_stop(), before, &self.settings) {
            self.start_by_user();
        } else if before.sound_output != self.settings.sound_output {
            self.restart_audio();
        }
        if before.input_switching != self.settings.input_switching {
            self.follow_input_switching(Some(before.input_switching), false);
        }
    }

    /// Offers only the monitors the role can take ([`Role::offers`]).
    fn monitor_dropdown(&mut self, ui: &mut egui::Ui, role: Role) {
        let offered: Vec<&Monitor> = self
            .monitors
            .iter()
            .filter(|monitor| role.offers(&self.settings, &self.monitors, monitor))
            .collect();
        let selected = role.choice(&mut self.settings);
        let shown = self
            .monitors
            .iter()
            .find(|monitor| selected.as_deref() == Some(&monitor.device_path))
            .map_or_else(|| "(choose)".to_owned(), monitor_label);
        let label = live::labelled_dot(ui, role.label(), role.colour());
        egui::ComboBox::from_id_salt(role.label())
            .selected_text(shown)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for monitor in offered {
                    ui.selectable_value(
                        selected,
                        Some(monitor.device_path.clone()),
                        monitor_label(monitor),
                    );
                }
            })
            .response
            .labelled_by(label.id);
    }

    fn audio_dropdown(&mut self, ui: &mut egui::Ui) {
        let card_display = self
            .monitors
            .iter()
            .find(|monitor| self.settings.card_display.as_ref() == Some(&monitor.device_path));
        let choices =
            settings::audio_source_choices(&self.playback_devices, &self.settings, card_display);
        let selected = &mut self.settings.audio_source;
        let shown = self
            .playback_devices
            .iter()
            .find(|device| selected.as_deref() == Some(&device.id))
            .map_or("(choose)", |device| &device.name);
        let label = live::labelled_dot(ui, "Audio source", look::LIVE);
        egui::ComboBox::from_id_salt("Audio source")
            .selected_text(shown)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for device in &choices {
                    ui.selectable_value(selected, Some(device.id.clone()), &device.name);
                }
            })
            .response
            .labelled_by(label.id);
        ui.horizontal_wrapped(|ui| {
            let shown = settings::sound_output_label(
                &self.settings,
                &self.playback_devices,
                self.card_display(),
            );
            ui.label(
                RichText::new(format!("Sound output: {shown} ·"))
                    .small()
                    .color(look::TEXT_DIM),
            );
            if ui
                .link(RichText::new("Change").small())
                .on_hover_text("Choose where the capture card's sound plays")
                .clicked()
            {
                self.choose_sound_output();
            }
        });
    }

    /// A fresh list of playback devices; a failed one keeps the old list and says why.
    fn take_playback_devices(&mut self, devices: Result<Vec<AudioDevice>, String>) {
        match devices {
            Ok(devices) => self.playback_devices = devices,
            Err(error) => self.status.other = Some(error),
        }
    }

    /// Opens the sound output dialog with the saved choice, on a fresh list of playback devices.
    fn choose_sound_output(&mut self) {
        self.take_playback_devices(audio::playback_devices());
        self.choosing_sound_output = Some(self.settings.sound_output.clone());
    }

    /// The sound output dialog: one dropdown, then Save or Cancel.
    fn sound_output_dialog(&mut self, context: &egui::Context) {
        let Some(choice) = &mut self.choosing_sound_output else {
            return;
        };
        let choices = settings::sound_output_choices(
            &self.playback_devices,
            self.settings.audio_source.as_deref(),
        );
        let mut close = false;
        let mut save = false;
        let modal = egui::Modal::new(egui::Id::unique("Sound output")).show(context, |ui| {
            ui.set_width(380.0);
            let title = ui.label(RichText::new("Sound output").strong().color(look::TEXT));
            ui.label(
                RichText::new(
                    "Where the capture card's sound plays. Automatic finds the card display's own \
                     sound output; choose No audio if the capture card has none.",
                )
                .small()
                .color(look::TEXT_DIM),
            );
            ui.add_space(8.0);
            let shown = choices
                .iter()
                .find(|(output, _)| output == choice)
                .map_or("Automatic", |(_, name)| name.as_str());
            egui::ComboBox::from_id_salt("Sound output choice")
                .selected_text(shown)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (output, name) in &choices {
                        ui.selectable_value(choice, output.clone(), name);
                    }
                })
                .response
                .labelled_by(title.id);

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                save = ui.button("Save").clicked();
                close = ui.button("Cancel").clicked();
            });
        });
        if save {
            self.settings.sound_output = choice.clone();
        }
        if save || close || modal.should_close() {
            self.choosing_sound_output = None;
        }
    }

    /// Start or Stop, the guard switch, and the status line with the guard notice's dismiss button.
    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (text, fill) = if self.status.shows_stop() {
                ("Stop", look::PROBLEM.gamma_multiply(0.75))
            } else {
                ("Start", look::ACCENT)
            };
            let button =
                egui::Button::new(RichText::new(text).size(16.0).strong().color(look::TEXT))
                    .fill(fill)
                    .corner_radius(10)
                    .min_size(egui::vec2(132.0, 40.0));
            if ui.add(button).clicked() {
                if self.status.shows_stop() {
                    self.stop(Status::Stopped);
                } else {
                    self.start_by_user();
                }
            }
            ui.add_space(12.0);
            live::switch(
                ui,
                &mut self.settings.guard_enabled,
                "Guard",
                "Keeps windows and the pointer off the card display",
            );
        });
        // The header shows the mirror's state; this box only says what it cannot.
        let Some(details) = self.status.details() else {
            return;
        };
        ui.add_space(6.0);
        // A slow mirror still runs, but the box warns.
        let tone = match look::Tone::of(&self.status.mirror) {
            look::Tone::Live if self.status.slow_mirror.is_some() => look::Tone::Busy,
            tone => tone,
        };
        egui::Frame::new()
            .fill(look::BACKGROUND)
            .stroke(egui::Stroke::new(1.0, tone.colour().gamma_multiply(0.35)))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    live::dot(ui, tone.colour());
                    ui.label(RichText::new(details).color(look::TEXT));
                    if self.status.choose_sound_output
                        && ui.small_button("Choose sound output").clicked()
                    {
                        self.choose_sound_output();
                    }
                    if self.status.guard.is_some()
                        && ui
                            .small_button("Dismiss")
                            .on_hover_text("Dismiss the guard notice")
                            .clicked()
                    {
                        self.status.dismiss_guard_notice();
                    }
                });
            });
    }
}

/// The parts not built for this platform yet (#36): a lasting notice that Start and Stop don't clear.
fn not_built_yet() -> Option<String> {
    let parts: Vec<&str> = [guard::NOT_BUILT, mirror::NOT_BUILT, audio::NOT_BUILT]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" | "))
}

fn update_guard(guard: &mut dyn Guard, settings: &Settings) {
    guard.update(
        settings.guard_enabled,
        settings.card_display.as_deref(),
        settings.send_windows_to.as_deref(),
    );
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
        self.follow(context);
    }

    // eframe calls it only while the window is shown; minimised, only `logic` runs.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let before = self.settings.clone();
        if let (Status::Running, Some(presents)) = (&self.status.mirror, self.mirror.presents()) {
            let refresh_hz = self.card_display().map_or(0, |monitor| monitor.refresh_hz);
            let now = Instant::now();
            self.frame_rate.sample(presents, refresh_hz, now);
            self.status.slow_mirror = self.frame_rate.slow_warning(now);
        } else if self.status.mirror != Status::Running {
            self.frame_rate = FrameRate::default();
            self.status.slow_mirror = None;
        }
        let (visible, focused, time) = ui.input(|input| {
            let viewport = input.viewport();
            let hidden = viewport.minimized == Some(true) || viewport.occluded == Some(true);
            (!hidden, input.focused, input.time)
        });
        // Without focus (the game has it) the animation holds still instead of stepping once a second.
        let time = focused.then_some(time);
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(look::PANEL)
                    .inner_margin(egui::Margin::symmetric(18, 0)),
            )
            .exact_size(60.0)
            .show_separator_line(false)
            .show(ui, |ui| {
                live::header(
                    ui,
                    &self.status.mirror,
                    self.frame_rate.shown,
                    time,
                    &mut self.settings.input_switching,
                    self.status.input.as_deref(),
                );
            });
        egui::Panel::bottom("controls")
            .frame(egui::Frame::new().fill(look::PANEL).inner_margin(16))
            .show(ui, |ui| self.controls(ui));
        let mut asked = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(look::BACKGROUND).inner_margin(16))
            .show(ui, |ui| {
                let hint = match settings::role_problem(&self.settings, &self.monitors) {
                    Some(problem) => RichText::new(problem).small().color(look::PROBLEM),
                    None => RichText::new("Click a monitor to choose its role")
                        .small()
                        .color(look::TEXT_DIM),
                };
                // A small window scrolls instead of hiding the choices under the controls.
                let shown = egui::ScrollArea::vertical().show(ui, |ui| {
                    live::section(ui, "LIVE LAYOUT", hint, |ui| {
                        asked = live::layout_map(
                            ui,
                            &self.monitors,
                            &mut self.settings,
                            &self.status.mirror,
                            time,
                            look::LAYOUT_HEIGHT + self.layout_extra,
                            &live::Inputs {
                                lists: &self.input_lists,
                                current: self.current_input.as_ref(),
                            },
                        );
                    });
                    ui.add_space(8.0);
                    live::section(ui, "CHOICES", RichText::new(""), |ui| {
                        ui.columns(2, |columns| {
                            self.monitor_dropdown(&mut columns[0], Role::SourceMonitor);
                            self.monitor_dropdown(&mut columns[1], Role::CardDisplay);
                        });
                        ui.add_space(6.0);
                        ui.columns(2, |columns| {
                            self.monitor_dropdown(&mut columns[0], Role::SendWindowsTo);
                            self.audio_dropdown(&mut columns[1]);
                        });
                    });
                });
                // The live layout takes the spare height; a pass drawn with the old height is redone.
                let extra = look::layout_extra(
                    self.layout_extra,
                    shown.inner_rect.height(),
                    shown.content_size.y,
                );
                if extra != self.layout_extra {
                    self.layout_extra = extra;
                    ui.ctx()
                        .request_discard("the live layout takes the spare height");
                }
            });
        if let Some(job) = asked {
            self.send_input_job(job);
        }
        self.sound_output_dialog(ui.ctx());
        // A newly chosen card display's connector is saved with it.
        settings::follow_card_display(&mut self.settings, &self.monitors);
        if self.settings != before {
            self.follow_settings(&before);
            if let Err(error) = self.settings.save() {
                self.status.other = Some(error);
            }
        }
        // Start and a changed choice can leave a restart waiting; `logic` runs it on time from here.
        if matches!(self.status.mirror, Status::Restarting(_)) && !self.mirror.is_running() {
            ui.ctx()
                .request_repaint_after(self.restart_at.saturating_duration_since(Instant::now()));
        }
        if let Some(after) = look::next_repaint(&self.status.mirror, visible, focused) {
            ui.ctx().request_repaint_after(after);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::GuardReport;
    use crate::mirror::StandInMirror;
    use std::time::Duration;

    /// Records what the settings window asked of the audio relay, and hands out `reports`.
    #[derive(Default, Clone)]
    pub(super) struct RecordingRelay(
        pub(super) std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        pub(super) std::rc::Rc<std::cell::RefCell<Vec<RelayReport>>>,
    );

    impl audio::Relay for RecordingRelay {
        fn start(
            &mut self,
            audio_source: &str,
            output: &SoundOutput,
            container: &str,
            card_display_name: &str,
        ) {
            let via = match output {
                SoundOutput::Automatic => String::new(),
                output => format!(" via {}", output.to_text()),
            };
            self.0.borrow_mut().push(format!(
                "start {audio_source} to {card_display_name} in {container}{via}"
            ));
        }
        fn stop(&mut self) {
            self.0.borrow_mut().push("stop".into());
        }
        fn take_report(&mut self) -> Option<RelayReport> {
            self.1.borrow_mut().pop()
        }
    }

    /// Hands out `reports`, latest last, remembers the card display it was last given, and counts display changes.
    #[derive(Default, Clone)]
    struct ReportingGuard(
        std::rc::Rc<std::cell::RefCell<Vec<GuardReport>>>,
        std::rc::Rc<std::cell::RefCell<Option<String>>>,
        std::rc::Rc<std::cell::RefCell<u32>>,
    );

    impl Guard for ReportingGuard {
        fn update(&mut self, _enabled: bool, card: Option<&str>, _send_windows_to: Option<&str>) {
            *self.1.borrow_mut() = card.map(str::to_owned);
        }
        fn take_report(&mut self) -> Option<GuardReport> {
            self.0.borrow_mut().pop()
        }
        fn follow_displays(&mut self) {
            *self.2.borrow_mut() += 1;
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
            graphics_card: String::new(),
            container_id: format!("{device_path} container"),
            connector: format!("{device_path} connector"),
        }
    }

    pub(super) fn window(card_display: &str) -> SettingsWindow {
        SettingsWindow {
            settings: Settings {
                source_monitor: Some("source".into()),
                card_display: Some(card_display.into()),
                ..Settings::default()
            },
            monitors: vec![monitor("source", true), monitor("card", false)],
            playback_devices: Vec::new(),
            choosing_sound_output: None,
            status: StatusLine::new(Status::Stopped),
            restart_at: Instant::now(),
            attempts: 0,
            minimise_when_running: false,
            mirror: Box::new(StandInMirror::default()),
            relay: Box::new(RecordingRelay::default()),
            guard: Box::new(ReportingGuard::default()),
            displays_changed: Arc::default(),
            _display_watcher: None,
            frame_rate: FrameRate::default(),
            layout_extra: 0.0,
            input_thread: None,
            start_input_thread: Box::new(|| input::new_input_thread(|| {})),
            input_lists: Vec::new(),
            current_input: None,
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
    fn a_choice_cleared_at_load_is_still_said_after_the_start_at_launch() {
        let mut window = window("card");
        let cleared =
            "\"Send windows to\" was the card display, so it is the primary monitor again.";
        window.status.other = Some(cleared.into());
        launched_minimised(&mut window);
        assert_eq!(window.status.text(), format!("Running | {cleared}"));
    }

    #[test]
    fn a_load_notice_and_a_start_notice_are_both_said_at_launch() {
        let mut window = window("card");
        window.status.other = Some("loaded".into());
        // Start says the card display can't be the primary monitor.
        window.monitors[0].primary = false;
        window.monitors[1].primary = true;
        window.launch(&egui::Context::default());
        assert_eq!(
            window.status.text(),
            "The card display can't be the primary monitor. | loaded"
        );
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

    #[cfg(not(windows))]
    #[test]
    fn on_linux_start_keeps_saying_what_does_not_work_yet() {
        let mut window = window("card");
        window.monitors = guard::monitors().unwrap();
        window.settings = Settings::default();
        window.status.displays = not_built_yet();
        window.start();
        assert_eq!(
            window.status.text(),
            "Choose a source monitor and a card display. \
             | The monitor list does not work on Linux yet. \
             | The mirror does not work on Linux yet. \
             | The audio relay does not work on Linux yet."
        );
    }

    #[test]
    fn audio_follows_start_and_stop() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        window.stop(Status::Stopped);
        assert_eq!(
            *relay.0.borrow(),
            ["stop", "start speakers to card in card container", "stop"]
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

    /// The settings window as `run` builds it, with the real mirror and display watcher, mirroring
    /// `source_monitor` onto `card_display`, and the guard it was handed.
    #[cfg(windows)]
    fn on_second_monitors(
        monitors: Vec<Monitor>,
        source_monitor: &Monitor,
        card_display: &Monitor,
    ) -> (SettingsWindow, ReportingGuard) {
        let displays_changed = Arc::new(AtomicBool::new(false));
        let display_watcher = {
            let displays_changed = displays_changed.clone();
            Some(
                guard::DisplayWatcher::new(move || displays_changed.store(true, Ordering::Relaxed))
                    .unwrap(),
            )
        };
        let guard = ReportingGuard::default();
        let window = SettingsWindow {
            settings: Settings {
                source_monitor: Some(source_monitor.device_path.clone()),
                card_display: Some(card_display.device_path.clone()),
                ..Settings::default()
            },
            monitors,
            playback_devices: Vec::new(),
            choosing_sound_output: None,
            status: StatusLine::new(Status::Stopped),
            restart_at: Instant::now(),
            attempts: 0,
            minimise_when_running: false,
            mirror: mirror::new_mirror(|| {}),
            relay: Box::new(RecordingRelay::default()),
            guard: Box::new(guard.clone()),
            displays_changed,
            _display_watcher: display_watcher,
            frame_rate: FrameRate::default(),
            layout_extra: 0.0,
            input_thread: None,
            start_input_thread: Box::new(|| input::new_input_thread(|| {})),
            input_lists: Vec::new(),
            current_input: None,
        };
        (window, guard)
    }

    /// Starts the mirror and follows it until it runs; it starts in the background.
    #[cfg(windows)]
    fn start_and_wait(window: &mut SettingsWindow, context: &egui::Context) {
        window.start();
        guard::second_monitor::wait_for("the mirror to run", Duration::from_secs(10), || {
            window.follow(context);
            window.status.mirror == Status::Running
        });
    }

    #[cfg(windows)]
    #[test]
    fn second_monitor_unplug_waits_for_the_card_display_and_replug_brings_the_mirror_back() {
        use guard::second_monitor::{Unplugged, wait_for};
        // The two virtual monitors share a graphics card, as SPEC section 1 needs.
        let Some(monitors) = guard::second_monitor::monitors(3) else {
            return;
        };
        let (source_monitor, card_display) = (monitors[1].clone(), monitors[2].clone());
        let (mut window, _guard) = on_second_monitors(monitors, &source_monitor, &card_display);
        let context = egui::Context::default();
        start_and_wait(&mut window, &context);
        assert_eq!(window.status.text(), "Running");

        let unplugged = Unplugged::new();
        wait_for("Waiting for card display", Duration::from_secs(15), || {
            window.follow(&context);
            window.status.mirror == Status::WaitingForCardDisplay
        });
        assert!(!window.mirror.is_running(), "the mirror still runs");

        drop(unplugged);
        wait_for("the mirror to come back", Duration::from_secs(30), || {
            window.follow(&context);
            window.mirror.is_running() && window.status.mirror == Status::Running
        });
        let back = window
            .monitors
            .iter()
            .find(|monitor| monitor.device_path == card_display.device_path)
            .unwrap();
        println!(
            "card display {} came back as {}",
            card_display.gdi_name, back.gdi_name
        );
        std::thread::sleep(Duration::from_secs(1));
        window.follow(&context);
        assert!(
            window.mirror.is_running(),
            "the mirror ended after the replug"
        );
        assert_eq!(window.status.text(), "Running");
    }

    /// A card display the user set to 120 Hz is read as 120 Hz, and the mirror runs on it. Whether
    /// it presents 120 times a second can't be proved here: a virtual monitor has no vertical
    /// blank, and its presents follow the runner's 64 Hz Hyper-V monitor instead (about 50 a
    /// second at 30 Hz, 64 at 120 Hz; docs/research/ci-second-monitor.md). That stays an owner check.
    #[cfg(windows)]
    #[test]
    fn second_monitor_mirror_runs_on_a_card_display_set_to_120_hz() {
        use guard::second_monitor::{RefreshRate, wait_for};
        use std::io::Write as _;
        let Some(monitors) = guard::second_monitor::monitors(3) else {
            return;
        };
        let (source_monitor, card_display) = (monitors[1].clone(), monitors[2].clone());
        let _rate = RefreshRate::set(&card_display, 120);
        let mut monitors = Vec::new();
        wait_for(
            "the card display at 120 Hz",
            Duration::from_secs(10),
            || {
                monitors = guard::monitors().unwrap();
                monitors.iter().any(|monitor| {
                    monitor.device_path == card_display.device_path && monitor.refresh_hz == 120
                })
            },
        );
        let card_display = monitors
            .iter()
            .find(|monitor| monitor.device_path == card_display.device_path)
            .unwrap()
            .clone();
        let (mut window, _guard) = on_second_monitors(monitors, &source_monitor, &card_display);
        let context = egui::Context::default();
        start_and_wait(&mut window, &context);
        // Measured as the header does, after the mirror settled.
        std::thread::sleep(Duration::from_millis(500));
        for _ in 0..=8 {
            let now = Instant::now();
            window
                .frame_rate
                .sample(window.mirror.presents().unwrap(), 120, now);
            std::thread::sleep(Duration::from_millis(500));
        }
        let shown = window.frame_rate.shown.unwrap();
        // Written to stderr directly: libtest captures only the print macros.
        let _ = writeln!(
            std::io::stderr(),
            "{shown:.1} presents a second on a {} Hz card display; slow warning: {:?}",
            card_display.refresh_hz,
            window.frame_rate.slow_warning(Instant::now())
        );
        assert!(
            shown >= 30.0,
            "{shown:.1} presents a second on a 120 Hz card display"
        );
        assert!(window.mirror.is_running(), "the mirror ended on its own");
    }

    #[cfg(windows)]
    #[test]
    fn second_monitor_source_monitor_unplug_says_so_and_replug_brings_the_mirror_back() {
        use guard::second_monitor::{Unplugged, wait_for};
        let Some(monitors) = guard::second_monitor::monitors(3) else {
            return;
        };
        let (source_monitor, card_display) = (monitors[1].clone(), monitors[2].clone());
        let (mut window, _guard) = on_second_monitors(monitors, &source_monitor, &card_display);
        let context = egui::Context::default();
        start_and_wait(&mut window, &context);
        assert_eq!(window.status.text(), "Running");

        let unplugged = Unplugged::monitor(&source_monitor.device_path);
        wait_for(
            "Waiting for source monitor",
            Duration::from_secs(15),
            || {
                window.follow(&context);
                window.status.mirror == Status::WaitingForSourceMonitor
            },
        );
        assert!(!window.mirror.is_running(), "the mirror still runs");
        assert!(window.status.shows_stop(), "the mirror won't come back");

        drop(unplugged);
        wait_for("the mirror to come back", Duration::from_secs(30), || {
            window.follow(&context);
            window.mirror.is_running() && window.status.mirror == Status::Running
        });
        std::thread::sleep(Duration::from_secs(1));
        window.follow(&context);
        assert!(
            window.mirror.is_running(),
            "the mirror ended after the replug"
        );
        assert_eq!(window.status.text(), "Running");
    }

    #[cfg(windows)]
    #[test]
    fn second_monitor_dropdown_change_while_running_moves_the_mirror_and_the_guard_at_once() {
        let Some(monitors) = guard::second_monitor::monitors(3) else {
            return;
        };
        let (first, second) = (monitors[1].clone(), monitors[2].clone());
        let (mut window, guard) = on_second_monitors(monitors, &first, &second);
        let context = egui::Context::default();
        start_and_wait(&mut window, &context);
        assert_eq!(window.status.text(), "Running");
        assert_eq!(mirror::shown_at(), Some((second.left, second.top)));

        // Swap them: the old source monitor becomes the card display.
        let before = window.settings.clone();
        window.settings.source_monitor = Some(second.device_path.clone());
        window.settings.card_display = Some(first.device_path.clone());
        window.follow_settings(&before);
        guard::second_monitor::wait_for("the mirror to run", Duration::from_secs(10), || {
            window.follow(&context);
            window.status.mirror == Status::Running
        });
        assert_eq!(window.status.text(), "Running");
        assert!(window.mirror.is_running(), "the mirror did not restart");
        assert_eq!(
            mirror::shown_at(),
            Some((first.left, first.top)),
            "the mirror window is not on the new card display"
        );
        assert_eq!(
            guard.1.borrow().as_deref(),
            Some(first.device_path.as_str()),
            "the guard guards another card display than the mirror shows on"
        );
    }

    #[test]
    fn playback_devices_that_cannot_be_listed_are_kept_and_reported() {
        let mut window = window("card");
        window.playback_devices = vec![AudioDevice {
            id: "speakers".into(),
            name: "Speakers".into(),
            description: "Speakers".into(),
            container_id: String::new(),
        }];
        window.take_playback_devices(Err("Could not list playback devices.".into()));
        assert_eq!(window.playback_devices.len(), 1);
        assert_eq!(
            window.status.text(),
            "Stopped | Could not list playback devices."
        );
    }

    #[test]
    fn no_audio_runs_the_mirror_without_the_relay() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.settings.sound_output = SoundOutput::NoAudio;
        window.start();
        assert_eq!(window.status.text(), "Running");
        assert_eq!(*relay.0.borrow(), ["stop"]);
    }

    #[test]
    fn a_new_sound_output_restarts_the_audio_but_not_the_mirror() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        // A mirror restart would fail and say so.
        fails_to_start(&mut window, Ended::Stop("the mirror restarted".into()));
        let before = window.settings.clone();
        window.settings.sound_output = SoundOutput::Device("hdmi".into());
        window.follow_settings(&before);
        assert_eq!(
            *relay.0.borrow(),
            [
                "stop",
                "start speakers to card in card container",
                "stop",
                "start speakers to card in card container via hdmi"
            ]
        );
        assert_eq!(window.status.text(), "Running");
    }

    #[test]
    fn no_sound_output_found_offers_to_choose_it_and_the_mirror_keeps_running() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        relay.1.borrow_mut().push(RelayReport::NoSoundOutput);
        window.follow_reports();
        assert_eq!(
            window.status.text(),
            "Running | Can't find the capture card's sound output. Choose it, or choose No audio if the capture card has no sound."
        );
        assert!(window.status.choose_sound_output);
        assert!(window.mirror.is_running());
        relay.1.borrow_mut().push(RelayReport::Relaying);
        window.follow_reports();
        assert!(!window.status.choose_sound_output);
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
            fail_start: None,
        });
    }

    fn fails_to_start(window: &mut SettingsWindow, failure: Ended) {
        window.mirror = Box::new(StandInMirror {
            fail_start: Some(failure),
            ..StandInMirror::default()
        });
    }

    #[test]
    fn transient_capture_failure_at_launch_is_retried_without_clicks() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        assert!(!launched_minimised(&mut window));
        assert_eq!(window.status.text(), "secure desktop");
        assert!(window.status.shows_stop());
        assert!(!window.mirror.is_running());
        // The retry waits for the restart timer.
        window.follow_mirror(&egui::Context::default());
        assert!(!window.mirror.is_running());
        window.restart_at = Instant::now();
        window.follow_mirror(&egui::Context::default());
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
        assert_eq!(
            relay.0.borrow().last().map(String::as_str),
            Some("start speakers to card in card container")
        );
    }

    #[test]
    fn permanent_capture_failure_at_launch_stops() {
        let mut window = window("card");
        fails_to_start(&mut window, Ended::Stop("driver too old".into()));
        window.launch(&egui::Context::default());
        assert_eq!(window.status.text(), "driver too old");
        assert!(!window.status.shows_stop());
        window.restart_at = Instant::now();
        window.follow_mirror(&egui::Context::default());
        assert!(!window.mirror.is_running());
        assert_eq!(window.status.text(), "driver too old");
    }

    #[test]
    fn source_monitor_lost_says_so_and_the_mirror_returns_with_it() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.start();
        let all = window.monitors.clone();
        let without_source = vec![all[1].clone()];
        window.follow_monitors(without_source);
        assert!(!window.mirror.is_running());
        assert_eq!(window.status.text(), "Waiting for source monitor");
        assert!(window.status.shows_stop());
        assert_eq!(relay.0.borrow().last().map(String::as_str), Some("stop"));
        window.follow_monitors(all);
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
        assert_eq!(
            relay.0.borrow().last().map(String::as_str),
            Some("start speakers to card in card container")
        );
    }

    #[test]
    fn card_display_with_a_new_device_path_is_followed_on_its_connector() {
        let guard = ReportingGuard::default();
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.guard = Box::new(guard.clone());
        window.start();
        // Display EDID mode: the card display comes back named after the next display, with a new device path.
        let monitors = window.monitors.clone();
        window.follow_monitors(monitors.clone());
        let renamed = Monitor {
            device_path: "card renamed".into(),
            name: "LG TV".into(),
            container_id: "TV container".into(),
            ..monitors[1].clone()
        };
        window.follow_monitors(vec![monitors[0].clone(), renamed]);
        assert_eq!(
            window.settings.card_display.as_deref(),
            Some("card renamed")
        );
        assert_eq!(guard.1.borrow().as_deref(), Some("card renamed"));
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
        assert_eq!(
            relay.0.borrow().last().map(String::as_str),
            Some("start speakers to LG TV in TV container")
        );
    }

    #[test]
    fn dropdown_change_while_running_moves_the_mirror_audio_and_guard_at_once() {
        let guard = ReportingGuard::default();
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        window.guard = Box::new(guard.clone());
        window.monitors.push(monitor("card 2", false));
        window.start();
        let before = window.settings.clone();
        window.settings.card_display = Some("card 2".into());
        window.follow_settings(&before);
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
        assert_eq!(
            relay.0.borrow().last().map(String::as_str),
            Some("start speakers to card 2 in card 2 container")
        );
        assert_eq!(guard.1.borrow().as_deref(), Some("card 2"));
    }

    #[test]
    fn refused_dropdown_change_stops_the_running_mirror() {
        let mut window = window("card");
        window.start();
        let before = window.settings.clone();
        window.settings.card_display = Some("source".into());
        window.follow_settings(&before);
        assert!(!window.mirror.is_running());
        assert_eq!(
            window.status.text(),
            "The source monitor and the card display must be different monitors."
        );
    }

    #[test]
    fn dropdown_change_leaves_a_stopped_mirror_stopped() {
        let mut window = window("card");
        let before = window.settings.clone();
        window.settings.source_monitor = Some("card".into());
        window.follow_settings(&before);
        assert!(!window.mirror.is_running());
        assert_eq!(window.status.text(), "Stopped");
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

    #[test]
    fn mirror_stopped_by_an_error_does_not_restart_on_a_display_change() {
        let mut window = window("card");
        window.start();
        mirror_ends(&mut window, Ended::Stop("The mirror stopped.".into()));
        window.follow_mirror(&egui::Context::default());
        let monitors = window.monitors.clone();
        window.follow_monitors(monitors);
        assert!(!window.mirror.is_running());
        assert_eq!(window.status.text(), "The mirror stopped.");
    }

    #[test]
    fn stopped_mirror_does_not_start_on_a_display_change() {
        let mut window = window("card");
        window.start();
        window.stop(Status::Stopped);
        let monitors = window.monitors.clone();
        window.follow_monitors(monitors);
        assert!(!window.mirror.is_running());
    }

    #[test]
    fn failed_restarts_back_off_and_a_running_mirror_resets_the_backoff() {
        let mut window = window("card");
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        window.start();
        let waits = |window: &SettingsWindow| {
            window
                .restart_at
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
        };
        assert!((0.9..=1.0).contains(&waits(&window)), "{}", waits(&window));
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        window.restart_at = Instant::now();
        window.follow_mirror(&egui::Context::default());
        assert_eq!(window.status.text(), "secure desktop");
        assert!((1.9..=2.0).contains(&waits(&window)), "{}", waits(&window));
        window.restart_at = Instant::now();
        window.follow_mirror(&egui::Context::default());
        assert_eq!(window.status.text(), "Running");
        // It ran, so losing it again restarts it at once.
        mirror_ends(&mut window, Ended::Restart("lost".into()));
        window.follow_mirror(&egui::Context::default());
        assert_eq!(window.status.text(), "Running");
    }

    #[test]
    fn a_dropdown_change_starts_the_backoff_over() {
        let mut window = window("card");
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        window.start();
        // As after a few failed restarts.
        window.attempts = 5;
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        let before = window.settings.clone();
        window.settings.audio_source = Some("headset".into());
        window.follow_settings(&before);
        assert_eq!(window.status.text(), "secure desktop");
        let wait = window.restart_at.saturating_duration_since(Instant::now());
        assert!(wait <= Duration::from_secs(1), "{wait:?}");
    }

    #[test]
    fn a_device_change_does_not_skip_the_restart_timer_but_a_changed_monitor_does() {
        let mut window = window("card");
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        window.start();
        // A USB device came or went: the monitors are the same.
        let monitors = window.monitors.clone();
        window.follow_monitors(monitors);
        assert!(!window.mirror.is_running(), "the restart timer was skipped");
        assert_eq!(window.status.text(), "secure desktop");
        let mut monitors = window.monitors.clone();
        monitors[1].refresh_hz = 30;
        window.follow_monitors(monitors);
        assert!(window.mirror.is_running());
        assert_eq!(window.status.text(), "Running");
    }

    #[test]
    fn a_display_change_reaches_the_guard_and_the_mirror_from_one_listener() {
        let guard = ReportingGuard::default();
        let mut window = window("card");
        window.guard = Box::new(guard.clone());
        window.displays_changed.store(true, Ordering::Relaxed);
        window.follow(&egui::Context::default());
        assert_eq!(*guard.2.borrow(), 1);
        window.follow(&egui::Context::default());
        assert_eq!(
            *guard.2.borrow(),
            1,
            "the guard followed a change that did not happen"
        );
    }

    /// A mirror that starts in the background: it shows the picture only when the test says so.
    #[derive(Default, Clone)]
    /// `.0` is (running, shows the picture); `.1` counts starts.
    struct SlowMirror(
        std::rc::Rc<std::cell::Cell<(bool, bool)>>,
        std::rc::Rc<std::cell::Cell<u32>>,
    );

    impl Mirror for SlowMirror {
        fn start(&mut self, _source: &Monitor, _card_display: &Monitor) -> Result<(), Ended> {
            self.0.set((true, false));
            self.1.set(self.1.get() + 1);
            Ok(())
        }
        fn stop(&mut self) {
            self.0.set((false, false));
        }
        fn is_running(&self) -> bool {
            self.0.get().0
        }
        fn shows_picture(&self) -> bool {
            self.0.get() == (true, true)
        }
        fn take_end(&mut self) -> Option<Ended> {
            None
        }
    }

    #[test]
    fn mirror_starting_in_the_background_says_so_then_runs_with_audio_and_minimises() {
        let (mut window, relay) = with_relay(window("card"), Some("speakers"));
        let mirror = SlowMirror::default();
        window.mirror = Box::new(mirror.clone());
        assert!(!launched_minimised(&mut window));
        assert_eq!(window.status.text(), "Starting");
        assert!(window.status.shows_stop());
        assert_eq!(
            *relay.0.borrow(),
            ["stop"],
            "audio started before the mirror"
        );
        mirror.0.set((true, true));
        let context = egui::Context::default();
        window.follow(&context);
        assert_eq!(window.status.text(), "Running");
        assert_eq!(
            relay.0.borrow().last().map(String::as_str),
            Some("start speakers to card in card container")
        );
        let mut output = context.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Minimized(true)),
            "launch did not minimise once the mirror ran"
        );
    }

    #[test]
    fn restart_in_the_background_keeps_saying_why_until_the_mirror_runs() {
        let mut window = window("card");
        fails_to_start(&mut window, Ended::Restart("secure desktop".into()));
        window.start();
        let mirror = SlowMirror::default();
        window.mirror = Box::new(mirror.clone());
        window.restart_at = Instant::now();
        window.follow_mirror(&egui::Context::default());
        assert!(window.mirror.is_running(), "the restart did not start");
        assert_eq!(window.status.text(), "secure desktop");
        // The next pass while it starts must not start it again.
        window.restart_at = Instant::now();
        window.follow_mirror(&egui::Context::default());
        assert_eq!(mirror.1.get(), 1, "a starting restart was started again");
        assert_eq!(window.status.text(), "secure desktop");
        mirror.0.set((true, true));
        window.follow_mirror(&egui::Context::default());
        assert_eq!(window.status.text(), "Running");
    }

    #[test]
    fn guard_notice_survives_start_stop_and_restart_until_the_guard_clears_it() {
        let guard = ReportingGuard::default();
        let mut window = window("card");
        window.guard = Box::new(guard.clone());
        guard
            .0
            .borrow_mut()
            .push(GuardReport::Notice("given up".into()));
        window.follow_reports();
        window.start();
        mirror_ends(&mut window, Ended::Restart("lost".into()));
        window.follow_mirror(&egui::Context::default());
        assert_eq!(window.status.text(), "Running | given up");
        window.stop(Status::Stopped);
        assert_eq!(window.status.text(), "Stopped | given up");
        window.follow_reports();
        assert_eq!(window.status.text(), "Stopped | given up");
        guard.0.borrow_mut().push(GuardReport::Clear);
        window.follow_reports();
        assert_eq!(window.status.text(), "Stopped");
    }

    /// The monitors an input thread read over DDC/CI, and how many input threads started.
    #[derive(Default, Clone)]
    struct InputRecord(
        Arc<std::sync::Mutex<Vec<String>>>,
        Arc<std::sync::atomic::AtomicU32>,
    );

    /// `window` with an input thread on monitors that don't answer DDC/CI, recording what it reads.
    fn with_input_thread(mut window: SettingsWindow) -> (SettingsWindow, InputRecord) {
        let record = InputRecord::default();
        let recording = record.clone();
        window.start_input_thread = Box::new(move || {
            recording.1.fetch_add(1, Ordering::Relaxed);
            let read = recording.0.clone();
            input::InputThread::spawn(
                |_: &Monitor| None,
                move |monitor: &Monitor| {
                    read.lock().unwrap().push(monitor.device_path.clone());
                    None
                },
                || {},
            )
        });
        (window, record)
    }

    /// Follows the input thread's reports until `done`.
    fn follow_input_thread(window: &mut SettingsWindow, done: impl Fn(&SettingsWindow) -> bool) {
        let start = Instant::now();
        while !done(window) {
            assert!(start.elapsed() < Duration::from_secs(10), "no input report");
            std::thread::sleep(Duration::from_millis(5));
            window.follow_reports();
        }
    }

    fn switch_input_switching(window: &mut SettingsWindow, on: bool) {
        let before = window.settings.clone();
        window.settings.input_switching = on;
        window.follow_settings(&before);
    }

    #[test]
    fn nothing_is_read_over_ddc_ci_while_input_switching_is_off() {
        let (mut window, record) = with_input_thread(window("card"));
        let context = egui::Context::default();
        window.launch(&context);
        let monitors = window.monitors.clone();
        window.follow_monitors(monitors.clone());
        window.start();
        window.follow(&context);
        assert!(window.input_thread.is_none());
        assert_eq!(
            record.1.load(Ordering::Relaxed),
            0,
            "an input thread started"
        );

        switch_input_switching(&mut window, true);
        follow_input_thread(&mut window, |window| !window.input_lists.is_empty());
        assert_eq!(*record.0.lock().unwrap(), ["source", "card"]);
        assert_eq!(
            window.status.text(),
            "Running | No monitor offers input switching over DDC/CI."
        );

        switch_input_switching(&mut window, false);
        assert!(window.input_lists.is_empty());
        assert_eq!(window.status.text(), "Running");
        window.follow_monitors(monitors);
        window.follow(&context);
        // Give a wrongly sent job time to run.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            record.0.lock().unwrap().len(),
            2,
            "read a monitor while off"
        );

        // On again: the same input thread reads every monitor again.
        switch_input_switching(&mut window, true);
        follow_input_thread(&mut window, |window| !window.input_lists.is_empty());
        assert_eq!(
            *record.0.lock().unwrap(),
            ["source", "card", "source", "card"]
        );
        assert_eq!(record.1.load(Ordering::Relaxed), 1, "a second input thread");
    }

    #[test]
    fn input_lists_are_read_at_launch_and_for_new_monitors_after_a_display_change() {
        let (mut window, record) = with_input_thread(window("card"));
        window.settings.input_switching = true;
        window.launch(&egui::Context::default());
        follow_input_thread(&mut window, |window| !window.input_lists.is_empty());
        window.input_lists.clear();
        let mut monitors = window.monitors.clone();
        monitors.push(monitor("new", false));
        window.follow_monitors(monitors);
        follow_input_thread(&mut window, |window| !window.input_lists.is_empty());
        assert_eq!(
            *record.0.lock().unwrap(),
            ["source", "card", "new"],
            "read a known monitor again"
        );
        assert_eq!(window.input_lists.len(), 3);
        assert_eq!(record.1.load(Ordering::Relaxed), 1, "one input thread");
    }

    #[test]
    fn a_switch_the_monitor_does_not_answer_is_said_in_the_status_line() {
        let (mut window, _record) = with_input_thread(window("card"));
        switch_input_switching(&mut window, true);
        let card = window.monitors[1].clone();
        window.send_input_job(input::Job::Switch(card, 0x11));
        follow_input_thread(&mut window, |window| {
            window.status.text().contains("Could not switch")
        });
        assert_eq!(
            window.status.text(),
            "Stopped | Could not switch card to HDMI 1: the monitor did not answer over DDC/CI."
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn on_linux_input_switching_says_it_does_not_work_yet() {
        let mut window = window("card");
        switch_input_switching(&mut window, true);
        assert!(window.input_thread.is_none());
        assert_eq!(
            window.status.text(),
            "Stopped | Input switching does not work on Linux yet."
        );
        switch_input_switching(&mut window, false);
        assert_eq!(window.status.text(), "Stopped");
    }
}
