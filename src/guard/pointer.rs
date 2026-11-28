//! Pointer decision: where the pointer on the sender goes. No Windows calls.

use super::placement::Rect;

/// The last pointer position seen off the card display, so the pointer can be put back there.
#[derive(Debug, Default)]
pub struct PointerHistory {
    last_off_card: Option<(i32, i32)>,
}

impl PointerHistory {
    /// Where to put the pointer, or `None` to leave it; `fallback` is used before any off-card position is seen.
    pub fn place(
        &mut self,
        pointer: (i32, i32),
        card_display: Rect,
        fallback: (i32, i32),
    ) -> Option<(i32, i32)> {
        let (x, y) = pointer;
        let on_card = (card_display.left..card_display.right).contains(&x)
            && (card_display.top..card_display.bottom).contains(&y);
        if on_card {
            return Some(self.last_off_card.unwrap_or(fallback));
        }
        self.last_off_card = Some(pointer);
        None
    }
}

/// Where a pointer seen at `pointer` on the card display goes when only that position is known (on
/// Wayland, which shows cardcast the pointer only over its own surface): the nearest point on any of
/// the `others` monitors, which is just across the edge it came over. `None` without another monitor.
#[cfg_attr(
    windows,
    allow(
        dead_code,
        reason = "Windows sees the pointer everywhere, so it uses PointerHistory"
    )
)]
pub fn nearest_off_card(pointer: (i32, i32), others: &[Rect]) -> Option<(i32, i32)> {
    let (x, y) = pointer;
    others
        .iter()
        .filter(|r| r.left < r.right && r.top < r.bottom)
        .map(|r| (x.clamp(r.left, r.right - 1), y.clamp(r.top, r.bottom - 1)))
        .min_by_key(|&(nx, ny)| {
            (i64::from(nx) - i64::from(x)).pow(2) + (i64::from(ny) - i64::from(y)).pow(2)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card display sits right of a 1920×1080 primary monitor.
    const CARD: Rect = Rect {
        left: 1920,
        top: 0,
        right: 3840,
        bottom: 1080,
    };
    const FALLBACK: (i32, i32) = (960, 520);

    #[test]
    fn pointer_on_the_card_display_returns_to_last_off_card_position() {
        let mut history = PointerHistory::default();
        assert_eq!(history.place((1900, 500), CARD, FALLBACK), None);
        assert_eq!(history.place((1919, 1079), CARD, FALLBACK), None);
        assert_eq!(
            history.place((1920, 1079), CARD, FALLBACK),
            Some((1919, 1079))
        );
        // Still on the card display next frame: still the last off-card position.
        assert_eq!(
            history.place((2500, 300), CARD, FALLBACK),
            Some((1919, 1079))
        );
    }

    #[test]
    fn pointer_elsewhere_is_untouched() {
        let mut history = PointerHistory::default();
        assert_eq!(history.place((100, 100), CARD, FALLBACK), None);
        assert_eq!(history.place((3840, 500), CARD, FALLBACK), None);
        assert_eq!(history.place((2000, 1080), CARD, FALLBACK), None);
    }

    #[test]
    fn a_pointer_seen_only_on_the_card_display_goes_just_across_the_nearest_edge() {
        let primary = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        // A third monitor below the card display.
        let below = Rect {
            left: 1920,
            top: 1080,
            right: 3840,
            bottom: 2160,
        };
        let others = [primary, below];
        assert_eq!(nearest_off_card((1920, 500), &others), Some((1919, 500)));
        assert_eq!(nearest_off_card((3000, 1079), &others), Some((3000, 1080)));
        // Below the primary monitor's bottom: its corner pixel.
        assert_eq!(
            nearest_off_card((1925, 1085), &[primary]),
            Some((1919, 1079))
        );
        assert_eq!(nearest_off_card((2500, 300), &[]), None);
    }

    #[test]
    fn pointer_on_the_card_display_before_any_off_card_position_goes_to_the_fallback() {
        let mut history = PointerHistory::default();
        assert_eq!(history.place((2500, 300), CARD, FALLBACK), Some(FALLBACK));
    }
}
