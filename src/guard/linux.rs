//! The Linux monitor list (docs/research/linux-support.md, section 4): the DRM connectors in sysfs
//! give each monitor's EDID and graphics card; the desktop (Wayland outputs, or RandR on X11) gives
//! its name, position, size and refresh rate. The display watcher follows the desktop's outputs.

mod mutter;
mod wayland;
mod x11;

use super::Monitor;
use super::desktop::{self, Desktop};
use crate::input::{edid_name, edid_product, edid_serial};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// A DRM connector with a monitor plugged in, from `/sys/class/drm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    /// The kernel's connector name, for example `HDMI-A-2`; two graphics cards can both have a `DP-1`.
    pub name: String,
    pub edid: Vec<u8>,
    /// The graphics card's PCI address (`0000:0c:00.0`), or its DRM name (`card0`) when it has none.
    pub card: String,
    /// The graphics card's name from pci.ids, else its kernel driver; empty if unknown.
    pub graphics_card: String,
}

/// An output as the desktop shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// The wl_output or RandR output name; on Plasma, GNOME, sway and Xorg's modesetting driver the
    /// kernel's connector name.
    pub name: String,
    /// What the desktop calls the monitor (the wl_output description); may be empty.
    pub description: String,
    /// The EDID, if the desktop gives it (RandR does).
    pub edid: Option<Vec<u8>>,
    /// The top-left corner on the desktop, in the desktop's coordinates (logical pixels on Wayland).
    pub left: i32,
    pub top: i32,
    /// The resolution in real pixels, after the output's rotation.
    pub width: u32,
    pub height: u32,
    /// The size in the desktop's coordinates; see [`Monitor::layout_size`].
    pub layout_size: (u32, u32),
    pub refresh_hz: u32,
}

/// Active monitors: the desktop's outputs, matched to their DRM connectors.
pub fn monitors() -> Result<Vec<Monitor>, String> {
    // The primary output from the desktop's own source: RandR on X11, Mutter on GNOME, the output
    // order on Plasma; other desktops have none, so the monitor at the origin is primary.
    let (outputs, primary) = match desktop::detect(|name| std::env::var(name).ok()) {
        Some(Desktop::X11) => x11::outputs().map(|outputs| (outputs, x11::primary())),
        Some(Desktop::GnomeWayland) => {
            wayland::outputs().map(|(outputs, _)| (outputs, mutter::primary()))
        }
        Some(_) => wayland::outputs(),
        None => Err("no desktop session".into()),
    }
    .map_err(|error| format!("Could not list monitors. ({error})"))?;
    let pci_ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let connectors = connectors(Path::new("/sys/class/drm"), &pci_ids);
    Ok(match_monitors(outputs, &connectors, primary.as_deref()))
}

/// The connected connectors under `drm` (`/sys/class/drm`), named with `pci_ids` (pci.ids' text).
pub fn connectors(drm: &Path, pci_ids: &str) -> Vec<Connector> {
    let Ok(entries) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    let mut connectors: Vec<Connector> = entries
        .flatten()
        .filter_map(|entry| {
            // `card0-HDMI-A-2` is connector `HDMI-A-2` of graphics card `card0`.
            let file = entry.file_name().into_string().ok()?;
            let (card, name) = file.split_once('-')?;
            let status = std::fs::read_to_string(drm.join(&file).join("status")).ok()?;
            if status.trim() != "connected" {
                return None;
            }
            let uevent =
                std::fs::read_to_string(drm.join(card).join("device/uevent")).unwrap_or_default();
            let value = |key: &str| {
                uevent
                    .lines()
                    .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                    .unwrap_or_default()
            };
            let graphics_card = value("PCI_ID")
                .split_once(':')
                .and_then(|(vendor, device)| pci_name(pci_ids, vendor, device))
                .unwrap_or(value("DRIVER"));
            let slot = value("PCI_SLOT_NAME");
            Some(Connector {
                name: name.to_owned(),
                edid: std::fs::read(drm.join(&file).join("edid")).unwrap_or_default(),
                card: if slot.is_empty() { card } else { slot }.to_owned(),
                graphics_card: graphics_card.to_owned(),
            })
        })
        .collect();
    connectors.sort_by(|a, b| (&a.card, &a.name).cmp(&(&b.card, &b.name)));
    connectors
}

/// The device's name in pci.ids' text, for example `AD103 [GeForce RTX 4080]` for `10DE`, `2704`.
fn pci_name<'a>(pci_ids: &'a str, vendor: &str, device: &str) -> Option<&'a str> {
    let (vendor, device) = (vendor.to_ascii_lowercase(), device.to_ascii_lowercase());
    // A vendor line is `<vendor>  <name>`; its devices follow as `\t<device>  <name>`, their
    // subsystems as `\t\t...`.
    let mut lines = pci_ids
        .lines()
        .skip_while(|line| line.split_once("  ").map(|(id, _)| id) != Some(vendor.as_str()));
    lines.next()?;
    lines
        .take_while(|line| line.starts_with(['\t', '#']))
        .find_map(|line| {
            line.strip_prefix('\t')?
                .strip_prefix(&device)?
                .strip_prefix("  ")
        })
}

/// Each output as a monitor, with the EDID and graphics card of its connector. The saved identity
/// (`device_path`) is the EDID's product and serial, else the connector; Linux keeps its own
/// settings file, so it need not match Windows' device path. The primary monitor is the output
/// named `primary` (the desktop's), else the one at the desktop's origin, where Windows puts it.
pub fn match_monitors(
    outputs: Vec<Output>,
    connectors: &[Connector],
    primary: Option<&str>,
) -> Vec<Monitor> {
    let primary = primary.filter(|name| outputs.iter().any(|output| output.name == *name));
    let mut monitors: Vec<Monitor> = outputs
        .into_iter()
        .map(|output| {
            let connector = connector_of(&output, connectors);
            let edid = connector
                .map(|connector| connector.edid.as_slice())
                .or(output.edid.as_deref())
                .unwrap_or_default();
            let connector_id = connector.map_or_else(String::new, |connector| {
                format!("{} {}", connector.card, connector.name)
            });
            let identity = edid_product(edid)
                .zip(edid_serial(edid))
                .map(|(product, serial)| format!("{product} {serial}"));
            let or_output_name = |text: String| {
                if text.is_empty() {
                    output.name.clone()
                } else {
                    text
                }
            };
            Monitor {
                device_path: identity.unwrap_or_else(|| or_output_name(connector_id.clone())),
                name: edid_name(edid).unwrap_or_else(|| or_output_name(output.description.clone())),
                width: output.width,
                height: output.height,
                layout_size: output.layout_size,
                refresh_hz: output.refresh_hz,
                primary: primary.map_or((output.left, output.top) == (0, 0), |name| {
                    name == output.name
                }),
                left: output.left,
                top: output.top,
                graphics_card: connector
                    .map(|connector| connector.graphics_card.clone())
                    .unwrap_or_default(),
                container_id: String::new(),
                connector: connector_id,
                gdi_name: output.name,
            }
        })
        .collect();
    // Some makers give every monitor of a model the same serial; the connector tells them apart.
    let paths: Vec<String> = monitors.iter().map(|m| m.device_path.clone()).collect();
    for monitor in &mut monitors {
        if paths
            .iter()
            .filter(|path| **path == monitor.device_path)
            .count()
            > 1
        {
            let place = if monitor.connector.is_empty() {
                &monitor.gdi_name
            } else {
                &monitor.connector
            };
            monitor.device_path = format!("{} {place}", monitor.device_path);
        }
    }
    monitors
}

/// The connector showing `output`: the one with its name and its monitor (the same EDID, or an EDID
/// name in the desktop's description), else the only one with either.
fn connector_of<'a>(output: &Output, connectors: &'a [Connector]) -> Option<&'a Connector> {
    let score = |connector: &Connector| {
        let same_monitor = match &output.edid {
            Some(edid) => *edid == connector.edid,
            None => {
                edid_name(&connector.edid).is_some_and(|name| output.description.contains(&name))
            }
        };
        2 * u8::from(connector.name == output.name) + u8::from(same_monitor)
    };
    let best = connectors
        .iter()
        .map(score)
        .max()
        .filter(|best| *best > 0)?;
    let mut found = connectors
        .iter()
        .filter(|connector| score(connector) == best);
    match (found.next(), found.next()) {
        (Some(connector), None) => Some(connector),
        _ => None,
    }
}

/// Calls its `wake` after every change of the desktop's outputs: one plugged in, unplugged, moved
/// or changing mode.
pub struct DisplayWatcher {
    stopped: Arc<AtomicBool>,
}

impl DisplayWatcher {
    /// Errors are plain words for the status line.
    pub fn new(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let stopped = Arc::new(AtomicBool::new(false));
        let still_watching = {
            let stopped = stopped.clone();
            move || {
                let watching = !stopped.load(Ordering::Relaxed);
                if watching {
                    wake();
                }
                watching
            }
        };
        let (started, started_result) = mpsc::channel();
        // shortcut: the thread blocks reading the desktop's socket, so a dropped watcher's thread
        // ends at the next display change, not at once; fine while one watcher lives as long as cardcast.
        std::thread::spawn(
            move || match desktop::detect(|name| std::env::var(name).ok()) {
                Some(Desktop::X11) => x11::watch(&started, still_watching),
                Some(_) => wayland::watch(&started, still_watching),
                None => {
                    let _ = started.send(Err("no desktop session".into()));
                }
            },
        );
        match started_result.recv() {
            Ok(Ok(())) => Ok(Self { stopped }),
            Ok(Err(error)) => Err(format!(
                "cardcast can't watch for display changes ({error}), so it won't follow the card display being unplugged or plugged back in. Restart cardcast."
            )),
            Err(_) => Err("cardcast can't watch for display changes, so it won't follow the card display being unplugged or plugged back in. Restart cardcast.".into()),
        }
    }
}

impl Drop for DisplayWatcher {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
