//! The Linux window guard behind [`Guard`]: EWMH on X11 (`ewmh.rs`), a KWin script on Plasma
//! (`kwin.rs`), and sway's and Hyprland's IPC and cardcast's GNOME Shell extension
//! (`compositor.rs`), all deciding with `placement.rs` (docs/research/linux-support.md, section 5).
//! What they share lives here: the monitors, the return history, the pointer guard's decisions
//! (`pointer.rs`) and the status-line reports.

use super::super::desktop::Desktop;
use super::super::placement::{self, Placement, Rect, ReturnHistory, WindowState};
use super::super::pointer::{PointerHistory, nearest_off_card};
use super::super::{Guard, GuardReport, Monitor};
use super::gnome::Gnome;
use super::hyprland::Hyprland;
use super::sway::Sway;
use super::{compositor, ewmh, kwin};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Lists the monitors; [`super::monitors`] outside tests.
pub type ListMonitors = fn() -> Result<Vec<Monitor>, String>;

/// The status line when the pointer guard can't start.
pub fn pointer_could_not_start(error: &str) -> String {
    format!("The pointer guard could not start ({error}). Untick and tick Guard to try again.")
}

/// The status line when a helper can't start.
pub fn could_not_start(error: &str) -> String {
    format!("The window guard could not start ({error}). Untick and tick Guard to try again.")
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

/// The card display and "send windows to" rectangles, the latter's output name, and where the
/// pointer guard works.
type Found = ((Rect, Rect), String, PointerArea);

/// What the pointer guard on Wayland needs: the card display's output and rectangle, and the other
/// monitors' rectangles, all in the desktop's coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerArea {
    pub output: String,
    pub card_display: Rect,
    pub others: Vec<Rect>,
}

impl PointerArea {
    /// Where a pointer seen at `pointer` on the card display goes.
    pub fn target(&self, pointer: (i32, i32)) -> Option<(i32, i32)> {
        nearest_off_card(pointer, &self.others)
    }

    /// The desktop's top-left corner: the smallest left and top of all monitors.
    pub fn origin(&self) -> (i32, i32) {
        let all = || self.others.iter().chain([&self.card_display]);
        (
            all().map(|r| r.left).min().unwrap_or(0),
            all().map(|r| r.top).min().unwrap_or(0),
        )
    }
}

/// What a guard helper knows on its own thread: the monitors, the return history, and each window's title for the give-up notice.
pub struct Guarding {
    card_display: String,
    send_windows_to: Option<String>,
    list_monitors: ListMonitors,
    /// Card display rectangle and the "send windows to" work area; `None` while there is nothing to guard.
    monitors: Option<(Rect, Rect)>,
    /// The desktop's name for the "send windows to" monitor's output, while there is something to guard.
    send_to_output: Option<String>,
    /// Where the pointer guard works, while there is something to guard.
    pointer_area: Option<PointerArea>,
    /// The last pointer position seen off the card display (X11, which sees it everywhere).
    pointer: PointerHistory,
    /// Why the pointer guard can't work here, for the status line when nothing else is to say.
    pointer_problem: Option<String>,
    /// Why there is nothing to guard, unless the card display is just unplugged.
    problem: Option<String>,
    history: ReturnHistory,
    titles: HashMap<isize, String>,
    /// Windows given up on because they are fullscreen and can't be moved without the focus.
    fullscreen: HashSet<isize>,
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
            send_to_output: None,
            pointer_area: None,
            pointer: PointerHistory::default(),
            pointer_problem: None,
            problem: None,
            history: ReturnHistory::default(),
            titles: HashMap::new(),
            fullscreen: HashSet::new(),
            reports,
            wake,
        }
    }

    /// At start and after a display change: finds the monitors and reports. `work_area` gives a
    /// monitor's work area from the desktop, else the guard uses the whole monitor.
    pub fn follow_displays(&mut self, work_area: impl FnMut(&Monitor) -> Option<Rect>) {
        (
            self.monitors,
            self.send_to_output,
            self.pointer_area,
            self.problem,
        ) = match self.find_monitors(work_area) {
            Ok(Some((monitors, output, area))) => (Some(monitors), Some(output), Some(area), None),
            Ok(None) => (None, None, None, None),
            Err(problem) => (None, None, None, Some(problem)),
        };
        self.report();
    }

    /// Where the pointer guard works; `None` while there is nothing to guard.
    pub fn pointer_area(&self) -> Option<&PointerArea> {
        self.pointer_area.as_ref()
    }

    /// Where to put the pointer, now at `pointer`, or `None` to leave it: the Windows decision, for
    /// a desktop that shows the pointer everywhere (X11). Before any position off the card display
    /// is seen, the middle of the "send windows to" monitor's work area.
    pub fn place_pointer(&mut self, pointer: (i32, i32)) -> Option<(i32, i32)> {
        let (card_display, send_windows_to) = self.monitors?;
        let fallback = (
            send_windows_to.left + (send_windows_to.right - send_windows_to.left) / 2,
            send_windows_to.top + (send_windows_to.bottom - send_windows_to.top) / 2,
        );
        self.pointer.place(pointer, card_display, fallback)
    }

    /// Why the pointer guard can't work here (plain words), or `None` once it can.
    pub fn pointer_problem(&mut self, problem: Option<String>) {
        if self.pointer_problem != problem {
            self.pointer_problem = problem;
            self.report();
        }
    }

    /// The desktop's name for the "send windows to" monitor's output (`DP-2`).
    pub fn send_windows_to_output(&self) -> Option<&str> {
        self.send_to_output.as_deref()
    }

    /// The card display and "send windows to" rectangles, and the latter's output name.
    fn find_monitors(
        &self,
        mut work_area: impl FnMut(&Monitor) -> Option<Rect>,
    ) -> Result<Option<Found>, String> {
        let monitors = (self.list_monitors)()?;
        let Some((card_display, send_windows_to)) = placement::guarded_monitors(
            &monitors,
            &self.card_display,
            self.send_windows_to.as_deref(),
        )?
        else {
            return Ok(None);
        };
        let area = PointerArea {
            output: card_display.gdi_name.clone(),
            card_display: monitor_rect(card_display),
            others: monitors
                .iter()
                .filter(|monitor| monitor.device_path != card_display.device_path)
                .map(monitor_rect)
                .collect(),
        };
        Ok(Some((
            (
                monitor_rect(card_display),
                work_area(send_windows_to).unwrap_or_else(|| monitor_rect(send_windows_to)),
            ),
            send_windows_to.gdi_name.clone(),
            area,
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

    /// Gives up on window `id` at once and names it in the status line.
    pub fn give_up(&mut self, id: isize) {
        self.history.give_up(id);
        self.report();
    }

    /// Gives up on window `id`, fullscreen on the card display, which the desktop can't move
    /// without taking the focus (Hyprland), and says so in the status line.
    pub fn give_up_fullscreen(&mut self, id: isize) {
        self.fullscreen.insert(id);
        self.give_up(id);
    }

    /// Window `id` can be moved again (it left fullscreen): it is guarded again.
    pub fn movable_again(&mut self, id: isize) {
        if self.fullscreen.remove(&id) {
            self.history.forget(id);
            self.report();
        }
    }

    /// A helper's own notice for the status line, such as KWin being gone.
    pub fn notice(&self, notice: &str) {
        let _ = self.reports.send(GuardReport::Notice(notice.to_owned()));
        (self.wake)();
    }

    /// Window `id` closed.
    pub fn forget(&mut self, id: isize) {
        self.titles.remove(&id);
        self.fullscreen.remove(&id);
        if self.history.forget(id) {
            self.report();
        }
    }

    fn report(&self) {
        let latest = self.history.latest_given_up();
        let title = latest
            .and_then(|id| self.titles.get(&id))
            .map(String::as_str);
        let report = match (self.problem.as_deref(), title) {
            (None, Some(title)) if latest.is_some_and(|id| self.fullscreen.contains(&id)) => {
                GuardReport::Notice(format!(
                    "\"{title}\" is fullscreen on the card display; cardcast can't move it without taking focus. Take it out of fullscreen or move it off to guard it again."
                ))
            }
            (problem, title) => placement::notice(problem, title),
        };
        let report = match (report, &self.pointer_problem) {
            (GuardReport::Clear, Some(pointer)) => GuardReport::Notice(pointer.clone()),
            (report, _) => report,
        };
        let _ = self.reports.send(report);
        (self.wake)();
    }
}

/// A started helper; dropping it stops it.
enum Helper {
    X11(ewmh::Running),
    Kwin(kwin::Running),
    /// sway, Hyprland or GNOME.
    Compositor(compositor::Running),
}

struct Running {
    card_display: String,
    send_windows_to: Option<String>,
    helper: Helper,
}

/// The window guard and pointer guard on X11, Plasma, GNOME, sway and Hyprland.
pub struct WindowGuard {
    desktop: Desktop,
    list_monitors: ListMonitors,
    running: Option<Running>,
    report_sender: Sender<GuardReport>,
    reports: Receiver<GuardReport>,
    wake: Wake,
}

impl WindowGuard {
    /// `desktop` is one with a helper: not [`Desktop::OtherWayland`].
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
        // Every helper starts and stops on its own thread, so the settings window never waits.
        let helper = match self.desktop {
            Desktop::X11 => Helper::X11(ewmh::start(guarding)),
            Desktop::Sway => Helper::Compositor(compositor::start::<Sway>(guarding)),
            Desktop::Hyprland => Helper::Compositor(compositor::start::<Hyprland>(guarding)),
            Desktop::GnomeWayland => Helper::Compositor(compositor::start::<Gnome>(guarding)),
            // `new_guard` gives other Wayland compositors no window guard.
            Desktop::PlasmaWayland | Desktop::OtherWayland => Helper::Kwin(kwin::start(guarding)),
        };
        self.running = Some(Running {
            card_display: card_display.to_owned(),
            send_windows_to: send_windows_to.map(str::to_owned),
            helper,
        });
    }

    fn take_report(&mut self) -> Option<GuardReport> {
        self.reports.try_iter().last()
    }

    fn follow_displays(&mut self) {
        match self.running.as_ref().map(|running| &running.helper) {
            Some(Helper::X11(helper)) => helper.follow_displays(),
            Some(Helper::Kwin(helper)) => helper.follow_displays(),
            Some(Helper::Compositor(helper)) => helper.follow_displays(),
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
    fn a_pointer_guard_problem_is_said_when_the_window_guard_has_nothing_to_say() {
        let (mut guarding, reports) = guarding(None);
        guarding.follow_displays(work_area);
        assert_eq!(reports.try_iter().last(), Some(GuardReport::Clear));
        guarding.pointer_problem(Some("no warp".into()));
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice("no warp".into()))
        );
        let _ = guarding.check(3, "Stubborn", on_card(), false, Instant::now());
        guarding.give_up(3);
        assert!(matches!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(notice)) if notice.contains("Stubborn")
        ));
        guarding.forget(3);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice("no warp".into()))
        );
        guarding.pointer_problem(None);
        assert_eq!(reports.try_iter().last(), Some(GuardReport::Clear));
    }

    #[test]
    fn the_pointer_goes_back_off_the_card_display() {
        let (mut guarding, _reports) = guarding(None);
        assert_eq!(guarding.place_pointer((2500, 300)), None);
        guarding.follow_displays(work_area);
        // Before any off-card position: the middle of the "send windows to" work area.
        assert_eq!(guarding.place_pointer((2500, 300)), Some((960, 518)));
        assert_eq!(guarding.place_pointer((1900, 300)), None);
        assert_eq!(guarding.place_pointer((1920, 300)), Some((1900, 300)));
        let area = guarding.pointer_area().unwrap();
        assert_eq!(area.output, "card");
        assert_eq!(area.card_display, rect(1920, 0, 3840, 1080));
        assert_eq!(area.target((1925, 300)), Some((1919, 300)));
        assert_eq!(area.origin(), (0, 0));
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
        assert_eq!(reports.try_iter().last(), Some(GuardReport::Clear));
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
    fn a_window_that_will_not_unmaximise_is_given_up_and_named() {
        let (mut guarding, reports) = guarding(None);
        guarding.follow_displays(work_area);
        let maximised = WindowState {
            frame: rect(1920, 0, 3840, 1080),
            maximised: true,
            ..WindowState::default()
        };
        let _ = guarding.check(3, "Stubborn", maximised, false, Instant::now());
        guarding.give_up(3);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(
                "The window guard left \"Stubborn\" on the card display because it keeps going back there. Move it off to guard it again.".into()
            ))
        );
        assert_eq!(
            guarding.check(3, "Stubborn", maximised, false, Instant::now()),
            Placement::Stay
        );
    }

    #[test]
    fn a_fullscreen_window_left_alone_is_named_until_it_leaves_fullscreen() {
        let (mut guarding, reports) = guarding(None);
        guarding.follow_displays(work_area);
        let _ = guarding.check(4, "Game", on_card(), false, Instant::now());
        guarding.give_up_fullscreen(4);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(
                "\"Game\" is fullscreen on the card display; cardcast can't move it without taking focus. Take it out of fullscreen or move it off to guard it again.".into()
            ))
        );
        assert_eq!(
            guarding.check(4, "Game", on_card(), false, Instant::now()),
            Placement::Stay
        );
        guarding.movable_again(4);
        assert_eq!(reports.try_iter().last(), Some(GuardReport::Clear));
        assert!(matches!(
            guarding.check(4, "Game", on_card(), false, Instant::now()),
            Placement::MoveTo(_)
        ));
    }

    #[test]
    fn the_send_windows_to_monitor_is_named_by_its_output() {
        let (mut guarding, _reports) = guarding(None);
        assert_eq!(guarding.send_windows_to_output(), None);
        guarding.follow_displays(work_area);
        assert_eq!(guarding.send_windows_to_output(), Some("primary"));
    }

    #[test]
    fn a_helper_notice_reaches_the_status_line_as_it_is() {
        let (guarding, reports) = guarding(None);
        guarding.notice(kwin::WAITING_FOR_KWIN);
        assert_eq!(
            reports.try_iter().last(),
            Some(GuardReport::Notice(
                "KWin is not running, so the window guard waits for it to start again.".into()
            ))
        );
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
        assert_eq!(reports.try_iter().last(), Some(GuardReport::Clear));
    }
}
