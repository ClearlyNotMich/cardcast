//! Window placement decision: where a window on the sender goes. No Windows calls.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A rectangle in desktop pixels; `right` and `bottom` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// What the window guard knows about one top-level window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowState {
    /// The visible frame, without the invisible resize borders.
    pub frame: Rect,
    pub maximised: bool,
    /// Where the window goes when restored from maximised or minimised, in desktop pixels.
    pub restore: Rect,
    pub minimised: bool,
    pub hidden: bool,
    pub cloaked: bool,
    pub tool: bool,
    /// One of cardcast's own windows, such as the mirror.
    pub own: bool,
}

/// Where a window goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Stay,
    /// Move the visible frame to this rectangle.
    MoveTo(Rect),
    /// Maximise on the monitor with this work area, restoring to `restore`.
    Maximise {
        work_area: Rect,
        restore: Rect,
    },
    /// The window keeps landing on the card display; leave it there and report it.
    GiveUp,
}

impl Rect {
    fn width(self) -> i32 {
        self.right - self.left
    }

    fn height(self) -> i32 {
        self.bottom - self.top
    }

    fn area(self) -> i64 {
        i64::from(self.width().max(0)) * i64::from(self.height().max(0))
    }

    fn intersection(self, other: Self) -> Self {
        Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }
}

/// Returns allowed per window per minute before the guard leaves the window alone.
const RETURNS_PER_MINUTE: usize = 3;

/// Recent returns per window, so the guard never fights a program in a loop.
#[derive(Debug, Default)]
pub struct ReturnHistory {
    returns: HashMap<isize, Vec<Instant>>,
    given_up: HashSet<isize>,
}

impl ReturnHistory {
    /// [`place`], plus giving up on window `id` once it lands on the card display too often.
    pub fn place(
        &mut self,
        id: isize,
        window: &WindowState,
        card_display: Rect,
        send_windows_to: Rect,
        now: Instant,
    ) -> Placement {
        if self.given_up.contains(&id) {
            return Placement::Stay;
        }
        let placement = place(window, card_display, send_windows_to);
        if placement == Placement::Stay {
            return placement;
        }
        let returns = self.returns.entry(id).or_default();
        returns.retain(|&at| now.duration_since(at) < Duration::from_secs(60));
        if returns.len() >= RETURNS_PER_MINUTE {
            self.returns.remove(&id);
            self.given_up.insert(id);
            return Placement::GiveUp;
        }
        returns.push(now);
        placement
    }
}

/// Decides where a window goes, given the card display and the "send windows to" monitor's work area.
pub fn place(window: &WindowState, card_display: Rect, send_windows_to: Rect) -> Placement {
    if window.minimised || window.hidden || window.cloaked || window.tool || window.own {
        return Placement::Stay;
    }
    let frame = window.frame;
    // "On the card display" means most of the visible frame is there.
    if frame.intersection(card_display).area() * 2 <= frame.area() {
        return Placement::Stay;
    }
    if window.maximised {
        return Placement::Maximise {
            work_area: send_windows_to,
            restore: move_into(window.restore, card_display, send_windows_to),
        };
    }
    Placement::MoveTo(move_into(frame, card_display, send_windows_to))
}

/// Keeps `rect`'s offset from the card display's corner on `send_windows_to`, shrunk and pulled inside it.
fn move_into(rect: Rect, card_display: Rect, send_windows_to: Rect) -> Rect {
    let width = rect.width().min(send_windows_to.width());
    let height = rect.height().min(send_windows_to.height());
    let left = send_windows_to.left + rect.left - card_display.left;
    let top = send_windows_to.top + rect.top - card_display.top;
    let left = left
        .min(send_windows_to.right - width)
        .max(send_windows_to.left);
    let top = top
        .min(send_windows_to.bottom - height)
        .max(send_windows_to.top);
    Rect {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The card display sits right of a 1920×1080 primary monitor whose work area loses a 40 px taskbar.
    const CARD: Rect = Rect {
        left: 1920,
        top: 0,
        right: 3840,
        bottom: 1080,
    };
    const PRIMARY_WORK: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    fn window(frame: Rect) -> WindowState {
        WindowState {
            frame,
            ..WindowState::default()
        }
    }

    #[test]
    fn window_mostly_on_the_card_display_is_returned() {
        // 800×600, 700 px wide part on the card display.
        let placement = place(&window(rect(1820, 100, 2620, 700)), CARD, PRIMARY_WORK);
        assert_eq!(placement, Placement::MoveTo(rect(0, 100, 800, 700)));
    }

    #[test]
    fn window_mostly_off_the_card_display_stays() {
        // 800×600, only 300 px wide part on the card display.
        let placement = place(&window(rect(1420, 100, 2220, 700)), CARD, PRIMARY_WORK);
        assert_eq!(placement, Placement::Stay);
    }

    #[test]
    fn window_too_big_for_the_work_area_shrinks_to_fit() {
        // 1920×1080 frame filling the card display; the primary work area is only 1040 high.
        let placement = place(&window(CARD), CARD, PRIMARY_WORK);
        assert_eq!(placement, Placement::MoveTo(PRIMARY_WORK));
    }

    #[test]
    fn maximised_window_is_maximised_on_the_send_windows_to_monitor_with_its_restore_rect_moved_too()
     {
        // Restores to 800×600 at (100, 50) on the card display.
        let maximised = WindowState {
            maximised: true,
            restore: rect(2020, 50, 2820, 650),
            ..window(CARD)
        };
        assert_eq!(
            place(&maximised, CARD, PRIMARY_WORK),
            Placement::Maximise {
                work_area: PRIMARY_WORK,
                restore: rect(100, 50, 900, 650),
            }
        );
    }

    #[test]
    fn maximised_window_restore_rect_too_big_for_the_work_area_shrinks_to_fit() {
        let maximised = WindowState {
            maximised: true,
            restore: rect(1900, -20, 3900, 1100),
            ..window(CARD)
        };
        assert_eq!(
            place(&maximised, CARD, PRIMARY_WORK),
            Placement::Maximise {
                work_area: PRIMARY_WORK,
                restore: PRIMARY_WORK,
            }
        );
    }

    #[test]
    fn minimised_hidden_cloaked_tool_and_own_windows_stay() {
        let on_card = window(rect(2000, 100, 2800, 700));
        for ignored in [
            WindowState {
                minimised: true,
                ..on_card
            },
            WindowState {
                hidden: true,
                ..on_card
            },
            WindowState {
                cloaked: true,
                ..on_card
            },
            WindowState {
                tool: true,
                ..on_card
            },
            WindowState {
                own: true,
                ..on_card
            },
        ] {
            assert_eq!(
                place(&ignored, CARD, PRIMARY_WORK),
                Placement::Stay,
                "{ignored:?}"
            );
        }
    }

    const ID: isize = 0x1234;

    fn land_on_card(history: &mut ReturnHistory, now: Instant) -> Placement {
        history.place(
            ID,
            &window(rect(2000, 100, 2800, 700)),
            CARD,
            PRIMARY_WORK,
            now,
        )
    }

    #[test]
    fn window_landing_a_fourth_time_within_a_minute_is_given_up_then_left_alone() {
        let mut history = ReturnHistory::default();
        let start = Instant::now();
        for second in 0..3 {
            let placement = land_on_card(&mut history, start + Duration::from_secs(second * 10));
            assert!(matches!(placement, Placement::MoveTo(_)), "{placement:?}");
        }
        assert_eq!(
            land_on_card(&mut history, start + Duration::from_secs(30)),
            Placement::GiveUp
        );
        assert_eq!(
            land_on_card(&mut history, start + Duration::from_secs(40)),
            Placement::Stay
        );
    }

    #[test]
    fn window_returned_less_often_than_a_few_times_a_minute_is_always_returned() {
        let mut history = ReturnHistory::default();
        let start = Instant::now();
        for second in 0..10 {
            let placement = land_on_card(&mut history, start + Duration::from_secs(second * 25));
            assert!(matches!(placement, Placement::MoveTo(_)), "{placement:?}");
        }
    }
}
