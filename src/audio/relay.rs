//! The audio relay: WASAPI loopback of the audio source, played on the card display's sound
//! output, on its own thread. See docs/win32-mirror-audio.md, "WASAPI loopback and rendering".

use super::RelayReport;
use super::pacing::{self, Drift};
use super::samples::{self, Format, Sample};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_RATEADJUST, IAudioCaptureClient,
    IAudioClient, IAudioClockAdjustment, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::HSTRING;

type Wake = Arc<dyn Fn() + Send + Sync>;

/// A failed open is retried this often: the card display's sound output appears a moment after
/// the card display does, and a removed device comes back the same way.
const RETRY: Duration = Duration::from_secs(1);
/// Failures in a row before the status line hears of it, so a re-plug does not flash an error.
const REPORT_AFTER: u32 = 3;
/// How long one wait for captured audio lasts before checking for Stop, in milliseconds.
const WAIT_MS: u32 = 100;

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct WasapiRelay {
    running: Option<Running>,
    report_sender: Sender<RelayReport>,
    reports: Receiver<RelayReport>,
    wake: Wake,
}

impl WasapiRelay {
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (report_sender, reports) = mpsc::channel();
        let wake = std::sync::Mutex::new(wake);
        Self {
            running: None,
            report_sender,
            reports,
            wake: Arc::new(move || {
                if let Ok(wake) = wake.lock() {
                    wake();
                }
            }),
        }
    }
}

impl Drop for WasapiRelay {
    fn drop(&mut self) {
        super::Relay::stop(self);
    }
}

impl super::Relay for WasapiRelay {
    fn start(&mut self, audio_source: &str, card_display_container: &str, card_display_name: &str) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let card_display = (
                card_display_container.to_owned(),
                card_display_name.to_owned(),
            );
            let audio_source = audio_source.to_owned();
            let (stop, reports, wake) =
                (stop.clone(), self.report_sender.clone(), self.wake.clone());
            std::thread::spawn(move || {
                run(&audio_source, &card_display, &stop, &reports, &wake);
            })
        };
        self.running = Some(Running { stop, thread });
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::Relaxed);
            let _ = running.thread.join();
        }
        // A problem from the stopped relay must not overwrite the status set with Stop.
        self.reports.try_iter().for_each(drop);
    }

    fn take_report(&mut self) -> Option<RelayReport> {
        self.reports.try_iter().last()
    }
}

/// Relays until stopped, re-opening both devices after any failure.
fn run(
    audio_source: &str,
    card_display: &(String, String),
    stop: &AtomicBool,
    reports: &Sender<RelayReport>,
    wake: &Wake,
) {
    // SAFETY: plain COM initialisation of this thread; it owns no apartment-bound objects.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let mut failures = 0;
    while !stop.load(Ordering::Relaxed) {
        let mut opened = || {
            if failures >= REPORT_AFTER {
                let _ = reports.send(RelayReport::Relaying);
                wake();
            }
            failures = 0;
        };
        let Err(error) = relay(audio_source, card_display, stop, &mut opened) else {
            break;
        };
        failures += 1;
        if failures == REPORT_AFTER {
            let _ = reports.send(RelayReport::Failing(error));
            wake();
        }
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(RETRY / 10);
        }
    }
    if com.is_ok() {
        // SAFETY: balances the successful CoInitializeEx above; every COM object of this thread was dropped in `relay`.
        unsafe { CoUninitialize() };
    }
}

/// Opens both devices and relays until `stop`; `Ok` only when stopped.
fn relay(
    audio_source: &str,
    (card_display_container, card_display_name): &(String, String),
    stop: &AtomicBool,
    opened: &mut dyn FnMut(),
) -> Result<(), String> {
    let devices = super::playback_devices()?;
    let output = super::card_output(
        &devices,
        audio_source,
        card_display_container,
        card_display_name,
    )?;
    // Declared before the clients so it is closed after them.
    let event = Event::new()?;
    // SAFETY: COM is initialised on this thread by `run`.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(failed("open the audio devices"))?;
    let (capture_client, from) = open(
        &enumerator,
        audio_source,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        "record the audio source",
    )?;
    // SAFETY: capture_client is initialised for event callbacks; the event outlives it (declared first).
    unsafe { capture_client.SetEventHandle(event.0) }.map_err(failed("record the audio source"))?;
    // SAFETY: capture_client is an initialised capture stream.
    let capture: IAudioCaptureClient =
        unsafe { capture_client.GetService() }.map_err(failed("record the audio source"))?;
    let (render_client, to) = open(
        &enumerator,
        &output.id,
        AUDCLNT_STREAMFLAGS_RATEADJUST,
        "play on the card display",
    )?;
    let playing = failed("play on the card display");
    // SAFETY: render_client is an initialised render stream.
    let buffer_frames = unsafe { render_client.GetBufferSize() }.map_err(&playing)?;
    // SAFETY: render_client is an initialised render stream.
    let render: IAudioRenderClient = unsafe { render_client.GetService() }.map_err(&playing)?;
    // SAFETY: render_client was initialised with AUDCLNT_STREAMFLAGS_RATEADJUST, which exposes this service.
    let clock: IAudioClockAdjustment = unsafe { render_client.GetService() }.map_err(&playing)?;
    let target = pacing::target_padding(to.rate, buffer_frames);
    let mut drift = Drift::default();
    if let Some(rate) = drift.update(from.rate, target, target) {
        // SAFETY: clock is a live COM object; this is cardcast's own thread, not a real-time engine thread.
        unsafe { clock.SetSampleRate(rate) }.map_err(failed(
            "match the audio source's sample rate on the card display (set both to the same rate in Windows' Sound settings)",
        ))?;
    }
    write(&render, target, None, to).map_err(&playing)?;
    // SAFETY: both clients are initialised; started streams are stopped below before they drop.
    unsafe { render_client.Start() }.map_err(&playing)?;
    // SAFETY: as above.
    let started = unsafe { capture_client.Start() }.map_err(failed("record the audio source"));
    opened();
    let result = started.and_then(|()| {
        pump(
            &event,
            &capture,
            &render_client,
            &render,
            &clock,
            (from, to),
            (buffer_frames, target),
            &mut drift,
            stop,
        )
    });
    // SAFETY: both clients are live; stopping a stopped stream is harmless.
    let _ = unsafe { capture_client.Stop() };
    // SAFETY: as above.
    let _ = unsafe { render_client.Stop() };
    result
}

/// Moves captured audio into the render stream until `stop`.
#[expect(
    clippy::too_many_arguments,
    reason = "the open streams and their sizes, passed once from `relay`"
)]
fn pump(
    event: &Event,
    capture: &IAudioCaptureClient,
    render_client: &IAudioClient,
    render: &IAudioRenderClient,
    clock: &IAudioClockAdjustment,
    (from, to): (Format, Format),
    (buffer_frames, target): (u32, u32),
    drift: &mut Drift,
    stop: &AtomicBool,
) -> Result<(), String> {
    let recording = failed("record the audio source");
    let playing = failed("play on the card display");
    let mut converted = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: the event handle is open for the life of `event`.
        if unsafe { WaitForSingleObject(event.0, WAIT_MS) } != WAIT_OBJECT_0 {
            // No packets while nothing plays on the audio source; check for Stop and wait again.
            continue;
        }
        converted.clear();
        // SAFETY: capture is a started capture stream.
        while unsafe { capture.GetNextPacketSize() }.map_err(&recording)? > 0 {
            let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0, 0);
            // SAFETY: the out pointers are valid locals; the buffer is released below before the next GetBuffer.
            unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }
                .map_err(&recording)?;
            let bytes = frames as usize * from.frame_bytes();
            if data.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0.cast_unsigned() != 0 {
                // Zero bytes are silence in every sample format.
                converted.resize(converted.len() + frames as usize * to.frame_bytes(), 0);
            } else {
                // SAFETY: GetBuffer returned `frames` whole frames of the mix format at `data`, valid until ReleaseBuffer.
                let input = unsafe { std::slice::from_raw_parts(data, bytes) };
                samples::convert(input, from, to, &mut converted);
            }
            // SAFETY: releases exactly the packet obtained above.
            unsafe { capture.ReleaseBuffer(frames) }.map_err(&recording)?;
        }
        // SAFETY: render_client is a started render stream.
        let padding = unsafe { render_client.GetCurrentPadding() }.map_err(&playing)?;
        let incoming = u32::try_from(converted.len() / to.frame_bytes()).unwrap_or(u32::MAX);
        let plan = pacing::plan_write(padding, buffer_frames, target, incoming);
        write(render, plan.silence, None, to).map_err(&playing)?;
        let data = &converted[..plan.data as usize * to.frame_bytes()];
        write(render, plan.data, Some(data), to).map_err(&playing)?;
        // An emptied buffer says nothing about drift: the audio source was just quiet.
        if plan.silence == 0
            && let Some(rate) = drift.update(from.rate, padding, target)
        {
            // SAFETY: clock is a live COM object; this is cardcast's own thread, not a real-time engine thread.
            unsafe { clock.SetSampleRate(rate) }.map_err(&playing)?;
        }
    }
    Ok(())
}

/// Writes `frames` frames of `data`, or of silence when `data` is `None`.
fn write(
    render: &IAudioRenderClient,
    frames: u32,
    data: Option<&[u8]>,
    to: Format,
) -> windows::core::Result<()> {
    if frames == 0 {
        return Ok(());
    }
    // SAFETY: render is a live render client; the caller keeps `frames` within the free space.
    let buffer = unsafe { render.GetBuffer(frames) }?;
    let flags = match data {
        Some(data) => {
            // Release-mode check: the copy below is unsound without it.
            assert_eq!(data.len(), frames as usize * to.frame_bytes());
            // SAFETY: GetBuffer returned room for `frames` frames, and the assert above makes that exactly data.len() bytes.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buffer, data.len()) };
            0
        }
        None => AUDCLNT_BUFFERFLAGS_SILENT.0.cast_unsigned(),
    };
    // SAFETY: releases exactly the frames obtained above.
    unsafe { render.ReleaseBuffer(frames, flags) }
}

/// Activates and initialises a shared-mode stream on the device `id` in its own mix format.
fn open(
    enumerator: &IMMDeviceEnumerator,
    id: &str,
    flags: u32,
    what: &'static str,
) -> Result<(IAudioClient, Format), String> {
    let failed = failed(what);
    // SAFETY: enumerator is a live COM object.
    let device: IMMDevice = unsafe { enumerator.GetDevice(&HSTRING::from(id)) }.map_err(&failed)?;
    // SAFETY: device is a live COM object.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(&failed)?;
    // SAFETY: client is a live, uninitialised audio client.
    let mix = MixFormat(unsafe { client.GetMixFormat() }.map_err(&failed)?);
    // SAFETY: GetMixFormat returned a valid format, freed when `mix` drops.
    let format = unsafe { format_of(mix.0) }
        .ok_or_else(|| format!("Audio stopped: could not {what}. (unsupported sample format)"))?;
    // SAFETY: mix points to the device's own mix format, valid until `mix` drops after this call.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            pacing::BUFFER_DURATION,
            0,
            mix.0,
            None,
        )
    }
    .map_err(&failed)?;
    Ok((client, format))
}

/// A mix format from `GetMixFormat`, freed with `CoTaskMemFree`.
struct MixFormat(*mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: the pointer came from GetMixFormat, which allocates with CoTaskMemAlloc; it is not used after this.
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

/// Reads a wave format, plain or extensible.
///
/// # Safety
/// `wave` points to a valid `WAVEFORMATEX`, followed by the rest of a `WAVEFORMATEXTENSIBLE`
/// when its tag says so.
unsafe fn format_of(wave: *const WAVEFORMATEX) -> Option<Format> {
    // SAFETY: the caller guarantees a valid WAVEFORMATEX; the struct is packed, so read unaligned.
    let header = unsafe { wave.read_unaligned() };
    let float = match u32::from(header.wFormatTag) {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_PCM => false,
        WAVE_FORMAT_EXTENSIBLE => {
            // SAFETY: the extensible tag means the caller's buffer holds a whole WAVEFORMATEXTENSIBLE.
            let extensible = unsafe { wave.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
            match extensible.SubFormat {
                KSDATAFORMAT_SUBTYPE_IEEE_FLOAT => true,
                KSDATAFORMAT_SUBTYPE_PCM => false,
                _ => return None,
            }
        }
        _ => return None,
    };
    let format = Format {
        sample: Sample::from_wave(float, header.wBitsPerSample)?,
        channels: header.nChannels,
        rate: header.nSamplesPerSec,
    };
    (header.nChannels > 0 && usize::from(header.nBlockAlign) == format.frame_bytes())
        .then_some(format)
}

/// An auto-reset event handle, closed on drop.
struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self, String> {
        // SAFETY: plain event creation with no security attributes or name.
        unsafe { CreateEventW(None, false, false, None) }
            .map(Self)
            .map_err(failed("wait for the audio source"))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed once, here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A status-line message for a failed WASAPI call.
fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| format!("Audio stopped: could not {what}. ({})", error.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Relay;
    use std::time::Instant;

    #[test]
    fn float_mix_format_is_read() {
        let mut wave = WAVEFORMATEXTENSIBLE::default();
        wave.Format.wFormatTag = u16::try_from(WAVE_FORMAT_EXTENSIBLE).unwrap();
        wave.Format.nChannels = 2;
        wave.Format.nSamplesPerSec = 48_000;
        wave.Format.nBlockAlign = 8;
        wave.Format.wBitsPerSample = 32;
        wave.Format.cbSize = 22;
        wave.SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        // SAFETY: points to a whole local WAVEFORMATEXTENSIBLE.
        let format = unsafe { format_of(std::ptr::addr_of!(wave).cast()) };
        assert_eq!(
            format,
            Some(Format {
                sample: Sample::F32,
                channels: 2,
                rate: 48_000
            })
        );
    }

    #[test]
    fn pcm_mix_format_is_read_and_a_bad_block_size_is_refused() {
        let mut wave = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: 44_100,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            ..Default::default()
        };
        // SAFETY: points to a whole local WAVEFORMATEX with a plain tag.
        let format = unsafe { format_of(&wave) };
        assert_eq!(format.map(|format| format.sample), Some(Sample::I16));
        wave.nBlockAlign = 3;
        // SAFETY: as above.
        assert_eq!(unsafe { format_of(&wave) }, None);
    }

    /// The windows-latest runner has no audio endpoints at all (checked for #9), so this runs
    /// the relay thread, COM and device listing up to the report of the missing audio source.
    #[test]
    fn missing_audio_source_reaches_the_status_line_and_stop_is_prompt() {
        let mut relay = WasapiRelay::new(|| {});
        relay.start("{0.0.0.00000000}.{missing}", "", "HD60 X");
        let deadline = Instant::now() + Duration::from_secs(15);
        let report = loop {
            if let Some(report) = relay.take_report() {
                break report;
            }
            assert!(Instant::now() < deadline, "no report");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(
            report,
            RelayReport::Failing("The audio source is not connected.".into())
        );
        let stopping = Instant::now();
        relay.stop();
        assert!(stopping.elapsed() < Duration::from_millis(500));
    }
}
