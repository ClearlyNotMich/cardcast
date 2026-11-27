//! The Linux audio relay (docs/research/linux-support.md, section 3): PipeWire's `pw-loopback`
//! records the audio source's monitor and plays it on the sound output, and `pw-dump` lists the
//! sinks. Both come with PipeWire (Arch: `pipewire-audio`; Debian and Ubuntu: `pipewire-bin`).

use super::{AudioDevice, RelayReport, SoundOutput};
use serde_json::Value;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type Wake = Arc<dyn Fn() + Send + Sync>;

/// A failed start is retried this often: the card display's sink appears a moment after the card
/// display does, and a removed device comes back the same way.
const RETRY: Duration = Duration::from_secs(1);
/// Failures in a row before the status line hears of it, so a re-plug does not flash an error.
const REPORT_AFTER: u32 = 3;
/// How long `pw-loopback` must run before the relay counts as started: WirePlumber ends it within
/// a second when a target is missing (WirePlumber 0.4.17 and 0.5.19, checked).
const SETTLE: Duration = Duration::from_secs(2);

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct PipeWireRelay {
    running: Option<Running>,
    report_sender: Sender<RelayReport>,
    reports: Receiver<RelayReport>,
    wake: Wake,
}

impl PipeWireRelay {
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (report_sender, reports) = mpsc::channel();
        let wake = Mutex::new(wake);
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

impl Drop for PipeWireRelay {
    fn drop(&mut self) {
        super::Relay::stop(self);
    }
}

impl super::Relay for PipeWireRelay {
    fn start(
        &mut self,
        audio_source: &str,
        output: &SoundOutput,
        card_display_container: &str,
        card_display_name: &str,
    ) {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let card_display = (
                output.clone(),
                card_display_container.to_owned(),
                card_display_name.to_owned(),
            );
            let audio_source = audio_source.to_owned();
            let (stop, reports, wake) =
                (stop.clone(), self.report_sender.clone(), self.wake.clone());
            std::thread::spawn(move || run(&audio_source, &card_display, &stop, &reports, &wake))
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

/// Relays until stopped, starting the loopback again after any failure.
fn run(
    audio_source: &str,
    card_display: &(SoundOutput, String, String),
    stop: &AtomicBool,
    reports: &Sender<RelayReport>,
    wake: &Wake,
) {
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
            let _ = reports.send(error);
            wake();
        }
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(RETRY / 10);
        }
    }
}

/// Runs one `pw-loopback` until `stop`; `Ok` only when stopped, or for No audio.
fn relay(
    audio_source: &str,
    (output, card_display_container, card_display_name): &(SoundOutput, String, String),
    stop: &AtomicBool,
    opened: &mut dyn FnMut(),
) -> Result<(), RelayReport> {
    let devices = playback_devices()?;
    let Some(output) = super::card_output(
        &devices,
        audio_source,
        output,
        card_display_container,
        card_display_name,
    )?
    else {
        return Ok(());
    };
    let mut loopback = Loopback::start(audio_source, &output.id)?;
    let started = Instant::now();
    let mut settled = false;
    while !stop.load(Ordering::Relaxed) {
        if let Ok(Some(_)) | Err(_) = loopback.0.try_wait() {
            return Err("PipeWire ended the audio relay. Is the audio source or the sound output still connected?"
                .to_owned()
                .into());
        }
        if !settled && started.elapsed() >= SETTLE {
            settled = true;
            opened();
        }
        std::thread::sleep(RETRY / 10);
    }
    loopback.end();
    Ok(())
}

/// A running `pw-loopback`, ended when dropped.
struct Loopback(Child);

impl Loopback {
    fn start(audio_source: &str, output: &str) -> Result<Self, String> {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("pw-loopback");
        command
            .args(loopback_args(audio_source, output))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: prctl is async-signal-safe and touches no memory of the parent. It ends the
        // loopback when the relay thread ends, so one never outlives a crashed cardcast.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().map(Self).map_err(|error| {
            format!(
                "Could not start the audio relay; is PipeWire installed? (pw-loopback: {error})"
            )
        })
    }

    fn end(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.end();
    }
}

/// `pw-loopback`'s arguments: record the monitor of the `audio_source` sink and play it on the
/// `output` sink, both by `node.name`. Neither stream moves to another sink when its own goes, so
/// the audio source is never played back into the sender's speakers; WirePlumber ends the loopback
/// instead.
fn loopback_args(audio_source: &str, output: &str) -> [String; 6] {
    let stay = "node.dont-reconnect=true node.dont-fallback=true";
    [
        "--name".into(),
        "cardcast".into(),
        "--capture-props".into(),
        format!(
            "stream.capture.sink=true target.object={} {stay}",
            quoted(audio_source)
        ),
        "--playback-props".into(),
        format!("target.object={} {stay}", quoted(output)),
    ]
}

/// `text` as a SPA JSON string.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The sinks PipeWire lists now.
pub fn playback_devices() -> Result<Vec<AudioDevice>, String> {
    let failed = |error: String| {
        format!("Could not list playback devices; is PipeWire installed and running? ({error})")
    };
    let dump = Command::new("pw-dump")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| failed(format!("pw-dump: {error}")))?;
    if !dump.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&dump.stderr).trim().to_owned(),
        ));
    }
    sinks(&String::from_utf8_lossy(&dump.stdout)).map_err(failed)
}

/// The playback devices (PipeWire's sinks) in `dump`, the JSON `pw-dump` prints: the id is the
/// sink's `node.name`, the description its `node.nick`, which an HDMI or DisplayPort sink takes
/// from the monitor's ELD, so it is the monitor's EDID name.
pub fn sinks(dump: &str) -> Result<Vec<AudioDevice>, String> {
    let objects: Vec<Value> = serde_json::from_str(dump).map_err(|error| error.to_string())?;
    let props = |object: &Value| object.pointer("/info/props").cloned().unwrap_or_default();
    let text = |props: &Value, key: &str| props[key].as_str().unwrap_or_default().to_owned();
    Ok(objects
        .iter()
        .map(props)
        .filter(|node| node["media.class"] == "Audio/Sink" && node["node.name"].is_string())
        .map(|node| {
            // The sink's sound card, whose bus path (`pci-0000:0c:00.1`) is its PCI address.
            let card = objects
                .iter()
                .find(|object| object["id"] == node["device.id"] && !node["device.id"].is_null())
                .map(props)
                .unwrap_or_default();
            let nick = text(&node, "node.nick");
            let bus_path = text(&card, "device.bus-path");
            let name = text(&node, "node.description");
            AudioDevice {
                id: text(&node, "node.name"),
                name: if name.is_empty() {
                    text(&node, "node.name")
                } else {
                    name
                },
                container_id: bus_path
                    .strip_prefix("pci-")
                    .map(|address| container_id(address, &nick))
                    .unwrap_or_default(),
                description: nick,
            }
        })
        .collect())
}

/// What ties a monitor to its HDMI or DisplayPort sink, as the container ID does on Windows: the
/// PCI slot of the graphics card (its sound controller is another function of the same slot, so
/// `0000:0c:00.0` and `0000:0c:00.1` share `0000:0c:00`) plus the monitor's EDID name, which the
/// sink carries from the ELD. Empty if either is unknown, or `pci_address` is a USB device's path.
pub fn container_id(pci_address: &str, name: &str) -> String {
    let name = name.trim();
    match pci_address.rsplit_once('.') {
        Some((slot, function))
            if !name.is_empty()
                && slot.split(':').count() == 3
                && function.len() == 1
                && function.chars().all(|c| c.is_ascii_digit()) =>
        {
            format!("{slot} {name}")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests;
