//! Look and feel: warm "keycap" light theme, bundled fonts and the custom widgets used everywhere.

use std::sync::Arc;

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Layout, Margin, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

pub const PAGE: Color32 = Color32::from_rgb(0xF7, 0xF4, 0xEE);
pub const GROUND: Color32 = Color32::from_rgb(0xEC, 0xE8, 0xE1);
pub const KEY: Color32 = Color32::from_rgb(0xFB, 0xF9, 0xF5);
pub const EDGE: Color32 = Color32::from_rgb(0xCE, 0xC7, 0xBB);
pub const IDLE: Color32 = Color32::from_rgb(0xE3, 0xDE, 0xD5);
pub const HOVER: Color32 = Color32::from_rgb(0xF3, 0xEF, 0xE7);
pub const INK: Color32 = Color32::from_rgb(0x1B, 0x1B, 0x1F);
pub const MUTED: Color32 = Color32::from_rgb(0x5F, 0x5B, 0x55);
pub const LABEL: Color32 = Color32::from_rgb(0x49, 0x48, 0x4A);
pub const SIGNAL: Color32 = Color32::from_rgb(0xF0, 0x45, 0x2B);
pub const SIGNAL_EDGE: Color32 = Color32::from_rgb(0xA8, 0x2B, 0x17);
pub const SIGNAL_TEXT: Color32 = Color32::from_rgb(0xC8, 0x36, 0x1F);
pub const OK: Color32 = Color32::from_rgb(0x2E, 0x7D, 0x4F);
pub const WARN: Color32 = Color32::from_rgb(0xB0, 0x6A, 0x00);

pub const CONTENT_WIDTH: f32 = 930.0;
pub const COLUMN_GAP: f32 = 26.0;

fn family(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

pub fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, family("bold"))
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, family("medium"))
}
pub fn display(size: f32) -> FontId {
    FontId::new(size, family("display"))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn text(t: impl Into<String>, font: FontId, color: Color32) -> RichText {
    RichText::new(t).font(font).color(color)
}

/// Install fonts and visuals.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        &mut fonts,
        "onest",
        include_bytes!("../assets/fonts/Onest-Regular.ttf"),
    );
    add(
        &mut fonts,
        "onest-medium",
        include_bytes!("../assets/fonts/Onest-Medium.ttf"),
    );
    add(
        &mut fonts,
        "onest-bold",
        include_bytes!("../assets/fonts/Onest-Bold.ttf"),
    );
    add(
        &mut fonts,
        "unbounded",
        include_bytes!("../assets/fonts/Unbounded-ExtraBold.ttf"),
    );
    add(
        &mut fonts,
        "jetbrains",
        include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    );

    let fallback: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with_fallback = |first: &str| {
        let mut v = vec![first.to_string()];
        v.extend(fallback.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, with_fallback("onest"));
    fonts
        .families
        .insert(FontFamily::Monospace, with_fallback("jetbrains"));
    fonts
        .families
        .insert(family("bold"), with_fallback("onest-bold"));
    fonts
        .families
        .insert(family("medium"), with_fallback("onest-medium"));
    fonts
        .families
        .insert(family("display"), with_fallback("unbounded"));
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::light();
    v.panel_fill = PAGE;
    v.window_fill = KEY;
    v.window_stroke = Stroke::new(1.0, EDGE);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(12);
    v.extreme_bg_color = KEY;
    v.faint_bg_color = HOVER;
    v.code_bg_color = GROUND;
    v.hyperlink_color = INK;
    v.selection.bg_fill = Color32::from_rgb(0xF6, 0xC9, 0xBF);
    v.selection.stroke = Stroke::new(1.5, INK);
    v.text_cursor.stroke = Stroke::new(2.0, INK);
    v.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(30),
    };
    v.window_shadow = v.popup_shadow;

    let w = &mut v.widgets;
    w.noninteractive.fg_stroke = Stroke::new(1.0, INK);
    w.noninteractive.bg_stroke = Stroke::new(1.0, EDGE);
    w.noninteractive.bg_fill = KEY;
    for (s, fill, stroke) in [
        (&mut w.inactive, GROUND, MUTED),
        (&mut w.hovered, IDLE, INK),
        (&mut w.active, IDLE, INK),
        (&mut w.open, IDLE, INK),
    ] {
        s.weak_bg_fill = fill;
        s.bg_fill = fill;
        s.bg_stroke = Stroke::new(1.5, stroke);
        s.fg_stroke = Stroke::new(1.5, INK);
        s.corner_radius = CornerRadius::same(10);
        s.expansion = 0.0;
    }
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 7.0);
        s.spacing.interact_size = vec2(40.0, 30.0);
        s.spacing.combo_width = 220.0;
        s.spacing.slider_width = 190.0;
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.spacing.scroll.dormant_handle_opacity = 0.0;
        s.spacing.scroll.dormant_background_opacity = 0.0;
        s.text_styles.insert(egui::TextStyle::Body, body(15.0));
        s.text_styles.insert(egui::TextStyle::Button, bold(15.0));
        s.text_styles.insert(egui::TextStyle::Small, body(12.5));
        s.text_styles.insert(egui::TextStyle::Monospace, mono(13.0));
        s.text_styles
            .insert(egui::TextStyle::Heading, display(30.0));
    });
}

// ---------------------------------------------------------------------------- layout pieces

/// Our mark: a red key with a little mouse on it.
pub fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(126.0, 30.0), Sense::hover());
    let p = ui.painter();
    let icon = Rect::from_min_size(rect.left_top() + vec2(0.0, 1.0), vec2(28.0, 28.0));
    p.rect_filled(
        icon.translate(vec2(0.0, 2.0)),
        CornerRadius::same(7),
        SIGNAL_EDGE,
    );
    p.rect_filled(icon, CornerRadius::same(7), SIGNAL);
    let m = Rect::from_center_size(icon.center(), vec2(11.0, 16.0));
    p.rect(
        m,
        CornerRadius::same(5),
        INK,
        Stroke::NONE,
        StrokeKind::Inside,
    );
    p.line_segment(
        [
            pos2(m.center().x, m.top() + 3.0),
            pos2(m.center().x, m.top() + 7.0),
        ],
        Stroke::new(1.6, KEY),
    );
    p.text(
        pos2(icon.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        "XMouse",
        bold(19.0),
        INK,
    );
}

/// Top band with an optional back pill on the left and the logo in the middle. Returns true if back was clicked.
pub fn top_bar(ui: &mut Ui, back: Option<&str>, right: Option<String>) -> bool {
    let mut clicked = false;
    egui::Panel::top("top-bar")
        .exact_size(60.0)
        .frame(
            egui::Frame::new()
                .fill(GROUND)
                .inner_margin(Margin::symmetric(16, 0))
                .stroke(Stroke::NONE),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .hline(rect.x_range(), rect.bottom() + 0.5, Stroke::new(1.0, EDGE));
            ui.horizontal_centered(|ui| {
                if let Some(label) = back {
                    clicked = back_pill(ui, label).clicked();
                }
            });
            let center = rect.center();
            let mut child = ui.new_child(
                egui::UiBuilder::new().max_rect(Rect::from_center_size(center, vec2(126.0, 30.0))),
            );
            logo(&mut child);
            if let Some(r) = right {
                ui.painter().text(
                    pos2(rect.right(), center.y),
                    Align2::RIGHT_CENTER,
                    r,
                    mono(12.0),
                    LABEL,
                );
            }
        });
    clicked
}

/// Bottom status band in monospace.
pub fn status_bar(ui: &mut Ui, status: &str) {
    egui::Panel::bottom("status-bar")
        .exact_size(30.0)
        .frame(egui::Frame::new().fill(PAGE).inner_margin(Margin {
            left: 18,
            right: 18,
            top: 0,
            bottom: 6,
        }))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().rect_filled(r, CornerRadius::ZERO, GROUND);
            ui.painter()
                .hline(r.x_range(), r.top(), Stroke::new(1.0, EDGE));
            ui.painter().text(
                pos2(r.left() + 14.0, r.center().y),
                Align2::LEFT_CENTER,
                status,
                mono(12.0),
                INK,
            );
        });
}

pub fn back_pill(ui: &mut Ui, label: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), bold(15.0), INK);
    let size = vec2(galley.size().x + 50.0, 38.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = if resp.hovered() { HOVER } else { KEY };
    p.rect(
        rect,
        CornerRadius::same(19),
        fill,
        Stroke::new(1.5, EDGE),
        StrokeKind::Inside,
    );
    let c = pos2(rect.left() + 20.0, rect.center().y);
    p.line_segment(
        [c + vec2(4.0, -6.0), c + vec2(-2.0, 0.0)],
        Stroke::new(2.2, INK),
    );
    p.line_segment(
        [c + vec2(-2.0, 0.0), c + vec2(4.0, 6.0)],
        Stroke::new(2.2, INK),
    );
    p.galley(
        pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        INK,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Big centered title + muted subtitle.
pub fn title(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.add_space(22.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).font(display(34.0)).color(INK));
        if !subtitle.is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new(subtitle).font(body(17.0)).color(MUTED));
        }
    });
    ui.add_space(14.0);
}

/// Monospace uppercase section label.
pub fn section(ui: &mut Ui, label: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(
            RichText::new(spaced_upper(label))
                .font(mono(12.5))
                .color(LABEL),
        );
    });
    ui.add_space(2.0);
}

fn spaced_upper(s: &str) -> String {
    s.to_uppercase()
}

/// A keycap card: light fill, thin edge and a thicker bottom edge.
pub fn group<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let edge_idx = ui.painter().add(Shape::Noop);
    let r = egui::Frame::new()
        .fill(KEY)
        .stroke(Stroke::new(1.0, EDGE))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        });
    let rect = r.response.rect;
    ui.painter().set(
        edge_idx,
        Shape::rect_filled(rect.translate(vec2(0.0, 3.0)), CornerRadius::same(16), EDGE),
    );
    ui.add_space(10.0);
    r.inner
}

/// Thin line between rows of a group.
pub fn divider(ui: &mut Ui) {
    let r = ui.available_rect_before_wrap();
    ui.painter()
        .hline(r.x_range(), r.top(), Stroke::new(1.0, EDGE));
}

/// Something shown at the start of a row.
#[derive(Clone, Default)]
pub enum Lead {
    #[default]
    None,
    Radio(bool),
    /// A keycap with text; `true` = red.
    Key(String, bool),
}

#[derive(Clone, Default)]
pub struct RowOptions {
    pub clickable: bool,
    pub chevron: bool,
    pub selected: bool,
    pub danger: bool,
    pub lead: Lead,
}

impl RowOptions {
    pub fn link() -> Self {
        Self {
            clickable: true,
            chevron: true,
            ..Default::default()
        }
    }
}

/// One row of a group: bold title, muted subtitle, controls on the right.
pub fn row(
    ui: &mut Ui,
    first: bool,
    title: &str,
    sub: &str,
    opts: RowOptions,
    right: impl FnOnce(&mut Ui),
) -> Response {
    if !first {
        divider(ui);
    }
    let bg_idx = ui.painter().add(Shape::Noop);
    let inner = egui::Frame::new()
        .inner_margin(Margin::symmetric(15, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 0.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_height(if sub.is_empty() { 30.0 } else { 40.0 });
                    match &opts.lead {
                        Lead::None => {}
                        Lead::Radio(sel) => {
                            radio_dot(ui, *sel);
                            ui.add_space(4.0);
                        }
                        Lead::Key(t, red) => {
                            keycap_sized(ui, t, bold(15.0), 30.0, *red, false);
                            ui.add_space(6.0);
                        }
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        let color = if opts.danger { SIGNAL_TEXT } else { INK };
                        ui.label(RichText::new(title).font(bold(16.0)).color(color));
                        if !sub.is_empty() {
                            ui.add(
                                egui::Label::new(RichText::new(sub).font(body(14.0)).color(MUTED))
                                    .wrap(),
                            );
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if opts.chevron {
                            chevron(ui);
                        }
                        right(ui);
                    });
                },
            );
        });
    let rect = inner.response.rect;
    let resp = if opts.clickable {
        ui.interact(
            rect,
            ui.scope_id().with(("row", title, sub)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        inner.response
    };
    if opts.selected || (opts.clickable && resp.hovered()) {
        ui.painter().set(
            bg_idx,
            Shape::rect_filled(
                rect.shrink(1.0),
                CornerRadius::same(14),
                if opts.selected { IDLE } else { HOVER },
            ),
        );
    }
    resp
}

/// A settings row: title with the control on the same line, explanation underneath.
pub fn control_row(
    ui: &mut Ui,
    first: bool,
    title: &str,
    sub: &str,
    right: impl FnOnce(&mut Ui),
) -> Response {
    if !first {
        divider(ui);
    }
    egui::Frame::new()
        .inner_margin(Margin::symmetric(15, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 0.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_height(34.0);
                    ui.label(RichText::new(title).font(bold(16.0)).color(INK));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| right(ui));
                },
            );
            if !sub.is_empty() {
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(sub).font(body(14.0)).color(MUTED)).wrap());
            }
        })
        .response
}

pub fn chevron(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
    let c = rect.center();
    let s = Stroke::new(2.0, INK);
    ui.painter()
        .line_segment([c + vec2(-3.0, -6.0), c + vec2(3.0, 0.0)], s);
    ui.painter()
        .line_segment([c + vec2(3.0, 0.0), c + vec2(-3.0, 6.0)], s);
}

/// A small keycap label (e.g. a key name).
pub fn keycap(ui: &mut Ui, label: &str) -> Response {
    keycap_sized(ui, label, mono(13.5), 26.0, false, false)
}

/// Keycap with optional red fill (active) – also used as a button.
pub fn keycap_sized(
    ui: &mut Ui,
    label: &str,
    font: FontId,
    height: f32,
    red: bool,
    clickable: bool,
) -> Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, INK);
    let w = (galley.size().x + 18.0).max(height);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, height + 3.0),
        if clickable {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = clickable && resp.is_pointer_button_down_on();
    let top = Rect::from_min_size(
        rect.min + vec2(0.0, if pressed { 2.0 } else { 0.0 }),
        vec2(w, height),
    );
    let (fill, edge) = if red {
        (SIGNAL, SIGNAL_EDGE)
    } else {
        (
            if clickable && resp.hovered() {
                KEY
            } else {
                PAGE
            },
            EDGE,
        )
    };
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 3.0), vec2(w, height)),
        CornerRadius::same(7),
        edge,
    );
    p.rect(
        top,
        CornerRadius::same(7),
        fill,
        Stroke::new(1.0, edge),
        StrokeKind::Inside,
    );
    p.galley(top.center() - galley.size() / 2.0, galley, INK);
    if clickable {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// The red 3D button.
pub fn primary(ui: &mut Ui, label: &str, width: f32) -> Response {
    button3d(ui, label, width, SIGNAL, SIGNAL_EDGE, true)
}

/// A cream 3D button.
pub fn secondary(ui: &mut Ui, label: &str, width: f32) -> Response {
    button3d(ui, label, width, KEY, EDGE, true)
}

pub fn button3d(
    ui: &mut Ui,
    label: &str,
    width: f32,
    fill: Color32,
    edge: Color32,
    enabled: bool,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), bold(17.0), INK);
    let w = width.max(galley.size().x + 52.0);
    let h = 48.0;
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, h + 5.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = resp.is_pointer_button_down_on();
    let shift = if pressed { 3.0 } else { 0.0 };
    let top = Rect::from_min_size(rect.min + vec2(0.0, shift), vec2(w, h));
    let p = ui.painter();
    let alpha = if enabled { 255 } else { 110 };
    let a = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 5.0), vec2(w, h)),
        CornerRadius::same(14),
        a(edge),
    );
    let fill = if resp.hovered() && enabled {
        brighten(fill)
    } else {
        fill
    };
    p.rect(
        top,
        CornerRadius::same(14),
        a(fill),
        if edge == EDGE {
            Stroke::new(1.0, EDGE)
        } else {
            Stroke::NONE
        },
        StrokeKind::Inside,
    );
    p.galley(top.center() - galley.size() / 2.0, galley, a(INK));
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

fn brighten(c: Color32) -> Color32 {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * 0.08) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// iOS-style switch.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(48.0, 28.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let p = ui.painter();
    let fill = if *on { INK } else { IDLE };
    p.rect(
        rect,
        CornerRadius::same(14),
        fill,
        Stroke::new(1.5, if *on { INK } else { MUTED }),
        StrokeKind::Inside,
    );
    let x = egui::lerp(rect.left() + 14.0..=rect.right() - 14.0, t);
    p.circle(
        pos2(x, rect.center().y),
        10.0,
        KEY,
        Stroke::new(1.5, if *on { INK } else { MUTED }),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Round radio indicator (painted only).
pub fn radio_dot(ui: &mut Ui, selected: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    let c = rect.center();
    if selected {
        ui.painter().circle_filled(c, 11.0, INK);
        ui.painter().circle_filled(c, 4.0, KEY);
    } else {
        ui.painter().circle(c, 10.0, KEY, Stroke::new(2.0, MUTED));
    }
}

/// Our slider: graphite fill, round black knob, monospace value on the right.
fn knob_slider(ui: &mut Ui, t: f32) -> (Response, Option<f32>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(170.0, 22.0), Sense::click_and_drag());
    let rail = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 4.0));
    let p = ui.painter();
    p.rect_filled(rail, CornerRadius::same(2), EDGE);
    let x = egui::lerp(rail.left()..=rail.right(), t.clamp(0.0, 1.0));
    p.rect_filled(
        Rect::from_min_max(rail.min, pos2(x, rail.max.y)),
        CornerRadius::same(2),
        INK,
    );
    let r = if resp.hovered() || resp.dragged() {
        9.0
    } else {
        8.0
    };
    p.circle(pos2(x, rect.center().y), r, INK, Stroke::new(2.0, KEY));
    let mut new = None;
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            new = Some(((pos.x - rail.left()) / rail.width()).clamp(0.0, 1.0));
        }
    }
    (resp.on_hover_cursor(egui::CursorIcon::PointingHand), new)
}

/// Slider with a monospace value on its right (for right-to-left row layouts).
pub fn slider_f32(
    ui: &mut Ui,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    fmt: impl Fn(f32) -> String,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    ui.add_sized(
        [62.0, 22.0],
        egui::Label::new(RichText::new(fmt(*v)).font(mono(13.5)).color(INK)),
    );
    let (_, new) = knob_slider(ui, (*v - lo) / (hi - lo));
    if let Some(t) = new {
        let nv = ((lo + t * (hi - lo)) * 100.0).round() / 100.0;
        if nv != *v {
            *v = nv;
            return true;
        }
    }
    false
}

pub fn slider_u32(
    ui: &mut Ui,
    v: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    step: f64,
    fmt: impl Fn(u32) -> String,
) -> bool {
    let (lo, hi) = (*range.start() as f32, *range.end() as f32);
    ui.add_sized(
        [86.0, 22.0],
        egui::Label::new(RichText::new(fmt(*v)).font(mono(13.5)).color(INK)),
    );
    let (_, new) = knob_slider(ui, (*v as f32 - lo) / (hi - lo));
    if let Some(t) = new {
        let raw = lo + t * (hi - lo);
        let nv = ((raw as f64 / step).round() * step) as u32;
        if nv != *v {
            *v = nv;
            return true;
        }
    }
    false
}

/// Two columns of equal width, centered in the content area. `add` is called with 0 then 1.
pub fn columns(ui: &mut Ui, mut add: impl FnMut(&mut Ui, usize)) {
    let total = ui.available_width().min(CONTENT_WIDTH);
    let col = (total - COLUMN_GAP) / 2.0;
    let pad = (ui.available_width() - total) / 2.0;
    ui.horizontal_top(|ui| {
        ui.add_space(pad);
        for i in 0..2 {
            if i == 1 {
                ui.add_space(COLUMN_GAP - ui.spacing().item_spacing.x);
            }
            ui.allocate_ui_with_layout(vec2(col, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(col);
                add(ui, i)
            });
        }
    });
}

/// One centered column of the given width.
pub fn narrow<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = width.min(ui.available_width());
    let pad = (ui.available_width() - w) / 2.0;
    ui.horizontal_top(|ui| {
        ui.add_space(pad);
        ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(w);
            add(ui)
        })
        .inner
    })
    .inner
}

/// Centered horizontal group of fixed total width.
pub fn centered_row(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    let pad = ((ui.available_width() - width) / 2.0).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        add(ui);
    });
}

pub fn small_link(ui: &mut Ui, label: &str) -> Response {
    ui.add(
        egui::Label::new(RichText::new(label).font(bold(14.5)).color(INK).underline())
            .sense(Sense::click()),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn hint(ui: &mut Ui, t: &str) {
    ui.label(RichText::new(t).font(body(13.5)).color(MUTED));
}

pub fn size(v: Vec2) -> Vec2 {
    v
}

// ---------------------------------------------------------------------------- more widgets

/// A pill group of options; the selected one is raised like a key. Items may carry a small keycap badge.
pub fn segmented(
    ui: &mut Ui,
    id: &str,
    items: &[(String, Option<String>)],
    selected: usize,
) -> Option<usize> {
    let _ = id;
    // Measure first, then allocate one fixed rect: aligns correctly in any parent layout.
    let parts: Vec<_> = items
        .iter()
        .map(|(label, badge)| {
            let g = ui.painter().layout_no_wrap(label.clone(), bold(15.0), INK);
            let b = badge
                .as_ref()
                .map(|b| ui.painter().layout_no_wrap(b.clone(), mono(12.0), INK));
            let w = g.size().x + 28.0 + b.as_ref().map(|g| g.size().x + 18.0).unwrap_or(0.0);
            (g, b, w)
        })
        .collect();
    let inner_w: f32 =
        parts.iter().map(|p| p.2).sum::<f32>() + 2.0 * (parts.len().saturating_sub(1)) as f32;
    let (outer, _) = ui.allocate_exact_size(vec2(inner_w + 8.0, 44.0), Sense::hover());
    ui.painter()
        .rect_filled(outer, CornerRadius::same(14), GROUND);
    let mut x = outer.left() + 4.0;
    let mut clicked = None;
    for (i, (galley, badge_g, w)) in parts.into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x, outer.top() + 4.0), vec2(w, 36.0));
        x += w + 2.0;
        let resp = ui
            .interact(rect, ui.scope_id().with(("seg", id, i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let p = ui.painter();
        if i == selected {
            p.rect_filled(rect.translate(vec2(0.0, 2.0)), CornerRadius::same(10), EDGE);
            p.rect(
                rect,
                CornerRadius::same(10),
                KEY,
                Stroke::new(1.0, EDGE),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(10), HOVER);
        }
        let tx = rect.left() + 14.0;
        let ty = rect.center().y - galley.size().y / 2.0;
        let tw = galley.size().x;
        p.galley(pos2(tx, ty), galley, INK);
        if i == selected {
            p.hline(tx..=tx + tw, ty + 21.0, Stroke::new(1.5, INK));
        }
        if let Some(g) = badge_g {
            let r = Rect::from_min_size(
                pos2(tx + tw + 10.0, rect.center().y - 11.0),
                vec2(g.size().x + 10.0, 22.0),
            );
            p.rect(
                r,
                CornerRadius::same(5),
                KEY,
                Stroke::new(1.0, EDGE),
                StrokeKind::Inside,
            );
            p.galley(r.center() - g.size() / 2.0, g, INK);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// Square key-style button used for − and +.
pub fn key_button(ui: &mut Ui, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    let top = Rect::from_min_size(
        rect.min + vec2(0.0, if pressed { 3.0 } else { 0.0 }),
        vec2(44.0, 40.0),
    );
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 4.0), vec2(44.0, 40.0)),
        CornerRadius::same(9),
        EDGE,
    );
    p.rect(
        top,
        CornerRadius::same(9),
        if resp.hovered() { KEY } else { PAGE },
        Stroke::new(1.5, MUTED),
        StrokeKind::Inside,
    );
    p.text(top.center(), Align2::CENTER_CENTER, label, bold(20.0), INK);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// − value + ; returns -1, 0 or +1.
pub fn stepper(ui: &mut Ui, value: &str) -> i32 {
    let mut d = 0;
    // right-to-left layouts add items from the right
    if key_button(ui, "+").clicked() {
        d = 1;
    }
    ui.add_sized(
        [96.0, 40.0],
        egui::Label::new(RichText::new(value).font(bold(18.0)).color(INK)),
    );
    if key_button(ui, "−").clicked() {
        d = -1;
    }
    d
}

/// "Label **Value** ▾" text that opens a menu. Returns the response to attach a popup to.
pub fn inline_picker(ui: &mut Ui, label: &str, value: &str, size: f32) -> Response {
    inline_picker_suffix(ui, label, value, "", size)
}

/// Like [`inline_picker`] with muted text after the arrow (e.g. " · Ready").
pub fn inline_picker_suffix(
    ui: &mut Ui,
    label: &str,
    value: &str,
    suffix: &str,
    size: f32,
) -> Response {
    let mut job = egui::text::LayoutJob::default();
    if !label.is_empty() {
        job.append(
            &format!("{label} "),
            0.0,
            egui::TextFormat {
                font_id: body(size),
                color: MUTED,
                ..Default::default()
            },
        );
    }
    job.append(
        value,
        0.0,
        egui::TextFormat {
            font_id: bold(size),
            color: INK,
            ..Default::default()
        },
    );
    let main = ui.painter().layout_job(job);
    let tail = ui
        .painter()
        .layout_no_wrap(suffix.to_string(), body(size), MUTED);
    let arrow_w = size * 1.1;
    let w = main.size().x + arrow_w + tail.size().x;
    let h = main.size().y.max(tail.size().y);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let p = ui.painter();
    p.galley(rect.left_top(), main.clone(), INK);
    let ax = rect.left() + main.size().x + arrow_w * 0.55;
    let ay = rect.center().y + 1.0;
    let s = size * 0.28;
    p.add(Shape::convex_polygon(
        vec![
            pos2(ax - s, ay - s * 0.6),
            pos2(ax + s, ay - s * 0.6),
            pos2(ax, ay + s * 0.7),
        ],
        INK,
        Stroke::NONE,
    ));
    p.galley(
        pos2(rect.left() + main.size().x + arrow_w, rect.top()),
        tail,
        MUTED,
    );
    if resp.hovered() {
        p.hline(
            rect.left()..=rect.left() + main.size().x,
            rect.bottom(),
            Stroke::new(1.0, INK),
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The big red Play button.
pub fn play_button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), display(34.0), INK);
    let w = (galley.size().x + 90.0).max(200.0);
    let h = 84.0;
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, h + 8.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = enabled && resp.is_pointer_button_down_on();
    let top = Rect::from_min_size(
        rect.min + vec2(0.0, if pressed { 5.0 } else { 0.0 }),
        vec2(w, h),
    );
    let a = |c: Color32| {
        if enabled {
            c
        } else {
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 110)
        }
    };
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(w, h)),
        CornerRadius::same(24),
        a(SIGNAL_EDGE),
    );
    p.rect_filled(
        top,
        CornerRadius::same(24),
        a(if resp.hovered() && enabled {
            brighten(SIGNAL)
        } else {
            SIGNAL
        }),
    );
    p.galley(top.center() - galley.size() / 2.0, galley, a(INK));
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Red frame around the whole window while you are controlling the console.
pub fn live_frame(ctx: &egui::Context) {
    let r = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("live-frame"),
    ))
    .rect_stroke(
        r.shrink(2.0),
        CornerRadius::ZERO,
        Stroke::new(4.0, SIGNAL),
        StrokeKind::Inside,
    );
}

/// Small keycap-like chip for controller buttons in lists (e.g. "RT").
pub fn pad_chip(ui: &mut Ui, label: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), bold(13.0), INK);
    let w = (galley.size().x + 16.0).max(30.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 28.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        KEY,
        Stroke::new(1.5, INK),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, INK);
}
