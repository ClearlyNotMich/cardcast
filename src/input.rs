//! Input switch decisions and command mode (SPEC section 11). Windows calls only in `ddc`, so every platform's DDC/CI code can share the rest.

#[cfg(windows)]
mod ddc;

use std::io::Write as _;

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
    #[cfg(windows)]
    let (code, line) = run_command(args, ddc::monitors, ddc::open);
    // shortcut: no DDC/CI on Linux yet, so a valid command exits 1; issue #46 adds it.
    #[cfg(not(windows))]
    let (code, line) = run_command(
        args,
        || {
            Err::<Vec<(MonitorId, ())>, _>(
                "Input switching is not available on Linux yet.".to_owned(),
            )
        },
        |()| None,
    );
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
    let block = edid.get(..128)?;
    let checksum = block.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    if block[..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] || checksum != 0 {
        return None;
    }
    // Four 18-byte descriptors; a display descriptor starts 00 00 00 <tag>, and 0xFF is the serial.
    for descriptor in block[54..126].chunks(18) {
        if descriptor[..4] == [0x00, 0x00, 0x00, 0xFF] {
            let text = descriptor[5..].split(|b| *b == b'\n').next()?;
            if !text.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                return None;
            }
            let text = std::str::from_utf8(text).ok()?.trim();
            if !text.is_empty() {
                return Some(text.to_owned());
            }
        }
    }
    let number = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
    (number != 0).then(|| number.to_string())
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
            "Not understood: {what}. Use: cardcast.exe input <monitor> <input>, or cardcast.exe input <monitor> toggle <input> <input>, where <input> is dp, dp2, hdmi1, hdmi2 or 0x01 to 0xFF."
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
    fn input_list_needs_two_different_inputs() {
        assert_eq!(input_list("(vcp(60(0F)))"), None);
        assert_eq!(input_list("(vcp(60(0F 0F)))"), None);
        assert_eq!(input_list("(vcp(60(11 0F 11)))"), Some(vec![0x11, 0x0F]));
    }
}
