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
    /// A failure says, in plain words for the status line, whether starting again can fix it.
    fn start(&mut self, source_monitor: &Monitor, card_display: &Monitor) -> Result<(), Ended>;
    fn stop(&mut self);
    /// False once stopped, or if the mirror ended on its own (for example a failed present).
    fn is_running(&self) -> bool;
    /// What to do about the mirror ending on its own; given once, after it ended.
    fn take_end(&mut self) -> Option<Ended>;
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

/// Decides from the HRESULT that ended the mirror thread whether a fresh start can fix it.
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

/// The mirror cannot run on Linux, so the settings window gets the stand-in.
#[cfg(not(windows))]
pub fn new_mirror(_wake: impl Fn() + Send + Sync + 'static) -> Box<dyn Mirror> {
    Box::new(StandInMirror::default())
}

/// Stand-in for Linux and tests; it remembers whether it was started, `end` ends it on its own,
/// and `fail_start` fails the next start.
#[cfg(any(test, not(windows)))]
#[derive(Debug, Default)]
pub struct StandInMirror {
    pub running: bool,
    pub end: Option<Ended>,
    pub fail_start: Option<Ended>,
}

#[cfg(any(test, not(windows)))]
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

    fn take_end(&mut self) -> Option<Ended> {
        let end = self.end.take()?;
        self.running = false;
        Some(end)
    }
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
/// source's graphics card, and SPEC section 1 puts the card display on that same card, so the
/// card display's device is reused and the picture is never copied between graphics cards.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows mirror window calls it")
)]
pub fn source_output<'a>(
    device_path: &str,
    card_display: &Output,
    monitors: &[Monitor],
    outputs: &'a [Output],
) -> Result<&'a Output, Ended> {
    let output = find_output(device_path, monitors, outputs).ok_or_else(|| {
        Ended::Restart("The source monitor is not connected to any graphics card.".into())
    })?;
    if output.adapter == card_display.adapter {
        Ok(output)
    } else {
        Err(Ended::Stop(
            "The source monitor and the card display are on different graphics cards. Plug both into the same one.".into(),
        ))
    }
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
    fn source_monitor_on_the_card_displays_graphics_card_is_duplicated() {
        let monitors = [
            monitor("source", r"\\.\DISPLAY1"),
            monitor("card", r"\\.\DISPLAY2"),
        ];
        let outputs = [output(0, 0, r"\\.\DISPLAY1"), output(0, 1, r"\\.\DISPLAY2")];
        assert_eq!(
            source_output("source", &outputs[1], &monitors, &outputs),
            Ok(&outputs[0])
        );
    }

    #[test]
    fn source_monitor_on_another_graphics_card_is_refused() {
        let monitors = [
            monitor("source", r"\\.\DISPLAY1"),
            monitor("card", r"\\.\DISPLAY2"),
        ];
        let outputs = [output(0, 0, r"\\.\DISPLAY1"), output(1, 0, r"\\.\DISPLAY2")];
        assert_eq!(
            source_output("source", &outputs[1], &monitors, &outputs),
            Err(Ended::Stop(
                "The source monitor and the card display are on different graphics cards. Plug both into the same one.".into()
            ))
        );
        // Gone during a replug: it may come back, so the start is retried.
        assert_eq!(
            source_output("gone", &outputs[1], &monitors, &outputs),
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
            refresh_hz: 60,
            primary: false,
            left: 0,
            top: 0,
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
