//! The window guard on a real desktop, run by CI: Xvfb with openbox, and `kwin_wayland --virtual`
//! with Xwayland. The test window is an X11 window, moved and maximised as a pager does (EWMH),
//! which both window managers honour; on KWin a native Wayland window (`foot`) is moved by a test
//! script of its own.

use super::super::desktop::Desktop;
use super::super::placement::Rect;
use super::super::{Guard, GuardReport, Monitor};
use super::ewmh::{self, Atoms};
use super::kwin;
use super::window_guard::{Guarding, ListMonitors, WindowGuard, monitor_rect};
use std::io::BufRead as _;
use std::sync::{Arc, Mutex};
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
    active_window: u32,
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
        let active_window = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let test = Self {
            logged: std::cell::Cell::new(None),
            active_window,
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
        self.property(self.root, self.active_window)
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

/// Polls until `done` holds; panics after twenty seconds.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
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
    let mut unmaximised = false;
    wait_for("the window to be maximised on the primary monitor", || {
        let frame = window.logged_frame();
        unmaximised |= !window.maximised();
        window.maximised() && mostly_on(frame, primary)
    });
    // KWin sends the window to the other output maximised; on X11 the guard unmaximises it to
    // learn where it restores to.
    assert!(
        !(unmaximised && desktop == Desktop::PlasmaWayland),
        "KWin unmaximised the window on its way"
    );
    window.set_maximised(false);
    wait_for("the window to restore on the primary monitor", || {
        window.logged_frame() == returned
    });
    assert_eq!(window.active(), active, "the guard changed the focus");
    assert_eq!(guard.take_report(), Some(GuardReport::Clear));
}

/// Whether KWin has the script; false while KWin doesn't answer.
fn kwin_script_loaded(plugin: &str) -> bool {
    kwin_scripting("isScriptLoaded", &plugin)
        .and_then(|reply| reply.body().deserialize())
        .unwrap_or(false)
}

fn kwin_scripting<B>(method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
{
    zbus::blocking::Connection::session()?.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        method,
        body,
    )
}

/// A started KWin guard, once its script is loaded.
fn start_kwin_guard() -> kwin::Running {
    let (reports, _) = std::sync::mpsc::channel();
    let guarding = Guarding::new("Virtual-1", None, super::monitors, reports, Arc::new(|| {}));
    let running = kwin::start(guarding);
    wait_for("the guard's script to load", || {
        kwin_script_loaded(running.plugin())
    });
    running
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
    wait_for("the guard to unload its script", || {
        !kwin_script_loaded(&plugin)
    });

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

/// `CARDCAST_KWIN_START` (CI): a shell command that starts `kwin_wayland --virtual` on this session
/// bus, which already runs one. KWin is killed: the status line says the guard waits for it; KWin
/// is started again: the guard loads its script again and the notice goes. Skips when unset.
#[test]
fn desktop_kwin_guard_follows_a_kwin_restart() {
    let Ok(start) = std::env::var("CARDCAST_KWIN_START") else {
        return;
    };
    let (sender, reports) = std::sync::mpsc::channel();
    let guarding = Guarding::new("right", None, xvfb_halves, sender, Arc::new(|| {}));
    let running = kwin::start(guarding);
    let plugin = running.plugin().to_owned();
    let mut status = None;
    let mut latest = move || {
        if let Some(report) = reports.try_iter().last() {
            eprintln!("status: {report:?}");
            status = Some(report);
        }
        status.clone()
    };
    let guarding = Some(GuardReport::Clear);
    let waiting = Some(GuardReport::Notice(kwin::WAITING_FOR_KWIN.into()));
    wait_for("the guard to start", || {
        kwin_script_loaded(&plugin) && latest() == guarding
    });

    // The KWin on this test's own session bus, never another.
    let bus = zbus::blocking::Connection::session().unwrap();
    let kwin: u32 = bus
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &"org.kde.KWin",
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let killed = std::process::Command::new("kill")
        .arg(kwin.to_string())
        .status()
        .unwrap();
    assert!(killed.success(), "could not stop KWin ({kwin})");
    wait_for("the notice that KWin is gone", || latest() == waiting);

    let started = std::process::Command::new("sh")
        .args(["-c", &format!("{start} &")])
        .status()
        .unwrap();
    assert!(started.success(), "{start} failed");
    let restarted = Instant::now();
    wait_for("the guard to load its script into the new KWin", || {
        kwin_script_loaded(&plugin)
    });
    wait_for("the notice to go", || latest() == guarding);
    eprintln!(
        "the guard was back {:?} after KWin started",
        restarted.elapsed()
    );
}

/// What the probe script last saw of the window and the move it is to make next.
#[derive(Default)]
struct Probed {
    seen: Option<(Rect, bool)>,
    command: Option<String>,
}

/// Serves the probe script (`PROBE_SCRIPT`) on the session bus.
struct ProbeService(Arc<Mutex<Probed>>);

#[zbus::interface(name = "io.github.ClearlyNotMich.cardcast.Probe")]
impl ProbeService {
    /// `<left> <top> <right> <bottom> <maximised>`; returns `move <left> <top>`, `maximise` or nothing.
    fn seen(&self, window: String) -> String {
        let numbers: Vec<i32> = window.split(' ').filter_map(|n| n.parse().ok()).collect();
        let mut probed = self.0.lock().unwrap();
        if let [left, top, right, bottom, maximised] = numbers[..] {
            let now = (
                Rect {
                    left,
                    top,
                    right,
                    bottom,
                },
                maximised != 0,
            );
            if probed.seen != Some(now) {
                eprintln!("frame {:?}, maximised {}", now.0, now.1);
            }
            probed.seen = Some(now);
        }
        probed.command.take().unwrap_or_default()
    }
}

/// A KWin script that tells the test where the window of process `@PID@` is every 20 ms, and moves or
/// maximises it when the test says so.
const PROBE_SCRIPT: &str = r#"
const probe = new QTimer();
probe.interval = 20;
probe.timeout.connect(() => {
    for (const w of workspace.windowList()) {
        if (w.pid !== @PID@) continue;
        const g = w.frameGeometry;
        const state = [g.x, g.y, g.x + g.width, g.y + g.height].map(Math.round)
            .concat([Number(w.maximizeMode === 3)]).join(" ");
        callDBus("@SERVICE@", "/probe", "io.github.ClearlyNotMich.cardcast.Probe", "Seen", state,
            reply => {
                const [action, x, y] = reply.split(" ");
                if (action === "move") {
                    w.frameGeometry = { x: Number(x), y: Number(y), width: g.width, height: g.height };
                } else if (action === "maximise") {
                    w.setMaximize(true, true);
                }
            });
    }
});
probe.start();
"#;

/// A native Wayland window, watched and moved through the probe script; closed on drop.
struct WaylandWindow {
    client: std::process::Child,
    probed: Arc<Mutex<Probed>>,
    _bus: zbus::blocking::Connection,
}

impl WaylandWindow {
    fn open(program: &str) -> Self {
        let probed = Arc::new(Mutex::new(Probed::default()));
        let bus = zbus::blocking::connection::Builder::session()
            .unwrap()
            .serve_at("/probe", ProbeService(probed.clone()))
            .unwrap()
            .build()
            .unwrap();
        let client = std::process::Command::new(program).spawn().unwrap();
        let file = std::env::temp_dir().join("cardcast-test-probe.js");
        let script = PROBE_SCRIPT
            .replace("@PID@", &client.id().to_string())
            .replace("@SERVICE@", bus.unique_name().unwrap().as_str());
        std::fs::write(&file, script).unwrap();
        let _ = kwin_scripting("unloadScript", &"cardcast-test-probe");
        kwin_scripting(
            "loadScript",
            &(file.to_string_lossy().as_ref(), "cardcast-test-probe"),
        )
        .unwrap();
        kwin_scripting("start", &()).unwrap();
        let window = Self {
            client,
            probed,
            _bus: bus,
        };
        wait_for(&format!("{program} to open"), || window.seen().is_some());
        // KWin may still be placing the new window; a move before then is lost.
        std::thread::sleep(Duration::from_millis(500));
        window
    }

    fn seen(&self) -> Option<(Rect, bool)> {
        self.probed.lock().unwrap().seen
    }

    fn frame(&self) -> Rect {
        self.seen().unwrap().0
    }

    fn maximised(&self) -> bool {
        self.seen().unwrap().1
    }

    fn command(&self, command: String) {
        self.probed.lock().unwrap().command = Some(command);
    }
}

impl Drop for WaylandWindow {
    fn drop(&mut self) {
        let _ = self.client.kill();
        let _ = self.client.wait();
        let _ = kwin_scripting("unloadScript", &"cardcast-test-probe");
    }
}

/// `CARDCAST_GUARD_DESKTOP=kwin` (CI, with `foot` installed): a native Wayland window moved onto
/// the card display comes back. With `CARDCAST_KWIN_COMPOSITES` (KWin has a render device, so the
/// window redraws), maximised there, it is maximised on the primary monitor, staying maximised on
/// the way.
#[test]
fn desktop_kwin_guard_returns_a_wayland_window() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("kwin") {
        return;
    }
    let monitors = super::monitors().unwrap();
    let card = monitors.iter().find(|m| !m.primary).unwrap();
    let card_rect = monitor_rect(card);
    let primary = monitor_rect(monitors.iter().find(|m| m.primary).unwrap());
    let window = WaylandWindow::open("foot");
    let mut guard = WindowGuard::new(Desktop::PlasmaWayland, super::monitors, || {});
    guard.update(true, Some(&card.device_path), None);

    let returned = offset(window.frame(), primary.left + 100, primary.top + 100);
    window.command(format!(
        "move {} {}",
        card_rect.left + 100,
        card_rect.top + 100
    ));
    wait_for(
        "the Wayland window to come back from the card display",
        || window.frame() == returned,
    );

    if std::env::var_os("CARDCAST_KWIN_COMPOSITES").is_none() {
        return;
    }
    guard.update(false, Some(&card.device_path), None);
    window.command(format!(
        "move {} {}",
        card_rect.left + 100,
        card_rect.top + 100
    ));
    wait_for("the Wayland window to reach the card display", || {
        mostly_on(window.frame(), card_rect)
    });
    window.command("maximise".into());
    wait_for(
        "the Wayland window to be maximised on the card display",
        || window.maximised() && mostly_on(window.frame(), card_rect),
    );
    guard.update(true, Some(&card.device_path), None);
    let mut unmaximised = false;
    wait_for(
        "the Wayland window to be maximised on the primary monitor",
        || {
            unmaximised |= !window.maximised();
            window.maximised() && mostly_on(window.frame(), primary)
        },
    );
    assert!(!unmaximised, "KWin unmaximised the window on its way");
}

/// Where the X pointer is.
fn x_pointer(connection: &RustConnection, root: Window) -> (i32, i32) {
    let pointer = connection.query_pointer(root).unwrap().reply().unwrap();
    (pointer.root_x.into(), pointer.root_y.into())
}

fn x_warp(connection: &RustConnection, root: Window, (x, y): (i32, i32)) {
    let (x, y) = (i16::try_from(x).unwrap(), i16::try_from(y).unwrap());
    connection
        .warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x, y)
        .unwrap();
    connection.flush().unwrap();
}

/// `CARDCAST_GUARD_DESKTOP=openbox` (CI, with `xdotool`): the pointer pushed onto the card display
/// goes back where it was last seen off it; a mouse moving onto it stops at its edge (the barrier);
/// a pointer another program has grabbed, as a game confining it, is left alone until let go.
#[test]
fn desktop_x11_pointer_guard_puts_the_pointer_back_and_leaves_a_grabbed_one() {
    use x11rb::protocol::xproto::{GrabMode, GrabStatus};
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("openbox") {
        return;
    }
    let monitors = xvfb_halves().unwrap();
    let card = monitor_rect(&monitors[1]);
    let (connection, screen) = x11rb::connect(None).unwrap();
    let root = connection.setup().roots[screen].root;
    let pointer = || x_pointer(&connection, root);
    let off_card = (card.left - 280, 500);
    x_warp(&connection, root, off_card);
    let mut guard = WindowGuard::new(Desktop::X11, xvfb_halves, || {});
    guard.update(true, Some("right"), None);
    wait_for("the guard to start", || {
        guard.take_report() == Some(GuardReport::Clear)
    });
    // Let the pointer guard see the off-card position.
    std::thread::sleep(Duration::from_millis(200));

    let pushed = (card.left + 720, 500);
    x_warp(&connection, root, pushed);
    wait_for("the pointer to go back to where it was", || {
        pointer() == off_card
    });
    eprintln!("pushed to {pushed:?}, put back at {:?}", pointer());

    // A mouse moving right, 100 pixels, from 30 pixels before the card display.
    x_warp(&connection, root, (card.left - 30, 500));
    std::thread::sleep(Duration::from_millis(200));
    let moved = std::process::Command::new("xdotool")
        .args(["mousemove_relative", "--", "100", "0"])
        .status()
        .unwrap();
    assert!(moved.success(), "xdotool failed");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        pointer(),
        (card.left - 1, 500),
        "the mouse did not stop at the card display's edge"
    );

    // Grabbed by another program: the guard leaves it on the card display.
    let (game, _) = x11rb::connect(None).unwrap();
    let grab = game
        .grab_pointer(
            false,
            root,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(grab.status, GrabStatus::SUCCESS);
    x_warp(&connection, root, pushed);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(pointer(), pushed, "the guard moved a grabbed pointer");
    game.ungrab_pointer(x11rb::CURRENT_TIME).unwrap();
    game.flush().unwrap();
    wait_for("the let-go pointer to go back", || {
        pointer() == (card.left - 1, 500)
    });
    eprintln!("let go, put back at {:?}", pointer());
}

/// `CARDCAST_GUARD_DESKTOP=kwin` (CI): the pointer pushed onto the card display with KWin's fake
/// input comes back just across its edge, on the primary monitor, as a probe overlay there sees.
#[test]
fn desktop_kwin_pointer_guard_puts_the_pointer_back() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("kwin") {
        return;
    }
    let monitors = super::monitors().unwrap();
    let card = monitors.iter().find(|m| !m.primary).unwrap();
    let primary = monitors.iter().find(|m| m.primary).unwrap();
    let (card_rect, primary_rect) = (monitor_rect(card), monitor_rect(primary));
    assert_eq!(
        card_rect.left, primary_rect.right,
        "the card display is not right of the primary monitor"
    );
    let (_probe, probed) = super::catcher::probe(primary);
    let mouse = super::catcher::FakeInput::connect().unwrap();
    let mut guard = WindowGuard::new(Desktop::PlasmaWayland, super::monitors, || {});
    guard.update(true, Some(&card.device_path), None);

    let pushed = (card_rect.left + 100, card_rect.top + 100);
    let back = (primary_rect.right - 1, pushed.1);
    // Pushed again until the guard has started, every half second.
    let (mut pushes, mut status) = (0, None);
    wait_for("the pointer to come back from the card display", || {
        if pushes % 25 == 0 {
            mouse.warp(pushed);
        }
        pushes += 1;
        if let Some(report) = guard.take_report() {
            eprintln!("status: {report:?}");
            status = Some(report);
        }
        *probed.lock().unwrap() == Some(back)
    });
    eprintln!("pushed to {pushed:?}, put back at {back:?}");
    assert_eq!(status, Some(GuardReport::Clear));
}
