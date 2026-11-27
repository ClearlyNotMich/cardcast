//! Settings model: saved choices, start-up refusal rules and the status line. No Windows calls.

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
    /// The audio relay's problem; cleared when it relays again, or on Start and Stop.
    pub audio: Option<String>,
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
            audio: None,
            guard: None,
            dismissed_guard: None,
            displays: None,
            other: None,
        }
    }

    /// Start and Stop: the new mirror status clears audio and other problems, not the lasting ones.
    pub fn set_mirror(&mut self, mirror: Status) {
        self.mirror = mirror;
        self.audio = None;
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
            Status::Restarting(message) | Status::Error(message) => Some(message),
            _ => None,
        };
        let parts: Vec<&str> = [
            reason,
            self.audio.as_ref(),
            self.guard.as_ref(),
            self.displays.as_ref(),
            self.other.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(String::as_str)
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

/// Choices that can't work together, in plain words. The dropdowns and the live layout allow
/// any choice, so a swap can go through a clash; the window warns about it instead.
pub fn role_problem(settings: &Settings) -> Option<&'static str> {
    let card_display = settings.card_display.as_ref()?;
    if settings.source_monitor.as_ref() == Some(card_display) {
        Some("The source monitor and the card display must be different monitors.")
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

/// The user's choices. Monitors are stored by their display-configuration device path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub source_monitor: Option<String>,
    pub card_display: Option<String>,
    pub send_windows_to: Option<String>,
    pub audio_source: Option<String>,
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
    use crate::mirror::{Ended, StandInMirror};

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
    fn the_card_display_cannot_share_a_monitor_with_another_role() {
        assert_eq!(
            role_problem(&chosen("card", "card")),
            Some("The source monitor and the card display must be different monitors.")
        );
        let send_to_card = Settings {
            send_windows_to: Some("card".into()),
            ..chosen("source", "card")
        };
        assert_eq!(
            role_problem(&send_to_card),
            Some("\"Send windows to\" can't be the card display.")
        );
        let fine = Settings {
            send_windows_to: Some("primary".into()),
            ..chosen("source", "card")
        };
        assert_eq!(role_problem(&fine), None);
        // Sending windows to the source monitor is fine, and nothing chosen is not a clash.
        let send_to_source = Settings {
            send_windows_to: Some("source".into()),
            ..chosen("source", "card")
        };
        assert_eq!(role_problem(&send_to_source), None);
        assert_eq!(role_problem(&Settings::default()), None);
    }

    #[test]
    fn details_hold_only_what_the_header_does_not_say() {
        assert_eq!(StatusLine::new(Status::Running).details(), None);
        assert_eq!(
            StatusLine::new(Status::WaitingForCardDisplay).details(),
            None
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
