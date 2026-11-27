//! The window guard on a real desktop, run by CI: Xvfb with openbox, and `kwin_wayland --virtual`
//! with Xwayland. The test window is an X11 window, moved and maximised as a pager does (EWMH),
//! which both window managers honour.

use super::super::desktop::Desktop;
use super::super::placement::Rect;
use super::super::{Guard, GuardReport, Monitor};
use super::ewmh::{self, Atoms};
use super::kwin;
use super::window_guard::{Guarding, ListMonitors, NO_POINTER_GUARD, WindowGuard, monitor_rect};
use std::io::BufRead as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Window,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// CI's Xvfb screen, 2560×1024, which `xrandr --setmonitor` splits into two monitors for openbox.
fn xvfb_halves() -> Result<Vec<Monitor>, String> {
    let half = |name: &str, left: i32| Monitor {
        device_path: name.into(),
        gdi_name: name.into(),
        name: name.into(),
        width: 1280,
        height: 1024,
        layout_size: (1280, 1024),
        refresh_hz: 60,
        primary: left == 0,
        left,
        top: 0,
        graphics_card: String::new(),
        container_id: String::new(),
        connector: String::new(),
    };
    Ok(vec![half("left", 0), half("right", 1280)])
}

struct TestWindow {
    /// The last frame and maximised state logged.
    logged: std::cell::Cell<Option<(Rect, bool)>>,
    connection: RustConnection,
    root: Window,
    atoms: Atoms,
    window: Window,
}

impl TestWindow {
    fn open(title: &str) -> Self {
        let (connection, screen) = x11rb::connect(None).unwrap();
        let root = connection.setup().roots[screen].root;
        let atoms = Atoms::new(&connection).unwrap().reply().unwrap();
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                root,
                0,
                0,
                400,
                300,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(0x0033_6699),
            )
            .unwrap();
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap();
        connection.map_window(window).unwrap();
        connection.flush().unwrap();
        let test = Self {
            logged: std::cell::Cell::new(None),
            connection,
            root,
            atoms,
            window,
        };
        wait_for(&format!("the window manager to manage {title}"), || {
            test.property(test.root, test.atoms._NET_CLIENT_LIST)
                .contains(&window)
        });
        test
    }

    fn property(&self, window: Window, property: u32) -> Vec<u32> {
        self.connection
            .get_property(false, window, property, AtomEnum::ANY, 0, 1024)
            .unwrap()
            .reply()
            .ok()
            .and_then(|reply| reply.value32().map(Iterator::collect))
            .unwrap_or_default()
    }

    fn frame(&self) -> Rect {
        let geometry = self
            .connection
            .get_geometry(self.window)
            .unwrap()
            .reply()
            .unwrap();
        let at = self
            .connection
            .translate_coordinates(self.window, self.root, 0, 0)
            .unwrap()
            .reply()
            .unwrap();
        ewmh::frame(
            (at.dst_x.into(), at.dst_y.into()),
            (geometry.width.into(), geometry.height.into()),
            self.extents(),
        )
    }

    /// The frame, logging it and the maximised state when they change.
    fn logged_frame(&self) -> Rect {
        let now = (self.frame(), self.maximised());
        if self.logged.replace(Some(now)) != Some(now) {
            eprintln!("frame {:?}, maximised {}", now.0, now.1);
        }
        now.0
    }

    fn extents(&self) -> [u32; 4] {
        let extents = self.property(self.window, self.atoms._NET_FRAME_EXTENTS);
        <[u32; 4]>::try_from(extents.as_slice()).unwrap_or_default()
    }

    fn maximised(&self) -> bool {
        let states = self.property(self.window, self.atoms._NET_WM_STATE);
        states.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_VERT)
            && states.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_HORZ)
    }

    fn active(&self) -> Vec<u32> {
        self.property(self.root, self.atoms._NET_ACTIVE_WINDOW)
    }

    fn send(&self, kind: u32, data: [u32; 5]) {
        let message = ClientMessageEvent::new(32, self.window, kind, data);
        self.connection
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            )
            .unwrap();
        self.connection.flush().unwrap();
    }

    /// Moves the frame's corner to `(left, top)`, keeping its size, as a pager does.
    fn move_to(&self, left: i32, top: i32) {
        let frame = self.frame();
        let target = Rect {
            left,
            top,
            right: left + frame.right - frame.left,
            bottom: top + frame.bottom - frame.top,
        };
        self.send(
            self.atoms._NET_MOVERESIZE_WINDOW,
            ewmh::moveresize(target, self.extents()),
        );
    }

    fn set_maximised(&self, maximised: bool) {
        self.send(
            self.atoms._NET_WM_STATE,
            [
                u32::from(maximised),
                self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
                self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
                2,
                0,
            ],
        );
    }
}

/// Polls until `done` holds; panics after ten seconds.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Most of the frame is on `monitor`.
fn mostly_on(frame: Rect, monitor: Rect) -> bool {
    let width = (frame.right.min(monitor.right) - frame.left.max(monitor.left)).max(0);
    let height = (frame.bottom.min(monitor.bottom) - frame.top.max(monitor.top)).max(0);
    let area = |r: Rect| i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
    i64::from(width) * i64::from(height) * 2 > area(frame)
}

fn offset(frame: Rect, left: i32, top: i32) -> Rect {
    Rect {
        left,
        top,
        right: left + frame.right - frame.left,
        bottom: top + frame.bottom - frame.top,
    }
}

/// `CARDCAST_GUARD_DESKTOP` (CI): `openbox` on Xvfb, or `kwin` for `kwin_wayland --virtual` with
/// two outputs. Skips when unset.
#[test]
fn desktop_window_guard_returns_windows_and_keeps_maximised_ones_maximised() {
    let Ok(desktop) = std::env::var("CARDCAST_GUARD_DESKTOP") else {
        return;
    };
    let (desktop, list_monitors): (Desktop, ListMonitors) = match desktop.as_str() {
        "openbox" => (Desktop::X11, xvfb_halves),
        "kwin" => (Desktop::PlasmaWayland, super::monitors),
        other => panic!("unknown CARDCAST_GUARD_DESKTOP {other}"),
    };
    let monitors = list_monitors().unwrap();
    eprintln!("{monitors:#?}");
    let card = monitors.iter().find(|m| !m.primary).unwrap();
    let card_rect = monitor_rect(card);
    let primary = monitor_rect(monitors.iter().find(|m| m.primary).unwrap());

    let window = TestWindow::open("cardcast guard test");
    // Opened last, so the window manager focuses it if it focuses new windows.
    let _focused = TestWindow::open("cardcast focus test");
    std::thread::sleep(Duration::from_millis(500));
    let active = window.active();
    let mut guard = WindowGuard::new(desktop, list_monitors, || {});
    guard.update(true, Some(&card.device_path), None);

    // Moved onto the card display while the guard runs: it comes back to the same place on the
    // primary monitor, without taking focus.
    let size = window.frame();
    window.move_to(card_rect.left + 100, card_rect.top + 100);
    let returned = offset(size, primary.left + 100, primary.top + 100);
    wait_for("the window to come back from the card display", || {
        window.logged_frame() == returned
    });
    assert_eq!(window.active(), active, "the guard changed the focus");

    // Maximised on the card display before the guard starts: it is maximised on the primary
    // monitor, and restores to the same place there.
    guard.update(false, Some(&card.device_path), None);
    window.move_to(card_rect.left + 100, card_rect.top + 100);
    wait_for("the window to reach the card display", || {
        mostly_on(window.frame(), card_rect)
    });
    window.set_maximised(true);
    wait_for("the window to be maximised on the card display", || {
        window.maximised() && mostly_on(window.frame(), card_rect)
    });
    guard.update(true, Some(&card.device_path), None);
    wait_for("the window to be maximised on the primary monitor", || {
        window.maximised() && mostly_on(window.logged_frame(), primary)
    });
    window.set_maximised(false);
    wait_for("the window to restore on the primary monitor", || {
        window.logged_frame() == returned
    });
    assert_eq!(window.active(), active, "the guard changed the focus");
    assert_eq!(
        guard.take_report(),
        Some(GuardReport::Notice(NO_POINTER_GUARD.into()))
    );
}

fn kwin_script_loaded(plugin: &str) -> bool {
    zbus::blocking::Connection::session()
        .unwrap()
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "isScriptLoaded",
            &plugin,
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap()
}

fn start_kwin_guard() -> kwin::Running {
    let (reports, _) = std::sync::mpsc::channel();
    let guarding = Guarding::new("Virtual-1", None, super::monitors, reports, Arc::new(|| {}));
    kwin::start(guarding).unwrap()
}

/// Run by the next test in a process of its own (`CARDCAST_KWIN_CHILD`): starts the guard, says its
/// script's name and waits to be killed.
#[test]
fn desktop_kwin_guard_child() {
    if std::env::var_os("CARDCAST_KWIN_CHILD").is_none() {
        return;
    }
    let running = start_kwin_guard();
    println!("plugin {}", running.plugin());
    std::thread::sleep(Duration::from_secs(60));
}

/// `CARDCAST_GUARD_DESKTOP=kwin` (CI): the guard's KWin script is gone once the guard stops, and
/// once a cardcast with a running guard is killed.
#[test]
fn desktop_kwin_script_never_outlives_cardcast() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("kwin") {
        return;
    }
    let running = start_kwin_guard();
    let plugin = running.plugin().to_owned();
    assert!(kwin_script_loaded(&plugin), "{plugin} is not loaded");
    drop(running);
    assert!(!kwin_script_loaded(&plugin), "{plugin} outlived the guard");

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["desktop_kwin_guard_child", "--nocapture"])
        .env("CARDCAST_KWIN_CHILD", "1")
        .env_remove("CARDCAST_GUARD_DESKTOP")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let plugin = std::io::BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
        .find_map(|line| line.strip_prefix("plugin ").map(str::to_owned))
        .unwrap_or_else(|| panic!("the child cardcast did not start its guard"));
    assert!(kwin_script_loaded(&plugin), "{plugin} is not loaded");
    child.kill().unwrap();
    child.wait().unwrap();
    let killed = Instant::now();
    wait_for("the script to unload itself", || {
        !kwin_script_loaded(&plugin)
    });
    eprintln!(
        "{plugin} unloaded itself {:?} after the kill",
        killed.elapsed()
    );
}
