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

**Matching the stream to the source monitor** (`linux::started`): monitors never overlap, so a
`position` names one: it must be the source monitor's top-left. Without a position, the `size`
(on the desktop or in pixels) must be the source monitor's and no other monitor's. Anything else
is refused, never guessed: another monitor ("…is not the source monitor. Press Start and share
NAME.") or a stream that can't be told apart (same size as another monitor, or neither position nor
size). A refused stream's token is not kept, so the next Start shows the dialog.

**The restore token** is saved in settings (`restore_token=` in `settings.txt`). `linux::capture`
takes the saved one and returns `Capture { picture, restore_token }`; the caller always saves
`restore_token`. Once `Start` shared the source monitor it is the new token, even if the PipeWire
stream then fails to open, so the automatic restart shows no dialog; after a portal failure or a
refused stream it is `None`; on X11 it is the old one. Wiring it between `Settings` and the mirror
is #43's, which owns the Linux `Mirror`.

## What the present gets (`mirror::Frame`)

`Picture::take_frame() -> Result<Option<Frame>, Ended>`: the newest frame since the last call,
`None` if none came, `Err` once the stream ended.

```rust
pub struct Frame { pub width: u32, pub height: u32, pub fourcc: u32, pub pixels: Pixels, pub release: Release }
pub enum Pixels {
    Memory { bytes: Vec<u8>, stride: u32 },
    DmaBuf { fd: OwnedFd, offset: u32, stride: u32, modifier: u64 },
}
```

A DMA-BUF frame holds its PipeWire buffer: the compositor doesn't render into that plane again
until the `Frame` (its `Release`) is dropped, which sends the buffer back to the PipeWire thread to
queue. A frame replaced before the present took it is dropped, and so released, the same way. The
stream asks for 3 to 16 buffers (`Buffers.buffers`), so with one frame held by the present and one
waiting the compositor still has one to draw in; the present must drop each frame once it no
longer reads the plane (after the import's GPU work is done), or the stream stalls. A buffer the
stream removed meanwhile (a new format) is not queued back. Memory frames are copies: their buffer
goes back at once and their `Release` does nothing. `Release::default()` is the no-op, for tests.

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

- **DmaBuf**: a duplicate of the plane's fd (a negative fd is skipped), with the chunk's offset
  and stride and the format's modifier, for the present to import (wgpu: research section 1); the
  buffer is held until the frame is dropped (above). Single plane only.
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

## The present (`mirror/linux/present.rs`, `screen.rs`, `gpu.rs`, #43)

`LinuxMirror` is the Linux `Mirror`. Start returns at once; a thread opens the surface and a wgpu
Vulkan device, then runs `capture` on a second thread that nothing joins, because `portal::share`
blocks until the dialog is answered. Stop joins only the first thread. The capture thread always
stores its `restore_token` and wakes the settings window, which saves it (`take_restore_token`),
even after Stop; its picture then has no receiver and is dropped, ending the stream.

- **Surface** (research section 2): a `zwlr_layer_shell_v1` overlay on the card display's
  `wl_output` (matched by name), anchored to all edges, `exclusive_zone` -1, keyboard
  interactivity none, an empty input region, drawn at the card display's resolution and scaled to
  its logical size with `wp_viewporter`; on X11 an override-redirect window over its RandR
  rectangle with an empty input shape (SHAPE). Clicks go through both. On GNOME the start stops
  with "needs the GNOME extension (Linux 9)". The surface shows with the first picture.
- **Drawing**: `PresentMode::Fifo`, frame latency 1, a black clear, then the picture aspect-fitted
  with `mirror::fit`, each refresh; `presents()` counts the presents. On X11 the loop is also
  capped at the card display's refresh (`present::next_present`), since a driver without vsync
  (lavapipe measured 200-1000 a second) would otherwise grab with MIT-SHM that often.
- **Memory frames** are uploaded with `write_texture`.
- **DMA-BUF frames** are read as wlroots reads a client's DMA-BUF [7]: the device is opened with
  `VK_EXT_queue_family_foreign` added (wgpu-hal's `open_with_callback`), else no modifiers are
  offered. Per frame, cardcast exports the plane's sync file (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`,
  Linux 6.0 [8]; on older kernels it polls the DMA-BUF itself, whose `POLLIN` waits for its
  writers [8]) and waits up to 100 ms for it on the CPU (wgpu's submit takes no wait semaphore);
  then imports it with `texture_from_dmabuf_fd` (single plane, modifiers the device lists with
  `TRANSFER_SRC` for BGRA and RGBA), records an acquire barrier from `VK_QUEUE_FAMILY_FOREIGN_EXT`
  in `GENERAL` layout to `TRANSFER_SRC_OPTIMAL` on the raw command buffer (`as_hal_mut`), copies
  into the kept texture, releases it back to the foreign family in `GENERAL`, waits for the copy
  (bounded) and drops the frame. wgpu is told the texture starts in `COPY_SRC`, so it never
  transitions it from `UNDEFINED`. Only a failed import restarts the mirror without modifiers;
  other failures restart it as they are. Checked locally (headless sway on Vulkan, AMD RADV, an
  AMD tiled modifier); CI has no graphics card, so it takes the shared-memory path.

**Stop** waits at most a second for the mirror thread; one stuck in the compositor (a Fifo present
while the card display sleeps) is left to end on its own. Waiting for the layer surface's first
configure is bounded to 5 seconds.

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
[7] wlroots, `render/vulkan/pass.c` (foreign acquire and release barriers),
`render/vulkan/vulkan.c` (`VK_EXT_queue_family_foreign`), `render/dmabuf_linux.c`
(`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`), <https://gitlab.freedesktop.org/wlroots/wlroots>
[8] Linux, DMA-BUF sharing: "Implicit Fence Poll Support" and `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`,
<https://docs.kernel.org/driver-api/dma-buf.html>, `include/uapi/linux/dma-buf.h`
