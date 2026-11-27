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

1. Plug the capture card's HDMI input into the sender's graphics card, and its USB side into the receiver. Any capture card that shows up in Windows as a monitor works; tested with the Elgato HD60 X and the AVerMedia Live Gamer Mini.
2. Plug the card display into the same graphics card as the source monitor if it has a free output, or else into another one (a second graphics card or the processor's integrated graphics). cardcast then copies the picture across, which costs the source monitor's graphics card a little work (ADR 0003).
   - On an NVIDIA graphics card, a monitor using Display Stream Compression (DSC, common on high-refresh monitors) can take two of the card's four display heads, so with two such monitors the card display may get no signal. A lower refresh rate or resolution on one of them, or turning DSC off on it (some monitors do this when switched from DisplayPort 1.4 to 1.2), frees a head, but costs that monitor refresh rate. Plugging the card display into the other graphics card keeps it.
3. In Windows Display settings, set the card display to **Extend these displays**, never Duplicate.
4. Set the card display's resolution and refresh rate in Windows (for example 1920×1080 at 60 Hz). cardcast never changes display modes. For 120 or 240 Hz, see [Higher refresh rates](#higher-refresh-rates-120-or-240-hz).
5. The card display must not be the primary monitor.
6. The source monitor can run with Windows HDR on or off. The mirror is always SDR.

## Settings

The settings window has these controls. Each monitor dropdown shows the monitor's name, resolution and refresh rate.

- **Source monitor**: the monitor to mirror. Never the card display.
- **Card display**: the capture card's display. Never the source monitor, the "send windows to" monitor or the primary monitor.
- **Send windows to**: where the window guard returns windows that land on the card display. Defaults to the primary monitor. Never the card display.
- **Audio source**: the playback device whose sound goes to the capture card.
- **Sound output** (the line under Audio source, **Change** to pick): where that sound is played so the capture card carries it. **Automatic** (default) finds the card display's own sound output. Pick a playback device yourself if Automatic finds the wrong one or none, or **No audio** if your capture card has no sound; the mirror runs either way. When Automatic finds nothing, the status line says so and offers **Choose sound output**.
- **Start / Stop**: starts or stops the mirror. Audio starts and stops with it.
- **Guard**: turns the window guard and the pointer guard on and off together. On by default; it does nothing until the card display is chosen. The guard works whether or not the mirror is running.
- **Live layout**: your monitors as Windows arranges them, to scale, each marked with what it is chosen as. Click a monitor to make it the source monitor, the card display or the "send windows to" monitor. While the mirror runs, the header says LIVE with the frame rate the card display gets.

The dropdowns and the live layout offer a monitor only for the roles it can take, so a choice that can't work can't be made. To swap the source monitor and the card display, first make a third monitor the card display. If Windows makes the card display the primary monitor, the live layout says so in red and the mirror refuses to start until you choose another card display. Settings saved by an older cardcast with the source monitor or "send windows to" on the card display are fixed at launch: that choice is cleared ("send windows to" goes back to the primary monitor), and the status line says so.
- **Status line**: shows "Running", "Waiting for card display", "Waiting for source monitor", "Stopped", or the last error in plain words, followed by any audio or guard problem. If the mirror keeps up with fewer than 90% of the card display's refreshes for a few seconds, it says so (see [Higher refresh rates](#higher-refresh-rates-120-or-240-hz)). While the button shows Stop, the mirror runs or starts again by itself; a mirror stopped by an error stays stopped until you press Start.

Choices are saved when you change them. Changing the source monitor, card display or audio source while the mirror runs or waits restarts it on the new choice at once. A new sound output restarts only the audio. On launch the mirror starts by itself, and the window starts minimised if it does. Closing the window quits cardcast.

cardcast also remembers the connector the card display is plugged into, so it finds the card display again when the capture card changes its name (for example a different EDID mode). A different monitor plugged into that connector therefore becomes the card display, unless it is the source monitor, the "send windows to" monitor or the primary monitor; then cardcast keeps waiting for the card display.

If the capture card or the source monitor is unplugged, the mirror stops and the status line says which one it waits for. When it is plugged back in, the mirror and audio resume by themselves.

## Higher refresh rates (120 or 240 Hz)

60 Hz is the usual case. If your capture card can take 120 or 240 Hz, the mirror can run at that rate too. cardcast never changes display modes: you set the rate in Windows, and the mirror follows it, drawing once per refresh of the card display.

1. Check that your capture card supports the mode (resolution and refresh rate) on its HDMI input, and that the receiver captures it at that rate (in OBS, the capture card source's resolution and FPS). A mode the card doesn't support shows no picture or is refused by Windows.
2. In Windows, open **Settings > System > Display**, select the card display, and open **Advanced display**. Under **Choose a refresh rate**, pick 120 Hz or 240 Hz. Windows lists only the rates the capture card offers.
3. Start cardcast (or let it run on). The header shows the frame rate the card display gets, for example about 120 fps at 120 Hz.

A higher rate costs the sender more: the mirror draws more often, and when the card display is on another graphics card than the source monitor, the picture crossing between the cards grows with the rate (for 1080p, about 0.5 GB/s at 60 Hz and 1 GB/s at 120 Hz; ADR 0003). If the sender can't keep up, the status line says the mirror shows fewer frames than the card display's refresh rate; set a lower rate in step 2.

## Window guard and pointer guard

- Any window that lands on the card display is moved to the "send windows to" monitor, without taking focus from the game. A maximised window stays maximised there.
- If the same window keeps going back to the card display (more than 3 times within a minute), the guard stops moving it and names it in the status line, so it never fights a program. The notice stays until you press **Dismiss guard notice**, or until that window leaves the card display or closes; then the guard watches it again. Turning the guard off and on, or changing the card display or "send windows to", also resets this.
- If "send windows to" is the card display, or there is no monitor to send windows to, the guard does nothing and says so in the status line.
- The pointer is checked about 60 times a second. If it is on the card display, it is put back where it was last seen on another monitor.

**Admin-window limit.** cardcast does not run as administrator, so the window guard cannot move windows of programs that run as administrator. If one opens on the card display, move it yourself with Win+Shift+Arrow.

## Games: use borderless mode

Run games in borderless (windowed fullscreen) mode. A game in exclusive fullscreen may not be capturable and can show a black mirror. When a game enters or leaves exclusive fullscreen, or the resolution changes, cardcast re-creates the capture by itself.

## Sound

cardcast records what the audio source plays and plays it again on the sound output. You keep hearing the audio source normally on the sender. Audio problems never stop the mirror.

To choose which programs go to the capture card, use Windows' own per-app sound settings (Settings > System > Sound > Volume mixer): send those programs to the audio source. cardcast has no per-app settings.

## Known limits

- Changing the SDR content brightness slider in Windows while the mirror runs has no effect until the mirror is restarted.
- A rotated (portrait) source monitor shows un-rotated in the mirror.
- Pointer pixels that normally invert the screen are drawn black, so such a pointer can vanish on dark scenes.
- If the mirror can't start or is interrupted for a reason that can pass (a monitor or graphics card going away, a driver reset, a mode change, a permission prompt), it tries again every second and the status line says why. A failure that can't pass (Windows or the graphics driver too old, a fault in cardcast) stops it with the reason; press Start to try again.
- The primary monitor is taken to be the one at the top-left corner (0,0) of the Windows desktop. Not yet confirmed on the sender.
- Automatic finds the card display's sound output by the container ID Windows gives a monitor and its HDMI sound, else by name. Not yet confirmed on the sender; choose the sound output yourself if it picks wrong.

## Reporting a problem

Open an issue on GitHub at `ClearlyNotMich/cardcast`. Include:

- what you did and what happened instead,
- the text of the status line,
- the CI run or commit the `.exe` came from,
- your monitors and capture card, and whether HDR was on.
