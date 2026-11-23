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
    let mut mirror = MirrorWindow::default();
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
    mirror.stop();
    // SAFETY: IsWindow accepts any handle.
    assert!(!unsafe { IsWindow(Some(window)) }.as_bool());
}
