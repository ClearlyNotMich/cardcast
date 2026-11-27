//! guard module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Linux guard and mirror helpers (#36) pick with it; its tests run everywhere"
    )
)]
pub mod desktop;
#[cfg(windows)]
mod displays;
#[cfg(target_os = "linux")]
mod linux;
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

#[cfg(windows)]
pub use displays::DisplayWatcher;
#[cfg(windows)]
pub(crate) use displays::device_node;

#[cfg(target_os = "linux")]
pub use linux::{DisplayWatcher, monitors};

/// A monitor on the sender, as shown in the settings dropdowns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// The stable key saved in settings: the display-configuration device path on Windows; on Linux
    /// the EDID's product and serial (`DEL42B9 4GZY5P3`), else the connector.
    pub device_path: String,
    /// GDI name of the source the monitor shows (`\\.\DISPLAYn`); on Linux the desktop's output
    /// name (`HDMI-A-2`). Changes between boots, so never saved.
    pub gdi_name: String,
    pub name: String,
    /// The resolution in real pixels, after the monitor's rotation; the dropdowns show it and the mirror uses it.
    pub width: u32,
    pub height: u32,
    /// The size in the desktop's coordinates, which `left` and `top` are in; the live layout draws
    /// it. On Wayland these are logical pixels, so a scaled monitor's is smaller than its resolution;
    /// on Windows it is the resolution, and on X11 too unless `xrandr --scale` stretches it.
    pub layout_size: (u32, u32),
    pub refresh_hz: u32,
    pub primary: bool,
    /// Top-left corner on the desktop.
    pub left: i32,
    pub top: i32,
    /// Name of the graphics card the monitor is plugged into, as DXGI (on Linux pci.ids) lists it;
    /// empty if unknown.
    pub graphics_card: String,
    /// The physical device the monitor is part of (`DEVPKEY_Device_ContainerId`), shared with its
    /// HDMI or DisplayPort sound output; on Linux `audio::container_id`. Empty if unknown.
    pub container_id: String,
    /// The graphics card's device path and the target id of the video output the monitor is on (on
    /// Linux its PCI address and DRM connector, `0000:0c:00.0 HDMI-A-2`); unlike `device_path` it
    /// stays when the monitor's EDID (its name) changes. Empty if unknown.
    pub connector: String,
}

/// What the settings window needs from the window guard and pointer guard.
pub trait Guard {
    /// Applies the guard checkbox and the chosen card display and "send windows to" monitor.
    fn update(&mut self, enabled: bool, card_display: Option<&str>, send_windows_to: Option<&str>);
    /// The guard's latest notice for the status line, if it changed.
    fn take_report(&mut self) -> Option<GuardReport>;
    /// After a display or device change: the guard finds its monitors again and checks every window.
    fn follow_displays(&mut self);
}

/// What the guard tells the status line; each report replaces the one before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardReport {
    /// A problem or a given-up window, in plain words; it stays until the next report or dismissed.
    Notice(String),
    /// Nothing to report any more.
    Clear,
}

/// The window guard; `wake` asks the settings window to repaint when a report is waiting.
#[cfg(windows)]
pub fn new_guard(wake: impl Fn() + Send + 'static) -> Box<dyn Guard> {
    Box::new(window_guard::WindowGuard::new(wake))
}

/// The Linux guards are not built yet (#36), so while the guard checkbox is on it says so.
#[cfg(not(windows))]
pub fn new_guard(_wake: impl Fn() + Send + 'static) -> Box<dyn Guard> {
    Box::new(NotOnLinuxYet::default())
}

#[cfg(not(windows))]
#[derive(Default)]
struct NotOnLinuxYet {
    enabled: bool,
    report: Option<GuardReport>,
}

#[cfg(not(windows))]
impl Guard for NotOnLinuxYet {
    fn update(&mut self, enabled: bool, _card: Option<&str>, _send_windows_to: Option<&str>) {
        self.enabled = enabled;
        self.report = Some(if enabled {
            GuardReport::Notice(
                "The window guard and pointer guard do not work on Linux yet.".into(),
            )
        } else {
            GuardReport::Clear
        });
    }

    fn take_report(&mut self) -> Option<GuardReport> {
        self.report.take()
    }

    fn follow_displays(&mut self) {
        if self.enabled {
            self.update(true, None, None);
        }
    }
}

/// Active display paths and modes from `QueryDisplayConfig`.
#[cfg(windows)]
pub(crate) fn display_config() -> Result<
    (
        Vec<windows::Win32::Devices::Display::DISPLAYCONFIG_PATH_INFO>,
        Vec<windows::Win32::Devices::Display::DISPLAYCONFIG_MODE_INFO>,
    ),
    String,
> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };

    let failed = |what: &str| format!("Could not list monitors. ({what})");
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
    modes.truncate(mode_count as usize);
    Ok((paths, modes))
}

/// The path's monitor device path and friendly name, or `None` if Windows cannot name its target.
#[cfg(windows)]
pub(crate) fn device_path(
    path: &windows::Win32::Devices::Display::DISPLAYCONFIG_PATH_INFO,
) -> Option<(String, String)> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
        DisplayConfigGetDeviceInfo,
    };
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
    target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
    target.header.size = u32::try_from(size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>()).ok()?;
    target.header.adapterId = path.targetInfo.adapterId;
    target.header.id = path.targetInfo.id;
    // SAFETY: header is the first field of a correctly sized DISPLAYCONFIG_TARGET_DEVICE_NAME, as size says.
    if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
        return None;
    }
    Some((
        wide_to_string(&target.monitorDevicePath),
        wide_to_string(&target.monitorFriendlyDeviceName),
    ))
}

/// A monitor's size on the desktop. The display configuration gives the source mode before the
/// monitor's rotation, so a monitor turned to portrait has its width and height swapped.
fn desktop_size(width: u32, height: u32, sideways: bool) -> (u32, u32) {
    if sideways {
        (height, width)
    } else {
        (width, height)
    }
}

#[cfg(test)]
mod desktop_size_tests {
    use super::desktop_size;

    #[test]
    fn a_portrait_monitor_is_taller_than_wide() {
        assert_eq!(desktop_size(2560, 1440, true), (1440, 2560));
    }

    #[test]
    fn a_landscape_or_upside_down_monitor_keeps_its_size() {
        assert_eq!(desktop_size(2560, 1440, false), (2560, 1440));
    }
}

/// Active monitors from the Windows display configuration, keyed by device path.
#[cfg(windows)]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_ROTATION_ROTATE90,
        DISPLAYCONFIG_ROTATION_ROTATE270, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DisplayConfigGetDeviceInfo,
    };

    let failed = |what: &str| format!("Could not list monitors. ({what})");
    let (paths, modes) = display_config()?;
    let cards = graphics_cards();
    let mut monitors = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let Some((device_path, name)) = device_path(path) else {
            continue;
        };
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
        let sideways = matches!(
            path.targetInfo.rotation,
            DISPLAYCONFIG_ROTATION_ROTATE90 | DISPLAYCONFIG_ROTATION_ROTATE270
        );
        let (width, height) = desktop_size(source_mode.width, source_mode.height, sideways);
        let gdi_name = wide_to_string(&source.viewGdiDeviceName);
        let graphics_card = cards
            .iter()
            .find(|(output, _)| output.eq_ignore_ascii_case(&gdi_name))
            .map(|(_, card)| card.clone())
            .unwrap_or_default();
        let container_id = displays::container_id(&device_path);
        monitors.push(Monitor {
            device_path,
            gdi_name,
            name: if name.is_empty() {
                format!("Monitor {}", index + 1)
            } else {
                name
            },
            width,
            height,
            layout_size: (width, height),
            refresh_hz: (rate.Numerator + rate.Denominator / 2)
                .checked_div(rate.Denominator)
                .unwrap_or(0),
            // Windows always places the primary monitor at the desktop origin.
            primary: source_mode.position.x == 0 && source_mode.position.y == 0,
            left: source_mode.position.x,
            top: source_mode.position.y,
            graphics_card,
            container_id,
            connector: displays::connector(path.targetInfo.adapterId, path.targetInfo.id),
        });
    }
    Ok(monitors)
}

/// Each DXGI output's GDI name with the name of the graphics card DXGI lists it under, the card
/// the mirror's device goes on (`mirror::find_output` matches the same way).
#[cfg(windows)]
fn graphics_cards() -> Vec<(String, String)> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let mut cards = Vec::new();
    // SAFETY: plain factory creation.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return cards;
    };
    // SAFETY: enumeration ends at the first index that fails (DXGI_ERROR_NOT_FOUND).
    for adapter in (0..).map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok()) {
        // SAFETY: plain description read.
        let Ok(description) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let card = wide_to_string(&description.Description);
        // SAFETY: as above, per output.
        for output in (0..).map_while(|index| unsafe { adapter.EnumOutputs(index) }.ok()) {
            // SAFETY: plain description read.
            if let Ok(output) = unsafe { output.GetDesc() } {
                cards.push((wide_to_string(&output.DeviceName), card.clone()));
            }
        }
    }
    cards
}

/// Helpers for the tests the CI `second-monitor` job runs: it adds two virtual monitors on one
/// virtual graphics card and sets `CARDCAST_SECOND_MONITOR` to that card (a PnP instance id).
#[cfg(all(test, windows))]
pub mod second_monitor {
    use super::Monitor;
    use std::io::Write as _;
    use std::process::Command;
    use std::time::{Duration, Instant};
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_ROTATION_ROTATE90,
        SDC_APPLY, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SetDisplayConfig,
    };

    const ENV: &str = "CARDCAST_SECOND_MONITOR";

    /// The primary monitor then the others, or `None` (the test skips) outside that job.
    /// Panics in that job with fewer than `count`, so a missing monitor fails the test instead of skipping it.
    pub fn monitors(count: usize) -> Option<Vec<Monitor>> {
        std::env::var_os(ENV)?;
        let mut monitors = super::monitors().unwrap_or_else(|error| panic!("{error}"));
        monitors.sort_by_key(|monitor| !monitor.primary);
        assert!(
            monitors.len() >= count && monitors[0].primary,
            "{ENV} is set but needs {count} monitors, one of them primary; found {}",
            monitors.len()
        );
        Some(monitors)
    }

    /// A device disabled with `pnputil`, as if unplugged; it is plugged back in on drop.
    pub struct Unplugged(String);

    impl Unplugged {
        /// Unplugs both virtual monitors by disabling their graphics card.
        pub fn new() -> Self {
            Self::device(std::env::var(ENV).unwrap_or_default())
        }

        /// Unplugs one monitor by disabling its PnP device, found from its device path
        /// (`\\?\DISPLAY#MTT1337#5&1&UID256#{guid}` is `DISPLAY\MTT1337\5&1&UID256`), since its
        /// `\\.\DISPLAYn` name changes on replug.
        pub fn monitor(device_path: &str) -> Self {
            let path = device_path.trim_start_matches(r"\\?\");
            let instance = path
                .rsplit_once('#')
                .map_or(path, |(instance, _guid)| instance);
            Self::device(instance.replace('#', r"\"))
        }

        fn device(id: String) -> Self {
            assert!(
                pnputil("/disable-device", &id),
                "pnputil /disable-device {id} failed"
            );
            Self(id)
        }
    }

    impl Drop for Unplugged {
        /// Never panics: it also runs while a failed test unwinds, and leaves the monitor connected for the next test.
        fn drop(&mut self) {
            if !pnputil("/enable-device", &self.0) {
                return;
            }
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline
                && !super::monitors().is_ok_and(|monitors| monitors.len() > 2)
            {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// A monitor set to another refresh rate at its resolution, as a user does in Windows' advanced
    /// display settings; its saved mode is put back on drop.
    pub struct RefreshRate(Vec<u16>);

    impl RefreshRate {
        pub fn set(monitor: &Monitor, hz: u32) -> Self {
            use windows::Win32::Graphics::Gdi::{
                CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
                DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
            };
            let name: Vec<u16> = monitor.gdi_name.encode_utf16().chain([0]).collect();
            let mode = DEVMODEW {
                dmSize: u16::try_from(size_of::<DEVMODEW>()).unwrap_or(0),
                dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
                dmPelsWidth: monitor.width,
                dmPelsHeight: monitor.height,
                dmDisplayFrequency: hz,
                ..DEVMODEW::default()
            };
            // SAFETY: name is NUL-terminated and mode a DEVMODEW with dmSize set; both outlive the call.
            let result = unsafe {
                ChangeDisplaySettingsExW(
                    windows::core::PCWSTR(name.as_ptr()),
                    Some(&raw const mode),
                    None,
                    CDS_TYPE(0),
                    None,
                )
            };
            assert_eq!(
                result, DISP_CHANGE_SUCCESSFUL,
                "{} could not be set to {hz} Hz",
                monitor.gdi_name
            );
            Self(name)
        }
    }

    impl Drop for RefreshRate {
        /// Never panics: it also runs while a failed test unwinds.
        fn drop(&mut self) {
            use windows::Win32::Graphics::Gdi::{CDS_TYPE, ChangeDisplaySettingsExW};
            // SAFETY: the name is NUL-terminated; no mode with no flags puts back the saved one.
            let _ = unsafe {
                ChangeDisplaySettingsExW(
                    windows::core::PCWSTR(self.0.as_ptr()),
                    None,
                    None,
                    CDS_TYPE(0),
                    None,
                )
            };
        }
    }

    fn pnputil(action: &str, device: &str) -> bool {
        Command::new("pnputil")
            .args([action, device])
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A monitor turned to portrait; it is turned back on drop.
    pub struct Rotated {
        device_path: String,
        paths: Vec<DISPLAYCONFIG_PATH_INFO>,
        modes: Vec<DISPLAYCONFIG_MODE_INFO>,
    }

    impl Rotated {
        /// Turns the monitor with `device_path` 90°, keeping its source mode: `SetDisplayConfig`
        /// takes it unswapped (docs/win32-pitfalls.md).
        pub fn portrait(device_path: &str) -> Self {
            let (paths, modes) = super::display_config().unwrap_or_else(|error| panic!("{error}"));
            let original = Self {
                device_path: device_path.to_owned(),
                paths: paths.clone(),
                modes: modes.clone(),
            };
            let mut paths = paths;
            let path = paths
                .iter_mut()
                .find(|path| super::device_path(path).is_some_and(|(p, _)| p == device_path))
                .unwrap_or_else(|| panic!("no display path for {device_path}"));
            path.targetInfo.rotation = DISPLAYCONFIG_ROTATION_ROTATE90;
            let result = apply(&paths, &modes);
            log(&format!("rotate 90: {result}"));
            assert_eq!(result, 0, "SetDisplayConfig could not rotate the monitor");
            original
        }

        /// Waits until the monitor's desktop is landscape; false on timeout.
        fn wait_for_landscape(&self) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let landscape = super::monitors().is_ok_and(|monitors| {
                    monitors.iter().any(|monitor| {
                        monitor.device_path == self.device_path && monitor.width > monitor.height
                    })
                });
                if landscape {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        }
    }

    impl Drop for Rotated {
        /// Waits until the monitor is landscape again so later tests don't run on a rotated
        /// monitor, and panics if it isn't; while a failed test unwinds it only logs, since a
        /// second panic would abort the test run.
        fn drop(&mut self) {
            let result = apply(&self.paths, &self.modes);
            let back = result == 0 && self.wait_for_landscape();
            log(&format!("rotation put back: {result}, landscape: {back}"));
            if !back && !std::thread::panicking() {
                panic!("could not turn {} back to landscape", self.device_path);
            }
        }
    }

    fn apply(paths: &[DISPLAYCONFIG_PATH_INFO], modes: &[DISPLAYCONFIG_MODE_INFO]) -> i32 {
        // SAFETY: both slices come from QueryDisplayConfig and stay alive for the call.
        unsafe {
            SetDisplayConfig(
                Some(paths),
                Some(modes),
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG,
            )
        }
    }

    /// Written to stderr directly: libtest captures only the print macros, and CI runs without --nocapture.
    pub fn log(line: &str) {
        let _ = writeln!(std::io::stderr(), "{line}");
    }

    /// Polls until `done` holds; panics after `limit`.
    pub fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < limit, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(windows)]
pub(crate) fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

/// Stops a thread that runs a message loop: posts WM_QUIT, then waits for it. The guard, mirror
/// and display watcher threads all stop this way. A post fails until the thread has a message
/// queue (it may still be starting) or while its queue is full, so it is retried until it lands
/// or the thread ended; no stop leaves a thread running.
#[cfg(windows)]
pub(crate) fn quit_and_join<T>(thread: std::thread::JoinHandle<T>) -> std::thread::Result<T> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
    // SAFETY: the handle is the thread's own, open until `thread` is joined below.
    let thread_id = unsafe { GetThreadId(HANDLE(thread.as_raw_handle())) };
    while !thread.is_finished() {
        // SAFETY: posting to a thread id has no memory effects; a thread without a queue just fails the post.
        if unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    thread.join()
}

#[cfg(all(test, windows))]
mod tests {
    use super::quit_and_join;
    use std::time::{Duration, Instant};
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG};

    #[test]
    fn quit_and_join_stops_a_thread_that_has_no_message_queue_yet() {
        // Like a mirror thread still opening the graphics card when Stop is pressed.
        let thread = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            let mut message = MSG::default();
            // SAFETY: message is a valid MSG; the first call creates the queue, the loop ends on WM_QUIT (0) or error (-1).
            while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {}
            "quit"
        });
        let (done, joined) = std::sync::mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = done.send(quit_and_join(thread).ok());
        });
        let joined = joined.recv_timeout(Duration::from_secs(5));
        println!("quit_and_join returned after {:?}", started.elapsed());
        assert_eq!(
            joined,
            Ok(Some("quit")),
            "the thread was left running or never asked to quit"
        );
    }

    use super::second_monitor::{Rotated, log, wait_for};
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows::core::BOOL;

    /// Each monitor's GDI name and `rcMonitor`, the desktop rectangle Windows reports.
    fn desktop_rects() -> Vec<(String, RECT)> {
        extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, rects: LPARAM) -> BOOL {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = u32::try_from(size_of::<MONITORINFOEXW>()).unwrap_or(0);
            // SAFETY: info is a MONITORINFOEXW with cbSize set, which GetMonitorInfoW accepts as a MONITORINFO.
            if unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) }.as_bool()
            {
                // SAFETY: rects is the Vec passed to EnumDisplayMonitors below, alive for its call.
                let rects = unsafe { &mut *(rects.0 as *mut Vec<(String, RECT)>) };
                rects.push((
                    super::wide_to_string(&info.szDevice),
                    info.monitorInfo.rcMonitor,
                ));
            }
            true.into()
        }
        let mut rects = Vec::new();
        // SAFETY: the callback only runs during this call, while rects is alive.
        let _ =
            unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&raw mut rects as isize)) };
        rects
    }

    #[test]
    fn second_monitor_rotated_monitor_has_the_size_and_position_the_desktop_gives_it() {
        let Some(monitors) = super::second_monitor::monitors(2) else {
            return;
        };
        // The rightmost, so turning it leaves no gap between monitors.
        let turned = monitors.iter().max_by_key(|monitor| monitor.left).unwrap();
        let _rotated = Rotated::portrait(&turned.device_path);
        let portrait = |rects: &[(String, RECT)]| {
            rects.iter().any(|(name, rect)| {
                *name == turned.gdi_name && rect.bottom - rect.top > rect.right - rect.left
            })
        };
        wait_for(
            "the desktop to show the monitor in portrait",
            Duration::from_secs(10),
            || portrait(&desktop_rects()),
        );
        let (paths, modes) = super::display_config().unwrap();
        for path in &paths {
            // SAFETY: as in `monitors`: the union holds modeInfoIdx, which points at a source mode.
            let source = unsafe {
                modes[path.sourceInfo.Anonymous.modeInfoIdx as usize]
                    .Anonymous
                    .sourceMode
            };
            log(&format!(
                "rotation {:?}: source mode {}x{}",
                path.targetInfo.rotation, source.width, source.height
            ));
        }
        let rects = desktop_rects();
        let monitors = super::monitors().unwrap();
        assert_eq!(monitors.len(), rects.len());
        for monitor in &monitors {
            let (_, rect) = rects
                .iter()
                .find(|(name, _)| *name == monitor.gdi_name)
                .unwrap();
            log(&format!(
                "{} {}: monitors() {}x{} at {},{}; rcMonitor {}x{} at {},{}",
                monitor.gdi_name,
                if monitor.gdi_name == turned.gdi_name {
                    "rotated 90"
                } else {
                    "unrotated"
                },
                monitor.width,
                monitor.height,
                monitor.left,
                monitor.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                rect.left,
                rect.top
            ));
            assert_eq!(
                (
                    monitor.left,
                    monitor.top,
                    monitor.width as i32,
                    monitor.height as i32
                ),
                (
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top
                ),
                "{}",
                monitor.gdi_name
            );
        }
    }
}

#[cfg(all(test, not(windows)))]
mod linux_tests {
    use super::*;

    #[test]
    fn on_linux_the_guard_says_it_does_not_run_yet_while_it_is_on() {
        let not_yet = GuardReport::Notice(
            "The window guard and pointer guard do not work on Linux yet.".into(),
        );
        let mut guard = new_guard(|| {});
        assert_eq!(guard.take_report(), None);
        guard.update(true, Some("card"), None);
        assert_eq!(guard.take_report(), Some(not_yet.clone()));
        assert_eq!(guard.take_report(), None);
        // A display change re-sends the notice while the guard is on, as the Windows guard does.
        guard.follow_displays();
        assert_eq!(guard.take_report(), Some(not_yet));
        guard.update(false, Some("card"), None);
        assert_eq!(guard.take_report(), Some(GuardReport::Clear));
        guard.follow_displays();
        assert_eq!(guard.take_report(), None);
    }
}
