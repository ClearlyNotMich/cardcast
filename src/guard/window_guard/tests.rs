//! Checks the guard's Win32 calls on real windows owned by a child process, as on the sender.
//! The child is this test binary re-run on the ignored `test_window_process` test.

use super::*;
use crate::guard::second_monitor::{self, wait_for};
use crate::guard::{Guard as _, Monitor};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SMTO_ABORTIFHUNG, SW_SHOWNOACTIVATE, SWP_NOSIZE, SendMessageTimeoutW,
    ShowWindow, TranslateMessage, WM_NULL, WS_OVERLAPPEDWINDOW,
};

const CHILD_MODE: &str = "CARDCAST_TEST_WINDOW";
const TITLE: &str = "cardcast test window";
/// Longest a guard call on another program's window may take.
const CALL_LIMIT: Duration = Duration::from_secs(2);

/// A top-level window in a child process; the process is killed on drop.
struct TestWindow {
    child: Child,
    window: HWND,
}

impl TestWindow {
    /// `pumping: false` makes the child stop pumping messages once the window exists, like a hung program.
    fn spawn(pumping: bool) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "guard::window_guard::tests::test_window_process",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD_MODE, if pumping { "pumping" } else { "hung" })
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let window = stdout
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.strip_prefix("window ")?.parse::<isize>().ok())
            .unwrap();
        Self {
            child,
            window: HWND(window as *mut _),
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs only as the child process: creates the test window, prints its handle, then pumps or hangs.
#[test]
#[ignore = "child process for the other tests"]
fn test_window_process() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    // SAFETY: a null module name returns this executable's handle.
    let instance = unsafe { GetModuleHandleW(None) }.unwrap();
    let class = WNDCLASSW {
        lpfnWndProc: Some(test_window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("cardcast test window"),
        ..Default::default()
    };
    // SAFETY: class points at static strings and a valid window procedure.
    unsafe { RegisterClassW(&class) };
    // SAFETY: the class was registered above.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("cardcast test window"),
            w!("cardcast test window"),
            WS_OVERLAPPEDWINDOW,
            200,
            150,
            640,
            480,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .unwrap();
    // SAFETY: window was created above on this thread.
    let _ = unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
    println!("window {}", window.0 as isize);
    if mode == "hung" {
        std::thread::sleep(Duration::from_secs(120));
        return;
    }
    let mut message = MSG::default();
    // SAFETY: message is a valid MSG; the parent kills this process when done.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        // SAFETY: message came from GetMessageW on this thread.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

extern "system" fn test_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwards the message this procedure received unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// The primary monitor's work area in desktop coordinates.
fn work_area() -> Rect {
    // SAFETY: plain lookup; MONITOR_DEFAULTTOPRIMARY always returns a monitor.
    let handle = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap(),
        ..Default::default()
    };
    // SAFETY: info is a MONITORINFO with cbSize set.
    assert!(unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool());
    rect(info.rcWork)
}

fn foreground() -> isize {
    // SAFETY: plain call returning a handle.
    unsafe { GetForegroundWindow() }.0 as isize
}

#[test]
fn maximise_keeps_window_maximised_with_restore_rect_from_issue_12() {
    let test = TestWindow::spawn(true);
    // SAFETY: plain call on a window that exists until `test` drops.
    let _ = unsafe { ShowWindow(test.window, SW_SHOWMAXIMIZED) };
    wait_for("window maximised", CALL_LIMIT, || {
        // SAFETY: plain flag read.
        unsafe { IsZoomed(test.window) }.as_bool()
    });
    let before = foreground();

    let work = work_area();
    let restore = Rect {
        left: work.left + 100,
        top: work.top + 80,
        right: work.left + 500,
        bottom: work.top + 380,
    };
    maximise(test.window, restore);

    // #12: the restore rect, read back through workspace coordinates, is the one the guard chose.
    wait_for("restore rect set", CALL_LIMIT, || {
        read_window(test.window).is_some_and(|(state, _)| state.restore == restore)
    });
    let (state, _) = read_window(test.window).unwrap();
    assert!(state.maximised, "window is still maximised");
    assert_eq!(
        foreground(),
        before,
        "maximise changed the foreground window"
    );
}

#[test]
fn move_window_puts_frame_on_target_without_changing_foreground() {
    let test = TestWindow::spawn(true);
    let before = foreground();
    let work = work_area();
    let target = Rect {
        left: work.left + 50,
        top: work.top + 40,
        right: work.left + 450,
        bottom: work.top + 340,
    };
    let (state, window_rect) = read_window(test.window).unwrap();
    move_window(test.window, state.frame, window_rect, target);

    wait_for("frame on target", CALL_LIMIT, || {
        read_window(test.window).is_some_and(|(state, _)| state.frame == target)
    });
    assert_eq!(foreground(), before, "moving changed the foreground window");
}

#[test]
fn guard_calls_on_a_hung_window_return_in_time() {
    let test = TestWindow::spawn(false);
    let window = test.window.0 as isize;
    // SAFETY: a synchronous message with a timeout; it proves the child really does not answer.
    let answered = unsafe {
        SendMessageTimeoutW(
            test.window,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            200,
            None,
        )
    };
    assert_eq!(answered.0, 0, "the hung child answered a message");

    let (done, finished) = mpsc::channel();
    // On its own thread, so a blocking call fails the test instead of hanging it.
    std::thread::spawn(move || {
        let window = HWND(window as *mut _);
        let title = window_title(window);
        let (state, window_rect) = read_window(window).unwrap();
        let target = Rect {
            left: state.frame.left + 10,
            ..state.frame
        };
        move_window(window, state.frame, window_rect, target);
        maximise(window, state.frame);
        let _ = done.send(title);
    });
    let title = finished.recv_timeout(CALL_LIMIT).unwrap();
    assert_eq!(title, TITLE);
}

/// A running window guard sending windows from `card_display` to `send_windows_to`.
fn guard_on(card_display: &Monitor, send_windows_to: &Monitor) -> WindowGuard {
    let mut guard = WindowGuard::new(|| {});
    guard.update(
        true,
        Some(&card_display.device_path),
        Some(&send_windows_to.device_path),
    );
    guard
}

/// Whether the centre of `frame` is on `monitor`.
fn on(monitor: &Monitor, frame: Rect) -> bool {
    let (x, y) = (
        (frame.left + frame.right) / 2,
        (frame.top + frame.bottom) / 2,
    );
    let right = monitor.left + i32::try_from(monitor.width).unwrap();
    let bottom = monitor.top + i32::try_from(monitor.height).unwrap();
    (monitor.left..right).contains(&x) && (monitor.top..bottom).contains(&y)
}

fn frame(window: HWND) -> Rect {
    read_window(window).unwrap().0.frame
}

/// Moves `window` onto `monitor` the way a user would, keeping its size.
fn move_onto(window: HWND, monitor: &Monitor) {
    // SAFETY: a plain move of a window that is alive and pumping in the child process.
    unsafe {
        SetWindowPos(
            window,
            None,
            monitor.left + 50,
            monitor.top + 50,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .unwrap();
    assert!(on(monitor, frame(window)), "the window did not move there");
}

#[test]
fn second_monitor_window_moved_onto_the_card_display_is_returned() {
    let Some(monitors) = second_monitor::monitors(2) else {
        return;
    };
    let (primary, card_display) = (&monitors[0], &monitors[1]);
    let _guard = guard_on(card_display, primary);
    let test = TestWindow::spawn(true);
    let before = foreground();
    move_onto(test.window, card_display);
    wait_for("window returned to the primary monitor", CALL_LIMIT, || {
        on(primary, frame(test.window))
    });
    assert_eq!(
        foreground(),
        before,
        "returning changed the foreground window"
    );
}

#[test]
fn second_monitor_window_on_the_card_display_at_guard_start_is_returned() {
    let Some(monitors) = second_monitor::monitors(2) else {
        return;
    };
    let (primary, card_display) = (&monitors[0], &monitors[1]);
    let test = TestWindow::spawn(true);
    move_onto(test.window, card_display);
    let _guard = guard_on(card_display, primary);
    wait_for("window returned to the primary monitor", CALL_LIMIT, || {
        on(primary, frame(test.window))
    });
}

#[test]
fn second_monitor_maximised_window_stays_maximised_on_return() {
    let Some(monitors) = second_monitor::monitors(2) else {
        return;
    };
    let (primary, card_display) = (&monitors[0], &monitors[1]);
    let test = TestWindow::spawn(true);
    move_onto(test.window, card_display);
    // SAFETY: plain call on a window that exists until `test` drops.
    let _ = unsafe { ShowWindow(test.window, SW_SHOWMAXIMIZED) };
    wait_for("window maximised on the card display", CALL_LIMIT, || {
        // SAFETY: plain flag read.
        unsafe { IsZoomed(test.window) }.as_bool() && on(card_display, frame(test.window))
    });
    let before = foreground();
    let _guard = guard_on(card_display, primary);
    wait_for("window returned to the primary monitor", CALL_LIMIT, || {
        on(primary, frame(test.window))
    });
    let (state, _) = read_window(test.window).unwrap();
    assert_eq!(
        foreground(),
        before,
        "returning changed the foreground window"
    );
    assert!(
        state.maximised,
        "the returned window is no longer maximised"
    );
    assert!(
        on(primary, state.restore),
        "restore position left on the card display"
    );
}

/// Moves `test`'s window onto the card display until the guard gives up on it, and checks the notice.
fn give_up_on(
    guard: &mut WindowGuard,
    test: &TestWindow,
    card_display: &Monitor,
    primary: &Monitor,
) {
    for _ in 0..placement::RETURNS_PER_MINUTE {
        move_onto(test.window, card_display);
        wait_for("window returned to the primary monitor", CALL_LIMIT, || {
            on(primary, frame(test.window))
        });
    }
    move_onto(test.window, card_display);
    wait_for_report(
        guard,
        "the give-up notice",
        |report| matches!(report, GuardReport::Notice(notice) if notice.contains(TITLE)),
    );
    assert!(
        on(card_display, frame(test.window)),
        "the given-up window was moved"
    );
}

/// Polls `take_report` until the latest report matches.
fn wait_for_report(guard: &mut WindowGuard, what: &str, wanted: impl Fn(&GuardReport) -> bool) {
    wait_for(what, CALL_LIMIT, || {
        guard.take_report().is_some_and(|report| wanted(&report))
    });
}

#[test]
fn second_monitor_give_up_notice_clears_when_the_window_is_moved_off() {
    let Some(monitors) = second_monitor::monitors(2) else {
        return;
    };
    let (primary, card_display) = (&monitors[0], &monitors[1]);
    let mut guard = guard_on(card_display, primary);
    let test = TestWindow::spawn(true);
    give_up_on(&mut guard, &test, card_display, primary);
    move_onto(test.window, primary);
    wait_for_report(&mut guard, "the notice to clear", |report| {
        *report == GuardReport::Clear
    });
}

#[test]
fn second_monitor_give_up_notice_clears_when_the_windows_process_is_killed() {
    let Some(monitors) = second_monitor::monitors(2) else {
        return;
    };
    let (primary, card_display) = (&monitors[0], &monitors[1]);
    let mut guard = guard_on(card_display, primary);
    let mut test = TestWindow::spawn(true);
    give_up_on(&mut guard, &test, card_display, primary);
    test.child.kill().unwrap();
    test.child.wait().unwrap();
    // Only EVENT_OBJECT_DESTROY tells the guard about a killed process's window.
    wait_for_report(&mut guard, "the notice to clear", |report| {
        *report == GuardReport::Clear
    });
}

fn pointer() -> (i32, i32) {
    let mut pointer = POINT::default();
    // SAFETY: pointer is a valid POINT.
    unsafe { GetCursorPos(&mut pointer) }.unwrap();
    (pointer.x, pointer.y)
}

#[test]
fn second_monitor_pointer_is_put_back_off_the_card_display() {
    let Some(monitors) = second_monitor::monitors(2) else {
        return;
    };
    let (primary, card_display) = (&monitors[0], &monitors[1]);
    let _guard = guard_on(card_display, primary);
    let off_card = (primary.left + 100, primary.top + 100);
    // SAFETY: plain call; moving the pointer has no memory effects.
    unsafe { SetCursorPos(off_card.0, off_card.1) }.unwrap();
    // Let the pointer guard see the off-card position.
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: as above.
    unsafe { SetCursorPos(card_display.left + 100, card_display.top + 100) }.unwrap();
    wait_for(
        "pointer back at its last off-card position",
        CALL_LIMIT,
        || pointer() == off_card,
    );
}
