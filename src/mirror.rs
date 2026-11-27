//! mirror module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

#[cfg(windows)]
mod capture;
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows capture calls it; its tests run everywhere"
    )
)]
mod pointer;
#[cfg(windows)]
mod window;

use crate::guard::Monitor;

/// What the settings window needs from the mirror.
pub trait Mirror {
    /// Returns at once: the mirror starts in the background, and a failure to start comes from
    /// `take_end` as if it ended. A failure says, in plain words for the status line, whether
    /// starting again can fix it.
    fn start(&mut self, source_monitor: &Monitor, card_display: &Monitor) -> Result<(), Ended>;
    /// Returns once the mirror thread is gone.
    fn stop(&mut self);
    /// True while it starts or runs; false once stopped, or if it ended on its own (for example a failed present).
    fn is_running(&self) -> bool;
    /// True once it started and shows the picture, until it is stopped or ends.
    fn shows_picture(&self) -> bool;
    /// What to do about the mirror ending on its own or failing to start; given once, after it ended.
    fn take_end(&mut self) -> Option<Ended>;
    /// Frames presented on the card display since this start, for the frame rate readout; `None`
    /// where nothing is counted (the stand-ins). Reading it costs the mirror nothing.
    fn presents(&self) -> Option<u64> {
        None
    }
}

/// What the settings window does after the mirror failed to start or ended on its own, with the status text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Start it again on a timer, like capture re-creates the duplication after access loss.
    Restart(String),
    /// Leave it stopped; starting again would fail the same way.
    Stop(String),
}

/// Whether a fresh start can fix a failure with this HRESULT. Only a bug or a missing capability
/// is permanent: a monitor or graphics card going away fails in ways no document lists in full,
/// and stopping on one would leave the mirror off after a replug (SPEC section 9).
fn transient(code: i32) -> bool {
    // E_OUTOFMEMORY, E_NOINTERFACE, E_NOTIMPL, E_POINTER and E_UNEXPECTED (a panicked mirror thread).
    const PERMANENT: [u32; 5] = [
        0x8007_000E,
        0x8000_4002,
        0x8000_4001,
        0x8000_4003,
        0x8000_FFFF,
    ];
    !PERMANENT.contains(&code.cast_unsigned())
}

/// Decides from the HRESULT that ended the mirror thread whether a fresh start can fix it. A lost
/// graphics card, the source monitor's or the card display's, restarts it on a fresh device.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror window calls it")
)]
pub fn ended(code: i32) -> Ended {
    let shown = code.cast_unsigned();
    if transient(code) {
        Ended::Restart(format!(
            "The mirror was interrupted; restarting it. (error 0x{shown:08X})"
        ))
    } else {
        Ended::Stop(format!(
            "The mirror stopped unexpectedly. Press Start to try again. (error 0x{shown:08X})"
        ))
    }
}

/// A start that failed at `what` with this HRESULT: `message` with both, retried unless permanent.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror window calls it")
)]
pub fn start_failed(message: &str, what: &str, code: i32) -> Ended {
    let message = format!("{message} ({what}, error 0x{:08X})", code.cast_unsigned());
    if transient(code) {
        Ended::Restart(message)
    } else {
        Ended::Stop(message)
    }
}

/// Whether capture re-creates its duplication on a timer instead of ending the mirror: after a
/// desktop switch or mode change (`DXGI_ERROR_ACCESS_LOST`, then `E_ACCESSDENIED` on the secure
/// desktop or `DXGI_ERROR_UNSUPPORTED` in a mode it can't duplicate). Anything else ends the
/// mirror so `ended` decides, and the status line stops saying "Running" over a frozen frame.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows capture calls it")
)]
pub fn recreate_capture_in_place(code: i32) -> bool {
    [0x887A_0026_u32, 0x8007_0005, 0x887A_0004].contains(&code.cast_unsigned())
}

/// The mirror window on the card display; `wake` asks the settings window to repaint when it ends on its own.
#[cfg(windows)]
pub fn new_mirror(wake: impl Fn() + Send + Sync + 'static) -> Box<dyn Mirror> {
    Box::new(window::MirrorWindow::new(wake))
}

/// Top-left corner of the mirror window, if one is open; for the settings window's Windows tests.
#[cfg(all(test, windows))]
pub fn shown_at() -> Option<(i32, i32)> {
    window::shown_at()
}

/// What the status line keeps saying while the mirror is not built for this platform (#36).
#[cfg(windows)]
pub const NOT_BUILT: Option<&str> = None;
#[cfg(not(windows))]
pub const NOT_BUILT: Option<&str> = Some("The mirror does not work on Linux yet.");

/// The Linux mirror is not built yet (#36), so every start fails and says so.
#[cfg(not(windows))]
pub fn new_mirror(_wake: impl Fn() + Send + Sync + 'static) -> Box<dyn Mirror> {
    Box::new(NotOnLinuxYet)
}

#[cfg(not(windows))]
struct NotOnLinuxYet;

#[cfg(not(windows))]
impl Mirror for NotOnLinuxYet {
    fn start(&mut self, _source_monitor: &Monitor, _card_display: &Monitor) -> Result<(), Ended> {
        Err(Ended::Stop(NOT_BUILT.unwrap_or_default().into()))
    }
    fn stop(&mut self) {}
    fn is_running(&self) -> bool {
        false
    }
    fn shows_picture(&self) -> bool {
        false
    }
    fn take_end(&mut self) -> Option<Ended> {
        None
    }
}

/// Stand-in for the tests and snapshots; it remembers whether it was started, `end` ends it on its
/// own, and `fail_start` fails the next start.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct StandInMirror {
    pub running: bool,
    pub end: Option<Ended>,
    pub fail_start: Option<Ended>,
}

#[cfg(test)]
impl Mirror for StandInMirror {
    fn start(&mut self, _source_monitor: &Monitor, _card_display: &Monitor) -> Result<(), Ended> {
        if let Some(failure) = self.fail_start.take() {
            return Err(failure);
        }
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.running = false;
    }

    fn is_running(&self) -> bool {
        self.running && self.end.is_none()
    }

    fn shows_picture(&self) -> bool {
        self.is_running()
    }

    fn take_end(&mut self) -> Option<Ended> {
        let end = self.end.take()?;
        self.running = false;
        Some(end)
    }
}

/// The value `cache` keeps, made by `make` the first time it succeeds. A failure is not kept, so
/// a passing one (for example out of memory) does not fail every later start. Callers that may
/// race serialise `make` themselves.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows capture calls it")
)]
pub fn kept_once_made<T, E>(
    cache: &std::sync::OnceLock<T>,
    make: impl FnOnce() -> Result<T, E>,
) -> Result<&T, E> {
    if let Some(kept) = cache.get() {
        return Ok(kept);
    }
    let made = make()?;
    Ok(cache.get_or_init(|| made))
}

/// Where a picture lands inside the card display, in pixels of the card display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// Scales `picture` to the largest size that fits in `screen` with its aspect ratio kept, centred.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror window calls it")
)]
pub fn fit(picture: (u32, u32), screen: (u32, u32)) -> Fit {
    let (picture_width, picture_height) = (u64::from(picture.0), u64::from(picture.1));
    let (screen_width, screen_height) = (u64::from(screen.0), u64::from(screen.1));
    // Rounded to the nearest pixel; a zero-sized picture fills nothing.
    let scaled = |size: u64, by: u64, of: u64| {
        u32::try_from((size * by + of / 2).checked_div(of).unwrap_or(0)).unwrap_or(0)
    };
    let (width, height) = if picture_width * screen_height > screen_width * picture_height {
        (
            screen.0,
            scaled(picture_height, screen_width, picture_width),
        )
    } else {
        (
            scaled(picture_width, screen_height, picture_height),
            screen.1,
        )
    };
    let (width, height) = if picture_width == 0 || picture_height == 0 {
        (0, 0)
    } else {
        (width, height)
    };
    Fit {
        left: (screen.0 - width) / 2,
        top: (screen.1 - height) / 2,
        width,
        height,
    }
}

/// The size of the desktop a duplicated surface of `surface` size shows. The surface stays
/// unrotated with the desktop turned inside it, so 90° and 270° swap width and height.
/// `rotation` is `DXGI_OUTDUPL_DESC.Rotation`: 2 is 90°, 3 is 180°, 4 is 270°, anything else none.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows capture calls it")
)]
pub fn upright_size(surface: (u32, u32), rotation: i32) -> (u32, u32) {
    if matches!(rotation, 2 | 4) {
        (surface.1, surface.0)
    } else {
        surface
    }
}

/// The texture coordinates in a duplicated surface turned by `rotation` (as in `upright_size`)
/// of the upright desktop's top-left, top-right, bottom-left and bottom-right corners. Microsoft's
/// Desktop Duplication sample turns it the same way (docs/win32-mirror-audio.md).
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows capture calls it")
)]
pub fn corner_uvs(rotation: i32) -> [[f32; 2]; 4] {
    match rotation {
        2 => [[0.0, 1.0], [0.0, 0.0], [1.0, 1.0], [1.0, 0.0]],
        3 => [[1.0, 1.0], [0.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        4 => [[1.0, 0.0], [1.0, 1.0], [0.0, 0.0], [0.0, 1.0]],
        _ => [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
    }
}

/// A DXGI output: the graphics card (adapter) that owns it and its GDI name (`\\.\DISPLAYn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub adapter: u32,
    pub output: u32,
    pub gdi_name: String,
}

/// The DXGI output showing the monitor with `device_path`, matched through the monitor's GDI name.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror window calls it")
)]
pub fn find_output<'a>(
    device_path: &str,
    monitors: &[Monitor],
    outputs: &'a [Output],
) -> Option<&'a Output> {
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.device_path == device_path)?;
    outputs
        .iter()
        .find(|output| output.gdi_name.eq_ignore_ascii_case(&monitor.gdi_name))
}

/// The DXGI output to duplicate for the source monitor. Desktop Duplication needs a device on the
/// source monitor's graphics card, so the mirror's device goes there; if the card display is on
/// another card, DXGI copies each present across (ADR 0003).
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror window calls it")
)]
pub fn source_output<'a>(
    device_path: &str,
    monitors: &[Monitor],
    outputs: &'a [Output],
) -> Result<&'a Output, Ended> {
    find_output(device_path, monitors, outputs).ok_or_else(|| {
        Ended::Restart("The source monitor is not connected to any graphics card.".into())
    })
}

/// What the HDR shader multiplies linear scRGB by so the monitor's SDR white becomes 1.0.
/// `sdr_white_level` is `DISPLAYCONFIG_SDR_WHITE_LEVEL.SDRWhiteLevel`: 1000 means 80 nits, scRGB 1.0.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror calls it")
)]
pub fn white_scale(sdr_white_level: u32) -> f32 {
    if sdr_white_level == 0 {
        1.0
    } else {
        1000.0 / sdr_white_level as f32
    }
}

/// The HDR shader's maths for one channel: linear scRGB to an sRGB-encoded SDR value in 0..=1.
/// The shader in `mirror::capture` must match it; a Windows test checks that it does.
#[cfg(test)]
pub fn sdr_from_hdr(linear: f32, white_scale: f32) -> f32 {
    let value = (linear * white_scale).clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn on_linux_the_mirror_says_it_does_not_run_yet() {
        let monitor = Monitor {
            device_path: "card".into(),
            gdi_name: String::new(),
            name: "card".into(),
            width: 1920,
            height: 1080,
            layout_size: (1920, 1080),
            refresh_hz: 60,
            primary: false,
            left: 0,
            top: 0,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        };
        assert_eq!(NOT_BUILT, Some("The mirror does not work on Linux yet."));
        let mut mirror = new_mirror(|| {});
        assert_eq!(
            mirror.start(&monitor, &monitor),
            Err(Ended::Stop("The mirror does not work on Linux yet.".into()))
        );
        assert!(!mirror.is_running());
    }

    #[test]
    fn a_lost_monitor_graphics_card_or_desktop_restarts_the_mirror() {
        // DXGI_ERROR_DEVICE_REMOVED, _DEVICE_RESET, _ACCESS_LOST, E_ACCESSDENIED (secure desktop),
        // DXGI_ERROR_UNSUPPORTED (mode), _NOT_CURRENTLY_AVAILABLE, _SESSION_DISCONNECTED, and
        // E_INVALIDARG (an output that moved to another graphics card during a replug).
        for code in [
            0x887A_0005_u32,
            0x887A_0007,
            0x887A_0026,
            0x8007_0005,
            0x887A_0004,
            0x887A_0022,
            0x887A_0028,
            0x8007_0057,
        ] {
            assert_eq!(
                ended(code.cast_signed()),
                Ended::Restart(format!(
                    "The mirror was interrupted; restarting it. (error 0x{code:08X})"
                ))
            );
        }
    }

    #[test]
    fn a_lost_graphics_card_on_either_side_restarts_the_mirror() {
        // What GetDeviceRemovedReason reports, whichever card was lost: the device's own (source
        // monitor's) card, or the card display's card that each present is copied to (ADR 0003).
        // DXGI_ERROR_DEVICE_HUNG, _DEVICE_REMOVED, _DEVICE_RESET, _DRIVER_INTERNAL_ERROR, _INVALID_CALL.
        for code in [
            0x887A_0006_u32,
            0x887A_0005,
            0x887A_0007,
            0x887A_0020,
            0x887A_0001,
        ] {
            assert!(
                matches!(ended(code.cast_signed()), Ended::Restart(_)),
                "0x{code:08X} stops the mirror"
            );
        }
    }

    #[test]
    fn a_bug_or_missing_capability_stops_the_mirror_with_its_code() {
        // E_OUTOFMEMORY, E_NOINTERFACE, E_NOTIMPL, E_POINTER and E_UNEXPECTED (a panic).
        for code in [
            0x8007_000E_u32,
            0x8000_4002,
            0x8000_4001,
            0x8000_4003,
            0x8000_FFFF,
        ] {
            assert_eq!(
                ended(code.cast_signed()),
                Ended::Stop(format!(
                    "The mirror stopped unexpectedly. Press Start to try again. (error 0x{code:08X})"
                ))
            );
        }
    }

    #[test]
    fn a_failed_start_keeps_its_words_and_is_retried_unless_permanent() {
        // DXGI_ERROR_DEVICE_REMOVED, then E_NOINTERFACE.
        assert_eq!(
            start_failed(
                "Could not start the mirror.",
                "swap chain",
                0x887A_0005_u32.cast_signed()
            ),
            Ended::Restart("Could not start the mirror. (swap chain, error 0x887A0005)".into())
        );
        assert_eq!(
            start_failed(
                "Could not start the mirror.",
                "swap chain",
                0x8000_4002_u32.cast_signed()
            ),
            Ended::Stop("Could not start the mirror. (swap chain, error 0x80004002)".into())
        );
    }

    #[test]
    fn capture_is_recreated_in_place_only_after_a_desktop_or_mode_change() {
        // DXGI_ERROR_ACCESS_LOST, E_ACCESSDENIED and DXGI_ERROR_UNSUPPORTED retry in place.
        for code in [0x887A_0026_u32, 0x8007_0005, 0x887A_0004] {
            assert!(recreate_capture_in_place(code.cast_signed()));
        }
        // DXGI_ERROR_DEVICE_REMOVED needs a fresh device, so the mirror ends and restarts.
        assert!(!recreate_capture_in_place(0x887A_0005_u32.cast_signed()));
    }

    #[test]
    fn a_failure_is_not_kept_so_the_next_start_tries_again() {
        let cache = std::sync::OnceLock::new();
        let made = std::cell::Cell::new(0);
        let make = |result: Result<u32, i32>| {
            made.set(made.get() + 1);
            result
        };
        // E_OUTOFMEMORY can pass.
        assert_eq!(
            kept_once_made(&cache, || make(Err(0x8007_000E_u32.cast_signed()))),
            Err(0x8007_000E_u32.cast_signed())
        );
        assert_eq!(kept_once_made(&cache, || make(Ok(7))), Ok(&7));
        assert_eq!(kept_once_made(&cache, || make(Ok(8))), Ok(&7));
        assert_eq!(made.get(), 2, "made again after it was kept");
    }

    #[test]
    fn sdr_white_becomes_full_white() {
        // 200 nits SDR white is scRGB 2.5 and SDRWhiteLevel 2500.
        assert!((sdr_from_hdr(2.5, white_scale(2500)) - 1.0).abs() < 1e-6);
        assert!((sdr_from_hdr(1.0, white_scale(1000)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn half_sdr_white_is_srgb_encoded() {
        // Linear 0.5 encodes to sRGB 0.7354.
        assert!((sdr_from_hdr(1.25, white_scale(2500)) - 0.7354).abs() < 1e-3);
    }

    #[test]
    fn hdr_highlights_and_out_of_gamut_values_clip() {
        assert!((sdr_from_hdr(12.5, white_scale(2500)) - 1.0).abs() < 1e-6);
        assert_eq!(sdr_from_hdr(-0.2, white_scale(2500)), 0.0);
    }

    #[test]
    fn missing_white_level_falls_back_to_80_nits() {
        assert_eq!(white_scale(0), 1.0);
    }

    #[test]
    fn source_monitor_is_duplicated_on_its_own_graphics_card() {
        let monitors = [
            monitor("source", r"\\.\DISPLAY1"),
            monitor("card", r"\\.\DISPLAY2"),
        ];
        // Same graphics card, then the card display on another one (ADR 0003).
        for outputs in [
            [output(0, 0, r"\\.\DISPLAY1"), output(0, 1, r"\\.\DISPLAY2")],
            [output(0, 0, r"\\.\DISPLAY1"), output(1, 0, r"\\.\DISPLAY2")],
        ] {
            assert_eq!(
                source_output("source", &monitors, &outputs),
                Ok(&outputs[0])
            );
        }
        // Gone during a replug: it may come back, so the start is retried.
        assert_eq!(
            source_output("gone", &monitors, &[output(0, 0, r"\\.\DISPLAY1")]),
            Err(Ended::Restart(
                "The source monitor is not connected to any graphics card.".into()
            ))
        );
    }

    fn at(left: u32, top: u32, width: u32, height: u32) -> Fit {
        Fit {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn same_aspect_fills_the_card_display() {
        assert_eq!(fit((2560, 1440), (1920, 1080)), at(0, 0, 1920, 1080));
    }

    #[test]
    fn wider_picture_is_letterboxed() {
        assert_eq!(fit((2560, 1080), (1920, 1080)), at(0, 135, 1920, 810));
    }

    #[test]
    fn taller_picture_is_pillarboxed() {
        assert_eq!(fit((1600, 1200), (1920, 1080)), at(240, 0, 1440, 1080));
    }

    #[test]
    fn odd_ratio_rounds_to_the_nearest_pixel() {
        // 1366×768 is a bit wider than 16:9: 1920 × 768 / 1366 = 1079.47 → 1079.
        assert_eq!(fit((1366, 768), (1920, 1080)), at(0, 0, 1920, 1079));
        // 1080 × 1000 / 1001 = 1078.92 → 1079, centred with the odd pixel on the right.
        assert_eq!(fit((1000, 1001), (1920, 1080)), at(420, 0, 1079, 1080));
    }

    /// Where surface pixel (x, y) lands on the upright desktop (`width` x `height`), the way
    /// Microsoft's Desktop Duplication sample places dirty rectangles (`SetDirtyVert`).
    fn on_desktop(rotation: i32, (x, y): (f32, f32), (width, height): (f32, f32)) -> (f32, f32) {
        match rotation {
            2 => (width - y, x),
            3 => (width - x, height - y),
            4 => (y, height - x),
            _ => (x, y),
        }
    }

    #[test]
    fn each_rotation_samples_the_surface_pixel_that_shows_there() {
        let surface = (800.0, 600.0);
        let point = (100.0, 50.0);
        // DXGI_MODE_ROTATION: identity, 90°, 180°, 270°.
        for rotation in [1, 2, 3, 4] {
            let (width, height) = upright_size((800, 600), rotation);
            let desktop = (width as f32, height as f32);
            let (x, y) = on_desktop(rotation, point, desktop);
            let (u, v) = (x / desktop.0, y / desktop.1);
            // The quad interpolates its corners' texture coordinates.
            let [top_left, top_right, bottom_left, bottom_right] = corner_uvs(rotation);
            let sampled: Vec<f32> = (0..2)
                .map(|axis| {
                    let top = top_left[axis] + (top_right[axis] - top_left[axis]) * u;
                    let bottom = bottom_left[axis] + (bottom_right[axis] - bottom_left[axis]) * u;
                    top + (bottom - top) * v
                })
                .collect();
            let expected = [point.0 / surface.0, point.1 / surface.1];
            assert!(
                (sampled[0] - expected[0]).abs() < 1e-6 && (sampled[1] - expected[1]).abs() < 1e-6,
                "rotation {rotation}: sampled {sampled:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn unrotated_or_unspecified_picture_is_sampled_as_it_is() {
        assert_eq!(corner_uvs(0), corner_uvs(1));
        assert_eq!(
            corner_uvs(1),
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
        );
    }

    #[test]
    fn a_portrait_source_monitor_is_fitted_by_its_upright_size() {
        // A 1920x1080 surface turned 90° or 270° is a 1080x1920 desktop: pillarboxed on 1920x1080.
        for rotation in [2, 4] {
            assert_eq!(upright_size((1920, 1080), rotation), (1080, 1920));
            assert_eq!(
                fit(upright_size((1920, 1080), rotation), (1920, 1080)),
                at(656, 0, 608, 1080)
            );
        }
        for rotation in [0, 1, 3] {
            assert_eq!(upright_size((1920, 1080), rotation), (1920, 1080));
        }
    }

    #[test]
    fn empty_picture_fills_nothing() {
        assert_eq!(fit((0, 0), (1920, 1080)), at(960, 540, 0, 0));
    }

    fn monitor(device_path: &str, gdi_name: &str) -> Monitor {
        Monitor {
            device_path: device_path.into(),
            gdi_name: gdi_name.into(),
            name: device_path.into(),
            width: 1920,
            height: 1080,
            layout_size: (1920, 1080),
            refresh_hz: 60,
            primary: false,
            left: 0,
            top: 0,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        }
    }

    fn output(adapter: u32, output: u32, gdi_name: &str) -> Output {
        Output {
            adapter,
            output,
            gdi_name: gdi_name.into(),
        }
    }

    #[test]
    fn card_display_is_found_on_the_graphics_card_that_owns_it() {
        let monitors = [
            monitor(r"\\?\DISPLAY#GSM1#source", r"\\.\DISPLAY1"),
            monitor(r"\\?\DISPLAY#ELG1#card", r"\\.\DISPLAY3"),
        ];
        let outputs = [
            output(0, 0, r"\\.\DISPLAY1"),
            output(1, 0, r"\\.\DISPLAY2"),
            output(1, 1, r"\\.\DISPLAY3"),
        ];
        assert_eq!(
            find_output(r"\\?\DISPLAY#ELG1#card", &monitors, &outputs),
            Some(&outputs[2])
        );
    }

    #[test]
    fn unplugged_card_display_has_no_output() {
        let monitors = [monitor("card", r"\\.\DISPLAY3")];
        assert_eq!(
            find_output("gone", &monitors, &[output(0, 0, r"\\.\DISPLAY3")]),
            None
        );
        assert_eq!(
            find_output("card", &monitors, &[output(0, 0, r"\\.\DISPLAY1")]),
            None
        );
    }
}
