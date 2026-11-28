use super::portal::{answered, request_path, select_options};
use super::*;
use crate::mirror::linux::LinuxMirror;
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
        "frame {}x{} fourcc {:08X}, restore token given: {}",
        frame.width,
        frame.height,
        frame.fourcc,
        capture.restore_token.is_some()
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

#[test]
fn the_mirror_shows_without_focus_on_each_desktop() {
    use super::present::{Shown, shown_on};
    assert_eq!(shown_on(Some(Desktop::X11)), Ok(Shown::OverrideRedirect));
    for desktop in [
        Desktop::PlasmaWayland,
        Desktop::Sway,
        Desktop::Hyprland,
        Desktop::OtherWayland,
    ] {
        assert_eq!(shown_on(Some(desktop)), Ok(Shown::LayerShell));
    }
    assert_eq!(shown_on(Some(Desktop::GnomeWayland)), Ok(Shown::Fullscreen));
    assert!(matches!(shown_on(None), Err(Ended::Stop(_))));
}

#[test]
fn each_capture_layout_is_drawn_from_its_byte_order() {
    use super::gpu::texture_format;
    use wgpu::TextureFormat::{Bgra8Unorm, Rgba8Unorm};
    assert_eq!(texture_format(XRGB8888), Some(Bgra8Unorm));
    assert_eq!(texture_format(ARGB8888), Some(Bgra8Unorm));
    assert_eq!(texture_format(XBGR8888), Some(Rgba8Unorm));
    assert_eq!(texture_format(ABGR8888), Some(Rgba8Unorm));
    assert_eq!(texture_format(u32::from_le_bytes(*b"NV12")), None);
}

#[test]
fn only_single_plane_copyable_dma_buf_layouts_are_offered() {
    use super::gpu::importable;
    use ash::vk::{DrmFormatModifierPropertiesEXT, FormatFeatureFlags};
    let layout = |modifier, planes, features| DrmFormatModifierPropertiesEXT {
        drm_format_modifier: modifier,
        drm_format_modifier_plane_count: planes,
        drm_format_modifier_tiling_features: features,
    };
    let copyable = FormatFeatureFlags::TRANSFER_SRC | FormatFeatureFlags::SAMPLED_IMAGE;
    assert_eq!(
        importable(&[
            layout(0, 1, copyable),
            // A compressed layout with its metadata in a second plane.
            layout(7, 2, copyable),
            layout(9, 1, FormatFeatureFlags::SAMPLED_IMAGE),
            layout(11, 1, copyable),
        ]),
        [0, 11]
    );
}

/// A picture that counts its drops, for the capture thread's tests.
struct Counted(std::sync::Arc<std::sync::atomic::AtomicU32>);

impl Picture for Counted {
    fn take_frame(&mut self) -> Result<Option<Frame>, Ended> {
        Ok(None)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn stop_does_not_wait_for_the_dialog_and_a_late_capture_keeps_its_token_but_not_its_stream() {
    use super::present::{Token, start_capture, wait_for_picture};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};
    let token = Arc::new(Mutex::new(Token::default()));
    let woken = Arc::new(AtomicU32::new(0));
    let dropped = Arc::new(AtomicU32::new(0));
    // The dialog stays open until `answer` sends.
    let (answer, answered) = mpsc::channel::<()>();
    let captured = {
        let (dropped, woken) = (dropped.clone(), woken.clone());
        start_capture(
            move |saved| {
                assert_eq!(saved, None);
                answered.recv().unwrap();
                Capture {
                    picture: Ok(Box::new(Counted(dropped))),
                    restore_token: Some("new".into()),
                }
            },
            token.clone(),
            Arc::new(move || {
                woken.fetch_add(1, Ordering::Relaxed);
            }),
        )
    };
    let stopped = Instant::now();
    let quit = AtomicBool::new(true);
    assert!(matches!(wait_for_picture(&captured, &quit), Ok(None)));
    assert!(stopped.elapsed() < Duration::from_secs(1), "Stop waited");
    // The mirror thread is gone; then the user answers the dialog.
    drop(captured);
    answer.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while dropped.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline, "the late capture was kept");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(woken.load(Ordering::Relaxed), 1);
    assert_eq!(
        super::present::take_unsaved(&token),
        Some(Some("new".into()))
    );
    assert_eq!(super::present::take_unsaved(&token), None, "saved twice");
}

#[test]
fn an_unchanged_token_is_not_saved_again() {
    use super::present::{Token, start_capture, wait_for_picture};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    let token = Arc::new(Mutex::new(Token::default()));
    // X11 keeps the token it was given: none.
    let captured = start_capture(
        |_| Capture {
            picture: Ok(Box::new(Still)),
            restore_token: None,
        },
        token.clone(),
        Arc::new(|| {}),
    );
    assert!(matches!(
        wait_for_picture(&captured, &AtomicBool::new(false)),
        Ok(Some(_))
    ));
    assert_eq!(super::present::take_unsaved(&token), None);
}

/// The colour at (`x`, `y`) of the card display's output, as the desktop shows it: `grim` (the
/// compositor's screencopy) on Wayland, `GetImage` of the root window on X11.
fn shown_on_output(card_display: &Monitor, x: u32, y: u32) -> [u8; 3] {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let shot = std::process::Command::new("grim")
            .args(["-t", "ppm", "-o", &card_display.gdi_name, "-"])
            .output()
            .unwrap();
        assert!(shot.status.success(), "grim: {shot:?}");
        // P6: "P6", width, height and maximum value, each followed by one whitespace byte.
        let mut header = shot.stdout.splitn(5, u8::is_ascii_whitespace);
        let fields: Vec<&[u8]> = header.by_ref().take(4).collect();
        let width: u32 = std::str::from_utf8(fields[1]).unwrap().parse().unwrap();
        let pixels = header.next().unwrap();
        let at = usize::try_from((y * width + x) * 3).unwrap();
        pixels[at..at + 3].try_into().unwrap()
    } else {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
        let (connection, screen) = x11rb::connect(None).unwrap();
        let root = connection.setup().roots[screen].root;
        let image = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                root,
                i16::try_from(card_display.left + i32::try_from(x).unwrap()).unwrap(),
                i16::try_from(card_display.top + i32::try_from(y).unwrap()).unwrap(),
                1,
                1,
                !0,
            )
            .unwrap()
            .reply()
            .unwrap();
        // XRGB8888, little-endian: blue, green, red.
        [image.data[2], image.data[1], image.data[0]]
    }
}

/// A window of `colour` over the source monitor, open while the returned connection is.
fn paint_x11_source_monitor(
    source_monitor: &Monitor,
    colour: [u8; 3],
) -> x11rb::rust_connection::RustConnection {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};
    let (connection, screen) = x11rb::connect(None).unwrap();
    let root = connection.setup().roots[screen].root;
    let window = connection.generate_id().unwrap();
    let [red, green, blue] = colour.map(u32::from);
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            i16::try_from(source_monitor.left).unwrap(),
            i16::try_from(source_monitor.top).unwrap(),
            u16::try_from(source_monitor.layout_size.0).unwrap(),
            u16::try_from(source_monitor.layout_size.1).unwrap(),
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(red << 16 | green << 8 | blue),
        )
        .unwrap()
        .check()
        .unwrap();
    connection.map_window(window).unwrap().check().unwrap();
    connection
}

/// CI's desktops (headless sway with xdg-desktop-portal-wlr, X.Org with two dummy outputs): the
/// mirror runs from output `CARDCAST_MIRROR_FROM`, showing `CARDCAST_EXPECT_COLOUR` (`#rrggbb`),
/// to output `CARDCAST_MIRROR_TO`, which must then show that colour where it showed another, and
/// count its presents; with `CARDCAST_EXPECT_HZ`, no more than one per refresh at that rate. X11
/// has no background per output, so there the test paints the source monitor
/// `CARDCAST_PAINT_SOURCE`. Skips when unset.
#[test]
fn desktop_mirrors_the_source_monitor_on_the_card_display() {
    use crate::mirror::Mirror;
    use std::time::{Duration, Instant};
    let (Ok(from), Ok(to), Ok(colour)) = (
        std::env::var("CARDCAST_MIRROR_FROM"),
        std::env::var("CARDCAST_MIRROR_TO"),
        std::env::var("CARDCAST_EXPECT_COLOUR"),
    ) else {
        return;
    };
    let rgb = |colour: &str| -> [u8; 3] {
        [1, 3, 5].map(|at| u8::from_str_radix(&colour[at..at + 2], 16).unwrap())
    };
    let expected = rgb(&colour);
    let monitors = crate::guard::monitors().unwrap();
    let named = |name: &str| {
        monitors
            .iter()
            .find(|monitor| monitor.gdi_name == name)
            .unwrap_or_else(|| panic!("no output {name} in {monitors:#?}"))
            .clone()
    };
    let (source, card) = (named(&from), named(&to));
    let _painted = std::env::var("CARDCAST_PAINT_SOURCE")
        .ok()
        .map(|paint| paint_x11_source_monitor(&source, rgb(&paint)));
    // A quarter in from the top-left and from the bottom-right (the pointer starts in the middle):
    // a mirror of the whole desktop, not just the source monitor, shows the card display's own
    // old picture at one of them.
    let points = [
        (card.width / 4, card.height / 4),
        (card.width * 3 / 4, card.height * 3 / 4),
    ];
    let shown_now = || points.map(|(x, y)| shown_on_output(&card, x, y));
    let before = shown_now();
    eprintln!("{to} shows {before:02x?} before the mirror");
    assert!(!before.contains(&expected), "{to} already shows {colour}");

    let mut mirror = LinuxMirror::new(|| {}, None);
    mirror.start(&source, &card).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !mirror.shows_picture() {
        assert!(
            mirror.is_running(),
            "the mirror ended: {:?}",
            mirror.take_end()
        );
        assert!(Instant::now() < deadline, "the mirror never showed");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Settled, then counted over two seconds.
    std::thread::sleep(Duration::from_secs(1));
    let first = mirror.presents().unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let rate = (mirror.presents().unwrap() - first) as f64 / 2.0;
    let shown = shown_now();
    eprintln!(
        "{to} ({} Hz) shows {shown:02x?} with the mirror; {rate:.1} presents a second; restore token saved: {}",
        card.refresh_hz,
        matches!(mirror.take_restore_token(), Some(Some(_)))
    );
    assert!(
        mirror.is_running(),
        "the mirror ended: {:?}",
        mirror.take_end()
    );
    assert_eq!(shown, [expected; 2], "{to} does not show {from}'s {colour}");
    assert!(rate >= 10.0, "{rate:.1} presents a second");
    // Fifo: one present per refresh of the card display, never more.
    if let Ok(refresh) = std::env::var("CARDCAST_EXPECT_HZ") {
        let refresh: f64 = refresh.parse().unwrap();
        assert!(
            rate <= refresh * 1.1,
            "{rate:.1} presents a second on a {refresh} Hz card display"
        );
    }
    mirror.stop();
    assert!(!mirror.is_running());
}

#[test]
fn the_mirror_shader_is_valid_wgsl() {
    let module = wgpu::naga::front::wgsl::parse_str(super::gpu::SHADER).unwrap();
    wgpu::naga::valid::Validator::new(
        wgpu::naga::valid::ValidationFlags::all(),
        wgpu::naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap();
}

#[test]
fn x11_presents_keep_to_the_card_displays_refresh_and_catch_up_without_a_burst() {
    use super::present::next_present;
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let period = Duration::from_secs(1) / 60;
    // Steady: one refresh after the last planned present, even if the loop ran early.
    assert_eq!(next_present(start, start, period), start + period);
    assert_eq!(
        next_present(start, start + period / 2, period),
        start + period
    );
    // After a stall of several refreshes, the next present is now, not a run of missed ones.
    let late = start + period * 5;
    assert_eq!(next_present(start, late, period), late);
}

#[test]
fn x11_refresh_period_is_the_exact_mode_timing_and_skips_no_refresh_at_59_94_hz() {
    use super::present::next_present;
    use super::screen::refresh_period;
    use std::time::{Duration, Instant};
    use x11rb::protocol::randr::{ModeFlag, ModeInfo};
    // 1920x1080 at 59.94 Hz: 148.35 MHz over 2200 x 1125.
    let mode = ModeInfo {
        dot_clock: 148_350_000,
        htotal: 2200,
        vtotal: 1125,
        ..ModeInfo::default()
    };
    let period = refresh_period(&mode);
    // The card display's refreshes, exactly: refresh k is at k * htotal * vtotal / dot_clock seconds.
    let refresh_ns = |k: u128| k * 2200 * 1125 * 1_000_000_000 / 148_350_000;
    let start = Instant::now();
    let mut planned = start;
    for k in 1..=100_000 {
        planned = next_present(planned, planned, period);
        let at = (planned - start).as_nanos();
        // Each present is ready by its refresh and lands after the one before: none skipped.
        assert!(at <= refresh_ns(k) && at > refresh_ns(k - 1), "refresh {k}");
    }
    // Interlace halves the lines per refresh (a field); doublescan doubles them.
    let flagged = |mode_flags| refresh_period(&ModeInfo { mode_flags, ..mode });
    assert_eq!(period, Duration::from_nanos(16_683_518));
    assert_eq!(
        flagged(ModeFlag::INTERLACE),
        Duration::from_nanos(8_341_759)
    );
    assert_eq!(
        flagged(ModeFlag::DOUBLE_SCAN),
        Duration::from_nanos(33_367_037)
    );
    // A mode without timings is taken as 60 Hz.
    assert_eq!(
        refresh_period(&ModeInfo::default()),
        Duration::from_secs(1) / 60
    );
}

#[test]
fn stop_leaves_a_stuck_mirror_thread_behind_instead_of_freezing() {
    use super::present::join_within;
    use std::time::{Duration, Instant};
    let (_release, stuck) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let _ = stuck.recv();
    });
    let started = Instant::now();
    assert!(join_within(thread, Duration::from_millis(100)).is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
    let done = std::thread::spawn(|| 7);
    assert_eq!(
        join_within(done, Duration::from_secs(5)).map(Result::unwrap),
        Some(7)
    );
}

#[test]
fn the_dma_buf_sync_file_ioctl_is_dma_buf_ioctl_export_sync_file() {
    // linux/dma-buf.h: _IOWR(DMA_BUF_BASE 'b', 2, struct dma_buf_export_sync_file), 8 bytes.
    assert_eq!(super::gpu::DMA_BUF_IOCTL_EXPORT_SYNC_FILE, 0xC008_6202);
}

#[test]
fn an_imported_plane_is_taken_from_the_compositor_and_given_back_in_general_layout() {
    use super::gpu::foreign_barrier;
    use ash::vk;
    let (acquire, acquire_stages) = foreign_barrier(vk::Image::null(), 3, true);
    assert_eq!(
        (
            acquire.src_queue_family_index,
            acquire.dst_queue_family_index
        ),
        (vk::QUEUE_FAMILY_FOREIGN_EXT, 3)
    );
    assert_eq!(
        (acquire.old_layout, acquire.new_layout),
        (
            vk::ImageLayout::GENERAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL
        )
    );
    assert_eq!(acquire.dst_access_mask, vk::AccessFlags::TRANSFER_READ);
    assert_eq!(acquire_stages[1], vk::PipelineStageFlags::TRANSFER);
    let (release, release_stages) = foreign_barrier(vk::Image::null(), 3, false);
    assert_eq!(
        (
            release.src_queue_family_index,
            release.dst_queue_family_index
        ),
        (3, vk::QUEUE_FAMILY_FOREIGN_EXT)
    );
    assert_eq!(
        (release.old_layout, release.new_layout),
        (
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::GENERAL
        )
    );
    assert_eq!(release_stages[0], vk::PipelineStageFlags::TRANSFER);
}

/// `CARDCAST_GUARD_DESKTOP=gnome` (CI: headless GNOME Shell with two virtual monitors, cardcast's
/// extension and `foot`): the mirror's window is fullscreen on the card display, and the focus
/// stays on the window that had it. Skips otherwise.
#[test]
fn desktop_gnome_mirror_window_is_fullscreen_on_the_card_display_without_the_focus() {
    use super::gpu::Gpu;
    use super::present::Shown;
    use super::screen::Screen;
    use crate::guard::gnome::{NAME, PATH};
    use std::time::{Duration, Instant};
    if std::env::var("CARDCAST_GUARD_DESKTOP").as_deref() != Ok("gnome") {
        return;
    }
    let bus = zbus::blocking::Connection::session().unwrap();
    let call = |method: &str| {
        bus.call_method(Some(NAME), PATH, Some(NAME), method, &())
            .unwrap()
    };
    let focus = || -> String { call("Focus").body().deserialize().unwrap() };
    let mirror = || {
        let windows: Vec<(String, String, String, bool)> =
            call("Windows").body().deserialize().unwrap();
        windows
            .into_iter()
            .find_map(|(_, state, _, mirror)| mirror.then_some(state))
    };
    let monitors = crate::guard::monitors().unwrap();
    let card = monitors.iter().find(|m| !m.primary).unwrap();
    /// Closed on drop, so a failed test leaves no window behind.
    struct Foot(std::process::Child);
    impl Drop for Foot {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _focused = Foot(std::process::Command::new("foot").spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    while focus().is_empty() {
        assert!(Instant::now() < deadline, "foot never took the focus");
        std::thread::sleep(Duration::from_millis(50));
    }
    let before = focus();

    let mut screen = Screen::open(Shown::Fullscreen, card).unwrap();
    let mut gpu = Gpu::open(&screen).unwrap();
    screen.show().unwrap();
    let expected = format!(
        "{} {} {} {} 1 0 0",
        card.left,
        card.top,
        card.left + i32::try_from(card.layout_size.0).unwrap(),
        card.top + i32::try_from(card.layout_size.1).unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        screen.follow().unwrap();
        let frame = Frame {
            width: 4,
            height: 4,
            fourcc: XRGB8888,
            pixels: Pixels::Memory {
                bytes: vec![0x80; 64],
                stride: 16,
            },
            release: Release::default(),
        };
        assert!(matches!(gpu.take(frame), super::gpu::Taken::Yes));
        gpu.present().unwrap();
        let shown = mirror();
        assert_eq!(focus(), before, "the mirror took the focus");
        if shown.as_deref() == Some(expected.as_str()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the mirror is at {shown:?}, not fullscreen on the card display ({expected})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // A while longer: the focus stays where it was.
    for _ in 0..20 {
        screen.follow().unwrap();
        assert_eq!(focus(), before, "the mirror took the focus");
        std::thread::sleep(Duration::from_millis(50));
    }
}
