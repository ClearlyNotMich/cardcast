//! guard module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

/// A monitor on the sender, as shown in the settings dropdowns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Display-configuration device path; the stable key saved in settings.
    pub device_path: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub primary: bool,
}

/// What the settings window needs from the window guard and pointer guard.
pub trait Guard {
    fn set_enabled(&mut self, enabled: bool);
}

/// Stand-in until the real guard lands; it only remembers the checkbox.
#[derive(Debug, Default)]
pub struct StandInGuard {
    pub enabled: bool,
}

impl Guard for StandInGuard {
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// Stand-in monitors so the settings window can be laid out on Linux.
#[cfg(not(windows))]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    let monitor = |device_path: &str, name: &str, width, height, refresh_hz, primary| Monitor {
        device_path: device_path.into(),
        name: name.into(),
        width,
        height,
        refresh_hz,
        primary,
    };
    Ok(vec![
        monitor(
            "stand-in-source",
            "Stand-in source monitor",
            2560,
            1440,
            500,
            false,
        ),
        monitor(
            "stand-in-primary",
            "Stand-in primary monitor",
            1920,
            1080,
            144,
            true,
        ),
        monitor(
            "stand-in-card",
            "Stand-in card display",
            1920,
            1080,
            60,
            false,
        ),
    ])
}

/// Active monitors from the Windows display configuration, keyed by device path.
#[cfg(windows)]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
        GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };

    let failed = |what: &str| format!("Could not list monitors ({what}).");
    let mut path_count = 0u32;
    let mut mode_count = 0u32;
    // SAFETY: both pointers are valid u32 locals for the duration of the call.
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
        .ok()
        .map_err(|_| failed("buffer sizes"))?;
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    // SAFETY: the arrays hold exactly the counts passed alongside them; Windows writes back the used counts.
    unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    }
    .ok()
    .map_err(|_| failed("display configuration"))?;
    paths.truncate(path_count as usize);

    let header_size = u32::try_from(size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>())
        .map_err(|_| failed("header size"))?;
    let mut monitors = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target.header.size = header_size;
        target.header.adapterId = path.targetInfo.adapterId;
        target.header.id = path.targetInfo.id;
        // SAFETY: header is the first field of a correctly sized DISPLAYCONFIG_TARGET_DEVICE_NAME, as size says.
        if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
            continue;
        }
        // SAFETY: without QDC_VIRTUAL_MODE_AWARE the union holds modeInfoIdx.
        let mode_index = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let Some(mode) = modes.get(mode_index) else {
            continue;
        };
        // SAFETY: the mode a path's source index points to is a source mode.
        let source_mode = unsafe { mode.Anonymous.sourceMode };
        let rate = path.targetInfo.refreshRate;
        let name = wide_to_string(&target.monitorFriendlyDeviceName);
        monitors.push(Monitor {
            device_path: wide_to_string(&target.monitorDevicePath),
            name: if name.is_empty() {
                format!("Monitor {}", index + 1)
            } else {
                name
            },
            width: source_mode.width,
            height: source_mode.height,
            refresh_hz: (rate.Numerator + rate.Denominator / 2)
                .checked_div(rate.Denominator)
                .unwrap_or(0),
            // Windows always places the primary monitor at the desktop origin.
            primary: source_mode.position.x == 0 && source_mode.position.y == 0,
        });
    }
    Ok(monitors)
}

#[cfg(windows)]
fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}
