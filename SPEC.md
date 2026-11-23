# cardcast — Spec

Nov 23, 2026 · @Mich

## 1. Summary

cardcast is a small Windows program for dual-PC streaming with a capture card. It runs on the sender and does three things: it shows a mirror of the source monitor on the card display, it keeps windows and the mouse pointer off the card display, and it plays the chosen audio source into the capture card.

The receiver needs no cardcast software. OBS on the receiver adds the capture card as a video source.

The reason it exists is sender load. Windows duplicate mode took away the source monitor's high refresh rate and added input lag. Extended mode works, but then windows open on a display nobody can see. An OBS fullscreen projector fixes the picture but not the lost windows, and needs OBS running on the sender.

Terms in bold in `GLOSSARY.md` are used with exactly that meaning here. `docs/adr/0001-capture-card-is-the-link.md` explains why the earlier network design was dropped.

## 2. Setup it assumes

- Two Windows PCs. The sender runs the game; the receiver runs OBS.
- A capture card connected to the receiver over USB, with its HDMI input plugged into the sender's graphics card. Tested cards: Elgato HD60 X and AVerMedia Live Gamer Mini.
- The card display is set to **Extend** in Windows, never Duplicate.
- The card display is plugged into the same graphics card as the source monitor, so the picture is never copied between graphics cards.
- The source monitor may run with Windows HDR on or off. The card display is always SDR.

## 3. Goals and non-goals

**Goals for v1.** All of these ship together as v1.

- Mirror one source monitor onto the card display at the card display's own resolution and refresh rate. The reference case is a 2560×1440 500 Hz source monitor shown at 1920×1080 60 Hz.
- The source monitor keeps its refresh rate and the game is not measurably slower. See the pass line in section 9.
- When the source monitor is in HDR, the mirror is converted to SDR and its colours look the same on the receiver as the OBS fullscreen projector's do today.
- No window ever stays on the card display, and the mouse pointer cannot stay there either.
- The chosen audio source is played to the card display's sound output, so the capture card carries sound.
- A settings window with: source monitor dropdown, card display dropdown, "send windows to" dropdown, audio source dropdown, Start/Stop button, guard checkbox.
- Choices are remembered. On launch the mirror starts by itself. If the capture card is unplugged and plugged back in, the mirror resumes by itself.

**Non-goals**

- Sending the picture over a network.
- Linux or macOS.
- Scenes, sources, overlays, encoding or recording. That is OBS's job on the receiver.
- Mirroring a single window or game instead of a whole monitor.
- Sending HDR to the receiver. The mirror is always SDR.
- A tray icon, an installer, or starting with Windows. These can come after v1.
- Running as administrator. Windows of programs running as administrator are therefore not moved by the window guard; the README says so.
- Hiding the card display from Windows completely. Windows only offers that on Enterprise and Pro for Workstations.

## 4. Mirror

The mirror follows the same three steps as OBS's fullscreen projector, and the picture stays on the graphics card throughout.

1. **Capture.** Get the source monitor's picture as a GPU texture. There are two ways, and the capture check in section 9 picks one before the mirror is written:
   - **Windows Graphics Capture, 8-bit output.** Preferred, because Windows then converts HDR to SDR and draws the mouse pointer into the picture itself. Turning off its yellow capture border needs Windows 11.
   - **DXGI Desktop Duplication.** What OBS's Display Capture uses by default on a desktop PC, so it is what the reference sender runs today. It needs more code: the mirror must convert HDR to SDR itself, using the monitor's SDR white level, and must draw the mouse pointer itself.
2. **Window.** Create a borderless, always-on-top window that exactly covers the card display. It must not take focus and must not appear in the taskbar or Alt+Tab.
3. **Draw.** Once per refresh of the card display, take the newest captured frame, draw it into the window's swap chain scaled to fit with the aspect ratio kept, and present it.

Behaviour:

- Pacing comes from the card display. A 500 Hz source monitor shown on a 60 Hz card display produces 60 draws per second, and older frames are skipped.
- cardcast never changes display modes. The user sets the card display's resolution and refresh rate in Windows.
- The mouse pointer is visible in the mirror whenever it is on the source monitor.
- When capture access is lost (resolution change, a game entering or leaving exclusive fullscreen, a permission prompt), capture is re-created without restarting the program.
- When the card display disappears, the mirror stops and waits. When it returns, the mirror starts again.
- Monitors are remembered by the display's device path from the Windows display configuration, not by `\\.\DISPLAYn` or list position. At run time that path is mapped to its DXGI output, and the Direct3D device is created on that output's graphics card, not the default one.
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
- Controls: source monitor dropdown, card display dropdown, "send windows to" dropdown, audio source dropdown, Start/Stop button, guard checkbox, and a one-line status (running, waiting for card display, or the last error in plain words).
- Dropdowns show each monitor's name, resolution and refresh rate.
- Settings are saved when changed and loaded at launch.
- The window repaints only on user input or when the mirror or window guard reports a status change.
- Closing the window quits cardcast. When launched with saved settings that start the mirror successfully, the window starts minimised.
- On Linux the window runs with the mirror and window guard replaced by stand-ins, so the layout can be checked during development.

## 7. Audio

- The audio source is one playback device on the sender, chosen in the dropdown. cardcast records what that device plays and plays it again on the card display's sound output.
- The sender keeps hearing the audio source normally.
- Which programs play into the audio source is decided in Windows' own per-app sound settings, not in cardcast.
- Audio starts and stops with the mirror, and resumes with it after the card is re-plugged.

## 8. Tech stack and layout

- Stable Rust, one crate named `cardcast`, one binary. Split into modules: `ui`, `mirror`, `guard` and `audio`.
- Windows APIs through the `windows` crate: Direct3D 11, DXGI, and Win32 windowing and events.
- The program declares per-monitor DPI awareness (version 2) in its manifest, so monitor rectangles and window sizes are real pixels on scaled displays.
- `unsafe` only in the `mirror`, `guard` and `audio` modules, each block with a `// SAFETY:` comment.
- Check each crate's current version and maintenance status before adding it.
- Licence: MIT OR Apache-2.0.

## 9. Build, test and acceptance

Code is written on Linux. GitHub Actions builds the Windows `.exe` on every push and attaches it to the run. The project owner downloads it and tests it on the sender in Windows. The mirror and window guard cannot be run on Linux.

CI must pass `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` on Windows and Linux.

v1 contains every part below. Each part is handed over as a test build when it is ready, so problems are found one part at a time; v1 is done when all of them pass together.

| Part | Accepted when |
| --- | --- |
| Guard | A window moved to the card display with Win+Shift+Arrow goes to the "send windows to" monitor at once, without taking focus. A window already there at launch does the same. The pointer cannot be parked on the card display. |
| Mirror | OBS on the receiver shows the source monitor through the capture card at 1080p60, pointer included. With HDR on, colours match the OBS fullscreen projector's. The source monitor still reports 500 Hz. Unplugging and re-plugging the card restores the mirror with no clicks. Restarting cardcast restores the mirror with no clicks. |
| Audio | The receiver hears the chosen audio source through the capture card, and the sender still hears it normally. |

**Capture check, before the mirror is written.** In OBS on the sender, switch Display Capture's method from Automatic to Windows Graphics Capture with Force SDR on, keep the fullscreen projector running, and play. If the game feels the same, the colours look right on the receiver and no yellow border shows, the mirror uses Windows Graphics Capture. Otherwise it uses Desktop Duplication.

**Sender load pass line.** Run the Forza Horizon 6 benchmark at normal settings and at the lowest settings, with cardcast stopped and with the mirror running, three runs each. The mean average fps with the mirror running must be within 2% of the mean without it, at both settings. This is checked when the mirror is delivered and again with everything running.

**Input lag check.** Average fps does not show input lag. With PresentMon logging the game, compare its presentation mode in three states: nothing running, the OBS fullscreen projector running, and the cardcast mirror running. cardcast passes when its presentation mode is no worse than the OBS projector's, which is the setup already accepted on the reference sender.

## 10. Risks

| Risk | Effect | What to do |
| --- | --- | --- |
| The sender's graphics card runs out of outputs with three monitors plus the card display. Not verified. | The card display does not light up | Test with the first mirror build; unplug one monitor to confirm |
| Capturing the source monitor changes how Windows presents the game | Added input lag | The OBS projector is accepted as feeling right on the reference sender but has not been measured; the input lag check in section 9 compares cardcast against it |
| HDR to SDR conversion looks different from OBS's | Washed-out or clipped colours on the receiver | Compare against the OBS projector; switch capture method or adjust the conversion |
| A game in exclusive fullscreen cannot be captured | Black mirror | Recommend borderless mode in the README |
| Programs running as administrator open on the card display | Window stays lost | Documented limit; revisit if it happens in practice |
