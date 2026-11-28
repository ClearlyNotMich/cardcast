# cardcast on Linux: full parity, any desktop

Question (#36): can cardcast reach Windows parity on Plasma Wayland, GNOME Wayland, wlroots (sway,
Hyprland) and X11, shipped as a tarball and an AUR package? Researched 2026-11-27 (branch
`research-linux` from `v1@73c4777`); "probe" marks read-only checks on the sender (CachyOS, KWin
6.8.3, PipeWire 1.6.12, xdg-desktop-portal 1.22.1, RTX 4080 `card1`, AMD iGPU `card0`).

Short answer: mirror, audio, monitors and input switching work everywhere through standard
interfaces. Both guards need a helper per desktop family; other wlroots compositors get none.
The existing seams (`Mirror`, `Guard`, `Relay`, `monitors()`, `playback_devices()`) already fit.

## Support matrix

| Subsystem | Plasma Wayland | GNOME Wayland | wlroots (sway, Hyprland) | X11 |
| --- | --- | --- | --- | --- |
| Mirror capture | yes: portal + `restore_token` | yes: portal + `restore_token` | yes: portal (xdpw/xdph) | yes: MIT-SHM (CPU copy) |
| HDR source monitor | yes, compositor converts (8-bit stream) | unverified | unverified | not researched |
| Mirror present | yes: layer-shell overlay | helper: fullscreen toplevel takes focus unless extension | yes: layer-shell overlay | yes: override-redirect |
| Audio relay | yes (PipeWire) | yes | yes | yes, if PipeWire runs |
| Monitor list, identity, hotplug | yes | yes | yes | yes |
| Window guard | helper: KWin script | helper: Shell extension | helper: IPC (sway, Hyprland); no on others | yes: EWMH |
| Pointer guard | helper: fake-input warp (fragile) | helper: Shell extension | helper: IPC (sway, Hyprland); no on others | yes: XFixes barriers + warp |
| Input switching (#35) | yes (i2c-dev) | yes | yes | yes |

## 1. Mirror capture

- **Portal.** `org.freedesktop.portal.ScreenCast`: `CreateSession`, `SelectSources` (types
  MONITOR, `cursor_mode`), `Start` (usually a dialog), then `OpenPipeWireRemote` hands over the
  PipeWire streams [1]. Cursor modes: 1 Hidden, 2 Embedded, 4 Metadata [1]. The sender's portal is
  version 6 with source types 7 and cursor modes 7 (probe), so Embedded draws the pointer for us.
- **No dialog after the first.** `persist_mode = 2` keeps the permission "until explicitly
  revoked" and `Start` returns a `restore_token`; tokens are single-use, so save the new one after
  each start; a token is ignored (the dialog comes back) when its monitor is gone [1]. The user
  picks the monitor in the dialog, so cardcast must check the stream is the chosen source monitor:
  stream properties carry `position` and `size` in compositor space, not a connector name [1].
  Unsandboxed apps get a stable app id only via `org.freedesktop.host.portal.Registry.Register`
  before any portal call, with an id matching a `.desktop` file [2]; the package must ship one.
- **No dialog at all (later, if needed).** KWin: `zkde_screencast_unstable_v1.stream_output` [5],
  hidden only from sandboxed clients [6] (v6 on the sender, probe) but "regular clients must not
  use" it [5]. Mutter: private `org.gnome.Mutter.ScreenCast.RecordMonitor(connector)` [7]. xdpw:
  `chooser_type=none` + `output_name=` in its config [8].
- **DMA-BUF vs SHM.** The consumer offers two `EnumFormat` params, DMA-BUF with modifiers first and
  shared memory as fallback; buffers then arrive as `SPA_DATA_DmaBuf` or `MemFd`/`MemPtr` [3].
  wgpu 30 imports a single-plane DMA-BUF with `wgpu_hal::vulkan::Device::texture_from_dmabuf_fd`
  (needs `Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`) and wraps it with
  `Device::create_texture_from_hal` [14]. KWin composites on the RTX 4080 here (linux-dmabuf main
  device `card1`, NVIDIA block-linear modifiers; probe), so the mirror's wgpu device should be the
  compositor's main device [11] to import without a copy.
- **HDR and cost.** KWin renders the scene again for each output stream, in 8-bit formats only
  (`ARGB8888`, `XRGB8888`, …) [4]: the compositor does HDR-to-SDR (cardcast's shader unused), on
  the game's GPU. Colours vs the OBS projector and the Forza/PresentMon lines are owner checks.
  Mutter and wlroots were not checked.
- **X11.** `ShmGetImage` copies a drawable into shared memory [18] (a CPU copy per frame, cost
  unmeasured); `XFixesGetCursorImage` gives the pointer [17] for `mirror/pointer.rs` to draw.

## 2. Mirror present

- **Wayland.** `xdg_toplevel.set_fullscreen(output)` names a preferred output, subject to compositor
  policy [9], and a new toplevel on GNOME gets focus by default (`focus-new-windows` = `smart`)
  [15]. `zwlr_layer_shell_v1.get_layer_surface(surface, output, layer, …)` pins a surface to one
  output, and `keyboard_interactivity` `none` (the default) means "never assign it the keyboard
  focus" [10]. KWin, Hyprland, sway and most wlroots compositors implement layer-shell; Mutter does
  not [10]. So: overlay layer surface on the card display (Plasma, wlroots), fullscreen toplevel on
  GNOME with the extension (section 5) keeping focus off it.
- **Pacing.** `wl_surface.frame` callbacks say when to draw next [12]; present with `Fifo`, and
  `wp_presentation` (KWin, Mutter, sway, Hyprland) can confirm the card display's refresh [12].
- **Across GPUs.** Clients make buffers the compositor's main device can import [11]; the
  compositor then shows them on an output of another GPU, like ADR 0003's cross-adapter present.
  The card display is on the iGPU here (`card0-HDMI-A-2`, probe).
- **wgpu.** winit 0.30's Wayland extensions offer no layer-shell [13], so the mirror thread gets
  its own Wayland connection (smithay-client-toolkit) and hands the `wl_surface` to wgpu through
  `SurfaceTargetUnsafe::RawHandle` [13]. On X11: an override-redirect window at the card display's
  rectangle (no window manager involved, so no focus); `_NET_WM_STATE_FULLSCREEN` is the managed
  alternative [16].
- **Not chosen:** a DRM lease (`wp_drm_lease_device_v1`, implemented by KWin, Mutter, sway,
  Hyprland [27]) would take the card display off the desktop entirely, and wgpu has
  `SurfaceTargetUnsafe::Drm` [13]. But KWin offers only `non-desktop` connectors for lease [27],
  a kernel property from EDID [22], so it needs a root-level EDID override.

## 3. Audio relay

- `libpipewire-module-loopback` passes a capture stream to a playback stream; `capture.props` with
  `stream.capture.sink = true` and `target.object` (a `node.name` or `object.serial` [21]) records
  a sink's monitor ports [20]. The audio source itself keeps playing (owner check).
- **Finding the card display's sink (Automatic).** On the sender the iGPU HDMI sink has
  `node.nick` and `alsa.name` = `Elgato1080HDR`, the same name as the card display's EDID and the
  ALSA ELD (`/proc/asound/card2/eld#0.0`), and its `device.bus-path` `pci-0000:0c:00.1` is the
  audio function of the iGPU `0000:0c:00.0` that drives `card0-HDMI-A-2` (probe). That maps onto
  today's `card_output`: PCI sibling as the container id, EDID name as the name match.

## 4. Monitor list and identity

- `/sys/class/drm/cardN-<connector>/{status,enabled,edid}` lists connectors per GPU, and
  `cardN/device` resolves to the PCI device (`amdgpu` card0, `nvidia` card1; probe). EDID gives the
  name and serial (probe: `Elgato1080HDR`, `DELL AW2523HF` serial `4GZY5P3`).
- Positions, sizes and refresh come from the desktop: `wl_output.name` is unique per compositor
  instance but has no defined format and is not persistent [12]; KWin uses the kernel connector
  names (`DP-2`, `HDMI-A-2`; probe). X11 RandR outputs carry an `EDID` property and send
  `RRScreenChangeNotify`/`RROutputChangeNotify` [19]. Wayland has no primary monitor in its core
  protocol; SPEC's "never the primary monitor" rule needs a Linux reading.
- Hotplug: the kernel sends a `HOTPLUG=1` uevent, optionally with `CONNECTOR` [22] (udev crate).
- Save monitors by EDID identity with the connector as fallback (as `device_path`/`connector` on
  Windows); two GPUs can both have a `DP-1`, so match wl_output to DRM connector by name plus EDID.

## 5. Window guard

- **X11:** `_NET_CLIENT_LIST` lists managed windows; `_NET_MOVERESIZE_WINDOW` moves one through the
  window manager [16]. Yes.
- **Plasma:** a KWin script loaded over D-Bus with `org.kde.kwin.Scripting.loadScript(path, name)`
  (probe). It gets `workspace.windowAdded`, `Window.frameGeometryChanged` and `outputChanged`, and
  can write `Window.frameGeometry` [23][24]; `callDBus` lets it ask cardcast (a small zbus
  service) where a window goes, so the decision stays in `guard/placement.rs` [23]. Helper.
- **GNOME:** only a Shell extension can move other windows: `Meta.Display::window-created`,
  `Meta.Window.move_to_monitor` [28]. The user must install and enable it, and a Wayland session
  cannot restart the shell to load it (restart is X11-only) [28], so a log-out is needed. Helper.
- **sway:** subscribe to `WINDOW` events (`new`, `move`, …) [29] and run
  `[con_id=N] move container to output <name>` [29]; `no_focus` stops new windows taking focus [29].
- **Hyprland:** socket2 events `openwindow`/`movewindow`, commands on `.socket.sock`, and a
  `move({ window, monitor })` dispatcher [30]; the wiki says dispatcher contents are "not guaranteed
  to be stable" and an unclosed request socket freezes Hyprland for up to five seconds [30].
- **Other wlroots (river, labwc, niri …):** `wlr-foreign-toplevel-management` can minimise,
  maximise, fullscreen, activate or close another client's window, not move it [31]. No guard; the
  status line says so.

## 6. Pointer guard

- **X11:** XFixes pointer barriers stop relative pointers at the card display's edges; absolute
  devices and `WarpPointer` ignore them [17], so keep the SPEC's 60 Hz check with a warp. Yes.
- **Wayland, detection:** the mirror's own surface covers the card display, so `wl_pointer.enter`
  on it means the pointer is there [12]. `wp_pointer_warp_v1` only warps within the client's own
  focused surface [26], so the warp back is compositor-specific:
  - **Plasma:** KWin's script `cursorPos` is read-only [24]; `org_kde_kwin_fake_input`
    `pointer_motion_absolute` works for unsandboxed clients, and its `authenticate` is a
    `// TODO: make secure` that accepts anyone [25][6]. Works today; may close in a later KWin.
  - **GNOME:** the extension, with `Clutter.Seat.warp_pointer` [28].
  - **sway:** `seat <seat> cursor set <x> <y>` [29]. **Hyprland:** `cursorpos` and a cursor
    `move({ x, y })` dispatcher [30].
  - Other wlroots: no.
- **Built (#49):** the mirror may be off, and sway's IPC has no pointer position, so on Plasma, sway
  and Hyprland the pointer guard has its own transparent layer-shell overlay on the card display
  (input on, keyboard off; the mirror's overlay takes no input, so the pointer reaches it) and warps
  from its `wl_pointer.enter`/`motion` to the nearest point on another monitor; no timer. A pointer
  a game locks or confines (pointer-constraints) never reaches the overlay; KWin also applies them to
  fake input (`pointer_input.cpp`, `updatePosition`). GNOME: the extension hears Mutter's cursor
  tracker `position-invalidated` (every motion, warps included) and warps with
  `Clutter.Seat.warp_pointer`. X11: barriers plus the SPEC's 16 ms check with `PointerHistory`, and
  no warp while another client holds a pointer grab (probed with `GrabPointer`). Gaps: on Wayland
  the pointer can stay on the card display during a button-held drag that started off it (the drag
  keeps the pointer focus), and on X11 during any active grab.

## 7. Input switching (#35)

DDC/CI needs read/write on the GPU's `/dev/i2c-*`; ddcutil's `60-ddcutil-i2c.rules` tags those
nodes `uaccess` [32], and the `i2c-dev` module must be loaded (a `modules-load.d` file) [32]. On the
sender both hold: `i2c_dev` loaded, ACL `user:mich:rw-` (probe). NVIDIA's proprietary driver may need
`NVreg_RegistryDwords=RMUseSwI2c=0x01` [32]. libddcutil is GPL-2.0-or-later (pacman probe), which
clashes with MIT OR Apache-2.0 binaries; use the MIT `ddc-i2c` crate (last release 2023) or the raw
protocol [33]. Ship our own uaccess rule and `modules-load.d` entry in the package.

## 8. Architecture

- Keep today's seam: `ui` only sees `Mirror`, `Guard`, `Relay`, `monitors()`, `playback_devices()`
  and `DisplayWatcher`, chosen by `cfg(windows)`. Add `cfg(target_os = "linux")` implementations
  beside the Windows ones (`mirror/linux.rs`, `guard/linux/{x11,kwin,gnome,sway,hyprland}.rs`,
  `audio/linux.rs`); stand-ins move to `cfg(test)` for the snapshots. No new traits.
- One plain function picks the desktop from `XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP`,
  `SWAYSOCK`, `HYPRLAND_INSTANCE_SIGNATURE`; a `match` on it selects the helper. Plain, Linux-tested
  decisions: desktop detection, wl_output↔DRM connector matching, portal stream↔monitor matching,
  Automatic sink choice, and the existing `place()`.
- `Monitor`'s Windows-named fields (`gdi_name`, `container_id`) get OS-neutral meanings;
  `CODING_STANDARDS.md` rule 2 becomes "platform calls only in mirror, guard, audio".
- Crates [33] (licence, latest): `ashpd` 0.14.3 MIT (portal); `pipewire` 0.10.1 MIT (needs
  libpipewire headers and Clang to build [34]); `smithay-client-toolkit` 0.21.1,
  `wayland-client` 0.31, `wayland-protocols(-wlr, -plasma)` MIT; `x11rb` 0.14 MIT OR Apache-2.0;
  `zbus` 6.5 MIT; `udev` 0.9.3 MIT (last release 2025-01); `ddc-i2c` 0.2.2 MIT. The tree already
  pulls `smithay-client-toolkit` 0.19/0.20, `wayland-client` 0.31.15, `x11rb` 0.13.2 and `zbus`
  5.19 through eframe (`Cargo.lock`): pin to those to avoid duplicates. Avoid `hyprland` (GPL-3.0)
  [33]; sway and Hyprland IPC are small enough to speak directly.

## 9. CI

- `ubuntu-24.04` has `xvfb` and `dbus`; Noble ships sway 1.9, xdg-desktop-portal-wlr 0.7.1,
  PipeWire 1.0.5, openbox, but KWin 5.27 [35]. A job can run in the official `archlinux` container
  (`jobs.<id>.container`) [35], which matches the sender and has KWin 6.
- Plan: (1) X11 guard: Xvfb + openbox, EWMH move tests. (2) Present, capture, IPC guard: headless
  sway (`WLR_BACKENDS=headless`, `WLR_HEADLESS_OUTPUTS=2` [36]) + xdpw `chooser_type=none` [8] +
  pipewire/wireplumber on a session D-Bus. (3) Audio: relay between two null sinks. (4) KWin script:
  `kwin_wayland --virtual --output-count 2` (probe `--help`) in the Arch container. GNOME, NVIDIA,
  HDR colours and game load stay owner checks.

## 10. Packaging

- Tarball: `cardcast`, licence texts, README, a `.desktop` file (portal app id [2]), the i2c udev
  rule and `modules-load.d` file. The repo has no licence files yet (Cargo.toml says MIT OR
  Apache-2.0); add them. Runtime: PipeWire, xdg-desktop-portal + backend, Vulkan loader + driver,
  libwayland/libxkbcommon (winit dlopens them [13]) or libX11. Build on the oldest runner (glibc).
- AUR: prebuilt deliverables with sources available take the `-bin` suffix; a versioned source
  build has none [37]. Start with `cardcast-bin` from the tarball CI built; a source `cardcast`
  follows `cargo fetch --locked` / `cargo build --frozen --release` [37]. Licences go in
  `/usr/share/licenses/$pkgname`; commit `PKGBUILD` and `.SRCINFO` (`makepkg --printsrcinfo`) [37].
- Owner only: an AUR account, an SSH key pair (`ssh-keygen -f ~/.ssh/aur`) with the public key in
  "My Account", then `git clone ssh://aur@aur.archlinux.org/cardcast-bin.git` and push [37].
  Undo `video=HDMI-A-2:d` on the sender before testing (#36).

## Staged recommendation

(0) ADR "cardcast also runs on Linux" (reverses SPEC's non-goal and AGENTS.md's Windows-only),
Linux monitor list. (1) Mirror (portal + PipeWire + wgpu; layer-shell/override-redirect), audio
relay, tarball and `cardcast-bin`. (2) Input switching. (3) Window guard: KWin script first (the
sender), then X11, sway/Hyprland, GNOME. (4) Pointer guard where a warp exists.

Risks: a portal dialog per new source monitor; KWin fake-input and Hyprland dispatchers may change
[25][30]; GNOME needs a user-installed extension; compositor re-rendering on the game's GPU is
unmeasured; NVIDIA DMA-BUF import into wgpu is unproven; X11 capture is a CPU copy.

## Tickets (dependency order; AC = acceptance, Owner = owner-checked)

1. **ADR 0004 and SPEC: Linux is supported.** AC: ADR with this matrix; SPEC 3/8/9, AGENTS.md, rule 2
   updated; stand-ins behind `cfg(test)`; Linux build and snapshots pass. Owner: approves the ADR.
2. **Linux monitor list, identity, hotplug.** AC: sysfs + wl_output/RandR, saved by EDID;
   detection and matching unit-tested; replug updates dropdowns. Owner: the four monitors are right.
3. **Mirror capture: portal + PipeWire.** AC: Embedded cursor, `persist_mode=2`, token saved,
   DMA-BUF with SHM fallback, stream matched to the monitor; CI sway + xdpw frame. Owner: no dialog
   after a restart.
4. **Mirror present.** AC: layer-shell / fullscreen toplevel / override-redirect, Fifo, `presents()`;
   CI sway and Xvfb. Owner: OBS 1080p60 with pointer, focus and 500 Hz kept, HDR colours, Forza 2%.
5. **Audio relay over PipeWire.** AC: monitor-to-sink loopback; Automatic by ELD name + PCI sibling,
   unit-tested; CI null sinks. Owner: receiver hears it, sender still hears it, No audio works.
6. **Tarball and `cardcast-bin`.** AC: CI attaches the tarball to releases; `PKGBUILD`/`.SRCINFO`
   pass `makepkg` in the Arch container. Owner: AUR account, SSH key, first push.
7. **Input switching on Linux (#35).** AC: DDC/CI over i2c-dev without root; udev rule and
   `modules-load.d` shipped. Owner: command mode toggles the AW2523HF input.
8. **Window guard: KWin script + X11 EWMH.** AC: SPEC 9 guard line; decisions in `place()`; CI
   Xvfb+openbox and `kwin_wayland --virtual`. Owner: Plasma on the sender.
9. **Window guard: sway, Hyprland, GNOME extension.** AC: IPC moves (CI sway); extension bundled;
   status-line notice elsewhere. Owner: GNOME/Hyprland if available.
10. **Pointer guard where possible.** AC: X11 barriers + warp, Plasma fake-input, sway/Hyprland IPC,
    GNOME extension; notice elsewhere. Owner: pointer can't be parked on the card display.

## Sources

[1] xdg-desktop-portal `org.freedesktop.portal.ScreenCast.xml`, <https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.portal.ScreenCast.xml> · [2] `org.freedesktop.host.portal.Registry.xml`, <https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.host.portal.Registry.xml>  
[3] PipeWire, DMA-BUF sharing, <https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/master/doc/dox/internals/dma-buf.dox> · [4] KWin `src/plugins/screencast/outputscreencastsource.cpp` and `screencaststream.cpp`, <https://invent.kde.org/plasma/kwin/-/tree/master/src/plugins/screencast>  
[5] KDE screencast protocol, <https://wayland.app/protocols/kde-zkde-screencast-unstable-v1> · [6] KWin `src/wayland_server.cpp` (`restrictedInterfaces`, `allowInterface`), <https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland_server.cpp>  
[7] Mutter `org.gnome.Mutter.ScreenCast.xml`, <https://gitlab.gnome.org/GNOME/mutter/-/blob/main/data/dbus-interfaces/org.gnome.Mutter.ScreenCast.xml> · [8] xdg-desktop-portal-wlr(5), <https://github.com/emersion/xdg-desktop-portal-wlr/blob/master/xdg-desktop-portal-wlr.5.scd>  
[9] xdg-shell, <https://wayland.app/protocols/xdg-shell> · [10] wlr layer shell (with compositor support table), <https://wayland.app/protocols/wlr-layer-shell-unstable-v1>  
[11] linux-dmabuf v1 feedback, <https://wayland.app/protocols/linux-dmabuf-v1> · [12] Wayland core (`wl_surface.frame`, `wl_output.name`, `wl_pointer.enter`), <https://wayland.app/protocols/wayland>; presentation-time, <https://wayland.app/protocols/presentation-time>  
[13] wgpu 30.0.2 `SurfaceTargetUnsafe`, <https://docs.rs/wgpu/30.0.2/wgpu/enum.SurfaceTargetUnsafe.html>; winit 0.30.13 `platform::wayland`, <https://docs.rs/winit/0.30.13/winit/platform/wayland/index.html> · [14] wgpu-hal 30.0.2 `vulkan::Device::texture_from_dmabuf_fd`, <https://docs.rs/wgpu-hal/30.0.2/wgpu_hal/vulkan/struct.Device.html>; wgpu `Features`, `Device::create_texture_from_hal`, <https://docs.rs/wgpu/30.0.2/wgpu/struct.Device.html>  
[15] gsettings-desktop-schemas `org.gnome.desktop.wm.preferences` `focus-new-windows`, <https://gitlab.gnome.org/GNOME/gsettings-desktop-schemas/-/blob/master/schemas/org.gnome.desktop.wm.preferences.gschema.xml.in> · [16] Extended Window Manager Hints, sections 3–5, <https://specifications.freedesktop.org/wm-spec/latest/>  
[17] XFixes protocol 6.1 (GetCursorImage, section 12 pointer barriers), <https://gitlab.freedesktop.org/xorg/proto/xorgproto/-/blob/master/fixesproto.txt> · [18] MIT-SHM, <https://www.x.org/releases/current/doc/xextproto/shm.html>  
[19] RandR protocol (`EDID` property, change notifications), <https://gitlab.freedesktop.org/xorg/proto/xorgproto/-/blob/master/randrproto.txt> · [20] PipeWire module-loopback, <https://docs.pipewire.org/page_module_loopback.html>; pw-loopback(1), <https://docs.pipewire.org/page_man_pw-loopback_1.html>  
[21] pipewire-props(7) `target.object`, <https://docs.pipewire.org/page_man_pipewire-props_7.html> · [22] Linux kernel KMS (`non_desktop`, hotplug uevents), <https://docs.kernel.org/gpu/drm-kms.html>, <https://docs.kernel.org/gpu/drm-kms-helpers.html>  
[23] KWin scripting API (KWin 6), <https://develop.kde.org/docs/plasma/kwin/api/> · [24] KWin `src/window.h` (`frameGeometry` WRITE `moveResize`), `src/scripting/workspace_wrapper.h` (`cursorPos` READ only), <https://invent.kde.org/plasma/kwin/-/tree/master/src>  
[25] KWin `src/backends/fakeinput/fakeinputbackend.cpp` (`authenticate`), <https://github.com/KDE/kwin/blob/master/src/backends/fakeinput/fakeinputbackend.cpp> · [26] pointer-warp-v1, <https://wayland.app/protocols/pointer-warp-v1>  
[27] drm-lease-v1, <https://wayland.app/protocols/drm-lease-v1>; KWin `src/wayland/drmlease_v1.cpp` (`isNonDesktop`), <https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/drmlease_v1.cpp> · [28] gjs.guide, Creating an extension, <https://gjs.guide/extensions/development/creating.html>; Mutter API: <https://gnome.pages.gitlab.gnome.org/mutter/meta/signal.Display.window-created.html>, <https://gnome.pages.gitlab.gnome.org/mutter/meta/method.Window.move_to_monitor.html>, <https://gnome.pages.gitlab.gnome.org/mutter/clutter/method.Seat.warp_pointer.html>  
[29] sway(5), sway-input(5), sway-ipc(7), <https://github.com/swaywm/sway/tree/master/sway> · [30] Hyprland wiki: IPC, Dispatchers, Using hyprctl, <https://github.com/hyprwm/hyprland-wiki/tree/main/content>  
[31] wlr foreign toplevel management, <https://wayland.app/protocols/wlr-foreign-toplevel-management-unstable-v1> · [32] ddcutil docs: I2C device permissions, kernel module, NVIDIA, <https://www.ddcutil.com/i2c_permissions/>, <https://www.ddcutil.com/kernel_module/>, <https://www.ddcutil.com/nvidia/>  
[33] crates.io API, 2026-11-27 (`ashpd`, `pipewire`, `smithay-client-toolkit`, `wayland-*`, `x11rb`, `zbus`, `udev`, `ddc-i2c`, `ddc-hi`, `swayipc`, `hyprland`), <https://crates.io/api/v1/crates/> · [34] pipewire-rs README, <https://gitlab.freedesktop.org/pipewire/pipewire-rs/-/blob/main/README.md>  
[35] Ubuntu 24.04 runner image, <https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md>; <https://packages.ubuntu.com/noble/>; GitHub, Running jobs in a container, <https://docs.github.com/en/actions/writing-workflows/choosing-where-your-workflow-runs/running-jobs-in-a-container>; <https://hub.docker.com/_/archlinux> · [36] wlroots `docs/env_vars.md`, <https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/master/docs/env_vars.md>  
[37] ArchWiki: AUR submission guidelines, Rust package guidelines, Arch package guidelines, <https://wiki.archlinux.org/title/AUR_submission_guidelines>, <https://wiki.archlinux.org/title/Rust_package_guidelines>, <https://wiki.archlinux.org/title/Arch_package_guidelines>  
