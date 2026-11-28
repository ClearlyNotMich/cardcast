//! The window guard on Hyprland, over its IPC sockets (<https://wiki.hypr.land/IPC/>): events on
//! `.socket2.sock` say when to look, and one request per connection on `.socket.sock` lists the
//! windows (`j/clients`) and moves them with the `movewindowpixel` and `movetoworkspacesilent`
//! dispatchers, which leave the focus alone (docs/research/linux-support.md, section 5). Hyprland's
//! fullscreen dispatchers act on the focused window only, so a fullscreen window on the card display
//! is left there and named in the status line. Hyprland freezes while a request socket stays open,
//! so each request has a timeout and is closed at once.

use super::super::placement::{Placement, Rect, WindowState};
use super::compositor::{Compositor, Events, Window};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The status line while Hyprland's socket doesn't answer.
pub const WAITING_FOR_HYPRLAND: &str =
    "Hyprland is not answering, so the window guard waits for it to start again.";

pub struct Hyprland {
    /// The instance's socket directory.
    directory: PathBuf,
    /// The event socket, shut down to end its reader.
    events: UnixStream,
}

impl Drop for Hyprland {
    fn drop(&mut self) {
        let _ = self.events.shutdown(std::net::Shutdown::Both);
    }
}

/// This instance's socket directory, `$XDG_RUNTIME_DIR/hypr/<signature>`, else Hyprland's older `/tmp/hypr`.
fn directory() -> Result<PathBuf, String> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
        .ok_or("HYPRLAND_INSTANCE_SIGNATURE is not set")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|runtime| Path::new(&runtime).join("hypr").join(&signature))
        .filter(|directory| directory.exists());
    Ok(runtime.unwrap_or_else(|| Path::new("/tmp/hypr").join(signature)))
}

/// Sends `what` on the request socket in `directory` and returns Hyprland's reply.
pub fn request(directory: &Path, what: &str) -> Result<String, String> {
    let run = || -> std::io::Result<String> {
        let mut stream = UnixStream::connect(directory.join(".socket.sock"))?;
        let timeout = Some(Duration::from_secs(2));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        stream.write_all(what.as_bytes())?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        Ok(reply)
    };
    run().map_err(|error| format!("Hyprland did not answer {what}: {error}"))
}

fn json(directory: &Path, what: &str) -> Result<Value, String> {
    serde_json::from_str(&request(directory, what)?).map_err(|error| error.to_string())
}

impl Compositor for Hyprland {
    fn waiting() -> String {
        WAITING_FOR_HYPRLAND.into()
    }

    fn connect(events: Events) -> Result<Self, String> {
        // Only this session's instance: never another one's directory, such as a nested Hyprland.
        let directory = directory()?;
        let stream = UnixStream::connect(directory.join(".socket2.sock"))
            .map_err(|error| format!("no Hyprland at {}: {error}", directory.display()))?;
        let reader = stream.try_clone().map_err(|error| error.to_string())?;
        std::thread::spawn(move || {
            // One `event>>data` line per event; any of them may have moved a window.
            for _ in BufReader::new(reader).lines().map_while(Result::ok) {
                if !events.changed() {
                    return;
                }
            }
            events.gone();
        });
        Ok(Self {
            directory,
            events: stream,
        })
    }

    fn windows(&mut self) -> Result<Vec<Window>, String> {
        windows(&json(&self.directory, "j/clients")?)
    }

    fn work_area(&mut self, output: &str) -> Option<Rect> {
        work_area(&json(&self.directory, "j/monitors").ok()?, output)
    }

    fn moves(window: &Window) -> bool {
        moves(window)
    }

    fn apply(&mut self, window: &Window, placement: Placement, send_windows_to: &str) {
        let Ok(monitors) = json(&self.directory, "j/monitors") else {
            return;
        };
        if let Some(dispatch) = dispatch(window, placement, &monitors, send_windows_to) {
            let _ = request(&self.directory, &dispatch);
        }
    }
}

fn number(value: &Value) -> i32 {
    value
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .unwrap_or(0)
}

/// `pixels` at `scale`, in the desktop's coordinates.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to i32's range first"
)]
fn logical(pixels: f64, scale: f64) -> i32 {
    (pixels / scale)
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// The windows in `j/clients`' reply, in Hyprland's layout coordinates. Unmapped and hidden ones
/// (a group's background tabs) and those on special workspaces (the scratchpad, id below 0) count
/// as hidden; a fullscreen window (`fullscreen` a number, or a boolean before Hyprland 0.42) as maximised.
pub fn windows(clients: &Value) -> Result<Vec<Window>, String> {
    let clients = clients.as_array().ok_or("j/clients is not a list")?;
    Ok(clients
        .iter()
        .filter_map(|client| {
            let left = number(&client["at"][0]);
            let top = number(&client["at"][1]);
            let fullscreen = &client["fullscreen"];
            Some(Window {
                id: client["address"].as_str()?.to_owned(),
                title: client["title"].as_str().unwrap_or_default().to_owned(),
                state: WindowState {
                    frame: Rect {
                        left,
                        top,
                        right: left.saturating_add(number(&client["size"][0])),
                        bottom: top.saturating_add(number(&client["size"][1])),
                    },
                    maximised: fullscreen == true
                        || fullscreen.as_i64().is_some_and(|mode| mode != 0),
                    hidden: client["mapped"] == false
                        || client["hidden"] == true
                        || client["workspace"]["id"].as_i64().is_some_and(|id| id < 0),
                    ..WindowState::default()
                },
                floating: client["floating"] == true,
            })
        })
        .collect())
}

/// The monitor's work area from `j/monitors`' reply: its size on the desktop (its mode over its
/// scale, turned with it) less what its bars reserve, `[left, top, right, bottom]`.
pub fn work_area(monitors: &Value, output: &str) -> Option<Rect> {
    let monitor = monitors
        .as_array()?
        .iter()
        .find(|monitor| monitor["name"] == output)?;
    let scale = monitor["scale"].as_f64().filter(|scale| *scale > 0.0)?;
    let mut width = logical(monitor["width"].as_f64()?, scale);
    let mut height = logical(monitor["height"].as_f64()?, scale);
    // Transforms 1, 3, 5 and 7 turn the monitor a quarter.
    if monitor["transform"]
        .as_i64()
        .is_some_and(|transform| transform % 2 == 1)
    {
        (width, height) = (height, width);
    }
    let reserved = |at: usize| number(&monitor["reserved"][at]);
    let (left, top) = (number(&monitor["x"]), number(&monitor["y"]));
    Some(Rect {
        left: left + reserved(0),
        top: top + reserved(1),
        right: left + width - reserved(2),
        bottom: top + height - reserved(3),
    })
}

/// Whether the guard can move `window`: not a fullscreen one, which would stay fullscreen on the
/// workspace it goes to, over the game; leaving fullscreen needs the focus.
pub fn moves(window: &Window) -> bool {
    !window.state.maximised
}

/// The dispatch that carries out `placement`: a floating window moves to its new position, a
/// tiled one to the active workspace of the "send windows to" monitor, silently (without following
/// it with the focus).
pub fn dispatch(
    window: &Window,
    placement: Placement,
    monitors: &Value,
    send_windows_to: &str,
) -> Option<String> {
    match placement {
        Placement::MoveTo(target) if window.floating => Some(format!(
            "dispatch movewindowpixel exact {} {},address:{}",
            target.left, target.top, window.id
        )),
        _ => {
            let workspace = monitors
                .as_array()?
                .iter()
                .find(|monitor| monitor["name"] == send_windows_to)?["activeWorkspace"]["id"]
                .as_i64()?;
            Some(format!(
                "dispatch movetoworkspacesilent {workspace},address:{}",
                window.id
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `j/clients` and `j/monitors` as documented on <https://wiki.hypr.land/IPC/> and printed by
    /// `hyprctl -j` (Hyprland 0.4x), trimmed: a tiled terminal on DP-1, a floating window on the
    /// card display HDMI-A-1, a fullscreen game and a scratchpad window.
    const CLIENTS: &str = r#"[
        {"address": "0x55d1c0a1b2c0", "mapped": true, "hidden": false, "at": [10, 40], "size": [1900, 1030],
         "workspace": {"id": 1, "name": "1"}, "floating": false, "pseudo": false, "monitor": 0,
         "class": "kitty", "title": "~", "pid": 1234, "xwayland": false, "pinned": false,
         "fullscreen": 0, "fullscreenClient": 0, "grouped": [], "tags": [], "swallowing": "0x0", "focusHistoryID": 0},
        {"address": "0x55d1c0a1b3d0", "mapped": true, "hidden": false, "at": [2020, 100], "size": [400, 300],
         "workspace": {"id": 2, "name": "2"}, "floating": true, "monitor": 1, "class": "pavucontrol",
         "title": "Volume Control", "pid": 1300, "fullscreen": 0, "focusHistoryID": 1},
        {"address": "0x55d1c0a1b4e0", "mapped": true, "hidden": false, "at": [1920, 0], "size": [1920, 1080],
         "workspace": {"id": 2, "name": "2"}, "floating": false, "monitor": 1, "class": "game",
         "title": "Game", "pid": 1400, "fullscreen": 2, "focusHistoryID": 2},
        {"address": "0x55d1c0a1b5f0", "mapped": true, "hidden": false, "at": [2120, 200], "size": [800, 600],
         "workspace": {"id": -98, "name": "special:magic"}, "floating": true, "monitor": 1,
         "class": "kitty", "title": "scratch", "pid": 1500, "fullscreen": false, "focusHistoryID": 3}
    ]"#;

    const MONITORS: &str = r#"[
        {"id": 0, "name": "DP-1", "description": "Dell Inc. AW2523HF", "make": "Dell Inc.", "model": "AW2523HF",
         "width": 1920, "height": 1080, "refreshRate": 500.0, "x": 0, "y": 0,
         "activeWorkspace": {"id": 1, "name": "1"}, "specialWorkspace": {"id": 0, "name": ""},
         "reserved": [0, 30, 0, 0], "scale": 1.0, "transform": 0, "focused": true, "dpmsStatus": true, "vrr": false},
        {"id": 1, "name": "HDMI-A-1", "description": "Elgato HD60 X", "width": 3840, "height": 2160,
         "refreshRate": 60.0, "x": 1920, "y": 0, "activeWorkspace": {"id": 2, "name": "2"},
         "reserved": [0, 0, 0, 0], "scale": 2.0, "transform": 1, "focused": false}
    ]"#;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn clients_give_each_window_its_frame_and_flags() {
        let found = windows(&serde_json::from_str(CLIENTS).unwrap()).unwrap();
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].id, "0x55d1c0a1b2c0");
        assert_eq!(found[0].state.frame, rect(10, 40, 1910, 1070));
        assert!(!found[0].floating && !found[0].state.maximised && !found[0].state.hidden);
        assert_eq!(found[1].title, "Volume Control");
        assert_eq!(found[1].state.frame, rect(2020, 100, 2420, 400));
        assert!(found[1].floating);
        assert!(found[2].state.maximised);
        assert!(found[3].state.hidden && !found[3].state.maximised);
        assert!(windows(&serde_json::json!({"error": 1})).is_err());
    }

    #[test]
    fn the_work_area_is_the_monitor_on_the_desktop_less_its_bars() {
        let monitors = serde_json::from_str(MONITORS).unwrap();
        assert_eq!(work_area(&monitors, "DP-1"), Some(rect(0, 30, 1920, 1080)));
        // 3840x2160 at scale 2, turned a quarter.
        assert_eq!(
            work_area(&monitors, "HDMI-A-1"),
            Some(rect(1920, 0, 3000, 1920))
        );
        assert_eq!(work_area(&monitors, "DP-2"), None);
    }

    #[test]
    fn a_fullscreen_window_is_left_alone() {
        let found = windows(&serde_json::from_str(CLIENTS).unwrap()).unwrap();
        assert!(moves(&found[0]) && moves(&found[1]));
        assert!(!moves(&found[2]));
    }

    #[test]
    fn floating_windows_move_and_others_go_to_the_monitors_workspace_silently() {
        let monitors: Value = serde_json::from_str(MONITORS).unwrap();
        let found = windows(&serde_json::from_str(CLIENTS).unwrap()).unwrap();
        let target = Placement::MoveTo(rect(100, 100, 500, 400));
        assert_eq!(
            dispatch(&found[1], target, &monitors, "DP-1").as_deref(),
            Some("dispatch movewindowpixel exact 100 100,address:0x55d1c0a1b3d0")
        );
        assert_eq!(
            dispatch(&found[0], target, &monitors, "DP-1").as_deref(),
            Some("dispatch movetoworkspacesilent 1,address:0x55d1c0a1b2c0")
        );
        assert_eq!(dispatch(&found[0], target, &monitors, "DP-9"), None);
    }

    #[test]
    fn a_request_is_written_once_and_the_reply_read_to_its_end() {
        let directory = std::env::temp_dir().join(format!("cardcast-hypr-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(directory.join(".socket.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut asked = [0; 9];
            stream.read_exact(&mut asked).unwrap();
            stream.write_all(b"[]").unwrap();
            asked
        });
        assert_eq!(request(&directory, "j/clients").unwrap(), "[]");
        assert_eq!(&server.join().unwrap(), b"j/clients");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
