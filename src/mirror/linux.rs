//! The Linux mirror capture (docs/research/linux-support.md, section 1; `docs/linux-capture.md`):
//! the xdg-desktop-portal ScreenCast hands over a PipeWire stream of the source monitor on Wayland,
//! and MIT-SHM copies it on X11. Both hand frames to the present through [`Picture`].

mod portal;
mod stream;
#[cfg(test)]
mod tests;
mod x11;

use super::{Ended, Picture};
use crate::guard::Monitor;
use crate::guard::desktop::{self, Desktop};
use pipewire::spa::param::video::VideoFormat;

/// DRM fourccs of the 8-bit RGB layouts the capture hands over (`drm_fourcc.h`), little-endian:
/// XRGB8888 is B, G, R, X bytes in memory.
pub const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub const XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
pub const ABGR8888: u32 = u32::from_le_bytes(*b"AB24");

/// A started capture of the source monitor.
pub struct Capture {
    pub picture: Box<dyn Picture + Send>,
    /// The portal's token to save in settings in place of the one passed to [`capture`], which a
    /// start spends; `None` clears it (X11 has none).
    pub restore_token: Option<String>,
}

/// Starts capturing `source_monitor`: on Wayland through the portal, whose dialog shows unless
/// `restore_token` (saved in settings) restores the last choice; on X11 with MIT-SHM. `modifiers`
/// are the DMA-BUF layouts the present can import; with none, frames come in memory. On failure
/// the saved token is spent, so clear it.
pub fn capture(
    source_monitor: &Monitor,
    restore_token: Option<&str>,
    modifiers: &[u64],
) -> Result<Capture, Ended> {
    match desktop::detect(|name| std::env::var(name).ok()) {
        Some(Desktop::X11) => Ok(Capture {
            picture: Box::new(x11::Shm::open(source_monitor)?),
            restore_token: None,
        }),
        Some(_) => {
            let portal::Shared {
                remote,
                node,
                position,
                size,
                restore_token,
                session,
            } = portal::share(restore_token)?;
            if !shows(source_monitor, position, size) {
                return Err(not_the_source_monitor(source_monitor));
            }
            Ok(Capture {
                picture: Box::new(stream::Stream::open(
                    remote,
                    node,
                    modifiers,
                    Box::new(session),
                )?),
                restore_token,
            })
        }
        None => Err(Ended::Stop(
            "No desktop session to capture the source monitor from.".into(),
        )),
    }
}

/// Whether the portal's stream, at `position` with `size` in the desktop's coordinates (either
/// may be missing), shows `monitor`. Monitors never overlap, so the position decides; without
/// it the size must be the monitor's, on the desktop or in pixels; with neither, it can't be told.
pub fn shows(monitor: &Monitor, position: Option<(i32, i32)>, size: Option<(i32, i32)>) -> bool {
    let as_size =
        |(width, height): (u32, u32)| i32::try_from(width).ok().zip(i32::try_from(height).ok());
    match (position, size) {
        (Some(position), _) => position == (monitor.left, monitor.top),
        (None, Some(size)) => {
            Some(size) == as_size(monitor.layout_size)
                || Some(size) == as_size((monitor.width, monitor.height))
        }
        (None, None) => true,
    }
}

/// The user shared another monitor in the dialog. Its token would restore that one, so it is
/// not kept, and only the user can pick again: the mirror stops.
fn not_the_source_monitor(source_monitor: &Monitor) -> Ended {
    Ended::Stop(format!(
        "The monitor shared in the screen-sharing dialog is not the source monitor. Press Start and share {}.",
        source_monitor.name
    ))
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
