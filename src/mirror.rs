//! mirror module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

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

#[cfg(test)]
mod tests {
    use super::*;

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
