# Linux mirror capture: the calls cardcast makes

How `src/mirror/linux` captures the source monitor (#42; ADR 0004; `docs/research/linux-support.md`,
section 1). Present (#43) takes the frames through `Picture::take_frame`.

## Wayland: the ScreenCast portal (`mirror/linux/portal.rs`)

All on the session bus, to `org.freedesktop.portal.Desktop` at `/org/freedesktop/portal/desktop`,
interface `org.freedesktop.portal.ScreenCast` [1], over the zbus 5 the tree already has (blocking API).

1. Read the properties `version` and `AvailableCursorModes` [1]. If the portal is missing, the start
   stops with "Screen capture needs xdg-desktop-portal and your desktop's portal backend."
2. Every request below passes a `handle_token`. The portal answers with a `Response(u, a{sv})`
   signal on `/org/freedesktop/portal/desktop/request/SENDER/TOKEN`, where SENDER is the caller's
   unique name without the `:` and with `.` as `_` [2]. cardcast subscribes to that path *before*
   the call, so the answer can't be missed [2]. Response 0 is success, 1 cancelled by the user
   (the mirror stops and says so), 2 any other failure [2].
3. `CreateSession({handle_token, session_handle_token})`: the result `session_handle` (`s`) [1].
4. `SelectSources(session, options)` [1]:
   - `types` = 1 (MONITOR), `multiple` = false;
   - `cursor_mode` = 2 (Embedded: the compositor draws the pointer in), only if
     `AvailableCursorModes` has bit 2 (portal version 2+);
   - from version 4: `persist_mode` = 2 ("until explicitly revoked") and, if settings hold one, the
     saved `restore_token`. A token is single-use; an invalid or spent one is ignored and the dialog
     shows [1].
5. `Start(session, "", {handle_token})`: the dialog, unless the token restored the choice. Results:
   `streams` `a(ua{sv})`, each a PipeWire node id with `position` `(ii)` and `size` `(ii)` in
   compositor coordinates [1], and `restore_token` `s` when persisted [1].
6. `OpenPipeWireRemote(session, {})`: a file descriptor for a PipeWire connection that sees only
   the shared stream [1].

cardcast keeps the bus connection open as long as the stream, and doesn't call `Session.Close`;
dropping the stream drops the connection.

**Matching the stream to the source monitor** (`linux::shows`): monitors never overlap, so the
stream's `position` must be the source monitor's top-left; without a position its `size` must be
the monitor's size on the desktop or in pixels; with neither it can't be told and is taken. A
different monitor stops the mirror ("…is not the source monitor. Press Start and share NAME.") and
its token is not kept, so the next Start shows the dialog.

**The restore token** is saved in settings (`restore_token=` in `settings.txt`). `linux::capture`
takes the saved one and returns the new one to save; any failure spends the old one, so the caller
clears it. Wiring it between `Settings` and the mirror is #43's, which owns the Linux `Mirror`.

## The PipeWire stream (`mirror/linux/stream.rs`)

The `pipewire` crate (0.10, MIT) on its own thread: `pw_context_connect_fd` with the portal's fd,
then `pw_stream_connect` (input, the stream's node, `AUTOCONNECT`) with two `EnumFormat` params,
as PipeWire's DMA-BUF guide asks [3]:

1. Video/raw in BGRx, BGRA, RGBx or RGBA (DRM `XRGB8888`, `ARGB8888`, `XBGR8888`, `ABGR8888`),
   with a `modifier` property, flags `MANDATORY | DONT_FIXATE`, listing the modifiers the present can
   import. Left out when the present gives none.
2. The same without a modifier: the shared-memory fallback.

On `param_changed` for the `Format`, a negotiated modifier (`SPA_VIDEO_FLAG_MODIFIER`) means
DMA-BUF: cardcast sets `Buffers.dataType` to `1 << DmaBuf`, else `1 << MemFd | 1 << MemPtr` [3].
Each `process` takes the newest buffer:

- **DmaBuf**: a duplicate of the plane's fd, with the chunk's offset and stride and the format's
  modifier, for the present to import (wgpu: research section 1). Single plane only.
- **MemFd**: read with `pread` from the fd at `mapoffset + chunk.offset`, not mapped. Measured in
  CI's container (Ubuntu 26.04, xdg-desktop-portal-wlr): its shared-memory buffers carry only
  `SPA_DATA_FLAG_MAPPABLE`, not `READABLE`, and reading PipeWire's own mapping
  (`PW_STREAM_FLAG_MAP_BUFFERS`) crashed with SIGSEGV.
- **MemPtr**: copied from the data pointer.

A stream that goes to `Error` or `Unconnected` (for example the monitor was unplugged) makes the
next `take_frame` return `Ended::Restart`.

## X11: MIT-SHM (`mirror/linux/x11.rs`)

x11rb with its `shm` and `xfixes` features. `ShmCreateSegment` (MIT-SHM 1.2) has the X server make
the shared memory and hand back its fd [4]; each `take_frame` calls `ShmGetImage` (ZPixmap, all
planes) for the source monitor's rectangle of the root window [4] and reads the segment, a CPU copy
per frame. Only a 24-bit desktop in 32-bit little-endian pixels (`XRGB8888`) is taken.
`XFixesGetCursorImage` (XFixes 4) gives the pointer's premultiplied ARGB image, position and
hotspot [5], blended into the frame by `linux::draw_pointer`.

## CI (`.github/workflows/ci.yml`)

`mirror::linux::tests::desktop_captures_a_frame_of_the_source_monitor` needs a frame of
`CARDCAST_EXPECT_COLOUR` from output `CARDCAST_CAPTURE`, and on Wayland a restore token:

- headless sway (pixman) with `output * bg #3366cc solid_color`, PipeWire, WirePlumber, and
  xdg-desktop-portal-wlr set to `chooser_type=none`, `output_name=HEADLESS-1` [6], on a private
  session bus. This takes the shared-memory path; DMA-BUF needs a real graphics card (owner check).
- Xvfb with `xsetroot -solid '#cc6633'`; `-noreset` stops the server resetting the root window when
  xsetroot disconnects.

## Sources

[1] xdg-desktop-portal, `org.freedesktop.portal.ScreenCast.xml`,
<https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.portal.ScreenCast.xml>
[2] xdg-desktop-portal, `org.freedesktop.portal.Request.xml`,
<https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.portal.Request.xml>
[3] PipeWire, DMA-BUF sharing,
<https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/master/doc/dox/internals/dma-buf.dox>
[4] MIT-SHM 1.2 in xcb-proto (`CreateSegment`, `GetImage`), <https://gitlab.freedesktop.org/xorg/proto/xcbproto/-/blob/master/src/shm.xml>
[5] XFixes protocol, <https://gitlab.freedesktop.org/xorg/proto/xorgproto/-/blob/master/fixesproto.txt>
[6] xdg-desktop-portal-wlr(5),
<https://github.com/emersion/xdg-desktop-portal-wlr/blob/master/xdg-desktop-portal-wlr.5.scd>
