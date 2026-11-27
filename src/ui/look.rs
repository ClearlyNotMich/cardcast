//! The settings window's look and the plain decisions behind it: colours, status tones, the
//! repaint policy, the frame rate and the live layout's geometry. No Windows calls.

use super::settings::{self, Settings, Status};
use crate::guard::Monitor;
use eframe::egui::{self, Color32, CornerRadius, Rect, Shadow, Stroke, pos2, vec2};
use std::time::{Duration, Instant};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x0A, 0x0D, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x11, 0x16, 0x21);
pub const RAISED: Color32 = Color32::from_rgb(0x19, 0x20, 0x2F);
pub const OUTLINE: Color32 = Color32::from_rgb(0x2A, 0x33, 0x48);
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEC, 0xF4);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8B, 0x95, 0xAB);
/// The one brand accent: buttons, selection and the card display.
pub const ACCENT: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xFF);
pub const SKY: Color32 = Color32::from_rgb(0x38, 0xBD, 0xF8);
pub const LIVE: Color32 = Color32::from_rgb(0x2E, 0xE5, 0x9D);
pub const BUSY: Color32 = Color32::from_rgb(0xFF, 0xB5, 0x47);
pub const PROBLEM: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x7A);
pub const IDLE: Color32 = Color32::from_rgb(0x6B, 0x74, 0x89);

/// The repaint cap while the status indicator animates: about 30 Hz.
pub const ANIMATION_FRAME: Duration = Duration::from_millis(33);
/// While another window (the game) has focus, only the frame rate readout refreshes, once a second.
pub const UNFOCUSED_FRAME: Duration = Duration::from_secs(1);

/// Dark, high-contrast visuals in the style of capture and GPU control apps (docs/research/ui-design.md).
pub fn apply(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    context.style_mut_of(egui::Theme::Dark, |style| {
        use egui::{FontId, TextStyle};
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(20.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.button_padding = vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        let visuals = &mut style.visuals;
        visuals.panel_fill = BACKGROUND;
        visuals.window_fill = PANEL;
        visuals.extreme_bg_color = BACKGROUND;
        visuals.faint_bg_color = PANEL;
        visuals.override_text_color = None;
        visuals.window_stroke = Stroke::new(1.0, OUTLINE);
        visuals.window_corner_radius = CornerRadius::same(10);
        visuals.menu_corner_radius = CornerRadius::same(8);
        let shadow = Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(140),
        };
        visuals.window_shadow = shadow;
        visuals.popup_shadow = shadow;
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        visuals.selection.stroke = Stroke::new(1.0, TEXT);
        visuals.hyperlink_color = SKY;
        let widgets = &mut visuals.widgets;
        for (state, fill, stroke) in [
            (&mut widgets.noninteractive, PANEL, OUTLINE),
            (&mut widgets.inactive, RAISED, OUTLINE),
            (
                &mut widgets.hovered,
                Color32::from_rgb(0x23, 0x2C, 0x40),
                ACCENT,
            ),
            (
                &mut widgets.active,
                Color32::from_rgb(0x2B, 0x35, 0x4D),
                ACCENT,
            ),
            (&mut widgets.open, RAISED, ACCENT),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, stroke);
            state.corner_radius = CornerRadius::same(6);
        }
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
        widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        widgets.active.fg_stroke = Stroke::new(1.5, TEXT);
        widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    });
}

/// How the mirror status reads at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Idle,
    Busy,
    Live,
    Problem,
}

impl Tone {
    pub fn of(status: &Status) -> Self {
        match status {
            Status::Stopped => Self::Idle,
            Status::Running => Self::Live,
            Status::Starting
            | Status::Restarting(_)
            | Status::WaitingForCardDisplay
            | Status::WaitingForSourceMonitor => Self::Busy,
            Status::Error(_) => Self::Problem,
        }
    }

    pub fn colour(self) -> Color32 {
        match self {
            Self::Idle => IDLE,
            Self::Busy => BUSY,
            Self::Live => LIVE,
            Self::Problem => PROBLEM,
        }
    }
}

/// The status indicator moves only while the mirror runs or is being started.
pub fn animates(status: &Status) -> bool {
    matches!(
        status,
        Status::Starting | Status::Running | Status::Restarting(_)
    )
}

/// When the window repaints by itself next, or `None` to wait for input or a report: about 30 Hz
/// while it animates and has focus, once a second for the frame rate of a running mirror while
/// the game has focus, and never while minimised or hidden, so the window costs the sender nothing then.
pub fn next_repaint(status: &Status, visible: bool, focused: bool) -> Option<Duration> {
    match () {
        _ if !visible || !animates(status) => None,
        _ if focused => Some(ANIMATION_FRAME),
        _ if *status == Status::Running => Some(UNFOCUSED_FRAME),
        _ => None,
    }
}

/// Where in a `period`-second cycle `time` is, from 0 up to 1.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a fraction from 0 to 1 fits an f32"
)]
pub fn phase(time: f64, period: f64) -> f32 {
    (time / period).rem_euclid(1.0) as f32
}

/// How long the mirror keeps up with too few refreshes before the status line says so.
pub const SLOW_FOR: Duration = Duration::from_secs(3);

/// Frames per second shown on the card display, from the mirror's count of presented frames.
#[derive(Debug, Default)]
pub struct FrameRate {
    since: Option<(u64, Instant)>,
    pub shown: Option<f32>,
    /// The card display's refresh rate at the last sample.
    refresh_hz: u32,
    /// Since when the shown rate is under 90% of the card display's refresh rate.
    slow_since: Option<Instant>,
}

impl FrameRate {
    /// Measures over at least half a second, so a 30 Hz repaint does not make the number jitter.
    /// `refresh_hz` is the card display's; 0 if unknown.
    pub fn sample(&mut self, presents: u64, refresh_hz: u32, now: Instant) {
        self.refresh_hz = refresh_hz;
        match self.since {
            // The count only goes down when a restarted mirror started counting again.
            Some((before, at)) if presents >= before => {
                let elapsed = now.saturating_duration_since(at);
                if elapsed >= Duration::from_millis(500) {
                    let rate = (presents - before) as f32 / elapsed.as_secs_f32();
                    self.shown = Some(rate);
                    self.since = Some((presents, now));
                    self.slow_since =
                        (rate < 0.9 * refresh_hz as f32).then(|| self.slow_since.unwrap_or(now));
                }
            }
            _ => self.since = Some((presents, now)),
        }
    }

    /// The status line's warning once the mirror has stayed under 90% of the card display's
    /// refresh rate for [`SLOW_FOR`]: the mirror presents once per refresh, so it misses refreshes.
    pub fn slow_warning(&self, now: Instant) -> Option<String> {
        let (rate, since) = (self.shown?, self.slow_since?);
        let hz = self.refresh_hz;
        (now.saturating_duration_since(since) >= SLOW_FOR).then(|| {
            if hz <= 60 {
                format!(
                    "The mirror shows only {rate:.0} frames a second on a {hz} Hz card display: \
                     the sender can't keep up."
                )
            } else {
                format!(
                    "The mirror shows only {rate:.0} frames a second on a {hz} Hz card display. \
                     Set a lower refresh rate for the card display in Windows."
                )
            }
        })
    }
}

/// What a monitor can be chosen as; the dropdowns and the live layout share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    SourceMonitor,
    CardDisplay,
    SendWindowsTo,
}

impl Role {
    pub const ALL: [Self; 3] = [Self::SourceMonitor, Self::CardDisplay, Self::SendWindowsTo];

    pub fn label(self) -> &'static str {
        match self {
            Self::SourceMonitor => "Source monitor",
            Self::CardDisplay => "Card display",
            Self::SendWindowsTo => "Send windows to",
        }
    }

    pub fn colour(self) -> Color32 {
        match self {
            Self::SourceMonitor => SKY,
            Self::CardDisplay => ACCENT,
            Self::SendWindowsTo => TEXT_DIM,
        }
    }

    /// The saved choice for this role: a monitor's device path.
    pub fn choice(self, settings: &mut Settings) -> &mut Option<String> {
        match self {
            Self::SourceMonitor => &mut settings.source_monitor,
            Self::CardDisplay => &mut settings.card_display,
            Self::SendWindowsTo => &mut settings.send_windows_to,
        }
    }

    /// Whether the dropdown and the role menu offer `monitor` for this role: never when it puts
    /// two roles on the card display, nor when it makes another [`settings::role_problem`]. A
    /// card display Windows made the primary monitor locks no other choice.
    pub fn offers(self, settings: &Settings, monitors: &[Monitor], monitor: &Monitor) -> bool {
        let mut chosen = settings.clone();
        *self.choice(&mut chosen) = Some(monitor.device_path.clone());
        let shared = chosen.card_display.is_some()
            && (chosen.source_monitor == chosen.card_display
                || chosen.send_windows_to == chosen.card_display);
        let problem = settings::role_problem(&chosen, monitors);
        !shared && (problem.is_none() || problem == settings::role_problem(settings, monitors))
    }

    /// Whether `monitor` is chosen for this role.
    pub fn is(self, settings: &Settings, monitor: &Monitor) -> bool {
        let chosen = match self {
            Self::SourceMonitor => &settings.source_monitor,
            Self::CardDisplay => &settings.card_display,
            Self::SendWindowsTo => &settings.send_windows_to,
        };
        chosen.as_deref() == Some(monitor.device_path.as_str())
    }
}

/// Where each monitor is drawn in `area`: the Windows desktop arrangement, to scale and centred,
/// each shrunk by `gap` so touching monitors stay apart. Any graphics card's monitors count alike.
pub fn fit_layout(monitors: &[Monitor], area: Rect, gap: f32) -> Vec<Rect> {
    let desktop = |monitor: &Monitor| {
        Rect::from_min_size(
            pos2(monitor.left as f32, monitor.top as f32),
            vec2(monitor.layout_size.0 as f32, monitor.layout_size.1 as f32),
        )
    };
    let Some(bounds) = monitors.iter().map(desktop).reduce(|a, b| a.union(b)) else {
        return Vec::new();
    };
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return Vec::new();
    }
    let scale = (area.width() / bounds.width()).min(area.height() / bounds.height());
    let offset = area.center() - bounds.center() * scale;
    monitors
        .iter()
        .map(|monitor| {
            let rect = desktop(monitor);
            Rect::from_min_max(rect.min * scale + offset, rect.max * scale + offset).shrink(gap)
        })
        .collect()
}

/// The live layout's height in a window too small to spare more.
pub const LAYOUT_HEIGHT: f32 = 180.0;

/// The live layout's height beyond [`LAYOUT_HEIGHT`] for the next pass: last pass's `extra`,
/// grown by the height the window had spare below the choices (`shown` minus `content`), or
/// shrunk by what did not fit, down to none. Whole points, so it settles instead of flickering.
pub fn layout_extra(extra: f32, shown: f32, content: f32) -> f32 {
    (extra + shown - content).max(0.0).round()
}

/// Where the live layout draws a chosen card display that is not connected: a 1080p screen a gap
/// right of the rightmost monitor, level with its top. Its `device_path` is empty.
pub fn card_display_placeholder(monitors: &[Monitor]) -> Monitor {
    let rightmost = monitors
        .iter()
        .max_by_key(|monitor| monitor.left.saturating_add_unsigned(monitor.layout_size.0));
    let (left, top) = rightmost.map_or((0, 0), |monitor| {
        (
            monitor
                .left
                .saturating_add_unsigned(monitor.layout_size.0)
                .saturating_add(240),
            monitor.top,
        )
    });
    Monitor {
        device_path: String::new(),
        gdi_name: String::new(),
        name: "Card display: not connected".into(),
        width: 1920,
        height: 1080,
        layout_size: (1920, 1080),
        refresh_hz: 0,
        primary: false,
        left,
        top,
        graphics_card: String::new(),
        container_id: String::new(),
        connector: String::new(),
    }
}

/// Whether the picture goes from one graphics card to another (ADR 0003). An unknown card is
/// taken to be the same one.
pub fn crosses_cards(source_monitor: &Monitor, card_display: &Monitor) -> bool {
    !source_monitor.graphics_card.is_empty()
        && !card_display.graphics_card.is_empty()
        && source_monitor.graphics_card != card_display.graphics_card
}

/// The short word the header shows for the mirror; the status line below says the rest.
pub fn headline(status: &Status) -> &'static str {
    match status {
        Status::Stopped => "Stopped",
        Status::Starting => "Starting",
        Status::Running => "Live",
        Status::WaitingForCardDisplay => "Waiting for card display",
        Status::WaitingForSourceMonitor => "Waiting for source monitor",
        Status::Restarting(_) => "Restarting",
        Status::Error(_) => "Stopped by an error",
    }
}

/// The picture's path in the live layout: an arc from `from` to `to`, raised by `lift`, as `steps` + 1 points.
pub fn arc_path(from: egui::Pos2, to: egui::Pos2, lift: f32, steps: u16) -> Vec<egui::Pos2> {
    let control = from.lerp(to, 0.5) - vec2(0.0, lift);
    (0..=steps)
        .map(|step| {
            let t = f32::from(step) / f32::from(steps.max(1));
            from.lerp(control, t).lerp(control.lerp(to, t), t)
        })
        .collect()
}

/// The point `t` (0 to 1) of the way along `points`, by index; close enough for evenly sampled arcs.
pub fn along(points: &[egui::Pos2], t: f32) -> Option<egui::Pos2> {
    let last = points.len().checked_sub(1)?;
    let at = t.clamp(0.0, 1.0) * last as f32;
    let index = (0..last)
        .find(|&index| at < (index + 1) as f32)
        .unwrap_or(last);
    let next = points.get(index + 1).unwrap_or(&points[index]);
    Some(points[index].lerp(*next, at - index as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_arcs_from_the_source_monitor_to_the_card_display() {
        let (from, to) = (pos2(0.0, 100.0), pos2(200.0, 100.0));
        let path = arc_path(from, to, 40.0, 10);
        assert_eq!(path.len(), 11);
        assert_eq!((path[0], path[10]), (from, to));
        assert!(
            (path[5] - pos2(100.0, 80.0)).length() < 1e-3,
            "{:?}",
            path[5]
        );
        assert_eq!(along(&path, 0.0), Some(from));
        assert_eq!(along(&path, 1.0), Some(to));
        assert!((along(&path, 0.5).unwrap() - path[5]).length() < 1e-3);
        assert_eq!(along(&[], 0.5), None);
    }

    fn monitor(device_path: &str, left: i32, top: i32, width: u32, height: u32) -> Monitor {
        Monitor {
            device_path: device_path.into(),
            gdi_name: String::new(),
            name: device_path.into(),
            width,
            height,
            layout_size: (width, height),
            refresh_hz: 60,
            primary: left == 0 && top == 0,
            left,
            top,
            graphics_card: String::new(),
            container_id: String::new(),
            connector: String::new(),
        }
    }

    #[test]
    fn layout_keeps_the_arrangement_to_scale_and_centred() {
        // A 1440p source monitor left of the primary monitor, the card display to its right.
        let monitors = [
            monitor("source", -2560, -360, 2560, 1440),
            monitor("primary", 0, 0, 1920, 1080),
            monitor("card", 1920, 0, 1920, 1080),
        ];
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(640.0, 200.0));
        let rects = fit_layout(&monitors, area, 0.0);
        // The desktop is 6400×1440; the area's width limits the scale to 0.1.
        let scale = 0.1;
        assert!((rects[0].width() - 2560.0 * scale).abs() < 1e-3);
        assert!((rects[2].height() - 1080.0 * scale).abs() < 1e-3);
        assert!((rects[0].right() - rects[1].left()).abs() < 1e-3);
        assert!((rects[1].right() - rects[2].left()).abs() < 1e-3);
        assert!((rects[1].top() - rects[0].top() - 360.0 * scale).abs() < 1e-3);
        let drawn = rects
            .iter()
            .fold(Rect::NOTHING, |all, rect| all.union(*rect));
        assert!((drawn.center() - area.center()).length() < 1e-3);
        assert!(area.expand(1e-3).contains_rect(drawn));
    }

    #[test]
    fn a_scaled_monitor_is_drawn_at_its_size_on_the_desktop() {
        // The sender's DP-1 on Plasma: 1440×2560 at scale 1.3 takes 1108×1969 of the desktop.
        let scaled = Monitor {
            layout_size: (1108, 1969),
            ..monitor("scaled", 0, 0, 1440, 2560)
        };
        let monitors = [scaled, monitor("right", 1108, 0, 1920, 1080)];
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(3028.0, 1969.0));
        let rects = fit_layout(&monitors, area, 0.0);
        assert!((rects[0].width() - 1108.0).abs() < 1e-3);
        assert!((rects[0].right() - rects[1].left()).abs() < 1e-3);
        let placeholder = card_display_placeholder(&monitors[..1]);
        assert_eq!(placeholder.left, 1108 + 240);
    }

    #[test]
    fn a_tall_arrangement_is_limited_by_the_height() {
        let monitors = [
            monitor("primary", 0, 0, 1920, 1080),
            monitor("card", 0, 1080, 1920, 1080),
        ];
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 216.0));
        let rects = fit_layout(&monitors, area, 2.0);
        assert!((rects[0].height() - (108.0 - 4.0)).abs() < 1e-3);
        assert!((rects[1].top() - rects[0].bottom() - 4.0).abs() < 1e-3);
    }

    #[test]
    fn a_missing_card_display_is_drawn_beside_the_monitors() {
        let monitors = [
            monitor("side", -2560, 0, 2560, 1440),
            monitor("primary", 0, 0, 2560, 1440),
            monitor("source", 2560, 180, 1920, 1080),
        ];
        let placeholder = card_display_placeholder(&monitors);
        assert_eq!(
            (placeholder.left, placeholder.top),
            (4480 + 240, 180),
            "a gap right of the rightmost monitor, level with its top"
        );
        assert_eq!((placeholder.width, placeholder.height), (1920, 1080));
        assert_eq!(
            card_display_placeholder(&[]).left,
            0,
            "no monitors: it stands alone"
        );
    }

    #[test]
    fn the_picture_crosses_graphics_cards_only_between_known_different_cards() {
        let on = |card: &str| Monitor {
            graphics_card: card.into(),
            ..monitor("m", 0, 0, 1, 1)
        };
        assert!(crosses_cards(&on("RTX 4080"), &on("Integrated GPU")));
        assert!(!crosses_cards(&on("RTX 4080"), &on("RTX 4080")));
        assert!(!crosses_cards(&on(""), &on("RTX 4080")), "unknown card");
    }

    #[test]
    fn the_live_layout_takes_the_spare_height_and_gives_it_back() {
        assert_eq!(
            layout_extra(0.0, 500.0, 400.0),
            100.0,
            "a band of 100 spare"
        );
        assert_eq!(
            layout_extra(100.0, 500.0, 500.0),
            100.0,
            "it fills the window"
        );
        assert_eq!(layout_extra(100.0, 450.0, 500.0), 50.0, "the window shrank");
        assert_eq!(
            layout_extra(50.0, 300.0, 500.0),
            0.0,
            "too small: it scrolls"
        );
        assert_eq!(layout_extra(0.0, 500.4, 400.0), 100.0, "whole points");
    }

    #[test]
    fn no_monitors_draw_nothing() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        assert!(fit_layout(&[], area, 0.0).is_empty());
        assert!(fit_layout(&[monitor("empty", 0, 0, 0, 0)], area, 0.0).is_empty());
    }

    #[test]
    fn a_monitor_shows_every_role_it_is_chosen_for() {
        let mut settings = Settings::default();
        *Role::CardDisplay.choice(&mut settings) = Some("card".into());
        *Role::SendWindowsTo.choice(&mut settings) = Some("card".into());
        let card = monitor("card", 0, 0, 1, 1);
        let roles: Vec<Role> = Role::ALL
            .into_iter()
            .filter(|role| role.is(&settings, &card))
            .collect();
        assert_eq!(roles, [Role::CardDisplay, Role::SendWindowsTo]);
        assert_eq!(settings.card_display.as_deref(), Some("card"));
    }

    /// The source monitor left of the primary monitor, the card display right of it.
    fn sender() -> (Vec<Monitor>, Settings) {
        let monitors = vec![
            monitor("source", -1920, 0, 1920, 1080),
            monitor("primary", 0, 0, 1920, 1080),
            monitor("card", 1920, 0, 1920, 1080),
            monitor("spare", 3840, 0, 1920, 1080),
        ];
        let settings = Settings {
            source_monitor: Some("source".into()),
            card_display: Some("card".into()),
            send_windows_to: Some("primary".into()),
            ..Settings::default()
        };
        (monitors, settings)
    }

    fn offered(role: Role, monitors: &[Monitor], settings: &Settings) -> Vec<String> {
        monitors
            .iter()
            .filter(|monitor| role.offers(settings, monitors, monitor))
            .map(|monitor| monitor.device_path.clone())
            .collect()
    }

    #[test]
    fn a_role_is_never_offered_on_a_monitor_it_would_clash_with() {
        let (monitors, settings) = sender();
        assert_eq!(
            offered(Role::CardDisplay, &monitors, &settings),
            ["card", "spare"],
            "never the source monitor, the primary or the send windows to monitor"
        );
        assert_eq!(
            offered(Role::SourceMonitor, &monitors, &settings),
            ["source", "primary", "spare"]
        );
        assert_eq!(
            offered(Role::SendWindowsTo, &monitors, &settings),
            ["source", "primary", "spare"]
        );
        // Nothing chosen yet: only the primary monitor can't be the card display.
        assert_eq!(
            offered(Role::CardDisplay, &monitors, &Settings::default()),
            ["source", "card", "spare"]
        );
    }

    #[test]
    fn a_card_display_that_became_the_primary_monitor_locks_no_other_choice() {
        let (mut monitors, settings) = sender();
        // Windows made the card display the primary monitor.
        monitors[1].primary = false;
        monitors[2].primary = true;
        assert_eq!(
            offered(Role::SourceMonitor, &monitors, &settings),
            ["source", "primary", "spare"]
        );
        assert_eq!(
            offered(Role::CardDisplay, &monitors, &settings),
            ["card", "spare"],
            "the current choice stays, and a new one fixes it"
        );
        assert_eq!(
            offered(Role::SendWindowsTo, &monitors, &settings),
            ["source", "primary", "spare"],
            "never the card display"
        );
    }

    #[test]
    fn the_window_animates_at_30_hz_only_while_seen_with_focus() {
        let running = Status::Running;
        assert_eq!(next_repaint(&running, true, true), Some(ANIMATION_FRAME));
        assert!(ANIMATION_FRAME >= Duration::from_secs(1) / 31);
        assert_eq!(next_repaint(&running, true, false), Some(UNFOCUSED_FRAME));
        assert_eq!(next_repaint(&running, false, true), None, "minimised");
        for busy in [Status::Starting, Status::Restarting("lost".into())] {
            assert_eq!(next_repaint(&busy, true, true), Some(ANIMATION_FRAME));
            // Without focus they have no frame rate to show and the animation holds still.
            assert_eq!(next_repaint(&busy, true, false), None, "{busy:?}");
        }
        for still in [
            Status::Stopped,
            Status::WaitingForCardDisplay,
            Status::WaitingForSourceMonitor,
            Status::Error("failed".into()),
        ] {
            assert_eq!(next_repaint(&still, true, true), None, "{still:?}");
        }
    }

    #[test]
    fn frame_rate_is_measured_over_half_a_second_and_survives_a_restart() {
        let start = Instant::now();
        let mut rate = FrameRate::default();
        rate.sample(0, 60, start);
        rate.sample(2, 60, start + Duration::from_millis(33));
        assert_eq!(rate.shown, None, "measured over too short a time");
        rate.sample(60, 60, start + Duration::from_secs(1));
        assert_eq!(rate.shown, Some(60.0));
        // A restarted mirror counts from 0 again.
        rate.sample(0, 60, start + Duration::from_secs(2));
        assert_eq!(rate.shown, Some(60.0));
        rate.sample(30, 60, start + Duration::from_secs(3));
        assert_eq!(rate.shown, Some(30.0));
    }

    #[test]
    fn a_mirror_below_90_percent_of_the_refresh_for_3_seconds_says_so() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut rate = FrameRate::default();
        rate.sample(0, 120, at(0));
        // 100 fps on a 120 Hz card display: under 108.
        rate.sample(50, 120, at(500));
        assert_eq!(rate.shown, Some(100.0));
        rate.sample(250, 120, at(2500));
        assert_eq!(
            rate.slow_warning(at(2500)),
            None,
            "not slow for long enough"
        );
        rate.sample(350, 120, at(3500));
        assert_eq!(
            rate.slow_warning(at(3500)).as_deref(),
            Some(
                "The mirror shows only 100 frames a second on a 120 Hz card display. \
                 Set a lower refresh rate for the card display in Windows."
            )
        );
        // Keeping up again clears it at once.
        rate.sample(470, 120, at(4500));
        assert_eq!(rate.slow_warning(at(4500)), None);
    }

    #[test]
    fn a_slow_mirror_at_60_hz_or_below_says_the_sender_cannot_keep_up() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut rate = FrameRate::default();
        for second in 0..=4 {
            rate.sample(40 * second, 60, at(second));
        }
        assert_eq!(
            rate.slow_warning(at(4)).as_deref(),
            Some(
                "The mirror shows only 40 frames a second on a 60 Hz card display: \
                 the sender can't keep up."
            )
        );
    }

    #[test]
    fn a_mirror_that_keeps_up_with_any_refresh_rate_is_not_slow() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        // 55 fps on 60 Hz, 120 on 120 Hz, 240 on 240 Hz, and a card display of unknown refresh.
        for (fps, hz) in [(55, 60), (120, 120), (240, 240), (30, 0)] {
            let mut rate = FrameRate::default();
            for second in 0..=10 {
                rate.sample(fps * second, hz, at(second));
            }
            assert_eq!(rate.slow_warning(at(10)), None, "{fps} fps at {hz} Hz");
        }
    }

    #[test]
    fn phase_wraps_from_0_to_1() {
        assert!((phase(0.5, 2.0) - 0.25).abs() < 1e-6);
        assert!((phase(5.0, 2.0) - 0.5).abs() < 1e-6);
        assert!((0.0..1.0).contains(&phase(1e9 + 0.3, 1.7)));
    }

    #[test]
    fn status_tones() {
        assert_eq!(Tone::of(&Status::Running), Tone::Live);
        assert_eq!(Tone::of(&Status::Stopped), Tone::Idle);
        assert_eq!(Tone::of(&Status::WaitingForCardDisplay), Tone::Busy);
        assert_eq!(Tone::of(&Status::Error("x".into())), Tone::Problem);
    }
}
