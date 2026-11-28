//! The window guard on Plasma (Wayland): a KWin script (`kwin.js`), loaded over D-Bus with
//! `org.kde.kwin.Scripting.loadScript`, gets KWin's window events and moves the windows; it asks
//! cardcast, a small service on the session bus, where each goes (docs/research/linux-support.md,
//! section 5; <https://develop.kde.org/docs/plasma/kwin/api/>).
//!
//! The script never outlives cardcast: the guard unloads it when it stops, and the script polls the
//! bus every second and unloads itself once cardcast's connection is gone. The bus closes a
//! process's connection however it ends, a crash or a kill included, and never reuses its name.
//!
//! All of it runs on the guard's own thread, which also watches KWin's name on the bus: when KWin
//! goes away the status line says so, and when it is back the thread loads the script again.

use super::super::placement::{Placement, Rect, WindowState};
use super::window_guard::{Guarding, could_not_start};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const INTERFACE: &str = "io.github.ClearlyNotMich.cardcast.WindowGuard";
const PATH: &str = "/guard";
const KWIN: &str = "org.kde.KWin";
const SCRIPT: &str = include_str!("kwin.js");

/// The status line while KWin is gone, for example restarting after a crash.
pub const WAITING_FOR_KWIN: &str =
    "KWin is not running, so the window guard waits for it to start again.";

/// How often the guard thread looks for KWin on the bus.
const KWIN_CHECK: Duration = Duration::from_secs(1);

/// Guards started in this process, so each script has its own name in KWin.
static STARTED: AtomicU32 = AtomicU32::new(0);

/// The guard thread; dropping it stops the thread, which then unloads the script.
pub struct Running {
    #[cfg(test)]
    plugin: String,
    generation: Arc<AtomicU32>,
    stopped: Arc<AtomicBool>,
    stop: Sender<()>,
}

impl Running {
    #[cfg(test)]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// The guard finds the monitors again and the script checks every window at its next poll,
    /// within a second.
    pub fn follow_displays(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Running {
    /// Never waits: KWin may hang, so the guard thread unloads the script on its own.
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.stop.send(());
    }
}

/// Starts the guard thread; its problems reach the status line through `guarding`.
pub fn start(guarding: Guarding) -> Running {
    let plugin = plugin_name(std::process::id(), STARTED.fetch_add(1, Ordering::Relaxed));
    let (stop, stop_received) = mpsc::channel();
    let service = Service {
        state: Arc::new(Mutex::new(State {
            guarding,
            seen: None,
            sweep: 0,
            ids: HashMap::new(),
            next_id: 0,
            polled: None,
        })),
        generation: Arc::new(AtomicU32::new(0)),
        stopped: Arc::new(AtomicBool::new(false)),
    };
    let running = Running {
        #[cfg(test)]
        plugin: plugin.clone(),
        generation: service.generation.clone(),
        stopped: service.stopped.clone(),
        stop,
    };
    std::thread::spawn(move || run(&plugin, service, &stop_received));
    running
}

fn run(plugin: &str, service: Service, stop: &Receiver<()>) {
    let state = service.state.clone();
    let connection = match zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.serve_at(PATH, service))
        .and_then(zbus::blocking::connection::Builder::build)
    {
        Ok(connection) => connection,
        Err(error) => {
            lock(&state)
                .guarding
                .notice(&could_not_start(&format!("no session bus: {error}")));
            return;
        }
    };
    let name = connection
        .unique_name()
        .map(ToString::to_string)
        .unwrap_or_default();
    let file = script_file(plugin);
    // KWin's name on the bus when the guard last looked: a new name is a restarted KWin.
    let mut kwin_seen: Option<Option<String>> = None;
    loop {
        let kwin = name_owner(&connection, KWIN);
        if kwin_seen.as_ref() != Some(&kwin) {
            if kwin.is_some() {
                if let Err(error) = load(&connection, &file, &name, plugin, &state) {
                    lock(&state).guarding.notice(&could_not_start(&error));
                }
            } else {
                lock(&state).guarding.notice(WAITING_FOR_KWIN);
            }
            kwin_seen = Some(kwin);
        }
        match stop.recv_timeout(KWIN_CHECK) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // If KWin doesn't answer, the script still unloads itself once this connection closes.
    let _ = scripting(&connection, "unloadScript", &plugin);
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The unique name owning `name` on the bus; `None` when nobody does.
fn name_owner(connection: &zbus::blocking::Connection, name: &str) -> Option<String> {
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &name,
        )
        .ok()?
        .body()
        .deserialize()
        .ok()
}

fn scripting<B>(
    connection: &zbus::blocking::Connection,
    method: &str,
    body: &B,
) -> zbus::Result<zbus::Message>
where
    B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
{
    connection.call_method(
        Some(KWIN),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        method,
        body,
    )
}

/// Writes the script, loads and runs it in KWin, and waits for its first poll.
fn load(
    connection: &zbus::blocking::Connection,
    file: &Path,
    name: &str,
    plugin: &str,
    state: &Mutex<State>,
) -> Result<(), String> {
    let (polled, first_poll) = mpsc::channel();
    {
        // A new KWin numbers its windows anew and the script sends the work areas again.
        let state = &mut *lock(state);
        state.polled = Some(polled);
        state.seen = None;
        for (_, id) in state.ids.drain() {
            state.guarding.forget(id);
        }
    }
    std::fs::write(file, script(name, plugin))
        .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
    let loaded = load_and_run(connection, file, plugin, &first_poll);
    // KWin has read the file once the script runs.
    let _ = std::fs::remove_file(file);
    loaded
}

fn load_and_run(
    connection: &zbus::blocking::Connection,
    file: &Path,
    plugin: &str,
    first_poll: &Receiver<()>,
) -> Result<(), String> {
    let not_kwin = |error: zbus::Error| format!("KWin is not answering: {error}");
    let id: i32 = scripting(
        connection,
        "loadScript",
        &(file.to_string_lossy().as_ref(), plugin),
    )
    .and_then(|reply| reply.body().deserialize())
    .map_err(not_kwin)?;
    if id < 0 {
        return Err("KWin did not load the window guard's script".into());
    }
    let _ = connection.call_method(
        Some(KWIN),
        format!("/Scripting/Script{id}").as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    );
    let wait = || first_poll.recv_timeout(Duration::from_secs(2)).is_ok();
    if wait() {
        return Ok(());
    }
    // KWin names a script's D-Bus object by its index, which an older script can still hold; then
    // `run` reached that one. `start` runs every loaded script that isn't running.
    scripting(connection, "start", &()).map_err(not_kwin)?;
    if wait() {
        Ok(())
    } else {
        Err("KWin did not run the window guard's script".into())
    }
}

/// Where the script is written for KWin to read: the user's runtime directory, else the temporary one.
fn script_file(plugin: &str) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join(format!("{plugin}.js"))
}

/// The script's name in KWin: cardcast's process id and the guard's number in it.
pub fn plugin_name(process: u32, guard: u32) -> String {
    format!("cardcast-window-guard-{process}-{guard}")
}

/// The script, told cardcast's bus name, its own name in KWin and the guard's interface.
pub fn script(unique_name: &str, plugin: &str) -> String {
    SCRIPT
        .replace("@SERVICE@", unique_name)
        .replace("@PLUGIN@", plugin)
        .replace("@INTERFACE@", INTERFACE)
}

/// What the script's calls share with the guard thread.
struct State {
    guarding: Guarding,
    /// The display generation and work areas the monitors were last found with.
    seen: Option<(u32, String)>,
    sweep: u32,
    /// KWin's window ids, numbered for the return history.
    ids: HashMap<String, isize>,
    next_id: isize,
    /// Told on the next poll, while the guard thread waits for a newly loaded script.
    polled: Option<Sender<()>>,
}

/// What the script asks cardcast; zbus calls it on its own thread.
struct Service {
    state: Arc<Mutex<State>>,
    generation: Arc<AtomicU32>,
    /// Set when the guard stops: from then on every window stays.
    stopped: Arc<AtomicBool>,
}

#[zbus::interface(name = "io.github.ClearlyNotMich.cardcast.WindowGuard")]
impl Service {
    /// Every second, with each output's work area (see [`work_areas`]); returns the sweep number,
    /// which changes when the script must check every window again.
    fn poll(&self, work_areas: String) -> u32 {
        let state = &mut *lock(&self.state);
        if let Some(polled) = state.polled.take() {
            let _ = polled.send(());
        }
        let seen = (self.generation.load(Ordering::Relaxed), work_areas);
        if state.seen.as_ref() != Some(&seen) && !self.stopped.load(Ordering::Relaxed) {
            let areas = self::work_areas(&seen.1);
            state
                .guarding
                .follow_displays(|monitor| areas.get(&monitor.gdi_name).copied());
            state.seen = Some(seen);
            state.sweep = state.sweep.wrapping_add(1);
        }
        state.sweep
    }

    /// Where window `id` (KWin's) with title `caption` goes; `window` as [`window_state`] reads it,
    /// the answer as [`answer`] writes it.
    fn check(&self, id: String, caption: String, window: String) -> String {
        let Some(window) = window_state(&window) else {
            return "stay".into();
        };
        if self.stopped.load(Ordering::Relaxed) {
            return "stay".into();
        }
        let state = &mut *lock(&self.state);
        let id = *state.ids.entry(id).or_insert_with(|| {
            state.next_id += 1;
            state.next_id
        });
        let placement = state
            .guarding
            .check(id, &caption, window, false, Instant::now());
        answer(
            placement,
            state.guarding.send_windows_to_output().unwrap_or_default(),
        )
    }

    fn forget(&self, id: String) {
        let state = &mut *lock(&self.state);
        if let Some(id) = state.ids.remove(&id) {
            state.guarding.forget(id);
        }
    }
}

/// The script's window: `<left> <top> <right> <bottom> <maximised> <minimised> <tool>`, the flags 0 or 1.
pub fn window_state(text: &str) -> Option<WindowState> {
    let numbers: Vec<i32> = text
        .split(' ')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [left, top, right, bottom, maximised, minimised, tool] =
        <[i32; 7]>::try_from(numbers).ok()?;
    Some(WindowState {
        frame: Rect {
            left,
            top,
            right,
            bottom,
        },
        maximised: maximised != 0,
        minimised: minimised != 0,
        tool: tool != 0,
        ..WindowState::default()
    })
}

/// What the script does: `stay`, `move <rect>`, or `send <output>`: KWin's own move to the
/// "send windows to" monitor's output, which keeps a maximised window maximised with no flicker
/// and moves its restore rectangle along (measured on `kwin_wayland --virtual`).
pub fn answer(placement: Placement, send_windows_to: &str) -> String {
    match placement {
        Placement::MoveTo(r) => format!("move {} {} {} {}", r.left, r.top, r.right, r.bottom),
        Placement::Maximise { .. } => format!("send {send_windows_to}"),
        Placement::Stay | Placement::GiveUp | Placement::Handled => "stay".into(),
    }
}

/// The script's work areas, `<output> <left> <top> <right> <bottom>` joined by `;`, by output name.
pub fn work_areas(text: &str) -> HashMap<String, Rect> {
    text.split(';')
        .filter_map(|area| {
            let mut words = area.split(' ');
            let name = words.next()?.to_owned();
            let numbers: Vec<i32> = words.map(str::parse).collect::<Result<_, _>>().ok()?;
            let [left, top, right, bottom] = <[i32; 4]>::try_from(numbers).ok()?;
            Some((
                name,
                Rect {
                    left,
                    top,
                    right,
                    bottom,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn the_scripts_window_has_a_frame_and_flags() {
        let state = window_state("-100 20 700 620 1 0 1").unwrap();
        assert_eq!(state.frame, rect(-100, 20, 700, 620));
        assert!(state.maximised && !state.minimised && state.tool);
        assert_eq!(window_state("0 0 1 1 0 0"), None);
        assert_eq!(window_state("0 0 1 1 0 0 0 0"), None);
        assert_eq!(window_state("0 0 1.5 1 0 0 0"), None);
    }

    #[test]
    fn the_script_is_told_to_stay_move_or_send_a_maximised_window_to_the_output() {
        let target = rect(100, 100, 500, 400);
        assert_eq!(answer(Placement::Stay, "DP-2"), "stay");
        assert_eq!(answer(Placement::GiveUp, "DP-2"), "stay");
        assert_eq!(
            answer(Placement::MoveTo(target), "DP-2"),
            "move 100 100 500 400"
        );
        let maximise = Placement::Maximise {
            work_area: rect(0, 0, 1920, 1036),
            restore: target,
        };
        assert_eq!(answer(maximise, "DP-2"), "send DP-2");
    }

    #[test]
    fn work_areas_are_found_by_output_name() {
        let areas = work_areas("DP-2 1108 0 3028 1036;HDMI-A-2 3668 1080 5588 2160;bad 1 2");
        assert_eq!(areas.len(), 2);
        assert_eq!(areas["DP-2"], rect(1108, 0, 3028, 1036));
        assert_eq!(areas["HDMI-A-2"], rect(3668, 1080, 5588, 2160));
        assert!(work_areas("").is_empty());
    }

    #[test]
    fn the_script_knows_cardcast_its_own_name_and_the_guard_interface() {
        let plugin = plugin_name(4321, 2);
        assert_eq!(plugin, "cardcast-window-guard-4321-2");
        let script = script(":1.234", &plugin);
        assert!(script.contains(r#"const service = ":1.234";"#));
        assert!(script.contains(r#"const plugin = "cardcast-window-guard-4321-2";"#));
        assert!(script.contains(&format!(r#"const guard = "{INTERFACE}";"#)));
        assert!(!script.contains('@'), "a placeholder is left");
        assert_eq!(
            <Service as zbus::object_server::Interface>::name().as_str(),
            INTERFACE
        );
    }
}
