//! Command mode's Windows calls: the monitors with their EDID serials, and DDC/CI through dxva2 (docs/win32-ddc.md).

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe in input's ddc module, for dxva2 and registry calls"
)]

use super::{Ddc, MonitorId, edid_serial};
use crate::guard;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Open_DevNode_Key, CM_REGISTRY_HARDWARE, CR_SUCCESS, RegDisposition_OpenExisting,
};
use windows::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DestroyPhysicalMonitor, GetCapabilitiesStringLength,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR, SetVCPFeature,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFOEXW, MonitorFromPoint,
};
use windows::Win32::System::Registry::{
    HKEY, KEY_READ, RRF_RT_REG_BINARY, RegCloseKey, RegGetValueW,
};
use windows::core::{PCWSTR, w};

/// VCP code Input Select (MCCS).
const INPUT_SELECT: u8 = 0x60;

/// Where a monitor is on the desktop, to reach it over DDC/CI. Looked up again for every command, since `\\.\DISPLAYn` changes on replug.
pub struct Place {
    gdi_name: String,
    left: i32,
    top: i32,
}

/// Every active monitor with its EDID serial and name, as the settings window lists them.
pub fn monitors() -> Result<Vec<(MonitorId, Place)>, String> {
    Ok(guard::monitors()?
        .into_iter()
        .map(|monitor| {
            let id = MonitorId {
                serial: edid(&monitor.device_path).and_then(|edid| edid_serial(&edid)),
                name: monitor.name,
            };
            let place = Place {
                gdi_name: monitor.gdi_name,
                left: monitor.left,
                top: monitor.top,
            };
            (id, place)
        })
        .collect())
}

/// The monitor's EDID from its device registry key (`Device Parameters\EDID`), or `None`.
fn edid(device_path: &str) -> Option<Vec<u8>> {
    let node = guard::device_node(device_path)?;
    let mut key = HKEY::default();
    // SAFETY: key is a valid out-pointer; the key is closed below.
    let opened = unsafe {
        CM_Open_DevNode_Key(
            node,
            KEY_READ.0,
            0,
            RegDisposition_OpenExisting,
            &mut key,
            CM_REGISTRY_HARDWARE,
        )
    };
    if opened != CR_SUCCESS {
        return None;
    }
    let read = |data: Option<&mut [u8]>, size: &mut u32| {
        let data = data.map(|data| data.as_mut_ptr().cast());
        // SAFETY: key is open; `data` is None or a buffer of `size` bytes, and Windows writes no more.
        unsafe {
            RegGetValueW(
                key,
                PCWSTR::null(),
                w!("EDID"),
                RRF_RT_REG_BINARY,
                None,
                data,
                Some(size),
            )
        }
    };
    let mut size = 0;
    let mut edid = Vec::new();
    if read(None, &mut size) == ERROR_SUCCESS {
        edid.resize(size as usize, 0);
        if read(Some(&mut edid), &mut size) != ERROR_SUCCESS {
            edid.clear();
        }
        edid.truncate(size as usize);
    }
    // SAFETY: key was opened above and is not used after this.
    let _ = unsafe { RegCloseKey(key) };
    (!edid.is_empty()).then_some(edid)
}

/// Opens the monitor at `place` over DDC/CI; `None` when it has moved or shares its desktop with another monitor (clone mode).
pub fn open(place: Place) -> Option<Box<dyn Ddc>> {
    let corner = POINT {
        x: place.left,
        y: place.top,
    };
    // SAFETY: plain lookup.
    let monitor = unsafe { MonitorFromPoint(corner, MONITOR_DEFAULTTONULL) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = u32::try_from(size_of::<MONITORINFOEXW>()).ok()?;
    // SAFETY: info is a MONITORINFOEXW whose cbSize says so; a null monitor just fails.
    if !unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast()) }.as_bool()
        || !guard::wide_to_string(&info.szDevice).eq_ignore_ascii_case(&place.gdi_name)
    {
        return None;
    }
    let mut count = 0;
    // SAFETY: count is a valid out-pointer.
    unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count) }.ok()?;
    if count != 1 {
        return None;
    }
    let mut physical = [PHYSICAL_MONITOR::default()];
    // SAFETY: the array holds the one physical monitor counted above; its handle is destroyed on drop.
    unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, &mut physical) }.ok()?;
    Some(Box::new(PhysicalMonitor(physical[0].hPhysicalMonitor)))
}

/// A dxva2 physical monitor handle, destroyed on drop.
struct PhysicalMonitor(HANDLE);

impl Drop for PhysicalMonitor {
    fn drop(&mut self) {
        // SAFETY: the handle came from GetPhysicalMonitorsFromHMONITOR and is not used after this.
        let _ = unsafe { DestroyPhysicalMonitor(self.0) };
    }
}

impl Ddc for PhysicalMonitor {
    fn capabilities(&mut self) -> Option<String> {
        let mut length = 0;
        // SAFETY: the handle is open and length a valid out-pointer.
        if unsafe { GetCapabilitiesStringLength(self.0, &mut length) } == 0 || length == 0 {
            return None;
        }
        let mut text = vec![0; usize::try_from(length).ok()?];
        // SAFETY: the handle is open; Windows writes at most text.len() bytes, null included.
        if unsafe { CapabilitiesRequestAndCapabilitiesReply(self.0, &mut text) } == 0 {
            return None;
        }
        let end = text.iter().position(|b| *b == 0).unwrap_or(text.len());
        text.truncate(end);
        String::from_utf8(text).ok()
    }

    fn current_input(&mut self) -> Option<u8> {
        let mut value = 0u32;
        // SAFETY: the handle is open and value a valid out-pointer; the optional outputs are not wanted.
        let read = unsafe {
            GetVCPFeatureAndVCPFeatureReply(self.0, INPUT_SELECT, None, &mut value, None)
        };
        (read != 0).then_some(value.to_le_bytes()[0])
    }

    fn set_input(&mut self, value: u8) -> bool {
        // SAFETY: the handle is open.
        unsafe { SetVCPFeature(self.0, INPUT_SELECT, u32::from(value)) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{command_monitor, run_command};
    use super::{monitors, open};
    use crate::guard::second_monitor;

    /// The CI virtual monitors have no DDC/CI (docs/win32-ddc.md, CI), so a switch fails cleanly.
    #[test]
    fn second_monitor_switch_without_ddc_ci_exits_1() {
        if second_monitor::monitors(3).is_none() {
            return;
        }
        let ids: Vec<_> = monitors().unwrap().into_iter().map(|(id, _)| id).collect();
        second_monitor::log(&format!("monitors: {ids:?}"));
        let mut tried = 0;
        for index in 0..ids.len() {
            let Some(wanted) = command_monitor(&ids, index) else {
                continue;
            };
            let args = ["input", wanted, "hdmi1"].map(str::to_owned);
            let (code, line) = run_command(&args, monitors, open);
            second_monitor::log(&format!("{args:?}: {code} {line}"));
            assert_eq!(code, 0, "{line}");
            assert!(
                line.ends_with(": the monitor did not answer over DDC/CI."),
                "{line}"
            );
            tried += 1;
        }
        assert!(tried > 0, "no monitor has a serial or name of its own");
    }
}
