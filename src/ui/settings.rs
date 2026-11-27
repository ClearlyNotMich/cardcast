//! Settings model: saved choices, start-up refusal rules and the status line. No Windows calls.

use crate::audio::{self, AudioDevice, SoundOutput};
use crate::guard::{GuardReport, Monitor};
use crate::mirror::{Ended, Mirror};
use std::time::Duration;

/// What the mirror is doing; it alone decides the Start/Stop button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Stopped,
    /// Started; the mirror shows the picture once it opened the graphics card and the window.
    Starting,
    Running,
    WaitingForCardDisplay,
    WaitingForSourceMonitor,
    /// The mirror failed to start or ended on its own and is being started again; why, in plain words.
    Restarting(String),
    /// The mirror is stopped by this error, in plain words.
    Error(String),
}

impl Status {
    /// The status in words, as the tests read it; the window shows `look::headline` and `StatusLine::details`.
    #[cfg(test)]
    pub fn text(&self) -> String {
        match self {
            Self::Stopped => "Stopped".into(),
            Self::Starting => "Starting".into(),
            Self::Running => "Running".into(),
            Self::WaitingForCardDisplay => "Waiting for card display".into(),
            Self::WaitingForSourceMonitor => "Waiting for source monitor".into(),
            Self::Restarting(message) | Self::Error(message) => message.clone(),
        }
    }
}

/// The one-line status under the controls: the mirror, then any audio, guard or other problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub mirror: Status,
    /// The running mirror keeps up with too few of the card display's refreshes (`look::FrameRate`).
    pub slow_mirror: Option<String>,
    /// The audio relay's problem; cleared when it relays again, or on Start and Stop.
    pub audio: Option<String>,
    /// Automatic found no sound output: the status line offers "Choose sound output".
    pub choose_sound_output: bool,
    /// The guard's notice; kept until the guard clears it or the user dismisses it.
    pub guard: Option<String>,
    /// The notice the user dismissed; the guard re-sends it after every display change.
    dismissed_guard: Option<String>,
    /// The display watcher could not start; kept until cardcast restarts.
    pub displays: Option<String>,
    /// A settings or monitor-list problem; cleared on Start and Stop.
    pub other: Option<String>,
}

impl StatusLine {
    pub fn new(mirror: Status) -> Self {
        Self {
            mirror,
            slow_mirror: None,
            audio: None,
            choose_sound_output: false,
            guard: None,
            dismissed_guard: None,
            displays: None,
            other: None,
        }
    }

    /// Start and Stop: the new mirror status clears its speed, audio and other problems, not the lasting ones.
    pub fn set_mirror(&mut self, mirror: Status) {
        self.mirror = mirror;
        self.slow_mirror = None;
        self.audio = None;
        self.choose_sound_output = false;
        self.other = None;
    }

    /// Takes the guard's report, unless it repeats the notice the user dismissed.
    pub fn follow_guard(&mut self, report: GuardReport) {
        match report {
            GuardReport::Notice(notice) if self.dismissed_guard.as_ref() == Some(&notice) => {}
            GuardReport::Notice(notice) => {
                self.dismissed_guard = None;
                self.guard = Some(notice);
            }
            GuardReport::Clear => {
                self.dismissed_guard = None;
                self.guard = None;
            }
        }
    }

    pub fn dismiss_guard_notice(&mut self) {
        self.dismissed_guard = self.guard.take();
    }

    /// The whole status line, as the tests read it.
    #[cfg(test)]
    pub fn text(&self) -> String {
        let mut text = self.mirror.text();
        let problems = [&self.audio, &self.guard, &self.displays, &self.other];
        for problem in problems.into_iter().flatten() {
            text.push_str(" | ");
            text.push_str(problem);
        }
        text
    }

    /// What the status box under the controls says, or `None` to hide it: why the mirror stopped or
    /// restarts, then any problem. The header already shows the mirror's state.
    pub fn details(&self) -> Option<String> {
        let reason = match &self.mirror {
            Status::Restarting(message) | Status::Error(message) => Some(message.as_str()),
            Status::WaitingForCardDisplay => Some(
                "The saved card display is not connected. Plug it in, or choose another card display.",
            ),
            _ => None,
        };
        let parts: Vec<&str> = [
            reason,
            self.slow_mirror.as_deref(),
            self.audio.as_deref(),
            self.guard.as_deref(),
            self.displays.as_deref(),
            self.other.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" | "))
    }

    /// Stop while the mirror runs or will start by itself; Start once it is stopped.
    pub fn shows_stop(&self) -> bool {
        !matches!(self.mirror, Status::Stopped | Status::Error(_))
    }
}

/// The status after a start made while restarting: a failure keeps restarting instead of stopping.
pub fn restarted(status: Status) -> Status {
    match status {
        Status::Error(message) => Status::Restarting(message),
        status => status,
    }
}

/// How long a restarting mirror waits before its next start, given the starts made since it last
/// showed the picture: at once after it ran, then 1 s doubling to 30 s. It never gives up, so a
/// slow mode change still recovers (SPEC section 9), but a failure that keeps coming back costs
/// the sender a device and a window only twice a minute.
pub fn restart_delay(attempts: u32) -> Duration {
    match attempts {
        0 => Duration::ZERO,
        attempts => Duration::from_secs(1 << (attempts - 1).min(5)).min(Duration::from_secs(30)),
    }
}

/// Choices that can't work together, in plain words; the one source of the refusal text. The
/// dropdowns and the live layout never offer them (`look::Role::offers`), and Start refuses them.
/// One can still happen: Windows can make the card display the primary monitor.
pub fn role_problem(settings: &Settings, monitors: &[Monitor]) -> Option<&'static str> {
    let card_display = settings.card_display.as_ref()?;
    let primary = monitors
        .iter()
        .any(|monitor| monitor.primary && &monitor.device_path == card_display);
    if settings.source_monitor.as_ref() == Some(card_display) {
        Some("The source monitor and the card display must be different monitors.")
    } else if primary {
        Some("The card display can't be the primary monitor.")
    } else if settings.send_windows_to.as_ref() == Some(card_display) {
        Some("\"Send windows to\" can't be the card display.")
    } else {
        None
    }
}

/// Applies the refusal rules, then starts the mirror in the background.
pub fn start(settings: &Settings, monitors: &[Monitor], mirror: &mut dyn Mirror) -> Status {
    let (Some(source_path), Some(card_path)) = (&settings.source_monitor, &settings.card_display)
    else {
        return Status::Error("Choose a source monitor and a card display.".into());
    };
    if let Some(problem) = role_problem(settings, monitors) {
        return Status::Error(problem.into());
    }
    let find = |path: &str| monitors.iter().find(|monitor| monitor.device_path == path);
    let Some(card_display) = find(card_path) else {
        return Status::WaitingForCardDisplay;
    };
    let Some(source_monitor) = find(source_path) else {
        return Status::WaitingForSourceMonitor;
    };
    match mirror.start(source_monitor, card_display) {
        Ok(()) => Status::Starting,
        Err(Ended::Restart(message)) => Status::Restarting(message),
        Err(Ended::Stop(message)) => Status::Error(message),
    }
}

/// What the settings window does to the mirror after a display change.
#[derive(Debug, PartialEq, Eq)]
pub enum Follow {
    Nothing,
    /// (Re)start it: a monitor came back or changed, or the mirror ended on its own.
    Start,
    /// Stop it with this waiting status: the source monitor or card display is gone.
    Stop(Status),
}

/// `wanted` is whether the mirror runs or will start by itself ([`StatusLine::shows_stop`]), so a
/// mirror stopped by Stop or an error stays stopped. `mirror_on` is whether it runs, starts or
/// waits on its restart timer: then only a changed monitor starts it again, so a device change
/// (a USB device too) does not skip the restart backoff. `before` and `now` are the source monitor
/// and card display in the monitor list before and after the change.
pub fn follow_displays(
    wanted: bool,
    mirror_on: bool,
    before: [Option<&Monitor>; 2],
    now: [Option<&Monitor>; 2],
) -> Follow {
    match now {
        _ if !wanted => Follow::Nothing,
        [_, None] => Follow::Stop(Status::WaitingForCardDisplay),
        [None, _] => Follow::Stop(Status::WaitingForSourceMonitor),
        _ if !mirror_on || before != now => Follow::Start,
        _ => Follow::Nothing,
    }
}

/// A changed source monitor, card display or audio source restarts a mirror that runs or will
/// start by itself, so it follows the choice at once, as the guard does.
pub fn restarts_on_change(wanted: bool, before: &Settings, after: &Settings) -> bool {
    wanted
        && (before.source_monitor != after.source_monitor
            || before.card_display != after.card_display
            || before.audio_source != after.audio_source)
}

/// Saved settings start the mirror at launch once both monitors were chosen.
pub fn starts_on_launch(settings: &Settings) -> bool {
    settings.source_monitor.is_some() && settings.card_display.is_some()
}

/// Keeps the saved card display found: a new EDID (Display EDID mode on a capture card) gives it
/// a new device path on the same connector, so a card display missing by its path is the monitor
/// on its saved connector (unless it is the primary monitor or has another role), and takes
/// that monitor's path. The connector of the card display found
/// is saved too. Returns whether the settings changed, so they are saved.
pub fn follow_card_display(settings: &mut Settings, monitors: &[Monitor]) -> bool {
    let Some(card_path) = &settings.card_display else {
        return false;
    };
    let saved_connector = settings.card_display_connector.as_deref().unwrap_or("");
    let found = monitors
        .iter()
        .find(|monitor| &monitor.device_path == card_path)
        .or_else(|| {
            // Never another role's monitor or the primary monitor: the mirror would cover it.
            let taken = [&settings.source_monitor, &settings.send_windows_to];
            monitors.iter().find(|monitor| {
                !saved_connector.is_empty()
                    && monitor.connector == saved_connector
                    && !monitor.primary
                    && !taken.contains(&&Some(monitor.device_path.clone()))
            })
        });
    let Some(card_display) = found.filter(|monitor| !monitor.connector.is_empty()) else {
        return false;
    };
    let before = (
        settings.card_display.clone(),
        settings.card_display_connector.clone(),
    );
    settings.card_display = Some(card_display.device_path.clone());
    settings.card_display_connector = Some(card_display.connector.clone());
    before
        != (
            settings.card_display.clone(),
            settings.card_display_connector.clone(),
        )
}

/// The "Sound output" line's value: No audio, the chosen playback device while it is connected,
/// else "Automatic" with what it finds for the card display now.
pub fn sound_output_label(
    settings: &Settings,
    devices: &[AudioDevice],
    card_display: Option<&Monitor>,
) -> String {
    let source = settings.audio_source.as_deref();
    match &settings.sound_output {
        SoundOutput::NoAudio => return "No audio".into(),
        SoundOutput::Device(id) => {
            if let Some(device) = devices
                .iter()
                .find(|device| &device.id == id && Some(id.as_str()) != source)
            {
                return device.name.clone();
            }
        }
        SoundOutput::Automatic => {}
    }
    let (Some(source), Some(card_display)) = (source, card_display) else {
        return "Automatic".into();
    };
    match audio::card_output(
        devices,
        source,
        &SoundOutput::Automatic,
        &card_display.container_id,
        &card_display.name,
    ) {
        Ok(Some(device)) => format!("Automatic · {}", device.name),
        _ => "Automatic · not found".into(),
    }
}

/// The audio source dropdown's choices: every playback device but the sound output in use (the
/// chosen one, or what Automatic finds for the card display), so the two never collide.
pub fn audio_source_choices<'a>(
    devices: &'a [AudioDevice],
    settings: &Settings,
    card_display: Option<&Monitor>,
) -> Vec<&'a AudioDevice> {
    let in_use = card_display.and_then(|card_display| {
        audio::sound_output_device(
            devices,
            &settings.sound_output,
            &card_display.container_id,
            &card_display.name,
        )
    });
    devices
        .iter()
        .filter(|device| Some(&device.id) != in_use.map(|in_use| &in_use.id))
        .collect()
}

/// The sound output dialog's choices: Automatic, every playback device but the audio source, No audio.
pub fn sound_output_choices(
    devices: &[AudioDevice],
    audio_source: Option<&str>,
) -> Vec<(SoundOutput, String)> {
    let mut choices = vec![(SoundOutput::Automatic, "Automatic".to_owned())];
    choices.extend(
        devices
            .iter()
            .filter(|device| Some(device.id.as_str()) != audio_source)
            .map(|device| (SoundOutput::Device(device.id.clone()), device.name.clone())),
    );
    choices.push((SoundOutput::NoAudio, "No audio".to_owned()));
    choices
}

/// The user's choices. Monitors are stored by their display-configuration device path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub source_monitor: Option<String>,
    pub card_display: Option<String>,
    pub send_windows_to: Option<String>,
    pub audio_source: Option<String>,
    pub sound_output: SoundOutput,
    /// The card display's connector ([`Monitor::connector`]), to find it when its device path changes.
    pub card_display_connector: Option<String>,
    pub guard_enabled: bool,
}

impl Default for Settings {
    /// The guard is on: it does nothing until the card display is chosen.
    fn default() -> Self {
        Self {
            source_monitor: None,
            card_display: None,
            send_windows_to: None,
            audio_source: None,
            sound_output: SoundOutput::Automatic,
            card_display_connector: None,
            guard_enabled: true,
        }
    }
}

impl Settings {
    /// Fills choices the user never made: "send windows to" defaults to the primary monitor.
    pub fn with_defaults(mut self, monitors: &[Monitor]) -> Self {
        if self.send_windows_to.is_none() {
            self.send_windows_to = monitors
                .iter()
                .find(|monitor| monitor.primary)
                .map(|monitor| monitor.device_path.clone());
        }
        self
    }

    /// Clears a role an older cardcast saved on the card display: the source monitor, so the card
    /// display and its guard stay, and "send windows to", which [`Self::with_defaults`] then sets
    /// to the primary monitor. Returns what was cleared, in plain words. A card display that is
    /// the primary monitor is kept: that is Windows' live state, and Start refuses it.
    pub fn clear_role_clashes(&mut self) -> Option<String> {
        let mut cleared = Vec::new();
        if self.card_display.is_some() && self.source_monitor == self.card_display {
            self.source_monitor = None;
            cleared.push(
                "The source monitor was the card display, so it was cleared. Choose a source monitor.",
            );
        }
        if self.card_display.is_some() && self.send_windows_to == self.card_display {
            self.send_windows_to = None;
            cleared.push(
                "\"Send windows to\" was the card display, so it is the primary monitor again.",
            );
        }
        (!cleared.is_empty()).then(|| cleared.join(" "))
    }

    /// Loads the saved settings; a missing or unreadable file gives defaults.
    pub fn load() -> Self {
        settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::from_text(&text))
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path().ok_or("Could not find a folder to save settings in.")?;
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|error| format!("Could not save settings: {error}"))?;
        }
        std::fs::write(&path, self.to_text())
            .map_err(|error| format!("Could not save settings: {error}"))
    }

    /// One `key=value` per line; device paths and audio ids never contain newlines.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for (key, value) in [
            ("source_monitor", &self.source_monitor),
            ("card_display", &self.card_display),
            ("send_windows_to", &self.send_windows_to),
            ("audio_source", &self.audio_source),
            ("card_display_connector", &self.card_display_connector),
        ] {
            if let Some(value) = value {
                text.push_str(&format!("{key}={value}\n"));
            }
        }
        text.push_str(&format!("sound_output={}\n", self.sound_output.to_text()));
        text.push_str(&format!("guard_enabled={}\n", self.guard_enabled));
        text
    }

    /// Unknown keys and malformed lines are ignored, so a damaged file falls back to defaults.
    pub fn from_text(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            let value = Some(value.to_owned());
            match key {
                "source_monitor" => settings.source_monitor = value,
                "card_display" => settings.card_display = value,
                "send_windows_to" => settings.send_windows_to = value,
                "audio_source" => settings.audio_source = value,
                "card_display_connector" => settings.card_display_connector = value,
                "sound_output" => {
                    settings.sound_output = SoundOutput::from_text(value.as_deref().unwrap_or(""));
                }
                "guard_enabled" => settings.guard_enabled = value.as_deref() == Some("true"),
                _ => {}
            }
        }
        settings
    }
}

/// %APPDATA%\\cardcast on Windows, the XDG config folder elsewhere.
fn settings_path() -> Option<std::path::PathBuf> {
    // Tests never touch the user's own settings.
    if cfg!(test) {
        return Some(
            std::env::temp_dir()
                .join("cardcast-tests")
                .join("settings.txt"),
        );
    }
    let folder = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
        })?;
    Some(folder.join("cardcast").join("settings.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{Ended, StandInMirror};

    #[test]
    fn settings_survive_save_and_load() {
        let saved = Settings {
            source_monitor: Some(r"\\?\DISPLAY#GSM1#5&1&UID1".into()),
            card_display: Some(r"\\?\DISPLAY#ELG1#5&2&UID2".into()),
            send_windows_to: Some(r"\\?\DISPLAY#DEL1#5&3&UID3".into()),
            audio_source: Some("{0.0.0.00000000}.{abc}".into()),
            sound_output: SoundOutput::Device("{0.0.0.00000000}.{hdmi}".into()),
            card_display_connector: Some(r"\\?\PCI#VEN_8086#3&1&0#{guid} target 4353".into()),
            guard_enabled: true,
        };
        assert_eq!(Settings::from_text(&saved.to_text()), saved);
    }

    fn playback(id: &str, name: &str, container_id: &str) -> AudioDevice {
        AudioDevice {
            id: id.into(),
            name: name.into(),
            description: name.into(),
            container_id: container_id.into(),
        }
    }

    /// The sender in Display EDID mode: the card display reads "Elgato 1080p", its playback device "HD60 X".
    fn sound_devices() -> (Vec<AudioDevice>, Monitor) {
        let devices = vec![
            playback("speakers", "Speakers", "computer"),
            playback("hdmi", "HD60 X", "card container"),
        ];
        let card = Monitor {
            name: "Elgato 1080p".into(),
            container_id: "card container".into(),
            ..monitor("card", false)
        };
        (devices, card)
    }

    fn with_output(output: SoundOutput) -> Settings {
        Settings {
            audio_source: Some("speakers".into()),
            sound_output: output,
            ..chosen("source", "card")
        }
    }

    #[test]
    fn sound_output_line_names_the_choice_and_what_automatic_finds() {
        let (devices, card) = sound_devices();
        let label = |output, card: Option<&Monitor>| {
            sound_output_label(&with_output(output), &devices, card)
        };
        assert_eq!(
            label(SoundOutput::Automatic, Some(&card)),
            "Automatic · HD60 X"
        );
        let unknown = Monitor {
            container_id: String::new(),
            ..card.clone()
        };
        assert_eq!(
            label(SoundOutput::Automatic, Some(&unknown)),
            "Automatic · not found"
        );
        assert_eq!(label(SoundOutput::Automatic, None), "Automatic");
        assert_eq!(label(SoundOutput::NoAudio, Some(&card)), "No audio");
        assert_eq!(label(SoundOutput::Device("hdmi".into()), None), "HD60 X");
        assert_eq!(
            label(SoundOutput::Device("gone".into()), Some(&card)),
            "Automatic · HD60 X"
        );
    }

    #[test]
    fn audio_source_choices_leave_out_the_sound_output_in_use() {
        let (devices, card) = sound_devices();
        let names = |output| -> Vec<String> {
            audio_source_choices(&devices, &with_output(output), Some(&card))
                .iter()
                .map(|device| device.id.clone())
                .collect()
        };
        // Automatic resolves to the card display's own sound output.
        assert_eq!(names(SoundOutput::Automatic), ["speakers"]);
        assert_eq!(names(SoundOutput::Device("speakers".into())), ["hdmi"]);
        // A chosen device that is gone falls back to Automatic.
        assert_eq!(names(SoundOutput::Device("gone".into())), ["speakers"]);
        assert_eq!(names(SoundOutput::NoAudio), ["speakers", "hdmi"]);
        // Nothing resolved without a card display: every device is offered.
        assert_eq!(
            audio_source_choices(&devices, &with_output(SoundOutput::Automatic), None).len(),
            2
        );
    }

    #[test]
    fn sound_output_choices_leave_out_the_audio_source() {
        let (devices, _) = sound_devices();
        assert_eq!(
            sound_output_choices(&devices, Some("speakers")),
            [
                (SoundOutput::Automatic, "Automatic".to_owned()),
                (SoundOutput::Device("hdmi".into()), "HD60 X".to_owned()),
                (SoundOutput::NoAudio, "No audio".to_owned()),
            ]
        );
    }

    #[test]
    fn guard_is_on_in_a_fresh_settings_file() {
        assert!(Settings::default().guard_enabled);
        assert!(Settings::from_text("").guard_enabled);
        assert!(!Settings::from_text("guard_enabled=false\n").guard_enabled);
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
            container_id: String::new(),
            connector: String::new(),
        }
    }

    fn sender_monitors() -> Vec<Monitor> {
        vec![
            monitor("source", false),
            monitor("primary", true),
            monitor("card", false),
        ]
    }

    fn on_connector(device_path: &str, connector: &str) -> Monitor {
        Monitor {
            connector: connector.into(),
            ..monitor(device_path, false)
        }
    }

    #[test]
    fn the_card_display_found_by_its_device_path_saves_its_connector() {
        let mut settings = chosen("source", "card");
        let monitors = [on_connector("card", "gpu target 1")];
        assert!(follow_card_display(&mut settings, &monitors));
        assert_eq!(
            settings.card_display_connector.as_deref(),
            Some("gpu target 1")
        );
        assert!(
            !follow_card_display(&mut settings, &monitors),
            "nothing new to save"
        );
    }

    #[test]
    fn a_card_display_with_a_new_device_path_is_found_on_its_connector() {
        // A new EDID (Display EDID mode on the capture card) gives the monitor a new device path.
        let mut settings = Settings {
            card_display_connector: Some("gpu target 1".into()),
            ..chosen("source", "old card")
        };
        let monitors = [
            on_connector("source", "gpu target 0"),
            on_connector("new card", "gpu target 1"),
        ];
        assert!(follow_card_display(&mut settings, &monitors));
        assert_eq!(settings.card_display.as_deref(), Some("new card"));
        assert_eq!(
            settings.card_display_connector.as_deref(),
            Some("gpu target 1")
        );
    }

    #[test]
    fn the_connector_never_makes_another_role_or_the_primary_monitor_the_card_display() {
        for taken in ["source", "send to", "primary"] {
            let mut settings = Settings {
                send_windows_to: Some("send to".into()),
                card_display_connector: Some("gpu target 1".into()),
                ..chosen("source", "old card")
            };
            let unchanged = settings.clone();
            let mut monitors = vec![
                on_connector("source", "gpu target 0"),
                on_connector("send to", "gpu target 2"),
                Monitor {
                    primary: true,
                    ..on_connector("primary", "gpu target 3")
                },
            ];
            monitors
                .iter_mut()
                .find(|monitor| monitor.device_path == taken)
                .unwrap()
                .connector = "gpu target 1".into();
            assert!(!follow_card_display(&mut settings, &monitors), "{taken}");
            assert_eq!(settings, unchanged, "{taken}");
            assert_eq!(
                start(&settings, &monitors, &mut StandInMirror::default()),
                Status::WaitingForCardDisplay,
                "{taken}"
            );
        }
    }

    #[test]
    fn a_card_display_gone_from_its_path_and_connector_is_not_found() {
        let mut settings = Settings {
            card_display_connector: Some("gpu target 1".into()),
            ..chosen("source", "old card")
        };
        let unchanged = settings.clone();
        // An unknown connector (empty) never matches.
        let monitors = [
            on_connector("source", "gpu target 0"),
            on_connector("other", ""),
        ];
        assert!(!follow_card_display(&mut settings, &monitors));
        assert_eq!(settings, unchanged);
        let mut unknown = Settings {
            card_display_connector: Some(String::new()),
            ..chosen("source", "old card")
        };
        assert!(!follow_card_display(&mut unknown, &monitors));
        assert_eq!(unknown.card_display.as_deref(), Some("old card"));
    }

    #[test]
    fn send_windows_to_defaults_to_the_primary_monitor() {
        let settings = Settings::default().with_defaults(&sender_monitors());
        assert_eq!(settings.send_windows_to.as_deref(), Some("primary"));
    }

    #[test]
    fn saved_send_windows_to_is_kept() {
        let settings = Settings {
            send_windows_to: Some("source".into()),
            ..Settings::default()
        }
        .with_defaults(&sender_monitors());
        assert_eq!(settings.send_windows_to.as_deref(), Some("source"));
    }

    fn chosen(source: &str, card: &str) -> Settings {
        Settings {
            source_monitor: Some(source.into()),
            card_display: Some(card.into()),
            ..Settings::default()
        }
    }

    struct FailingMirror(Ended);

    impl Mirror for FailingMirror {
        fn start(&mut self, _source: &Monitor, _card_display: &Monitor) -> Result<(), Ended> {
            Err(self.0.clone())
        }
        fn stop(&mut self) {}
        fn is_running(&self) -> bool {
            false
        }
        fn shows_picture(&self) -> bool {
            false
        }
        fn take_end(&mut self) -> Option<crate::mirror::Ended> {
            None
        }
    }

    fn start_text(settings: &Settings, mirror: &mut dyn Mirror) -> String {
        start(settings, &sender_monitors(), mirror).text()
    }

    #[test]
    fn start_refuses_when_source_monitor_is_the_card_display() {
        assert_eq!(
            start_text(&chosen("card", "card"), &mut StandInMirror::default()),
            "The source monitor and the card display must be different monitors."
        );
    }

    #[test]
    fn start_refuses_when_card_display_is_primary() {
        assert_eq!(
            start_text(&chosen("source", "primary"), &mut StandInMirror::default()),
            "The card display can't be the primary monitor."
        );
    }

    #[test]
    fn start_refuses_without_a_choice() {
        assert_eq!(
            start_text(&Settings::default(), &mut StandInMirror::default()),
            "Choose a source monitor and a card display."
        );
    }

    #[test]
    fn start_waits_when_card_display_is_unplugged() {
        assert_eq!(
            start_text(&chosen("source", "gone"), &mut StandInMirror::default()),
            "Waiting for card display"
        );
    }

    #[test]
    fn start_runs_the_mirror() {
        let mut mirror = StandInMirror::default();
        assert_eq!(
            start_text(&chosen("source", "card"), &mut mirror),
            "Starting"
        );
        assert!(mirror.running);
    }

    #[test]
    fn start_waits_when_source_monitor_is_unplugged() {
        let status = start(
            &chosen("gone", "card"),
            &sender_monitors(),
            &mut StandInMirror::default(),
        );
        assert_eq!(status.text(), "Waiting for source monitor");
        assert!(StatusLine::new(status).shows_stop());
    }

    #[test]
    fn transient_mirror_failure_at_start_keeps_the_stop_button_to_retry() {
        let failure =
            "Could not capture the source monitor. (Desktop Duplication, error 0x80070005)";
        let mut mirror = FailingMirror(Ended::Restart(failure.into()));
        let status = start(&chosen("source", "card"), &sender_monitors(), &mut mirror);
        assert_eq!(status, Status::Restarting(failure.into()));
        assert!(StatusLine::new(status).shows_stop());
    }

    #[test]
    fn permanent_mirror_failure_at_start_stops_with_its_reason() {
        let failure = "Could not start the mirror: the graphics driver is too old. Update it. (Direct3D 11.1)";
        let mut mirror = FailingMirror(Ended::Stop(failure.into()));
        let status = start(&chosen("source", "card"), &sender_monitors(), &mut mirror);
        assert_eq!(status, Status::Error(failure.into()));
        assert!(!StatusLine::new(status).shows_stop());
    }

    #[test]
    fn audio_and_other_problems_follow_the_mirror_status_without_changing_the_button() {
        let line = StatusLine {
            mirror: Status::Running,
            audio: Some("The audio source is not connected.".into()),
            other: Some("Could not save settings: denied".into()),
            ..StatusLine::new(Status::Running)
        };
        assert_eq!(
            line.text(),
            "Running | The audio source is not connected. | Could not save settings: denied"
        );
        assert!(line.shows_stop());
    }

    #[test]
    fn dismissed_guard_notice_stays_dismissed_until_the_guard_says_something_else() {
        let mut line = StatusLine::new(Status::Running);
        let notice = || GuardReport::Notice("given up".into());
        line.follow_guard(notice());
        line.dismiss_guard_notice();
        // A display change re-sends the same notice.
        line.follow_guard(notice());
        assert_eq!(line.text(), "Running");
        line.follow_guard(GuardReport::Notice("other".into()));
        assert_eq!(line.text(), "Running | other");
        line.dismiss_guard_notice();
        line.follow_guard(GuardReport::Clear);
        // The window was handled; giving up on it again is news.
        line.follow_guard(GuardReport::Notice("other".into()));
        assert_eq!(line.text(), "Running | other");
    }

    #[test]
    fn start_and_stop_keep_the_guard_and_display_watcher_notices() {
        let mut line = StatusLine {
            mirror: Status::Running,
            audio: Some("audio".into()),
            guard: Some("guard".into()),
            displays: Some("displays".into()),
            other: Some("other".into()),
            ..StatusLine::new(Status::Running)
        };
        assert_eq!(line.text(), "Running | audio | guard | displays | other");
        line.set_mirror(Status::Stopped);
        assert_eq!(line.text(), "Stopped | guard | displays");
    }

    #[test]
    fn button_shows_stop_exactly_while_the_mirror_runs_or_will_start_by_itself() {
        let shows_stop = |mirror| StatusLine::new(mirror).shows_stop();
        assert!(shows_stop(Status::Starting));
        assert!(shows_stop(Status::Running));
        assert!(shows_stop(Status::WaitingForCardDisplay));
        assert!(shows_stop(Status::WaitingForSourceMonitor));
        assert!(shows_stop(Status::Restarting("lost".into())));
        assert!(!shows_stop(Status::Stopped));
        assert!(!shows_stop(Status::Error("failed".into())));
    }

    #[test]
    fn a_failed_restart_keeps_restarting() {
        assert_eq!(
            restarted(Status::Error(
                "Could not capture the source monitor.".into()
            )),
            Status::Restarting("Could not capture the source monitor.".into())
        );
        assert_eq!(restarted(Status::Running), Status::Running);
    }

    #[test]
    fn restarts_wait_one_second_doubling_to_thirty_after_failed_starts() {
        let waits: Vec<u64> = (0..9)
            .map(|attempts| restart_delay(attempts).as_secs())
            .collect();
        assert_eq!(waits, [0, 1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(restart_delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn the_card_display_cannot_share_a_monitor_with_another_role_or_be_the_primary() {
        let problem = |settings: &Settings| role_problem(settings, &sender_monitors());
        assert_eq!(
            problem(&chosen("card", "card")),
            Some("The source monitor and the card display must be different monitors.")
        );
        let send_to_card = Settings {
            send_windows_to: Some("card".into()),
            ..chosen("source", "card")
        };
        assert_eq!(
            problem(&send_to_card),
            Some("\"Send windows to\" can't be the card display.")
        );
        assert_eq!(
            problem(&chosen("source", "primary")),
            Some("The card display can't be the primary monitor.")
        );
        // Sent to by default, the primary monitor still reads as the primary monitor.
        let primary_by_default = Settings::default().with_defaults(&sender_monitors());
        assert_eq!(
            problem(&Settings {
                card_display: Some("primary".into()),
                ..primary_by_default
            }),
            Some("The card display can't be the primary monitor.")
        );
        let fine = Settings {
            send_windows_to: Some("primary".into()),
            ..chosen("source", "card")
        };
        assert_eq!(problem(&fine), None);
        // Sending windows to the source monitor is fine, and nothing chosen is not a clash.
        let send_to_source = Settings {
            send_windows_to: Some("source".into()),
            ..chosen("source", "card")
        };
        assert_eq!(problem(&send_to_source), None);
        assert_eq!(problem(&Settings::default()), None);
        // A card display that is not connected is not known to be the primary monitor.
        assert_eq!(problem(&chosen("source", "gone")), None);
    }

    #[test]
    fn start_refuses_every_role_problem_with_its_text() {
        let send_to_card = Settings {
            send_windows_to: Some("card".into()),
            ..chosen("source", "card")
        };
        for settings in [
            chosen("card", "card"),
            chosen("source", "primary"),
            send_to_card,
        ] {
            let problem = role_problem(&settings, &sender_monitors()).unwrap();
            assert_eq!(
                start(&settings, &sender_monitors(), &mut StandInMirror::default()),
                Status::Error(problem.into())
            );
        }
    }

    /// Settings an older cardcast saved, as launch loads them.
    fn launched(saved: &Settings) -> (Settings, Option<String>) {
        let mut settings = Settings::from_text(&saved.to_text());
        let cleared = settings.clear_role_clashes();
        (settings.with_defaults(&sender_monitors()), cleared)
    }

    #[test]
    fn saved_send_windows_to_on_the_card_display_is_reset_to_the_primary_monitor() {
        let (settings, cleared) = launched(&Settings {
            send_windows_to: Some("card".into()),
            ..chosen("source", "card")
        });
        assert_eq!(
            settings,
            Settings {
                send_windows_to: Some("primary".into()),
                ..chosen("source", "card")
            }
        );
        assert_eq!(
            cleared.as_deref(),
            Some("\"Send windows to\" was the card display, so it is the primary monitor again.")
        );
    }

    #[test]
    fn saved_source_monitor_on_the_card_display_is_cleared_and_the_card_display_kept() {
        let (settings, cleared) = launched(&chosen("card", "card"));
        assert_eq!(settings.source_monitor, None);
        assert_eq!(settings.card_display.as_deref(), Some("card"));
        assert_eq!(
            cleared.as_deref(),
            Some(
                "The source monitor was the card display, so it was cleared. Choose a source monitor."
            )
        );
        assert!(!starts_on_launch(&settings));
        // Both at once: both are cleared, and both are said.
        let (settings, cleared) = launched(&Settings {
            send_windows_to: Some("card".into()),
            ..chosen("card", "card")
        });
        assert_eq!(settings.source_monitor, None);
        assert_eq!(settings.send_windows_to.as_deref(), Some("primary"));
        assert_eq!(cleared.unwrap().matches(" was the card display").count(), 2);
    }

    #[test]
    fn a_saved_card_display_that_is_the_primary_monitor_is_kept_and_start_refuses_it() {
        // Primary is Windows' live state: with the other monitors asleep, the card display can be it for a while.
        let saved = chosen("source", "primary");
        let (settings, cleared) = launched(&saved);
        assert_eq!(cleared, None);
        assert_eq!(settings.card_display.as_deref(), Some("primary"));
        assert_eq!(
            start(&settings, &sender_monitors(), &mut StandInMirror::default()),
            Status::Error("The card display can't be the primary monitor.".into())
        );
    }

    #[test]
    fn settings_without_a_clash_load_unchanged() {
        let saved = Settings {
            send_windows_to: Some("source".into()),
            ..chosen("source", "card")
        };
        assert_eq!(launched(&saved), (saved, None));
    }

    #[test]
    fn details_hold_only_what_the_header_does_not_say() {
        assert_eq!(StatusLine::new(Status::Stopped).details(), None);
        assert_eq!(StatusLine::new(Status::Running).details(), None);
        assert_eq!(
            StatusLine::new(Status::WaitingForCardDisplay).details(),
            Some(
                "The saved card display is not connected. Plug it in, or choose another card display."
                    .into()
            )
        );
        assert_eq!(
            StatusLine::new(Status::Error("driver too old".into())).details(),
            Some("driver too old".into())
        );
        assert_eq!(
            StatusLine::new(Status::Restarting("lost".into())).details(),
            Some("lost".into())
        );
        let line = StatusLine {
            audio: Some("audio".into()),
            guard: Some("guard".into()),
            ..StatusLine::new(Status::Error("failed".into()))
        };
        assert_eq!(line.details(), Some("failed | audio | guard".into()));
    }

    #[test]
    fn a_slow_mirror_is_said_while_it_runs_and_cleared_by_start_and_stop() {
        let mut line = StatusLine {
            slow_mirror: Some("slow".into()),
            guard: Some("guard".into()),
            ..StatusLine::new(Status::Running)
        };
        assert_eq!(line.details(), Some("slow | guard".into()));
        line.set_mirror(Status::Stopped);
        assert_eq!(line.details(), Some("guard".into()));
    }

    #[test]
    fn stopped_status_text() {
        assert_eq!(Status::Stopped.text(), "Stopped");
    }

    /// The source monitor and card display as the monitor list holds them.
    fn both<'a>(source: &'a Monitor, card: &'a Monitor) -> [Option<&'a Monitor>; 2] {
        [Some(source), Some(card)]
    }

    #[test]
    fn mirror_stops_and_waits_when_the_card_display_is_unplugged() {
        let (source, card) = (monitor("source", false), monitor("card", false));
        assert_eq!(
            follow_displays(true, true, both(&source, &card), [Some(&source), None]),
            Follow::Stop(Status::WaitingForCardDisplay)
        );
        // Both gone, as when their graphics card goes: the card display is named first.
        assert_eq!(
            follow_displays(true, true, both(&source, &card), [None, None]),
            Follow::Stop(Status::WaitingForCardDisplay)
        );
    }

    #[test]
    fn mirror_stops_and_waits_when_the_source_monitor_is_unplugged() {
        let (source, card) = (monitor("source", false), monitor("card", false));
        assert_eq!(
            follow_displays(true, true, both(&source, &card), [None, Some(&card)]),
            Follow::Stop(Status::WaitingForSourceMonitor)
        );
        // Already stopped (restarting, or waiting for the card display, which came back).
        assert_eq!(
            follow_displays(true, false, [None, None], [None, Some(&card)]),
            Follow::Stop(Status::WaitingForSourceMonitor)
        );
    }

    #[test]
    fn mirror_starts_when_the_missing_monitor_comes_back() {
        let (source, card) = (monitor("source", false), monitor("card", false));
        assert_eq!(
            follow_displays(true, false, [Some(&source), None], both(&source, &card)),
            Follow::Start
        );
        assert_eq!(
            follow_displays(true, false, [None, Some(&card)], both(&source, &card)),
            Follow::Start
        );
    }

    #[test]
    fn mirror_restarts_when_either_monitor_changes_or_the_mirror_ended() {
        let (source, card) = (monitor("source", false), monitor("card", false));
        let moved = |monitor: &Monitor| Monitor {
            left: 1920,
            ..monitor.clone()
        };
        assert_eq!(
            follow_displays(
                true,
                true,
                both(&source, &card),
                both(&source, &moved(&card))
            ),
            Follow::Start
        );
        assert_eq!(
            follow_displays(
                true,
                true,
                both(&source, &card),
                both(&moved(&source), &card)
            ),
            Follow::Start
        );
        assert_eq!(
            follow_displays(true, false, both(&source, &card), both(&source, &card)),
            Follow::Start
        );
    }

    #[test]
    fn running_mirror_is_left_alone_when_another_monitor_changes() {
        let (source, card) = (monitor("source", false), monitor("card", false));
        assert_eq!(
            follow_displays(true, true, both(&source, &card), both(&source, &card)),
            Follow::Nothing
        );
    }

    #[test]
    fn stopped_mirror_ignores_the_monitors() {
        let (source, card) = (monitor("source", false), monitor("card", false));
        assert_eq!(
            follow_displays(false, false, [None, None], both(&source, &card)),
            Follow::Nothing
        );
        assert_eq!(
            follow_displays(false, false, both(&source, &card), [None, None]),
            Follow::Nothing
        );
    }

    #[test]
    fn a_changed_monitor_or_audio_choice_restarts_a_wanted_mirror() {
        let before = Settings {
            audio_source: Some("speakers".into()),
            ..chosen("source", "card")
        };
        let changed = [
            chosen("other", "card"),
            chosen("source", "other"),
            Settings {
                audio_source: Some("headset".into()),
                ..before.clone()
            },
        ];
        for after in &changed {
            assert!(restarts_on_change(true, &before, after), "{after:?}");
            assert!(!restarts_on_change(false, &before, after), "{after:?}");
        }
        // The guard's own choices don't touch the mirror.
        let guard_only = Settings {
            send_windows_to: Some("primary".into()),
            guard_enabled: false,
            ..before.clone()
        };
        assert!(!restarts_on_change(true, &before, &guard_only));
    }

    #[test]
    fn saved_monitors_start_the_mirror_at_launch() {
        assert!(starts_on_launch(&chosen("source", "card")));
        assert!(!starts_on_launch(&Settings::default()));
        let half = Settings {
            card_display: Some("card".into()),
            ..Settings::default()
        };
        assert!(!starts_on_launch(&half));
    }
}

/// Runs in the CI `second-monitor` job: the virtual display driver there can load a custom EDID.
#[cfg(test)]
#[cfg(windows)]
mod second_monitor_tests {
    use super::{Settings, follow_card_display};
    use crate::guard::{self, second_monitor};

    const FOLDER: &str = r"C:\VirtualDisplayDriver";

    /// Both virtual monitors with a new EDID named "cardcast EDID", as when a capture card switches
    /// to Display EDID mode; the driver loads it when its graphics card restarts. Restored on drop.
    struct NewEdid(String);

    impl NewEdid {
        fn new() -> Self {
            let settings = format!(r"{FOLDER}\vdd_settings.xml");
            let before = std::fs::read_to_string(&settings).unwrap();
            assert!(
                before.contains("<CustomEdid>false</CustomEdid>"),
                "{before}"
            );
            std::fs::write(format!(r"{FOLDER}\user_edid.bin"), edid()).unwrap();
            std::fs::write(
                &settings,
                before.replace(
                    "<CustomEdid>false</CustomEdid>",
                    "<CustomEdid>true</CustomEdid>",
                ),
            )
            .unwrap();
            drop(second_monitor::Unplugged::new());
            Self(before)
        }
    }

    impl Drop for NewEdid {
        fn drop(&mut self) {
            let _ = std::fs::write(format!(r"{FOLDER}\vdd_settings.xml"), &self.0);
            let _ = std::fs::remove_file(format!(r"{FOLDER}\user_edid.bin"));
            drop(second_monitor::Unplugged::new());
        }
    }

    /// A 1080p EDID 1.4 from manufacturer "CCT", product 0x1234, named "cardcast EDID".
    fn edid() -> Vec<u8> {
        let mut edid = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        edid.extend([
            0x0C, 0x74, 0x34, 0x12, 0x01, 0x00, 0x00, 0x00, 0x01, 0x22, 0x01, 0x04,
        ]);
        edid.extend([0x80, 0x35, 0x1E, 0x78, 0x0A]);
        edid.extend([0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54]);
        edid.extend([0x00; 3]);
        edid.extend([0x01; 16]);
        edid.extend([
            0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0x0F, 0x28,
            0x21, 0x00, 0x00, 0x1E,
        ]);
        edid.extend([0x00, 0x00, 0x00, 0xFC, 0x00]);
        edid.extend(b"cardcast EDID");
        edid.extend([
            0x00, 0x00, 0x00, 0xFD, 0x00, 0x32, 0x4B, 0x1E, 0x53, 0x11, 0x00, 0x0A, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x20,
        ]);
        edid.extend([0x00, 0x00, 0x00, 0x10, 0x00]);
        edid.extend([0x00; 13]);
        edid.push(0x00);
        assert_eq!(edid.len(), 127);
        let sum = edid.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        edid.push(0u8.wrapping_sub(sum));
        edid
    }

    #[test]
    fn second_monitor_card_display_with_a_new_edid_is_found_on_its_connector() {
        let Some(monitors) = second_monitor::monitors(3) else {
            return;
        };
        let card = monitors[2].clone();
        println!("before: {card:#?}");
        assert!(!card.container_id.is_empty(), "no container ID: {card:?}");
        // This driver keeps the device path when the EDID changes, so the saved path stands in for the old one.
        let mut settings = Settings {
            card_display: Some(
                r"\\?\DISPLAY#OLD0000#1&1&0&UID1#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}".into(),
            ),
            card_display_connector: Some(card.connector.clone()),
            ..Settings::default()
        };
        // Restarts the graphics card, which gets a new LUID.
        let _edid = NewEdid::new();
        let now = guard::monitors().unwrap();
        println!("after: {now:#?}");
        assert!(follow_card_display(&mut settings, &now));
        assert_eq!(settings.card_display, Some(card.device_path.clone()));
        let found = now
            .iter()
            .find(|monitor| monitor.device_path == card.device_path)
            .unwrap();
        assert_eq!(found.name, "cardcast EDID");
        assert_eq!(found.connector, card.connector);
        assert_ne!(
            found.container_id, card.container_id,
            "the container ID follows the EDID"
        );
    }
}
