//! Input switch decisions (SPEC section 11). No Windows calls, so every platform's DDC/CI code can share them.

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
    fn input_list_needs_two_different_inputs() {
        assert_eq!(input_list("(vcp(60(0F)))"), None);
        assert_eq!(input_list("(vcp(60(0F 0F)))"), None);
        assert_eq!(input_list("(vcp(60(11 0F 11)))"), Some(vec![0x11, 0x0F]));
    }
}
