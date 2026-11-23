//! Render buffer sizing and clock-drift correction for the relay. No Windows calls.
//!
//! The audio source and the card display's sound output run on different clocks, so the render
//! stream would slowly run empty (clicks) or fill up (growing delay). The relay keeps the render
//! padding near a target by nudging the render stream's sample rate (`IAudioClockAdjustment`),
//! see docs/win32-mirror-audio.md, "Clock drift between the two devices".

/// Render buffer to ask for, in 100-ns units: 100 ms.
pub const BUFFER_DURATION: i64 = 1_000_000;

/// Padding to hold in the render buffer, in frames: 40 ms, and at most half the buffer.
pub fn target_padding(rate: u32, buffer_frames: u32) -> u32 {
    (rate / 25).min(buffer_frames / 2)
}

/// Frames to write into the render buffer for one batch of captured frames.
#[derive(Debug, PartialEq, Eq)]
pub struct Write {
    /// Silence first: after the buffer ran empty it is refilled to the target before data.
    pub silence: u32,
    /// Captured frames that fit; the rest are dropped.
    pub data: u32,
}

pub fn plan_write(padding: u32, buffer_frames: u32, target: u32, incoming: u32) -> Write {
    let silence = if padding == 0 { target } else { 0 };
    let space = buffer_frames.saturating_sub(padding + silence);
    Write {
        silence,
        data: incoming.min(space),
    }
}

const SMOOTHING: f64 = 0.05;
/// A buffer twice the target asks for 1% faster, before the clamp.
const GAIN: f64 = 0.01;
/// Half a percent is under a tenth of a semitone; clock drift is far smaller.
const MAX_ADJUST: f64 = 0.005;
/// Smaller changes are not worth a `SetSampleRate` call (about 10 ppm at 48 kHz).
const MIN_STEP_HZ: f32 = 0.5;

/// Picks the render stream's sample rate from its padding, smoothed over many packets.
#[derive(Debug, Default)]
pub struct Drift {
    smoothed: Option<f64>,
    set: Option<f32>,
}

impl Drift {
    /// The rate to pass to `SetSampleRate`, or `None` if the current one is close enough.
    /// `nominal` is the audio source's sample rate.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a sample rate within half a percent of a u32 rate fits an f32"
    )]
    pub fn update(&mut self, nominal: u32, padding: u32, target: u32) -> Option<f32> {
        let padding = f64::from(padding);
        // About 200 ms of 10 ms packets, so one packet's jitter barely moves the rate.
        let smoothed = self.smoothed.map_or(padding, |smoothed| {
            smoothed + SMOOTHING * (padding - smoothed)
        });
        self.smoothed = Some(smoothed);
        let error = (smoothed - f64::from(target)) / f64::from(target.max(1));
        let adjust = (error * GAIN).clamp(-MAX_ADJUST, MAX_ADJUST);
        // A higher rate makes the engine read our frames faster, draining the buffer.
        let rate = (f64::from(nominal) * (1.0 + adjust)) as f32;
        if self.set.is_some_and(|set| (set - rate).abs() < MIN_STEP_HZ) {
            return None;
        }
        self.set = Some(rate);
        Some(rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_40_ms_within_half_the_buffer() {
        assert_eq!(target_padding(48_000, 4_800), 1_920);
        assert_eq!(target_padding(44_100, 4_410), 1_764);
        assert_eq!(target_padding(48_000, 2_000), 1_000);
    }

    #[test]
    fn data_is_written_while_it_fits() {
        assert_eq!(
            plan_write(1_920, 4_800, 1_920, 480),
            Write {
                silence: 0,
                data: 480
            }
        );
    }

    #[test]
    fn a_full_buffer_drops_what_does_not_fit() {
        assert_eq!(
            plan_write(4_000, 4_800, 1_920, 1_000),
            Write {
                silence: 0,
                data: 800
            }
        );
    }

    #[test]
    fn an_empty_buffer_is_refilled_with_silence_to_the_target_first() {
        assert_eq!(
            plan_write(0, 4_800, 1_920, 480),
            Write {
                silence: 1_920,
                data: 480
            }
        );
        assert_eq!(
            plan_write(0, 4_800, 1_920, 4_000),
            Write {
                silence: 1_920,
                data: 2_880
            }
        );
    }

    /// Runs `drift` for a second of 10 ms packets at a steady padding; returns the last rate set.
    fn settle(drift: &mut Drift, padding: u32) -> Option<f32> {
        (0..100)
            .filter_map(|_| drift.update(48_000, padding, 1_920))
            .last()
    }

    #[test]
    fn the_first_update_sets_the_audio_sources_rate() {
        assert_eq!(
            Drift::default().update(48_000, 1_920, 1_920),
            Some(48_000.0)
        );
    }

    #[test]
    fn padding_at_the_target_keeps_the_rate() {
        let mut drift = Drift::default();
        assert_eq!(drift.update(48_000, 1_920, 1_920), Some(48_000.0));
        assert_eq!(settle(&mut drift, 1_920), None);
    }

    #[test]
    fn a_filling_buffer_is_drained_faster() {
        let mut drift = Drift::default();
        let rate = settle(&mut drift, 2_400).unwrap();
        assert!(rate > 48_000.0 && rate <= 48_240.0, "{rate}");
    }

    #[test]
    fn an_emptying_buffer_is_drained_slower() {
        let mut drift = Drift::default();
        let rate = settle(&mut drift, 1_400).unwrap();
        assert!((47_760.0..48_000.0).contains(&rate), "{rate}");
    }

    #[test]
    fn the_adjustment_is_at_most_half_a_percent() {
        let mut drift = Drift::default();
        assert_eq!(settle(&mut drift, 4_800), Some(48_240.0));
        assert_eq!(settle(&mut drift, 0), Some(47_760.0));
    }
}
