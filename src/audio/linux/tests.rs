use super::*;
use crate::audio::{RelayReport, SoundOutput, card_output};

/// `pw-dump` on the sender (CachyOS, PipeWire 1.6.12), recorded 2026-11-28 and cut to the core's
/// version, the audio devices, the sinks and one source: the HD60 X's sink on the AMD integrated
/// graphics' HDMI audio (`0000:0c:00.1`), and a USB headset.
const SENDER_DUMP: &str = include_str!("../../../tests/fixtures/sender-pw-dump.json");

const HEADSET: &str =
    "alsa_output.usb-BEACN_BEACN_Studio_0031240800178C-00.Basic__Headphones__sink";
const CARD: &str = "alsa_output.pci-0000_0c_00.1.hdmi-stereo";

#[test]
fn the_senders_sinks_are_its_playback_devices() {
    let sinks = sinks(SENDER_DUMP).unwrap();
    let found: Vec<_> = sinks
        .iter()
        .map(|sink| {
            (
                sink.id.as_str(),
                sink.name.as_str(),
                sink.description.as_str(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (HEADSET, "BEACN Studio Headphones", "BEACN Studio"),
            (
                CARD,
                "Radeon High Definition Audio Controller Digital Stereo (HDMI) [Elgato1080HDR]",
                "Elgato1080HDR"
            ),
        ]
    );
}

#[test]
fn automatic_finds_the_card_displays_sink_by_its_graphics_card_and_name() {
    let sinks = sinks(SENDER_DUMP).unwrap();
    // The card display as guard::monitors lists it: on graphics card 0000:0c:00.0.
    let container = container_id("0000:0c:00.0", "Elgato1080HDR");
    let found = card_output(
        &sinks,
        HEADSET,
        &SoundOutput::Automatic,
        &container,
        "Elgato1080HDR",
    );
    assert_eq!(
        found.map(|sink| sink.map(|sink| sink.id.as_str())),
        Ok(Some(CARD))
    );
}

/// A `pw-dump` with one sound card per sink: (node.name, node.nick, the card's device.bus-path).
fn dump(sinks: &[(&str, &str, &str)]) -> String {
    let objects: Vec<String> = sinks
        .iter()
        .enumerate()
        .flat_map(|(index, (name, nick, bus_path))| {
            [
                format!(
                    r#"{{"id": {index}, "type": "PipeWire:Interface:Device", "info": {{"props": {{"device.bus-path": "{bus_path}", "media.class": "Audio/Device"}}}}}}"#
                ),
                format!(
                    r#"{{"id": {}, "type": "PipeWire:Interface:Node", "info": {{"props": {{"device.id": {index}, "media.class": "Audio/Sink", "node.name": "{name}", "node.nick": "{nick}", "node.description": "{nick} sink"}}}}}}"#,
                    100 + index
                ),
            ]
        })
        .collect();
    format!("[{}]", objects.join(","))
}

fn automatic(dump: &str, graphics_card: &str) -> Result<Option<String>, RelayReport> {
    let sinks = sinks(dump).unwrap();
    card_output(
        &sinks,
        "speakers",
        &SoundOutput::Automatic,
        &container_id(graphics_card, "Elgato1080HDR"),
        "Elgato1080HDR",
    )
    .map(|sink| sink.map(|sink| sink.id.clone()))
}

#[test]
fn two_capture_cards_are_told_apart_by_their_graphics_card() {
    let dump = dump(&[
        ("speakers", "Ryzen HD Audio Controller", "pci-0000:0c:00.6"),
        ("radeon-hdmi", "Elgato1080HDR", "pci-0000:0c:00.1"),
        ("nvidia-hdmi", "Elgato1080HDR", "pci-0000:01:00.1"),
    ]);
    assert_eq!(
        automatic(&dump, "0000:01:00.0"),
        Ok(Some("nvidia-hdmi".into()))
    );
    assert_eq!(
        automatic(&dump, "0000:0c:00.0"),
        Ok(Some("radeon-hdmi".into()))
    );
}

#[test]
fn two_sinks_with_the_name_on_one_graphics_card_are_not_found() {
    let dump = dump(&[
        ("speakers", "Speakers", "pci-0000:0c:00.6"),
        ("hdmi-1", "Elgato1080HDR", "pci-0000:0c:00.1"),
        ("hdmi-2", "Elgato1080HDR", "pci-0000:0c:00.1"),
    ]);
    assert_eq!(
        automatic(&dump, "0000:0c:00.0"),
        Err(RelayReport::NoSoundOutput)
    );
}

#[test]
fn the_graphics_cards_sink_for_another_monitor_is_not_the_sound_output() {
    // Its HDMI audio plays to the other monitor, so only the name could find the capture card.
    let dump = dump(&[
        ("speakers", "Speakers", "pci-0000:0c:00.6"),
        ("hdmi", "DELL AW2523HF", "pci-0000:0c:00.1"),
    ]);
    assert_eq!(
        automatic(&dump, "0000:0c:00.0"),
        Err(RelayReport::NoSoundOutput)
    );
}

#[test]
fn a_usb_sink_or_a_graphics_card_without_pci_has_no_container() {
    assert_eq!(
        container_id("0000:09:00.0-usb-0:5.4:1.2", "Elgato1080HDR"),
        ""
    );
    assert_eq!(container_id("card0", "Elgato1080HDR"), "");
    assert_eq!(container_id("0000:0c:00.0", " "), "");
    assert_eq!(
        container_id("0000:0c:00.1", " Elgato1080HDR "),
        "0000:0c:00 Elgato1080HDR"
    );
}

/// The loudest sample `sink` plays in one second, recorded from its monitor with `pw-record`.
fn peak(sink: &str) -> i16 {
    let file = std::env::temp_dir().join(format!("cardcast-{sink}-{}.wav", std::process::id()));
    let status = std::process::Command::new("timeout")
        .args([
            "1",
            "pw-record",
            "--target",
            sink,
            "-P",
            "{ stream.capture.sink=true }",
        ])
        .args(["--format", "s16", "--rate", "48000", "--channels", "2"])
        .arg(&file)
        .status()
        .unwrap();
    // timeout ends pw-record, so it exits 124.
    assert_eq!(status.code(), Some(124), "pw-record {sink}");
    let wav = std::fs::read(&file).unwrap();
    let data = wav.windows(4).position(|chunk| chunk == b"data").unwrap() + 8;
    let samples: Vec<i16> = wav[data..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| i16::from_le_bytes(*sample))
        .collect();
    eprintln!("{sink}: {} samples", samples.len());
    assert!(samples.len() > 48_000, "recorded {sink} for a second");
    samples
        .iter()
        .map(|sample| sample.saturating_abs())
        .max()
        .unwrap()
}

/// Plays a tone into `sink` until dropped.
struct Tone(std::process::Child);

impl Tone {
    fn play(sink: &str) -> Self {
        // Thirty seconds of a square wave of about 440 Hz as a 16-bit stereo WAV at 48 kHz.
        let frames: u32 = 48_000 * 30;
        let mut wav = Vec::new();
        for (chunk, size) in [(&b"RIFF"[..], 36 + frames * 4), (b"WAVEfmt ", 16)] {
            wav.extend_from_slice(chunk);
            wav.extend_from_slice(&size.to_le_bytes());
        }
        // PCM, 2 channels, 48000 Hz, 192000 bytes a second, 4 bytes a frame, 16 bits.
        for field in [1u16, 2, 48_000, 0, 0xEE00, 2, 4, 16] {
            wav.extend_from_slice(&field.to_le_bytes());
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 4).to_le_bytes());
        for frame in 0..frames {
            let sample: i16 = if frame / 55 % 2 == 0 { 8000 } else { -8000 };
            wav.extend_from_slice(&[sample.to_le_bytes(), sample.to_le_bytes()].concat());
        }
        let file = std::env::temp_dir().join(format!("cardcast-tone-{}.wav", std::process::id()));
        std::fs::write(&file, wav).unwrap();
        let player = std::process::Command::new("pw-play")
            .args(["--target", sink])
            .arg(&file)
            .spawn()
            .unwrap();
        Self(player)
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// CI (`CARDCAST_NULL_SINKS`): PipeWire and WirePlumber run with two null sinks, `cardcast-source`
/// and `cardcast-card` (`node.nick` "cardcast card", the card display's name). Skips when unset.
#[test]
fn null_sinks_relay_the_audio_source_to_the_sound_output() {
    if std::env::var_os("CARDCAST_NULL_SINKS").is_none() {
        return;
    }
    const LOUD: i16 = 4000;
    const QUIET: i16 = 100;
    let (source, card) = ("cardcast-source", "cardcast-card");
    let devices = crate::audio::playback_devices().unwrap();
    eprintln!("{devices:#?}");
    for sink in [source, card] {
        assert!(
            devices.iter().any(|device| device.id == sink),
            "{sink} not listed"
        );
    }
    let _tone = Tone::play(source);
    assert!(peak(source) > LOUD, "the tone plays into the audio source");
    assert!(
        peak(card) < QUIET,
        "the sound output is silent before Start"
    );

    let mut relay = crate::audio::new_relay(|| {});
    relay.start(source, &SoundOutput::Automatic, "", "cardcast card");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let card_peak = peak(card);
        eprintln!("sound output peak {card_peak}");
        if card_peak > LOUD {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no audio reached the sound output"
        );
    }
    assert!(peak(source) > LOUD, "the audio source still plays");
    assert_eq!(relay.take_report(), None);
    relay.stop();
    assert!(peak(card) < QUIET, "Stop ends the relay");
    relay.start(source, &SoundOutput::NoAudio, "", "cardcast card");
    assert!(peak(card) < QUIET, "No audio relays nothing");

    relay.start(source, &SoundOutput::Automatic, "", "no such card");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let report = loop {
        if let Some(report) = relay.take_report() {
            break report;
        }
        assert!(std::time::Instant::now() < deadline, "no report");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(report, RelayReport::NoSoundOutput);
    relay.stop();
}

#[test]
fn the_loopback_records_the_audio_sources_monitor_and_stays_on_its_sinks() {
    assert_eq!(
        loopback_args("speakers", r#"odd "name"\"#),
        [
            "--name",
            "cardcast",
            "--capture-props",
            r#"stream.capture.sink=true target.object="speakers" node.dont-reconnect=true node.dont-fallback=true"#,
            "--playback-props",
            r#"target.object="odd \"name\"\\" node.dont-reconnect=true node.dont-fallback=true"#,
        ]
    );
}

#[test]
fn pipewire_or_wireplumber_below_the_minimum_refuses_the_relay() {
    let wireplumber = |version: &str| {
        format!(
            "wireplumber\nCompiled with libwireplumber {version}\nLinked with libwireplumber {version}\n"
        )
    };
    // Ubuntu 24.04 (CI) and the sender.
    assert_eq!(supported("1.0.5", &wireplumber("0.4.17")), Ok(()));
    assert_eq!(supported("1.6.12", &wireplumber("0.5.19")), Ok(()));
    assert_eq!(supported("0.3.44", &wireplumber("0.4.12")), Ok(()));
    assert_eq!(
        supported("0.3.43", &wireplumber("0.5.19")),
        Err("The audio relay needs PipeWire 0.3.44 or newer; this is PipeWire 0.3.43.".into())
    );
    // Ubuntu 22.04.
    assert_eq!(
        supported("0.3.48", &wireplumber("0.4.8")),
        Err("The audio relay needs WirePlumber 0.4.12 or newer; this is WirePlumber 0.4.8.".into())
    );
    // WirePlumber before 0.4.12 does not print the version it runs.
    for unknown in ["wireplumber\n", ""] {
        assert_eq!(
            supported("1.0.5", unknown),
            Err(
                "The audio relay needs WirePlumber 0.4.12 or newer; is WirePlumber installed?"
                    .into()
            )
        );
    }
}

#[test]
fn the_pipewire_version_comes_from_the_dump() {
    assert_eq!(pipewire_version(SENDER_DUMP), "1.6.12");
    assert_eq!(pipewire_version("[]"), "");
}

/// A `pw-dump` with the audio source and output sinks, the loopback (process 4242) and `links`
/// as (output node, input node): 10 the audio source, 20 the output, 31 and 32 the loopback's
/// capture and playback streams, 40 another program's stream.
fn loopback_dump(links: &[(u32, u32)]) -> String {
    let node = |id: u32, client: u32, class: &str, name: &str| {
        format!(
            r#"{{"id": {id}, "type": "PipeWire:Interface:Node", "info": {{"props": {{"client.id": {client}, "media.class": "{class}", "node.name": "{name}"}}}}}}"#
        )
    };
    let client = |id: u32, pid: u32| {
        format!(
            r#"{{"id": {id}, "type": "PipeWire:Interface:Client", "info": {{"props": {{"application.process.id": {pid}}}}}}}"#
        )
    };
    let mut objects = vec![
        client(5, 1000),
        client(30, 4242),
        client(41, 7000),
        node(10, 5, "Audio/Sink", "speakers"),
        node(20, 5, "Audio/Sink", "capture-card"),
        node(31, 30, "Stream/Input/Audio", "input.cardcast"),
        node(32, 30, "Stream/Output/Audio", "output.cardcast"),
        node(40, 41, "Stream/Input/Audio", "recorder"),
    ];
    objects.extend(links.iter().map(|(output, input)| {
        format!(
            r#"{{"id": {}, "type": "PipeWire:Interface:Link", "info": {{"output-node-id": {output}, "input-node-id": {input}, "state": "active"}}}}"#,
            1000 + output * 100 + input
        )
    }));
    format!("[{}]", objects.join(","))
}

#[test]
fn the_loopback_is_linked_only_from_the_audio_source_and_to_the_sound_output() {
    let linked = |links: &[(u32, u32)]| {
        loopback_linked(&loopback_dump(links), 4242, "speakers", "capture-card")
    };
    // Each of the two channels has its own link.
    assert!(linked(&[(10, 31), (10, 31), (32, 20), (32, 20)]));
    assert!(!linked(&[]), "no WirePlumber links nothing");
    assert!(!linked(&[(10, 31)]), "nothing plays on the sound output");
    assert!(!linked(&[(32, 20)]), "nothing records the audio source");
    assert!(!linked(&[(10, 40), (32, 20)]), "another program records it");
    assert!(!linked(&[(20, 31), (32, 10)]), "the sinks swapped");
    assert!(
        !linked(&[(10, 31), (32, 10)]),
        "played back into the audio source"
    );
    assert!(
        !loopback_linked(
            &loopback_dump(&[(10, 31), (32, 20)]),
            9999,
            "speakers",
            "capture-card"
        ),
        "another process's loopback"
    );
}

#[test]
fn a_loopback_whose_cardcast_is_gone_does_not_start() {
    let spawn = |parent: u32| {
        let mut command = Command::new("true");
        die_with(&mut command, parent);
        command.spawn().and_then(|mut child| child.wait())
    };
    assert!(spawn(std::process::id()).unwrap().success());
    assert!(
        spawn(u32::MAX).is_err(),
        "reparented: cardcast died before prctl"
    );
}

#[test]
fn a_loopback_that_cannot_start_is_reported_in_plain_words() {
    assert_eq!(
        spawn_failed(&std::io::Error::from(std::io::ErrorKind::NotFound)),
        "Could not start the audio relay: pw-loopback is not installed. Install PipeWire's tools (see README, Sound)."
    );
    assert_eq!(
        spawn_failed(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        "Could not start the audio relay: pw-loopback did not run."
    );
}

/// CI (`CARDCAST_NO_WIREPLUMBER`): PipeWire runs with the null sinks but no WirePlumber, so
/// nothing links the loopback. Skips when unset.
#[test]
fn without_wireplumber_the_unlinked_relay_is_reported() {
    if std::env::var_os("CARDCAST_NO_WIREPLUMBER").is_none() {
        return;
    }
    let mut relay = crate::audio::new_relay(|| {});
    relay.start(
        "cardcast-source",
        &SoundOutput::Automatic,
        "",
        "cardcast card",
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let report = loop {
        if let Some(report) = relay.take_report() {
            break report;
        }
        assert!(std::time::Instant::now() < deadline, "no report");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(report, RelayReport::Failing(NOT_LINKED.into()));
    relay.stop();
}

/// CI (`CARDCAST_PIPEWIRE_STOPPED`): the PipeWire daemon is stopped with SIGSTOP, so `pw-dump`
/// never gets an answer. Skips when unset.
#[test]
fn a_stopped_pipewire_is_reported_after_the_timeout() {
    if std::env::var_os("CARDCAST_PIPEWIRE_STOPPED").is_none() {
        return;
    }
    let started = std::time::Instant::now();
    assert_eq!(crate::audio::playback_devices(), Err(NO_ANSWER.to_owned()));
    let waited = started.elapsed();
    eprintln!("waited {waited:?}");
    assert!(
        waited < DUMP_TIMEOUT + std::time::Duration::from_secs(2),
        "waited {waited:?}"
    );
}
