# A premium look for the settings window

Question: what makes capture and GPU control apps look premium, and which egui techniques
reproduce that without adding sender load? Researched 2026-11-27 for the `ui-design` branch.

Short answer: one dark neutral scale, one brand accent plus status colours, 8–12 px rounding,
soft shadows only on floating layers, and a picture of the hardware drawn with the painter
instead of more widgets. All of it is plain `egui::Visuals` and `Painter` calls; no new crate,
no font file, no continuous repaint.

## 1. References

| Reference | What makes it look premium | Source |
| --- | --- | --- |
| Windows 11 Fluent design | Three rounding levels: 8 px for windows, flyouts and dialogs, 4 px for in-page controls such as buttons, 0 px where straight edges meet. Accent colour only for important elements. | [Geometry][geometry], [Color][color] |
| Windows 11 Mica | Layering: a base layer and raised content layers, with the material sampled once, "designed for app performance". | [Mica][mica] |
| Windows display settings | "Rearrange your displays": numbered monitor rectangles in their desktop arrangement, chosen by clicking. Users already know this map. | [Multiple monitors][monitors] |
| OBS Studio (Yami theme) | An eight-step grey scale from `#13141A` to `#5B6273`; the window is `grey7` (`#1D1F26`), panels `grey6`, the preview darkest (`grey8`); one blue primary. | [Yami.obt][yami] |
| NVIDIA App | A "unified GPU control center"; live numbers (FPS, GPU and CPU use, latency) are shown as real-time statistics in its overlay, not buried in settings pages. | [NVIDIA App][nvidia] |
| Rerun viewer (egui) | egui can look premium: a neutral grey token scale, Inter as the proportional font, corner-radius tokens (6 px buttons, 12 px cards), 0.2 s animations, popup shadows. | [dark_theme.ron][rerun-theme], [design_tokens.rs][rerun-tokens] |
| Elgato 4K Capture Utility | Could not check: its help pages did not load from here; only the [downloads page][elgato] lists it. Not used as evidence. | — |

## 2. What carries over to cardcast

- **Dark neutral base with one accent.** OBS and Rerun both build on one grey scale and one
  primary colour. cardcast uses a blue-black scale (`#0A0D14` base, `#111621` panels, `#19202F`
  raised) and violet `#8B5CFF` as its accent, which also marks the card display.
- **Status colours separate from the accent.** Green for live, amber for busy or waiting, rose for
  an error, grey for stopped (`look::Tone`). The NVIDIA overlay's lesson is that the live numbers
  are the interesting part, so the header shows LIVE and the frame rate in a status pill.
- **Rounding by level, as Fluent does.** 12 px cards, 10 px window and map, 6–8 px controls.
  `Visuals::widgets.*.corner_radius`, `Frame::corner_radius`, `window_corner_radius`.
- **Soft shadows only where something floats.** `Visuals::popup_shadow` and `window_shadow`
  (`Shadow { offset, blur, spread, color }`); glows under chosen monitors use
  `RectShape::with_blur_width`, which egui tessellates once per frame like any rectangle.
- **The hardware as a picture.** Windows' "rearrange your displays" map is the model for the live
  layout: monitors to scale in their desktop arrangement, clicked to choose a role. It is drawn
  with `Painter` (`rect_filled`, `rect_stroke`, `text`, `Shape::dashed_line`) and made clickable
  with `Ui::interact` and `Popup::menu`. Each tile names itself to screen readers via
  `Response::widget_info`.
- **Painter-drawn widgets** where egui's stock look is plain: the guard switch (a checkbox for
  screen readers), the pulsing status dot, the arc the picture travels along, a gradient wash in
  the header (`Mesh` with per-vertex colours).

## 3. What was left out, and why

- **A custom font.** Rerun ships Inter; egui's default fonts are Ubuntu-Light plus icons, and have
  no `●`, `▶` or `■` (the snapshot tests caught this; those glyphs are drawn with the painter).
  Embedding Inter (SIL OFL) adds a font file to the `.exe`; worth it if the owner wants it.
- **Mica or acrylic backdrops.** They need Windows calls in the UI, which `CODING_STANDARDS.md`
  rule 2 forbids, and eframe does not offer them.
- **egui's `Spinner`.** It calls `request_repaint()` on every frame it is shown
  (`egui/src/widgets/spinner.rs`), so it repaints as fast as the display allows. cardcast draws
  its own arc and caps the rate itself.

## 4. Repaint and sender load

eframe runs only `App::logic` while the window is minimised or hidden, never `App::ui`
(`eframe/src/native/epi_integration.rs`, `update_logic_only`). `Context::request_repaint_after`
schedules one more pass after an idle wait. cardcast's rule (`look::next_repaint`, unit-tested):

| Window | Mirror | Repaints by itself |
| --- | --- | --- |
| Minimised or occluded | any | never |
| Shown with focus | starting, running, restarting | every 33 ms (about 30 Hz), to animate |
| Shown, the game has focus | running | once a second, for the frame rate; the animation holds still |
| Shown | stopped, waiting, error | never; input and reports wake it, as before |

The frame rate comes from a counter the mirror thread already kept (`presents`, one relaxed atomic
add per present); the settings window reads it, so measuring costs the mirror nothing.

## 5. Live preview thumbnail: not done

A thumbnail of the mirror would need either a copy of each shown frame from the graphics card to
memory (a staging texture and `Map`, which waits for the GPU) or a texture shared between the
mirror's Direct3D 11 device and egui's renderer, plus a texture upload on every repaint. Both add
GPU and CPU work on the sender while it plays, which is the one thing cardcast exists to avoid
(`GLOSSARY.md`, sender load), and the receiver already shows the picture in OBS. The live layout
shows that the picture flows instead: the card display takes the source monitor's tint and dots
travel along the arc between them.

## 6. Audio level: not done

A level meter would need a peak scan of every captured audio packet in the relay thread and a
30 Hz repaint to show it. The scan is cheap, but it would run in untested Windows code for a
decoration; the status line already reports audio problems.

[geometry]: https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/geometry
[color]: https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/color
[mica]: https://learn.microsoft.com/en-us/windows/apps/design/style/mica
[monitors]: https://support.microsoft.com/en-us/windows/how-to-use-multiple-monitors-in-windows-329c6962-5a4d-b481-7baa-bec9671f728a
[yami]: https://github.com/obsproject/obs-studio/blob/master/frontend/data/themes/Yami.obt
[nvidia]: https://www.nvidia.com/en-us/software/nvidia-app/
[rerun-theme]: https://github.com/rerun-io/rerun/blob/main/crates/ui_tools/re_ui/data/dark_theme.ron
[rerun-tokens]: https://github.com/rerun-io/rerun/blob/main/crates/ui_tools/re_ui/src/design_tokens.rs
[elgato]: https://www.elgato.com/us/en/s/downloads
