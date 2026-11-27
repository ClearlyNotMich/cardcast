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
    /// Starts relaying `audio_source` (a playback device id) to the sound output [`card_output`]
    /// picks for `output` and the card display. Problems arrive later through `take_report`, as
    /// the relay keeps retrying.
    fn start(
        &mut self,
        audio_source: &str,
        output: &SoundOutput,
        card_display_container: &str,
        card_display_name: &str,
    );
    fn stop(&mut self);
    /// The relay's latest change for the status line.
    fn take_report(&mut self) -> Option<RelayReport>;
}

/// What the relay tells the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows relay and the tests send them")
)]
pub enum RelayReport {
    /// It keeps failing, for this reason in plain words.
    Failing(String),
    /// Automatic found no sound output ([`NO_SOUND_OUTPUT`]); the user can choose one or No audio.
    NoSoundOutput,
    /// It relays again after a reported failure.
    Relaying,
}

impl From<String> for RelayReport {
    fn from(problem: String) -> Self {
        Self::Failing(problem)
    }
}

/// The "Sound output" setting: where the audio source is played.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SoundOutput {
    /// The card display's own sound output, found by container ID, then by name.
    #[default]
    Automatic,
    /// This playback device (an endpoint id); Automatic while it is not connected.
    Device(String),
    /// No relay, for a capture card without sound.
    NoAudio,
}

impl SoundOutput {
    /// As saved in settings: "automatic", "none" or the endpoint id, which never reads either.
    pub fn to_text(&self) -> &str {
        match self {
            Self::Automatic => "automatic",
            Self::NoAudio => "none",
            Self::Device(id) => id,
        }
    }

    pub fn from_text(text: &str) -> Self {
        match text {
            "" | "automatic" => Self::Automatic,
            "none" => Self::NoAudio,
            id => Self::Device(id.to_owned()),
        }
    }
}

/// The audio relay; `wake` asks the settings window to repaint when a report is waiting.
#[cfg(windows)]
pub fn new_relay(wake: impl Fn() + Send + 'static) -> Box<dyn Relay> {
    Box::new(relay::WasapiRelay::new(wake))
}

/// What the status line keeps saying while the relay is not built for this platform (#36).
#[cfg(windows)]
pub const NOT_BUILT: Option<&str> = None;
#[cfg(not(windows))]
pub const NOT_BUILT: Option<&str> = Some("The audio relay does not work on Linux yet.");

/// The Linux relay is not built yet (#36), so every start reports that.
#[cfg(not(windows))]
pub fn new_relay(_wake: impl Fn() + Send + 'static) -> Box<dyn Relay> {
    Box::new(NotOnLinuxYet { started: false })
}

/// `started` until the start's report is taken.
#[cfg(not(windows))]
struct NotOnLinuxYet {
    started: bool,
}

#[cfg(not(windows))]
impl Relay for NotOnLinuxYet {
    fn start(&mut self, _source: &str, _output: &SoundOutput, _container: &str, _name: &str) {
        self.started = true;
    }
    fn stop(&mut self) {}
    fn take_report(&mut self) -> Option<RelayReport> {
        std::mem::take(&mut self.started)
            .then(|| RelayReport::Failing(NOT_BUILT.unwrap_or_default().into()))
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
    /// `PKEY_Device_ContainerId`, the physical device it is part of; empty if unknown.
    pub container_id: String,
}

/// The status line when Automatic finds no sound output, next to a "Choose sound output" button.
pub const NO_SOUND_OUTPUT: &str = "Can't find the capture card's sound output. Choose it, or choose No audio if the capture card has no sound.";

/// Where the audio source is played for the `output` setting, or `None` for No audio: the chosen
/// playback device while it is connected, else [`automatic`] without the audio source. The error
/// is the status line's report.
pub fn card_output<'a>(
    devices: &'a [AudioDevice],
    audio_source: &str,
    output: &SoundOutput,
    card_display_container: &str,
    card_display_name: &str,
) -> Result<Option<&'a AudioDevice>, RelayReport> {
    if !devices.iter().any(|device| device.id == audio_source) {
        return Err("The audio source is not connected.".to_owned().into());
    }
    match output {
        SoundOutput::NoAudio => return Ok(None),
        SoundOutput::Device(id) if id == audio_source => {
            return Err(
                "The audio source can't also be the sound output. Choose another sound output."
                    .to_owned()
                    .into(),
            );
        }
        SoundOutput::Device(id) => {
            if let Some(device) = devices.iter().find(|device| &device.id == id) {
                return Ok(Some(device));
            }
        }
        SoundOutput::Automatic => {}
    }
    automatic(
        devices,
        Some(audio_source),
        card_display_container,
        card_display_name,
    )
    .map(Some)
    .ok_or(RelayReport::NoSoundOutput)
}

/// The playback device `output` plays to, whatever the audio source: the chosen one while it is
/// connected, else what [`automatic`] finds; `None` for No audio or when nothing is found.
pub fn sound_output_device<'a>(
    devices: &'a [AudioDevice],
    output: &SoundOutput,
    card_display_container: &str,
    card_display_name: &str,
) -> Option<&'a AudioDevice> {
    match output {
        SoundOutput::NoAudio => None,
        SoundOutput::Device(id) => devices
            .iter()
            .find(|device| &device.id == id)
            .or_else(|| automatic(devices, None, card_display_container, card_display_name)),
        SoundOutput::Automatic => {
            automatic(devices, None, card_display_container, card_display_name)
        }
    }
}

/// Automatic: the one playback device in the card display's container (Windows gives a monitor
/// and its HDMI or DisplayPort sound output the same container ID), else the one playback device
/// named after the card display; never `audio_source`, so `None` when that is the only match.
fn automatic<'a>(
    devices: &'a [AudioDevice],
    audio_source: Option<&str>,
    card_display_container: &str,
    card_display_name: &str,
) -> Option<&'a AudioDevice> {
    let mut in_container = devices.iter().filter(|device| {
        !card_display_container.is_empty()
            && device
                .container_id
                .eq_ignore_ascii_case(card_display_container)
    });
    if let (Some(device), None) = (in_container.next(), in_container.next()) {
        return (Some(device.id.as_str()) != audio_source).then_some(device);
    }
    let card_display_name = card_display_name.trim();
    let mut named = devices.iter().filter(|device| {
        Some(device.id.as_str()) != audio_source
            && without_number(&device.description).eq_ignore_ascii_case(card_display_name)
    });
    match (named.next(), named.next()) {
        (Some(device), None) => Some(device),
        _ => None,
    }
}

/// A playback device's name without the number Windows puts in front when names repeat
/// (digits, a hyphen and a space), so "1- Speakers" reads "Speakers" but "3-D Audio" stays.
fn without_number(name: &str) -> &str {
    let name = name.trim();
    let rest = name.trim_start_matches(|c: char| c.is_ascii_digit());
    match rest.strip_prefix("- ") {
        Some(rest) if rest.len() < name.len() - 2 => rest.trim(),
        _ => name,
    }
}

/// Linux lists no playback devices yet (#36); [`NOT_BUILT`] tells the user.
#[cfg(not(windows))]
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    Ok(Vec::new())
}

/// Active playback devices from WASAPI.
#[cfg(windows)]
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_ContainerId, PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName,
    };
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantToGUID;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        STGM_READ,
    };

    let failed =
        |error: windows::core::Error| format!("Could not list playback devices. ({error})");
    // SAFETY: plain COM initialisation of this thread. On the UI thread this joins winit's STA; on the relay
    // thread, already MTA, it returns RPC_E_CHANGED_MODE and COM stays initialised as MTA, so the calls below
    // are valid on either. Left initialised for the life of the thread on purpose.
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
        // SAFETY: store is a live COM object and the key is a static PROPERTYKEY.
        let container = unsafe { store.GetValue(&PKEY_Device_ContainerId) };
        // SAFETY: the PROPVARIANT is GetValue's own; PropVariantToGUID fails on any type but a GUID.
        let container = container.and_then(|value| unsafe { PropVariantToGUID(&value) });
        devices.push(AudioDevice {
            id: id.map_err(|_| "A playback device has an unreadable id.".to_owned())?,
            name: name.to_string(),
            description: description.to_string(),
            container_id: container.map_or_else(|_| String::new(), |guid| format!("{guid:?}")),
        });
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn on_linux_the_audio_relay_says_it_does_not_run_yet() {
        let not_yet = "The audio relay does not work on Linux yet.";
        assert_eq!(playback_devices(), Ok(Vec::new()));
        assert_eq!(NOT_BUILT, Some(not_yet));
        let mut relay = new_relay(|| {});
        assert_eq!(relay.take_report(), None);
        relay.start("speakers", &SoundOutput::Automatic, "card", "Elgato");
        assert_eq!(
            relay.take_report(),
            Some(RelayReport::Failing(not_yet.into()))
        );
        assert_eq!(relay.take_report(), None);
    }

    const CARD: &str = "card container";

    fn device(id: &str, description: &str) -> AudioDevice {
        AudioDevice {
            id: id.into(),
            name: format!("{description} (High Definition Audio)"),
            description: description.into(),
            container_id: format!("container of {id}"),
        }
    }

    fn in_container(id: &str, description: &str, container_id: &str) -> AudioDevice {
        AudioDevice {
            container_id: container_id.into(),
            ..device(id, description)
        }
    }

    /// The sender in Display EDID mode: the card display reads "Elgato 1080p", its playback device "HD60 X".
    fn sender_devices() -> Vec<AudioDevice> {
        vec![
            device("headphones", "Headphones"),
            in_container("hdmi-card", "HD60 X", CARD),
            device("hdmi-monitor", "DELL U2720Q"),
        ]
    }

    fn automatic<'a>(
        devices: &'a [AudioDevice],
        audio_source: &str,
        container: &str,
        name: &str,
    ) -> Result<Option<&'a AudioDevice>, RelayReport> {
        card_output(
            devices,
            audio_source,
            &SoundOutput::Automatic,
            container,
            name,
        )
    }

    fn not_found() -> Result<Option<&'static AudioDevice>, RelayReport> {
        Err(RelayReport::NoSoundOutput)
    }

    #[test]
    fn automatic_finds_the_sound_output_by_container_id_whatever_its_name() {
        let devices = sender_devices();
        assert_eq!(
            automatic(&devices, "headphones", CARD, "Elgato 1080p"),
            Ok(Some(&devices[1]))
        );
    }

    #[test]
    fn automatic_without_a_container_id_and_another_name_asks_the_user() {
        let devices = sender_devices();
        assert_eq!(
            automatic(&devices, "headphones", "", "Elgato 1080p"),
            not_found()
        );
        assert_eq!(
            automatic(
                &devices,
                "headphones",
                "container of the TV",
                "Elgato 1080p"
            ),
            not_found()
        );
    }

    #[test]
    fn container_match_ignores_case() {
        let devices = vec![
            device("headphones", "Headphones"),
            in_container(
                "hdmi-card",
                "HD60 X",
                "8C7ED206-3F8A-4827-B3AB-AE9E1FAEFC6C",
            ),
        ];
        assert_eq!(
            automatic(
                &devices,
                "headphones",
                "8c7ed206-3f8a-4827-b3ab-ae9e1faefc6c",
                "Elgato 1080p"
            ),
            Ok(Some(&devices[1]))
        );
    }

    #[test]
    fn a_container_shared_by_several_devices_falls_back_to_the_name() {
        // As when the card display reports the computer's own container, which the speakers share.
        let devices = vec![
            in_container("speakers", "Speakers", "computer"),
            in_container("headphones", "Headphones", "computer"),
            in_container("hdmi-card", "HD60 X", "computer"),
        ];
        assert_eq!(
            automatic(&devices, "speakers", "computer", "HD60 X"),
            Ok(Some(&devices[2]))
        );
    }

    #[test]
    fn the_card_displays_own_sound_output_cannot_be_the_audio_source() {
        let devices = sender_devices();
        assert_eq!(
            automatic(&devices, "hdmi-card", CARD, "Elgato 1080p"),
            not_found(),
            "the audio source never plays into itself"
        );
    }

    #[test]
    fn without_a_container_id_the_name_finds_it_ignoring_case_and_spaces() {
        let devices = sender_devices();
        for name in ["HD60 X", " hd60 x "] {
            assert_eq!(
                automatic(&devices, "headphones", "", name),
                Ok(Some(&devices[1])),
                "{name}"
            );
        }
    }

    #[test]
    fn the_name_match_skips_the_audio_source_with_the_same_name() {
        // Two capture cards: Windows numbers their playback devices, and one is the audio source.
        let devices = vec![
            device("other-card", "1- HD60 X"),
            device("hdmi-card", "2- HD60 X"),
        ];
        assert_eq!(
            automatic(&devices, "other-card", "", "HD60 X"),
            Ok(Some(&devices[1]))
        );
    }

    #[test]
    fn two_playback_devices_with_the_card_displays_name_are_not_found() {
        let devices = vec![
            device("headphones", "Headphones"),
            device("hdmi-card", "1- HD60 X"),
            device("other-card", "2- HD60 X"),
        ];
        assert_eq!(automatic(&devices, "headphones", "", "HD60 X"), not_found());
    }

    #[test]
    fn only_the_audio_source_with_the_name_is_not_found() {
        let devices = sender_devices();
        assert_eq!(automatic(&devices, "hdmi-card", "", "HD60 X"), not_found());
    }

    #[test]
    fn only_windows_numbering_is_stripped_from_names() {
        assert_eq!(without_number("1- HD60 X"), "HD60 X");
        assert_eq!(without_number("12- HD60 X"), "HD60 X");
        assert_eq!(without_number(" HD60 X "), "HD60 X");
        for kept in [
            "3-D Audio",
            "2-HD60 X",
            "27GR95QE",
            "2 Speakers",
            "1 - HD60 X",
        ] {
            assert_eq!(without_number(kept), kept);
        }
    }

    #[test]
    fn a_chosen_sound_output_is_used_whatever_the_card_display() {
        let devices = sender_devices();
        let chosen = SoundOutput::Device("hdmi-monitor".into());
        assert_eq!(
            card_output(&devices, "headphones", &chosen, CARD, "Elgato 1080p"),
            Ok(Some(&devices[2]))
        );
    }

    #[test]
    fn a_chosen_sound_output_that_is_gone_falls_back_to_automatic() {
        let devices = sender_devices();
        let chosen = SoundOutput::Device("unplugged".into());
        assert_eq!(
            card_output(&devices, "headphones", &chosen, CARD, "Elgato 1080p"),
            Ok(Some(&devices[1]))
        );
    }

    #[test]
    fn a_chosen_sound_output_cannot_be_the_audio_source() {
        let devices = sender_devices();
        let chosen = SoundOutput::Device("headphones".into());
        assert_eq!(
            card_output(&devices, "headphones", &chosen, CARD, "Elgato 1080p"),
            Err(RelayReport::Failing(
                "The audio source can't also be the sound output. Choose another sound output."
                    .into()
            ))
        );
    }

    #[test]
    fn no_audio_has_no_sound_output() {
        let devices = sender_devices();
        assert_eq!(
            card_output(
                &devices,
                "headphones",
                &SoundOutput::NoAudio,
                CARD,
                "Elgato 1080p"
            ),
            Ok(None)
        );
    }

    #[test]
    fn a_missing_audio_source_is_reported() {
        let devices = sender_devices();
        assert_eq!(
            automatic(&devices, "gone", CARD, "Elgato 1080p"),
            Err(RelayReport::Failing(
                "The audio source is not connected.".into()
            ))
        );
    }

    #[test]
    fn sound_output_survives_save_and_load() {
        for output in [
            SoundOutput::Automatic,
            SoundOutput::NoAudio,
            SoundOutput::Device("{0.0.0.00000000}.{abc}".into()),
        ] {
            assert_eq!(SoundOutput::from_text(output.to_text()), output);
        }
        assert_eq!(SoundOutput::from_text(""), SoundOutput::Automatic);
    }

    #[test]
    fn the_not_found_text_is_plain() {
        assert_eq!(
            NO_SOUND_OUTPUT,
            "Can't find the capture card's sound output. Choose it, or choose No audio if the capture card has no sound."
        );
    }
}
