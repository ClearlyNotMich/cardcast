//! The window guard on GNOME, through cardcast's GNOME Shell extension
//! (`packaging/linux/usr/share/gnome-shell/extensions/`; docs/research/linux-support.md, section 5): only the Shell can
//! move other programs' windows there. The extension lists and moves the windows on the session
//! bus and signals each change to the connections that called its `Connect`; the decisions stay
//! here. The same extension keeps the mirror from
//! taking the focus, so the mirror needs it too.
//!
//! It is the pointer guard on GNOME too: told the card display's rectangle (`GuardPointer`), it
//! watches Mutter's cursor tracker and signals `PointerOnCard` when the pointer is there, and
//! cardcast has it moved off with `Clutter.Seat.warp_pointer` (`WarpPointer`). No timer: Mutter says
//! when the pointer moves. Mutter applies a game's pointer lock and confinement to the warp too
//! (`meta-seat-impl.c`, `constrain_coordinates`).

use super::super::placement::{Placement, Rect, WindowState};
use super::compositor::{Compositor, Events, Window};
use super::kwin::window_state;
use super::window_guard::PointerArea;
use std::sync::{Arc, Mutex, PoisonError};

pub const NAME: &str = "io.github.ClearlyNotMich.cardcast.Shell";
pub const PATH: &str = "/io/github/ClearlyNotMich/cardcast/Shell";

/// The mirror's Wayland app id, by which the extension knows it.
pub const MIRROR_APP_ID: &str = "io.github.ClearlyNotMich.cardcast.Mirror";

/// How to get the extension, for the status line.
pub const INSTALL_EXTENSION: &str = "The cardcast package installs it; from the tarball, copy usr/share/gnome-shell/extensions/cardcast@clearlynotmich.github.io to ~/.local/share/gnome-shell/extensions/. Then log out and back in, and turn it on in the Extensions app.";

/// The status line while the extension isn't running.
pub fn needs_extension() -> String {
    format!(
        "The window guard on GNOME needs cardcast's GNOME Shell extension, which is not running. {INSTALL_EXTENSION}"
    )
}

/// Whether the extension is on the session bus.
pub fn extension_running(connection: &zbus::blocking::Connection) -> bool {
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &NAME,
        )
        .ok()
        .and_then(|reply| reply.body().deserialize().ok())
        .unwrap_or(false)
}

/// The pointer guard's problem when the extension is older than this cardcast.
pub fn needs_newer_extension() -> String {
    format!("it needs the GNOME Shell extension from this cardcast. {INSTALL_EXTENSION}")
}

pub struct Gnome {
    connection: zbus::blocking::Connection,
    /// Where the pointer guard works, for the thread that hears `PointerOnCard`.
    pointer: Arc<Mutex<Option<PointerArea>>>,
}

impl Drop for Gnome {
    /// Closing the connection ends the signal reader.
    fn drop(&mut self) {
        let _ = self.connection.clone().close();
    }
}

impl Gnome {
    /// Calls the extension's `method`.
    pub fn call<B, R>(&self, method: &str, body: &B) -> Result<R, String>
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::export::serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        self.connection
            .call_method(Some(NAME), PATH, Some(NAME), method, body)
            .and_then(|reply| reply.body().deserialize())
            .map_err(|error| error.to_string())
    }
}

impl Compositor for Gnome {
    fn waiting() -> String {
        needs_extension()
    }

    fn connect(events: Events) -> Result<Self, String> {
        let connection =
            zbus::blocking::Connection::session().map_err(|error| error.to_string())?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(NAME)
            .and_then(|rule| rule.path(PATH))
            .and_then(|rule| rule.member("Changed"))
            .map_err(|error| error.to_string())?
            .build();
        let changes = zbus::blocking::MessageIterator::for_match_rule(rule, &connection, None)
            .map_err(|error| error.to_string())?;
        // The extension turned off or GNOME Shell gone: its name changes owner.
        let owner = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")
            .and_then(|rule| rule.member("NameOwnerChanged"))
            .and_then(|rule| rule.arg(0, NAME))
            .map_err(|error| error.to_string())?
            .build();
        let owner = zbus::blocking::MessageIterator::for_match_rule(owner, &connection, None)
            .map_err(|error| error.to_string())?;
        let on_card = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(NAME)
            .and_then(|rule| rule.path(PATH))
            .and_then(|rule| rule.member("PointerOnCard"))
            .map_err(|error| error.to_string())?
            .build();
        let on_card = zbus::blocking::MessageIterator::for_match_rule(on_card, &connection, None)
            .map_err(|error| error.to_string())?;
        if !extension_running(&connection) {
            return Err("the extension is not running".into());
        }
        let gnome = Self {
            connection,
            pointer: Arc::default(),
        };
        let (pointer, warper) = (gnome.pointer.clone(), gnome.connection.clone());
        std::thread::spawn(move || {
            for message in on_card.flatten() {
                let Ok(at) = message.body().deserialize::<(i32, i32)>() else {
                    continue;
                };
                let to = pointer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(|area| area.target(at));
                if let Some(to) = to {
                    let _ = warper.call_method(Some(NAME), PATH, Some(NAME), "WarpPointer", &to);
                }
            }
        });
        gnome.call::<_, ()>("Connect", &())?;
        let events = std::sync::Arc::new(events);
        let changed = events.clone();
        std::thread::spawn(move || {
            for _ in changes {
                if !changed.changed() {
                    return;
                }
            }
        });
        std::thread::spawn(move || {
            if owner
                .into_iter()
                .next()
                .is_some_and(|message| message.is_ok())
            {
                events.gone();
            }
        });
        Ok(gnome)
    }

    fn windows(&mut self) -> Result<Vec<Window>, String> {
        let listed: Vec<(String, String, String, bool)> = self.call("Windows", &())?;
        Ok(windows(listed))
    }

    fn work_area(&mut self, output: &str) -> Option<Rect> {
        let (left, top, right, bottom) = self.call("WorkArea", &output).ok()?;
        Some(Rect {
            left,
            top,
            right,
            bottom,
        })
    }

    fn guard_pointer(&mut self, area: Option<&PointerArea>) -> Result<(), String> {
        *self.pointer.lock().unwrap_or_else(PoisonError::into_inner) = area.cloned();
        // An empty rectangle stops it.
        let card = area.map_or(Rect::default(), |area| area.card_display);
        self.call::<_, ()>(
            "GuardPointer",
            &(card.left, card.top, card.right, card.bottom),
        )
        .map_err(|error| {
            if error.contains("UnknownMethod") {
                needs_newer_extension()
            } else {
                error
            }
        })
    }

    fn apply(&mut self, window: &Window, placement: Placement, send_windows_to: &str) {
        let _: Result<(), String> = match placement {
            Placement::MoveTo(r) => self.call(
                "Move",
                &(window.id.as_str(), r.left, r.top, r.right, r.bottom),
            ),
            _ => self.call("SendToMonitor", &(window.id.as_str(), send_windows_to)),
        };
    }
}

/// The extension's windows: id, state as [`window_state`] reads it, title, and
/// whether it is the mirror.
pub fn windows(listed: Vec<(String, String, String, bool)>) -> Vec<Window> {
    listed
        .into_iter()
        .filter_map(|(id, state, title, mirror)| {
            Some(Window {
                id,
                title,
                state: WindowState {
                    mirror,
                    ..window_state(&state)?
                },
                floating: true,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extensions_windows_are_read_and_a_bad_one_is_skipped() {
        let found = windows(vec![
            (
                "12".into(),
                "2020 100 2420 400 0 0 0".into(),
                "Files".into(),
                false,
            ),
            (
                "13".into(),
                "1920 0 3840 1080 1 0 1".into(),
                "Settings".into(),
                false,
            ),
            ("14".into(), "nonsense".into(), "Bad".into(), false),
            (
                "15".into(),
                "1920 0 3840 1080 1 0 0".into(),
                "cardcast mirror".into(),
                true,
            ),
        ]);
        assert_eq!(found.len(), 3);
        assert!(!found[0].state.mirror && found[2].state.mirror);
        assert_eq!(found[0].id, "12");
        assert_eq!(found[0].title, "Files");
        assert_eq!(found[0].state.frame.left, 2020);
        assert!(found[1].state.maximised && found[1].state.tool);
    }

    #[test]
    fn the_extension_and_its_install_steps_are_named_as_packaged() {
        let uuid = "cardcast@clearlynotmich.github.io";
        let metadata = include_str!(
            "../../../packaging/linux/usr/share/gnome-shell/extensions/cardcast@clearlynotmich.github.io/metadata.json"
        );
        assert!(metadata.contains(&format!(r#""uuid": "{uuid}""#)));
        assert!(INSTALL_EXTENSION.contains(&format!("usr/share/gnome-shell/extensions/{uuid}")));
        let extension = include_str!(
            "../../../packaging/linux/usr/share/gnome-shell/extensions/cardcast@clearlynotmich.github.io/extension.js"
        );
        assert!(extension.contains(&format!("const NAME = '{NAME}';")));
        assert!(extension.contains(&format!("const PATH = '{PATH}';")));
        assert!(extension.contains(&format!("const MIRROR = '{}';", MIRROR_APP_ID)));
    }
}
