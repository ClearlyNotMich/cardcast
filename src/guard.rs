//! guard module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows window guard calls it; its tests run everywhere"
    )
)]
mod placement;
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows pointer guard calls it; its tests run everywhere"
    )
)]
mod pointer;
#[cfg(windows)]
mod window_guard;

/// A monitor on the sender, as shown in the settings dropdowns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Display-configuration device path; the stable key saved in settings.
    pub device_path: String,
    /// GDI name of the source the monitor shows (`\\.\DISPLAYn`); changes between boots, so never saved.
    pub gdi_name: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub primary: bool,
    /// Top-left corner on the desktop.
    pub left: i32,
    pub top: i32,
}

/// What the settings window needs from the window guard and pointer guard.
pub trait Guard {
    /// Applies the guard checkbox and the chosen card display and "send windows to" monitor.
    fn update(&mut self, enabled: bool, card_display: Option<&str>, send_windows_to: Option<&str>);
    /// The guard's latest message for the status line, in plain words.
    fn take_report(&mut self) -> Option<String>;
}

/// The window guard; `wake` asks the settings window to repaint when a report is waiting.
#[cfg(windows)]
pub fn new_guard(wake: impl Fn() + Send + 'static) -> Box<dyn Guard> {
    Box::new(window_guard::WindowGuard::new(wake))
}

/// The guard cannot run on Linux, so the settings window gets the stand-in.
#[cfg(not(windows))]
pub fn new_guard(_wake: impl Fn() + Send + 'static) -> Box<dyn Guard> {
    Box::new(StandInGuard::default())
}

/// Stand-in for Linux; it only remembers the checkbox.
#[cfg(not(windows))]
#[derive(Debug, Default)]
pub struct StandInGuard {
    pub enabled: bool,
}

#[cfg(not(windows))]
impl Guard for StandInGuard {
    fn update(&mut self, enabled: bool, _card: Option<&str>, _send_windows_to: Option<&str>) {
        self.enabled = enabled;
    }

    fn take_report(&mut self) -> Option<String> {
        None
    }
}

/// Stand-in monitors so the settings window can be laid out on Linux.
#[cfg(not(windows))]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    let monitor = |device_path: &str, name: &str, width, height, refresh_hz, primary| Monitor {
        device_path: device_path.into(),
        gdi_name: String::new(),
        name: name.into(),
        width,
        height,
        refresh_hz,
        primary,
        left: 0,
        top: 0,
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
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
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
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = u32::try_from(size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>())
            .map_err(|_| failed("header size"))?;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        // SAFETY: header is the first field of a correctly sized DISPLAYCONFIG_SOURCE_DEVICE_NAME, as size says.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
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
            gdi_name: wide_to_string(&source.viewGdiDeviceName),
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
            left: source_mode.position.x,
            top: source_mode.position.y,
        });
    }
    Ok(monitors)
}

#[cfg(windows)]
pub(crate) fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}
