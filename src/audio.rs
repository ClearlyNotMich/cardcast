//! audio module. Windows calls live here.

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe only in mirror, guard and audio, for Windows API calls"
)]

#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows relay calls it; its tests run everywhere"
    )
)]
mod pacing;
#[cfg(windows)]
mod relay;
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "only the Windows relay calls it; its tests run everywhere"
    )
)]
mod samples;

/// What the settings window needs from the audio relay, which records the audio source and
/// plays it on the card display's sound output.
pub trait Relay {
    /// Starts relaying `audio_source` (a playback device id) to the sound output named after the
    /// card display. Problems arrive later through `take_report`, as the relay keeps retrying.
    fn start(&mut self, audio_source: &str, card_display_name: &str);
    fn stop(&mut self);
    /// The relay's latest problem for the status line, in plain words.
    fn take_report(&mut self) -> Option<String>;
}

/// The audio relay; `wake` asks the settings window to repaint when a report is waiting.
#[cfg(windows)]
pub fn new_relay(wake: impl Fn() + Send + 'static) -> Box<dyn Relay> {
    Box::new(relay::WasapiRelay::new(wake))
}

/// The relay cannot run on Linux, so the settings window gets the stand-in.
#[cfg(not(windows))]
pub fn new_relay(_wake: impl Fn() + Send + 'static) -> Box<dyn Relay> {
    Box::new(StandInRelay)
}

/// Stand-in for Linux; it does nothing.
#[cfg(not(windows))]
pub struct StandInRelay;

#[cfg(not(windows))]
impl Relay for StandInRelay {
    fn start(&mut self, _audio_source: &str, _card_display_name: &str) {}
    fn stop(&mut self) {}
    fn take_report(&mut self) -> Option<String> {
        None
    }
}

/// A playback device on the sender, as shown in the audio source dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Endpoint id; the stable key saved in settings.
    pub id: String,
    pub name: String,
    /// `PKEY_Device_DeviceDesc`; for an HDMI sound output it is the monitor's EDID name.
    pub description: String,
}

/// The card display's sound output: the playback device named after the card display, which
/// must not be the audio source itself.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows relay calls it")
)]
pub fn card_output<'a>(
    devices: &'a [AudioDevice],
    audio_source: &str,
    card_display_name: &str,
) -> Result<&'a AudioDevice, String> {
    if !devices.iter().any(|device| device.id == audio_source) {
        return Err("The audio source is not connected.".into());
    }
    let card_display_name = card_display_name.trim();
    let output = devices
        .iter()
        .find(|device| device.description.trim().eq_ignore_ascii_case(card_display_name))
        .ok_or_else(|| {
            format!(
                "Could not find the card display's sound output (a playback device named {card_display_name})."
            )
        })?;
    if output.id == audio_source {
        return Err(
            "The audio source is the card display's own sound output. Choose another audio source."
                .into(),
        );
    }
    Ok(output)
}

/// Stand-in playback devices so the settings window can be laid out on Linux.
#[cfg(not(windows))]
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    Ok(vec![AudioDevice {
        id: "stand-in-speakers".into(),
        name: "Stand-in speakers".into(),
        description: "Speakers".into(),
    }])
}

/// Active playback devices from WASAPI.
#[cfg(windows)]
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName,
    };
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
        // SAFETY: store is a live COM object and the key is a static PROPERTYKEY.
        let description = unsafe { store.GetValue(&PKEY_Device_DeviceDesc) }.map_err(failed)?;
        devices.push(AudioDevice {
            id: id.map_err(|_| "A playback device has an unreadable id.".to_owned())?,
            name: name.to_string(),
            description: description.to_string(),
        });
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, description: &str) -> AudioDevice {
        AudioDevice {
            id: id.into(),
            name: format!("{description} (High Definition Audio)"),
            description: description.into(),
        }
    }

    fn sender_devices() -> Vec<AudioDevice> {
        vec![
            device("headphones", "Headphones"),
            device("hdmi-card", "HD60 X"),
            device("hdmi-monitor", "DELL U2720Q"),
        ]
    }

    #[test]
    fn card_output_is_the_device_named_after_the_card_display() {
        let devices = sender_devices();
        assert_eq!(
            card_output(&devices, "headphones", "HD60 X"),
            Ok(&devices[1])
        );
    }

    #[test]
    fn card_output_name_match_ignores_case_and_spaces() {
        let devices = sender_devices();
        assert_eq!(
            card_output(&devices, "headphones", " hd60 x "),
            Ok(&devices[1])
        );
    }

    #[test]
    fn card_output_cannot_be_the_audio_source() {
        let devices = sender_devices();
        assert_eq!(
            card_output(&devices, "hdmi-card", "HD60 X"),
            Err("The audio source is the card display's own sound output. Choose another audio source.".into())
        );
    }

    #[test]
    fn missing_audio_source_or_card_output_is_reported() {
        let devices = sender_devices();
        assert_eq!(
            card_output(&devices, "gone", "HD60 X"),
            Err("The audio source is not connected.".into())
        );
        assert_eq!(
            card_output(&devices, "headphones", "Live Gamer Mini"),
            Err("Could not find the card display's sound output (a playback device named Live Gamer Mini).".into())
        );
    }
}
