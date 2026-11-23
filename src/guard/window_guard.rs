//! The window guard and pointer guard: out-of-context window event hooks and a pointer timer on their own thread.

use super::GuardReport;
use super::placement::{self, Placement, Rect, ReturnHistory, WindowState};
use super::pointer::PointerHistory;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, DispatchMessageW, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_SHOW, EVENT_SYSTEM_MINIMIZEEND, EnumWindows, GA_ROOT, GWL_EXSTYLE, GetAncestor,
    GetClassNameW, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect,
    GetWindowThreadProcessId, InternalGetWindowText, IsIconic, IsWindowVisible, IsZoomed,
    KillTimer, MSG, OBJID_WINDOW, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SW_SHOWMAXIMIZED,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetCursorPos, SetTimer,
    SetWindowPlacement, SetWindowPos, WINDOWPLACEMENT, WINEVENT_OUTOFCONTEXT, WM_APP, WM_TIMER,
    WPF_ASYNCWINDOWPLACEMENT, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

/// How long a window must be still before the guard checks it, so a drag is checked once.
const SETTLE: Duration = Duration::from_millis(50);

/// Pointer check interval: about 60 times a second.
const POINTER_INTERVAL_MS: u32 = 16;

/// Thread message to the guard thread: the displays or devices changed.
const FOLLOW_DISPLAYS: u32 = WM_APP;

/// The guard thread and what it was started with.
struct Running {
    thread_id: u32,
    thread: JoinHandle<()>,
    card_display: String,
    send_windows_to: Option<String>,
}

pub struct WindowGuard {
    running: Option<Running>,
    report_sender: Sender<GuardReport>,
    reports: Receiver<GuardReport>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl WindowGuard {
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (report_sender, reports) = mpsc::channel();
        let wake = std::sync::Mutex::new(wake);
        Self {
            running: None,
            report_sender,
            reports,
            wake: std::sync::Arc::new(move || {
                if let Ok(wake) = wake.lock() {
                    wake();
                }
            }),
        }
    }

    /// Returns once the guard thread is gone, and its hooks and timers with it.
    fn stop(&mut self) {
        drop(self.running.take().map(|running| running.thread));
    }
}

impl Drop for WindowGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

impl WindowGuard {
    fn report(&self, report: GuardReport) {
        let _ = self.report_sender.send(report);
        (self.wake)();
    }
}

impl super::Guard for WindowGuard {
    fn update(&mut self, enabled: bool, card_display: Option<&str>, send_windows_to: Option<&str>) {
        let wanted = card_display.filter(|_| enabled);
        if let Some(running) = &self.running
            && Some(running.card_display.as_str()) == wanted
            && running.send_windows_to.as_deref() == send_windows_to
        {
            return;
        }
        self.stop();
        let Some(card_display) = wanted else {
            // The guard is off, so its notices are out of date.
            self.report(GuardReport::Clear);
            return;
        };
        let (started_sender, started) = mpsc::channel();
        let thread = {
            let card_display = card_display.to_owned();
            let send_windows_to = send_windows_to.map(str::to_owned);
            let reports = self.report_sender.clone();
            let wake = self.wake.clone();
            std::thread::spawn(move || {
                run(card_display, send_windows_to, reports, wake, started_sender);
            })
        };
        match started.recv() {
            Ok(thread_id) => {
                self.running = Some(Running {
                    thread_id,
                    thread,
                    card_display: card_display.to_owned(),
                    send_windows_to: send_windows_to.map(str::to_owned),
                });
            }
            Err(_) => {
                let _ = thread.join();
                self.report(GuardReport::Notice(
                    "The window guard could not start. Untick and tick Guard to try again.".into(),
                ));
            }
        }
    }

    fn take_report(&mut self) -> Option<GuardReport> {
        self.reports.try_iter().last()
    }

    fn follow_displays(&mut self) {
        if let Some(running) = &self.running.as_ref().filter(|_| false) {
            // SAFETY: posting to a thread id has no memory effects; the guard thread made its queue before it started.
            let _ = unsafe {
                PostThreadMessageW(running.thread_id, FOLLOW_DISPLAYS, WPARAM(0), LPARAM(0))
            };
        }
    }
}

/// Guard thread state, reached from the hook and window callbacks.
struct State {
    card_display: String,
    send_windows_to: Option<String>,
    /// Card display rectangle and the "send windows to" work area; `None` while there is nothing to guard.
    monitors: Option<(Rect, Rect)>,
    /// Why there is nothing to guard, unless the card display is just unplugged.
    problem: Option<String>,
    /// Windows waiting to settle, with their latest event time.
    pending: HashMap<isize, Instant>,
    timer: usize,
    history: ReturnHistory,
    pointer_timer: usize,
    pointer_history: PointerHistory,
    reports: Sender<GuardReport>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn run(
    card_display: String,
    send_windows_to: Option<String>,
    reports: Sender<GuardReport>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    started: Sender<u32>,
) {
    let state = State {
        card_display,
        send_windows_to,
        monitors: None,
        problem: None,
        pending: HashMap::new(),
        timer: 0,
        history: ReturnHistory::default(),
        pointer_timer: 0,
        pointer_history: PointerHistory::default(),
        reports,
        wake,
    };
    STATE.set(Some(state));

    let hook = |event| {
        // SAFETY: on_event matches WINEVENTPROC; out-of-context hooks call it on this thread while it pumps messages.
        unsafe {
            SetWinEventHook(
                event,
                event,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        }
    };
    let hooks = [
        hook(EVENT_OBJECT_SHOW),
        hook(EVENT_OBJECT_LOCATIONCHANGE),
        hook(EVENT_SYSTEM_MINIMIZEEND),
        hook(EVENT_OBJECT_DESTROY),
    ];
    if hooks.iter().any(HWINEVENTHOOK::is_invalid) {
        cleanup(&hooks);
        return;
    }

    let mut message = MSG::default();
    // SAFETY: message is a valid MSG; PM_NOREMOVE only creates this thread's queue so WM_QUIT can be posted.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: plain call returning this thread's id.
    if started.send(unsafe { GetCurrentThreadId() }).is_err() {
        cleanup(&hooks);
        return;
    }
    with_state(|state| {
        // SAFETY: a thread timer with no window posts WM_TIMER to this thread's queue.
        state.pointer_timer = unsafe { SetTimer(None, 0, POINTER_INTERVAL_MS, None) };
        follow_displays(state);
    });
    // SAFETY: message is a valid MSG; the loop ends on WM_QUIT (0) or error (-1).
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        if message.message == WM_TIMER && message.hwnd.is_invalid() {
            with_state(|state| {
                if message.wParam.0 == state.pointer_timer {
                    check_pointer(state);
                } else {
                    check_settled(state);
                }
            });
        } else if message.message == FOLLOW_DISPLAYS && message.hwnd.is_invalid() {
            with_state(follow_displays);
        } else {
            // SAFETY: message came from GetMessageW on this thread.
            unsafe { DispatchMessageW(&message) };
        }
    }
    cleanup(&hooks);
}

fn cleanup(hooks: &[HWINEVENTHOOK]) {
    for &hook in hooks.iter().filter(|hook| !hook.is_invalid()) {
        // SAFETY: the hook was installed by this thread and is removed once.
        let _ = unsafe { UnhookWinEvent(hook) };
    }
    with_state(|state| {
        for timer in [state.timer, state.pointer_timer] {
            if timer != 0 {
                // SAFETY: the thread timer was created by this thread.
                let _ = unsafe { KillTimer(None, timer) };
            }
        }
    });
}

/// Runs `f` on the thread state; a re-entrant call is dropped rather than panicking.
fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|cell| {
        if let Ok(mut state) = cell.try_borrow_mut()
            && let Some(state) = state.as_mut()
        {
            f(state);
        }
    });
}

extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // Position events arrive for every mouse movement; drop all but top-level windows at once.
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || window.is_invalid() {
        return;
    }
    // A destroyed window has no ancestor to check; only a given-up one matters.
    if event == EVENT_OBJECT_DESTROY {
        with_state(|state| {
            if state.history.forget(window.0 as isize) {
                report(state);
            }
        });
        return;
    }
    // SAFETY: GetAncestor only reads the window tree; a stale handle returns null.
    if unsafe { GetAncestor(window, GA_ROOT) } != window {
        return;
    }
    with_state(|state| {
        state.pending.insert(window.0 as isize, Instant::now());
        if state.timer == 0 {
            // SAFETY: a thread timer with no window posts WM_TIMER to this thread's queue.
            state.timer = unsafe { SetTimer(None, 0, 25, None) };
        }
    });
}

/// Puts the pointer back off the card display; a pointer confined by a game never gets there.
fn check_pointer(state: &mut State) {
    let Some((card_display, send_windows_to)) = state.monitors else {
        return;
    };
    let mut pointer = POINT::default();
    // SAFETY: pointer is a valid POINT; this fails only on a secure desktop, where the guard does nothing.
    if unsafe { GetCursorPos(&mut pointer) }.is_err() {
        return;
    }
    let fallback = (
        (send_windows_to.left + send_windows_to.right) / 2,
        (send_windows_to.top + send_windows_to.bottom) / 2,
    );
    if let Some((x, y)) =
        state
            .pointer_history
            .place((pointer.x, pointer.y), card_display, fallback)
    {
        // SAFETY: plain call; moving the pointer has no memory effects.
        let _ = unsafe { SetCursorPos(x, y) };
    }
}

fn check_settled(state: &mut State) {
    let now = Instant::now();
    let settled: Vec<isize> = state
        .pending
        .iter()
        .filter(|&(_, &at)| now.duration_since(at) >= SETTLE)
        .map(|(&window, _)| window)
        .collect();
    for window in settled {
        state.pending.remove(&window);
        check(state, HWND(window as *mut _), now);
    }
    if state.pending.is_empty() && state.timer != 0 {
        // SAFETY: the thread timer was created by this thread.
        let _ = unsafe { KillTimer(None, state.timer) };
        state.timer = 0;
    }
}

fn sweep(state: &mut State) {
    let mut windows: Vec<HWND> = Vec::new();
    // SAFETY: lparam points at `windows`, which outlives the synchronous enumeration.
    let _ = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut windows as *mut Vec<HWND> as isize),
        )
    };
    let now = Instant::now();
    for window in windows {
        check(state, window, now);
    }
}

extern "system" fn collect_window(window: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: sweep passes a pointer to a live Vec<HWND> and nothing else touches it meanwhile.
    let windows = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    windows.push(window);
    BOOL(1)
}

/// Reads the window, decides, and moves it without activating it, reordering it or waiting on its program.
fn check(state: &mut State, window: HWND, now: Instant) {
    let Some((card_display, send_windows_to)) = state.monitors else {
        return;
    };
    let Some((window_state, window_rect)) = read_window(window) else {
        return;
    };
    let target = match state.history.place(
        window.0 as isize,
        &window_state,
        card_display,
        send_windows_to,
        now,
    ) {
        Placement::Stay => return,
        Placement::MoveTo(target) => target,
        Placement::Maximise { work_area, restore } => {
            maximise(window, restore);
            // Changing only the placement leaves a maximised window on its monitor, and maximising
            // a restored one activates it, so move the maximised frame over (measured on the CI runner).
            work_area
        }
        Placement::GiveUp | Placement::Handled => {
            report(state);
            return;
        }
    };
    move_window(window, window_state.frame, window_rect, target);
}

/// Moves the window so its visible frame lands on `target`, without waiting on its program.
fn move_window(window: HWND, frame: Rect, window_rect: Rect, target: Rect) {
    // The window rectangle includes invisible resize borders around the visible frame; keep them.
    let x = target.left - (frame.left - window_rect.left);
    let y = target.top - (frame.top - window_rect.top);
    let width = (target.right - target.left) + (window_rect.right - window_rect.left)
        - (frame.right - frame.left);
    let height = (target.bottom - target.top) + (window_rect.bottom - window_rect.top)
        - (frame.bottom - frame.top);
    // SAFETY: SWP_ASYNCWINDOWPOS posts the request, so a hung program cannot block this thread.
    let _ = unsafe {
        SetWindowPos(
            window,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        )
    };
}

/// Maximises the window on the monitor holding `restore` and makes `restore` its restore position.
fn maximise(window: HWND, restore: Rect) {
    let Some(mut placement) = window_placement(window) else {
        return;
    };
    let (x, y) = workspace_origin();
    placement.rcNormalPosition = RECT {
        left: restore.left - x,
        top: restore.top - y,
        right: restore.right - x,
        bottom: restore.bottom - y,
    };
    placement.showCmd = SW_SHOWMAXIMIZED.0 as u32;
    placement.flags = WPF_ASYNCWINDOWPLACEMENT;
    // SAFETY: placement is a WINDOWPLACEMENT with length set; WPF_ASYNCWINDOWPLACEMENT posts the request, so a hung program cannot block this thread.
    let _ = unsafe { SetWindowPlacement(window, &placement) };
}

fn window_placement(window: HWND) -> Option<WINDOWPLACEMENT> {
    let mut placement = WINDOWPLACEMENT {
        length: u32::try_from(size_of::<WINDOWPLACEMENT>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: placement is a WINDOWPLACEMENT with length set; a stale handle just fails.
    unsafe { GetWindowPlacement(window, &mut placement) }.ok()?;
    Some(placement)
}

/// Desktop position of workspace (0,0): the primary monitor's work area corner.
fn workspace_origin() -> (i32, i32) {
    // SAFETY: plain lookup; MONITOR_DEFAULTTOPRIMARY always returns a monitor.
    let handle = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: info is a MONITORINFO with cbSize set.
    if unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool() {
        (info.rcWork.left, info.rcWork.top)
    } else {
        (0, 0)
    }
}

/// The window's state and its full window rectangle; none of these calls wait on the window's program.
fn read_window(window: HWND) -> Option<(WindowState, Rect)> {
    let mut window_rect = RECT::default();
    // SAFETY: window_rect is a valid RECT; a stale handle just fails.
    unsafe { GetWindowRect(window, &mut window_rect) }.ok()?;
    let mut frame = RECT::default();
    // SAFETY: frame is a RECT, matching DWMWA_EXTENDED_FRAME_BOUNDS and the size passed.
    let frame = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            u32::try_from(size_of::<RECT>()).unwrap_or(0),
        )
    }
    .map_or(window_rect, |()| frame);
    let mut cloaked = 0u32;
    // SAFETY: cloaked is a u32, matching DWMWA_CLOAKED and the size passed.
    let cloaked = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            u32::try_from(size_of::<u32>()).unwrap_or(0),
        )
    }
    .is_ok_and(|()| cloaked != 0);
    let mut process_id = 0u32;
    // SAFETY: process_id is a valid u32 out pointer.
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    let (x, y) = workspace_origin();
    let normal = window_placement(window)?.rcNormalPosition;
    let restore = Rect {
        left: normal.left + x,
        top: normal.top + y,
        right: normal.right + x,
        bottom: normal.bottom + y,
    };
    // SAFETY: the remaining calls only read window flags and styles.
    let state = unsafe {
        WindowState {
            frame: rect(frame),
            maximised: IsZoomed(window).as_bool(),
            restore,
            minimised: IsIconic(window).as_bool(),
            hidden: !IsWindowVisible(window).as_bool(),
            cloaked,
            tool: GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TOOLWINDOW.0 as isize != 0,
            mirror: placement::is_mirror(process_id == GetCurrentProcessId(), &class_name(window)),
        }
    };
    Some((state, rect(window_rect)))
}

fn rect(rect: RECT) -> Rect {
    Rect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn class_name(window: HWND) -> String {
    let mut name = [0u16; 256];
    // SAFETY: name is a writable buffer; reads the class without messaging the window.
    let length = unsafe { GetClassNameW(window, &mut name) };
    let length = usize::try_from(length).unwrap_or(0);
    String::from_utf16_lossy(&name[..length])
}

fn window_title(window: HWND) -> String {
    let mut title = [0u16; 256];
    // SAFETY: title is a writable buffer; reads the stored caption without messaging, even for cardcast's own windows.
    let length = unsafe { InternalGetWindowText(window, &mut title) };
    let length = usize::try_from(length).unwrap_or(0);
    String::from_utf16_lossy(&title[..length])
}

/// Sends the guard's current notice to the status line.
fn report(state: &State) {
    let title = state
        .history
        .latest_given_up()
        .map(|window| window_title(HWND(window as *mut _)));
    let _ = state.reports.send(placement::notice(
        state.problem.as_deref(),
        title.as_deref(),
    ));
    (state.wake)();
}

/// At start and after a display change: finds the monitors, reports, and checks every window.
fn follow_displays(state: &mut State) {
    (state.monitors, state.problem) = match find_monitors(state) {
        Ok(monitors) => (monitors, None),
        Err(problem) => (None, Some(problem)),
    };
    report(state);
    sweep(state);
}

/// The card display rectangle and the "send windows to" work area; `Ok(None)` while the card display is unplugged.
fn find_monitors(state: &State) -> Result<Option<(Rect, Rect)>, String> {
    let monitors = super::monitors()?;
    let Some((card_display, send_windows_to)) = placement::guarded_monitors(
        &monitors,
        &state.card_display,
        state.send_windows_to.as_deref(),
    )?
    else {
        return Ok(None);
    };
    let info = |monitor: &super::Monitor| {
        let point = POINT {
            x: monitor.left,
            y: monitor.top,
        };
        // SAFETY: plain lookup by point; MONITOR_DEFAULTTONULL returns null when nothing is there.
        let handle = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
        let mut info = MONITORINFO {
            cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: info is a MONITORINFO with cbSize set; a null handle just fails.
        unsafe { GetMonitorInfoW(handle, &mut info) }
            .as_bool()
            .then_some(info)
    };
    let (Some(card_display), Some(send_windows_to)) = (info(card_display), info(send_windows_to))
    else {
        return Err("The window guard could not find the card display on the desktop.".into());
    };
    Ok(Some((
        rect(card_display.rcMonitor),
        rect(send_windows_to.rcWork),
    )))
}

#[cfg(test)]
mod tests;
