# cardcast

cardcast is a small Windows program for streaming with two PCs and a capture card. It runs on the sender, the PC the game runs on. It does three things:

- It shows a mirror of the source monitor on the card display, so the receiver can capture it.
- It keeps windows and the mouse pointer off the card display.
- It plays the chosen audio source into the capture card, so the capture card carries sound.

The receiver needs no cardcast software. OBS on the receiver adds the capture card as a video source.

Terms are defined in `GLOSSARY.md`. The full design is in `SPEC.md` and `docs/adr/`.

## Download

There is no installer and no release page yet. Each push builds `cardcast.exe` in GitHub Actions.

1. Open the repository's **Actions** tab and pick the latest green run of the **CI** workflow on the `v1` branch.
2. Under **Artifacts**, download `cardcast-exe`.
3. Unzip it and run `cardcast.exe`. Do not run it as administrator.

## Setup

1. Plug the capture card's HDMI input into the sender's graphics card, and its USB side into the receiver.
2. Plug the card display into the **same graphics card** as the source monitor. If they are on different graphics cards, Start fails with a message in the status line.
3. In Windows Display settings, set the card display to **Extend these displays**, never Duplicate.
4. Set the card display's resolution and refresh rate in Windows (for example 1920×1080 at 60 Hz). cardcast never changes display modes.
5. The card display must not be the primary monitor.
6. The source monitor can run with Windows HDR on or off. The mirror is always SDR.

## Settings

The settings window has these controls. Each monitor dropdown shows the monitor's name, resolution and refresh rate.

- **Source monitor**: the monitor to mirror.
- **Card display**: the capture card's display. It must differ from the source monitor and must not be the primary monitor, or the mirror refuses to start.
- **Send windows to**: where the window guard returns windows that land on the card display. Defaults to the primary monitor.
- **Audio source**: the playback device whose sound goes to the capture card.
- **Start / Stop**: starts or stops the mirror. Audio starts and stops with it.
- **Guard**: turns the window guard and the pointer guard on and off together. On by default; it does nothing until the card display is chosen. The guard works whether or not the mirror is running.
- **Live layout**: your monitors as Windows arranges them, to scale, each marked with what it is chosen as. Click a monitor to make it the source monitor, the card display or the "send windows to" monitor. While the mirror runs, the header says LIVE with the frame rate the card display gets.
- **Status line**: shows "Running", "Waiting for card display", "Waiting for source monitor", "Stopped", or the last error in plain words, followed by any audio or guard problem. While the button shows Stop, the mirror runs or starts again by itself; a mirror stopped by an error stays stopped until you press Start.

Choices are saved when you change them. Changing the source monitor, card display or audio source while the mirror runs or waits restarts it on the new choice at once. On launch the mirror starts by itself, and the window starts minimised if it does. Closing the window quits cardcast.

If the capture card or the source monitor is unplugged, the mirror stops and the status line says which one it waits for. When it is plugged back in, the mirror and audio resume by themselves.

## Window guard and pointer guard

- Any window that lands on the card display is moved to the "send windows to" monitor, without taking focus from the game. A maximised window stays maximised there.
- If the same window keeps going back to the card display (more than 3 times within a minute), the guard stops moving it and names it in the status line, so it never fights a program. The notice stays until you press **Dismiss guard notice**, or until that window leaves the card display or closes; then the guard watches it again. Turning the guard off and on, or changing the card display or "send windows to", also resets this.
- If "send windows to" is the card display, or there is no monitor to send windows to, the guard does nothing and says so in the status line.
- The pointer is checked about 60 times a second. If it is on the card display, it is put back where it was last seen on another monitor.

**Admin-window limit.** cardcast does not run as administrator, so the window guard cannot move windows of programs that run as administrator. If one opens on the card display, move it yourself with Win+Shift+Arrow.

## Games: use borderless mode

Run games in borderless (windowed fullscreen) mode. A game in exclusive fullscreen may not be capturable and can show a black mirror. When a game enters or leaves exclusive fullscreen, or the resolution changes, cardcast re-creates the capture by itself.

## Sound

cardcast records what the audio source plays and plays it again on the card display's sound output. You keep hearing the audio source normally on the sender.

To choose which programs go to the capture card, use Windows' own per-app sound settings (Settings > System > Sound > Volume mixer): send those programs to the audio source. cardcast has no per-app settings.

## Known limits

- Changing the SDR content brightness slider in Windows while the mirror runs has no effect until the mirror is restarted.
- A rotated (portrait) source monitor shows un-rotated in the mirror.
- Pointer pixels that normally invert the screen are drawn black, so such a pointer can vanish on dark scenes.
- If the mirror can't start or is interrupted for a reason that can pass (a monitor or graphics card going away, a driver reset, a mode change, a permission prompt), it tries again every second and the status line says why. A failure that can't pass (Windows or the graphics driver too old, the source monitor and card display on different graphics cards, a fault in cardcast) stops it with the reason; press Start to try again.
- The primary monitor is taken to be the one at the top-left corner (0,0) of the Windows desktop. Not yet confirmed on the sender.
- The card display's sound output is found by matching its name to the monitor's name. Not yet confirmed on the sender or on any capture card.

## Reporting a problem

Open an issue on GitHub at `ClearlyNotMich/cardcast`. Include:

- what you did and what happened instead,
- the text of the status line,
- the CI run or commit the `.exe` came from,
- your monitors and capture card, and whether HDR was on.
