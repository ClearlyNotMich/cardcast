//! The window guard on sway, over its IPC socket (`SWAYSOCK`, the i3 protocol,
//! <https://man.archlinux.org/man/sway-ipc.7>): `window`, `workspace` and `output` events say when
//! to look, `get_tree` lists the windows, and commands with `[con_id=N]` criteria move them without
//! focusing them (docs/research/linux-support.md, section 5). The pointer guard moves the pointer
//! with `seat - cursor set` (sway-input(5)), from its own socket on the overlay's thread
//! (`catcher.rs`).

use super::super::placement::{Placement, Rect, WindowState};
use super::catcher::{self, Catcher};
use super::compositor::{Compositor, Events, Window, candidates};
use super::window_guard::PointerArea;
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
pub const GET_TREE: u32 = 4;

/// The status line while sway's socket doesn't answer.
pub const WAITING_FOR_SWAY: &str =
    "sway is not answering, so the window guard waits for it to start again.";

pub struct Sway {
    commands: UnixStream,
    /// The subscribed connection, shut down to end its reader.
    events: UnixStream,
    pointer: Option<Catcher>,
}

impl Drop for Sway {
    fn drop(&mut self) {
        let _ = self.events.shutdown(std::net::Shutdown::Both);
    }
}

/// The live sway socket: `SWAYSOCK`, else a restarted sway's beside it.
fn connect_socket() -> Result<UnixStream, String> {
    let path = PathBuf::from(std::env::var_os("SWAYSOCK").ok_or("SWAYSOCK is not set")?);
    candidates(&path, |name| name.starts_with("sway-ipc."))
        .iter()
        .find_map(|path| UnixStream::connect(path).ok())
        .ok_or_else(|| format!("no sway at {}", path.display()))
}

impl Sway {
    /// Connects to sway's socket, without events; the CI tests use it to look and move too.
    pub fn commands() -> Result<UnixStream, String> {
        let stream = connect_socket()?;
        // A hung sway must not hold the guard thread for ever.
        let timeout = Some(Duration::from_secs(2));
        stream
            .set_read_timeout(timeout)
            .and_then(|()| stream.set_write_timeout(timeout))
            .map_err(|error| error.to_string())?;
        Ok(stream)
    }
}

/// Sends message `kind` with `payload` and returns sway's reply.
pub fn request(stream: &mut UnixStream, kind: u32, payload: &str) -> Result<Value, String> {
    send(stream, kind, payload).map_err(|error| error.to_string())?;
    let (_, reply) = receive(stream).map_err(|error| error.to_string())?;
    serde_json::from_slice(&reply).map_err(|error| error.to_string())
}

fn send(stream: &mut UnixStream, kind: u32, payload: &str) -> std::io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(std::io::Error::other)?;
    let mut message = b"i3-ipc".to_vec();
    message.extend_from_slice(&length.to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(payload.as_bytes());
    stream.write_all(&message)
}

/// One message: its type and payload.
fn receive(stream: &mut UnixStream) -> std::io::Result<(u32, Vec<u8>)> {
    let mut header = [0; 14];
    stream.read_exact(&mut header)?;
    let (magic, numbers) = header.split_at(6);
    if magic != b"i3-ipc" {
        return Err(std::io::Error::other("not sway's protocol"));
    }
    let number = |at: usize| {
        numbers
            .get(at..at + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map_or(0, u32::from_ne_bytes)
    };
    let mut payload = vec![0; usize::try_from(number(0)).unwrap_or(0)];
    stream.read_exact(&mut payload)?;
    Ok((number(4), payload))
}

impl Compositor for Sway {
    fn waiting() -> String {
        WAITING_FOR_SWAY.into()
    }

    fn connect(events: Events) -> Result<Self, String> {
        let commands = Self::commands()?;
        let mut subscribed = connect_socket()?;
        let reply = request(
            &mut subscribed,
            SUBSCRIBE,
            r#"["window","workspace","output"]"#,
        )?;
        if reply["success"] != true {
            return Err(format!("sway refused the subscription: {reply}"));
        }
        let mut reader = subscribed.try_clone().map_err(|error| error.to_string())?;
        std::thread::spawn(move || {
            while receive(&mut reader).is_ok() {
                if !events.changed() {
                    return;
                }
            }
            events.gone();
        });
        Ok(Self {
            commands,
            events: subscribed,
            pointer: None,
        })
    }

    fn guard_pointer(&mut self, area: Option<&PointerArea>) -> Result<(), String> {
        self.pointer = None;
        let Some(area) = area else {
            return Ok(());
        };
        let mut commands = Self::commands()?;
        let origin = area.origin();
        self.pointer = catcher::guard(area, move |to| {
            let _ = request(&mut commands, RUN_COMMAND, &cursor_set(to, origin));
        })?;
        Ok(())
    }

    fn windows(&mut self) -> Result<Vec<Window>, String> {
        Ok(windows(&request(&mut self.commands, GET_TREE, "")?))
    }

    fn work_area(&mut self, output: &str) -> Option<Rect> {
        work_area(
            &request(&mut self.commands, GET_WORKSPACES, "").ok()?,
            output,
        )
    }

    fn apply(&mut self, window: &Window, placement: Placement, send_windows_to: &str) {
        let _ = request(
            &mut self.commands,
            RUN_COMMAND,
            &command(window, placement, send_windows_to),
        );
    }
}

fn rect(value: &Value) -> Rect {
    let number = |key: &str| {
        value[key]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0)
    };
    Rect {
        left: number("x"),
        top: number("y"),
        right: number("x").saturating_add(number("width")),
        bottom: number("y").saturating_add(number("height")),
    }
}

/// The windows in `get_tree`'s reply: the views (nodes with a process), in sway's coordinates, each
/// frame with its title bar (`deco_rect`, above `rect`), as `move absolute position` places it.
/// The scratchpad (output `__i3`) is skipped; a window sway doesn't show, such as one on a
/// workspace in the background, counts as hidden until it shows. A fullscreen window counts as
/// maximised.
pub fn windows(tree: &Value) -> Vec<Window> {
    fn walk(node: &Value, found: &mut Vec<Window>) {
        if node["type"] == "output" && node["name"] == "__i3" {
            return;
        }
        if let Some(id) = node["id"].as_i64().filter(|_| node["pid"].is_i64()) {
            let mut frame = rect(&node["rect"]);
            let title_bar = rect(&node["deco_rect"]);
            frame.top -= title_bar.bottom - title_bar.top;
            found.push(Window {
                id: id.to_string(),
                title: node["name"].as_str().unwrap_or_default().to_owned(),
                state: WindowState {
                    frame,
                    maximised: node["fullscreen_mode"].as_i64().unwrap_or(0) != 0,
                    hidden: node["visible"] == false,
                    ..WindowState::default()
                },
                floating: node["type"] == "floating_con",
            });
        }
        for child in ["nodes", "floating_nodes"]
            .iter()
            .filter_map(|key| node[key].as_array())
            .flatten()
        {
            walk(child, found);
        }
    }
    let mut found = Vec::new();
    walk(tree, &mut found);
    found
}

/// The output's work area from `get_workspaces`' reply: its visible workspace's rectangle, which
/// leaves out its bars.
pub fn work_area(workspaces: &Value, output: &str) -> Option<Rect> {
    workspaces
        .as_array()?
        .iter()
        .find(|workspace| workspace["output"] == output && workspace["visible"] == true)
        .map(|workspace| rect(&workspace["rect"]))
}

/// The sway command that carries out `placement`: a floating window moves to its new position, a
/// tiled one to the "send windows to" output, where sway fits it in. A fullscreen one leaves
/// fullscreen first: sway would focus it over the window focused there, the game.
pub fn command(window: &Window, placement: Placement, send_windows_to: &str) -> String {
    let id = &window.id;
    match placement {
        Placement::MoveTo(target) if window.floating => format!(
            "[con_id={id}] move absolute position {} {}",
            target.left, target.top
        ),
        Placement::Maximise { .. } => format!(
            "[con_id={id}] fullscreen disable, move container to output \"{send_windows_to}\""
        ),
        _ => format!("[con_id={id}] move container to output \"{send_windows_to}\""),
    }
}

/// The sway command that puts the pointer at `to` on the desktop: `cursor set` counts from the
/// desktop's top-left corner, `origin`, not from sway's 0, 0.
pub fn cursor_set(to: (i32, i32), origin: (i32, i32)) -> String {
    format!(
        "seat - cursor set {} {}",
        to.0.saturating_sub(origin.0),
        to.1.saturating_sub(origin.1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `get_tree` from sway 1.11 on two headless outputs: a tiled foot on HEADLESS-2 and a floating
    /// one above it.
    const TREE: &str = include_str!("../../../tests/fixtures/sway-get-tree.json");

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn the_tree_gives_each_window_with_its_frame_and_whether_it_floats() {
        let tree: Value = serde_json::from_str(TREE).unwrap();
        let found = windows(&tree);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].id, "7");
        assert_eq!(found[0].title, "foot");
        assert_eq!(found[0].state.frame, rect(0, 0, 1280, 720));
        assert!(!found[0].floating && !found[0].state.maximised && !found[0].state.hidden);
        assert_eq!(found[1].id, "8");
        assert_eq!(found[1].state.frame, rect(290, 88, 990, 609));
        assert!(found[1].floating);
    }

    #[test]
    fn fullscreen_and_hidden_windows_and_the_scratchpad() {
        let tree = serde_json::json!({"type": "root", "nodes": [
            {"type": "output", "name": "__i3", "nodes": [{"type": "workspace", "floating_nodes": [
                {"type": "floating_con", "id": 1, "pid": 5, "rect": {"x": 0, "y": 0, "width": 9, "height": 9}}
            ]}]},
            {"type": "output", "name": "DP-1", "nodes": [{"type": "workspace", "nodes": [
                {"type": "con", "id": 2, "pid": 6, "name": "Game", "fullscreen_mode": 1, "visible": true,
                 "rect": {"x": 0, "y": 0, "width": 1920, "height": 1080}},
                {"type": "con", "id": 3, "pid": 7, "name": null, "visible": false,
                 "rect": {"x": 0, "y": 0, "width": 1920, "height": 1080}}
            ]}]}
        ]});
        let found = windows(&tree);
        assert_eq!(found.len(), 2);
        assert!(found[0].state.maximised && !found[0].state.hidden);
        assert_eq!(found[1].title, "");
        assert!(found[1].state.hidden);
    }

    #[test]
    fn the_work_area_is_the_visible_workspace_on_the_output() {
        let workspaces = serde_json::json!([
            {"output": "DP-1", "visible": false, "rect": {"x": 0, "y": 0, "width": 1920, "height": 1080}},
            {"output": "DP-1", "visible": true, "rect": {"x": 0, "y": 30, "width": 1920, "height": 1050}},
            {"output": "HDMI-A-1", "visible": true, "rect": {"x": 1920, "y": 0, "width": 1920, "height": 1080}}
        ]);
        assert_eq!(
            work_area(&workspaces, "DP-1"),
            Some(rect(0, 30, 1920, 1080))
        );
        assert_eq!(work_area(&workspaces, "DP-2"), None);
    }

    #[test]
    fn floating_windows_move_tiled_ones_go_to_the_output_and_fullscreen_ones_leave_fullscreen() {
        let window = |floating, maximised| Window {
            id: "7".into(),
            title: String::new(),
            state: WindowState {
                maximised,
                ..WindowState::default()
            },
            floating,
        };
        let target = Placement::MoveTo(rect(100, 120, 500, 420));
        assert_eq!(
            command(&window(true, false), target, "DP-1"),
            "[con_id=7] move absolute position 100 120"
        );
        assert_eq!(
            command(&window(false, false), target, "DP-1"),
            "[con_id=7] move container to output \"DP-1\""
        );
        let maximise = Placement::Maximise {
            work_area: rect(0, 0, 1920, 1080),
            restore: rect(0, 0, 1, 1),
        };
        assert_eq!(
            command(&window(true, true), maximise, "DP-1"),
            "[con_id=7] fullscreen disable, move container to output \"DP-1\""
        );
    }

    #[test]
    fn the_pointer_is_set_from_the_desktops_corner() {
        assert_eq!(
            cursor_set((1919, 500), (0, 0)),
            "seat - cursor set 1919 500"
        );
        assert_eq!(
            cursor_set((-1, 500), (-1920, -100)),
            "seat - cursor set 1919 600"
        );
    }

    #[test]
    fn messages_are_framed_as_sway_reads_them() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        send(&mut a, GET_TREE, "{}").unwrap();
        assert_eq!(receive(&mut b).unwrap(), (GET_TREE, b"{}".to_vec()));
        b.write_all(b"i3-ipX\0\0\0\0\0\0\0\0").unwrap();
        assert!(receive(&mut a).is_err());
    }
}
