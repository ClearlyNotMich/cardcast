//! Settings model: saved choices, start-up refusal rules and the status line. No Windows calls.

use crate::guard::Monitor;
use crate::mirror::Mirror;

/// What the mirror is doing; it alone decides the Start/Stop button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Running,
    WaitingForCardDisplay,
    /// The mirror ended on its own and is being started again; why, in plain words.
    Restarting(String),
    /// The mirror is stopped by this error, in plain words.
    Error(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Self::Stopped => "Stopped".into(),
            Self::Running => "Running".into(),
            Self::WaitingForCardDisplay => "Waiting for card display".into(),
            Self::Restarting(message) | Self::Error(message) => message.clone(),
        }
    }
}

/// The one-line status under the controls: the mirror, then any audio or other problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub mirror: Status,
    /// The audio relay's problem; cleared when it relays again, or on Start and Stop.
    pub audio: Option<String>,
    /// A guard, settings or monitor-list problem; cleared on Start and Stop.
    pub other: Option<String>,
}

impl StatusLine {
    pub fn new(mirror: Status) -> Self {
        Self {
            mirror,
            audio: None,
            other: None,
        }
    }

    pub fn text(&self) -> String {
        let mut text = self.mirror.text();
        for problem in [&self.audio, &self.other].into_iter().flatten() {
            text.push_str(" | ");
            text.push_str(problem);
        }
        text
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

/// Applies the refusal rules, then starts the mirror.
pub fn start(settings: &Settings, monitors: &[Monitor], mirror: &mut dyn Mirror) -> Status {
    let (Some(source_path), Some(card_path)) = (&settings.source_monitor, &settings.card_display)
    else {
        return Status::Error("Choose a source monitor and a card display.".into());
    };
    if source_path == card_path {
        return Status::Error(
            "The source monitor and the card display must be different monitors.".into(),
        );
    }
    let find = |path: &str| monitors.iter().find(|monitor| monitor.device_path == path);
    let Some(card_display) = find(card_path) else {
        return Status::WaitingForCardDisplay;
    };
    if card_display.primary {
        return Status::Error("The card display can't be the primary monitor.".into());
    }
    let Some(source_monitor) = find(source_path) else {
        return Status::Error("The source monitor is not connected.".into());
    };
    match mirror.start(source_monitor, card_display) {
        Ok(()) => Status::Running,
        Err(message) => Status::Error(message),
    }
}

/// What the settings window does to the mirror after a display change.
#[derive(Debug, PartialEq, Eq)]
pub enum Follow {
    Nothing,
    /// (Re)start it: the card display came back or changed, or the mirror ended on its own.
    Start,
    /// Stop it and wait: the card display is gone.
    Stop,
}

/// `wanted` is true from Start (pressed or at launch) until Stop is pressed; `before` and `now`
/// are the card display in the monitor list before and after the change.
pub fn follow_card_display(
    wanted: bool,
    mirror_running: bool,
    before: Option<&Monitor>,
    now: Option<&Monitor>,
) -> Follow {
    match now {
        _ if !wanted => Follow::Nothing,
        None if mirror_running => Follow::Stop,
        None => Follow::Nothing,
        Some(card) if !mirror_running || before != Some(card) => Follow::Start,
        Some(_) => Follow::Nothing,
    }
}

/// Saved settings start the mirror at launch once both monitors were chosen.
pub fn starts_on_launch(settings: &Settings) -> bool {
    settings.source_monitor.is_some() && settings.card_display.is_some()
}

/// The user's choices. Monitors are stored by their display-configuration device path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub source_monitor: Option<String>,
    pub card_display: Option<String>,
    pub send_windows_to: Option<String>,
    pub audio_source: Option<String>,
    pub guard_enabled: bool,
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
        ] {
            if let Some(value) = value {
                text.push_str(&format!("{key}={value}\n"));
            }
        }
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
                "guard_enabled" => settings.guard_enabled = value.as_deref() == Some("true"),
                _ => {}
            }
        }
        settings
    }
}

/// %APPDATA%\\cardcast on Windows, the XDG config folder elsewhere.
fn settings_path() -> Option<std::path::PathBuf> {
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
    use crate::mirror::StandInMirror;

    #[test]
    fn settings_survive_save_and_load() {
        let saved = Settings {
            source_monitor: Some(r"\\?\DISPLAY#GSM1#5&1&UID1".into()),
            card_display: Some(r"\\?\DISPLAY#ELG1#5&2&UID2".into()),
            send_windows_to: Some(r"\\?\DISPLAY#DEL1#5&3&UID3".into()),
            audio_source: Some("{0.0.0.00000000}.{abc}".into()),
            guard_enabled: true,
        };
        assert_eq!(Settings::from_text(&saved.to_text()), saved);
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

    fn sender_monitors() -> Vec<Monitor> {
        vec![
            monitor("source", false),
            monitor("primary", true),
            monitor("card", false),
        ]
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

    struct FailingMirror;

    impl Mirror for FailingMirror {
        fn start(&mut self, _source: &Monitor, _card_display: &Monitor) -> Result<(), String> {
            Err("Could not capture the source monitor.".into())
        }
        fn stop(&mut self) {}
        fn is_running(&self) -> bool {
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
            "Running"
        );
        assert!(mirror.running);
    }

    #[test]
    fn mirror_error_reaches_the_status_line() {
        assert_eq!(
            start_text(&chosen("source", "card"), &mut FailingMirror),
            "Could not capture the source monitor."
        );
    }

    #[test]
    fn audio_and_other_problems_follow_the_mirror_status_without_changing_the_button() {
        let line = StatusLine {
            mirror: Status::Running,
            audio: Some("The audio source is not connected.".into()),
            other: Some("Could not save settings: denied".into()),
        };
        assert_eq!(
            line.text(),
            "Running | The audio source is not connected. | Could not save settings: denied"
        );
        assert!(line.shows_stop());
    }

    #[test]
    fn button_shows_stop_exactly_while_the_mirror_runs_or_will_start_by_itself() {
        let shows_stop = |mirror| StatusLine::new(mirror).shows_stop();
        assert!(shows_stop(Status::Running));
        assert!(shows_stop(Status::WaitingForCardDisplay));
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
    fn stopped_status_text() {
        assert_eq!(Status::Stopped.text(), "Stopped");
    }

    #[test]
    fn mirror_stops_when_the_card_display_is_unplugged() {
        let card = monitor("card", false);
        assert_eq!(
            follow_card_display(true, true, Some(&card), None),
            Follow::Stop
        );
    }

    #[test]
    fn mirror_starts_when_the_card_display_comes_back() {
        let card = monitor("card", false);
        assert_eq!(
            follow_card_display(true, false, None, Some(&card)),
            Follow::Start
        );
    }

    #[test]
    fn mirror_restarts_when_the_card_display_changes_or_the_mirror_ended() {
        let card = monitor("card", false);
        let moved = Monitor {
            left: 1920,
            ..card.clone()
        };
        assert_eq!(
            follow_card_display(true, true, Some(&card), Some(&moved)),
            Follow::Start
        );
        assert_eq!(
            follow_card_display(true, false, Some(&card), Some(&card)),
            Follow::Start
        );
    }

    #[test]
    fn running_mirror_is_left_alone_when_another_monitor_changes() {
        let card = monitor("card", false);
        assert_eq!(
            follow_card_display(true, true, Some(&card), Some(&card)),
            Follow::Nothing
        );
        assert_eq!(
            follow_card_display(true, false, None, None),
            Follow::Nothing
        );
    }

    #[test]
    fn stopped_mirror_ignores_the_card_display() {
        let card = monitor("card", false);
        assert_eq!(
            follow_card_display(false, false, None, Some(&card)),
            Follow::Nothing
        );
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
