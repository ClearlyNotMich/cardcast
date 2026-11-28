//! The window guard on X11, through the window manager (Extended Window Manager Hints,
//! <https://specifications.freedesktop.org/wm-spec/latest/>): `_NET_CLIENT_LIST` lists the
//! windows, `_NET_MOVERESIZE_WINDOW` and `_NET_WM_STATE` move and maximise them without focusing
//! them. A reader thread passes the X events to the guard thread, which checks a window once it
//! has been still for a moment, as on Windows.
//!
//! The same thread is the pointer guard (docs/research/linux-support.md, section 6): XFixes pointer
//! barriers along the card display's edges stop a mouse there, and since absolute devices and warps
//! pass them (<https://gitlab.freedesktop.org/xorg/proto/xorgproto/-/blob/master/fixesproto.txt>,
//! section 12), the pointer is also checked about 60 times a second and put back, as on Windows. It
//! is left alone while another program holds a pointer grab, as a game confining it does.

use super::super::placement::{Placement, Rect, WindowState};
use super::window_guard::{Guarding, could_not_start, monitor_rect};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{BarrierDirections, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _,
    CreateWindowAux, EventMask, GrabMode, GrabStatus, MapState, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        _NET_CLIENT_LIST,
        _NET_CURRENT_DESKTOP,
        _NET_WORKAREA,
        _NET_FRAME_EXTENTS,
        _NET_MOVERESIZE_WINDOW,
        _NET_WM_NAME,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_DIALOG,
        UTF8_STRING,
    }
}

/// How long a window must be still before the guard checks it, so a drag is checked once.
const SETTLE: Duration = Duration::from_millis(50);

/// How long the window manager gets to unmaximise a window before the guard gives up on it.
const UNMAXIMISE_LIMIT: Duration = Duration::from_secs(2);

/// Pointer check interval: about 60 times a second, as on Windows.
const POINTER_INTERVAL: Duration = Duration::from_millis(16);

enum Message {
    Event(Event),
    FollowDisplays,
    Stop,
}

/// The running guard thread; dropping it stops the thread and its event reader.
pub struct Running {
    messages: Sender<Message>,
    stopped: Arc<AtomicBool>,
}

impl Running {
    pub fn follow_displays(&self) {
        let _ = self.messages.send(Message::FollowDisplays);
    }
}

impl Drop for Running {
    /// Never waits on the X server; the guard thread moves nothing once `stopped` is set.
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.messages.send(Message::Stop);
    }
}

/// The connection and what the guard thread asks the window manager with.
struct X11 {
    connection: Arc<RustConnection>,
    root: Window,
    atoms: Atoms,
    /// A window of cardcast's own, destroyed to wake the event reader when the guard stops.
    waker: Window,
    /// Set when the guard stops: from then on nothing moves.
    stopped: Arc<AtomicBool>,
    /// The X server has pointer barriers (XFixes 5).
    barriers: bool,
}

/// Starts the guard thread, which connects to the X server; its problems reach the status line
/// through `guarding`.
pub fn start(guarding: Guarding) -> Running {
    let (messages, received) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let running = Running {
        messages: messages.clone(),
        stopped: stopped.clone(),
    };
    std::thread::spawn(move || {
        let x11 = match connect(stopped) {
            Ok(x11) => x11,
            Err(error) => {
                guarding.notice(&could_not_start(&error.to_string()));
                return;
            }
        };
        let connection = x11.connection.clone();
        let stopped = x11.stopped.clone();
        std::thread::spawn(move || {
            while let Ok(event) = connection.wait_for_event() {
                if stopped.load(Ordering::Relaxed) || messages.send(Message::Event(event)).is_err()
                {
                    break;
                }
            }
        });
        run(&x11, guarding, &received);
    });
    running
}

fn connect(stopped: Arc<AtomicBool>) -> Result<X11, Box<dyn Error>> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("no X screen")?
        .root;
    let atoms = Atoms::new(&connection)?.reply()?;
    let barriers = connection
        .xfixes_query_version(5, 0)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|version| version.major_version >= 5);
    connection
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()?;
    let waker = connection.generate_id()?;
    connection
        .create_window(
            0,
            waker,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )?
        .check()?;
    Ok(X11 {
        connection: Arc::new(connection),
        root,
        atoms,
        waker,
        stopped,
        barriers,
    })
}

/// The guard thread's windows: the managed ones, those waiting to settle, and those being unmaximised to be maximised again.
#[derive(Default)]
struct Windows {
    clients: HashSet<Window>,
    pending: HashMap<Window, Instant>,
    /// With when they were unmaximised.
    remaximising: HashMap<Window, Instant>,
    /// The pointer barriers around the card display.
    barriers: Vec<u32>,
}

fn run(x11: &X11, mut guarding: Guarding, messages: &Receiver<Message>) {
    let mut windows = Windows::default();
    follow_displays(x11, &mut guarding, &mut windows);
    let mut pointer_at = Instant::now();
    loop {
        let settled_at = windows.pending.values().min().map(|at| *at + SETTLE);
        let overdue_at = windows
            .remaximising
            .values()
            .min()
            .map(|at| *at + UNMAXIMISE_LIMIT);
        let pointer_due = guarding.pointer_area().is_some().then_some(pointer_at);
        let message = match settled_at
            .into_iter()
            .chain(overdue_at)
            .chain(pointer_due)
            .min()
        {
            Some(at) => messages.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => messages.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Message::FollowDisplays) => follow_displays(x11, &mut guarding, &mut windows),
            Ok(Message::Event(event)) => on_event(x11, &mut guarding, &mut windows, &event),
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if pointer_due.is_some_and(|at| now >= at) {
            check_pointer(x11, &mut guarding);
            pointer_at = now + POINTER_INTERVAL;
        }
        let overdue = windows
            .remaximising
            .iter()
            .filter(|&(_, &at)| now.duration_since(at) >= UNMAXIMISE_LIMIT);
        let settled: Vec<Window> = windows
            .pending
            .iter()
            .filter(|&(_, &at)| now.duration_since(at) >= SETTLE)
            .chain(overdue)
            .map(|(&window, _)| window)
            .collect();
        for window in settled {
            windows.pending.remove(&window);
            check(x11, &mut guarding, &mut windows, window, now);
        }
    }
    x11.stopped.store(true, Ordering::Relaxed);
    set_barriers(x11, &mut windows.barriers, None);
    let _ = x11.connection.destroy_window(x11.waker);
    let _ = x11.connection.flush();
}

fn on_event(x11: &X11, guarding: &mut Guarding, windows: &mut Windows, event: &Event) {
    let changed = match event {
        Event::PropertyNotify(event) if event.window == x11.root => {
            if event.atom == x11.atoms._NET_CLIENT_LIST {
                update_clients(x11, guarding, windows);
            } else if event.atom == x11.atoms._NET_WORKAREA {
                follow_displays(x11, guarding, windows);
            }
            return;
        }
        Event::PropertyNotify(event) => event.window,
        Event::ConfigureNotify(event) => event.window,
        Event::MapNotify(event) => event.window,
        _ => return,
    };
    if windows.clients.contains(&changed) {
        windows.pending.insert(changed, Instant::now());
    }
}

/// Finds the monitors, then checks every window.
fn follow_displays(x11: &X11, guarding: &mut Guarding, windows: &mut Windows) {
    let workarea = current_workarea(x11);
    guarding.follow_displays(|monitor| Some(work_area(monitor_rect(monitor), workarea)));
    let card_display = guarding.pointer_area().map(|area| area.card_display);
    set_barriers(x11, &mut windows.barriers, card_display);
    windows.clients.clear();
    update_clients(x11, guarding, windows);
}

/// Follows `_NET_CLIENT_LIST`: watches and checks new windows, forgets closed ones.
fn update_clients(x11: &X11, guarding: &mut Guarding, windows: &mut Windows) {
    let listed: HashSet<Window> = property32(x11, x11.root, x11.atoms._NET_CLIENT_LIST)
        .into_iter()
        .collect();
    for gone in windows.clients.difference(&listed) {
        guarding.forget(id(*gone));
        windows.pending.remove(gone);
        windows.remaximising.remove(gone);
    }
    let now = Instant::now();
    let new: Vec<Window> = listed.difference(&windows.clients).copied().collect();
    windows.clients = listed;
    for new in new {
        let events = EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE;
        let _ = x11
            .connection
            .change_window_attributes(new, &ChangeWindowAttributesAux::new().event_mask(events));
        check(x11, guarding, windows, new, now);
    }
}

/// Puts the pointer back off the card display, unless another program holds a pointer grab.
fn check_pointer(x11: &X11, guarding: &mut Guarding) {
    if x11.stopped.load(Ordering::Relaxed) {
        return;
    }
    let Some(pointer) = x11
        .connection
        .query_pointer(x11.root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .filter(|pointer| pointer.same_screen)
    else {
        return;
    };
    let Some((x, y)) = guarding.place_pointer((pointer.root_x.into(), pointer.root_y.into()))
    else {
        return;
    };
    let (Ok(x), Ok(y)) = (i16::try_from(x), i16::try_from(y)) else {
        return;
    };
    // X has no way to ask whether the pointer is grabbed but to try; a grab that succeeds is
    // nobody else's, and is let go at once.
    let free = x11
        .connection
        .grab_pointer(
            false,
            x11.root,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            x11rb::NONE,
            x11rb::CURRENT_TIME,
        )
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.status == GrabStatus::SUCCESS);
    if free {
        let _ = x11.connection.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = x11
            .connection
            .warp_pointer(x11rb::NONE, x11.root, 0, 0, 0, 0, x, y);
    }
    let _ = x11.connection.flush();
}

/// Replaces the pointer barriers with ones around `card_display`, or none.
fn set_barriers(x11: &X11, barriers: &mut Vec<u32>, card_display: Option<Rect>) {
    for barrier in barriers.drain(..) {
        let _ = x11.connection.xfixes_delete_pointer_barrier(barrier);
    }
    for (x1, y1, x2, y2, leaving) in card_display
        .filter(|_| x11.barriers)
        .map(barrier_lines)
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(barrier) = x11.connection.generate_id() else {
            break;
        };
        let created = x11.connection.xfixes_create_pointer_barrier(
            barrier,
            x11.root,
            x1,
            y1,
            x2,
            y2,
            leaving,
            &[],
        );
        if created.is_ok() {
            barriers.push(barrier);
        }
    }
    let _ = x11.connection.flush();
}

/// A pointer barrier: `(x1, y1, x2, y2, directions)`.
type BarrierLine = (u16, u16, u16, u16, BarrierDirections);

/// The pointer barriers along the card display's four edges: `(x1, y1, x2, y2, directions)`, each
/// letting the pointer through only on its way off the card display (XFixes' directions are the
/// ones allowed). `None` for an edge outside X's 16-bit coordinates.
pub fn barrier_lines(card: Rect) -> [Option<BarrierLine>; 4] {
    let line = |x1: i32, y1: i32, x2: i32, y2: i32, leaving| {
        Some((
            u16::try_from(x1).ok()?,
            u16::try_from(y1).ok()?,
            u16::try_from(x2).ok()?,
            u16::try_from(y2).ok()?,
            leaving,
        ))
    };
    let Rect {
        left,
        top,
        right,
        bottom,
    } = card;
    [
        line(left, top, left, bottom, BarrierDirections::NEGATIVE_X),
        line(right, top, right, bottom, BarrierDirections::POSITIVE_X),
        line(left, top, right, top, BarrierDirections::NEGATIVE_Y),
        line(left, bottom, right, bottom, BarrierDirections::POSITIVE_Y),
    ]
}

fn id(window: Window) -> isize {
    isize::try_from(window).unwrap_or(isize::MAX)
}

/// Reads the window, decides, and asks the window manager to move it.
fn check(x11: &X11, guarding: &mut Guarding, windows: &mut Windows, window: Window, now: Instant) {
    if x11.stopped.load(Ordering::Relaxed) {
        return;
    }
    let Some((state, extents, title)) = read_window(x11, window) else {
        return;
    };
    if state.maximised
        && let Some(&since) = windows.remaximising.get(&window)
    {
        // Not unmaximised yet: its restored frame comes with a later event, unless the window
        // manager refuses; then it stays on the card display, named in the status line.
        if now.duration_since(since) >= UNMAXIMISE_LIMIT {
            windows.remaximising.remove(&window);
            guarding.give_up(id(window));
        }
        return;
    }
    let remaximising = windows.remaximising.remove(&window).is_some();
    match guarding.check(id(window), &title, state, remaximising, now) {
        Placement::MoveTo(target) => move_frame(x11, window, target, extents),
        Placement::Maximise { .. } if !remaximising => {
            // The restore rectangle is the window manager's own; unmaximising shows it.
            windows.remaximising.insert(window, now);
            set_maximised(x11, window, false);
        }
        Placement::Maximise { restore, .. } => {
            move_frame(x11, window, restore, extents);
            set_maximised(x11, window, true);
        }
        Placement::Stay if remaximising => set_maximised(x11, window, true),
        Placement::Stay | Placement::GiveUp | Placement::Handled => {}
    }
    let _ = x11.connection.flush();
}

/// The window's state, its frame extents and its title; `None` once it is gone.
fn read_window(x11: &X11, window: Window) -> Option<(WindowState, [u32; 4], String)> {
    let connection = &*x11.connection;
    let geometry = connection.get_geometry(window).ok()?;
    let position = connection
        .translate_coordinates(window, x11.root, 0, 0)
        .ok()?;
    let attributes = connection.get_window_attributes(window).ok()?;
    let (geometry, position, attributes) = (
        geometry.reply().ok()?,
        position.reply().ok()?,
        attributes.reply().ok()?,
    );
    let extents = property32(x11, window, x11.atoms._NET_FRAME_EXTENTS);
    let extents = <[u32; 4]>::try_from(extents.as_slice()).unwrap_or_default();
    let states = property32(x11, window, x11.atoms._NET_WM_STATE);
    let has = |state: Atom| states.contains(&state);
    let types = property32(x11, window, x11.atoms._NET_WM_WINDOW_TYPE);
    let state = WindowState {
        frame: frame(
            (position.dst_x.into(), position.dst_y.into()),
            (geometry.width.into(), geometry.height.into()),
            extents,
        ),
        maximised: has(x11.atoms._NET_WM_STATE_MAXIMIZED_VERT)
            && has(x11.atoms._NET_WM_STATE_MAXIMIZED_HORZ),
        minimised: has(x11.atoms._NET_WM_STATE_HIDDEN),
        // Unmapped: minimised, or on another virtual desktop.
        hidden: attributes.map_state != MapState::VIEWABLE,
        tool: is_tool(
            &types,
            x11.atoms._NET_WM_WINDOW_TYPE_NORMAL,
            x11.atoms._NET_WM_WINDOW_TYPE_DIALOG,
        ),
        ..WindowState::default()
    };
    Some((state, extents, title(x11, window)))
}

fn title(x11: &X11, window: Window) -> String {
    let read = |property: Atom, kind: Atom| {
        x11.connection
            .get_property(false, window, property, kind, 0, 256)
            .ok()?
            .reply()
            .ok()
            .filter(|reply| !reply.value.is_empty())
            .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
    };
    read(x11.atoms._NET_WM_NAME, x11.atoms.UTF8_STRING)
        .or_else(|| read(AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
        .unwrap_or_default()
}

/// A 32-bit list property (`CARDINAL`, `ATOM` or `WINDOW`); empty if unset.
fn property32(x11: &X11, window: Window, property: Atom) -> Vec<u32> {
    x11.connection
        .get_property(false, window, property, AtomEnum::ANY, 0, 4096)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .and_then(|reply| reply.value32().map(Iterator::collect))
        .unwrap_or_default()
}

/// The current desktop's `_NET_WORKAREA` (x, y, width, height).
fn current_workarea(x11: &X11) -> Option<[u32; 4]> {
    let desktop = property32(x11, x11.root, x11.atoms._NET_CURRENT_DESKTOP)
        .first()
        .copied()
        .unwrap_or(0);
    let areas = property32(x11, x11.root, x11.atoms._NET_WORKAREA);
    let start = usize::try_from(desktop).ok()?.checked_mul(4)?;
    <[u32; 4]>::try_from(areas.get(start..start + 4)?).ok()
}

fn move_frame(x11: &X11, window: Window, target: Rect, extents: [u32; 4]) {
    let message = ClientMessageEvent::new(
        32,
        window,
        x11.atoms._NET_MOVERESIZE_WINDOW,
        moveresize(target, extents),
    );
    send_to_window_manager(x11, message);
}

/// `_NET_WM_STATE` add (1) or remove (0) both maximised states, from a pager (source 2).
fn set_maximised(x11: &X11, window: Window, maximised: bool) {
    let message = ClientMessageEvent::new(
        32,
        window,
        x11.atoms._NET_WM_STATE,
        [
            u32::from(maximised),
            x11.atoms._NET_WM_STATE_MAXIMIZED_VERT,
            x11.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
            2,
            0,
        ],
    );
    send_to_window_manager(x11, message);
}

fn send_to_window_manager(x11: &X11, message: ClientMessageEvent) {
    let _ = x11.connection.send_event(
        false,
        x11.root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        message,
    );
}

/// The visible frame of a window whose client area is at `position` with `size`, framed by the
/// window manager's `_NET_FRAME_EXTENTS` (left, right, top, bottom).
pub fn frame(position: (i32, i32), size: (u32, u32), extents: [u32; 4]) -> Rect {
    let length = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    let [left, right, top, bottom] = extents.map(length);
    Rect {
        left: position.0 - left,
        top: position.1 - top,
        right: position.0.saturating_add(length(size.0)) + right,
        bottom: position.1.saturating_add(length(size.1)) + bottom,
    }
}

/// `_NET_MOVERESIZE_WINDOW`'s data that puts the frame on `target`: with north-west gravity (1)
/// x and y are the frame's corner, width and height the client's, without the frame extents; the
/// flags say all four are given (bits 8–11) by a pager (source 2, bits 12–15).
pub fn moveresize(target: Rect, extents: [u32; 4]) -> [u32; 5] {
    let [left, right, top, bottom] = extents;
    let client = |length: i32, frame: u32| {
        u32::try_from(length)
            .unwrap_or(0)
            .saturating_sub(frame)
            .max(1)
    };
    // X coordinates travel as 32-bit two's complement, so a negative position stays negative.
    let coordinate = |value: i32| u32::from_ne_bytes(value.to_ne_bytes());
    [
        1 | 0b1111 << 8 | 2 << 12,
        coordinate(target.left),
        coordinate(target.top),
        client(target.right - target.left, left + right),
        client(target.bottom - target.top, top + bottom),
    ]
}

/// Whether windows of these `_NET_WM_WINDOW_TYPE`s (most wanted first) are left alone, like
/// Windows' tool windows: anything but a normal window or a dialog. No type means normal.
pub fn is_tool(types: &[Atom], normal: Atom, dialog: Atom) -> bool {
    types
        .first()
        .is_some_and(|&kind| kind != normal && kind != dialog)
}

/// The monitor's work area: the current desktop's `_NET_WORKAREA` (x, y, width, height), which
/// spans all monitors, cut to the monitor; the whole monitor without one, or if they don't meet.
pub fn work_area(monitor: Rect, workarea: Option<[u32; 4]>) -> Rect {
    let Some([x, y, width, height]) = workarea else {
        return monitor;
    };
    let value = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    let area = Rect {
        left: monitor.left.max(value(x)),
        top: monitor.top.max(value(y)),
        right: monitor.right.min(value(x).saturating_add(value(width))),
        bottom: monitor.bottom.min(value(y).saturating_add(value(height))),
    };
    if area.left < area.right && area.top < area.bottom {
        area
    } else {
        monitor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Openbox's default theme: 1 px borders and a 22 px title bar.
    const EXTENTS: [u32; 4] = [1, 1, 22, 1];

    #[test]
    fn the_frame_is_the_client_area_with_the_window_managers_borders_around_it() {
        assert_eq!(
            frame((101, 122), (400, 300), EXTENTS),
            rect(100, 100, 502, 423)
        );
        assert_eq!(frame((5, 5), (10, 10), [0; 4]), rect(5, 5, 15, 15));
    }

    #[test]
    fn a_move_puts_the_frame_corner_on_the_target_and_sizes_the_client_inside_it() {
        assert_eq!(
            moveresize(rect(100, 100, 502, 423), EXTENTS),
            [0x2f01, 100, 100, 400, 300]
        );
        // A monitor left of or above the primary one has negative coordinates.
        let [_, x, y, ..] = moveresize(rect(-1920, -5, 0, 400), EXTENTS);
        assert_eq!((x as i32, y as i32), (-1920, -5));
    }

    #[test]
    fn barriers_line_the_card_display_and_let_the_pointer_only_leave() {
        let lines = barrier_lines(rect(1280, 0, 2560, 1024));
        assert_eq!(
            lines,
            [
                Some((1280, 0, 1280, 1024, BarrierDirections::NEGATIVE_X)),
                Some((2560, 0, 2560, 1024, BarrierDirections::POSITIVE_X)),
                Some((1280, 0, 2560, 0, BarrierDirections::NEGATIVE_Y)),
                Some((1280, 1024, 2560, 1024, BarrierDirections::POSITIVE_Y)),
            ]
        );
        assert_eq!(barrier_lines(rect(-10, 0, 100, 100))[0], None);
    }

    #[test]
    fn only_normal_windows_and_dialogs_are_guarded() {
        let (normal, dialog, dock) = (10, 11, 12);
        assert!(!is_tool(&[], normal, dialog));
        assert!(!is_tool(&[normal], normal, dialog));
        assert!(!is_tool(&[dialog, dock], normal, dialog));
        assert!(is_tool(&[dock, normal], normal, dialog));
    }

    #[test]
    fn the_work_area_is_the_monitor_less_the_panels_on_it() {
        let primary = rect(0, 0, 1920, 1080);
        let card = rect(1920, 0, 3840, 1080);
        // A 40 px panel along the bottom of a 3840 px wide desktop.
        let workarea = Some([0, 0, 3840, 1040]);
        assert_eq!(work_area(primary, workarea), rect(0, 0, 1920, 1040));
        assert_eq!(work_area(card, workarea), rect(1920, 0, 3840, 1040));
        assert_eq!(work_area(primary, None), primary);
        assert_eq!(work_area(card, Some([0, 0, 1920, 1080])), card);
    }
}
