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
    /// Errors are plain words for the status line.
    fn start(&mut self, source_monitor: &Monitor, card_display: &Monitor) -> Result<(), String>;
    fn stop(&mut self);
}

/// The mirror window on the card display.
#[cfg(windows)]
pub fn new_mirror() -> Box<dyn Mirror> {
    Box::new(window::MirrorWindow::default())
}

/// The mirror cannot run on Linux, so the settings window gets the stand-in.
#[cfg(not(windows))]
pub fn new_mirror() -> Box<dyn Mirror> {
    Box::new(StandInMirror::default())
}

/// Stand-in for Linux and tests; it only remembers whether it was started.
#[cfg(any(test, not(windows)))]
#[derive(Debug, Default)]
pub struct StandInMirror {
    pub running: bool,
}

#[cfg(any(test, not(windows)))]
impl Mirror for StandInMirror {
    fn start(&mut self, _source_monitor: &Monitor, _card_display: &Monitor) -> Result<(), String> {
        self.running = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.running = false;
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
) -> Result<&'a Output, &'static str> {
    let output = find_output(device_path, monitors, outputs)
        .ok_or("The source monitor is not connected to any graphics card.")?;
    if output.adapter == card_display.adapter {
        Ok(output)
    } else {
        Err(
            "The source monitor and the card display are on different graphics cards. Plug both into the same one.",
        )
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
        let refused = source_output("source", &outputs[1], &monitors, &outputs);
        assert!(refused.unwrap_err().contains("different graphics cards"));
        let unplugged = source_output("gone", &outputs[1], &monitors, &outputs);
        assert!(unplugged.unwrap_err().contains("not connected"));
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
