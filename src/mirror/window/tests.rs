//! Checks the mirror window on the runner's desktop; its only monitor stands in for the card display.

use super::*;
use crate::mirror::capture::duplicating;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowDisplayAffinity, GetWindowLongPtrW,
    GetWindowRect, IsWindow, WS_CAPTION, WS_THICKFRAME,
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
    mirror
}

fn window(mirror: &MirrorWindow) -> HWND {
    HWND(mirror.running.as_ref().unwrap().window as *mut _)
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
fn second_monitor_card_display_on_another_graphics_card_is_refused() {
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
    // The job's primary monitor sits on the runner's own graphics card, the virtual monitors on the virtual one.
    let source_monitor = &monitors[0];
    let Some(card_display) = monitors
        .iter()
        .find(|monitor| adapter(monitor) != adapter(source_monitor))
    else {
        panic!("no monitor on a graphics card other than the primary monitor's");
    };
    let _duplicating = duplicating();
    let mut mirror = MirrorWindow::new(|| {});
    let refused = mirror.start(source_monitor, card_display).unwrap_err();
    assert_eq!(
        refused,
        "The source monitor and the card display are on different graphics cards. Plug both into the same one."
    );
    assert!(!mirror.is_running());
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
