//! Painter-drawn parts of the settings window: the header, the status indicator, the live layout
//! and the guard switch. The decisions they draw live in `look`, with tests. No Windows calls.

use super::look::{self, Role, Tone};
use super::settings::{self, Settings, Status};
use crate::guard::Monitor;
use crate::input::{self, Job, MonitorId, Probe};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, epaint::RectShape, pos2, vec2,
};

/// A titled, rounded panel, with a hint or warning at the right of its title.
pub fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    hint: RichText,
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
                    ui.label(hint);
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

/// The top bar: the cardcast mark and name, the mirror's state with the frame rate, and the
/// settings cog with the input switching toggle and its `notice`.
pub fn header(
    ui: &mut egui::Ui,
    status: &Status,
    frame_rate: Option<f32>,
    time: Option<f64>,
    input_switching: &mut bool,
    notice: Option<&str>,
) {
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
            settings_cog(ui, input_switching, notice);
            status_pill(ui, status, frame_rate, time);
        });
    });
}

/// The cog at the top right; it opens a panel with the one toggle, "Input switching".
fn settings_cog(ui: &mut egui::Ui, input_switching: &mut bool, notice: Option<&str>) {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Settings"));
    let response = response.on_hover_text("Settings");
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    let colour = if open || response.hovered() {
        look::TEXT
    } else {
        look::TEXT_DIM
    };
    cog(ui.painter(), rect.center(), colour);
    egui::Popup::menu(&response)
        .align(egui::RectAlign::BOTTOM_END)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.label(caption("SETTINGS"));
            switch(
                ui,
                input_switching,
                "Input switching",
                "Switch monitor inputs over DDC/CI from the live layout",
            );
            if let (true, Some(notice)) = (*input_switching, notice) {
                ui.label(RichText::new(notice).small().color(look::BUSY));
            }
        });
}

/// A cog; the default font has none.
fn cog(painter: &egui::Painter, centre: Pos2, colour: Color32) {
    for tooth in 0..8_u8 {
        let angle = f32::from(tooth) * std::f32::consts::TAU / 8.0;
        let direction = vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [centre + direction * 5.0, centre + direction * 9.0],
            Stroke::new(3.5, colour),
        );
    }
    painter.circle_filled(centre, 6.5, colour);
    painter.circle_filled(centre, 2.5, look::PANEL);
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

/// What the live layout's input switch buttons show.
pub struct Inputs<'a> {
    /// The input lists last read, in the monitor list's order.
    pub lists: &'a [Probe],
    /// The current input of the monitor whose input menu opened last: its device path and the value.
    pub current: Option<&'a (String, u8)>,
}

/// The live layout: the sender's monitors as Windows arranges them, to scale, `height` high.
/// Each shows its graphics card and the roles it is chosen for, and clicking it chooses them; a chosen card display that is not connected is a dashed placeholder. While the
/// mirror runs, the picture travels along an arc from the source monitor to the card display.
/// A monitor with an input list gets an input switch button; returns what its menu asks of the input thread.
pub fn layout_map(
    ui: &mut egui::Ui,
    monitors: &[Monitor],
    settings: &mut Settings,
    status: &Status,
    time: Option<f64>,
    height: f32,
    inputs: &Inputs,
) -> Option<Job> {
    let size = vec2(ui.available_width(), height);
    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 10, look::BACKGROUND);
    let grid = Stroke::new(1.0, look::BACKGROUND.lerp_to_gamma(look::PANEL, 0.5));
    let mut x = area.left() + 32.0;
    while x < area.right() {
        painter.vline(x, area.y_range(), grid);
        x += 32.0;
    }
    let mut y = area.top() + 32.0;
    while y < area.bottom() {
        painter.hline(area.x_range(), y, grid);
        y += 32.0;
    }
    if monitors.is_empty() {
        painter.text(
            area.center(),
            Align2::CENTER_CENTER,
            "No monitors found",
            FontId::proportional(14.0),
            look::TEXT_DIM,
        );
        return None;
    }
    let connected = |path: &Option<String>| {
        monitors
            .iter()
            .find(|monitor| path.as_ref() == Some(&monitor.device_path))
    };
    let missing_card_display = (settings.card_display.is_some()
        && connected(&settings.card_display).is_none())
    .then(|| look::card_display_placeholder(monitors));
    let drawn: Vec<Monitor> = monitors
        .iter()
        .chain(&missing_card_display)
        .cloned()
        .collect();
    let rects = look::fit_layout(&drawn, area.shrink2(vec2(6.0, 34.0)), 3.0);
    let live = *status == Status::Running;
    let clash = settings::role_problem(settings, monitors).is_some();
    let mut ends = [None, None];
    let mut asked = None;
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
        let problem = clash && roles.contains(&Role::CardDisplay);
        monitor_tile(
            &painter,
            monitor,
            *rect,
            &roles,
            live,
            response.hovered(),
            problem,
        );
        role_menu(&response, monitor, monitors, settings);
        if let Some(index) = inputs
            .lists
            .iter()
            .position(|list| list.device_path == monitor.device_path)
        {
            asked = input_switch(ui, monitor, *rect, inputs, index).or(asked);
        }
    }
    if let (Some(placeholder), Some(rect)) = (&missing_card_display, rects.last()) {
        placeholder_tile(&painter, *rect);
        ui.interact(*rect, ui.scope_id().with("placeholder"), Sense::hover())
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &placeholder.name));
        ends[1] = Some(*rect);
    }
    if let [Some(source), Some(card)] = ends {
        let crosses = match (
            connected(&settings.source_monitor),
            connected(&settings.card_display),
        ) {
            (Some(source), Some(card)) => look::crosses_cards(source, card),
            _ => false,
        };
        picture_path(
            &painter,
            source.center_top(),
            card.center_top(),
            live,
            crosses,
            time,
        );
    }
    asked
}

/// The input switch button at a tile's bottom right, for a monitor with an input list (`index` in
/// `inputs.lists`). Its menu lists the inputs with the current one ticked, read as it opens; each
/// input switches the monitor, and its Copy command copies the command doing the same.
fn input_switch(
    ui: &mut egui::Ui,
    monitor: &Monitor,
    tile: Rect,
    inputs: &Inputs,
    index: usize,
) -> Option<Job> {
    let list = inputs.lists.get(index)?.inputs.as_deref()?;
    // Bottom right: role badges are narrower than names, so they leave the corner free.
    let rect = Rect::from_min_size(tile.right_bottom() - vec2(28.0, 28.0), vec2(20.0, 20.0));
    let response = ui.interact(
        rect,
        ui.scope_id().with(("inputs", &monitor.device_path)),
        Sense::click(),
    );
    let label = format!("Inputs of {}", monitor.name);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    let response = response.on_hover_text("Switch this monitor's input");
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    let mut asked = (response.clicked() && !open).then(|| Job::Read(monitor.clone()));
    let colour = if open || response.hovered() {
        look::TEXT
    } else {
        look::TEXT_DIM
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 5, look::BACKGROUND.gamma_multiply(0.85));
    painter.rect_stroke(rect, 5, Stroke::new(1.0, colour), StrokeKind::Inside);
    // Two arrows, one each way.
    let at = |x: f32, y: f32| rect.lerp_inside(vec2(x, y));
    for points in [
        [at(0.22, 0.36), at(0.78, 0.36), at(0.62, 0.22)],
        [at(0.78, 0.64), at(0.22, 0.64), at(0.38, 0.78)],
    ] {
        painter.add(Shape::line(points.to_vec(), Stroke::new(1.5, colour)));
    }

    let current = inputs
        .current
        .filter(|(device_path, _)| *device_path == monitor.device_path)
        .map(|(_, value)| *value);
    let ids: Vec<MonitorId> = inputs.lists.iter().map(|list| list.id.clone()).collect();
    let command_monitor = input::command_monitor(&ids, index);
    egui::Popup::menu(&response)
        .align(egui::RectAlign::RIGHT_START)
        .align_alternatives(&[egui::RectAlign::LEFT_START, egui::RectAlign::BOTTOM_START])
        .show(|ui| {
            ui.label(RichText::new(&monitor.name).strong().color(look::TEXT));
            egui::Grid::new(("input menu", &monitor.device_path))
                .num_columns(2)
                .show(ui, |ui| {
                    for &value in list {
                        let switch = ui
                            .horizontal(|ui| {
                                let (tick, _) =
                                    ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                if current == Some(value) {
                                    check_mark(ui.painter(), tick, look::LIVE);
                                }
                                ui.add(
                                    egui::Button::new(
                                        RichText::new(input::input_name(value)).color(look::TEXT),
                                    )
                                    .frame(false),
                                )
                            })
                            .inner;
                        if switch.clicked() && current != Some(value) {
                            asked = Some(Job::Switch(monitor.clone(), value));
                        }
                        if let Some(wanted) = command_monitor
                            && ui
                                .link(RichText::new("Copy command").small())
                                .on_hover_text("For a Stream Deck key or a shortcut")
                                .clicked()
                        {
                            ui.ctx().copy_text(input::copy_command(wanted, value));
                        }
                        ui.end_row();
                    }
                });
            if command_monitor.is_none() {
                ui.label(
                    RichText::new(
                        "No command can pick this monitor: another has the same serial and name.",
                    )
                    .small()
                    .color(look::TEXT_DIM),
                );
            }
        });
    asked
}

/// The roles a clicked monitor can take ([`Role::offers`]); the current ones are ticked. It opens
/// beside the tile, so it covers neither the tile nor the choices below.
fn role_menu(
    response: &egui::Response,
    monitor: &Monitor,
    monitors: &[Monitor],
    settings: &mut Settings,
) {
    egui::Popup::menu(response)
        .align(egui::RectAlign::RIGHT_START)
        .align_alternatives(&[egui::RectAlign::LEFT_START, egui::RectAlign::BOTTOM_START])
        .show(|ui| {
            ui.label(RichText::new(&monitor.name).strong().color(look::TEXT));
            for role in Role::ALL {
                if !role.offers(settings, monitors, monitor) {
                    continue;
                }
                let chosen = role.is(settings, monitor);
                let row = ui.horizontal(|ui| {
                    let (tick, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    if chosen {
                        check_mark(ui.painter(), tick, role.colour());
                    }
                    dot(ui, role.colour());
                    ui.add(
                        egui::Button::new(RichText::new(role.label()).color(look::TEXT))
                            .frame(false),
                    )
                });
                if row.inner.clicked() {
                    *role.choice(settings) = Some(monitor.device_path.clone());
                }
            }
        });
}

/// A tick; the default font has no "✔".
fn check_mark(painter: &egui::Painter, rect: Rect, colour: Color32) {
    let at = |x: f32, y: f32| rect.lerp_inside(vec2(x, y));
    painter.add(Shape::line(
        vec![at(0.1, 0.55), at(0.4, 0.85), at(0.9, 0.2)],
        Stroke::new(2.0, colour),
    ));
}

/// The chosen card display while it is not connected: a dashed outline where it would be.
fn placeholder_tile(painter: &egui::Painter, rect: Rect) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    painter.add(Shape::dashed_line(
        &corners,
        Stroke::new(1.5, look::ACCENT.gamma_multiply(0.7)),
        6.0,
        4.0,
    ));
    painter.with_clip_rect(rect.shrink(3.0)).text(
        rect.center(),
        Align2::CENTER_CENTER,
        "Card display\nnot connected",
        FontId::proportional(12.0),
        look::ACCENT,
    );
}

fn monitor_tile(
    painter: &egui::Painter,
    monitor: &Monitor,
    rect: Rect,
    roles: &[Role],
    live: bool,
    hovered: bool,
    problem: bool,
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
    // A dark bezel round the screen.
    painter.rect_filled(rect, 8, look::BACKGROUND);
    let screen = rect.shrink(4.0);
    painter.rect_filled(screen, 5, look::RAISED);
    painter.rect_filled(screen, 5, fill);
    let stroke = match (hovered, problem, accent) {
        (true, _, _) => Stroke::new(2.0, look::TEXT),
        (false, true, _) => Stroke::new(2.0, look::PROBLEM),
        (false, false, Some(colour)) => Stroke::new(2.0, colour),
        (false, false, None) => Stroke::new(1.0, look::OUTLINE),
    };
    painter.rect_stroke(rect, 8, stroke, StrokeKind::Inside);

    let painter = painter.with_clip_rect(screen.shrink(2.0));
    let mut line = screen.center_top() + vec2(0.0, 6.0);
    painter.text(
        line,
        Align2::CENTER_TOP,
        &monitor.name,
        FontId::proportional(13.0),
        look::TEXT,
    );
    let primary = if monitor.primary { " · primary" } else { "" };
    for detail in [
        format!(
            "{}×{} · {} Hz{primary}",
            monitor.width, monitor.height, monitor.refresh_hz
        ),
        monitor.graphics_card.clone(),
    ] {
        line += vec2(0.0, 15.0);
        painter.text(
            line,
            Align2::CENTER_TOP,
            detail,
            FontId::proportional(10.5),
            look::TEXT_DIM,
        );
    }
    // Badges stack up from the bottom; a tile too small for them shows its roles by its outline.
    let mut badge_bottom = screen.bottom() - 5.0;
    for role in roles.iter().rev() {
        let text = painter.layout_no_wrap(
            role.label().to_owned(),
            FontId::proportional(10.0),
            role.colour(),
        );
        let size = text.size() + vec2(12.0, 3.0);
        if badge_bottom - size.y < line.y + 14.0 {
            break;
        }
        let badge =
            Rect::from_center_size(pos2(screen.center().x, badge_bottom - size.y / 2.0), size);
        painter.rect_filled(badge, 255, role.colour().gamma_multiply(0.18));
        painter.galley(badge.min + vec2(6.0, 1.5), text, role.colour());
        badge_bottom = badge.top() - 3.0;
    }
}

/// The arc the picture takes from the source monitor to the card display; dots travel along it
/// while live. When it `crosses` graphics cards, its top says so.
fn picture_path(
    painter: &egui::Painter,
    from: Pos2,
    to: Pos2,
    live: bool,
    crosses: bool,
    time: Option<f64>,
) {
    let lift = 22.0 + from.distance(to) * 0.12;
    let path = look::arc_path(from, to, lift, 32);
    if let (true, Some(top)) = (crosses, look::along(&path, 0.5)) {
        painter.text(
            top - vec2(0.0, 4.0),
            Align2::CENTER_BOTTOM,
            "across graphics cards",
            FontId::proportional(10.5),
            look::TEXT_DIM,
        );
    }
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

/// A sliding switch with its title and a note beside it; a click anywhere on it toggles it.
/// Screen readers read it as the checkbox it replaces, named `title`.
pub fn switch(ui: &mut egui::Ui, on: &mut bool, title: &str, note: &str) -> egui::Response {
    let title =
        ui.painter()
            .layout_no_wrap(title.to_owned(), FontId::proportional(14.0), look::TEXT);
    let note =
        ui.painter()
            .layout_no_wrap(note.to_owned(), FontId::proportional(11.0), look::TEXT_DIM);
    let text_width = title.size().x.max(note.size().x);
    let height = (title.size().y + 2.0 + note.size().y).max(24.0);
    let (rect, mut response) =
        ui.allocate_exact_size(vec2(44.0 + 12.0 + text_width, height), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, title.text())
    });
    let track_rect =
        Rect::from_min_size(pos2(rect.left(), rect.center().y - 12.0), vec2(44.0, 24.0));
    let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = look::IDLE.lerp_to_gamma(look::LIVE, how_on);
    let painter = ui.painter();
    painter.rect_filled(track_rect, 255, track.gamma_multiply(0.35));
    painter.rect_stroke(track_rect, 255, Stroke::new(1.0, track), StrokeKind::Inside);
    let knob = pos2(
        egui::lerp(track_rect.left() + 12.0..=track_rect.right() - 12.0, how_on),
        track_rect.center().y,
    );
    painter.circle_filled(knob, 8.0, look::TEXT);
    let text_left = track_rect.right() + 12.0;
    let title_height = title.size().y;
    painter.galley(pos2(text_left, rect.top()), title, look::TEXT);
    painter.galley(
        pos2(text_left, rect.top() + title_height + 2.0),
        note,
        look::TEXT_DIM,
    );
    response
}
