# cardcast — Spec

Nov 23, 2026 · @Mich

## 1. Summary

cardcast is a small Windows and Linux program for dual-PC streaming with a capture card (Linux: ADR 0004). It runs on the sender and does three things: it shows a mirror of the source monitor on the card display, it keeps windows and the mouse pointer off the card display, and it plays the chosen audio source into the capture card.

The receiver needs no cardcast software. OBS on the receiver adds the capture card as a video source.

The reason it exists is sender load. Windows duplicate mode took away the source monitor's high refresh rate and added input lag. Extended mode works, but then windows open on a display nobody can see. An OBS fullscreen projector fixes the picture but not the lost windows, and needs OBS running on the sender.

Terms in bold in `GLOSSARY.md` are used with exactly that meaning here. `docs/adr/0001-capture-card-is-the-link.md` explains why the earlier network design was dropped.

## 2. Setup it assumes

- Two PCs. The sender runs the game, on Windows or on Linux (X11 or Wayland: Plasma, GNOME, sway, Hyprland or another compositor); the receiver runs OBS. The reference Linux sender runs CachyOS with KDE Plasma on Wayland.
- A capture card connected to the receiver over USB, with its HDMI input plugged into the sender's graphics card. Any capture card works that shows up on the sender as a monitor; its sound, if it has any, as a playback device (on Linux, a PipeWire sink). Tested reference cards: Elgato HD60 X and AVerMedia Live Gamer Mini.
- The card display is set to **Extend** in Windows, never Duplicate; on Linux it is a separate output in the desktop's display settings, never mirrored.
- The card display may be plugged into the same graphics card as the source monitor, or into another one (a second GPU or the integrated GPU); then the mirror converts the picture on the source monitor's card and Windows copies it across (ADR 0003). On Linux the compositor shows the mirror on the other graphics card's output (`docs/research/linux-support.md`, section 2).
- The source monitor may run with HDR on or off (Windows HDR, or the desktop's HDR on Linux). The card display is always SDR.

## 3. Goals and non-goals

**Goals for v1.** All of these ship together as v1.

- Mirror one source monitor onto the card display at the card display's own resolution and refresh rate. The reference case is a 2560×1440 500 Hz source monitor shown at 1920×1080 60 Hz.
- The source monitor keeps its refresh rate and the game is not measurably slower. See the pass line in section 9.
- When the source monitor is in HDR, the mirror is converted to SDR and its colours look the same on the receiver as the OBS fullscreen projector's do today.
- No window ever stays on the card display, and the mouse pointer cannot stay there either.
- The chosen audio source is played to the sound output, so the capture card carries sound. A capture card without sound works with No audio.
- A settings window with: source monitor dropdown, card display dropdown, "send windows to" dropdown, audio source dropdown, sound output line, Start/Stop button, guard checkbox.
- Choices are remembered. On launch the mirror starts by itself. If the capture card is unplugged and plugged back in, the mirror resumes by itself.
- The same cardcast runs on a Linux sender, on any desktop (Plasma, GNOME, sway, Hyprland on Wayland, and X11), shipped as a tarball and an AUR package (ADR 0004, `docs/research/linux-support.md`). Section 9 gives each part's Linux line. Until a part is built for Linux, the status line says it does not work on Linux yet; it never pretends.

**Non-goals**

- Sending the picture over a network.
- macOS.
- Scenes, sources, overlays, encoding or recording. That is OBS's job on the receiver.
- Mirroring a single window or game instead of a whole monitor.
- Sending HDR to the receiver. The mirror is always SDR.
- A tray icon, an installer, or starting with Windows. These can come after v1.
- Running as administrator. Windows of programs running as administrator are therefore not moved by the window guard; the README says so.
- Hiding the card display from Windows completely. Windows only offers that on Enterprise and Pro for Workstations.

## 4. Mirror

The mirror follows the same three steps as OBS's fullscreen projector, and the picture stays on the graphics card throughout.

1. **Capture.** Get the source monitor's picture as a GPU texture. There are two ways, and ADR 0002 chose Desktop Duplication (provisional; the section 9 capture check can still overturn it):
   - **Windows Graphics Capture, 8-bit output.** Preferred, because it can draw the mouse pointer into the picture itself (build 19041+). That Windows converts HDR to SDR well for an 8-bit frame pool is not documented (it may clip), and turning off the yellow capture border needs build 20348+ and may need package identity; the capture check verifies both empirically. See `docs/win32-mirror-audio.md`.
   - **DXGI Desktop Duplication.** What OBS's Display Capture uses by default on a desktop PC, so it is what the reference sender runs today. It needs more code: the mirror must convert HDR to SDR itself, using the monitor's SDR white level, and must draw the mouse pointer itself.
2. **Window.** Create a borderless, always-on-top window that exactly covers the card display. It must not take focus and must not appear in the taskbar or Alt+Tab.
3. **Draw.** Once per refresh of the card display, take the newest captured frame, draw it into the window's swap chain scaled to fit with the aspect ratio kept, and present it.

Behaviour:

- Pacing comes from the card display. A 500 Hz source monitor shown on a 60 Hz card display produces 60 draws per second, and older frames are skipped.
- cardcast never changes display modes. The user sets the card display's resolution and refresh rate in Windows; 60 Hz is the usual case, and 120 or 240 Hz works when the capture card supports it. Nothing in cardcast assumes 60 Hz.
- The mouse pointer is visible in the mirror whenever it is on the source monitor.
- When capture access is lost (resolution change, a game entering or leaving exclusive fullscreen, a permission prompt), capture is re-created without restarting the program.
- When the card display disappears, the mirror stops and waits. When it returns, the mirror starts again.
- Monitors are remembered by the display's device path from the Windows display configuration, not by `\\.\DISPLAYn` or list position. At run time that path is mapped to its DXGI output, and the Direct3D device is created on the source monitor's graphics card, not the default one.
- The mirror refuses to start when the source monitor and the card display are the same display, or when the card display is the primary monitor.
- The mirror window runs on its own thread with its own message loop, separate from the settings window.

## 5. Guard

The window guard returns any window that lands on the card display to the monitor chosen in the "send windows to" dropdown, which defaults to the primary monitor.

- It reacts whenever a top-level window appears, is restored, or changes position by any means. That includes dragging, Win+Shift+Arrow, and programs that reposition themselves after opening. It also checks every window once at start and after any display change.
- Position events arrive constantly, including one for every mouse movement. The guard discards everything that is not a top-level window at once, then checks the window once a short moment later, not on every event.
- It listens with out-of-context window event hooks on its own thread. It must not install a low-level mouse or keyboard hook, because that would add input lag.
- A window counts as being on the card display when most of its visible frame is there.
- Returning a window must never take focus from the game or change which window is on top, and must not wait on a program that has stopped responding.
- A returned window keeps its size where it fits, and a maximised window ends up maximised on the monitor it is returned to.
- If the same window lands on the card display more than a few times within a minute, the guard stops returning it and reports it in the status line, so it never fights a program in a loop.
- The mirror's own window is never moved. Minimised, hidden, cloaked and tool windows are ignored.
- The window guard works whether or not the mirror is running.
- The decision "given this window and these monitors, where does it go" is a plain function with no Windows calls in it, so it is unit-tested on any OS.

**Pointer guard.** Arranging the card display corner-to-corner in Windows Display settings is not enough; the pointer still slips through the corner. So cardcast checks the pointer's position about 60 times a second, and when it is on the card display, puts it back at the last place it was seen on another monitor. The pointer can therefore be on the card display for about one frame at most. This uses a timer, never a mouse hook. A game that confines the pointer to its own window is unaffected.

One guard checkbox switches the window guard and the pointer guard on and off together.

## 6. Settings window

- Built with `egui` (`eframe`). It must use no CPU while minimised.
- Controls: source monitor dropdown, card display dropdown, "send windows to" dropdown, audio source dropdown, a "Sound output: <choice> · Change" line under it, Start/Stop button, guard checkbox, and the status: the header names the state (starting, running, waiting for card display, waiting for source monitor, restarting, stopped by an error), and a line under the controls gives the error, restart reason or notice in plain words when there is one.
- Dropdowns show each monitor's name, resolution and refresh rate. The dropdowns and the live layout's role menu offer a monitor only for a role it can take: the card display is never the source monitor, the "send windows to" monitor or the primary monitor. If Windows makes the card display the primary monitor, the live layout says so and Start refuses, with the same text. Settings saved by an older version with the source monitor or "send windows to" on the card display load with that choice cleared, and the status line says so.
- When a running mirror stays under 90% of the card display's refresh rate for a few seconds, the status line says so in plain words.
- Settings are saved when changed and loaded at launch. Few settings, safe defaults: an invalid choice is refused, not warned about.
- "Change" opens a dialog with one dropdown (Automatic, the playback devices except the audio source, No audio) and Save / Cancel. When Automatic finds no sound output, the status line says so in plain words and shows a "Choose sound output" button that opens the same dialog. The audio source dropdown never offers the sound output in use.
- The window repaints on user input or when the mirror or window guard reports a status change. While the mirror starts, runs or restarts, its status indicator and the dots travelling along the live layout's arc animate at most about 30 times a second while the window has focus. Without focus nothing animates, and only a running mirror's frame rate refreshes, once a second; never while minimised.
- Closing the window quits cardcast. When launched with saved settings that start the mirror successfully, the window starts minimised.
- On Linux the window runs with the mirror and window guard replaced by stand-ins, so the layout can be checked during development.

## 7. Audio

- The audio source is one playback device on the sender, chosen in the dropdown. cardcast records what that device plays and plays it again on the sound output.
- The sound output setting is Automatic (default), a chosen playback device, or No audio. Automatic picks the one playback device with the card display's container ID (Windows gives a monitor and its HDMI or DisplayPort sound output the same one), else the one playback device other than the audio source named after the card display; a name that fits several devices counts as not found. A chosen device that is not connected falls back to Automatic. The audio source can't be the sound output. No audio runs the mirror without the relay.
- Audio problems never stop or hold up the mirror; they show in the status line.
- The sender keeps hearing the audio source normally.
- Which programs play into the audio source is decided in Windows' own per-app sound settings, not in cardcast.
- Audio starts and stops with the mirror, and resumes with it after the card is re-plugged.

## 8. Tech stack and layout

- Stable Rust, one crate named `cardcast`, one binary. Split into modules: `ui`, `mirror`, `guard`, `audio` and `input` (monitor input switching, section 11).
- Windows APIs through the `windows` crate: Direct3D 11, DXGI, and Win32 windowing and events.
- The program declares per-monitor DPI awareness (version 2) in its manifest, so monitor rectangles and window sizes are real pixels on scaled displays.
- Linux (ADR 0004): the same `Mirror`, `Guard`, `Relay` and `Ddc` traits and plain decision functions, with the Linux implementations in `cfg(target_os = "linux")` modules beside the Windows ones; no new traits for existing subsystems. Capture through the xdg-desktop-portal ScreenCast and PipeWire on Wayland, MIT-SHM on X11; present as a layer-shell overlay (Plasma, wlroots), an override-redirect window (X11) or a fullscreen window plus the Shell extension (GNOME); audio as a PipeWire loopback; the guards through a helper per desktop (KWin script, GNOME Shell extension, sway or Hyprland IPC, EWMH and XFixes on X11). One plain function, `guard::desktop::detect`, picks the desktop from the environment. Crates and versions: `docs/research/linux-support.md`, section 8; where eframe already pulls a crate in, use that version.
- The stand-in mirror, which fakes a running mirror, exists only in tests (`cfg(test)`) for the snapshots; a release build never fakes success.
- `unsafe` only in the `mirror`, `guard`, `audio` and `input` modules, on Windows and Linux alike, each block with a `// SAFETY:` comment.
- Check each crate's current version and maintenance status before adding it.
- Licence: GPL-3.0-or-later (`LICENSE`); only crates with a GPL-compatible licence.

## 9. Build, test and acceptance

Code is written on Linux. GitHub Actions builds the Windows `.exe` (`cardcast-exe`) and the Linux tarball (`cardcast-linux-x86_64.tar.gz`) on every push and attaches both to the run. The project owner downloads them and tests them on the sender, in Windows and on CachyOS with KDE Plasma on Wayland. The Windows mirror and guards cannot be run on Linux.

CI must pass `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cargo build --release` on Windows and Linux. The Linux CI desktops come with the Linux parts (research section 9): Xvfb with openbox, headless sway with xdg-desktop-portal-wlr and PipeWire, and `kwin_wayland --virtual`. GNOME, NVIDIA, HDR colours and game load stay owner checks.

v1 contains every part below. Each part is handed over as a test build when it is ready, so problems are found one part at a time; v1 is done when all of them pass together.

The Linux column is the same check on a Linux sender, per desktop as in the research's support matrix; "helper" means a per-desktop helper cardcast ships. Where a desktop has no helper, the status line says so and that line is not owed.

| Part | Accepted when | Linux |
| --- | --- | --- |
| Guard | A window moved to the card display with Win+Shift+Arrow goes to the "send windows to" monitor at once, without taking focus. A window already there at launch does the same. The pointer cannot be parked on the card display. | Same, with the desktop's own move-to-monitor shortcut. Window guard: Plasma (helper: KWin script), GNOME (helper: Shell extension), sway and Hyprland (helper: IPC), X11 (EWMH); other Wayland compositors get the status-line notice. Pointer guard: X11 (XFixes barriers and warp), Plasma (helper: fake-input warp, may break with KWin updates), GNOME (helper: Shell extension), sway and Hyprland (helper: IPC). CI: Xvfb with openbox, sway, `kwin_wayland --virtual`; owner: Plasma on the sender. |
| Mirror | OBS on the receiver shows the source monitor through the capture card at 1080p60, pointer included. With HDR on, colours match the OBS fullscreen projector's. The source monitor still reports 500 Hz. Unplugging and re-plugging the card restores the mirror with no clicks. Restarting cardcast restores the mirror with no clicks. | Same on every desktop: portal capture on Wayland (a permission dialog the first time only; restarting cardcast restores it with no clicks), MIT-SHM on X11 (a CPU copy). Presented as a layer-shell overlay (Plasma, wlroots), an override-redirect window (X11), or on GNOME a fullscreen window that needs the Shell extension so it doesn't take focus. HDR colours: owner-checked on Plasma (the compositor converts); unverified on GNOME and wlroots, not researched on X11. CI: a frame through sway with xdg-desktop-portal-wlr, and Xvfb. |
| Audio | The receiver hears the chosen audio source through the capture card, and the sender still hears it normally, with the sound output on Automatic. With No audio the mirror runs without sound. Checked on the reference cards. | Same on every desktop where PipeWire runs: a PipeWire loopback, Automatic finds the card display's sink by name plus the matching graphics card. CI: a relay between two null sinks. |
| Input switch (section 11, after v1) | With the toggle off, nothing is sent over DDC/CI. With it on, the AW2523HF tile lists DisplayPort 1, HDMI 1 and HDMI 2 with the current one ticked, and choosing HDMI 1 switches it. A Stream Deck key running `cardcast.exe input 4GZY5P3 toggle dp hdmi1` through a shortcut switches it there and back while a game has focus, without taking focus; an input it doesn't list exits with 1. If Windows drops the monitor while it shows HDMI 1, the settings window shows the waiting state and the mirror resumes when the monitor is back. Owner-checked; CI runs the unit tests only. | Same with `cardcast input ...`, on every desktop: DDC/CI over i2c-dev without root, with the udev rule and `modules-load.d` file shipped. Owner-checked on the AW2523HF. |

**Capture check, before the mirror is written.** In OBS on the sender, switch Display Capture's method from Automatic to Windows Graphics Capture with Force SDR on, keep the fullscreen projector running, and play. If the game feels the same, the colours look right on the receiver and no yellow border shows, the mirror uses Windows Graphics Capture. Otherwise it uses Desktop Duplication.

**Sender load pass line.** Run the Forza Horizon 6 benchmark at normal settings and at the lowest settings, with cardcast stopped and with the mirror running, three runs each. The mean average fps with the mirror running must be within 2% of the mean without it, at both settings. This is checked when the mirror is delivered and again with everything running. On Linux the owner runs the same check on the sender when the Linux mirror is delivered.

**Input lag check.** Average fps does not show input lag. With PresentMon logging the game, compare its presentation mode in three states: nothing running, the OBS fullscreen projector running, and the cardcast mirror running. cardcast passes when its presentation mode is no worse than the OBS projector's, which is the setup already accepted on the reference sender. PresentMon is Windows-only and the research names no Linux counterpart, so this check is Windows-only for now.

## 10. Risks

| Risk | Effect | What to do |
| --- | --- | --- |
| The sender's graphics card runs out of outputs with three monitors plus the card display. Seen on the sender (issue #25). | The card display does not light up | Plug the card display into another graphics card (ADR 0003) |
| Capturing the source monitor changes how Windows presents the game | Added input lag | The OBS projector is accepted as feeling right on the reference sender but has not been measured; the input lag check in section 9 compares cardcast against it |
| HDR to SDR conversion looks different from OBS's | Washed-out or clipped colours on the receiver | Compare against the OBS projector; switch capture method or adjust the conversion |
| A game in exclusive fullscreen cannot be captured | Black mirror | Recommend borderless mode in the README |
| Programs running as administrator open on the card display | Window stays lost | Documented limit; revisit if it happens in practice |

## 11. Monitor input switching

Comes after v1 (issue #35). cardcast can switch a monitor's input over DDC/CI, as the monitor's own buttons do, from the live layout or from a command. Sourced facts: `docs/win32-ddc.md`.

- **Universal.** Any monitor that answers DDC/CI. No brand or model names in production code. Input values follow MCCS (VCP 0x60): `0x0F` DisplayPort 1, `0x10` DisplayPort 2, `0x11` HDMI 1, `0x12` HDMI 2. Any other value a monitor lists (for example a maker's own USB-C value) is shown as "Input 0x1B".
- **Input list.** A monitor's inputs are the values in `60(...)` of its DDC/CI capabilities string. A monitor whose string can't be read or lists fewer than two inputs has no input switch. DDC/CI can't tell whether an input has a live cable, so cardcast never claims it does.
- **Settings cog.** A small cog at the top right of the settings window opens a panel with one toggle, "Input switching", off by default and saved with the other settings. While it is off, the settings window sends nothing over DDC/CI.
- **Reading the monitors.** While the toggle is on, cardcast reads every monitor's input list when the toggle is switched on, at launch, and after each display change. All DDC/CI calls run on their own input thread, one monitor at a time, never on the settings window, guard or mirror thread.
- **Input switch button.** A monitor tile in the live layout whose monitor has an input list gets an input switch button. It opens a menu of that monitor's inputs with the current one ticked; the current input is read when the menu opens. Choosing an input switches the monitor. Each input has a "Copy command" button that copies the command switching this monitor to it, with the full path to `cardcast.exe` and the monitor's serial, or its name when no other monitor has that name but the serial is missing or shared (EDID placeholder serials are); when neither picks only this monitor, the menu says so.
- **When a monitor leaves.** If Windows drops a monitor after its input switched away, that is an unplug: the existing waiting states apply ("Waiting for source monitor", "Waiting for card display") and the mirror resumes when the monitor is back. A monitor that has left Windows can't be switched back by cardcast; the monitor's own buttons can.
- **Status texts.** In the status line: "Could not switch <monitor> to <input>: the monitor did not answer over DDC/CI." When the toggle is on and no monitor has an input list: "No monitor offers input switching over DDC/CI."

**Command mode.** Run with arguments, `cardcast.exe` does one input switch and exits. It opens no window, does not read or change the settings, and works whether or not the settings window is open or the toggle is on, because running the command is the user's choice. Run without arguments, it starts as before.

```
cardcast.exe input <monitor> <input>
cardcast.exe input <monitor> toggle <input> <input>
```

- `<monitor>` is the monitor's EDID serial (for example `4GZY5P3`) or its name as cardcast shows it (for example `"AW2523HF"`), both compared ignoring case. A name that fits more than one monitor is refused; use the serial. `\\.\DISPLAYn` names and list positions are not accepted, because they change.
- `<input>` is `dp`, `dp2`, `hdmi1`, `hdmi2`, or a value `0x01` to `0xFF` for any other input. It must be in the monitor's input list; anything else is refused.
- `toggle a b` switches to `b` when the monitor is on `a`, otherwise to `a`.
- A monitor already on the input is left alone and counts as success.
- One plain line goes to standard error when the caller captured it, for example "Switched AW2523HF to HDMI 1." or "AW2523HF does not list HDMI 2; it lists DisplayPort 1, HDMI 1." Nothing else is shown: no window and no message box, so the game keeps focus.

| Exit code | Meaning |
| --- | --- |
| 0 | The monitor took the switch, or was already on that input |
| 1 | The switch failed: no such monitor, a name that fits several, no input list, an input it doesn't list, or no answer over DDC/CI |
| 2 | The command was not understood |

For Stream Deck or a script: make a Windows shortcut whose target is the copied command and point Stream Deck's System → Open at it. Shortcut hotkeys are not recommended, because Windows ignores them while a fullscreen game runs; the README says so.

**Testing.** The CI virtual monitors have no DDC/CI. The capabilities parser, input names, EDID serial, monitor selection, the toggle decision, the command grammar and the exit codes are plain functions with Linux unit tests. The cog and the button are in the snapshots. Real switching is checked by the owner on the sender (section 9).
