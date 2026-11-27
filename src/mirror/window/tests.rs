//! Checks the mirror window on the runner's desktop; its only monitor stands in for the card display.

use super::*;
use crate::mirror::capture::duplicating;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowDisplayAffinity,
    GetWindowLongPtrW, GetWindowRect, IsWindow, IsWindowVisible, WS_CAPTION, WS_THICKFRAME,
};

fn card_display() -> Monitor {
    crate::guard::monitors()
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
}

fn started(card_display: &Monitor) -> MirrorWindow {
    let mut mirror = MirrorWindow::new(|| {});
    mirror.start(card_display, card_display).unwrap();
    shows_picture(&mut mirror);
    mirror
}

/// Waits for a started mirror to show the picture; fails with its reason if it ended instead.
fn shows_picture(mirror: &mut MirrorWindow) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mirror.shows_picture() {
        if let Some(end) = mirror.take_end() {
            panic!("the mirror did not start: {end:?}");
        }
        assert!(
            Instant::now() < deadline,
            "the mirror never showed the picture"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How a started mirror ended; fails if it is still running after 10 s.
fn end(mirror: &mut MirrorWindow) -> Ended {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(end) = mirror.take_end() {
            return end;
        }
        assert!(Instant::now() < deadline, "the mirror did not end");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn window(mirror: &MirrorWindow) -> HWND {
    HWND(
        mirror
            .running
            .as_ref()
            .unwrap()
            .shown
            .load(Ordering::Acquire) as *mut _,
    )
}

#[test]
fn start_returns_before_the_mirror_opens_the_graphics_card() {
    let _duplicating = duplicating();
    let card_display = card_display();
    let woken = Arc::new(AtomicU64::new(0));
    let mut mirror = {
        let woken = woken.clone();
        MirrorWindow::new(move || {
            woken.fetch_add(1, Ordering::Relaxed);
        })
    };
    let start = Instant::now();
    mirror.start(&card_display, &card_display).unwrap();
    let returned = start.elapsed();
    let showing = mirror.shows_picture();
    // SAFETY: the class name is a static string; no window name is matched.
    let window = unsafe { FindWindowW(CLASS, None) }.ok();
    let visible = window.is_some_and(|window| {
        // SAFETY: IsWindowVisible accepts any handle.
        unsafe { IsWindowVisible(window) }.as_bool()
    });
    println!("start returned after {returned:?}");
    assert!(
        !showing && !visible,
        "start waited for the mirror to show the picture"
    );
    assert!(mirror.is_running(), "the mirror is not starting");
    shows_picture(&mut mirror);
    println!("the picture showed after {:?}", start.elapsed());
    assert!(
        woken.load(Ordering::Relaxed) >= 1,
        "the settings window was not woken to see the mirror run"
    );
    mirror.stop();
    assert!(!mirror.is_running());
}

#[test]
fn stop_while_starting_never_shows_the_mirror_window() {
    let _duplicating = duplicating();
    let card_display = card_display();
    let mut mirror = MirrorWindow::new(|| {});
    mirror.start(&card_display, &card_display).unwrap();
    let shown = mirror.running.as_ref().unwrap().shown.clone();
    let stopping = Instant::now();
    mirror.stop();
    println!("stop during the start took {:?}", stopping.elapsed());
    assert_eq!(
        shown.load(Ordering::Acquire),
        0,
        "the mirror window was shown after Stop"
    );
}

#[test]
fn a_mirror_thread_that_panics_wakes_the_settings_window() {
    let woken = Arc::new(AtomicU64::new(0));
    let thread = {
        let woken = woken.clone();
        std::thread::spawn(move || {
            let _wake = WakeOnEnd(Arc::new(move || {
                woken.fetch_add(1, Ordering::Relaxed);
            }));
            panic!("a bug in the mirror thread");
        })
    };
    assert!(thread.join().is_err());
    assert_eq!(woken.load(Ordering::Relaxed), 1, "the panic woke nobody");
}

#[test]
fn stop_while_starting_leaves_no_mirror_thread_or_window_behind() {
    let _duplicating = duplicating();
    let card_display = card_display();
    let mut mirror = MirrorWindow::new(|| {});
    mirror.start(&card_display, &card_display).unwrap();
    mirror.stop();
    assert!(!mirror.is_running());
    // A mirror thread left running would have opened its window by now.
    std::thread::sleep(Duration::from_secs(1));
    // SAFETY: the class name is a static string; no window name is matched.
    let window = unsafe { FindWindowW(CLASS, None) };
    assert!(window.is_err(), "a mirror window was left behind");
}

fn foreground() -> isize {
    // SAFETY: plain call returning a handle.
    unsafe { GetForegroundWindow() }.0 as isize
}

#[test]
fn mirror_window_covers_the_card_display_with_its_styles() {
    let _duplicating = duplicating();
    let card_display = card_display();
    let before = foreground();
    let mirror = started(&card_display);
    let window = window(&mirror);
    // Let any activation the show caused reach the foreground.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        foreground(),
        before,
        "showing the mirror changed the foreground window"
    );

    let mut rect = RECT::default();
    // SAFETY: rect is a valid RECT; the window lives until `mirror` drops.
    unsafe { GetWindowRect(window, &mut rect) }.unwrap();
    let width = i32::try_from(card_display.width).unwrap();
    let height = i32::try_from(card_display.height).unwrap();
    assert_eq!(
        (rect.left, rect.top, rect.right, rect.bottom),
        (
            card_display.left,
            card_display.top,
            card_display.left + width,
            card_display.top + height
        )
    );
    // SAFETY: plain style reads.
    let (style, extended) = unsafe {
        (
            GetWindowLongPtrW(window, GWL_STYLE),
            GetWindowLongPtrW(window, GWL_EXSTYLE),
        )
    };
    let has = |styles: isize, flag: u32| styles & flag as isize != 0;
    assert!(has(style, WS_POPUP.0), "not a popup window");
    assert!(!has(style, WS_CAPTION.0 | WS_THICKFRAME.0), "has a border");
    assert!(has(extended, WS_EX_TOPMOST.0), "not topmost");
    assert!(has(extended, WS_EX_NOACTIVATE.0), "can be activated");
    assert!(has(extended, WS_EX_TOOLWINDOW.0), "shows in Alt+Tab");
    let mut affinity = 0;
    // SAFETY: affinity is a valid u32 out pointer.
    unsafe { GetWindowDisplayAffinity(window, &mut affinity) }.unwrap();
    assert_eq!(
        affinity, WDA_EXCLUDEFROMCAPTURE.0,
        "not excluded from capture"
    );
}

#[test]
fn mirror_window_presents_at_the_refresh_rate() {
    let _duplicating = duplicating();
    let card_display = card_display();
    let mirror = started(&card_display);
    let presents = &mirror.running.as_ref().unwrap().presents;
    std::thread::sleep(Duration::from_millis(500));
    let (start, at_start) = (Instant::now(), presents.load(Ordering::Relaxed));
    std::thread::sleep(Duration::from_secs(2));
    let counted = presents.load(Ordering::Relaxed) - at_start;
    let per_second = counted as f64 / start.elapsed().as_secs_f64();
    let refresh = f64::from(card_display.refresh_hz);
    assert!(
        per_second <= refresh * 1.1 + 1.0 && per_second >= refresh * 0.5,
        "{per_second:.1} presents per second on a {refresh} Hz monitor"
    );
}

#[test]
fn stop_closes_the_mirror_window() {
    let _duplicating = duplicating();
    let mut mirror = started(&card_display());
    let window = window(&mirror);
    assert!(mirror.is_running());
    mirror.stop();
    assert!(!mirror.is_running());
    // SAFETY: IsWindow accepts any handle.
    assert!(!unsafe { IsWindow(Some(window)) }.as_bool());
}

#[test]
fn second_monitor_mirror_starts_on_it_and_presents() {
    // The two virtual monitors share a graphics card, as SPEC section 1 needs.
    let Some(monitors) = crate::guard::second_monitor::monitors(3) else {
        return;
    };
    let (source_monitor, card_display) = (&monitors[1], &monitors[2]);
    let _duplicating = duplicating();
    let mut mirror = MirrorWindow::new(|| {});
    mirror.start(source_monitor, card_display).unwrap();
    shows_picture(&mut mirror);
    let mut rect = RECT::default();
    // SAFETY: rect is a valid RECT; the window lives until `mirror` drops.
    unsafe { GetWindowRect(window(&mirror), &mut rect) }.unwrap();
    assert_eq!(
        (rect.left, rect.top),
        (card_display.left, card_display.top),
        "mirror window is not on the card display"
    );
    let presents = &mirror.running.as_ref().unwrap().presents;
    std::thread::sleep(Duration::from_millis(500));
    let (start, at_start) = (Instant::now(), presents.load(Ordering::Relaxed));
    std::thread::sleep(Duration::from_secs(2));
    let counted = presents.load(Ordering::Relaxed) - at_start;
    let per_second = counted as f64 / start.elapsed().as_secs_f64();
    // Loose upper bound: a virtual monitor has no vertical blank, and the runner measured 47 presents a second at 30 Hz.
    let refresh = f64::from(card_display.refresh_hz);
    assert!(
        (refresh * 0.5..refresh * 8.0).contains(&per_second),
        "{per_second:.1} presents per second on a {refresh} Hz card display"
    );
    assert!(mirror.is_running(), "the mirror ended on its own");
}

#[test]
fn second_monitor_card_display_on_another_graphics_card_is_mirrored() {
    // SPIKE (research-cross-gpu, not for merge): Desktop Duplication on the source monitor's
    // graphics card, swap chain on the card display's window on another graphics card.
    let Some(monitors) = crate::guard::second_monitor::monitors(2) else {
        return;
    };
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1() }.unwrap();
    let outputs = outputs(&factory);
    let adapter = |monitor: &Monitor| {
        find_output(&monitor.device_path, &monitors, &outputs)
            .unwrap()
            .adapter
    };
    let name = |index: u32| {
        // SAFETY: index came from `outputs` on this factory.
        let description = unsafe { factory.EnumAdapters1(index).unwrap().GetDesc1() }.unwrap();
        crate::guard::wide_to_string(&description.Description)
    };
    // The job's primary monitor sits on the runner's own graphics card, the virtual monitors on the virtual one.
    let source_monitor = &monitors[0];
    let Some(card_display) = monitors
        .iter()
        .find(|monitor| adapter(monitor) != adapter(source_monitor))
    else {
        panic!("no monitor on a graphics card other than the primary monitor's");
    };
    println!(
        "source monitor on adapter {} ({}), card display on adapter {} ({})",
        adapter(source_monitor),
        name(adapter(source_monitor)),
        adapter(card_display),
        name(adapter(card_display))
    );
    // The refusal test that used `end` is replaced by this spike.
    let _ = end;
    let _duplicating = duplicating();
    let mut mirror = MirrorWindow::new(|| {});
    mirror.start(source_monitor, card_display).unwrap();
    shows_picture(&mut mirror);
    let presents = &mirror.running.as_ref().unwrap().presents;
    std::thread::sleep(Duration::from_millis(500));
    let (start, at_start) = (Instant::now(), presents.load(Ordering::Relaxed));
    std::thread::sleep(Duration::from_secs(5));
    let counted = presents.load(Ordering::Relaxed) - at_start;
    let per_second = counted as f64 / start.elapsed().as_secs_f64();
    let refresh = f64::from(card_display.refresh_hz);
    println!(
        "{per_second:.1} presents per second across graphics cards on a {refresh} Hz card display"
    );
    assert!(
        mirror.is_running(),
        "the mirror ended: {:?}",
        mirror.take_end()
    );
    assert!(
        per_second >= refresh * 0.5,
        "{per_second:.1} presents per second"
    );
}

#[test]
fn a_lost_graphics_card_restarts_and_other_failures_stop() {
    assert!(matches!(
        ended(windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED.0),
        Ended::Restart(_)
    ));
    assert!(matches!(
        ended(windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_HUNG.0),
        Ended::Restart(_)
    ));
    assert!(matches!(
        ended(windows::Win32::Graphics::Dxgi::DXGI_ERROR_ACCESS_LOST.0),
        Ended::Restart(_)
    ));
    assert!(matches!(
        ended(windows::Win32::Foundation::E_OUTOFMEMORY.0),
        Ended::Stop(_)
    ));
}

#[test]
fn stopped_mirror_has_no_end_to_report() {
    let _duplicating = duplicating();
    let mut mirror = started(&card_display());
    assert_eq!(mirror.take_end(), None);
    mirror.stop();
    assert_eq!(mirror.take_end(), None);
}
