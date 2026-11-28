//! The mirror's surface on the card display, which never takes the keyboard focus
//! (docs/research/linux-support.md, section 2): a `zwlr_layer_shell_v1` overlay pinned to the
//! card display's `wl_output` with `keyboard_interactivity` none; on GNOME, which has no
//! layer-shell, a fullscreen `xdg_toplevel` on that output, which cardcast's Shell extension keeps
//! from taking the focus; or on X11 an override-redirect window over its rectangle, which no window
//! manager sees. wgpu draws into it through raw handles.

use super::super::Ended;
use super::present::Shown;
use crate::guard::Monitor;
use std::error::Error;
use std::num::NonZeroU32;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;
use std::time::Duration;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer as Level, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};
use wgpu::rwh::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, XcbDisplayHandle,
    XcbWindowHandle,
};
use x11rb::connection::Connection as _;
use x11rb::protocol::randr::{ConnectionExt as _, ModeFlag, ModeInfo};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ClipOrdering, ConnectionExt as _, CreateWindowAux, WindowClass};
use x11rb::xcb_ffi::XCBConnection;

pub enum Screen {
    Wayland(Wayland),
    X11(X11Window),
}

impl Screen {
    /// The hidden surface over `card_display`; [`Screen::show`] shows it.
    pub fn open(shown: Shown, card_display: &Monitor) -> Result<Self, Ended> {
        match shown {
            Shown::LayerShell | Shown::Fullscreen => {
                Wayland::open(shown, card_display).map(Self::Wayland)
            }
            Shown::OverrideRedirect => {
                X11Window::open(card_display)
                    .map(Self::X11)
                    .map_err(|error| {
                        Ended::Restart(format!(
                            "Could not open the mirror window on the card display. ({error})"
                        ))
                    })
            }
        }
    }

    /// The handles wgpu draws through; valid while `self` lives.
    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        match self {
            Self::Wayland(layer) => (
                WaylandDisplayHandle::new(layer.display).into(),
                WaylandWindowHandle::new(layer.surface_pointer).into(),
            ),
            Self::X11(window) => (
                XcbDisplayHandle::new(Some(window.connection_pointer), window.screen).into(),
                XcbWindowHandle::new(window.window).into(),
            ),
        }
    }

    /// The size of the picture to draw, in pixels: the card display's resolution.
    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::Wayland(layer) => layer.size,
            Self::X11(window) => window.size,
        }
    }

    /// Shows the surface. A layer surface shows with its first picture.
    pub fn show(&mut self) -> Result<(), Ended> {
        match self {
            Self::Wayland(_) => Ok(()),
            Self::X11(window) => window.show().map_err(|error| {
                Ended::Restart(format!(
                    "Could not show the mirror window on the card display. ({error})"
                ))
            }),
        }
    }

    /// Takes the desktop's news; an error once the surface is gone (the card display was unplugged).
    pub fn follow(&mut self) -> Result<(), Ended> {
        match self {
            Self::Wayland(layer) => layer.follow(),
            Self::X11(_) => Ok(()),
        }
    }
}

/// What the layer surface's events say.
#[derive(Default)]
struct Events {
    /// The `wl_output` names, in the order bound.
    names: Vec<Option<String>>,
    /// The size the compositor gave the surface, in its logical pixels; 0 lets cardcast choose.
    configured: Option<(u32, u32)>,
    /// The size in the toplevel's latest configure, which its surface's configure then applies.
    toplevel_size: (u32, u32),
    /// The compositor took the surface away, for example with its output.
    closed: bool,
}

/// The surface's role.
enum Role {
    Layer(ZwlrLayerSurfaceV1),
    Toplevel(XdgSurface, XdgToplevel),
}

pub struct Wayland {
    // Dropped in this order: the role before its wl_surface, then the connection.
    viewport: Option<WpViewport>,
    role: Role,
    surface: WlSurface,
    queue: EventQueue<Events>,
    events: Events,
    connection: Connection,
    display: NonNull<std::ffi::c_void>,
    surface_pointer: NonNull<std::ffi::c_void>,
    size: (u32, u32),
}

// SAFETY: the raw pointers are libwayland's display and surface proxies, which libwayland lets any
// thread use; the mirror thread alone uses them.
unsafe impl Send for Wayland {}

impl Wayland {
    /// A layer surface, or for [`Shown::Fullscreen`] a fullscreen toplevel.
    fn open(shown: Shown, card_display: &Monitor) -> Result<Self, Ended> {
        let failed = |error: &dyn std::fmt::Display| {
            Ended::Restart(format!(
                "Could not open the mirror on the card display. ({error})"
            ))
        };
        let connection = Connection::connect_to_env().map_err(|error| failed(&error))?;
        let (globals, mut queue) =
            registry_queue_init::<Events>(&connection).map_err(|error| failed(&error))?;
        let handle = queue.handle();
        let compositor: WlCompositor = globals
            .bind(&handle, 4..=6, ())
            .map_err(|error| failed(&error))?;
        let layer_shell = (shown == Shown::LayerShell)
            .then(|| globals.bind::<ZwlrLayerShellV1, _, _>(&handle, 1..=4, ()))
            .transpose()
            .map_err(|_| {
                Ended::Stop(
                    "This desktop can't show the mirror: it has no layer-shell (zwlr_layer_shell_v1)."
                        .into(),
                )
            })?;
        let xdg_shell = (shown == Shown::Fullscreen)
            .then(|| globals.bind::<XdgWmBase, _, _>(&handle, 1..=6, ()))
            .transpose()
            .map_err(|error| failed(&error))?;
        let viewporter = globals.bind::<WpViewporter, _, _>(&handle, 1..=1, ()).ok();
        let outputs: Vec<WlOutput> = globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == "wl_output" && global.version >= 4)
                .enumerate()
                .map(|(index, global)| globals.registry().bind(global.name, 4, &handle, index))
                .collect()
        });
        let mut events = Events {
            names: vec![None; outputs.len()],
            ..Events::default()
        };
        queue
            .roundtrip(&mut events)
            .map_err(|error| failed(&error))?;
        let output = events
            .names
            .iter()
            .position(|name| name.as_deref() == Some(&card_display.gdi_name))
            .and_then(|index| outputs.get(index))
            .ok_or_else(|| Ended::Restart("The card display is not connected.".into()))?;
        let surface = compositor.create_surface(&handle, ());
        let role = match (layer_shell, xdg_shell) {
            (Some(shell), _) => {
                let layer = shell.get_layer_surface(
                    &surface,
                    Some(output),
                    Level::Overlay,
                    "cardcast mirror".into(),
                    &handle,
                    (),
                );
                layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
                layer.set_size(0, 0);
                // Over panels too, and never the keyboard focus.
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                Role::Layer(layer)
            }
            (None, Some(shell)) => {
                let xdg = shell.get_xdg_surface(&surface, &handle, ());
                let toplevel = xdg.get_toplevel(&handle, ());
                // The Shell extension knows the mirror by its app id.
                toplevel.set_app_id(crate::guard::gnome::MIRROR_APP_ID.into());
                toplevel.set_title("cardcast mirror".into());
                toplevel.set_fullscreen(Some(output));
                Role::Toplevel(xdg, toplevel)
            }
            (None, None) => return Err(failed(&"no shell to show the mirror with")),
        };
        // Clicks go through to whatever is below; the card display takes no input.
        let region = compositor.create_region(&handle, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        // The first commit has no picture: it asks for the size and shows nothing yet.
        surface.commit();
        // Bounded, so Stop never waits on a compositor that doesn't answer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while events.configured.is_none() && !events.closed {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(Ended::Restart(
                    "The desktop did not place the mirror on the card display.".into(),
                ));
            }
            connection.flush().map_err(|error| failed(&error))?;
            if let Some(guard) = queue.prepare_read() {
                let mut ready = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let millis = libc::c_int::try_from(left.as_millis()).unwrap_or(libc::c_int::MAX);
                // SAFETY: one valid pollfd, for the connection's descriptor, open for the call.
                if unsafe { libc::poll(&raw mut ready, 1, millis) } > 0 {
                    guard.read().map_err(|error| failed(&error))?;
                }
            }
            queue
                .dispatch_pending(&mut events)
                .map_err(|error| failed(&error))?;
        }
        let configured = events
            .configured
            .filter(|_| !events.closed)
            .ok_or_else(|| Ended::Restart("The card display is not connected.".into()))?;
        // A toplevel left to choose its size takes the card display's.
        let configured = if configured.0 == 0 || configured.1 == 0 {
            card_display.layout_size
        } else {
            configured
        };
        // The picture is drawn at the card display's resolution and shown at its logical size;
        // without viewporter it is drawn at the logical size.
        let (viewport, size) = match viewporter {
            Some(viewporter) => {
                let viewport = viewporter.get_viewport(&surface, &handle, ());
                viewport.set_destination(
                    i32::try_from(configured.0).unwrap_or(i32::MAX),
                    i32::try_from(configured.1).unwrap_or(i32::MAX),
                );
                (Some(viewport), (card_display.width, card_display.height))
            }
            None => (None, configured),
        };
        let display = NonNull::new(connection.backend().display_ptr().cast())
            .ok_or_else(|| failed(&"no Wayland display"))?;
        let surface_pointer = NonNull::new(surface.id().as_ptr().cast())
            .ok_or_else(|| failed(&"no Wayland surface"))?;
        Ok(Self {
            viewport,
            role,
            surface,
            queue,
            events,
            connection,
            display,
            surface_pointer,
            size,
        })
    }

    fn follow(&mut self) -> Result<(), Ended> {
        let _ = self.connection.flush();
        if let Some(guard) = self.queue.prepare_read() {
            // Only what is already on the socket: never waits.
            let _ = guard.read_without_dispatch();
        }
        self.queue
            .dispatch_pending(&mut self.events)
            .map_err(|error| Ended::Restart(format!("The mirror lost the desktop. ({error})")))?;
        if self.events.closed {
            return Err(Ended::Restart("The card display is not connected.".into()));
        }
        Ok(())
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        match &self.role {
            Role::Layer(layer) => layer.destroy(),
            Role::Toplevel(xdg, toplevel) => {
                toplevel.destroy();
                xdg.destroy();
            }
        }
        self.surface.destroy();
        let _ = self.connection.flush();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Events {
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

impl Dispatch<WlOutput, usize> for Events {
    fn event(
        events: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let (wl_output::Event::Name { name }, Some(slot)) = (event, events.names.get_mut(*index))
        {
            *slot = Some(name);
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Events {
    fn event(
        events: &mut Self,
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
                events.configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => events.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Events {
    fn event(
        _: &mut Self,
        shell: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Unanswered pings would make the Shell call the mirror not responding.
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Events {
    fn event(
        events: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            events.configured = Some(events.toplevel_size);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Events {
    fn event(
        events: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                events.toplevel_size = (
                    u32::try_from(width).unwrap_or(0),
                    u32::try_from(height).unwrap_or(0),
                );
            }
            xdg_toplevel::Event::Close => events.closed = true,
            _ => {}
        }
    }
}

delegate_noop!(Events: WlCompositor);
delegate_noop!(Events: WlRegion);
delegate_noop!(Events: ignore WlSurface);
delegate_noop!(Events: ZwlrLayerShellV1);
delegate_noop!(Events: WpViewporter);
delegate_noop!(Events: WpViewport);

pub struct X11Window {
    connection: XCBConnection,
    connection_pointer: NonNull<std::ffi::c_void>,
    screen: i32,
    window: NonZeroU32,
    size: (u32, u32),
    /// One refresh of the card display, exact from its RandR mode.
    pub refresh: Duration,
}

// SAFETY: the raw pointer is the XCB connection `connection` owns, which XCB lets any thread use;
// the mirror thread alone uses it.
unsafe impl Send for X11Window {}

impl X11Window {
    fn open(card_display: &Monitor) -> Result<Self, Box<dyn Error>> {
        let (connection, screen) = XCBConnection::connect(None)?;
        let root = connection.setup().roots.get(screen).ok_or("no X screen")?;
        let (root, black) = (root.root, root.black_pixel);
        let window = connection.generate_id()?;
        // The card display's rectangle on the root window, as RandR gives it.
        let size = card_display.layout_size;
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                i16::try_from(card_display.left)?,
                i16::try_from(card_display.top)?,
                u16::try_from(size.0)?,
                u16::try_from(size.1)?,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(black),
            )?
            .check()?;
        // An empty input shape: clicks go through to whatever is below.
        connection
            .shape_rectangles(
                shape::SO::SET,
                shape::SK::INPUT,
                ClipOrdering::UNSORTED,
                window,
                0,
                0,
                &[],
            )?
            .check()?;
        // The card display's CRTC: monitors never overlap, so its top-left names it.
        let resources = connection
            .randr_get_screen_resources_current(root)?
            .reply()?;
        let mut mode = ModeInfo::default();
        for crtc in &resources.crtcs {
            let crtc = connection
                .randr_get_crtc_info(*crtc, resources.config_timestamp)?
                .reply()?;
            if (i32::from(crtc.x), i32::from(crtc.y)) == (card_display.left, card_display.top)
                && let Some(found) = resources.modes.iter().find(|m| m.id == crtc.mode)
            {
                mode = *found;
            }
        }
        let connection_pointer =
            NonNull::new(connection.get_raw_xcb_connection()).ok_or("no XCB connection")?;
        Ok(Self {
            refresh: refresh_period(&mode),
            connection_pointer,
            screen: i32::try_from(screen)?,
            window: NonZeroU32::new(window).ok_or("no window")?,
            size,
            connection,
        })
    }

    fn show(&self) -> Result<(), Box<dyn Error>> {
        self.connection.map_window(self.window.get())?.check()?;
        Ok(())
    }
}

/// One refresh of `mode`: htotal × vtotal dots at the dot clock, with half the lines when
/// interlaced (a field) and twice when doublescanned: RandR's `MODEINFO` and its `Interlace` and
/// `DoubleScan` flags, read as xrandr's `mode_refresh` does (docs/linux-capture.md, [9] and [10]).
/// Rounded down to the nanosecond, so the cap never runs slower than the card display. A mode
/// without timings counts as 60 Hz.
pub fn refresh_period(mode: &ModeInfo) -> Duration {
    // In half-lines, so an interlaced field's half line stays exact.
    let mut half_lines = u128::from(mode.vtotal) * 2;
    if mode.mode_flags.contains(ModeFlag::DOUBLE_SCAN) {
        half_lines *= 2;
    }
    if mode.mode_flags.contains(ModeFlag::INTERLACE) {
        half_lines /= 2;
    }
    let half_dots = u128::from(mode.htotal) * half_lines;
    match (half_dots * 1_000_000_000).checked_div(u128::from(mode.dot_clock) * 2) {
        Some(nanos) if nanos > 0 => Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX)),
        _ => Duration::from_secs(1) / 60,
    }
}

impl Drop for X11Window {
    fn drop(&mut self) {
        let _ = self.connection.destroy_window(self.window.get());
        let _ = self.connection.flush();
    }
}
