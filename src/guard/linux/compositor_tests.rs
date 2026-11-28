//! The window guard over a compositor's own interface on a real desktop, run by CI: headless sway
//! (and, where available, headless GNOME Shell with cardcast's extension). The test windows are
//! `foot` terminals, moved through the compositor's own interface as a user would move them.

use super::super::desktop::Desktop;
use super::super::placement::Rect;
use super::super::{Guard, GuardReport, Monitor};
use super::compositor::{self, Compositor, Events};
use super::gnome::{Gnome, MIRROR_APP_ID};
use super::sway::{self, Sway};
use super::window_guard::{Guarding, WindowGuard, monitor_rect};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// A `foot` terminal with its own app id; closed on drop.
struct Foot(std::process::Child);

impl Foot {
    fn open(app_id: &str) -> Self {
        Self(
            std::process::Command::new("foot")
                .args(["--app-id", app_id, "--title", app_id])
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for Foot {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The card display (the monitor that isn't primary) and the primary monitor.
fn card_and_primary() -> (Monitor, Rect) {
    let monitors = super::monitors().unwrap();
    eprintln!("{monitors:#?}");
    let card = monitors.iter().find(|m| !m.primary).unwrap().clone();
    let primary = monitor_rect(monitors.iter().find(|m| m.primary).unwrap());
    (card, primary)
}

fn sway_tree() -> Value {
    sway::request(&mut Sway::commands().unwrap(), sway::GET_TREE, "").unwrap()
}

fn sway_run(command: &str) {
    let reply = sway::request(&mut Sway::commands().unwrap(), sway::RUN_COMMAND, command).unwrap();
    assert_eq!(reply[0]["success"], true, "{command}: {reply}");
}

/// The node in sway's tree that `matches`.
fn sway_node(tree: &Value, matches: &dyn Fn(&Value) -> bool) -> Option<Value> {
    if matches(tree) {
        return Some(tree.clone());
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|key| tree[key].as_array())
        .flatten()
        .find_map(|child| sway_node(child, matches))
}

/// The window with `app_id`: its sway id and frame.
fn sway_window(app_id: &str) -> Option<(i64, Rect)> {
    let node = sway_node(&sway_tree(), &|node| node["app_id"] == app_id)?;
    let id = node["id"].as_i64()?;
    let frame = sway::windows(&node).first()?.state.frame;
    Some((id, frame))
}

fn sway_focused() -> Option<i64> {
    sway_node(&sway_tree(), &|node| node["focused"] == true)?["id"].as_i64()
}

/// `CARDCAST_GUARD_DESKTOP=sway` (CI: headless sway with two outputs, and `foot`): a tiled window
/// on the card display when the guard starts goes to the primary monitor, a floating one moved there
/// while it runs comes back to the same place on the primary monitor, and a fullscreen one comes
/// back tiled, so it doesn't cover the focused window; the focus never moves. Skips otherwise.
#[test]
fn desktop_sway_guard_returns_windows_without_taking_focus() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("sway") {
        return;
    }
    let (card, primary) = card_and_primary();
    let card_rect = monitor_rect(&card);
    let _guarded = Foot::open("cardcast-guard-test");
    wait_for("the guarded window to open", || {
        sway_window("cardcast-guard-test").is_some()
    });
    // Opened last, so sway focuses it.
    let _focused = Foot::open("cardcast-focus-test");
    wait_for("the focused window to open", || {
        sway_window("cardcast-focus-test").is_some()
    });
    let (id, _) = sway_window("cardcast-guard-test").unwrap();
    let frame = |id| {
        sway_node(&sway_tree(), &|node| node["id"] == id)
            .and_then(|node| {
                sway::windows(&node)
                    .first()
                    .map(|window| window.state.frame)
            })
            .unwrap()
    };

    // Tiled on the card display before the guard starts.
    sway_run(&format!(
        "[con_id={id}] move container to output \"{}\"",
        card.gdi_name
    ));
    wait_for("the window to reach the card display", || {
        mostly_on(frame(id), card_rect)
    });
    let focused = sway_focused();
    eprintln!("focused {focused:?}");
    let mut guard = WindowGuard::new(Desktop::Sway, super::monitors, || {});
    guard.update(true, Some(&card.device_path), None);
    wait_for("the tiled window to go to the primary monitor", || {
        mostly_on(frame(id), primary)
    });
    assert_eq!(sway_focused(), focused, "the guard changed the focus");

    // Floating, moved onto the card display while the guard runs.
    sway_run(&format!("[con_id={id}] floating enable"));
    std::thread::sleep(Duration::from_millis(300));
    let size = frame(id);
    let returned = Rect {
        left: primary.left + 100,
        top: primary.top + 100,
        right: primary.left + 100 + size.right - size.left,
        bottom: primary.top + 100 + size.bottom - size.top,
    };
    let focused = sway_focused();
    sway_run(&format!(
        "[con_id={id}] move absolute position {} {}",
        card_rect.left + 100,
        card_rect.top + 100
    ));
    wait_for("the floating window to come back", || {
        let now = frame(id);
        eprintln!("frame {now:?}");
        now == returned
    });
    assert_eq!(sway_focused(), focused, "the guard changed the focus");

    // Fullscreen on the card display: back on the primary monitor, tiled.
    guard.update(false, Some(&card.device_path), None);
    sway_run(&format!(
        "[con_id={id}] floating disable, move container to output \"{}\", fullscreen enable",
        card.gdi_name
    ));
    wait_for("the window to be fullscreen on the card display", || {
        frame(id) == card_rect
    });
    let focused = sway_focused();
    guard.update(true, Some(&card.device_path), None);
    wait_for("the window to be back on the primary monitor", || {
        mostly_on(frame(id), primary)
    });
    assert_eq!(sway_focused(), focused, "the guard changed the focus");
    assert_eq!(guard.take_report(), Some(GuardReport::Clear));
}

/// `CARDCAST_SWAY_START` (CI): a shell command that starts another headless sway in this runtime
/// directory. The sway in `SWAYSOCK` is killed: the status line says the guard waits for it; the new
/// one starts: the guard connects to it and the notice goes. Skips when unset.
#[test]
fn desktop_sway_guard_follows_a_sway_restart() {
    let Ok(start) = std::env::var("CARDCAST_SWAY_START") else {
        return;
    };
    let (sender, reports) = std::sync::mpsc::channel();
    let monitors = || {
        let monitor = |name: &str, left| Monitor {
            device_path: name.into(),
            gdi_name: name.into(),
            name: name.into(),
            width: 1280,
            height: 720,
            layout_size: (1280, 720),
            refresh_hz: 60,
            primary: left == 0,
            left,
            top: 0,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        };
        Ok(vec![monitor("HEADLESS-1", 0), monitor("HEADLESS-2", 1280)])
    };
    let guarding = Guarding::new("HEADLESS-2", None, monitors, sender, Arc::new(|| {}));
    let _running = compositor::start::<Sway>(guarding);
    let mut status = None;
    let mut latest = move || {
        if let Some(report) = reports.try_iter().last() {
            eprintln!("status: {report:?}");
            status = Some(report);
        }
        status.clone()
    };
    let guarding = Some(GuardReport::Clear);
    let waiting = Some(GuardReport::Notice(sway::WAITING_FOR_SWAY.into()));
    wait_for("the guard to start", || latest() == guarding);

    // The sway in this test's SWAYSOCK, `sway-ipc.<uid>.<pid>.sock`, never another.
    let socket = std::env::var("SWAYSOCK").unwrap();
    let pid = socket.rsplit('.').nth(1).unwrap();
    let killed = std::process::Command::new("kill")
        .arg(pid)
        .status()
        .unwrap();
    assert!(killed.success(), "could not stop sway ({pid})");
    wait_for("the notice that sway is gone", || latest() == waiting);

    let started = std::process::Command::new("sh")
        .args(["-c", &format!("{start} &")])
        .status()
        .unwrap();
    assert!(started.success(), "{start} failed");
    wait_for("the guard to connect to the new sway", || {
        latest() == guarding
    });
}

/// The extension's window with title `title`: its id and frame.
fn gnome_window(gnome: &mut Gnome, title: &str) -> Option<(String, Rect)> {
    gnome
        .windows()
        .unwrap()
        .into_iter()
        .find(|window| window.title == title)
        .map(|window| (window.id, window.state.frame))
}

/// `CARDCAST_GUARD_DESKTOP=gnome` (CI: headless GNOME Shell with two virtual monitors, cardcast's
/// extension and `foot`): a window moved onto the card display comes back to the same place on the
/// primary monitor without the focus moving, and a window with the mirror's app id never keeps
/// the focus. Skips otherwise.
#[test]
fn desktop_gnome_extension_returns_windows_and_keeps_the_focus_off_the_mirror() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("gnome") {
        return;
    }
    let (card, primary) = card_and_primary();
    let card_rect = monitor_rect(&card);
    let mut gnome = Gnome::connect(Events::unheard()).unwrap();
    let _guarded = Foot::open("cardcast-guard-test");
    wait_for("the guarded window to open", || {
        gnome_window(&mut gnome, "cardcast-guard-test").is_some()
    });
    let _focused = Foot::open("cardcast-focus-test");
    wait_for("the focused window to open and take the focus", || {
        gnome_window(&mut gnome, "cardcast-focus-test")
            .is_some_and(|(id, _)| gnome.call::<_, String>("Focus", &()).ok() == Some(id))
    });
    let focused: String = gnome.call("Focus", &()).unwrap();
    let (id, size) = gnome_window(&mut gnome, "cardcast-guard-test").unwrap();
    let mut guard = WindowGuard::new(Desktop::GnomeWayland, super::monitors, || {});
    guard.update(true, Some(&card.device_path), None);
    wait_for("the guard to start", || {
        guard.take_report() == Some(GuardReport::Clear)
    });

    let (width, height) = (size.right - size.left, size.bottom - size.top);
    let _: () = gnome
        .call(
            "Move",
            &(
                id.as_str(),
                card_rect.left + 100,
                card_rect.top + 100,
                card_rect.left + 100 + width,
                card_rect.top + 100 + height,
            ),
        )
        .unwrap();
    wait_for("the window to come back to the primary monitor", || {
        let (_, frame) = gnome_window(&mut gnome, "cardcast-guard-test").unwrap();
        eprintln!("frame {frame:?}");
        frame.left == primary.left + 100 && mostly_on(frame, primary)
    });
    assert_eq!(
        gnome.call::<_, String>("Focus", &()).unwrap(),
        focused,
        "the guard changed the focus"
    );

    // A window with the mirror's app id: it opens, and the focus goes straight back.
    let _mirror = Foot::open(MIRROR_APP_ID);
    wait_for("the mirror to open", || {
        gnome
            .windows()
            .unwrap()
            .iter()
            .any(|window| window.state.mirror)
    });
    let opened = Instant::now();
    while opened.elapsed() < Duration::from_secs(3) {
        assert_eq!(
            gnome.call::<_, String>("Focus", &()).unwrap(),
            focused,
            "the mirror kept the focus"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Turns cardcast's extension on or off.
fn gnome_extension(method: &str) {
    let done: bool = zbus::blocking::Connection::session()
        .unwrap()
        .call_method(
            Some("org.gnome.Shell.Extensions"),
            "/org/gnome/Shell/Extensions",
            Some("org.gnome.Shell.Extensions"),
            method,
            &"cardcast@clearlynotmich.github.io",
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(done, "{method} failed");
}

/// `CARDCAST_GUARD_DESKTOP=gnome` (CI): another process on the session bus gets no window list and
/// hears no `Changed` while cardcast does, and cardcast hears at once when the extension goes away
/// (not at its next one-second check). Skips otherwise.
#[test]
fn desktop_gnome_extension_serves_only_cardcast_and_says_when_it_goes() {
    use super::compositor::Message;
    use super::gnome::{NAME, PATH};
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("gnome") {
        return;
    }
    let (events, messages) = Events::listened();
    let _gnome = Gnome::connect(events).unwrap();
    let other = zbus::blocking::Connection::session().unwrap();
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(NAME)
        .and_then(|rule| rule.member("Changed"))
        .unwrap()
        .build();
    let changes = zbus::blocking::MessageIterator::for_match_rule(rule, &other, None).unwrap();
    let (heard, other_heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for _ in changes {
            let _ = heard.send(());
        }
    });
    assert!(
        other
            .call_method(Some(NAME), PATH, Some(NAME), "Windows", &())
            .is_err(),
        "another process got the window list"
    );

    let _window = Foot::open("cardcast-changed-test");
    loop {
        match messages.recv_timeout(Duration::from_secs(20)) {
            Ok(Message::Changed(_)) => break,
            Ok(_) => {}
            Err(error) => panic!("cardcast heard no Changed: {error}"),
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        other_heard.try_recv().is_err(),
        "another process heard Changed"
    );

    gnome_extension("DisableExtension");
    let deadline = Instant::now() + Duration::from_millis(500);
    let gone = loop {
        match messages.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Message::Gone(_)) => break true,
            Ok(_) => {}
            Err(_) => break false,
        }
    };
    gnome_extension("EnableExtension");
    assert!(gone, "cardcast didn't hear the extension go");
}

/// `CARDCAST_GUARD_DESKTOP=gnome` (CI): the extension is turned off, as GNOME does while the screen
/// is locked: the status line says the guard needs it; it is turned on again: the guard is back and
/// the notice goes. Skips otherwise.
#[test]
fn desktop_gnome_guard_follows_the_extension_turned_off_and_on() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("gnome") {
        return;
    }
    let (card, _) = card_and_primary();
    let (sender, reports) = std::sync::mpsc::channel();
    let guarding = Guarding::new(
        &card.device_path,
        None,
        super::monitors,
        sender,
        Arc::new(|| {}),
    );
    let _running = compositor::start::<Gnome>(guarding);
    let mut status = None;
    let mut latest = move || {
        if let Some(report) = reports.try_iter().last() {
            eprintln!("status: {report:?}");
            status = Some(report);
        }
        status.clone()
    };
    let guarding = Some(GuardReport::Clear);
    let waiting = Some(GuardReport::Notice(super::gnome::needs_extension()));
    wait_for("the guard to start", || latest() == guarding);
    gnome_extension("DisableExtension");
    wait_for("the notice that the extension is off", || {
        latest() == waiting
    });
    gnome_extension("EnableExtension");
    wait_for("the guard to be back", || latest() == guarding);
}

/// `CARDCAST_GUARD_DESKTOP=sway` (CI): the pointer pushed onto the card display with a virtual mouse
/// comes back just across its edge, on the primary monitor (`seat - cursor set`), as a probe
/// overlay there sees. Skips otherwise.
#[test]
fn desktop_sway_pointer_guard_puts_the_pointer_back() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("sway") {
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
    let mouse = super::catcher::VirtualPointer::new();
    let desktop = (
        u32::try_from(card_rect.right - primary_rect.left).unwrap(),
        u32::try_from(card_rect.bottom.max(primary_rect.bottom) - primary_rect.top).unwrap(),
    );
    let origin = (primary_rect.left, primary_rect.top);
    let mut guard = WindowGuard::new(Desktop::Sway, super::monitors, || {});
    guard.update(true, Some(&card.device_path), None);

    let pushed = (card_rect.left + 100, card_rect.top + 100);
    let back = (primary_rect.right - 1, pushed.1);
    // Pushed again until the guard has started, every half second.
    let (mut pushes, mut status) = (0, None);
    wait_for("the pointer to come back from the card display", || {
        if pushes % 25 == 0 {
            mouse.move_to(pushed, origin, desktop);
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

/// `CARDCAST_GUARD_DESKTOP=gnome` (CI): the pointer pushed onto the card display comes back just
/// across its edge, on the primary monitor, through the extension. Skips otherwise.
#[test]
fn desktop_gnome_pointer_guard_puts_the_pointer_back() {
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("gnome") {
        return;
    }
    let (card, primary) = card_and_primary();
    let card_rect = monitor_rect(&card);
    assert_eq!(
        card_rect.left, primary.right,
        "the card display is not right of the primary monitor"
    );
    let gnome = Gnome::connect(Events::unheard()).unwrap();
    let start: (i32, i32) = gnome.call("Pointer", &()).unwrap();
    let mut guard = WindowGuard::new(Desktop::GnomeWayland, super::monitors, || {});
    guard.update(true, Some(&card.device_path), None);

    let pushed = (card_rect.left + 100, card_rect.top + 100);
    let back = (primary.right - 1, pushed.1);
    // Pushed again until the guard has started, every half second.
    let (mut pushes, mut status) = (0, None);
    wait_for("the pointer to come back from the card display", || {
        if pushes % 25 == 0 {
            gnome.call::<_, ()>("WarpPointer", &pushed).unwrap();
        }
        pushes += 1;
        if let Some(report) = guard.take_report() {
            eprintln!("status: {report:?}");
            status = Some(report);
        }
        gnome.call::<_, (i32, i32)>("Pointer", &()).unwrap() == back
    });
    eprintln!("pushed to {pushed:?}, put back at {back:?}");
    assert_eq!(status, Some(GuardReport::Clear));
    // Where it was, for the next tests: GNOME opens new windows on the pointer's monitor.
    drop(guard);
    gnome.call::<_, ()>("WarpPointer", &start).unwrap();
}
