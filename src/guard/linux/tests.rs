use super::*;

/// `/sys/class/drm` on the sender (CachyOS, Plasma Wayland), recorded 2026-11-28: the HD60 X on the
/// AMD integrated graphics' HDMI-A-2, three monitors on the RTX 4080's DP-1 to DP-3.
fn sender_drm() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sender-drm")
}

/// The sender's lines of `/usr/share/hwdata/pci.ids`, plus another vendor's device 13c0.
const PCI_IDS: &str = "\
# nee ATI Technologies, Inc.
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t1114  Krackan [Radeon 840M / 860M Graphics]
\t13c0  Granite Ridge [Radeon 610M]
\t\t1043 8877  Radeon 610M
1093  National Instruments
\t13c0  PXI-6508
10de  NVIDIA Corporation
\t13c0  GM204 [GeForce GTX 980]
# AD103
\t2704  AD103 [GeForce RTX 4080]
";

const RTX_4080: &str = "AD103 [GeForce RTX 4080]";
const RADEON: &str = "Granite Ridge [Radeon 610M]";

/// An unscaled output: its size on the desktop is its resolution.
fn output(name: &str, description: &str, at: (i32, i32), size: (u32, u32), hz: u32) -> Output {
    Output {
        layout_size: size,
        name: name.into(),
        description: description.into(),
        edid: None,
        left: at.0,
        top: at.1,
        width: size.0,
        height: size.1,
        refresh_hz: hz,
    }
}

/// The sender's outputs as Plasma's wl_output and xdg-output list them (`wayland-info`), recorded
/// with the connectors: DP-1 is turned to portrait at scale 1.3.
fn sender_outputs() -> Vec<Output> {
    vec![
        Output {
            layout_size: (1108, 1969),
            ..output(
                "DP-1",
                "Dell Inc. Dell S2417DG",
                (0, 373),
                (1440, 2560),
                144,
            )
        },
        output(
            "HDMI-A-2",
            "Elgato Systems LLC Elgato1080HDR",
            (3668, 1080),
            (1920, 1080),
            240,
        ),
        output(
            "DP-3",
            "Microstep MAG 272Q X50",
            (1108, 1080),
            (2560, 1440),
            500,
        ),
        output(
            "DP-2",
            "Dell Inc. DELL AW2523HF",
            (1108, 0),
            (1920, 1080),
            360,
        ),
    ]
}

#[test]
fn the_senders_connected_connectors_are_read_with_their_graphics_cards() {
    let connectors = connectors(&sender_drm(), PCI_IDS);
    let found: Vec<_> = connectors
        .iter()
        .map(|c| {
            (
                c.card.as_str(),
                c.name.as_str(),
                c.graphics_card.as_str(),
                edid_name(&c.edid),
            )
        })
        .collect();
    let named = |name: &str| Some(name.to_owned());
    assert_eq!(
        found,
        [
            ("0000:01:00.0", "DP-1", RTX_4080, named("Dell S2417DG")),
            ("0000:01:00.0", "DP-2", RTX_4080, named("DELL AW2523HF")),
            ("0000:01:00.0", "DP-3", RTX_4080, named("MAG 272Q X50")),
            ("0000:0c:00.0", "HDMI-A-2", RADEON, named("Elgato1080HDR")),
        ]
    );
}

#[test]
fn without_pci_ids_the_graphics_card_is_its_driver() {
    let cards: Vec<_> = connectors(&sender_drm(), "")
        .into_iter()
        .map(|c| c.graphics_card)
        .collect();
    assert_eq!(cards, ["nvidia", "nvidia", "nvidia", "amdgpu"]);
}

#[test]
fn pci_name_looks_only_under_the_vendor() {
    assert_eq!(pci_name(PCI_IDS, "1002", "13C0"), Some(RADEON));
    assert_eq!(
        pci_name(PCI_IDS, "10DE", "13c0"),
        Some("GM204 [GeForce GTX 980]")
    );
    assert_eq!(pci_name(PCI_IDS, "10de", "2704"), Some(RTX_4080));
    assert_eq!(pci_name(PCI_IDS, "1002", "2704"), None);
    assert_eq!(pci_name(PCI_IDS, "abcd", "13c0"), None);
}

#[test]
fn the_senders_four_monitors_are_matched_to_their_connectors() {
    let connectors = connectors(&sender_drm(), PCI_IDS);
    let monitors = match_monitors(sender_outputs(), &connectors, Some("DP-3"));
    let found: Vec<_> = monitors
        .iter()
        .map(|m| {
            (
                m.name.as_str(),
                m.device_path.as_str(),
                m.connector.as_str(),
                m.graphics_card.as_str(),
                m.primary,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "Dell S2417DG",
                "DELA0E7 #ASNV8046oaXd",
                "0000:01:00.0 DP-1",
                RTX_4080,
                false
            ),
            (
                "Elgato1080HDR",
                "EGA0000 1080240",
                "0000:0c:00.0 HDMI-A-2",
                RADEON,
                false
            ),
            (
                "MAG 272Q X50",
                "MSI6CD7 16843009",
                "0000:01:00.0 DP-3",
                RTX_4080,
                true
            ),
            (
                "DELL AW2523HF",
                "DEL42B9 4GZY5P3",
                "0000:01:00.0 DP-2",
                RTX_4080,
                false
            ),
        ]
    );
    let portrait = &monitors[0];
    assert_eq!(
        (
            portrait.width,
            portrait.height,
            portrait.layout_size,
            portrait.left,
            portrait.top
        ),
        (1440, 2560, (1108, 1969), 0, 373),
        "the resolution, and the logical rectangle on the desktop"
    );
    let card = &monitors[1];
    assert_eq!(
        (
            card.gdi_name.as_str(),
            card.left,
            card.top,
            card.width,
            card.height,
            card.refresh_hz
        ),
        ("HDMI-A-2", 3668, 1080, 1920, 1080, 240)
    );
}

/// The sender's connectors with DP-2's monitor moved to the AMD card's DP-1, so both cards have a DP-1.
fn two_dp_1s() -> Vec<Connector> {
    let mut connectors = connectors(&sender_drm(), PCI_IDS);
    connectors[1].name = "DP-1".into();
    connectors[1].card = "0000:0c:00.0".into();
    connectors[1].graphics_card = RADEON.into();
    connectors
}

#[test]
fn two_graphics_cards_with_a_dp_1_are_told_apart_by_the_monitor() {
    let outputs = vec![
        output("DP-1", "Dell Inc. DELL AW2523HF", (0, 0), (1920, 1080), 360),
        output(
            "DP-1",
            "Dell Inc. Dell S2417DG",
            (1920, 0),
            (2560, 1440),
            144,
        ),
    ];
    let monitors = match_monitors(outputs, &two_dp_1s(), None);
    let found: Vec<_> = monitors.iter().map(|m| m.connector.as_str()).collect();
    assert_eq!(found, ["0000:0c:00.0 DP-1", "0000:01:00.0 DP-1"]);
}

#[test]
fn an_output_that_could_be_either_connector_gets_none() {
    let outputs = vec![output("DP-1", "", (0, 0), (1920, 1080), 60)];
    let monitor = &match_monitors(outputs, &two_dp_1s(), None)[0];
    assert_eq!(
        (monitor.connector.as_str(), monitor.graphics_card.as_str()),
        ("", "")
    );
    assert_eq!(
        (monitor.device_path.as_str(), monitor.name.as_str()),
        ("DP-1", "DP-1")
    );
}

#[test]
fn an_x11_output_with_another_name_is_matched_by_its_edid() {
    // Xorg's amdgpu driver names HDMI-A-2 `HDMI-A-1`, after its own count.
    let connectors = connectors(&sender_drm(), PCI_IDS);
    let elgato = connectors.iter().find(|c| c.name == "HDMI-A-2").unwrap();
    let mut hdmi = output("HDMI-A-1", "", (0, 0), (1920, 1080), 60);
    hdmi.edid = Some(elgato.edid.clone());
    let monitor = &match_monitors(vec![hdmi], &connectors, None)[0];
    assert_eq!(
        (
            monitor.name.as_str(),
            monitor.connector.as_str(),
            monitor.gdi_name.as_str()
        ),
        ("Elgato1080HDR", "0000:0c:00.0 HDMI-A-2", "HDMI-A-1")
    );
}

#[test]
fn an_output_with_no_connector_is_listed_by_its_own_name() {
    // A headless or nested desktop's outputs have no DRM connector.
    let outputs = vec![
        output("HEADLESS-1", "Headless output 1", (0, 0), (1920, 1080), 60),
        output("HEADLESS-2", "", (1920, 0), (1920, 1080), 60),
    ];
    let monitors = match_monitors(outputs, &connectors(&sender_drm(), PCI_IDS), None);
    let found: Vec<_> = monitors
        .iter()
        .map(|m| {
            (
                m.device_path.as_str(),
                m.name.as_str(),
                m.connector.as_str(),
                m.primary,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("HEADLESS-1", "Headless output 1", "", true),
            ("HEADLESS-2", "HEADLESS-2", "", false),
        ]
    );
}

#[test]
fn without_a_named_primary_output_the_one_at_the_origin_is_primary() {
    let primaries = |primary| {
        match_monitors(sender_outputs(), &[], primary)
            .iter()
            .map(|m| m.primary)
            .collect::<Vec<_>>()
    };
    assert_eq!(primaries(Some("DP-3")), [false, false, true, false]);
    // No monitor of the sender is at the origin.
    assert_eq!(primaries(None), [false; 4]);
    assert_eq!(primaries(Some("gone")), [false; 4]);
    let at_origin = vec![
        output("A", "", (0, 0), (1, 1), 60),
        output("B", "", (1, 0), (1, 1), 60),
    ];
    let primary: Vec<_> = match_monitors(at_origin, &[], Some("gone"))
        .iter()
        .map(|m| m.primary)
        .collect();
    assert_eq!(primary, [true, false]);
}

#[test]
fn two_monitors_with_the_same_serial_keep_apart_by_their_connector() {
    let mut connectors = connectors(&sender_drm(), PCI_IDS);
    connectors[0].edid = connectors[1].edid.clone();
    let outputs = vec![
        output("DP-1", "", (0, 0), (1920, 1080), 60),
        output("DP-2", "", (1920, 0), (1920, 1080), 60),
    ];
    let paths: Vec<_> = match_monitors(outputs, &connectors, None)
        .into_iter()
        .map(|m| m.device_path)
        .collect();
    assert_eq!(
        paths,
        [
            "DEL42B9 4GZY5P3 0000:01:00.0 DP-1",
            "DEL42B9 4GZY5P3 0000:01:00.0 DP-2"
        ]
    );
}

/// `CARDCAST_EXPECT_MONITORS` (set by CI's Xvfb and headless sway steps): the monitor list,
/// `<output> <width>x<height> <layout width>x<layout height>+<left>+<top>[ primary]` by output
/// name, joined by `; `. Skips when unset.
fn expected_monitors() -> Option<String> {
    std::env::var("CARDCAST_EXPECT_MONITORS").ok()
}

fn listed(monitors: &[Monitor]) -> String {
    let mut each: Vec<String> = monitors
        .iter()
        .map(|m| {
            format!(
                "{} {}x{} {}x{}+{}+{}{}",
                m.gdi_name,
                m.width,
                m.height,
                m.layout_size.0,
                m.layout_size.1,
                m.left,
                m.top,
                if m.primary { " primary" } else { "" }
            )
        })
        .collect();
    // The compositor's order is its own; sway announces its outputs last to first.
    each.sort();
    each.join("; ")
}

#[test]
fn desktop_lists_its_outputs() {
    let Some(expected) = expected_monitors() else {
        return;
    };
    let monitors = monitors().unwrap_or_else(|error| panic!("{error}"));
    eprintln!("{monitors:#?}");
    assert_eq!(listed(&monitors), expected);
}

/// `CARDCAST_PLUG_COMMAND` (CI's headless sway): a shell command that plugs in the output named
/// `CARDCAST_PLUGGED`; once a wake lists it, `CARDCAST_EXPECT_PLUGGED` is the monitor list. Skips
/// when unset.
#[test]
fn desktop_hotplug_wakes_the_display_watcher() {
    let (Ok(plug), Ok(plugged), Ok(expected)) = (
        std::env::var("CARDCAST_PLUG_COMMAND"),
        std::env::var("CARDCAST_PLUGGED"),
        std::env::var("CARDCAST_EXPECT_PLUGGED"),
    ) else {
        return;
    };
    let (woke, wakes) = mpsc::channel();
    let _watcher = DisplayWatcher::new(move || {
        let _ = woke.send(());
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let status = std::process::Command::new("sh")
        .args(["-c", &plug])
        .status()
        .unwrap();
    assert!(status.success(), "{plug} failed");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let monitors = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            wakes.recv_timeout(left).is_ok(),
            "the display watcher did not wake for {plugged}"
        );
        let monitors = monitors().unwrap();
        if monitors.iter().any(|m| m.gdi_name == plugged) {
            break monitors;
        }
    };
    assert_eq!(listed(&monitors), expected);
}
