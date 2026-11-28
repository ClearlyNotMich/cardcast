//! The xdg-desktop-portal ScreenCast over D-Bus (`docs/linux-capture.md`): a session sharing one
//! monitor with the pointer drawn in, kept until revoked, handed over as a PipeWire remote.

use super::super::Ended;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{DynamicType, ObjectPath, OwnedObjectPath, OwnedValue, Value};

/// `SelectSources` `types`: share a monitor, not a window.
const MONITOR: u32 = 1;
/// `cursor_mode`: the compositor draws the pointer into the picture.
const EMBEDDED: u32 = 2;
/// `persist_mode`: the permission lasts until the user revokes it.
const PERSISTENT: u32 = 2;

/// A started ScreenCast session sharing one monitor.
pub struct Shared {
    /// The PipeWire remote that carries the stream.
    pub remote: OwnedFd,
    /// The stream's PipeWire node.
    pub node: u32,
    /// Where the shared monitor is, in the desktop's coordinates, if the portal says.
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    /// What restores this choice without the dialog next time; single-use.
    pub restore_token: Option<String>,
    /// The portal closes the session when cardcast leaves the bus, so this keeps it open.
    pub session: Connection,
}

/// Asks the portal for the monitor to share: the dialog shows unless `restore_token` still holds.
/// Blocks until the user answers it.
/// shortcut: a stop cannot cancel the open dialog; upgrade if the present (#43) has to join this thread then.
pub fn share(restore_token: Option<&str>) -> Result<Shared, Ended> {
    let bus = Connection::session().map_err(|error| missing(&error))?;
    let portal = Proxy::new(
        &bus,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast",
    )
    .map_err(|error| missing(&error))?;
    let version: u32 = portal
        .get_property("version")
        .map_err(|error| missing(&error))?;
    let cursor_modes: u32 = portal.get_property("AvailableCursorModes").unwrap_or(0);
    let sender = bus
        .unique_name()
        .map(ToString::to_string)
        .unwrap_or_default();

    let token = next_token();
    let mut options = HashMap::from([
        ("handle_token", Value::from(token.clone())),
        ("session_handle_token", Value::from(token.clone())),
    ]);
    let created = request(&bus, &portal, &sender, "CreateSession", &token, &(options,))?;
    let session = created
        .get("session_handle")
        .and_then(|handle| String::try_from(handle.clone()).ok())
        .and_then(|handle| OwnedObjectPath::try_from(handle).ok())
        .ok_or_else(|| failed("no session"))?;

    let token = next_token();
    options = select_options(version, cursor_modes, restore_token);
    options.insert("handle_token", Value::from(token.clone()));
    request(
        &bus,
        &portal,
        &sender,
        "SelectSources",
        &token,
        &(&session, options),
    )?;

    let token = next_token();
    options = HashMap::from([("handle_token", Value::from(token.clone()))]);
    let started = request(
        &bus,
        &portal,
        &sender,
        "Start",
        &token,
        &(&session, "", options),
    )?;
    let streams = started
        .get("streams")
        .and_then(|streams| {
            Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(streams.clone()).ok()
        })
        .unwrap_or_default();
    let (node, properties) = streams
        .into_iter()
        .next()
        .ok_or_else(|| failed("no stream"))?;
    let pair = |key: &str| {
        properties
            .get(key)
            .and_then(|value| <(i32, i32)>::try_from(value.clone()).ok())
    };

    let remote: zbus::zvariant::OwnedFd = portal
        .call(
            "OpenPipeWireRemote",
            &(&session, HashMap::<&str, Value>::new()),
        )
        .map_err(|error| failed(&error.to_string()))?;
    Ok(Shared {
        remote: remote.into(),
        node,
        position: pair("position"),
        size: pair("size"),
        restore_token: started
            .get("restore_token")
            .and_then(|token| String::try_from(token.clone()).ok()),
        session: bus,
    })
}

/// `SelectSources` options: one monitor, the pointer drawn in where the portal can (version 2+),
/// and from version 4 a permission that persists, restored by `restore_token` if there is one.
pub fn select_options(
    version: u32,
    cursor_modes: u32,
    restore_token: Option<&str>,
) -> HashMap<&'static str, Value<'static>> {
    let mut options = HashMap::from([
        ("types", Value::from(MONITOR)),
        ("multiple", Value::from(false)),
    ]);
    if cursor_modes & EMBEDDED != 0 {
        options.insert("cursor_mode", Value::from(EMBEDDED));
    }
    if version >= 4 {
        options.insert("persist_mode", Value::from(PERSISTENT));
        if let Some(token) = restore_token {
            options.insert("restore_token", Value::from(token.to_owned()));
        }
    }
    options
}

/// The object path the portal answers a request on: its sender's unique name, without the colon
/// and with dots as underscores, and the request's `handle_token`.
pub fn request_path(sender: &str, token: &str) -> String {
    format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        sender.trim_start_matches(':').replace('.', "_")
    )
}

/// What a request's `Response` code means for the mirror: 0 is success.
pub fn answered(code: u32) -> Result<(), Ended> {
    match code {
        0 => Ok(()),
        1 => Err(Ended::Stop(
            "Screen sharing was cancelled. Press Start and share the source monitor.".into(),
        )),
        _ => Err(failed(&format!("response {code}"))),
    }
}

/// Calls `method` and waits for its `Response`, subscribed before the call so it cannot be missed.
fn request<B: zbus::export::serde::Serialize + DynamicType>(
    bus: &Connection,
    portal: &Proxy,
    sender: &str,
    method: &str,
    token: &str,
    body: &B,
) -> Result<HashMap<String, OwnedValue>, Ended> {
    let path = request_path(sender, token);
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Request")
        .and_then(|rule| rule.member("Response"))
        .and_then(|rule| rule.path(ObjectPath::try_from(path.as_str())?))
        .map(zbus::match_rule::Builder::build)
        .map_err(|error| failed(&error.to_string()))?;
    let mut responses = MessageIterator::for_match_rule(rule, bus, Some(1))
        .map_err(|error| failed(&error.to_string()))?;
    portal
        .call::<_, _, OwnedObjectPath>(method, body)
        .map_err(|error| failed(&format!("{method}: {error}")))?;
    let response = responses
        .next()
        .ok_or_else(|| failed("the portal left the bus"))?
        .map_err(|error| failed(&error.to_string()))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = response
        .body()
        .deserialize()
        .map_err(|error| failed(&error.to_string()))?;
    answered(code)?;
    Ok(results)
}

/// A fresh `handle_token`, unique within this cardcast.
fn next_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("cardcast{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// No session bus or no ScreenCast portal: installing one is the fix, so starting again won't help.
fn missing(error: &zbus::Error) -> Ended {
    Ended::Stop(format!(
        "Screen capture needs xdg-desktop-portal and your desktop's portal backend. ({error})"
    ))
}

fn failed(what: &str) -> Ended {
    Ended::Stop(format!(
        "The screen-sharing portal could not share the source monitor. ({what})"
    ))
}
