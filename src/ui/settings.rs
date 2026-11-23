//! Settings model: saved choices, start-up refusal rules and the status line. No Windows calls.

use crate::guard::Monitor;
use crate::mirror::Mirror;

/// The one-line status under the controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Running,
    WaitingForCardDisplay,
    /// The last error, in plain words.
    Error(String),
}

impl Status {
    pub fn text(&self) -> String {
        match self {
            Self::Stopped => "Stopped".into(),
            Self::Running => "Running".into(),
            Self::WaitingForCardDisplay => "Waiting for card display".into(),
            Self::Error(message) => message.clone(),
        }
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
    fn stopped_status_text() {
        assert_eq!(Status::Stopped.text(), "Stopped");
    }
}
