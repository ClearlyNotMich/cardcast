//! The Linux window guard behind [`Guard`]: EWMH on X11 (`ewmh.rs`) and a KWin script on Plasma
//! (`kwin.rs`), both deciding with `placement.rs` (docs/research/linux-support.md, section 5). What
//! they share lives here: the monitors, the return history and the status-line reports.

use super::super::desktop::Desktop;
use super::super::placement::{self, Placement, Rect, ReturnHistory, WindowState};
use super::super::{Guard, GuardReport, Monitor};
use super::{ewmh, kwin};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Lists the monitors; [`super::monitors`] outside tests.
pub type ListMonitors = fn() -> Result<Vec<Monitor>, String>;

/// No pointer guard on Linux yet (#36), so the status line says so whenever the window guard has
/// nothing else to say; it never pretends.
pub const NO_POINTER_GUARD: &str = "The pointer guard does not work on Linux yet.";

/// The report the status line gets on Linux: `report`, or the missing pointer guard instead of nothing.
pub fn honest(report: GuardReport) -> GuardReport {
    match report {
        GuardReport::Clear => GuardReport::Notice(NO_POINTER_GUARD.into()),
        notice @ GuardReport::Notice(_) => notice,
    }
}

/// The monitor's rectangle in the desktop's coordinates.
pub fn monitor_rect(monitor: &Monitor) -> Rect {
    let size = |length: u32| i32::try_from(length).unwrap_or(i32::MAX);
    Rect {
        left: monitor.left,
        top: monitor.top,
        right: monitor.left.saturating_add(size(monitor.layout_size.0)),
        bottom: monitor.top.saturating_add(size(monitor.layout_size.1)),
    }
}

/// What a guard helper knows on its own thread: the monitors, the return history, and each window's title for the give-up notice.
pub struct Guarding {
    card_display: String,
    send_windows_to: Option<String>,
    list_monitors: ListMonitors,
    /// Card display rectangle and the "send windows to" work area; `None` while there is nothing to guard.
    monitors: Option<(Rect, Rect)>,
    /// Why there is nothing to guard, unless the card display is just unplugged.
    problem: Option<String>,
    history: ReturnHistory,
    titles: HashMap<isize, String>,
    reports: Sender<GuardReport>,
    wake: Wake,
}

impl Guarding {
    pub fn new(
        card_display: &str,
        send_windows_to: Option<&str>,
        list_monitors: ListMonitors,
        reports: Sender<GuardReport>,
        wake: Wake,
    ) -> Self {
        Self {
            card_display: card_display.to_owned(),
            send_windows_to: send_windows_to.map(str::to_owned),
            list_monitors,
            monitors: None,
            problem: None,
            history: ReturnHistory::default(),
            titles: HashMap::new(),
            reports,
            wake,
        }
    }

    /// At start and after a display change: finds the monitors and reports. `work_area` gives a
    /// monitor's work area from the desktop, else the guard uses the whole monitor.
    pub fn follow_displays(&mut self, work_area: impl Fn(&Monitor) -> Option<Rect>) {
        (self.monitors, self.problem) = match self.find_monitors(work_area) {
            Ok(monitors) => (monitors, None),
            Err(problem) => (None, Some(problem)),
        };
        self.report();
    }

    fn find_monitors(
        &self,
        work_area: impl Fn(&Monitor) -> Option<Rect>,
    ) -> Result<Option<(Rect, Rect)>, String> {
        let monitors = (self.list_monitors)()?;
        let Some((card_display, send_windows_to)) = placement::guarded_monitors(
            &monitors,
            &self.card_display,
            self.send_windows_to.as_deref(),
        )?
        else {
            return Ok(None);
        };
        Ok(Some((
            monitor_rect(card_display),
            work_area(send_windows_to).unwrap_or_else(|| monitor_rect(send_windows_to)),
        )))
    }

    /// Where window `id` goes. Neither desktop tells where a maximised window restores to, so a
    /// helper first unmaximises a maximised window on the card display (a [`Placement::Maximise`]
    /// with `remaximising` false), then checks it again with `remaximising` true and its restored
    /// frame: [`Placement::Maximise`] moves it there and maximises it, [`Placement::Stay`]
    /// maximises it where it is. Only the first check counts as a return.
    pub fn check(
        &mut self,
        id: isize,
        title: &str,
        window: WindowState,
        remaximising: bool,
        now: Instant,
    ) -> Placement {
        let Some((card_display, send_windows_to)) = self.monitors else {
            return Placement::Stay;
        };
        self.titles.insert(id, title.to_owned());
        if remaximising {
            let restored = WindowState {
                maximised: true,
                restore: window.frame,
                ..window
            };
            return placement::place(&restored, card_display, send_windows_to);
        }
        let placement = self
            .history
            .place(id, &window, card_display, send_windows_to, now);
        if matches!(placement, Placement::GiveUp | Placement::Handled) {
            self.report();
        }
        placement
    }

    /// Window `id` closed.
    pub fn forget(&mut self, id: isize) {
        self.titles.remove(&id);
        if self.history.forget(id) {
            self.report();
        }
    }

    fn report(&self) {
        let title = self
            .history
            .latest_given_up()
            .and_then(|id| self.titles.get(&id));
        let _ = self.reports.send(honest(placement::notice(
            self.problem.as_deref(),
            title.map(String::as_str),
        )));
        (self.wake)();
    }
}

/// A started helper; dropping it stops it.
enum Helper {
    X11(ewmh::Running),
    Kwin(kwin::Running),
}

struct Running {
    card_display: String,
    send_windows_to: Option<String>,
    helper: Helper,
}

/// The window guard on X11 or Plasma; on Linux the pointer guard is not built yet.
pub struct WindowGuard {
    desktop: Desktop,
    list_monitors: ListMonitors,
    running: Option<Running>,
    report_sender: Sender<GuardReport>,
    reports: Receiver<GuardReport>,
    wake: Wake,
}

impl WindowGuard {
    /// `desktop` is [`Desktop::X11`] or [`Desktop::PlasmaWayland`], the desktops with a helper.
    pub fn new(
        desktop: Desktop,
        list_monitors: ListMonitors,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let (report_sender, reports) = mpsc::channel();
        let wake = std::sync::Mutex::new(wake);
        Self {
            desktop,
            list_monitors,
            running: None,
            report_sender,
            reports,
            wake: Arc::new(move || {
                if let Ok(wake) = wake.lock() {
                    wake();
                }
            }),
        }
    }

    fn report(&self, report: GuardReport) {
        let _ = self.report_sender.send(report);
        (self.wake)();
    }
}

impl Guard for WindowGuard {
    fn update(&mut self, enabled: bool, card_display: Option<&str>, send_windows_to: Option<&str>) {
        let wanted = card_display.filter(|_| enabled);
        if let Some(running) = &self.running
            && Some(running.card_display.as_str()) == wanted
            && running.send_windows_to.as_deref() == send_windows_to
        {
            return;
        }
        // Dropping the helper stops it before another starts.
        self.running = None;
        let Some(card_display) = wanted else {
            // The guard is off, so its notices are out of date.
            self.report(GuardReport::Clear);
            return;
        };
        let guarding = Guarding::new(
            card_display,
            send_windows_to,
            self.list_monitors,
            self.report_sender.clone(),
            self.wake.clone(),
        );
        let helper = match self.desktop {
            Desktop::X11 => ewmh::start(guarding).map(Helper::X11),
            _ => kwin::start(guarding).map(Helper::Kwin),
        };
        match helper {
            Ok(helper) => {
                self.running = Some(Running {
                    card_display: card_display.to_owned(),
                    send_windows_to: send_windows_to.map(str::to_owned),
                    helper,
                });
            }
            Err(error) => self.report(GuardReport::Notice(format!(
                "The window guard could not start ({error}). Untick and tick Guard to try again."
            ))),
        }
    }

    fn take_report(&mut self) -> Option<GuardReport> {
        self.reports.try_iter().last()
    }

    fn follow_displays(&mut self) {
        match self.running.as_ref().map(|running| &running.helper) {
            Some(Helper::X11(helper)) => helper.follow_displays(),
            Some(Helper::Kwin(helper)) => helper.follow_displays(),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn monitor(device_path: &str, left: i32, primary: bool) -> Monitor {
        Monitor {
            device_path: device_path.into(),
            gdi_name: device_path.into(),
            name: device_path.into(),
            width: 1920,
            height: 1080,
            layout_size: (1920, 1080),
            refresh_hz: 60,
            primary,
            left,
            top: 0,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        }
    }

    /// The primary monitor at the origin and the card display right of it.
    fn two_monitors() -> Result<Vec<Monitor>, String> {
        Ok(vec![
            monitor("primary", 0, true),
            monitor("card", 1920, false),
        ])
    }

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn guarding(send_windows_to: Option<&str>) -> (Guarding, Receiver<GuardReport>) {
        let (sender, reports) = mpsc::channel();
        let guarding = Guarding::new(
            "card",
            send_windows_to,
            two_monitors,
            sender,
            Arc::new(|| {}),
        );
        (guarding, reports)
    }

    fn on_card() -> WindowState {
        WindowState {
            frame: rect(2020, 100, 2420, 400),
            ..WindowState::default()
        }
    }

    const PANEL: i32 = 44;

    /// The primary monitor's work area: the monitor less a panel at its bottom.
    fn work_area(monitor: &Monitor) -> Option<Rect> {
        (monitor.device_path == "primary").then(|| rect(0, 0, 1920, 1080 - PANEL))
    }

    #[test]
    fn with_nothing_else_to_say_the_status_line_says_the_pointer_guard_is_missing() {
        assert_eq!(
            honest(GuardReport::Clear),
            GuardReport::Notice("The pointer guard does not work on Linux yet.".into())
        );
        let problem = GuardReport::Notice("problem".into());
        assert_eq!(honest(problem.clone()), problem);
    }

    #[test]
    fn a_monitors_rectangle_is_its_size_on_the_desktop() {
        let scaled = Monitor {
            layout_size: (1477, 831),
            top: -831,
            ..monitor("scaled", 1920, false)
        };
        assert_eq!(monitor_rect(&scaled), rect(1920, -831, 3397, 0));
    }

    #[test]
    fn windows_go_to_the_send_windows_to_work_area_and_nothing_moves_before_the_monitors_are_found()
    {
        let (mut guarding, reports) = guarding(None);
        let now = Instant::now();
        assert_eq!(
            guarding.check(1, "Kate", on_card(), false, now),
            Placement::Stay
        );
        guarding.follow_displays(work_area);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(NO_POINTER_GUARD.into()))
        );
        assert_eq!(
            guarding.check(1, "Kate", on_card(), false, now),
            Placement::MoveTo(rect(100, 100, 500, 400))
        );
        let tall = WindowState {
            frame: rect(2020, 0, 2420, 1080),
            ..WindowState::default()
        };
        assert_eq!(
            guarding.check(2, "Dolphin", tall, false, now),
            Placement::MoveTo(rect(100, 0, 500, 1080 - PANEL))
        );
    }

    #[test]
    fn a_monitor_problem_is_reported_and_nothing_moves() {
        let (mut guarding, reports) = guarding(Some("card"));
        guarding.follow_displays(work_area);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(
                "The window guard is off: \"Send windows to\" is the card display. Choose another monitor.".into()
            ))
        );
        assert_eq!(
            guarding.check(1, "Kate", on_card(), false, Instant::now()),
            Placement::Stay
        );
    }

    #[test]
    fn a_maximised_window_is_unmaximised_then_maximised_where_its_restored_frame_goes() {
        let (mut guarding, _reports) = guarding(None);
        guarding.follow_displays(work_area);
        let now = Instant::now();
        let maximised = WindowState {
            frame: rect(1920, 0, 3840, 1080),
            maximised: true,
            ..WindowState::default()
        };
        assert!(matches!(
            guarding.check(1, "Kate", maximised, false, now),
            Placement::Maximise { .. }
        ));
        // Unmaximised, it is back at its restore rectangle on the card display.
        assert_eq!(
            guarding.check(1, "Kate", on_card(), true, now),
            Placement::Maximise {
                work_area: rect(0, 0, 1920, 1080 - PANEL),
                restore: rect(100, 100, 500, 400),
            }
        );
        // One that restores off the card display is maximised where it is.
        let off_card = WindowState {
            frame: rect(100, 100, 500, 400),
            ..WindowState::default()
        };
        assert_eq!(
            guarding.check(1, "Kate", off_card, true, now),
            Placement::Stay
        );
    }

    #[test]
    fn remaximising_does_not_count_as_a_return() {
        let (mut guarding, _reports) = guarding(None);
        guarding.follow_displays(work_area);
        let now = Instant::now();
        for second in 0..placement::RETURNS_PER_MINUTE {
            let at = now + Duration::from_secs(second as u64);
            assert!(matches!(
                guarding.check(1, "Kate", on_card(), false, at),
                Placement::MoveTo(_)
            ));
            assert!(matches!(
                guarding.check(1, "Kate", on_card(), true, at),
                Placement::Maximise { .. }
            ));
        }
    }

    #[test]
    fn a_given_up_window_is_named_until_it_closes() {
        let (mut guarding, reports) = guarding(None);
        guarding.follow_displays(work_area);
        let now = Instant::now();
        while guarding.check(7, "Kate", on_card(), false, now) != Placement::GiveUp {}
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(
                "The window guard left \"Kate\" on the card display because it keeps going back there. Move it off to guard it again.".into()
            ))
        );
        guarding.forget(7);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(NO_POINTER_GUARD.into()))
        );
    }
}
