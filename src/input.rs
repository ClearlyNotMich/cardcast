//! Input switch decisions and command mode (SPEC section 11). Platform calls only in `ddc` (Windows) and `linux`, so both share the rest.

#[cfg(windows)]
mod ddc;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as ddc;

use crate::guard::Monitor;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// The program's name in commands.
const EXE: &str = if cfg!(windows) {
    "cardcast.exe"
} else {
    "cardcast"
};

/// A monitor's DDC/CI connection. Every call blocks, the capabilities string for up to seconds (docs/win32-ddc.md).
pub trait Ddc {
    /// The capabilities string, or `None` when the monitor doesn't answer.
    fn capabilities(&mut self) -> Option<String>;
    /// The low byte of VCP 0x60, or `None` when the monitor doesn't answer.
    fn current_input(&mut self) -> Option<u8>;
    /// Sets VCP 0x60; false when the monitor doesn't answer.
    fn set_input(&mut self, value: u8) -> bool;
}

/// Command mode: does the switch `args` (the arguments after the program name) asks for, writes one line to standard error, and returns the exit code.
pub fn command(args: &[String]) -> i32 {
    let (code, line) = run_command(args, ddc::monitors, ddc::open);
    // Fails silently when the caller didn't capture standard error, as a GUI program has none.
    let _ = writeln!(std::io::stderr(), "{line}");
    code
}

/// Command mode with the platform's calls passed in: `monitors` lists each monitor with what `open` needs to reach it over DDC/CI. Returns the exit code (0 switched or already there, 1 failed, 2 not understood) and the line for standard error.
pub fn run_command<M>(
    args: &[String],
    monitors: impl FnOnce() -> Result<Vec<(MonitorId, M)>, String>,
    open: impl FnOnce(M) -> Option<Box<dyn Ddc>>,
) -> (i32, String) {
    let request = match parse_input_command(args) {
        Ok(request) => request,
        Err(line) => return (2, line),
    };
    let (ids, handles): (Vec<_>, Vec<_>) = match monitors() {
        Ok(monitors) => monitors.into_iter().unzip(),
        Err(line) => return (1, line),
    };
    let wanted = &request.monitor;
    let (index, handle) = match select_monitor(&ids, wanted) {
        Ok(index) => match handles.into_iter().nth(index) {
            Some(handle) => (index, handle),
            None => return (1, format!("No monitor has the serial or name {wanted:?}.")),
        },
        Err(SelectError::NoSuchMonitor) => {
            return (1, format!("No monitor has the serial or name {wanted:?}."));
        }
        Err(SelectError::SeveralFit) => {
            return (
                1,
                format!(
                    "More than one monitor has the serial or name {wanted:?}; use one only this monitor has."
                ),
            );
        }
    };
    let name = &ids[index].name;
    let no_answer = |to: &str| {
        (
            1,
            format!("Could not switch {name}{to}: the monitor did not answer over DDC/CI."),
        )
    };
    let to = match request.target {
        InputTarget::Input(value) => format!(" to {}", input_name(value)),
        InputTarget::Toggle(..) => String::new(),
    };
    let Some(mut ddc) = open(handle) else {
        return no_answer(&to);
    };
    let Some(capabilities) = ddc.capabilities() else {
        return no_answer(&to);
    };
    let Some(inputs) = input_list(&capabilities) else {
        return (
            1,
            format!("{name} has no input switch: it lists fewer than two inputs over DDC/CI."),
        );
    };
    let Some(current) = ddc.current_input() else {
        return no_answer(&to);
    };
    match decide_switch(request.target, &inputs, current) {
        Err(NotListed(value)) => {
            let listed: Vec<String> = inputs.iter().map(|value| input_name(*value)).collect();
            (
                1,
                format!(
                    "{name} does not list {}; it lists {}.",
                    input_name(value),
                    listed.join(", ")
                ),
            )
        }
        Ok(Switch::AlreadyOn(value)) => (0, format!("{name} is already on {}.", input_name(value))),
        Ok(Switch::SwitchTo(value)) if ddc.set_input(value) => {
            (0, format!("Switched {name} to {}.", input_name(value)))
        }
        Ok(Switch::SwitchTo(value)) => no_answer(&format!(" to {}", input_name(value))),
    }
}

/// The input list: the values in `60(...)` of the capabilities string's `vcp(...)`, or `None` when it can't be read or has fewer than two.
pub fn input_list(capabilities: &str) -> Option<Vec<u8>> {
    let capabilities = capabilities.to_ascii_lowercase();
    let mut rest = &capabilities[capabilities.find("vcp(")? + 4..];
    loop {
        rest = rest.trim_start();
        if rest.starts_with(')') {
            return None;
        }
        let code = hex_bytes(rest.get(..2)?)?;
        rest = rest[2..].trim_start();
        let Some(group) = rest.strip_prefix('(') else {
            continue;
        };
        let end = group.find(')')?;
        if code == [0x60] {
            let mut inputs = Vec::new();
            for value in hex_bytes(&group[..end])? {
                if !inputs.contains(&value) {
                    inputs.push(value);
                }
            }
            return (inputs.len() >= 2).then_some(inputs);
        }
        rest = &group[end + 1..];
    }
}

/// The input value for a command word: `dp`, `dp2`, `hdmi1`, `hdmi2`, or `0x01` to `0xFF`, ignoring case.
pub fn input_value(word: &str) -> Option<u8> {
    let word = word.to_ascii_lowercase();
    match word.as_str() {
        "dp" => Some(0x0F),
        "dp2" => Some(0x10),
        "hdmi1" => Some(0x11),
        "hdmi2" => Some(0x12),
        _ => match hex_bytes(word.strip_prefix("0x")?)?.as_slice() {
            [value] if word.len() == 4 && *value != 0 => Some(*value),
            _ => None,
        },
    }
}

/// The name cardcast shows for an input value, for example "HDMI 1" or "Input 0x1B".
pub fn input_name(value: u8) -> String {
    match value {
        0x0F => "DisplayPort 1".to_owned(),
        0x10 => "DisplayPort 2".to_owned(),
        0x11 => "HDMI 1".to_owned(),
        0x12 => "HDMI 2".to_owned(),
        _ => format!("Input 0x{value:02X}"),
    }
}

/// The monitor's serial from its EDID: the serial descriptor's text, else the 32-bit serial number in decimal; `None` when it has neither or the EDID is broken.
pub fn edid_serial(edid: &[u8]) -> Option<String> {
    let block = edid_block(edid)?;
    if let Some(text) = descriptor_text(block, 0xFF)? {
        return Some(text);
    }
    let number = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
    (number != 0).then(|| number.to_string())
}

/// The monitor's name from its EDID name descriptor, for example `DELL AW2523HF`; `None` when it has none or the EDID is broken.
#[cfg_attr(
    windows,
    allow(
        dead_code,
        reason = "only the Linux monitor list reads it; Windows has its own"
    )
)]
pub fn edid_name(edid: &[u8]) -> Option<String> {
    descriptor_text(edid_block(edid)?, 0xFC)?
}

/// The EDID's maker and product code as Windows writes them in a monitor's device path, for example `DEL42B9`.
#[cfg_attr(
    windows,
    allow(
        dead_code,
        reason = "only the Linux monitor list reads it; Windows has its own"
    )
)]
pub fn edid_product(edid: &[u8]) -> Option<String> {
    let block = edid_block(edid)?;
    let maker = u16::from_be_bytes([block[8], block[9]]);
    // Three letters of five bits each, 1 is 'A'.
    let letter = |shift: u16| char::from(b'@' + ((maker >> shift) & 0x1F) as u8);
    let product = u16::from_le_bytes([block[10], block[11]]);
    Some(format!(
        "{}{}{}{product:04X}",
        letter(10),
        letter(5),
        letter(0)
    ))
}

/// The EDID's 128-byte base block, or `None` when its header or checksum is wrong.
fn edid_block(edid: &[u8]) -> Option<&[u8]> {
    let block = edid.get(..128)?;
    let checksum = block.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    (block[..8] == [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] && checksum == 0)
        .then_some(block)
}

/// The first non-blank text of the base block's display descriptors with `tag`: `Some(None)` when
/// there is none, `None` when it holds a control character (a broken EDID).
fn descriptor_text(block: &[u8], tag: u8) -> Option<Option<String>> {
    // Four 18-byte descriptors; a display descriptor starts 00 00 00 <tag>.
    for descriptor in block[54..126].chunks(18) {
        if descriptor[..4] == [0x00, 0x00, 0x00, tag] {
            let text = descriptor[5..].split(|b| *b == b'\n').next()?;
            if !text.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                return None;
            }
            let text = std::str::from_utf8(text).ok()?.trim();
            if !text.is_empty() {
                return Some(Some(text.to_owned()));
            }
        }
    }
    Some(None)
}

/// What a command's `<monitor>` is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorId {
    /// The EDID serial, see [`edid_serial`].
    pub serial: Option<String>,
    /// The name cardcast shows.
    pub name: String,
}

/// Why no monitor was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectError {
    NoSuchMonitor,
    /// More than one monitor fits; the user must give the serial.
    SeveralFit,
}

/// The index of the one monitor whose serial or name is `wanted`, ignoring case.
pub fn select_monitor(monitors: &[MonitorId], wanted: &str) -> Result<usize, SelectError> {
    let wanted = wanted.to_lowercase();
    let mut fits = monitors.iter().enumerate().filter(|(_, monitor)| {
        monitor.name.to_lowercase() == wanted
            || monitor
                .serial
                .as_ref()
                .is_some_and(|serial| serial.to_lowercase() == wanted)
    });
    match (fits.next(), fits.next()) {
        (Some((index, _)), None) => Ok(index),
        (None, _) => Err(SelectError::NoSuchMonitor),
        (Some(_), Some(_)) => Err(SelectError::SeveralFit),
    }
}

/// What the Copy command puts as `<monitor>` for monitor `index`: its serial if that selects only it, else its name if that does; `None` when neither does.
pub fn command_monitor(monitors: &[MonitorId], index: usize) -> Option<&str> {
    let monitor = monitors.get(index)?;
    [monitor.serial.as_deref(), Some(monitor.name.as_str())]
        .into_iter()
        .flatten()
        .find(|wanted| select_monitor(monitors, wanted) == Ok(index))
}

/// The input a command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    Input(u8),
    /// `toggle a b`: to `b` when the monitor is on `a`, otherwise to `a`.
    Toggle(u8, u8),
}

/// A parsed `input <monitor> …` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequest {
    /// A serial or a name, for [`select_monitor`].
    pub monitor: String,
    pub target: InputTarget,
}

/// Parses the arguments after the program name; `Err` is the line for standard error, and command mode exits with code 2.
pub fn parse_input_command(args: &[String]) -> Result<InputRequest, String> {
    let not_understood = |what: &str| {
        format!(
            "Not understood: {what}. Use: {EXE} input <monitor> <input>, or {EXE} input <monitor> toggle <input> <input>, where <input> is dp, dp2, hdmi1, hdmi2 or 0x01 to 0xFF."
        )
    };
    let value = |word: &String| {
        input_value(word).ok_or_else(|| not_understood(&format!("{word:?} is not an input")))
    };
    let (monitor, target) = match args {
        [command, ..] if !command.eq_ignore_ascii_case("input") => {
            return Err(not_understood(&format!("{command:?} is not a command")));
        }
        [_, monitor, ..] if monitor.is_empty() => return Err(not_understood("no monitor")),
        [_, _, toggle] | [_, _, toggle, _] if toggle.eq_ignore_ascii_case("toggle") => {
            return Err(not_understood("toggle needs two inputs"));
        }
        [_, monitor, input] => (monitor, InputTarget::Input(value(input)?)),
        [_, monitor, toggle, a, b] if toggle.eq_ignore_ascii_case("toggle") => {
            let (a, b) = (value(a)?, value(b)?);
            if a == b {
                return Err(not_understood("toggle needs two different inputs"));
            }
            (monitor, InputTarget::Toggle(a, b))
        }
        _ => return Err(not_understood("wrong number of words")),
    };
    Ok(InputRequest {
        monitor: monitor.clone(),
        target,
    })
}

/// What to do with a monitor on input `current`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    AlreadyOn(u8),
    SwitchTo(u8),
}

/// A requested input that the monitor's input list doesn't have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotListed(pub u8);

/// Decides the switch; every input the command names must be in the input list. Compare only the low byte of VCP 0x60 as `current`.
pub fn decide_switch(
    target: InputTarget,
    input_list: &[u8],
    current: u8,
) -> Result<Switch, NotListed> {
    let (wanted, named) = match target {
        InputTarget::Input(value) => (value, [value, value]),
        InputTarget::Toggle(a, b) => (if current == a { b } else { a }, [a, b]),
    };
    if let Some(missing) = named.into_iter().find(|value| !input_list.contains(value)) {
        return Err(NotListed(missing));
    }
    Ok(if current == wanted {
        Switch::AlreadyOn(wanted)
    } else {
        Switch::SwitchTo(wanted)
    })
}

/// The input thread for the settings window; `wake` asks it to repaint when a report is waiting.
pub fn new_input_thread(wake: impl Fn() + Send + 'static) -> Result<InputThread, String> {
    #[cfg(target_os = "linux")]
    if let Some(problem) = linux::access_problem() {
        return Err(problem.to_owned());
    }
    InputThread::spawn(ddc::serial, ddc::open_monitor, wake)
}

/// The settings window's DDC/CI calls, one at a time on their own thread, never the settings
/// window's, guard's or mirror's. The window keeps one for its lifetime, so two threads never talk
/// to one monitor; switching input switching off cancels its jobs instead. Dropped, it ends after
/// the monitor it is reading.
pub struct InputThread {
    /// Each job goes with the flag that cancels it.
    jobs: mpsc::Sender<(Arc<AtomicBool>, Job)>,
    reports: mpsc::Receiver<Report>,
    /// Cancels the jobs sent since the last [`Self::cancel`].
    cancelled: Arc<AtomicBool>,
}

impl InputThread {
    /// Starts the thread with the platform's calls, as [`run_job`] takes them.
    pub fn spawn(
        serial: impl Fn(&Monitor) -> Option<String> + Send + 'static,
        open: impl Fn(&Monitor) -> Option<Box<dyn Ddc>> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let (jobs, waiting) = mpsc::channel::<(Arc<AtomicBool>, Job)>();
        let (reports_to, reports) = mpsc::channel();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || {
                // The input lists read since the last cancel, so a monitor's are read once.
                let mut known = Vec::new();
                let mut since: Option<Arc<AtomicBool>> = None;
                while let Ok(first) = waiting.recv() {
                    let jobs: Vec<_> = std::iter::once(first).chain(waiting.try_iter()).collect();
                    for (cancelled, job) in newest_probe_only(jobs) {
                        if cancelled.load(Ordering::Relaxed) {
                            continue;
                        }
                        if !since
                            .as_ref()
                            .is_some_and(|since| Arc::ptr_eq(since, &cancelled))
                        {
                            known.clear();
                            since = Some(Arc::clone(&cancelled));
                        }
                        let switches = matches!(job, Job::Switch(..));
                        if let Some(report) = run_job(job, &known, &serial, &open, &cancelled) {
                            if let Report::Probed(probes) = &report {
                                known.clone_from(probes);
                            }
                            if reports_to.send(report).is_err() {
                                return;
                            }
                            wake();
                        }
                        // DDC/CI 1.1: 50 ms after a Set before the next call (docs/win32-ddc.md).
                        if switches {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .map_err(|error| format!("Could not start input switching: {error}"))?;
        Ok(Self {
            jobs,
            reports,
            cancelled: Arc::default(),
        })
    }

    pub fn send(&self, job: Job) {
        // Fails only once the thread ended, which it does only when dropped.
        let _ = self.jobs.send((Arc::clone(&self.cancelled), job));
    }

    pub fn take_report(&self) -> Option<Report> {
        self.reports.try_recv().ok()
    }

    /// Input switching was switched off: the jobs sent so far are dropped, a probe stops after the
    /// monitor it is reading, its reports are dropped, and the next probe reads every monitor again.
    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.cancelled = Arc::default();
        while self.reports.try_recv().is_ok() {}
    }
}

impl Drop for InputThread {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// The jobs waiting for the input thread, with only the newest probe kept: each reads every
/// monitor, so the older ones are out of date.
pub fn newest_probe_only<T>(jobs: Vec<(T, Job)>) -> Vec<(T, Job)> {
    let newest = jobs
        .iter()
        .rposition(|(_, job)| matches!(job, Job::Probe(_)));
    jobs.into_iter()
        .enumerate()
        .filter(|(index, (_, job))| !matches!(job, Job::Probe(_)) || Some(*index) == newest)
        .map(|(_, job)| job)
        .collect()
}

/// What the settings window asks of the input thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Read every monitor's serial and input list, one monitor at a time.
    Probe(Vec<Monitor>),
    /// Read the monitor's current input, as its input menu opens.
    Read(Monitor),
    /// Switch the monitor to this input.
    Switch(Monitor, u8),
}

/// A monitor's serial and input list, as the input thread read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub device_path: String,
    pub id: MonitorId,
    /// `None`: the monitor has no input switch.
    pub inputs: Option<Vec<u8>>,
}

/// What the input thread tells the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Probed(Vec<Probe>),
    /// The monitor's current input; `None` when it didn't answer.
    Current {
        device_path: String,
        input: Option<u8>,
    },
    /// The monitor (its device path) took the input, or the status line's text when it didn't answer.
    Switched(Result<(String, u8), String>),
}

/// Does `job` with the platform's calls passed in: `serial` reads a monitor's EDID serial and `open` reaches it over DDC/CI. A probe takes a monitor in `known` (read before) from there, and reads only the others; one that `stop` ends before its last monitor reports nothing.
pub fn run_job(
    job: Job,
    known: &[Probe],
    serial: &dyn Fn(&Monitor) -> Option<String>,
    open: &dyn Fn(&Monitor) -> Option<Box<dyn Ddc>>,
    stop: &AtomicBool,
) -> Option<Report> {
    Some(match job {
        Job::Probe(monitors) => {
            let mut probes = Vec::new();
            for monitor in monitors {
                if let Some(probe) = known
                    .iter()
                    .find(|probe| probe.device_path == monitor.device_path)
                {
                    probes.push(probe.clone());
                    continue;
                }
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                let inputs = open(&monitor)
                    .and_then(|mut ddc| ddc.capabilities())
                    .and_then(|capabilities| input_list(&capabilities));
                probes.push(Probe {
                    id: MonitorId {
                        serial: serial(&monitor),
                        name: monitor.name,
                    },
                    device_path: monitor.device_path,
                    inputs,
                });
            }
            Report::Probed(probes)
        }
        Job::Read(monitor) => Report::Current {
            input: open(&monitor).and_then(|mut ddc| ddc.current_input()),
            device_path: monitor.device_path,
        },
        Job::Switch(monitor, value) => Report::Switched(
            if open(&monitor).is_some_and(|mut ddc| ddc.set_input(value)) {
                Ok((monitor.device_path, value))
            } else {
                Err(format!(
                    "Could not switch {} to {}: the monitor did not answer over DDC/CI.",
                    monitor.name,
                    input_name(value)
                ))
            },
        ),
    })
}

/// The status line's text after reading the input lists, when no monitor has one.
pub fn probed_notice(probes: &[Probe]) -> Option<&'static str> {
    (!probes.iter().any(|probe| probe.inputs.is_some()))
        .then_some("No monitor offers input switching over DDC/CI.")
}

/// The command switching `monitor` (from [`command_monitor`]) to input `value`, run by `exe`.
// shortcut: a name with a `"` in it breaks the quoting; quote it as Windows does if a monitor ever has one.
pub fn command_line(exe: &str, monitor: &str, value: u8) -> String {
    let monitor = if monitor.contains(' ') {
        format!("\"{monitor}\"")
    } else {
        monitor.to_owned()
    };
    let input = match value {
        0x0F => "dp".to_owned(),
        0x10 => "dp2".to_owned(),
        0x11 => "hdmi1".to_owned(),
        0x12 => "hdmi2".to_owned(),
        _ => format!("0x{value:02X}"),
    };
    format!("\"{exe}\" input {monitor} {input}")
}

/// [`command_line`] for this cardcast, as the Copy command button copies it.
pub fn copy_command(monitor: &str, value: u8) -> String {
    let exe =
        std::env::current_exe().map_or_else(|_| EXE.to_owned(), |exe| exe.display().to_string());
    command_line(&exe, monitor, value)
}

/// Whether the settings window reads every monitor's input list now: only while the toggle is `on`, when it was just switched on, at launch (`was_on` is `None`) or after a display change.
pub fn reads_input_lists(was_on: Option<bool>, on: bool, displays_changed: bool) -> bool {
    on && (was_on != Some(true) || displays_changed)
}

/// Hex pairs, spaced or not (`0F 11` or `0F11`); `None` for anything else.
fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for word in text.split_whitespace() {
        if word.len() % 2 != 0 || !word.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        for pair in word.as_bytes().chunks(2) {
            bytes.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn input_list_reads_vcp_60() {
        let caps = "(prot(monitor)type(LCD)model(AW2523HF)cmds(01 02 03 07 0C E3 F3)vcp(02 04 10 12 60(0F 11 12) D6(01 04 05))mccs_ver(2.1))";
        assert_eq!(input_list(caps), Some(vec![0x0F, 0x11, 0x12]));
    }

    #[test]
    fn input_list_reads_values_with_no_spaces() {
        assert_eq!(
            input_list("(vcp(02041012600F1112(0F1112)))"),
            None,
            "a code run together with the next is not 60"
        );
        assert_eq!(
            input_list("(vcp(0204 60(0F1112) D6(0104)))"),
            Some(vec![0x0F, 0x11, 0x12])
        );
        assert_eq!(
            input_list("(vcp(02 04 60(0F11 12)))"),
            Some(vec![0x0F, 0x11, 0x12])
        );
    }

    #[test]
    fn input_list_reads_a_string_missing_the_outer_parenthesis() {
        assert_eq!(
            input_list("prot(monitor)type(lcd)vcp(02 04 60(0f 11) D6(01 04))mccs_ver(2.1)"),
            Some(vec![0x0F, 0x11])
        );
    }

    #[test]
    fn input_list_refuses_garbage() {
        for caps in [
            "",
            "garbage",
            "(prot(monitor)type(lcd))",
            "(vcp(02 04 10))",
            "(vcp(02 04 60))",
            "(vcp(02 04 60(0F 11 12",
            "(vcp(02 04 60(0F 1 12)))",
            "(vcp(02 04 60(0F XX 12)))",
            "(vcp(02 04 60(+F 11)))",
            "(vcp(02 ?? 60(0F 11)))",
            "(vcp(02 04 60(0F 11 ü2)))",
        ] {
            assert_eq!(input_list(caps), None, "{caps:?}");
        }
    }

    #[test]
    fn input_value_reads_names_and_hex_values() {
        for (word, value) in [
            ("dp", 0x0F),
            ("DP2", 0x10),
            ("hdmi1", 0x11),
            ("Hdmi2", 0x12),
            ("0x1b", 0x1B),
            ("0X1B", 0x1B),
            ("0x01", 0x01),
            ("0xFF", 0xFF),
        ] {
            assert_eq!(input_value(word), Some(value), "{word:?}");
        }
        for word in [
            "", "dp1", "hdmi", "hdmi3", "usbc", "0x00", "0x1", "0x100", "1b", "0x+1", "0xü",
        ] {
            assert_eq!(input_value(word), None, "{word:?}");
        }
    }

    #[test]
    fn input_name_follows_mccs() {
        assert_eq!(input_name(0x0F), "DisplayPort 1");
        assert_eq!(input_name(0x10), "DisplayPort 2");
        assert_eq!(input_name(0x11), "HDMI 1");
        assert_eq!(input_name(0x12), "HDMI 2");
        assert_eq!(input_name(0x1B), "Input 0x1B");
        assert_eq!(input_name(0x03), "Input 0x03");
    }

    /// A 128-byte EDID base block with a valid header and checksum.
    fn edid(serial_number: u32, serial_descriptor: Option<&[u8; 13]>) -> Vec<u8> {
        let mut edid = vec![0; 128];
        edid[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        edid[12..16].copy_from_slice(&serial_number.to_le_bytes());
        // Descriptor 1 is the preferred timing, descriptor 2 a monitor name (tag 0xFC).
        edid[54] = 0x01;
        edid[72 + 3] = 0xFC;
        edid[72 + 5..72 + 18].copy_from_slice(b"AW2523HF\n    ");
        if let Some(text) = serial_descriptor {
            edid[90 + 3] = 0xFF;
            edid[90 + 5..90 + 18].copy_from_slice(text);
        }
        let sum = edid.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
        edid[127] = 0u8.wrapping_sub(sum);
        edid
    }

    #[test]
    fn edid_serial_prefers_the_serial_descriptor() {
        assert_eq!(
            edid_serial(&edid(12345, Some(b"4GZY5P3\n     "))),
            Some("4GZY5P3".to_owned())
        );
        assert_eq!(
            edid_serial(&edid(0, Some(b"ABCDEFGHIJKLM"))),
            Some("ABCDEFGHIJKLM".to_owned()),
            "13 characters need no line feed"
        );
    }

    #[test]
    fn edid_serial_falls_back_to_the_serial_number() {
        assert_eq!(edid_serial(&edid(12345, None)), Some("12345".to_owned()));
        assert_eq!(
            edid_serial(&edid(12345, Some(b"\n            "))),
            Some("12345".to_owned()),
            "an empty descriptor"
        );
    }

    #[test]
    fn edid_serial_refuses_a_missing_serial_or_a_broken_edid() {
        assert_eq!(edid_serial(&edid(0, None)), None);
        let mut checksum = edid(12345, Some(b"4GZY5P3\n     "));
        checksum[127] = checksum[127].wrapping_add(1);
        assert_eq!(edid_serial(&checksum), None, "wrong checksum");
        let mut header = edid(12345, None);
        header[0] = 0x01;
        assert_eq!(edid_serial(&header), None, "wrong header");
        assert_eq!(edid_serial(&edid(12345, None)[..127]), None, "too short");
        assert_eq!(edid_serial(&[]), None);
        assert_eq!(
            edid_serial(&edid(12345, Some(b"4GZ\x01Y5P3\n    "))),
            None,
            "a control character"
        );
    }

    #[test]
    fn edid_name_and_product_read_the_name_descriptor_and_the_maker() {
        let mut dell = edid(12345, None);
        // "DEL" is 00100 00101 01100, product 0xA0FA little-endian.
        dell[8..12].copy_from_slice(&[0x10, 0xAC, 0xFA, 0xA0]);
        let sum = dell[..127].iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
        dell[127] = 0u8.wrapping_sub(sum);
        assert_eq!(edid_name(&dell), Some("AW2523HF".to_owned()));
        assert_eq!(edid_product(&dell), Some("DELA0FA".to_owned()));
        dell[0] = 0x01;
        assert_eq!((edid_name(&dell), edid_product(&dell)), (None, None));
    }

    fn monitor(serial: Option<&str>, name: &str) -> MonitorId {
        MonitorId {
            serial: serial.map(str::to_owned),
            name: name.to_owned(),
        }
    }

    #[test]
    fn select_monitor_by_serial_or_name_ignoring_case() {
        let monitors = [
            monitor(Some("4GZY5P3"), "AW2523HF"),
            monitor(None, "DELL P2210"),
        ];
        assert_eq!(select_monitor(&monitors, "4gzy5p3"), Ok(0));
        assert_eq!(select_monitor(&monitors, "aw2523hf"), Ok(0));
        assert_eq!(select_monitor(&monitors, "Dell P2210"), Ok(1));
        assert_eq!(
            select_monitor(&monitors, "P2210"),
            Err(SelectError::NoSuchMonitor),
            "no partial names"
        );
        assert_eq!(
            select_monitor(&monitors, r"\\.\DISPLAY1"),
            Err(SelectError::NoSuchMonitor)
        );
        assert_eq!(
            select_monitor(&[], "AW2523HF"),
            Err(SelectError::NoSuchMonitor)
        );
    }

    #[test]
    fn select_monitor_refuses_a_name_that_fits_two_monitors() {
        let monitors = [
            monitor(Some("4GZY5P3"), "AW2523HF"),
            monitor(Some("9XYZ123"), "AW2523HF"),
        ];
        assert_eq!(
            select_monitor(&monitors, "AW2523HF"),
            Err(SelectError::SeveralFit)
        );
        assert_eq!(select_monitor(&monitors, "9xyz123"), Ok(1));
        let serial_is_a_name = [monitor(Some("ABC"), "Left"), monitor(None, "abc")];
        assert_eq!(
            select_monitor(&serial_is_a_name, "ABC"),
            Err(SelectError::SeveralFit)
        );
    }

    #[test]
    fn command_monitor_is_unique_among_the_monitors() {
        let monitors = [
            monitor(Some("4GZY5P3"), "AW2523HF"),
            monitor(Some("16843009"), "DELL P2210"),
            monitor(Some("16843009"), "DELL U2412M"),
            monitor(None, "Generic PnP Monitor"),
        ];
        assert_eq!(command_monitor(&monitors, 0), Some("4GZY5P3"));
        assert_eq!(
            command_monitor(&monitors, 1),
            Some("DELL P2210"),
            "a placeholder serial shared by two monitors"
        );
        assert_eq!(command_monitor(&monitors, 3), Some("Generic PnP Monitor"));
        assert_eq!(command_monitor(&monitors, 4), None);
    }

    #[test]
    fn command_monitor_is_none_when_neither_is_unique() {
        let twins = [
            monitor(Some("16843009"), "AW2523HF"),
            monitor(Some("16843009"), "AW2523HF"),
        ];
        assert_eq!(command_monitor(&twins, 0), None);
        let serial_is_a_name = [monitor(Some("ABC"), "Left"), monitor(None, "abc")];
        assert_eq!(command_monitor(&serial_is_a_name, 0), Some("Left"));
        assert_eq!(command_monitor(&serial_is_a_name, 1), None);
    }

    fn parse(command: &str) -> Result<InputRequest, String> {
        let args: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        parse_input_command(&args)
    }

    #[test]
    fn parse_input_command_reads_one_input() {
        assert_eq!(
            parse("input 4GZY5P3 hdmi1"),
            Ok(InputRequest {
                monitor: "4GZY5P3".to_owned(),
                target: InputTarget::Input(0x11),
            })
        );
        let args = [
            "input".to_owned(),
            "Dell P2210".to_owned(),
            "0x1B".to_owned(),
        ];
        assert_eq!(
            parse_input_command(&args),
            Ok(InputRequest {
                monitor: "Dell P2210".to_owned(),
                target: InputTarget::Input(0x1B),
            }),
            "a quoted name with a space is one argument"
        );
    }

    #[test]
    fn parse_input_command_reads_a_toggle() {
        assert_eq!(
            parse("input 4GZY5P3 toggle dp hdmi1"),
            Ok(InputRequest {
                monitor: "4GZY5P3".to_owned(),
                target: InputTarget::Toggle(0x0F, 0x11),
            })
        );
    }

    #[test]
    fn parse_input_command_refuses_every_grammar_error() {
        for command in [
            "",
            "switch 4GZY5P3 dp",
            "input",
            "input 4GZY5P3",
            "input 4GZY5P3 hdmi3",
            "input 4GZY5P3 0x00",
            "input 4GZY5P3 dp hdmi1",
            "input 4GZY5P3 toggle",
            "input 4GZY5P3 toggle dp",
            "input 4GZY5P3 toggle dp hdmi3",
            "input 4GZY5P3 toggle usbc hdmi1",
            "input 4GZY5P3 toggle dp 0x0f",
            "input 4GZY5P3 toggle dp hdmi1 hdmi2",
        ] {
            let error = parse(command).unwrap_err();
            assert!(
                error.starts_with("Not understood: "),
                "{command:?}: {error}"
            );
        }
        let empty_monitor = ["input".to_owned(), "".to_owned(), "dp".to_owned()];
        assert!(parse_input_command(&empty_monitor).is_err());
    }

    #[test]
    fn decide_switch_to_one_input() {
        let inputs = [0x0F, 0x11, 0x12];
        assert_eq!(
            decide_switch(InputTarget::Input(0x11), &inputs, 0x0F),
            Ok(Switch::SwitchTo(0x11))
        );
        assert_eq!(
            decide_switch(InputTarget::Input(0x11), &inputs, 0x11),
            Ok(Switch::AlreadyOn(0x11))
        );
        assert_eq!(
            decide_switch(InputTarget::Input(0x10), &inputs, 0x0F),
            Err(NotListed(0x10))
        );
    }

    #[test]
    fn decide_switch_toggles_from_each_side() {
        let inputs = [0x0F, 0x11, 0x12];
        let toggle = InputTarget::Toggle(0x0F, 0x11);
        assert_eq!(
            decide_switch(toggle, &inputs, 0x0F),
            Ok(Switch::SwitchTo(0x11))
        );
        assert_eq!(
            decide_switch(toggle, &inputs, 0x11),
            Ok(Switch::SwitchTo(0x0F))
        );
        assert_eq!(
            decide_switch(toggle, &inputs, 0x12),
            Ok(Switch::SwitchTo(0x0F)),
            "on neither: to the first"
        );
    }

    #[test]
    fn decide_switch_refuses_a_toggle_with_an_unlisted_input() {
        let inputs = [0x0F, 0x11];
        assert_eq!(
            decide_switch(InputTarget::Toggle(0x0F, 0x12), &inputs, 0x11),
            Err(NotListed(0x12)),
            "refused even though the switch would go to the listed one"
        );
        assert_eq!(
            decide_switch(InputTarget::Toggle(0x1B, 0x11), &inputs, 0x0F),
            Err(NotListed(0x1B))
        );
    }

    #[test]
    fn parse_input_command_says_toggle_needs_two_inputs() {
        for command in ["input 4GZY5P3 toggle", "input 4GZY5P3 TOGGLE dp"] {
            let error = parse(command).unwrap_err();
            assert!(
                error.starts_with("Not understood: toggle needs two inputs."),
                "{command:?}: {error}"
            );
        }
    }

    #[test]
    fn input_list_refuses_a_nested_parenthesis() {
        assert_eq!(input_list("(vcp(02 60(0F (11) 12)))"), None);
        assert_eq!(input_list("(vcp(02 D6(01 (04)) 60(0F 11)))"), None);
    }

    /// A monitor answering DDC/CI as scripted; `set` records the value it was switched to.
    struct FakeDdc {
        capabilities: Option<&'static str>,
        current: Option<u8>,
        answers_set: bool,
        set: Rc<Cell<Option<u8>>>,
    }

    impl Ddc for FakeDdc {
        fn capabilities(&mut self) -> Option<String> {
            self.capabilities.map(str::to_owned)
        }

        fn current_input(&mut self) -> Option<u8> {
            self.current
        }

        fn set_input(&mut self, value: u8) -> bool {
            self.set.set(Some(value));
            self.answers_set
        }
    }

    const AW2523HF_CAPS: &str =
        "(prot(monitor)type(LCD)model(AW2523HF)vcp(02 04 60(0F 11 12))mccs_ver(2.1))";

    /// The AW2523HF on `current`, answering everything.
    fn aw2523hf(current: u8) -> FakeDdc {
        FakeDdc {
            capabilities: Some(AW2523HF_CAPS),
            current: Some(current),
            answers_set: true,
            set: Rc::default(),
        }
    }

    /// Runs `command` against the AW2523HF and a DELL P2210, opening the AW2523HF as `ddc`.
    fn run(command: &str, ddc: Option<FakeDdc>) -> (i32, String) {
        let args: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        let monitors = vec![
            (monitor(None, "DELL P2210"), 0),
            (monitor(Some("4GZY5P3"), "AW2523HF"), 1),
        ];
        run_command(
            &args,
            || Ok(monitors),
            |index| {
                assert_eq!(index, 1, "opened the wrong monitor");
                ddc.map(|ddc| Box::new(ddc) as Box<dyn Ddc>)
            },
        )
    }

    #[test]
    fn run_command_exits_2_when_not_understood() {
        for command in ["input", "switch 4GZY5P3 dp", "input 4GZY5P3 hdmi3"] {
            let (code, line) = run_command(
                &command
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                || -> Result<Vec<(MonitorId, ())>, String> { panic!("listed monitors") },
                |()| panic!("opened a monitor"),
            );
            assert_eq!(code, 2, "{command:?}");
            assert!(line.starts_with("Not understood: "), "{command:?}: {line}");
            let usage = format!("Use: {EXE} input <monitor> <input>, or {EXE} input");
            assert!(line.contains(&usage), "{line}");
        }
    }

    #[test]
    fn run_command_switches_and_exits_0() {
        let ddc = aw2523hf(0x0F);
        let set = Rc::clone(&ddc.set);
        assert_eq!(
            run("input 4gzy5p3 hdmi1", Some(ddc)),
            (0, "Switched AW2523HF to HDMI 1.".to_owned())
        );
        assert_eq!(set.get(), Some(0x11));
    }

    #[test]
    fn run_command_toggles_back() {
        let ddc = aw2523hf(0x11);
        let set = Rc::clone(&ddc.set);
        assert_eq!(
            run("input AW2523HF toggle dp hdmi1", Some(ddc)),
            (0, "Switched AW2523HF to DisplayPort 1.".to_owned())
        );
        assert_eq!(set.get(), Some(0x0F));
    }

    #[test]
    fn run_command_leaves_a_monitor_already_on_the_input() {
        let ddc = aw2523hf(0x11);
        let set = Rc::clone(&ddc.set);
        assert_eq!(
            run("input 4GZY5P3 hdmi1", Some(ddc)),
            (0, "AW2523HF is already on HDMI 1.".to_owned())
        );
        assert_eq!(set.get(), None, "nothing sent");
    }

    #[test]
    fn run_command_exits_1_for_an_unlisted_input() {
        let ddc = FakeDdc {
            capabilities: Some("(vcp(60(0F 11)))"),
            ..aw2523hf(0x0F)
        };
        let set = Rc::clone(&ddc.set);
        assert_eq!(
            run("input 4GZY5P3 hdmi2", Some(ddc)),
            (
                1,
                "AW2523HF does not list HDMI 2; it lists DisplayPort 1, HDMI 1.".to_owned()
            )
        );
        assert_eq!(set.get(), None, "nothing sent");
    }

    #[test]
    fn run_command_exits_1_for_a_wrong_monitor() {
        assert_eq!(
            run("input P2210 dp", None),
            (
                1,
                r#"No monitor has the serial or name "P2210"."#.to_owned()
            )
        );
        let (code, line) = run_command(
            &["input".to_owned(), "AW2523HF".to_owned(), "dp".to_owned()],
            || {
                Ok(vec![
                    (monitor(Some("1"), "AW2523HF"), ()),
                    (monitor(Some("2"), "AW2523HF"), ()),
                ])
            },
            |()| panic!("opened a monitor"),
        );
        assert_eq!(code, 1);
        assert_eq!(
            line,
            r#"More than one monitor has the serial or name "AW2523HF"; use one only this monitor has."#
        );
        let (code, line) = run_command(
            &["input".to_owned(), "AW2523HF".to_owned(), "dp".to_owned()],
            || Err::<Vec<(MonitorId, ())>, _>("Could not list monitors.".to_owned()),
            |()| panic!("opened a monitor"),
        );
        assert_eq!((code, line.as_str()), (1, "Could not list monitors."));
    }

    #[test]
    fn run_command_exits_1_when_the_monitor_does_not_answer() {
        let no_answer = |to: &str| {
            (
                1,
                format!("Could not switch AW2523HF{to}: the monitor did not answer over DDC/CI."),
            )
        };
        assert_eq!(run("input 4GZY5P3 hdmi1", None), no_answer(" to HDMI 1"));
        assert_eq!(run("input 4GZY5P3 toggle dp hdmi1", None), no_answer(""));
        let silent = FakeDdc {
            capabilities: None,
            ..aw2523hf(0x0F)
        };
        assert_eq!(
            run("input 4GZY5P3 hdmi1", Some(silent)),
            no_answer(" to HDMI 1")
        );
        let silent = FakeDdc {
            current: None,
            ..aw2523hf(0x0F)
        };
        assert_eq!(
            run("input 4GZY5P3 hdmi1", Some(silent)),
            no_answer(" to HDMI 1")
        );
        let silent = FakeDdc {
            answers_set: false,
            ..aw2523hf(0x0F)
        };
        assert_eq!(
            run("input 4GZY5P3 hdmi1", Some(silent)),
            no_answer(" to HDMI 1")
        );
    }

    #[test]
    fn run_command_exits_1_without_an_input_list() {
        let one_input = FakeDdc {
            capabilities: Some("(vcp(02 04 60(0F)))"),
            ..aw2523hf(0x0F)
        };
        assert_eq!(
            run("input 4GZY5P3 dp", Some(one_input)),
            (
                1,
                "AW2523HF has no input switch: it lists fewer than two inputs over DDC/CI."
                    .to_owned()
            )
        );
    }

    #[test]
    fn input_lists_are_read_only_while_the_toggle_is_on() {
        // (was on: None at launch, on, displays changed) -> read.
        for (was_on, on, displays_changed, read) in [
            (None, false, false, false),
            (None, false, true, false),
            (Some(false), false, true, false),
            (Some(true), false, false, false),
            (Some(true), false, true, false),
            (None, true, false, true),
            (Some(false), true, false, true),
            (Some(true), true, true, true),
            (Some(true), true, false, false),
        ] {
            assert_eq!(
                reads_input_lists(was_on, on, displays_changed),
                read,
                "{was_on:?} {on} {displays_changed}"
            );
        }
    }

    fn screen(device_path: &str, name: &str) -> Monitor {
        Monitor {
            device_path: device_path.into(),
            gdi_name: String::new(),
            name: name.into(),
            width: 1920,
            height: 1080,
            layout_size: (1920, 1080),
            refresh_hz: 60,
            primary: false,
            left: 0,
            top: 0,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        }
    }

    /// The AW2523HF answers DDC/CI, the card display doesn't, and the other monitor lists one input.
    fn sender_ddc(monitor: &Monitor) -> Option<Box<dyn Ddc>> {
        let capabilities = match monitor.device_path.as_str() {
            "aw" => AW2523HF_CAPS,
            "one" => "(vcp(60(0F)))",
            _ => return None,
        };
        Some(Box::new(FakeDdc {
            capabilities: Some(capabilities),
            ..aw2523hf(0x0F)
        }))
    }

    #[test]
    fn probe_reads_each_monitors_serial_and_input_list() {
        let monitors = [
            screen("aw", "AW2523HF"),
            screen("card", "Elgato 1080p"),
            screen("one", "DELL P2210"),
        ];
        let serial = |monitor: &Monitor| (monitor.device_path == "aw").then(|| "4GZY5P3".into());
        let probes = run_job(
            Job::Probe(monitors.to_vec()),
            &[],
            &serial,
            &sender_ddc,
            &AtomicBool::new(false),
        );
        let probe = |device_path: &str, serial: Option<&str>, name: &str, inputs| Probe {
            device_path: device_path.into(),
            id: monitor(serial, name),
            inputs,
        };
        assert_eq!(
            probes,
            Some(Report::Probed(vec![
                probe(
                    "aw",
                    Some("4GZY5P3"),
                    "AW2523HF",
                    Some(vec![0x0F, 0x11, 0x12])
                ),
                probe("card", None, "Elgato 1080p", None),
                probe("one", None, "DELL P2210", None),
            ]))
        );
    }

    #[test]
    fn read_and_switch_say_when_the_monitor_does_not_answer() {
        let no_serial = |_: &Monitor| None;
        let run = |job| run_job(job, &[], &no_serial, &sender_ddc, &AtomicBool::new(false));
        let (aw, card) = (screen("aw", "AW2523HF"), screen("card", "Elgato 1080p"));
        assert_eq!(
            run(Job::Read(aw.clone())),
            Some(Report::Current {
                device_path: "aw".into(),
                input: Some(0x0F)
            })
        );
        assert_eq!(
            run(Job::Read(card.clone())),
            Some(Report::Current {
                device_path: "card".into(),
                input: None
            })
        );
        assert_eq!(
            run(Job::Switch(aw, 0x11)),
            Some(Report::Switched(Ok(("aw".into(), 0x11))))
        );
        assert_eq!(
            run(Job::Switch(card, 0x11)),
            Some(Report::Switched(Err(
                "Could not switch Elgato 1080p to HDMI 1: the monitor did not answer over DDC/CI."
                    .into()
            )))
        );
    }

    #[test]
    fn the_status_line_says_when_no_monitor_offers_input_switching() {
        let probe = |inputs| Probe {
            device_path: "aw".into(),
            id: monitor(None, "AW2523HF"),
            inputs,
        };
        let none = "No monitor offers input switching over DDC/CI.";
        assert_eq!(probed_notice(&[]), Some(none));
        assert_eq!(probed_notice(&[probe(None)]), Some(none));
        assert_eq!(
            probed_notice(&[probe(None), probe(Some(vec![0x0F, 0x11]))]),
            None
        );
    }

    #[test]
    fn copy_command_switches_this_monitor_to_this_input() {
        let exe = r"C:\Program Files\cardcast\cardcast.exe";
        assert_eq!(
            command_line(exe, "4GZY5P3", 0x11),
            r#""C:\Program Files\cardcast\cardcast.exe" input 4GZY5P3 hdmi1"#
        );
        assert_eq!(
            command_line("cardcast.exe", "DELL P2210", 0x1B),
            r#""cardcast.exe" input "DELL P2210" 0x1B"#
        );
        // Each input word reads back as its value.
        for value in [0x0F, 0x10, 0x11, 0x12, 0x01, 0x1B, 0xFF] {
            let line = command_line("cardcast.exe", "4GZY5P3", value);
            let word = line.rsplit(' ').next().unwrap();
            assert_eq!(input_value(word), Some(value), "{line}");
        }
        assert!(command_line("cardcast.exe", "4GZY5P3", 0x0F).ends_with(" dp"));
    }

    #[test]
    fn the_input_thread_does_the_jobs_and_wakes_the_settings_window() {
        let woken = std::sync::Arc::new(AtomicBool::new(false));
        let wake = {
            let woken = std::sync::Arc::clone(&woken);
            move || woken.store(true, Ordering::Relaxed)
        };
        let thread = InputThread::spawn(|_: &Monitor| None, sender_ddc, wake).unwrap();
        thread.send(Job::Read(screen("aw", "AW2523HF")));
        let start = std::time::Instant::now();
        let report = loop {
            if let Some(report) = thread.take_report() {
                break report;
            }
            assert!(start.elapsed().as_secs() < 10, "no report");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(
            report,
            Report::Current {
                device_path: "aw".into(),
                input: Some(0x0F)
            }
        );
        assert!(woken.load(Ordering::Relaxed));
    }

    #[test]
    fn probe_reads_only_monitors_not_seen_before() {
        let opened = std::sync::Mutex::new(Vec::new());
        let open = |monitor: &Monitor| {
            opened.lock().unwrap().push(monitor.device_path.clone());
            sender_ddc(monitor)
        };
        let known = [
            Probe {
                device_path: "aw".into(),
                id: monitor(Some("4GZY5P3"), "AW2523HF"),
                inputs: Some(vec![0x0F, 0x11]),
            },
            Probe {
                device_path: "gone".into(),
                id: monitor(None, "Gone"),
                inputs: Some(vec![0x0F, 0x11]),
            },
            Probe {
                device_path: "card".into(),
                id: monitor(None, "Elgato 1080p"),
                inputs: None,
            },
        ];
        let monitors = vec![
            screen("aw", "AW2523HF"),
            screen("card", "Elgato 1080p"),
            screen("one", "DELL P2210"),
        ];
        let probes = run_job(
            Job::Probe(monitors),
            &known,
            &|_: &Monitor| None,
            &open,
            &AtomicBool::new(false),
        );
        assert_eq!(*opened.lock().unwrap(), ["one"]);
        assert_eq!(
            probes,
            Some(Report::Probed(vec![
                known[0].clone(),
                known[2].clone(),
                Probe {
                    device_path: "one".into(),
                    id: monitor(None, "DELL P2210"),
                    inputs: None,
                },
            ]))
        );
    }

    #[test]
    fn only_the_newest_waiting_probe_runs() {
        let (aw, card) = (screen("aw", "AW2523HF"), screen("card", "Elgato 1080p"));
        let waiting = vec![
            ((), Job::Probe(vec![aw.clone()])),
            ((), Job::Read(aw.clone())),
            ((), Job::Probe(vec![aw.clone(), card.clone()])),
            ((), Job::Switch(card.clone(), 0x11)),
        ];
        assert_eq!(
            newest_probe_only(waiting),
            [
                ((), Job::Read(aw.clone())),
                ((), Job::Probe(vec![aw, card.clone()])),
                ((), Job::Switch(card, 0x11)),
            ]
        );
    }

    #[test]
    fn switching_off_and_on_keeps_one_thread_and_reads_every_monitor_again() {
        let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recording = std::sync::Arc::clone(&opened);
        let mut thread = InputThread::spawn(
            |_: &Monitor| None,
            move |monitor: &Monitor| {
                recording.lock().unwrap().push(monitor.device_path.clone());
                sender_ddc(monitor)
            },
            || {},
        )
        .unwrap();
        let wait_for_probe = |thread: &InputThread| {
            let start = std::time::Instant::now();
            loop {
                if let Some(Report::Probed(_)) = thread.take_report() {
                    return;
                }
                assert!(start.elapsed().as_secs() < 10, "no report");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let monitors = vec![screen("aw", "AW2523HF")];
        thread.send(Job::Probe(monitors.clone()));
        wait_for_probe(&thread);
        thread.send(Job::Probe(monitors.clone()));
        wait_for_probe(&thread);
        assert_eq!(
            *opened.lock().unwrap(),
            ["aw"],
            "read a known monitor again"
        );
        thread.cancel();
        thread.send(Job::Probe(monitors));
        wait_for_probe(&thread);
        assert_eq!(*opened.lock().unwrap(), ["aw", "aw"]);
    }

    #[test]
    fn probe_stops_when_the_toggle_is_switched_off() {
        let opened = std::sync::Mutex::new(Vec::new());
        let stop = AtomicBool::new(false);
        let open = |monitor: &Monitor| {
            opened.lock().unwrap().push(monitor.device_path.clone());
            stop.store(true, Ordering::Relaxed);
            None
        };
        let monitors = vec![screen("aw", "AW2523HF"), screen("one", "DELL P2210")];
        assert_eq!(
            run_job(Job::Probe(monitors), &[], &|_: &Monitor| None, &open, &stop),
            None
        );
        assert_eq!(*opened.lock().unwrap(), ["aw"], "read the next monitor");
    }

    #[test]
    fn input_list_needs_two_different_inputs() {
        assert_eq!(input_list("(vcp(60(0F)))"), None);
        assert_eq!(input_list("(vcp(60(0F 0F)))"), None);
        assert_eq!(input_list("(vcp(60(11 0F 11)))"), Some(vec![0x11, 0x0F]));
    }
}
