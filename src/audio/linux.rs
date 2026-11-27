//! The Linux audio relay (docs/research/linux-support.md, section 3): PipeWire's `pw-loopback`
//! records the audio source's monitor and plays it on the sound output, and `pw-dump` lists the
//! sinks. Both come with PipeWire (Arch: `pipewire-audio`; Debian and Ubuntu: `pipewire-bin`;
//! Fedora: `pipewire-utils`), and WirePlumber links the loopback.

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
/// How long `pw-dump` may take before PipeWire counts as not answering; it answers in well under
/// a second.
const DUMP_TIMEOUT: Duration = Duration::from_secs(3);
const NO_ANSWER: &str = "PipeWire is not answering. Is it still running?";
const NOT_LINKED: &str =
    "PipeWire did not connect the audio relay. Is WirePlumber installed and running?";
/// The first PipeWire with `target.object` (`PW_KEY_TARGET_OBJECT` in `src/pipewire/keys.h`, new
/// at tag 0.3.44): <https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/0.3.44/src/pipewire/keys.h>.
const MIN_PIPEWIRE: (u32, u32, u32) = (0, 3, 44);
/// WirePlumber links a stream to its `target.object` by node name from 0.4.9 (NEWS.rst: "policy-node
/// now supports the 'target.object' key"), and a `node.dont-reconnect` stream with a target never
/// falls back to the default sink (`src/scripts/policy-node.lua`); 0.5.0 documents
/// `node.dont-fallback` (`docs/rst/policies/linking.rst`). 0.4.12 is the first whose `wireplumber
/// --version` says which version runs (`src/main.c`), so older ones are refused as unknown:
/// <https://gitlab.freedesktop.org/pipewire/wireplumber/-/blob/master/NEWS.rst>.
const MIN_WIREPLUMBER: (u32, u32, u32) = (0, 4, 12);

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
    let dump = pw_dump()?;
    supported(&pipewire_version(&dump), &wireplumber_version())?;
    let devices = sinks(&dump).map_err(unreadable)?;
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
    let output = output.id.clone();
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
            if !loopback_linked(&pw_dump()?, loopback.0.id(), audio_source, &output) {
                return Err(NOT_LINKED.to_owned().into());
            }
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
        let mut command = Command::new("pw-loopback");
        command
            .args(loopback_args(audio_source, output))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        die_with(&mut command, std::process::id());
        command
            .spawn()
            .map(Self)
            .map_err(|error| spawn_failed(&error))
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

/// Ends `command`'s process when the thread that spawns it ends, so a loopback never outlives a
/// crashed cardcast; and does not start it when `parent` already died before that took hold.
fn die_with(command: &mut std::process::Command, parent: u32) {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl and getppid are async-signal-safe, and the closure touches no memory of the
    // parent and allocates nothing.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Reparented: the parent died between fork and prctl, so no signal will come.
            if u32::try_from(libc::getppid()) != Ok(parent) {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Why `pw-loopback` did not start, in plain words.
fn spawn_failed(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        "Could not start the audio relay: pw-loopback is not installed. Install PipeWire's tools (see README, Sound).".to_owned()
    } else {
        "Could not start the audio relay: pw-loopback did not run.".to_owned()
    }
}

/// Whether `dump` shows the loopback of process `pid` recording the `audio_source` sink and
/// playing on the `output` sink; without WirePlumber it runs but nothing links it.
fn loopback_linked(dump: &str, pid: u32, audio_source: &str, output: &str) -> bool {
    let Ok(objects) = serde_json::from_str::<Vec<Value>>(dump) else {
        return false;
    };
    let of_type = |kind: &'static str| {
        objects
            .iter()
            .filter(move |object| object["type"] == format!("PipeWire:Interface:{kind}"))
    };
    let props = |object: &Value| object.pointer("/info/props").cloned().unwrap_or_default();
    let clients: Vec<&Value> = of_type("Client")
        .filter(|client| props(client)["application.process.id"] == pid)
        .map(|client| &client["id"])
        .collect();
    let nodes: Vec<(Value, Value, bool)> = of_type("Node")
        .map(|node| {
            let props = props(node);
            let ours = clients.contains(&&props["client.id"]);
            (node["id"].clone(), props["node.name"].clone(), ours)
        })
        .collect();
    let sink = |name: &str| nodes.iter().find(|node| node.1 == name && !node.2);
    let (Some(audio_source), Some(output)) = (sink(audio_source), sink(output)) else {
        return false;
    };
    let ours = |id: &Value| nodes.iter().any(|node| &node.0 == id && node.2);
    let links: Vec<(&Value, &Value)> = of_type("Link")
        .map(|link| {
            (
                &link["info"]["output-node-id"],
                &link["info"]["input-node-id"],
            )
        })
        .collect();
    links
        .iter()
        .any(|(from, to)| *from == &audio_source.0 && ours(to))
        && links
            .iter()
            .any(|(from, to)| ours(from) && *to == &output.0)
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
    sinks(&pw_dump()?).map_err(unreadable)
}

fn unreadable(_error: String) -> String {
    "Could not read PipeWire's list of playback devices.".to_owned()
}

/// What `pw-dump` prints, ended after [`DUMP_TIMEOUT`] so a hung PipeWire can't hang cardcast.
fn pw_dump() -> Result<String, String> {
    dump_through("timeout")
}

/// [`pw_dump`] through the `timeout` command.
fn dump_through(timeout: &str) -> Result<String, String> {
    let dump = Command::new(timeout)
        .args([
            "--kill-after=1",
            &format!("{}", DUMP_TIMEOUT.as_secs()),
            "pw-dump",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match dump.map(|dump| (dump.status.code(), dump.stdout)) {
        Ok((Some(0), dump)) => Ok(String::from_utf8_lossy(&dump).into_owned()),
        // timeout's codes: 124 timed out, 137 killed after it, 127 no pw-dump.
        Ok((Some(124 | 137), _)) => Err(NO_ANSWER.to_owned()),
        Ok((Some(127), _)) => Err("Could not list playback devices: pw-dump is not installed. Install PipeWire's tools (see README, Sound).".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(
            "Could not list playback devices: the timeout command (coreutils) is not installed."
                .to_owned(),
        ),
        _ => Err("Could not list playback devices. Is PipeWire running?".to_owned()),
    }
}

/// The version of the PipeWire daemon in `dump`, empty if it is not there.
fn pipewire_version(dump: &str) -> String {
    serde_json::from_str::<Vec<Value>>(dump)
        .unwrap_or_default()
        .iter()
        .find(|object| object["type"] == "PipeWire:Interface:Core")
        .and_then(|core| core.pointer("/info/version")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What `wireplumber --version` prints, empty if it does not run.
fn wireplumber_version() -> String {
    Command::new("wireplumber")
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

/// Whether this PipeWire (its version) and WirePlumber (what `wireplumber --version` prints)
/// keep each loopback stream on its own sink. Below [`MIN_PIPEWIRE`] and [`MIN_WIREPLUMBER`] a
/// stream may fall back to the default sink and play the audio source back into itself.
fn supported(pipewire: &str, wireplumber: &str) -> Result<(), String> {
    let at_least = |version: &str, (major, minor, micro): (u32, u32, u32)| {
        let mut parts = version.split('.').map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .ok()
        });
        match (parts.next(), parts.next(), parts.next()) {
            (Some(Some(a)), Some(Some(b)), Some(Some(c))) => (a, b, c) >= (major, minor, micro),
            _ => false,
        }
    };
    let wanted = |(major, minor, micro): (u32, u32, u32)| format!("{major}.{minor}.{micro}");
    if !at_least(pipewire, MIN_PIPEWIRE) {
        return Err(format!(
            "The audio relay needs PipeWire {} or newer; this is PipeWire {pipewire}.",
            wanted(MIN_PIPEWIRE)
        ));
    }
    let Some(wireplumber) = wireplumber
        .lines()
        .find_map(|line| line.strip_prefix("Linked with libwireplumber "))
        .map(str::trim)
    else {
        return Err(format!(
            "The audio relay needs WirePlumber {} or newer; is WirePlumber installed?",
            wanted(MIN_WIREPLUMBER)
        ));
    };
    if !at_least(wireplumber, MIN_WIREPLUMBER) {
        return Err(format!(
            "The audio relay needs WirePlumber {} or newer; this is WirePlumber {wireplumber}.",
            wanted(MIN_WIREPLUMBER)
        ));
    }
    Ok(())
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
