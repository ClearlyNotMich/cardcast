//! Painter-drawn parts of the settings window: the header, the status indicator, the live layout
//! and the guard switch. The decisions they draw live in `look`, with tests. No Windows calls.

use super::look::{self, Role, Tone};
use super::settings::{Settings, Status};
use crate::guard::Monitor;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, epaint::RectShape, pos2, vec2,
};

/// A titled, rounded panel.
pub fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::new()
        .fill(look::PANEL)
        .stroke(Stroke::new(1.0, look::OUTLINE))
        .corner_radius(12)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(caption(title));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(hint).small().color(look::TEXT_DIM));
                });
            });
            ui.add_space(4.0);
            add(ui)
        })
        .inner
}

/// A dot in `colour`; the default font has no "●".
pub fn dot(ui: &mut egui::Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, colour);
}

/// A label after a dot in `colour`; returns the label, to name the control it labels.
pub fn labelled_dot(ui: &mut egui::Ui, label: &str, colour: Color32) -> egui::Response {
    ui.horizontal(|ui| {
        dot(ui, colour);
        ui.label(RichText::new(label).color(colour))
    })
    .inner
}

fn caption(text: &str) -> RichText {
    RichText::new(text)
        .small()
        .strong()
        .extra_letter_spacing(1.2)
        .color(look::TEXT_DIM)
}

/// The top bar: the cardcast mark and name, and the mirror's state with the frame rate.
pub fn header(ui: &mut egui::Ui, status: &Status, frame_rate: Option<f32>, time: Option<f64>) {
    let rect = ui.max_rect();
    // A faint accent wash from the left, like the glow on capture-app title bars.
    let mut wash = egui::Mesh::default();
    let glow = look::ACCENT.gamma_multiply(0.16);
    for (corner, colour) in [
        (rect.left_top(), glow),
        (rect.right_top(), Color32::TRANSPARENT),
        (rect.right_bottom(), Color32::TRANSPARENT),
        (rect.left_bottom(), glow),
    ] {
        wash.colored_vertex(corner, colour);
    }
    wash.add_triangle(0, 1, 2);
    wash.add_triangle(0, 2, 3);
    ui.painter().add(wash);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, look::OUTLINE),
    );

    ui.horizontal_centered(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(30.0, 24.0), Sense::hover());
        logo(ui.painter(), mark);
        ui.label(
            RichText::new("cardcast")
                .size(22.0)
                .strong()
                .color(look::TEXT),
        );
        ui.label(RichText::new("sender").small().color(look::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            status_pill(ui, status, frame_rate, time);
        });
    });
}

/// Two overlapping screens, the source monitor's and the card display's colours.
fn logo(painter: &egui::Painter, rect: Rect) {
    let screen = |min: Pos2, colour: Color32| {
        let screen = Rect::from_min_size(min, vec2(20.0, 14.0));
        painter.rect_filled(screen, 3, colour.gamma_multiply(0.25));
        painter.rect_stroke(screen, 3, Stroke::new(2.0, colour), StrokeKind::Inside);
    };
    screen(rect.left_top() + vec2(0.0, 2.0), look::SKY);
    screen(rect.left_top() + vec2(9.0, 8.0), look::ACCENT);
}

fn status_pill(ui: &mut egui::Ui, status: &Status, frame_rate: Option<f32>, time: Option<f64>) {
    let tone = Tone::of(status);
    egui::Frame::new()
        .fill(tone.colour().gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, tone.colour().gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            // The header lays the pill out from the right, so its parts are added last first.
            ui.horizontal(|ui| {
                if let (Status::Running, Some(rate)) = (status, frame_rate) {
                    ui.label(
                        RichText::new(format!("{rate:.0} fps"))
                            .monospace()
                            .color(look::TEXT),
                    );
                    ui.label(RichText::new("·").color(look::TEXT_DIM));
                }
                ui.label(
                    RichText::new(look::headline(status).to_uppercase())
                        .strong()
                        .extra_letter_spacing(0.8)
                        .color(tone.colour()),
                );
                let (dot, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                indicator(ui.painter(), dot.center(), status, time);
            });
        });
}

/// A dot in the status colour: a ring pulses out of it while the mirror runs, and an arc circles
/// it while it starts. `time` is `None` while the window lacks focus, which freezes it.
pub fn indicator(painter: &egui::Painter, centre: Pos2, status: &Status, time: Option<f64>) {
    let colour = Tone::of(status).colour();
    let animated = look::animates(status);
    painter.add(
        RectShape::filled(
            Rect::from_center_size(centre, vec2(10.0, 10.0)),
            5,
            colour.gamma_multiply(0.6),
        )
        .with_blur_width(10.0),
    );
    painter.circle_filled(centre, 4.5, colour);
    match (status, time) {
        (Status::Running, Some(time)) => {
            let phase = look::phase(time, 1.6);
            painter.circle_stroke(
                centre,
                4.5 + 5.0 * phase,
                Stroke::new(1.5, colour.gamma_multiply(1.0 - phase)),
            );
        }
        (_, Some(time)) if animated => {
            let start = look::phase(time, 1.0) * std::f32::consts::TAU;
            let arc: Vec<Pos2> = (0..=12_u8)
                .map(|step| {
                    let angle = start + f32::from(step) * 0.35;
                    centre + 8.0 * vec2(angle.cos(), angle.sin())
                })
                .collect();
            painter.add(Shape::line(arc, Stroke::new(1.5, colour)));
        }
        _ if animated => {
            painter.circle_stroke(centre, 8.0, Stroke::new(1.0, colour.gamma_multiply(0.4)));
        }
        _ => {}
    }
}

/// The live layout: the sender's monitors as Windows arranges them, to scale. Each shows the roles
/// it is chosen for, and clicking it chooses them; while the mirror runs, the picture travels
/// along an arc from the source monitor to the card display.
pub fn layout_map(
    ui: &mut egui::Ui,
    monitors: &[Monitor],
    settings: &mut Settings,
    status: &Status,
    time: Option<f64>,
) {
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 180.0), Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 10, look::BACKGROUND);
    let mut x = area.left() + 24.0;
    while x < area.right() {
        painter.vline(x, area.y_range(), Stroke::new(1.0, look::PANEL));
        x += 24.0;
    }
    let mut y = area.top() + 24.0;
    while y < area.bottom() {
        painter.hline(area.x_range(), y, Stroke::new(1.0, look::PANEL));
        y += 24.0;
    }
    if monitors.is_empty() {
        painter.text(
            area.center(),
            Align2::CENTER_CENTER,
            "No monitors found",
            FontId::proportional(14.0),
            look::TEXT_DIM,
        );
        return;
    }
    let rects = look::fit_layout(monitors, area.shrink2(vec2(20.0, 24.0)), 3.0);
    let live = *status == Status::Running;
    let mut ends = [None, None];
    for (monitor, rect) in monitors.iter().zip(&rects) {
        let roles: Vec<Role> = Role::ALL
            .into_iter()
            .filter(|role| role.is(settings, monitor))
            .collect();
        for (end, role) in ends
            .iter_mut()
            .zip([Role::SourceMonitor, Role::CardDisplay])
        {
            if roles.contains(&role) {
                *end = Some(*rect);
            }
        }
        let response = ui
            .interact(
                *rect,
                ui.scope_id().with(&monitor.device_path),
                Sense::click(),
            )
            .on_hover_text("Click to choose what this monitor is for");
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &monitor.name));
        monitor_tile(&painter, monitor, *rect, &roles, live, response.hovered());
        egui::Popup::menu(&response).show(|ui| {
            ui.label(RichText::new(&monitor.name).strong());
            for role in Role::ALL {
                ui.selectable_value(
                    role.choice(settings),
                    Some(monitor.device_path.clone()),
                    RichText::new(role.label()).color(role.colour()),
                );
            }
        });
    }
    if let [Some(source), Some(card)] = ends {
        link(&painter, source.center_top(), card.center_top(), live, time);
    }
}

fn monitor_tile(
    painter: &egui::Painter,
    monitor: &Monitor,
    rect: Rect,
    roles: &[Role],
    live: bool,
    hovered: bool,
) {
    let accent = roles
        .iter()
        .find(|role| **role != Role::SendWindowsTo)
        .map(|role| role.colour());
    if let Some(colour) = accent {
        painter.add(RectShape::filled(rect, 8, colour.gamma_multiply(0.22)).with_blur_width(18.0));
    }
    let fill = match accent {
        // The card display shows the mirror, so it takes the source monitor's tint while live.
        Some(_) if live && roles.contains(&Role::CardDisplay) => look::SKY.gamma_multiply(0.18),
        Some(colour) => colour.gamma_multiply(0.10),
        None => look::RAISED,
    };
    painter.rect_filled(rect, 8, look::RAISED);
    painter.rect_filled(rect, 8, fill);
    let stroke = match (hovered, accent) {
        (true, _) => Stroke::new(2.0, look::TEXT),
        (false, Some(colour)) => Stroke::new(2.0, colour),
        (false, None) => Stroke::new(1.0, look::OUTLINE),
    };
    painter.rect_stroke(rect, 8, stroke, StrokeKind::Inside);

    let painter = painter.with_clip_rect(rect.shrink(3.0));
    let top = rect.center_top() + vec2(0.0, 8.0);
    painter.text(
        top,
        Align2::CENTER_TOP,
        &monitor.name,
        FontId::proportional(13.0),
        look::TEXT,
    );
    let primary = if monitor.primary { " · primary" } else { "" };
    painter.text(
        top + vec2(0.0, 18.0),
        Align2::CENTER_TOP,
        format!(
            "{}×{} · {} Hz{primary}",
            monitor.width, monitor.height, monitor.refresh_hz
        ),
        FontId::proportional(10.5),
        look::TEXT_DIM,
    );
    // Badges stack up from the bottom; a tile too small for them shows its roles by its outline.
    let mut badge_bottom = rect.bottom() - 7.0;
    for role in roles.iter().rev() {
        let text = painter.layout_no_wrap(
            role.label().to_owned(),
            FontId::proportional(10.0),
            role.colour(),
        );
        let size = text.size() + vec2(12.0, 4.0);
        if badge_bottom - size.y < top.y + 32.0 {
            break;
        }
        let badge =
            Rect::from_center_size(pos2(rect.center().x, badge_bottom - size.y / 2.0), size);
        painter.rect_filled(badge, 255, role.colour().gamma_multiply(0.18));
        painter.galley(badge.min + vec2(6.0, 2.0), text, role.colour());
        badge_bottom = badge.top() - 3.0;
    }
}

/// The arc the picture takes from the source monitor to the card display; dots travel along it while live.
fn link(painter: &egui::Painter, from: Pos2, to: Pos2, live: bool, time: Option<f64>) {
    let lift = 22.0 + from.distance(to) * 0.12;
    let path = look::link_path(from, to, lift, 32);
    if !live {
        painter.add(Shape::dashed_line(
            &path,
            Stroke::new(1.5, look::OUTLINE),
            6.0,
            5.0,
        ));
        return;
    }
    painter.add(Shape::line(
        path.clone(),
        Stroke::new(2.0, look::SKY.gamma_multiply(0.45)),
    ));
    let phase = time.map_or(0.0, |time| look::phase(time, 1.4));
    for dot in 0..4_u8 {
        let t = (phase + f32::from(dot) / 4.0).fract();
        if let Some(at) = look::along(&path, t) {
            painter.add(
                RectShape::filled(
                    Rect::from_center_size(at, vec2(8.0, 8.0)),
                    4,
                    look::SKY.gamma_multiply(0.5),
                )
                .with_blur_width(8.0),
            );
            painter.circle_filled(at, 2.5, look::TEXT);
        }
    }
    painter.circle_filled(to, 3.5, look::ACCENT);
}

/// A sliding switch, read by screen readers as the checkbox it replaces.
pub fn switch(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(44.0, 24.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, label)
    });
    let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = look::IDLE.lerp_to_gamma(look::LIVE, how_on);
    ui.painter()
        .rect_filled(rect, 255, track.gamma_multiply(0.35));
    ui.painter()
        .rect_stroke(rect, 255, Stroke::new(1.0, track), StrokeKind::Inside);
    let knob = pos2(
        egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, how_on),
        rect.center().y,
    );
    ui.painter().circle_filled(knob, 8.0, look::TEXT);
    response
}
