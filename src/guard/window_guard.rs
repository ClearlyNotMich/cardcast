//! The window guard and pointer guard: out-of-context window event hooks and a pointer timer on their own thread.

use super::placement::{Placement, Rect, ReturnHistory, WindowState};
use super::pointer::PointerHistory;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_MINIMIZEEND, EnumWindows, GA_ROOT,
    GWL_EXSTYLE, GetAncestor, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, KillTimer, MSG,
    OBJID_WINDOW, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, RegisterClassW,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetCursorPos, SetTimer,
    SetWindowPos, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    WM_DISPLAYCHANGE, WM_QUIT, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{BOOL, w};

/// How long a window must be still before the guard checks it, so a drag is checked once.
const SETTLE: Duration = Duration::from_millis(50);

/// Pointer check interval: about 60 times a second.
const POINTER_INTERVAL_MS: u32 = 16;

/// The guard thread and what it was started with.
struct Running {
    thread_id: u32,
    thread: JoinHandle<()>,
    card_display: String,
    send_windows_to: Option<String>,
}

pub struct WindowGuard {
    running: Option<Running>,
    report_sender: Sender<String>,
    reports: Receiver<String>,
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

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            // SAFETY: posting to a thread id has no memory effects; a dead thread just fails the post.
            let posted =
                unsafe { PostThreadMessageW(running.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            if posted.is_ok() {
                let _ = running.thread.join();
            }
        }
    }
}

impl Drop for WindowGuard {
    fn drop(&mut self) {
        self.stop();
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
                let _ = self
                    .report_sender
                    .send("The window guard could not start.".into());
            }
        }
    }

    fn take_report(&mut self) -> Option<String> {
        self.reports.try_iter().last()
    }
}

/// Guard thread state, reached from the hook and window callbacks.
struct State {
    card_display: String,
    send_windows_to: Option<String>,
    /// Card display rectangle and the "send windows to" work area; `None` while the card display is unplugged.
    monitors: Option<(Rect, Rect)>,
    /// Windows waiting to settle, with their latest event time.
    pending: HashMap<isize, Instant>,
    timer: usize,
    history: ReturnHistory,
    pointer_timer: usize,
    pointer_history: PointerHistory,
    reports: Sender<String>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn run(
    card_display: String,
    send_windows_to: Option<String>,
    reports: Sender<String>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    started: Sender<u32>,
) {
    let mut state = State {
        card_display,
        send_windows_to,
        monitors: None,
        pending: HashMap::new(),
        timer: 0,
        history: ReturnHistory::default(),
        pointer_timer: 0,
        pointer_history: PointerHistory::default(),
        reports,
        wake,
    };
    state.monitors = find_monitors(&state);
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
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        }
    };
    let hooks = [
        hook(EVENT_OBJECT_SHOW),
        hook(EVENT_OBJECT_LOCATIONCHANGE),
        hook(EVENT_SYSTEM_MINIMIZEEND),
    ];
    let display_window = create_display_window();
    if hooks.iter().any(HWINEVENTHOOK::is_invalid) || display_window.is_none() {
        cleanup(&hooks, display_window);
        return;
    }

    let mut message = MSG::default();
    // SAFETY: message is a valid MSG; PM_NOREMOVE only creates this thread's queue so WM_QUIT can be posted.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: plain call returning this thread's id.
    if started.send(unsafe { GetCurrentThreadId() }).is_err() {
        cleanup(&hooks, display_window);
        return;
    }
    with_state(|state| {
        // SAFETY: a thread timer with no window posts WM_TIMER to this thread's queue.
        state.pointer_timer = unsafe { SetTimer(None, 0, POINTER_INTERVAL_MS, None) };
        sweep(state);
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
        } else {
            // SAFETY: message came from GetMessageW on this thread.
            unsafe { DispatchMessageW(&message) };
        }
    }
    cleanup(&hooks, display_window);
}

fn cleanup(hooks: &[HWINEVENTHOOK], display_window: Option<HWND>) {
    for &hook in hooks.iter().filter(|hook| !hook.is_invalid()) {
        // SAFETY: the hook was installed by this thread and is removed once.
        let _ = unsafe { UnhookWinEvent(hook) };
    }
    if let Some(window) = display_window {
        // SAFETY: the window was created by this thread.
        let _ = unsafe { DestroyWindow(window) };
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

/// A hidden top-level window, because only top-level windows hear about display changes.
fn create_display_window() -> Option<HWND> {
    // SAFETY: a null module name returns this executable's handle.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(display_window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("cardcast window guard"),
        ..Default::default()
    };
    // SAFETY: class points at static strings and a valid window procedure; registering twice only fails.
    unsafe { RegisterClassW(&class) };
    // SAFETY: the class was registered above; the window is never shown.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("cardcast window guard"),
            w!(""),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .ok()
}

extern "system" fn display_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DISPLAYCHANGE {
        with_state(|state| {
            state.monitors = find_monitors(state);
            sweep(state);
        });
    }
    // SAFETY: forwards the message this procedure received unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
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
        Placement::MoveTo(target) | Placement::Maximise(target) => target,
        Placement::GiveUp => {
            let _ = state.reports.send(format!(
                "The window guard left \"{}\" on the card display because it keeps going back there.",
                window_title(window)
            ));
            (state.wake)();
            return;
        }
    };
    // The window rectangle includes invisible resize borders around the visible frame; keep them.
    let frame = window_state.frame;
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
    // SAFETY: the remaining calls only read window flags and styles.
    let state = unsafe {
        WindowState {
            frame: rect(frame),
            maximised: IsZoomed(window).as_bool(),
            minimised: IsIconic(window).as_bool(),
            hidden: !IsWindowVisible(window).as_bool(),
            cloaked,
            tool: GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TOOLWINDOW.0 as isize != 0,
            own: process_id == GetCurrentProcessId(),
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

fn window_title(window: HWND) -> String {
    let mut title = [0u16; 256];
    // SAFETY: title is a writable buffer; for another program's window this reads cached text without messaging it.
    let length = unsafe { GetWindowTextW(window, &mut title) };
    let length = usize::try_from(length).unwrap_or(0);
    String::from_utf16_lossy(&title[..length])
}

/// The card display rectangle and the "send windows to" work area (primary if it is not connected).
fn find_monitors(state: &State) -> Option<(Rect, Rect)> {
    let monitors = super::monitors().ok()?;
    let card_display = monitors
        .iter()
        .find(|monitor| monitor.device_path == state.card_display)?;
    let send_windows_to = state
        .send_windows_to
        .as_ref()
        .and_then(|path| monitors.iter().find(|monitor| &monitor.device_path == path))
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))?;
    if send_windows_to == card_display {
        return None;
    }
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
    Some((
        rect(info(card_display)?.rcMonitor),
        rect(info(send_windows_to)?.rcWork),
    ))
}
