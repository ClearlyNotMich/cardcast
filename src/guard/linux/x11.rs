//! The desktop's outputs on X11 through RandR 1.5: each output's CRTC rectangle, mode and `EDID`
//! property, the primary output, and change notifications
//! (<https://gitlab.freedesktop.org/xorg/proto/xorgproto/-/blob/master/randrproto.txt>).

use super::Output;
use std::error::Error;
use std::sync::mpsc;
use x11rb::connection::Connection as _;
use x11rb::protocol::randr::{ConnectionExt as _, ModeInfo, NotifyMask, Rotation};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

/// A connection to `DISPLAY` and its first screen's root window, with RandR's version agreed.
fn connect() -> Result<(RustConnection, Window), Box<dyn Error>> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("no X screen")?
        .root;
    connection.randr_query_version(1, 5)?.reply()?;
    Ok((connection, root))
}

pub fn outputs() -> Result<Vec<Output>, Box<dyn Error>> {
    let (connection, root) = connect()?;
    let resources = connection
        .randr_get_screen_resources_current(root)?
        .reply()?;
    let edid_atom = connection.intern_atom(true, b"EDID")?.reply()?.atom;
    let mut outputs = Vec::new();
    for output in &resources.outputs {
        let info = connection
            .randr_get_output_info(*output, resources.config_timestamp)?
            .reply()?;
        // An output with no CRTC shows nothing: it is off or has no monitor.
        if info.crtc == 0 {
            continue;
        }
        let crtc = connection
            .randr_get_crtc_info(info.crtc, resources.config_timestamp)?
            .reply()?;
        let edid = if edid_atom == 0 {
            Vec::new()
        } else {
            // Up to 1024 bytes: the length counts 32-bit units.
            connection
                .randr_get_output_property(*output, edid_atom, AtomEnum::ANY, 0, 256, false, false)?
                .reply()?
                .data
        };
        let mode = resources.modes.iter().find(|mode| mode.id == crtc.mode);
        let layout_size = (crtc.width.into(), crtc.height.into());
        // The CRTC's size is the mode's after rotation, unless `xrandr --scale` stretches it.
        let (width, height) = mode.map_or(layout_size, |mode| {
            super::super::desktop_size(
                mode.width.into(),
                mode.height.into(),
                crtc.rotation
                    .intersects(Rotation::ROTATE90 | Rotation::ROTATE270),
            )
        });
        outputs.push(Output {
            name: String::from_utf8_lossy(&info.name).into_owned(),
            description: String::new(),
            edid: (!edid.is_empty()).then_some(edid),
            left: crtc.x.into(),
            top: crtc.y.into(),
            width,
            height,
            layout_size,
            refresh_hz: mode.map_or(0, refresh_hz),
        });
    }
    Ok(outputs)
}

fn refresh_hz(mode: &ModeInfo) -> u32 {
    let total = u64::from(mode.htotal) * u64::from(mode.vtotal);
    (u64::from(mode.dot_clock) + total / 2)
        .checked_div(total)
        .and_then(|hz| u32::try_from(hz).ok())
        .unwrap_or(0)
}

/// RandR's primary output; `None` without one.
pub fn primary() -> Option<String> {
    let (connection, root) = connect().ok()?;
    let output = connection
        .randr_get_output_primary(root)
        .ok()?
        .reply()
        .ok()?
        .output;
    let info = connection
        .randr_get_output_info(output, 0)
        .ok()?
        .reply()
        .ok()?;
    Some(String::from_utf8_lossy(&info.name).into_owned())
}

/// Sends whether it could start to `started`, then calls `wake` after every RandR change until
/// `wake` returns false or the X server closes the connection.
pub fn watch(started: &mpsc::Sender<Result<(), String>>, wake: impl Fn() -> bool) {
    let setup = || -> Result<_, Box<dyn Error>> {
        let (connection, root) = connect()?;
        connection
            .randr_select_input(
                root,
                NotifyMask::SCREEN_CHANGE | NotifyMask::OUTPUT_CHANGE | NotifyMask::CRTC_CHANGE,
            )?
            .check()?;
        Ok(connection)
    };
    let connection = match setup() {
        Ok(connection) => connection,
        Err(error) => {
            let _ = started.send(Err(error.to_string()));
            return;
        }
    };
    if started.send(Ok(())).is_err() {
        return;
    }
    while connection.wait_for_event().is_ok() {
        if !wake() {
            return;
        }
    }
}
