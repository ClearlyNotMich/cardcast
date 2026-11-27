//! The display watcher: a hidden top-level window on its own thread that hears every display and
//! device change. It is cardcast's one listener: the settings window follows it and passes each
//! change on to the guard.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    RegisterClassW, WINDOW_EX_STYLE, WM_DEVICECHANGE, WM_DISPLAYCHANGE, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

thread_local! {
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Calls its `wake` after every display change (a monitor plugged, unplugged or changing mode) and
/// every device change: a monitor's device can go away with no display change (docs/win32-mirror-audio.md).
pub struct DisplayWatcher {
    /// `None` once dropped.
    running: Option<JoinHandle<()>>,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the Windows tests"))]
    window: isize,
}

impl DisplayWatcher {
    /// Errors are plain words for the status line.
    pub fn new(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let (started_sender, started) = mpsc::channel();
        let thread = std::thread::spawn(move || run(Box::new(wake), &started_sender));
        match started.recv() {
            Ok(window) => Ok(Self {
                running: Some(thread),
                window,
            }),
            Err(_) => {
                let _ = thread.join();
                Err("cardcast can't watch for display changes, so it won't follow the card display being unplugged or plugged back in. Restart cardcast.".into())
            }
        }
    }
}

impl Drop for DisplayWatcher {
    fn drop(&mut self) {
        if let Some(thread) = self.running.take() {
            let _ = super::quit_and_join(thread);
        }
    }
}

fn run(wake: Box<dyn Fn()>, started: &mpsc::Sender<isize>) {
    WAKE.set(Some(wake));
    let Some(window) = create_window() else {
        return;
    };
    if started.send(window.0 as isize).is_ok() {
        let mut message = MSG::default();
        // SAFETY: message is a valid MSG; the loop ends on WM_QUIT (0) or error (-1).
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
            // SAFETY: message came from GetMessageW on this thread.
            unsafe { DispatchMessageW(&message) };
        }
    }
    // SAFETY: the window was created by this thread and is destroyed once.
    let _ = unsafe { DestroyWindow(window) };
}

/// Hidden, because only top-level windows hear about display and device changes.
fn create_window() -> Option<HWND> {
    // SAFETY: a null module name returns this executable's handle.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("cardcast displays"),
        ..Default::default()
    };
    // SAFETY: class points at static strings and a valid window procedure; registering twice only fails.
    unsafe { RegisterClassW(&class) };
    // SAFETY: the class was registered above; the window is never shown.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("cardcast displays"),
            w!(""),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .ok()
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DISPLAYCHANGE || message == WM_DEVICECHANGE {
        WAKE.with_borrow(|wake| {
            if let Some(wake) = wake {
                wake();
            }
        });
    }
    // SAFETY: forwards the message this procedure received unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// The container ID of the monitor with this display-configuration device path (a device
/// interface path): interface → device instance id → devnode → `DEVPKEY_Device_ContainerId`.
/// Empty if Windows cannot say.
pub(super) fn container_id(device_path: &str) -> String {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL,
        CM_Locate_DevNodeW, CR_SUCCESS, MAX_DEVICE_ID_LEN,
    };
    use windows::Win32::Devices::Properties::{
        DEVPKEY_Device_ContainerId, DEVPKEY_Device_InstanceId, DEVPROP_TYPE_GUID,
        DEVPROP_TYPE_STRING, DEVPROPTYPE,
    };
    use windows::core::{GUID, PCWSTR};

    let path: Vec<u16> = device_path.encode_utf16().chain([0]).collect();
    let mut instance = [0u16; MAX_DEVICE_ID_LEN as usize];
    let mut size = u32::try_from(size_of_val(&instance)).unwrap_or(0);
    let mut kind = DEVPROPTYPE::default();
    // SAFETY: path is null-terminated; the buffer holds `size` bytes and Windows writes no more.
    let found = unsafe {
        CM_Get_Device_Interface_PropertyW(
            PCWSTR(path.as_ptr()),
            &DEVPKEY_Device_InstanceId,
            &mut kind,
            Some(instance.as_mut_ptr().cast()),
            &mut size,
            0,
        )
    };
    if found != CR_SUCCESS || kind != DEVPROP_TYPE_STRING {
        return String::new();
    }
    let mut node = 0;
    // SAFETY: instance holds the null-terminated id just written (shorter than MAX_DEVICE_ID_LEN).
    let located = unsafe {
        CM_Locate_DevNodeW(
            &mut node,
            PCWSTR(instance.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        )
    };
    if located != CR_SUCCESS {
        return String::new();
    }
    let mut container = GUID::zeroed();
    let mut size = u32::try_from(size_of::<GUID>()).unwrap_or(0);
    // SAFETY: the buffer is one GUID, as `size` says; a bigger value fails instead of being written.
    let read = unsafe {
        CM_Get_DevNode_PropertyW(
            node,
            &DEVPKEY_Device_ContainerId,
            &mut kind,
            Some((&raw mut container).cast()),
            &mut size,
            0,
        )
    };
    if read != CR_SUCCESS || kind != DEVPROP_TYPE_GUID {
        return String::new();
    }
    format!("{container:?}")
}

/// The monitor's connector: its graphics card's device path, which stays across boots unlike the
/// card's LUID, and the target id of the video output. Empty if Windows cannot name the card.
pub(super) fn connector(adapter: windows::Win32::Foundation::LUID, target: u32) -> String {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
        DisplayConfigGetDeviceInfo,
    };
    let mut name = DISPLAYCONFIG_ADAPTER_NAME::default();
    name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME;
    name.header.size = u32::try_from(size_of::<DISPLAYCONFIG_ADAPTER_NAME>()).unwrap_or(0);
    name.header.adapterId = adapter;
    // SAFETY: header is the first field of a correctly sized DISPLAYCONFIG_ADAPTER_NAME, as size says.
    if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } != 0 {
        return String::new();
    }
    format!(
        "{} LUID {:?} target {target}",
        super::wide_to_string(&name.adapterDevicePath),
        (adapter.HighPart, adapter.LowPart)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};
    use windows::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW};

    #[test]
    fn display_and_device_changes_wake_the_settings_window() {
        let wakes = Arc::new(AtomicU32::new(0));
        let watcher = {
            let wakes = wakes.clone();
            DisplayWatcher::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap()
        };
        let window = HWND(watcher.window as *mut _);
        // SAFETY: the window is alive until the watcher drops; WM_DISPLAYCHANGE carries no pointers.
        unsafe { PostMessageW(Some(window), WM_DISPLAYCHANGE, WPARAM(32), LPARAM(0)) }.unwrap();
        // SAFETY: as above; DBT_DEVNODES_CHANGED (7) carries no pointer either.
        unsafe { PostMessageW(Some(window), WM_DEVICECHANGE, WPARAM(7), LPARAM(0)) }.unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while wakes.load(Ordering::Relaxed) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        drop(watcher);
        // SAFETY: IsWindow accepts any handle.
        assert!(!unsafe { IsWindow(Some(window)) }.as_bool());
    }
}
