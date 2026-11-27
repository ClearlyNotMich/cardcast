# cardcast also runs on Linux

On 2026-11-27 the owner asked for cardcast to run on Linux with full parity and to be released there, on any desktop (X11 and Wayland), as a tarball plus an AUR package. This reverses SPEC's "Windows-only" and AGENTS.md's "Windows-only app". The reference Linux sender is the owner's: CachyOS, KDE Plasma on Wayland, an RTX 4080 with the card display on the AMD integrated GPU.

The Linux build keeps today's module interfaces (the mirror, guard and audio relay traits and the plain decision functions) and puts the Linux implementations behind `cfg(target_os = "linux")` modules, with no new traits. One plain function detects the desktop. The Linux stand-ins move behind `cfg(test)`, so a Linux build is a real cardcast. Per subsystem, from `docs/research/linux-support.md`:

- **Mirror capture**: the xdg-desktop-portal ScreenCast with PipeWire on every Wayland desktop, with a permission dialog only the first time (`persist_mode` plus a saved `restore_token`); MIT-SHM on X11 (a CPU copy).
- **Mirror present**: a layer-shell overlay on Plasma and wlroots, override-redirect on X11, and on GNOME a fullscreen window that needs the Shell extension so it doesn't take focus.
- **Audio relay**: a PipeWire loopback. Automatic finds the card display's sink by name plus the matching graphics card.
- **Window guard and pointer guard**: per-desktop helpers (a KWin script, a GNOME Shell extension, sway/Hyprland IPC, and EWMH plus XFixes barriers on X11). Where no helper exists, the status line says so instead of pretending.
- **Input switching (#35)**: DDC/CI over i2c-dev without root.

## Considered options

- **Windows only (status quo)**: rejected by the owner.
- **A separate Linux app**: rejected, because two codebases would drift. The decisions are already platform-free plain functions.
- **New platform traits for every subsystem**: rejected for now. The existing traits are the seam, and a new trait would be Speculative Generality.

## Consequences

The cardcast licence became GPL-3.0-or-later on 2026-11-27 (owner decision, `LICENSE`), and only GPL-compatible crates may be added; the research avoided none it needed. CI grows Linux jobs: Xvfb with openbox for X11, headless sway with xdg-desktop-portal-wlr and PipeWire, and `kwin_wayland --virtual` in an Arch container. GNOME, NVIDIA DMA-BUF import, HDR and the game-load lines stay owner checks. KWin fake-input and Hyprland dispatchers are unstable interfaces, so the guards there may break with compositor updates. Every SPEC section 9 line now needs a Linux column.
