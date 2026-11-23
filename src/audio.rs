//! audio module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(unsafe_code)]

/// A playback device on the sender, as shown in the audio source dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Endpoint id; the stable key saved in settings.
    pub id: String,
    pub name: String,
}

/// Stand-in playback devices so the settings window can be laid out on Linux.
#[cfg(not(windows))]
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    Ok(vec![AudioDevice {
        id: "stand-in-speakers".into(),
        name: "Stand-in speakers".into(),
    }])
}

/// Active playback devices from WASAPI.
#[cfg(windows)]
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        STGM_READ,
    };

    let failed =
        |error: windows::core::Error| format!("Could not list playback devices ({error}).");
    // SAFETY: plain COM initialisation of this thread; apartment-threaded to match the OLE init winit does on the UI thread.
    // Left initialised for the life of the thread on purpose: the UI thread lives as long as cardcast.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // SAFETY: COM is initialised on this thread above.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(failed)?;
    // SAFETY: enumerator is a live COM object.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }.map_err(failed)?;
    // SAFETY: collection is a live COM object.
    let count = unsafe { collection.GetCount() }.map_err(failed)?;
    let mut devices = Vec::new();
    for index in 0..count {
        // SAFETY: index is below the collection's count.
        let device = unsafe { collection.Item(index) }.map_err(failed)?;
        // SAFETY: device is a live COM object; the returned string is freed below with CoTaskMemFree.
        let id_ptr = unsafe { device.GetId() }.map_err(failed)?;
        // SAFETY: GetId returns a valid null-terminated wide string.
        let id = unsafe { id_ptr.to_string() };
        // SAFETY: the string was allocated by COM and is not used after this.
        unsafe { CoTaskMemFree(Some(id_ptr.0.cast())) };
        // SAFETY: device is a live COM object.
        let store = unsafe { device.OpenPropertyStore(STGM_READ) }.map_err(failed)?;
        // SAFETY: store is a live COM object and the key is a static PROPERTYKEY.
        let name = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.map_err(failed)?;
        devices.push(AudioDevice {
            id: id.map_err(|_| "A playback device has an unreadable id.".to_owned())?,
            name: name.to_string(),
        });
    }
    Ok(devices)
}
