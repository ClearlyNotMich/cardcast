//! The window guard over a compositor's own interface: sway's and Hyprland's IPC sockets and
//! cardcast's GNOME Shell extension (docs/research/linux-support.md, section 5). Each says when a
//! window changed; the guard thread then lists every window once things are still for a moment, as
//! on Windows, and once a second in case a move came without an event. The decisions stay in
//! `placement.rs`.
//!
//! When the compositor goes away (a crash, or on GNOME the extension turned off) the status line
//! says so, and the thread connects again every second until it is back.

use super::super::placement::{Placement, Rect, WindowState};
use super::window_guard::Guarding;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// How long things must be still after an event before the guard checks the windows.
const SETTLE: Duration = Duration::from_millis(50);

/// How often the guard checks every window without an event, and tries to reconnect.
const POLL: Duration = Duration::from_secs(1);

/// A window as the compositor lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The compositor's id for it.
    pub id: String,
    pub title: String,
    pub state: WindowState,
    /// Floating windows move to a position; tiled ones only to another output.
    pub floating: bool,
}

/// A compositor the guard talks to.
pub trait Compositor: Sized {
    /// The status line while the compositor can't be reached.
    fn waiting() -> String;
    /// Connects, and starts telling `events` about window changes.
    fn connect(events: Events) -> Result<Self, String>;
    /// Every window; an error once the compositor is gone.
    fn windows(&mut self) -> Result<Vec<Window>, String>;
    /// The output's work area: the output less its panels.
    fn work_area(&mut self, output: &str) -> Option<Rect>;
    /// Whether `apply` can move `window` without taking the focus; if not, the guard gives up on it.
    fn moves(_window: &Window) -> bool {
        true
    }
    /// Carries out `placement` for `window`; `send_windows_to` names the output to send it to.
    fn apply(&mut self, window: &Window, placement: Placement, send_windows_to: &str);
}

pub(super) enum Message {
    /// News from connection number `.0`.
    Changed(u32),
    Gone(u32),
    FollowDisplays,
    Stop,
}

/// How a connection's event reader tells the guard thread; stale connections' news is ignored.
pub struct Events {
    sender: Sender<Message>,
    connection: u32,
}

impl Events {
    /// Events nobody listens to, for tests that only ask and move.
    #[cfg(test)]
    pub fn unheard() -> Self {
        Self {
            sender: mpsc::channel().0,
            connection: 0,
        }
    }

    /// Events the test hears, as connection 0.
    #[cfg(test)]
    pub fn listened() -> (Self, Receiver<Message>) {
        let (sender, received) = mpsc::channel();
        let events = Self {
            sender,
            connection: 0,
        };
        (events, received)
    }

    /// A window changed; false once the guard thread no longer listens.
    pub fn changed(&self) -> bool {
        self.sender.send(Message::Changed(self.connection)).is_ok()
    }

    /// The compositor closed the connection.
    pub fn gone(&self) {
        let _ = self.sender.send(Message::Gone(self.connection));
    }
}

/// The guard thread; dropping it stops the thread.
pub struct Running {
    messages: Sender<Message>,
    stopped: Arc<AtomicBool>,
}

impl Running {
    pub fn follow_displays(&self) {
        let _ = self.messages.send(Message::FollowDisplays);
    }
}

impl Drop for Running {
    /// Never waits on the compositor; the thread moves nothing once `stopped` is set.
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.messages.send(Message::Stop);
    }
}

/// Starts the guard thread for compositor `C`; its problems reach the status line through `guarding`.
pub fn start<C: Compositor + 'static>(guarding: Guarding) -> Running {
    let (messages, received) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let running = Running {
        messages: messages.clone(),
        stopped: stopped.clone(),
    };
    std::thread::spawn(move || run::<C>(guarding, &messages, &received, &stopped));
    running
}

fn run<C: Compositor>(
    mut guarding: Guarding,
    sender: &Sender<Message>,
    messages: &Receiver<Message>,
    stopped: &AtomicBool,
) {
    let mut waiting = false;
    for connection in 0.. {
        let events = Events {
            sender: sender.clone(),
            connection,
        };
        if let Ok(mut compositor) = C::connect(events) {
            waiting = false;
            let mut serving = Serving {
                compositor: &mut compositor,
                guarding: &mut guarding,
                ids: HashMap::new(),
                next_id: 0,
                stopped,
            };
            let gone = serving.serve(connection, messages);
            serving.forget_all();
            if !gone {
                return;
            }
        }
        // While it is away: said once, then tried again every second. A stopped guard says nothing.
        if !waiting && !stopped.load(Ordering::Relaxed) {
            guarding.notice(&C::waiting());
            waiting = true;
        }
        let retry = Instant::now() + POLL;
        loop {
            match messages.recv_timeout(retry.saturating_duration_since(Instant::now())) {
                Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
                // An old connection's news, or a display change, which the next connection follows.
                Ok(_) => {}
            }
        }
    }
}

struct Serving<'a, C> {
    compositor: &'a mut C,
    guarding: &'a mut Guarding,
    /// The compositor's window ids, numbered for the return history.
    ids: HashMap<String, isize>,
    next_id: isize,
    stopped: &'a AtomicBool,
}

impl<C: Compositor> Serving<'_, C> {
    /// Guards until the compositor goes (true) or the guard stops (false).
    fn serve(&mut self, connection: u32, messages: &Receiver<Message>) -> bool {
        self.follow_displays();
        let mut due = Some(Instant::now());
        loop {
            let at = due.unwrap_or_else(|| Instant::now() + POLL);
            match messages.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return false,
                Ok(Message::Gone(from)) if from == connection => return true,
                Ok(Message::Changed(from)) if from == connection => {
                    due.get_or_insert_with(|| Instant::now() + SETTLE);
                }
                Ok(Message::FollowDisplays) => {
                    self.follow_displays();
                    due = Some(Instant::now());
                }
                Ok(Message::Changed(_) | Message::Gone(_)) => {}
                Err(RecvTimeoutError::Timeout) => {
                    due = None;
                    if self.check_all().is_err() {
                        return true;
                    }
                }
            }
        }
    }

    fn follow_displays(&mut self) {
        let compositor = &mut *self.compositor;
        self.guarding
            .follow_displays(|monitor| compositor.work_area(&monitor.gdi_name));
    }

    fn check_all(&mut self) -> Result<(), String> {
        let windows = self.compositor.windows()?;
        let now = Instant::now();
        for window in &windows {
            if self.stopped.load(Ordering::Relaxed) {
                return Ok(());
            }
            let id = *self.ids.entry(window.id.clone()).or_insert_with(|| {
                self.next_id += 1;
                self.next_id
            });
            let moves = C::moves(window);
            if moves {
                self.guarding.movable_again(id);
            }
            let placement = self
                .guarding
                .check(id, &window.title, window.state, false, now);
            if !matches!(placement, Placement::MoveTo(_) | Placement::Maximise { .. }) {
                continue;
            }
            if moves {
                let output = self
                    .guarding
                    .send_windows_to_output()
                    .unwrap_or_default()
                    .to_owned();
                self.compositor.apply(window, placement, &output);
            } else {
                self.guarding.give_up_fullscreen(id);
            }
        }
        let closed: Vec<String> = self
            .ids
            .keys()
            .filter(|id| !windows.iter().any(|window| window.id == **id))
            .cloned()
            .collect();
        for id in closed {
            if let Some(id) = self.ids.remove(&id) {
                self.guarding.forget(id);
            }
        }
        Ok(())
    }

    /// A new connection may number its windows anew.
    fn forget_all(&mut self) {
        for (_, id) in self.ids.drain() {
            self.guarding.forget(id);
        }
    }
}

/// `first`, then the other entries of its directory whose names `matches`, newest first: where a
/// restarted compositor's socket is. A crashed compositor leaves its socket behind, which refuses
/// connections.
pub fn candidates(
    first: &std::path::Path,
    matches: impl Fn(&str) -> bool,
) -> Vec<std::path::PathBuf> {
    let mut others: Vec<(std::time::SystemTime, std::path::PathBuf)> = first
        .parent()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_str().is_some_and(&matches))
        .map(|entry| entry.path())
        .filter(|path| path != first)
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, path)
        })
        .collect();
    others.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    std::iter::once(first.to_owned())
        .chain(others.into_iter().map(|(_, path)| path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::GuardReport;
    use crate::guard::Monitor;
    use std::sync::Mutex;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn monitors() -> Result<Vec<Monitor>, String> {
        let monitor = |name: &str, left: i32| Monitor {
            device_path: name.into(),
            gdi_name: name.into(),
            name: name.into(),
            width: 1920,
            height: 1080,
            layout_size: (1920, 1080),
            refresh_hz: 60,
            primary: left == 0,
            left,
            top: 0,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        };
        Ok(vec![monitor("DP-1", 0), monitor("HDMI-A-1", 1920)])
    }

    /// What the fake compositor shares with the test.
    #[derive(Default)]
    struct Shared {
        windows: Vec<Window>,
        applied: Vec<(String, Placement, String)>,
        up: bool,
        connects: u32,
        events: Option<Events>,
    }

    static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

    fn shared<T>(f: impl FnOnce(&mut Shared) -> T) -> T {
        f(SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(Shared::default))
    }

    struct Fake;

    impl Compositor for Fake {
        fn waiting() -> String {
            "The fake compositor is not running.".into()
        }
        fn connect(events: Events) -> Result<Self, String> {
            shared(|shared| {
                if !shared.up {
                    return Err("down".into());
                }
                shared.connects += 1;
                shared.events = Some(events);
                Ok(Fake)
            })
        }
        fn windows(&mut self) -> Result<Vec<Window>, String> {
            shared(|shared| {
                if shared.up {
                    Ok(shared.windows.clone())
                } else {
                    Err("gone".into())
                }
            })
        }
        fn moves(window: &Window) -> bool {
            !window.state.maximised
        }
        fn work_area(&mut self, output: &str) -> Option<Rect> {
            (output == "DP-1").then(|| rect(0, 30, 1920, 1080))
        }
        fn apply(&mut self, window: &Window, placement: Placement, send_windows_to: &str) {
            shared(|shared| {
                shared
                    .applied
                    .push((window.id.clone(), placement, send_windows_to.into()));
            });
        }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_window_on_the_card_display_is_returned_and_the_compositor_going_away_is_said_and_followed()
    {
        // One the fake can't move: fullscreen, as on Hyprland.
        let fullscreen = Window {
            id: "f".into(),
            title: "Game".into(),
            state: WindowState {
                frame: rect(1920, 0, 3840, 1080),
                maximised: true,
                ..WindowState::default()
            },
            floating: false,
        };
        let on_card = Window {
            id: "a".into(),
            title: "Kate".into(),
            state: WindowState {
                frame: rect(2020, 100, 2420, 400),
                ..WindowState::default()
            },
            floating: true,
        };
        shared(|shared| {
            shared.up = false;
            shared.windows = vec![on_card.clone(), fullscreen];
        });
        let (reports, received) = mpsc::channel();
        let guarding = Guarding::new("HDMI-A-1", None, monitors, reports, Arc::new(|| {}));
        let running = start::<Fake>(guarding);
        let waiting = GuardReport::Notice(Fake::waiting());
        let mut latest = None;
        let mut status = move || {
            if let Some(report) = received.try_iter().last() {
                latest = Some(report);
            }
            latest.clone()
        };
        wait_for("the waiting notice", || status() == Some(waiting.clone()));

        // It comes up: the window goes to the work area, at the same place.
        shared(|shared| shared.up = true);
        wait_for("the window to be returned", || {
            shared(|shared| !shared.applied.is_empty())
        });
        assert_eq!(
            shared(|shared| shared.applied[0].clone()),
            (
                "a".into(),
                // The same place below the work area's top.
                Placement::MoveTo(rect(100, 130, 500, 430)),
                "DP-1".into()
            )
        );
        let left_alone = GuardReport::Notice(
            "\"Game\" is fullscreen on the card display; cardcast can't move it without taking focus. Take it out of fullscreen or move it off to guard it again.".into(),
        );
        wait_for("the fullscreen window to be named", || {
            status() == Some(left_alone.clone())
        });
        assert!(shared(|shared| shared
            .applied
            .iter()
            .all(|(id, ..)| id == "a")));

        // It goes away: said, and back on the next connection.
        shared(|shared| {
            shared.up = false;
            if let Some(events) = &shared.events {
                events.gone();
            }
        });
        wait_for("the waiting notice again", || {
            status() == Some(waiting.clone())
        });
        shared(|shared| shared.up = true);
        wait_for("a new connection", || shared(|shared| shared.connects >= 2));
        drop(running);
    }

    #[test]
    fn a_restarted_compositors_socket_is_found_beside_the_old_one() {
        let dir = std::env::temp_dir().join(format!("cardcast-candidates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["sway-ipc.1000.10.sock", "sway-ipc.1000.20.sock", "other"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let first = dir.join("sway-ipc.1000.10.sock");
        let found = candidates(&first, |name| name.starts_with("sway-ipc."));
        assert_eq!(found, [first, dir.join("sway-ipc.1000.20.sock")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
