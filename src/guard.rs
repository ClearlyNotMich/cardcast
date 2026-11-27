//! guard module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

#[cfg(windows)]
mod displays;
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

/// Linux has no display changes to hear; this stand-in never wakes.
#[cfg(not(windows))]
pub struct DisplayWatcher;

#[cfg(not(windows))]
impl DisplayWatcher {
    pub fn new(_wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        Ok(Self)
    }
}

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
    /// Name of the graphics card the monitor is plugged into, as DXGI lists it; empty if unknown.
    pub graphics_card: String,
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

    fn take_report(&mut self) -> Option<GuardReport> {
        None
    }

    fn follow_displays(&mut self) {}
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
        graphics_card: if device_path == "stand-in-card" {
            "Stand-in integrated graphics".into()
        } else {
            "Stand-in graphics card".into()
        },
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

/// Active monitors from the Windows display configuration, keyed by device path.
#[cfg(windows)]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
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
        let gdi_name = wide_to_string(&source.viewGdiDeviceName);
        let graphics_card = cards
            .iter()
            .find(|(output, _)| output.eq_ignore_ascii_case(&gdi_name))
            .map(|(_, card)| card.clone())
            .unwrap_or_default();
        monitors.push(Monitor {
            device_path,
            gdi_name,
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
            graphics_card,
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
    use std::process::Command;
    use std::time::{Duration, Instant};

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

    fn pnputil(action: &str, device: &str) -> bool {
        Command::new("pnputil")
            .args([action, device])
            .status()
            .is_ok_and(|status| status.success())
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
}
