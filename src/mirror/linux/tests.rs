use super::portal::{answered, request_path, select_options};
use super::*;
use crate::mirror::{Frame, Pixels, Release};
use zbus::zvariant::Value;

fn monitor(left: i32, top: i32, size: (u32, u32), layout_size: (u32, u32)) -> Monitor {
    Monitor {
        device_path: "DEL42B9 4GZY5P3".into(),
        gdi_name: "DP-2".into(),
        name: "DELL AW2523HF".into(),
        width: size.0,
        height: size.1,
        layout_size,
        refresh_hz: 500,
        primary: true,
        left,
        top,
        graphics_card: String::new(),
        container_id: String::new(),
        connector: String::new(),
    }
}

fn other(left: i32, size: (u32, u32)) -> Monitor {
    Monitor {
        device_path: "ELG1 card".into(),
        gdi_name: "HDMI-A-2".into(),
        name: "Elgato1080HDR".into(),
        primary: false,
        ..monitor(left, 0, size, size)
    }
}

/// What `started` decides with `source` among `monitors`, for a stream at `position` with `size`
/// whose `Start` gave token "new"; `opened` says whether it opened the stream.
fn decide(
    source: &Monitor,
    monitors: &[Monitor],
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    open: Result<(), Ended>,
) -> (Result<(), Ended>, Option<String>, bool) {
    let opened = std::cell::Cell::new(false);
    let capture = started(source, monitors, position, size, Some("new".into()), || {
        opened.set(true);
        open.map(|()| Box::new(Still) as Box<dyn Picture + Send>)
    });
    (
        capture.picture.map(|_| ()),
        capture.restore_token,
        opened.get(),
    )
}

/// A picture that never changes, for `started`'s tests.
struct Still;

impl Picture for Still {
    fn size(&self) -> (u32, u32) {
        (0, 0)
    }
    fn take_frame(&mut self) -> Result<Option<Frame>, Ended> {
        Ok(None)
    }
}

#[test]
fn the_stream_at_the_source_monitors_position_is_opened_and_its_token_kept() {
    let source = monitor(1920, 0, (2560, 1440), (1707, 960));
    // Another monitor of the same size doesn't matter: the position tells them apart.
    let monitors = [source.clone(), other(0, (1707, 960))];
    assert_eq!(
        decide(
            &source,
            &monitors,
            Some((1920, 0)),
            Some((1707, 960)),
            Ok(())
        ),
        (Ok(()), Some("new".into()), true)
    );
}

#[test]
fn another_monitors_stream_is_not_opened_and_its_token_dropped() {
    let source = monitor(1920, 0, (2560, 1440), (1707, 960));
    let monitors = [source.clone(), other(0, (1920, 1080))];
    let wrong = Err(not_the_source_monitor(&source));
    assert_eq!(
        decide(&source, &monitors, Some((0, 0)), Some((1707, 960)), Ok(())),
        (wrong.clone(), None, false)
    );
    assert_eq!(
        decide(&source, &monitors, None, Some((1920, 1080)), Ok(())),
        (wrong, None, false)
    );
}

#[test]
fn without_a_position_a_size_only_the_source_monitor_has_decides() {
    let source = monitor(1920, 0, (2560, 1440), (1707, 960));
    let monitors = [source.clone(), other(0, (1920, 1080))];
    for size in [(1707, 960), (2560, 1440)] {
        assert_eq!(
            decide(&source, &monitors, None, Some(size), Ok(())),
            (Ok(()), Some("new".into()), true)
        );
    }
}

#[test]
fn a_stream_that_could_be_another_monitor_is_refused_not_guessed() {
    let source = monitor(1920, 0, (1920, 1080), (1920, 1080));
    let monitors = [source.clone(), other(0, (1920, 1080))];
    let unknown = Err(cannot_tell(&source));
    // Two monitors of that size, then no position or size at all.
    assert_eq!(
        decide(&source, &monitors, None, Some((1920, 1080)), Ok(())),
        (unknown.clone(), None, false)
    );
    assert_eq!(
        decide(&source, &monitors[..1], None, None, Ok(())),
        (unknown, None, false)
    );
}

#[test]
fn a_stream_that_fails_to_open_keeps_the_new_token_so_a_restart_shows_no_dialog() {
    let source = monitor(0, 0, (1920, 1080), (1920, 1080));
    let failed = Ended::Restart("Could not open the screen capture stream.".into());
    assert_eq!(
        decide(
            &source,
            std::slice::from_ref(&source),
            Some((0, 0)),
            None,
            Err(failed.clone())
        ),
        (Err(failed), Some("new".into()), true)
    );
}

#[test]
fn a_refused_stream_says_why_and_names_the_source_monitor() {
    let source = monitor(0, 0, (1920, 1080), (1920, 1080));
    assert_eq!(
        not_the_source_monitor(&source),
        Ended::Stop(
            "The monitor shared in the screen-sharing dialog is not the source monitor. Press Start and share DELL AW2523HF.".into()
        )
    );
    assert_eq!(
        cannot_tell(&source),
        Ended::Stop(
            "cardcast can't tell which monitor the screen-sharing dialog shared, so it doesn't show it. Make the source monitor, DELL AW2523HF, the only one of its size, or use a desktop whose portal says where the shared monitor is.".into()
        )
    );
}

/// A frame whose buffer counts its releases.
fn held_frame(released: &std::sync::Arc<std::sync::atomic::AtomicU32>) -> Frame {
    let released = released.clone();
    Frame {
        width: 1,
        height: 1,
        fourcc: XRGB8888,
        pixels: Pixels::Memory {
            bytes: vec![0; 4],
            stride: 4,
        },
        release: Release::new(move || {
            released.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }),
    }
}

#[test]
fn a_frame_gives_its_buffer_back_once_when_dropped() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let released = std::sync::Arc::new(AtomicU32::new(0));
    let frame = held_frame(&released);
    assert_eq!(released.load(Ordering::Relaxed), 0, "released while held");
    drop(frame);
    assert_eq!(released.load(Ordering::Relaxed), 1);
    // A copy holds nothing.
    drop(Release::default());
}

#[test]
fn a_frame_replaced_before_it_is_taken_gives_its_buffer_back() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let (first, second) = (
        std::sync::Arc::new(AtomicU32::new(0)),
        std::sync::Arc::new(AtomicU32::new(0)),
    );
    let mut latest = super::stream::Latest::default();
    latest.put(held_frame(&first));
    latest.put(held_frame(&second));
    assert_eq!(
        first.load(Ordering::Relaxed),
        1,
        "the replaced frame is still held"
    );
    // The taken frame stays held until the present drops it.
    let taken = latest.frame.take();
    assert_eq!(second.load(Ordering::Relaxed), 0);
    drop(taken);
    assert_eq!(second.load(Ordering::Relaxed), 1);
}

#[test]
fn pipewire_byte_orders_become_drm_fourccs() {
    assert_eq!(fourcc(VideoFormat::BGRx), Some(XRGB8888));
    assert_eq!(fourcc(VideoFormat::BGRA), Some(ARGB8888));
    assert_eq!(fourcc(VideoFormat::RGBx), Some(XBGR8888));
    assert_eq!(fourcc(VideoFormat::RGBA), Some(ABGR8888));
    assert_eq!(fourcc(VideoFormat::I420), None);
    // drm_fourcc.h: fourcc_code('X', 'R', '2', '4').
    assert_eq!(XRGB8888, 0x3432_5258);
}

#[test]
fn rows_past_the_buffer_are_refused() {
    let bytes = [0_u8; 100];
    assert_eq!(rows(&bytes, 20, 16, 5).map(<[u8]>::len), Some(80));
    assert_eq!(rows(&bytes, 21, 16, 5), None);
    assert_eq!(rows(&bytes, u32::MAX, 16, 5), None);
}

#[test]
fn the_portal_is_asked_for_one_monitor_with_the_pointer_kept_until_revoked() {
    let options = select_options(5, 7, Some("token"));
    assert_eq!(options.get("types"), Some(&Value::from(1_u32)));
    assert_eq!(options.get("multiple"), Some(&Value::from(false)));
    assert_eq!(options.get("cursor_mode"), Some(&Value::from(2_u32)));
    assert_eq!(options.get("persist_mode"), Some(&Value::from(2_u32)));
    assert_eq!(
        options.get("restore_token"),
        Some(&Value::from("token".to_owned()))
    );
}

#[test]
fn an_older_portal_gets_only_the_options_it_knows() {
    // Before version 4 no persistence; without Embedded among its cursor modes, the default.
    let options = select_options(3, 1, Some("token"));
    assert!(!options.contains_key("cursor_mode"));
    assert!(!options.contains_key("persist_mode"));
    assert!(!options.contains_key("restore_token"));
    assert!(!select_options(5, 7, None).contains_key("restore_token"));
}

#[test]
fn the_portal_answers_on_a_path_named_after_the_sender_and_token() {
    assert_eq!(
        request_path(":1.42", "cardcast3"),
        "/org/freedesktop/portal/desktop/request/1_42/cardcast3"
    );
}

#[test]
fn a_cancelled_dialog_stops_the_mirror() {
    assert_eq!(answered(0), Ok(()));
    assert_eq!(
        answered(1),
        Err(Ended::Stop(
            "Screen sharing was cancelled. Press Start and share the source monitor.".into()
        ))
    );
    assert!(matches!(answered(2), Err(Ended::Stop(_))));
}

#[test]
fn the_pointer_is_blended_over_the_frame_and_cut_at_its_edges() {
    // A 2x2 frame of dark grey, and a 2x2 pointer hanging off the top-left corner by one pixel:
    // only its bottom-right pixel, half-transparent premultiplied white, lands (at 0, 0).
    let mut frame = [0x40_u8; 16];
    let argb = [0xFFFF_0000, 0xFFFF_0000, 0xFFFF_0000, 0x8080_8080];
    draw_pointer(
        &mut frame,
        (2, 2),
        &Pointer {
            left: -1,
            top: -1,
            width: 2,
            height: 2,
            argb: &argb,
        },
    );
    // 0x80 + 0x40 * 0x7F / 0xFF = 0x80 + 0x1F.
    assert_eq!(frame[..4], [0x9F, 0x9F, 0x9F, 0x40]);
    assert!(frame[4..].iter().all(|&byte| byte == 0x40));
}

/// The colour of the pixel at (`x`, `y`) of a frame in memory, as `[red, green, blue]`.
fn rgb_at(frame: &Frame, x: u32, y: u32) -> Option<[u8; 3]> {
    let Pixels::Memory { bytes, stride } = &frame.pixels else {
        return None;
    };
    let at = usize::try_from(y * stride + x * 4).ok()?;
    let [first, second, third] = bytes.get(at..at + 3)?.try_into().ok()?;
    match frame.fourcc {
        XRGB8888 | ARGB8888 => Some([third, second, first]),
        XBGR8888 | ABGR8888 => Some([first, second, third]),
        _ => None,
    }
}

/// CI's desktops (headless sway with xdg-desktop-portal-wlr, Xvfb): `CARDCAST_CAPTURE` names the
/// output to capture and `CARDCAST_EXPECT_COLOUR` (`#rrggbb`) the colour it shows; a frame's pixel
/// a quarter in from its top-left must have it (X servers start the pointer in the middle). Skips
/// when unset.
#[test]
fn desktop_captures_a_frame_of_the_source_monitor() {
    let (Ok(output), Ok(colour)) = (
        std::env::var("CARDCAST_CAPTURE"),
        std::env::var("CARDCAST_EXPECT_COLOUR"),
    ) else {
        return;
    };
    let expected: Vec<u8> = (1..7)
        .step_by(2)
        .map(|at| u8::from_str_radix(&colour[at..at + 2], 16).unwrap())
        .collect();
    let monitors = crate::guard::monitors().unwrap();
    let source = monitors
        .iter()
        .find(|monitor| monitor.gdi_name == output)
        .unwrap_or_else(|| panic!("no output {output} in {monitors:#?}"));
    // Linear and implicit layouts are offered too, so the DMA-BUF offer is negotiated; a desktop
    // without DMA-BUF (sway on pixman) falls back to shared memory.
    let mut capture = capture(source, &monitors, None, &[0, (1 << 56) - 1]);
    let picture = capture
        .picture
        .as_mut()
        .unwrap_or_else(|ended| panic!("{ended:?}"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let frame = loop {
        if let Some(frame) = picture.take_frame().unwrap() {
            break frame;
        }
        assert!(std::time::Instant::now() < deadline, "no frame came");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    eprintln!(
        "frame {}x{} fourcc {:08X}, size {:?}, restore token {:?}",
        frame.width,
        frame.height,
        frame.fourcc,
        picture.size(),
        capture.restore_token
    );
    // The portal hands a token only for a persistent permission (`persist_mode`).
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        assert!(
            capture.restore_token.is_some(),
            "the portal gave no restore token"
        );
    }
    assert_eq!(
        (frame.width, frame.height),
        (source.width, source.height),
        "the frame is not the source monitor's size"
    );
    assert_eq!(
        rgb_at(&frame, frame.width / 4, frame.height / 4).map(Vec::from),
        Some(expected),
        "the frame is not {colour}"
    );
}
