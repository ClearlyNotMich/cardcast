//! The window guard on Plasma (Wayland): a KWin script (`kwin.js`), loaded over D-Bus with
//! `org.kde.kwin.Scripting.loadScript`, gets KWin's window events and moves the windows; it asks
//! cardcast, a small service on the session bus, where each goes (docs/research/linux-support.md,
//! section 5; <https://develop.kde.org/docs/plasma/kwin/api/>).
//!
//! The script never outlives cardcast: the guard unloads it when it stops, and the script polls the
//! bus every second and unloads itself once cardcast's connection is gone. The bus closes a
//! process's connection however it ends, a crash or a kill included, and never reuses its name.

use super::super::placement::{Placement, Rect, WindowState};
use super::window_guard::Guarding;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const INTERFACE: &str = "io.github.ClearlyNotMich.cardcast.WindowGuard";
const PATH: &str = "/guard";
const KWIN: &str = "org.kde.KWin";
const SCRIPT: &str = include_str!("kwin.js");

/// The loaded script and cardcast's connection; dropping it unloads the script.
pub struct Running {
    connection: zbus::blocking::Connection,
    plugin: String,
    generation: Arc<AtomicU32>,
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
    fn drop(&mut self) {
        // If KWin doesn't answer, the script still unloads itself once this connection closes.
        let _ = scripting(&self.connection, "unloadScript", &self.plugin);
    }
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

/// Serves the guard on the session bus and loads the script into KWin; errors are plain words.
pub fn start(guarding: Guarding) -> Result<Running, String> {
    let generation = Arc::new(AtomicU32::new(0));
    let (polled, first_poll) = mpsc::channel();
    let service = Service {
        guarding,
        generation: generation.clone(),
        polled: Some(polled),
        seen: None,
        sweep: 0,
        ids: HashMap::new(),
        next_id: 0,
    };
    let connection = zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.serve_at(PATH, service))
        .and_then(zbus::blocking::connection::Builder::build)
        .map_err(|error| format!("no session bus: {error}"))?;
    let name = connection
        .unique_name()
        .ok_or("no name on the session bus")?
        .to_string();
    let running = Running {
        connection,
        plugin: plugin_name(&name),
        generation,
    };
    let file = script_file(&running.plugin);
    std::fs::write(&file, script(&name, &running.plugin))
        .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
    let loaded = load(&running, &file, &first_poll);
    // KWin has read the file once the script runs.
    let _ = std::fs::remove_file(&file);
    loaded.map(|()| running)
}

/// Loads and runs the script, and waits for its first poll.
fn load(running: &Running, file: &Path, first_poll: &Receiver<()>) -> Result<(), String> {
    let not_kwin = |error: zbus::Error| format!("KWin is not answering: {error}");
    let id: i32 = scripting(
        &running.connection,
        "loadScript",
        &(file.to_string_lossy().as_ref(), running.plugin.as_str()),
    )
    .and_then(|reply| reply.body().deserialize())
    .map_err(not_kwin)?;
    if id < 0 {
        return Err("KWin did not load the window guard's script".into());
    }
    let _ = running.connection.call_method(
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
    scripting(&running.connection, "start", &()).map_err(not_kwin)?;
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

/// The script's name in KWin, unique to this connection (`:1.234` is `cardcast-window-guard-1-234`).
pub fn plugin_name(unique_name: &str) -> String {
    format!(
        "cardcast-window-guard-{}",
        unique_name.trim_start_matches(':').replace('.', "-")
    )
}

/// The script, told cardcast's bus name, its own name in KWin and the guard's interface.
pub fn script(unique_name: &str, plugin: &str) -> String {
    SCRIPT
        .replace("@SERVICE@", unique_name)
        .replace("@PLUGIN@", plugin)
        .replace("@INTERFACE@", INTERFACE)
}

/// What the script asks cardcast; zbus calls it on its own thread.
struct Service {
    guarding: Guarding,
    generation: Arc<AtomicU32>,
    /// Told on the first poll, so `start` knows the script runs.
    polled: Option<Sender<()>>,
    /// The display generation and work areas the monitors were last found with.
    seen: Option<(u32, String)>,
    sweep: u32,
    /// KWin's window ids, numbered for the return history.
    ids: HashMap<String, isize>,
    next_id: isize,
}

#[zbus::interface(name = "io.github.ClearlyNotMich.cardcast.WindowGuard")]
impl Service {
    /// Every second, with each output's work area (see [`work_areas`]); returns the sweep number,
    /// which changes when the script must check every window again.
    fn poll(&mut self, work_areas: String) -> u32 {
        if let Some(polled) = self.polled.take() {
            let _ = polled.send(());
        }
        let seen = (self.generation.load(Ordering::Relaxed), work_areas);
        if self.seen.as_ref() != Some(&seen) {
            let areas = self::work_areas(&seen.1);
            self.guarding
                .follow_displays(|monitor| areas.get(&monitor.gdi_name).copied());
            self.seen = Some(seen);
            self.sweep = self.sweep.wrapping_add(1);
        }
        self.sweep
    }

    /// Where window `id` (KWin's) with title `caption` goes; `window` as [`window_state`] reads it,
    /// the answer as [`answer`] writes it.
    fn check(&mut self, id: String, caption: String, window: String) -> String {
        let Some((state, remaximising)) = window_state(&window) else {
            return "stay".into();
        };
        let id = *self.ids.entry(id).or_insert_with(|| {
            self.next_id += 1;
            self.next_id
        });
        let placement = self
            .guarding
            .check(id, &caption, state, remaximising, Instant::now());
        answer(placement, remaximising)
    }

    fn forget(&mut self, id: String) {
        if let Some(id) = self.ids.remove(&id) {
            self.guarding.forget(id);
        }
    }
}

/// The script's window: `<left> <top> <right> <bottom> <maximised> <minimised> <tool>
/// <remaximising>`, the flags 0 or 1; with whether the guard is maximising it again.
pub fn window_state(text: &str) -> Option<(WindowState, bool)> {
    let numbers: Vec<i32> = text
        .split(' ')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [
        left,
        top,
        right,
        bottom,
        maximised,
        minimised,
        tool,
        remaximising,
    ] = <[i32; 8]>::try_from(numbers).ok()?;
    let state = WindowState {
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
    };
    Some((state, remaximising != 0))
}

/// What the script does: `stay`, `move <rect>`, `unmaximise` to see where the window restores to,
/// or `maximise <rect>`: move the restored window there and maximise it (see `Guarding::check`).
pub fn answer(placement: Placement, remaximising: bool) -> String {
    let rect = |r: Rect| format!("{} {} {} {}", r.left, r.top, r.right, r.bottom);
    match placement {
        Placement::MoveTo(target) => format!("move {}", rect(target)),
        Placement::Maximise { .. } if !remaximising => "unmaximise".into(),
        Placement::Maximise { restore, .. } => format!("maximise {}", rect(restore)),
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
        let (state, remaximising) = window_state("-100 20 700 620 1 0 1 0").unwrap();
        assert_eq!(state.frame, rect(-100, 20, 700, 620));
        assert!(state.maximised && !state.minimised && state.tool && !remaximising);
        assert!(window_state("0 0 1 1 0 0 0 1").unwrap().1);
        assert_eq!(window_state("0 0 1 1 0 0 0"), None);
        assert_eq!(window_state("0 0 1.5 1 0 0 0 0"), None);
    }

    #[test]
    fn the_script_is_told_to_stay_move_or_unmaximise_then_maximise() {
        let target = rect(100, 100, 500, 400);
        assert_eq!(answer(Placement::Stay, false), "stay");
        assert_eq!(answer(Placement::GiveUp, false), "stay");
        assert_eq!(
            answer(Placement::MoveTo(target), false),
            "move 100 100 500 400"
        );
        let maximise = Placement::Maximise {
            work_area: rect(0, 0, 1920, 1036),
            restore: target,
        };
        assert_eq!(answer(maximise, false), "unmaximise");
        assert_eq!(answer(maximise, true), "maximise 100 100 500 400");
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
        let plugin = plugin_name(":1.234");
        assert_eq!(plugin, "cardcast-window-guard-1-234");
        let script = script(":1.234", &plugin);
        assert!(script.contains(r#"const service = ":1.234";"#));
        assert!(script.contains(r#"const plugin = "cardcast-window-guard-1-234";"#));
        assert!(script.contains(&format!(r#"const guard = "{INTERFACE}";"#)));
        assert!(!script.contains('@'), "a placeholder is left");
        assert_eq!(
            <Service as zbus::object_server::Interface>::name().as_str(),
            INTERFACE
        );
    }
}
