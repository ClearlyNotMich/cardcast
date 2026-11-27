//! X11 capture with MIT-SHM (`docs/linux-capture.md`): `ShmGetImage` copies the source monitor's
//! rectangle of the root window into a shared segment each frame, a CPU copy, and XFixes gives the
//! pointer, drawn in as the portal's embedded cursor would be.

use super::super::{Ended, Frame, Picture, Pixels, Release};
use super::{Pointer, XRGB8888, draw_pointer};
use crate::guard::Monitor;
use std::error::Error;
use std::fs::File;
use std::os::unix::fs::FileExt;
use x11rb::connection::Connection as _;
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ImageFormat, ImageOrder, Window};
use x11rb::rust_connection::RustConnection;

pub struct Shm {
    connection: RustConnection,
    root: Window,
    segment: shm::Seg,
    /// The segment, shared with the X server.
    memory: File,
    /// The source monitor's rectangle on the root window.
    left: i16,
    top: i16,
    width: u16,
    height: u16,
}

impl Shm {
    pub fn open(source_monitor: &Monitor) -> Result<Self, Ended> {
        open(source_monitor).map_err(|error| {
            Ended::Stop(format!(
                "Could not capture the source monitor with MIT-SHM. ({error})"
            ))
        })
    }
}

fn open(source_monitor: &Monitor) -> Result<Shm, Box<dyn Error>> {
    let (connection, screen) = x11rb::connect(None)?;
    let setup = connection.setup();
    let root = setup.roots.get(screen).ok_or("no X screen")?;
    // 24-bit colour in 32-bit little-endian pixels is XRGB8888; other depths are not handled.
    let bits = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == root.root_depth)
        .map(|format| format.bits_per_pixel);
    if root.root_depth != 24 || bits != Some(32) || setup.image_byte_order != ImageOrder::LSB_FIRST
    {
        return Err(format!("a {}-bit desktop", root.root_depth).into());
    }
    let root = root.root;
    // ShmCreateSegment came with MIT-SHM 1.2; XFixes 4 has GetCursorImage with the position.
    connection.shm_query_version()?.reply()?;
    connection.xfixes_query_version(4, 0)?.reply()?;
    let left = i16::try_from(source_monitor.left)?;
    let top = i16::try_from(source_monitor.top)?;
    let width = u16::try_from(source_monitor.layout_size.0)?;
    let height = u16::try_from(source_monitor.layout_size.1)?;
    let segment = connection.generate_id()?;
    let size = u32::from(width) * u32::from(height) * 4;
    let memory = connection
        .shm_create_segment(segment, size, false)?
        .reply()?
        .shm_fd;
    Ok(Shm {
        connection,
        root,
        segment,
        memory: File::from(memory),
        left,
        top,
        width,
        height,
    })
}

impl Drop for Shm {
    fn drop(&mut self) {
        let _ = self.connection.shm_detach(self.segment);
        let _ = self.connection.flush();
    }
}

impl Shm {
    fn grab(&self) -> Result<Frame, Box<dyn Error>> {
        self.connection
            .shm_get_image(
                self.root,
                self.left,
                self.top,
                self.width,
                self.height,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                self.segment,
                0,
            )?
            .reply()?;
        let (width, height) = (u32::from(self.width), u32::from(self.height));
        let mut bytes = vec![0; usize::try_from(width * height * 4)?];
        self.memory.read_exact_at(&mut bytes, 0)?;
        let cursor = self.connection.xfixes_get_cursor_image()?.reply()?;
        draw_pointer(
            &mut bytes,
            (width, height),
            &Pointer {
                left: i32::from(cursor.x) - i32::from(cursor.xhot) - i32::from(self.left),
                top: i32::from(cursor.y) - i32::from(cursor.yhot) - i32::from(self.top),
                width: u32::from(cursor.width),
                height: u32::from(cursor.height),
                argb: &cursor.cursor_image,
            },
        );
        Ok(Frame {
            width,
            height,
            fourcc: XRGB8888,
            pixels: Pixels::Memory {
                bytes,
                stride: width * 4,
            },
            release: Release::default(),
        })
    }
}

impl Picture for Shm {
    /// A fresh copy on every call: X11 says nothing about when the desktop changed.
    fn take_frame(&mut self) -> Result<Option<Frame>, Ended> {
        self.grab().map(Some).map_err(|error| {
            Ended::Restart(format!(
                "The screen capture stopped; restarting it. ({error})"
            ))
        })
    }
}
