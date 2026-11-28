//! The Linux mirror capture (docs/research/linux-support.md, section 1; `docs/linux-capture.md`):
//! the xdg-desktop-portal ScreenCast hands over a PipeWire stream of the source monitor on Wayland,
//! and MIT-SHM copies it on X11. Both hand frames to the present through [`Picture`].

mod gpu;
mod portal;
mod present;
mod screen;
mod stream;
#[cfg(test)]
mod tests;
mod x11;

pub use present::LinuxMirror;

use super::{Ended, Picture};
use crate::guard::Monitor;
use crate::guard::desktop::{self, Desktop};
use pipewire::spa::param::video::VideoFormat;
use std::time::Duration;

/// How long a stream may go without a frame before cardcast asks PipeWire why: once linked, the
/// first frame comes well within a second.
pub const FIRST_FRAME: Duration = Duration::from_secs(5);

/// DRM fourccs of the 8-bit RGB layouts the capture hands over (`drm_fourcc.h`), little-endian:
/// XRGB8888 is B, G, R, X bytes in memory.
pub const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub const XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
pub const ABGR8888: u32 = u32::from_le_bytes(*b"AB24");

/// A capture of the source monitor, started or failed.
pub struct Capture {
    pub picture: Result<Box<dyn Picture + Send>, Ended>,
    /// The token to save in settings in place of the one passed to [`capture`], started or not:
    /// the portal's new one once the source monitor was shared (single-use, so the old one is
    /// spent); `None`, clearing it, when the portal failed or shared another monitor. X11 keeps
    /// the old one.
    pub restore_token: Option<String>,
}

/// Starts capturing `source_monitor`, one of `monitors`: on Wayland through the portal, whose
/// dialog shows unless `restore_token` (saved in settings) restores the last choice; on X11 with
/// MIT-SHM. `modifiers` are the DMA-BUF layouts the present can import; with none, frames come in
/// memory. Blocks while the portal's dialog is open.
pub fn capture(
    source_monitor: &Monitor,
    monitors: &[Monitor],
    restore_token: Option<&str>,
    modifiers: &[u64],
) -> Capture {
    match desktop::detect(|name| std::env::var(name).ok()) {
        Some(Desktop::X11) => Capture {
            picture: x11::Shm::open(source_monitor)
                .map(|shm| Box::new(shm) as Box<dyn Picture + Send>),
            restore_token: restore_token.map(str::to_owned),
        },
        Some(_) => match portal::share(restore_token) {
            Ok(portal::Shared {
                remote,
                node,
                position,
                size,
                restore_token,
                session,
            }) => started(
                source_monitor,
                monitors,
                position,
                size,
                restore_token,
                || {
                    stream::Stream::open(remote, node, modifiers, Box::new(session))
                        .map(|stream| Box::new(stream) as Box<dyn Picture + Send>)
                },
            ),
            Err(ended) => Capture {
                picture: Err(ended),
                restore_token: None,
            },
        },
        None => Capture {
            picture: Err(Ended::Stop(
                "No desktop session to capture the source monitor from.".into(),
            )),
            restore_token: restore_token.map(str::to_owned),
        },
    }
}

/// After the portal's `Start` shared a stream at `position` with `size` (either may be missing)
/// and gave `restore_token`: `open`s the stream if it is `source_monitor`'s, keeping the token even
/// if the stream fails to open, so a restart shows no dialog. Another monitor's stream, or one that
/// can't be told apart from another, is refused and its token dropped, so the next Start asks.
pub fn started(
    source_monitor: &Monitor,
    monitors: &[Monitor],
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    restore_token: Option<String>,
    open: impl FnOnce() -> Result<Box<dyn Picture + Send>, Ended>,
) -> Capture {
    let as_size =
        |(width, height): (u32, u32)| i32::try_from(width).ok().zip(i32::try_from(height).ok());
    let sized = |monitor: &Monitor, size| {
        Some(size) == as_size(monitor.layout_size)
            || Some(size) == as_size((monitor.width, monitor.height))
    };
    // Monitors never overlap, so a position names one; a size only if no other monitor has it.
    let shared = match (position, size) {
        (Some(position), _) if position == (source_monitor.left, source_monitor.top) => Ok(()),
        (Some(_), _) => Err(not_the_source_monitor(source_monitor)),
        (None, Some(size)) if !sized(source_monitor, size) => {
            Err(not_the_source_monitor(source_monitor))
        }
        (None, Some(size))
            if !monitors.iter().any(|monitor| {
                monitor.device_path != source_monitor.device_path && sized(monitor, size)
            }) =>
        {
            Ok(())
        }
        _ => Err(cannot_tell(source_monitor)),
    };
    match shared {
        Ok(()) => Capture {
            picture: open(),
            restore_token,
        },
        Err(ended) => Capture {
            picture: Err(ended),
            restore_token: None,
        },
    }
}

/// The user shared another monitor in the dialog; only the user can pick again, so the mirror stops.
fn not_the_source_monitor(source_monitor: &Monitor) -> Ended {
    Ended::Stop(format!(
        "The monitor shared in the screen-sharing dialog is not the source monitor. Press Start and share {}.",
        source_monitor.name
    ))
}

/// The portal didn't say where the shared monitor is, and its size (if given) fits another one too.
fn cannot_tell(source_monitor: &Monitor) -> Ended {
    Ended::Stop(format!(
        "cardcast can't tell which monitor the screen-sharing dialog shared, so it doesn't show it. Make the source monitor, {}, the only one of its size, or use a desktop whose portal says where the shared monitor is.",
        source_monitor.name
    ))
}

/// What ends a stream that has had no frame for `waited`: nothing before [`FIRST_FRAME`] or while
/// PipeWire links it (`linked`, `None` if `pw-dump` could not tell), as then a frame may still
/// come; else a status naming WirePlumber, which links the portal's stream (#58).
pub fn no_frame(waited: Duration, linked: impl FnOnce() -> Option<bool>) -> Option<Ended> {
    if waited < FIRST_FRAME {
        return None;
    }
    let why = match linked() {
        Some(true) => return None,
        Some(false) => "PipeWire did not connect the screen capture, so no picture came.",
        None => "No picture came from the screen capture, and pw-dump could not say why.",
    };
    Some(Ended::Stop(format!(
        "{why} Is WirePlumber installed and running? Then press Start."
    )))
}

/// The DRM fourcc of a PipeWire video format cardcast asks for.
pub fn fourcc(format: VideoFormat) -> Option<u32> {
    match format {
        VideoFormat::BGRx => Some(XRGB8888),
        VideoFormat::BGRA => Some(ARGB8888),
        VideoFormat::RGBx => Some(XBGR8888),
        VideoFormat::RGBA => Some(ABGR8888),
        _ => None,
    }
}

/// The `height` rows of `stride` bytes from `offset` in `bytes`, or `None` if they run past its end.
pub fn rows(bytes: &[u8], offset: u32, stride: u32, height: u32) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let length = usize::try_from(u64::from(stride) * u64::from(height)).ok()?;
    bytes.get(start..start.checked_add(length)?)
}

/// A pointer image to draw into a frame: premultiplied ARGB pixels, as XFixes gives them, with its
/// top-left corner in the frame's pixels (it may hang off any edge).
pub struct Pointer<'a> {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub argb: &'a [u32],
}

/// Draws `pointer` over an XRGB8888 frame of `size`, with packed rows; what falls outside is cut off.
pub fn draw_pointer(frame: &mut [u8], size: (u32, u32), pointer: &Pointer) {
    for row in 0..pointer.height {
        for column in 0..pointer.width {
            let (Ok(x), Ok(y)) = (
                u32::try_from(pointer.left + i32::try_from(column).unwrap_or(i32::MAX)),
                u32::try_from(pointer.top + i32::try_from(row).unwrap_or(i32::MAX)),
            ) else {
                continue;
            };
            let at = usize::try_from(row * pointer.width + column).unwrap_or(usize::MAX);
            let (true, true, Some(&argb)) = (x < size.0, y < size.1, pointer.argb.get(at)) else {
                continue;
            };
            let [blue, green, red, alpha] = argb.to_le_bytes();
            let Ok(start) = usize::try_from((y * size.0 + x) * 4) else {
                continue;
            };
            let Some(pixel) = frame.get_mut(start..start + 3) else {
                continue;
            };
            for (channel, source) in pixel.iter_mut().zip([blue, green, red]) {
                let kept = u16::from(*channel) * u16::from(255 - alpha) / 255;
                *channel = u8::try_from(u16::from(source) + kept).unwrap_or(u8::MAX);
            }
        }
    }
}
