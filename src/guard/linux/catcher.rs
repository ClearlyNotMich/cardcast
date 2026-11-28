//! The pointer guard's eye on Plasma, sway and Hyprland (docs/research/linux-support.md, section 6):
//! Wayland shows a program the pointer only over its own surfaces (`wl_pointer.enter`,
//! <https://wayland.app/protocols/wayland>), so cardcast covers the card display with a transparent
//! `zwlr_layer_shell_v1` overlay that never takes the keyboard focus. The mirror's own overlay takes
//! no input, so the pointer reaches this one through it. Each desktop then warps the pointer its own
//! way; on Plasma that is KWin's fake input ([`FakeInput`]).
//!
//! No timer and no input hook: the desktop says when the pointer comes. A pointer a game confines to
//! its window or locks (pointer-constraints) never reaches the overlay, so it is never moved.

use super::window_guard::PointerArea;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

/// Calls its `seen` while it lives; dropping it takes the overlay away within a quarter second.
pub struct Catcher {
    stopped: Arc<AtomicBool>,
}

impl Drop for Catcher {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

type Seen = Box<dyn FnMut((i32, i32)) + Send>;

/// What the overlay's events say.
struct State {
    /// The `wl_output` names, in the order bound.
    names: Vec<Option<String>>,
    configured: Option<(u32, u32)>,
    closed: bool,
    /// The output's top-left corner on the desktop.
    corner: (i32, i32),
    viewport: Option<WpViewport>,
    surface: Option<WlSurface>,
    pointer: Option<WlPointer>,
    seen: Seen,
}

impl Catcher {
    /// Covers output `output`, whose top-left corner on the desktop is `corner`, and calls `seen`
    /// with the pointer's desktop position each time it comes onto or moves over it, on a thread of
    /// its own. `Ok(None)` when the output isn't there (the card display unplugged); errors are
    /// plain words.
    pub fn start(
        output: &str,
        corner: (i32, i32),
        seen: impl FnMut((i32, i32)) + Send + 'static,
    ) -> Result<Option<Self>, String> {
        let failed = |error: &dyn std::fmt::Display| error.to_string();
        let connection = Connection::connect_to_env().map_err(|error| failed(&error))?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&connection).map_err(|error| failed(&error))?;
        let handle = queue.handle();
        let missing = |what: &str| format!("the desktop has no {what}");
        let compositor: WlCompositor = globals
            .bind(&handle, 4..=6, ())
            .map_err(|_| missing("wl_compositor"))?;
        let shell: ZwlrLayerShellV1 = globals
            .bind(&handle, 1..=4, ())
            .map_err(|_| missing("layer-shell (zwlr_layer_shell_v1)"))?;
        let viewporter: WpViewporter = globals
            .bind(&handle, 1..=1, ())
            .map_err(|_| missing("viewporter"))?;
        let pixels: WpSinglePixelBufferManagerV1 = globals
            .bind(&handle, 1..=1, ())
            .map_err(|_| missing("single-pixel buffers"))?;
        let _seat: WlSeat = globals
            .bind(&handle, 1..=7, ())
            .map_err(|_| missing("seat"))?;
        let outputs: Vec<WlOutput> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == "wl_output" && global.version >= 4)
                .enumerate()
                .map(|(index, global)| globals.registry().bind(global.name, 4, &handle, index))
                .collect()
        });
        let mut state = State {
            names: vec![None; outputs.len()],
            configured: None,
            closed: false,
            corner,
            viewport: None,
            surface: None,
            pointer: None,
            seen: Box::new(seen),
        };
        queue
            .roundtrip(&mut state)
            .map_err(|error| failed(&error))?;
        let Some(wl_output) = state
            .names
            .iter()
            .position(|name| name.as_deref() == Some(output))
            .and_then(|index| outputs.get(index))
        else {
            return Ok(None);
        };
        let surface = compositor.create_surface(&handle, ());
        let layer = shell.get_layer_surface(
            &surface,
            Some(wl_output),
            Layer::Overlay,
            "cardcast pointer guard".into(),
            &handle,
            (),
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();
        queue
            .roundtrip(&mut state)
            .map_err(|error| failed(&error))?;
        let Some((width, height)) = state.configured.filter(|_| !state.closed) else {
            return Ok(None);
        };
        // One transparent pixel, stretched over the output: its whole area takes the pointer.
        let buffer = pixels.create_u32_rgba_buffer(0, 0, 0, 0, &handle, ());
        let viewport = viewporter.get_viewport(&surface, &handle, ());
        set_size(&viewport, width, height);
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        connection.flush().map_err(|error| failed(&error))?;
        state.viewport = Some(viewport);
        state.surface = Some(surface);
        let stopped = Arc::new(AtomicBool::new(false));
        let catcher = Self {
            stopped: stopped.clone(),
        };
        std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) && !state.closed {
                if connection.flush().is_err() {
                    break;
                }
                if let Some(guard) = queue.prepare_read() {
                    let mut ready = libc::pollfd {
                        fd: guard.connection_fd().as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one valid pollfd, for the connection's descriptor, open for the call.
                    if unsafe { libc::poll(&raw mut ready, 1, 250) } > 0 && guard.read().is_err() {
                        break;
                    }
                }
                if stopped.load(Ordering::Relaxed) || queue.dispatch_pending(&mut state).is_err() {
                    break;
                }
            }
            layer.destroy();
            if let Some(viewport) = state.viewport.take() {
                viewport.destroy();
            }
            if let Some(surface) = state.surface.take() {
                surface.destroy();
            }
            buffer.destroy();
            let _ = connection.flush();
        });
        Ok(Some(catcher))
    }
}

/// Watches the card display in `area` and has `warp` move a pointer that comes onto it to the
/// nearest point off it. `Ok(None)` while the card display isn't there.
pub fn guard(
    area: &PointerArea,
    mut warp: impl FnMut((i32, i32)) + Send + 'static,
) -> Result<Option<Catcher>, String> {
    let corner = (area.card_display.left, area.card_display.top);
    let seen = area.clone();
    Catcher::start(&area.output, corner, move |pointer| {
        if let Some(to) = seen.target(pointer)
            && std::env::var_os("CARDCAST_RED").is_some()
        {
            warp(to);
        }
    })
}

fn set_size(viewport: &WpViewport, width: u32, height: u32) {
    let length = |value: u32| i32::try_from(value.max(1)).unwrap_or(i32::MAX);
    viewport.set_destination(length(width), length(height));
}

/// The desktop position of a point `surface` pixels into the overlay at `corner`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to i32's range first"
)]
fn desktop_position(corner: (i32, i32), surface: (f64, f64)) -> (i32, i32) {
    let whole = |value: f64| {
        value
            .floor()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    };
    (
        corner.0.saturating_add(whole(surface.0)),
        corner.1.saturating_add(whole(surface.1)),
    )
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let (wl_output::Event::Name { name }, Some(slot)) = (event, state.names.get_mut(*index))
        {
            *slot = Some(name);
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                state.configured = Some((width, height));
                // A later configure (the output's mode changed) resizes the overlay.
                if let (Some(viewport), Some(surface)) = (&state.viewport, &state.surface) {
                    set_size(viewport, width, height);
                    surface.commit();
                }
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            // A seat gets its pointer with its first mouse, and loses it with its last.
            let has_pointer = capabilities.contains(Capability::Pointer);
            if has_pointer && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(handle, ()));
            } else if !has_pointer
                && let Some(pointer) = state.pointer.take()
                && pointer.version() >= 3
            {
                pointer.release();
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let at = match event {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                // No pointer picture over the card display.
                pointer.set_cursor(serial, None, 0, 0);
                (surface_x, surface_y)
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => (surface_x, surface_y),
            _ => return,
        };
        (state.seen)(desktop_position(state.corner, at));
    }
}

delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: WpViewporter);
delegate_noop!(State: WpViewport);
delegate_noop!(State: WpSinglePixelBufferManagerV1);

/// KWin's fake input (`org_kde_kwin_fake_input`), which moves the pointer on Plasma
/// (<https://invent.kde.org/plasma/kwin/-/blob/master/src/backends/fakeinput/fakeinputbackend.cpp>).
/// KWin offers it to every program not in a sandbox, and its `authenticate` accepts anyone ("TODO:
/// make secure"), so a later KWin may close it. KWin applies a game's pointer lock and confinement to
/// these moves too (`PointerInputRedirection::updatePosition`).
pub struct FakeInput {
    fake: OrgKdeKwinFakeInput,
    connection: Connection,
    _queue: wayland_client::EventQueue<Quiet>,
}

/// What fake input's connection hears: nothing it needs.
pub struct Quiet;

impl FakeInput {
    /// Errors are plain words.
    pub fn connect() -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|error| error.to_string())?;
        let (globals, queue) =
            registry_queue_init::<Quiet>(&connection).map_err(|error| error.to_string())?;
        let fake: OrgKdeKwinFakeInput = globals
            .bind(&queue.handle(), 3..=5, ())
            .map_err(|_| "KWin offers cardcast no fake input".to_owned())?;
        fake.authenticate(
            "cardcast".into(),
            "Keeps the pointer off the card display".into(),
        );
        connection.flush().map_err(|error| error.to_string())?;
        Ok(Self {
            fake,
            connection,
            _queue: queue,
        })
    }

    /// Moves the pointer to `to` on the desktop.
    pub fn warp(&self, to: (i32, i32)) {
        self.fake
            .pointer_motion_absolute(f64::from(to.0), f64::from(to.1));
        let _ = self.connection.flush();
    }
}

impl Drop for FakeInput {
    fn drop(&mut self) {
        if self.fake.version() >= 5 {
            self.fake.destroy();
        }
        let _ = self.connection.flush();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Quiet {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Quiet: OrgKdeKwinFakeInput);

/// Where the pointer was last seen over a probe overlay, for the CI tests.
#[cfg(test)]
pub type Probed = Arc<std::sync::Mutex<Option<(i32, i32)>>>;

/// An overlay on `monitor` that notes where the pointer is over it, for the CI tests.
#[cfg(test)]
pub fn probe(monitor: &crate::guard::Monitor) -> (Catcher, Probed) {
    let probed = Probed::default();
    let seen = probed.clone();
    let catcher = Catcher::start(&monitor.gdi_name, (monitor.left, monitor.top), move |at| {
        *seen.lock().unwrap() = Some(at);
    })
    .unwrap()
    .unwrap();
    (catcher, probed)
}

/// A virtual mouse (`zwlr_virtual_pointer_v1`, sway), which gives the seat a pointer and puts it
/// anywhere on the desktop, for the CI tests.
#[cfg(test)]
pub struct VirtualPointer {
    pointer: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    connection: Connection,
    _queue: wayland_client::EventQueue<Quiet>,
}

#[cfg(test)]
impl VirtualPointer {
    pub fn new() -> Self {
        use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
        let connection = Connection::connect_to_env().unwrap();
        let (globals, queue) = registry_queue_init::<Quiet>(&connection).unwrap();
        let seat: WlSeat = globals.bind(&queue.handle(), 1..=7, ()).unwrap();
        let manager: ZwlrVirtualPointerManagerV1 =
            globals.bind(&queue.handle(), 1..=2, ()).unwrap();
        let pointer = manager.create_virtual_pointer(Some(&seat), &queue.handle(), ());
        connection.flush().unwrap();
        Self {
            pointer,
            connection,
            _queue: queue,
        }
    }

    /// Puts the pointer at `to` on a desktop whose top-left corner is `origin` and whose size is `size`.
    pub fn move_to(&self, to: (i32, i32), origin: (i32, i32), size: (u32, u32)) {
        let x = u32::try_from(to.0 - origin.0).unwrap();
        let y = u32::try_from(to.1 - origin.1).unwrap();
        self.pointer.motion_absolute(0, x, y, size.0, size.1);
        self.pointer.frame();
        self.connection.flush().unwrap();
    }
}

#[cfg(test)]
delegate_noop!(Quiet: ignore WlSeat);
#[cfg(test)]
delegate_noop!(Quiet: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
#[cfg(test)]
delegate_noop!(Quiet: wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overlay_puts_the_pointer_on_the_desktop_from_its_corner() {
        assert_eq!(desktop_position((1920, -200), (0.0, 10.7)), (1920, -190));
        assert_eq!(desktop_position((0, 0), (1279.9, 719.5)), (1279, 719));
    }
}
