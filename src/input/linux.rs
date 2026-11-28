//! Command mode's and the input thread's Linux calls: the monitors from their DRM connectors in
//! sysfs, and DDC/CI over i2c-dev (docs/research/linux-support.md, section 7; docs/win32-ddc.md for
//! the protocol and its waits).

// unsafe is opt-in per module; see Cargo.toml [lints].
#![allow(
    unsafe_code,
    reason = "SPEC allows unsafe in input's platform modules, for the i2c-dev ioctl"
)]

use super::{Ddc, MonitorId, edid_name, edid_serial};
use crate::guard::Monitor;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// VCP code Input Select (MCCS).
const INPUT_SELECT: u8 = 0x60;
/// The DDC/CI address on a monitor's i2c bus, and the EDID's.
const DDC_CI: u8 = 0x37;
const EDID: u8 = 0x50;
/// linux/i2c-dev.h: sets the address the next read and write go to; FORCE even while a kernel
/// driver holds the address, which ddcutil 3.0.2 retries with after EBUSY (its `--force-slave-address`).
const I2C_SLAVE: libc::Ioctl = 0x0703;
const I2C_SLAVE_FORCE: libc::Ioctl = 0x0706;
/// What the status line and command mode say when cardcast can't use the i2c buses at all.
pub const NO_MODULE: &str = "Load the i2c-dev module: run sudo modprobe i2c-dev, and copy usr/lib/modules-load.d/cardcast-i2c.conf from cardcast's tarball to /etc/modules-load.d/ so it loads at every start.";
pub const NO_PERMISSION: &str = "No permission for the monitor's i2c bus: install cardcast's udev rule: copy usr/lib/udev/rules.d/60-cardcast-i2c.rules from cardcast's tarball to /etc/udev/rules.d/, then run sudo udevadm control --reload and sudo udevadm trigger, or restart.";
/// DDC/CI 1.1 section 5: retry at least 3 times on no answer.
const ATTEMPTS: usize = 4;
/// The longest capabilities string read; real ones are a few hundred bytes.
const LONGEST_CAPABILITIES: usize = 4096;

/// A monitor's DRM connector, to reach it over DDC/CI. Looked up again for every command.
pub struct Place {
    /// The connector in sysfs, for example `/sys/class/drm/card1-DP-2`.
    dir: PathBuf,
    edid: Vec<u8>,
}

/// Every connected monitor with an EDID, named by it as the settings window names them.
pub fn monitors() -> Result<Vec<(MonitorId, Place)>, String> {
    if let Some(problem) = access_problem() {
        return Err(problem.to_owned());
    }
    Ok(drm_connectors()
        .into_iter()
        .filter(|(_, _, edid)| !edid.is_empty())
        .map(|(_, dir, edid)| {
            let id = MonitorId {
                serial: edid_serial(&edid),
                name: edid_name(&edid).unwrap_or_else(|| connector_name(&dir).to_owned()),
            };
            (id, Place { dir, edid })
        })
        .collect())
}

/// The monitor's EDID serial, see [`edid_serial`].
pub fn serial(monitor: &Monitor) -> Option<String> {
    edid_serial(&place(monitor)?.edid)
}

/// Opens a monitor from the settings window's list over DDC/CI, as [`open`] does.
pub fn open_monitor(monitor: &Monitor) -> Option<Box<dyn Ddc>> {
    open(place(monitor)?)
}

/// Why cardcast can't reach any monitor over i2c-dev, from opening every graphics card's bus.
pub fn access_problem() -> Option<&'static str> {
    let mut buses: Vec<u32> = std::fs::read_dir("/sys/class/drm")
        .map(|entries| {
            entries
                .flatten()
                .flat_map(|entry| {
                    // A card's buses are on its device, a DisplayPort AUX bus on its connector.
                    let mut buses = buses_in(&entry.path().join("device"));
                    buses.extend(buses_in(&entry.path()));
                    buses
                })
                .collect()
        })
        .unwrap_or_default();
    buses.sort_unstable();
    buses.dedup();
    let opened: Vec<_> = buses
        .iter()
        .map(|bus| {
            File::options()
                .read(true)
                .write(true)
                .open(format!("/dev/i2c-{bus}"))
                .map(drop)
                .map_err(|error| error.kind())
        })
        .collect();
    i2c_problem(&opened)
}

/// The problem from opening each graphics card bus's `/dev/i2c-N`: none when one opens or there
/// are none, else missing permission (no udev rule) before missing nodes (no i2c-dev module).
pub fn i2c_problem(opened: &[Result<(), std::io::ErrorKind>]) -> Option<&'static str> {
    use std::io::ErrorKind::{NotFound, PermissionDenied};
    if opened.iter().any(Result::is_ok) {
        None
    } else if opened.contains(&Err(PermissionDenied)) {
        Some(NO_PERMISSION)
    } else if opened.contains(&Err(NotFound)) {
        Some(NO_MODULE)
    } else {
        None
    }
}

/// The connector of `monitor`, whose `connector` is `<card> <connector name>` (guard's Linux list).
fn place(monitor: &Monitor) -> Option<Place> {
    drm_connectors()
        .into_iter()
        .find(|(id, _, edid)| *id == monitor.connector && !edid.is_empty())
        .map(|(_, dir, edid)| Place { dir, edid })
}

/// The connected connectors under `/sys/class/drm`: the id guard's Linux monitor list gives
/// `Monitor::connector` (the card's PCI address, or its DRM name, and the connector name), the
/// connector's directory and its EDID.
fn drm_connectors() -> Vec<(String, PathBuf, Vec<u8>)> {
    let drm = Path::new("/sys/class/drm");
    let Ok(entries) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let (card, name) = file.split_once('-')?;
            let dir = drm.join(&file);
            let status = std::fs::read_to_string(dir.join("status")).ok()?;
            if status.trim() != "connected" {
                return None;
            }
            let uevent =
                std::fs::read_to_string(drm.join(card).join("device/uevent")).unwrap_or_default();
            let slot = uevent
                .lines()
                .find_map(|line| line.strip_prefix("PCI_SLOT_NAME="))
                .unwrap_or(card);
            let edid = std::fs::read(dir.join("edid")).unwrap_or_default();
            Some((format!("{slot} {name}"), dir, edid))
        })
        .collect()
}

/// `HDMI-A-2` for `/sys/class/drm/card0-HDMI-A-2`.
fn connector_name(dir: &Path) -> &str {
    let file = dir.file_name().and_then(|name| name.to_str()).unwrap_or("");
    file.split_once('-').map_or(file, |(_, name)| name)
}

/// Opens the monitor at `place` over DDC/CI; `None` when no i2c bus of its graphics card has its
/// EDID, or several do (two monitors with one EDID on one card).
pub fn open(place: Place) -> Option<Box<dyn Ddc>> {
    // The connector's own bus: a `ddc` link (most drivers) or an `i2c-N` child (DisplayPort AUX).
    let mut direct: Vec<u32> = std::fs::read_link(place.dir.join("ddc"))
        .ok()
        .and_then(|link| bus_number(link.file_name()?.to_str()?))
        .into_iter()
        .collect();
    direct.extend(buses_in(&place.dir));
    // Drivers that link neither (NVIDIA's, amdgpu's HDMI) keep their buses on the card's device (the connector's `device` is the DRM card).
    let card = place.dir.join("device/device");
    let bus = pick_bus(&place.edid, &direct, &buses_in(&card), read_edid)?;
    Some(Box::new(I2cMonitor(device(bus, DDC_CI)?)))
}

/// The bus numbers of the `i2c-N` entries in `dir`.
fn buses_in(dir: &Path) -> Vec<u32> {
    let mut buses: Vec<u32> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| bus_number(entry.file_name().to_str()?))
                .collect()
        })
        .unwrap_or_default();
    buses.sort_unstable();
    buses
}

fn bus_number(name: &str) -> Option<u32> {
    name.strip_prefix("i2c-")?.parse().ok()
}

/// The monitor's i2c bus, as ddcutil finds it: the connector's own bus (`direct`) whose EDID
/// (`read_edid`) is the connector's, else the one bus of its graphics card (`card`) that has it.
/// `None` when none has it, or several card buses do and the bus can't be told.
pub fn pick_bus(
    edid: &[u8],
    direct: &[u32],
    card: &[u32],
    read_edid: impl Fn(u32) -> Option<Vec<u8>>,
) -> Option<u32> {
    let base = edid.get(..128)?;
    let has_edid = |bus: &u32| read_edid(*bus).is_some_and(|read| read.get(..128) == Some(base));
    if let Some(bus) = direct.iter().find(|bus| has_edid(bus)) {
        return Some(*bus);
    }
    let mut fits = card
        .iter()
        .filter(|bus| !direct.contains(*bus) && has_edid(bus));
    match (fits.next(), fits.next()) {
        (Some(bus), None) => Some(*bus),
        _ => None,
    }
}

/// `/dev/i2c-<bus>` opened to talk to `address`.
fn device(bus: u32, address: u8) -> Option<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .open(format!("/dev/i2c-{bus}"))
        .ok()?;
    let set = |request| {
        // SAFETY: the fd is open for the call; both requests take the address as a plain integer.
        let set = unsafe { libc::ioctl(file.as_raw_fd(), request, libc::c_ulong::from(address)) };
        if set == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    match set(I2C_SLAVE) {
        Err(error) if error.raw_os_error() == Some(libc::EBUSY) => set(I2C_SLAVE_FORCE).ok()?,
        result => result.ok()?,
    }
    Some(file)
}

/// The EDID base block at address 0x50 of `bus`, as the monitor sends it.
fn read_edid(bus: u32) -> Option<Vec<u8>> {
    let mut file = device(bus, EDID)?;
    file.write_all(&[0]).ok()?;
    let mut edid = vec![0; 128];
    file.read_exact(&mut edid).ok()?;
    Some(edid)
}

/// A DDC/CI message from cardcast to the monitor as i2c-dev writes it: the source address 0x51,
/// the length, `data` and the checksum, which also covers the destination address 0x6E that the
/// kernel sends (DDC/CI 1.1 section 4).
pub fn request(data: &[u8]) -> Vec<u8> {
    let length = u8::try_from(data.len()).unwrap_or(0x7F) | 0x80;
    let mut message = vec![0x51, length];
    message.extend_from_slice(data);
    message.push(message.iter().fold(DDC_CI << 1, |sum, b| sum ^ b));
    message
}

/// The data of a monitor's reply as i2c-dev reads it (address 0x6E, length, data, checksum, then
/// whatever follows); `None` when the address, length or checksum is wrong, or it is the null
/// message a busy monitor sends. The checksum starts from the host's address 0x50.
pub fn reply(bytes: &[u8]) -> Option<&[u8]> {
    let [0x6E, length, rest @ ..] = bytes else {
        return None;
    };
    if length & 0x80 == 0 {
        return None;
    }
    let data = rest.get(..usize::from(length & 0x7F))?;
    let checksum = *rest.get(data.len())?;
    let sum = bytes[..2 + data.len()].iter().fold(EDID, |sum, b| sum ^ b);
    (!data.is_empty() && sum == checksum).then_some(data)
}

/// The low byte (SL) of VCP `code`'s current value from a Get VCP Feature reply's data.
pub fn current_value(data: &[u8], code: u8) -> Option<u8> {
    match data {
        [0x02, 0x00, reply_code, _, _, _, _, low] if *reply_code == code => Some(*low),
        _ => None,
    }
}

/// The text of a Capabilities reply's data for the fragment at `offset`; empty at the end.
pub fn capabilities_fragment(data: &[u8], offset: u16) -> Option<&[u8]> {
    let [0xE3, high, low, fragment @ ..] = data else {
        return None;
    };
    (u16::from_be_bytes([*high, *low]) == offset).then_some(fragment)
}

/// A monitor's i2c bus, open at the DDC/CI address.
struct I2cMonitor(File);

impl I2cMonitor {
    /// Sends `data`, waits `wait`, and reads a reply of up to `size` bytes that `answer` accepts;
    /// retried 40 ms apart when the monitor doesn't answer (DDC/CI 1.1 section 5).
    fn ask<T>(
        &mut self,
        data: &[u8],
        wait: Duration,
        size: usize,
        answer: impl Fn(&[u8]) -> Option<T>,
    ) -> Option<T> {
        let message = request(data);
        let mut buffer = vec![0; size];
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(40));
            }
            if self.0.write_all(&message).is_err() {
                continue;
            }
            std::thread::sleep(wait);
            if self.0.read_exact(&mut buffer).is_err() {
                continue;
            }
            if let Some(value) = reply(&buffer).and_then(&answer) {
                return Some(value);
            }
        }
        None
    }
}

impl Ddc for I2cMonitor {
    fn capabilities(&mut self) -> Option<String> {
        let mut text = Vec::new();
        loop {
            let offset = u16::try_from(text.len()).ok()?;
            // At most 32 bytes per fragment, 50 ms after each (DDC/CI 1.1 section 4.6).
            let fragment = self.ask(
                &[0xF3, offset.to_be_bytes()[0], offset.to_be_bytes()[1]],
                Duration::from_millis(50),
                40,
                |data| capabilities_fragment(data, offset).map(<[u8]>::to_vec),
            )?;
            if fragment.is_empty() {
                break;
            }
            text.extend(fragment);
            if text.len() > LONGEST_CAPABILITIES {
                return None;
            }
        }
        text.retain(|b| *b != 0);
        String::from_utf8(text).ok()
    }

    fn current_input(&mut self) -> Option<u8> {
        // 40 ms from a Get request to reading its reply (DDC/CI 1.1 section 4.3).
        self.ask(
            &[0x01, INPUT_SELECT],
            Duration::from_millis(40),
            12,
            |data| current_value(data, INPUT_SELECT),
        )
    }

    fn set_input(&mut self, value: u8) -> bool {
        let message = request(&[0x03, INPUT_SELECT, 0x00, value]);
        // A Set has no reply, so only a failed write is retried; the input thread waits 50 ms after it.
        (0..ATTEMPTS).any(|attempt| {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(40));
            }
            self.0.write_all(&message).is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes recorded with `ddcutil --trace i2c` from the sender's AW2523HF (DisplayPort, NVIDIA).
    const GET_INPUT: [u8; 5] = [0x51, 0x82, 0x01, 0x60, 0xDC];
    const INPUT_REPLY: [u8; 14] = [
        0x6E, 0x88, 0x02, 0x00, 0x60, 0x00, 0x12, 0x12, 0x0F, 0x11, 0xCA, 0x6E, 0x80, 0xBE,
    ];
    const FIRST_FRAGMENT: [u8; 32] = [
        0x6E, 0x9D, 0xE3, 0x00, 0x00, 0x28, 0x70, 0x72, 0x6F, 0x74, 0x28, 0x6D, 0x6F, 0x6E, 0x69,
        0x74, 0x6F, 0x72, 0x29, 0x74, 0x79, 0x70, 0x65, 0x28, 0x4C, 0x43, 0x44, 0x29, 0x6D, 0x6F,
        0x64, 0x28,
    ];
    const LAST_FRAGMENT: [u8; 6] = [0x6E, 0x83, 0xE3, 0x01, 0xAF, 0xF0];

    #[test]
    fn requests_carry_length_and_checksum() {
        assert_eq!(request(&[0x01, 0x60]), GET_INPUT);
        assert_eq!(request(&[0x01, 0x10]), [0x51, 0x82, 0x01, 0x10, 0xAC]);
        assert_eq!(
            request(&[0xF3, 0x00, 0x1A]),
            [0x51, 0x83, 0xF3, 0x00, 0x1A, 0x55]
        );
        assert_eq!(
            request(&[0xF3, 0x01, 0xAF]),
            [0x51, 0x83, 0xF3, 0x01, 0xAF, 0xE1]
        );
        // Set VCP 0x60 to HDMI 1, from DDC/CI 1.1's framing; never sent by the tests.
        assert_eq!(
            request(&[0x03, 0x60, 0x00, 0x11]),
            [0x51, 0x84, 0x03, 0x60, 0x00, 0x11, 0xC9]
        );
    }

    #[test]
    fn a_get_reply_gives_the_low_byte_of_the_current_input() {
        let data = reply(&INPUT_REPLY).unwrap();
        assert_eq!(data, &INPUT_REPLY[2..10]);
        assert_eq!(current_value(data, 0x60), Some(0x11));
        assert_eq!(current_value(data, 0x10), None, "another code's reply");
        let unsupported = [0x02, 0x01, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(current_value(&unsupported, 0x60), None);
    }

    #[test]
    fn capabilities_fragments_follow_their_offset() {
        let data = reply(&FIRST_FRAGMENT).unwrap();
        assert_eq!(
            capabilities_fragment(data, 0),
            Some(&b"(prot(monitor)type(LCD)mod"[..])
        );
        assert_eq!(capabilities_fragment(data, 26), None, "an old fragment");
        let last = reply(&LAST_FRAGMENT).unwrap();
        assert_eq!(capabilities_fragment(last, 0x01AF), Some(&[][..]));
        assert_eq!(capabilities_fragment(&INPUT_REPLY[2..10], 0), None);
    }

    #[test]
    fn broken_replies_are_refused() {
        let mut wrong_checksum = INPUT_REPLY;
        wrong_checksum[10] ^= 1;
        let mut wrong_value = INPUT_REPLY;
        wrong_value[9] = 0x0F;
        let mut no_length_bit = INPUT_REPLY;
        no_length_bit[1] = 0x08;
        let mut from_elsewhere = INPUT_REPLY;
        from_elsewhere[0] = 0x6F;
        for bytes in [
            &wrong_checksum[..],
            &wrong_value,
            &no_length_bit,
            &from_elsewhere,
            &INPUT_REPLY[..10],
            &[0x6E, 0x80, 0xBE],
            &[0xFF; 12],
            &[0; 12],
            &[],
        ] {
            assert_eq!(reply(bytes), None, "{bytes:02X?}");
        }
    }

    fn edid(tag: u8) -> Vec<u8> {
        let mut edid = vec![tag; 256];
        edid[200] = 0xEE;
        edid
    }

    #[test]
    fn the_bus_is_the_connectors_own_or_the_one_card_bus_with_its_edid() {
        let read = |bus: u32| match bus {
            1 | 4 => Some(edid(4)[..128].to_vec()),
            3 => Some(edid(3)),
            _ => None,
        };
        let monitor = edid(4);
        assert_eq!(pick_bus(&monitor, &[], &[0, 1, 2, 3], read), Some(1));
        assert_eq!(pick_bus(&monitor, &[4], &[0, 1, 2, 3], read), Some(4));
        assert_eq!(pick_bus(&monitor, &[2], &[0, 1, 3], read), Some(1));
        assert_eq!(
            pick_bus(&monitor, &[], &[0, 1, 3, 4], read),
            None,
            "two fit"
        );
        assert_eq!(pick_bus(&monitor, &[], &[0, 2, 3], read), None);
        assert_eq!(pick_bus(&[], &[], &[0, 1], read), None, "no EDID");
        assert_eq!(pick_bus(&edid(3), &[], &[3], read), Some(3));
    }

    #[test]
    fn a_bus_cardcast_may_not_open_is_said_plainly() {
        use std::io::ErrorKind::{NotFound, PermissionDenied};
        assert_eq!(i2c_problem(&[]), None, "no graphics card bus: no answer");
        assert_eq!(i2c_problem(&[Ok(()), Err(PermissionDenied)]), None);
        assert_eq!(
            i2c_problem(&[Err(NotFound), Err(NotFound)]),
            Some(NO_MODULE)
        );
        assert_eq!(
            i2c_problem(&[Err(PermissionDenied), Err(PermissionDenied)]),
            Some(NO_PERMISSION)
        );
        assert!(NO_MODULE.starts_with("Load the i2c-dev module: "));
        assert!(NO_PERMISSION.starts_with(
            "No permission for the monitor's i2c bus: install cardcast's udev rule: "
        ));
    }
}
