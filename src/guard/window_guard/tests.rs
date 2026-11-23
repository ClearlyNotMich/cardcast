//! Checks the guard's Win32 calls on real windows owned by a child process, as on the sender.
//! The child is this test binary re-run on the ignored `test_window_process` test.

use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SMTO_ABORTIFHUNG, SW_SHOWNOACTIVATE, SendMessageTimeoutW, ShowWindow,
    TranslateMessage, WM_NULL, WS_OVERLAPPEDWINDOW,
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

/// Polls until `done` holds, because the guard's moves are posted to the window's program.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < CALL_LIMIT, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
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
    wait_for("window maximised", || {
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
    wait_for("restore rect set", || {
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

    wait_for("frame on target", || {
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
