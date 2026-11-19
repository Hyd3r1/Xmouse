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
/// Width of our slider when its row has room for it.
pub const SLIDER_WIDTH: f32 = 170.0;

/// The one spacing scale (points). Gaps between things come from here.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
}

/// The corner radii: small keys and chips, controls, buttons and floating cards, cards.
pub mod radius {
    pub const TINY: u8 = 5;
    pub const KEY: u8 = 8;
    pub const CONTROL: u8 = 10;
    pub const BUTTON: u8 = 14;
    pub const CARD: u8 = 16;
}

/// Motion: every animation in the app takes its duration and easing from here, so hover,
/// press, selection and page changes all move the same calm way.
pub mod motion {
    use std::time::Duration;

    /// Hover and press feedback.
    pub const FAST: f32 = 0.12;
    /// Switches, selection, focus rings, enabled ↔ disabled.
    pub const BASE: f32 = 0.18;
    /// Page changes, toasts, the sliding pill of a segmented control.
    pub const SLOW: f32 = 0.24;
    /// How often an animation asks for a new frame (~144 per second, never a busy loop).
    pub const FRAME: Duration = Duration::from_millis(6);
    /// How often a spinner asks for a new frame.
    pub const SPINNER_FRAME: Duration = Duration::from_millis(16);

    /// Ease-out: fast start, soft landing. `t` in 0..=1.
    pub fn ease_out(t: f32) -> f32 {
        let u = 1.0 - t.clamp(0.0, 1.0);
        1.0 - u * u * u
    }

    /// One step of the spring-like approach all the animated values use: move `value`
    /// towards `target` so that it has (as good as) arrived after `secs`. It never
    /// overshoots, and turning around mid-way is smooth because only the current value matters.
    pub fn approach(value: f32, target: f32, dt: f32, secs: f32) -> f32 {
        if secs <= 0.0 {
            return target;
        }
        // 99 % of the way after `secs`.
        let k = 1.0 - (-dt.max(0.0) * 4.6 / secs).exp();
        value + (target - value) * k
    }

    #[derive(Clone, Copy)]
    struct Anim {
        value: f32,
        time: f64,
        frame: u64,
        resting: bool,
    }

    /// Animate towards `target` (see [`approach`]). The first time, or when the widget was
    /// not on screen in the previous frame, the value is simply `target`: nothing animates
    /// just because a page opened. Asks for new frames only while it is still moving.
    pub fn value_eps(ctx: &egui::Context, id: egui::Id, target: f32, secs: f32, eps: f32) -> f32 {
        let now = ctx.input(|i| i.time);
        let frame = ctx.cumulative_frame_nr();
        let value = ctx.data_mut(|d| {
            let a = d.get_temp_mut_or(
                id,
                Anim {
                    value: target,
                    time: now,
                    frame,
                    resting: true,
                },
            );
            if frame > a.frame + 1 {
                a.value = target;
            } else if (a.value - target).abs() <= eps {
                a.value = target;
            } else {
                // A value that was at rest starts moving now, not when it was last drawn.
                let dt = if a.resting {
                    1.0 / 120.0
                } else {
                    ((now - a.time) as f32).clamp(0.0, 1.0 / 30.0)
                };
                a.value = approach(a.value, target, dt, secs);
            }
            a.time = now;
            a.frame = frame;
            a.resting = a.value == target;
            a.value
        });
        if value != target {
            ctx.request_repaint_after(FRAME);
        }
        value
    }

    /// [`value_eps`] for values in 0..=1.
    pub fn value(ctx: &egui::Context, id: egui::Id, target: f32, secs: f32) -> f32 {
        value_eps(ctx, id, target, secs, 0.004)
    }

    /// Animate a state: 0 when `on` is false, 1 when true.
    pub fn flag(ctx: &egui::Context, id: egui::Id, on: bool, secs: f32) -> f32 {
        value(ctx, id, if on { 1.0 } else { 0.0 }, secs)
    }

    /// Where an animated value is right now, without moving it.
    pub fn peek(ctx: &egui::Context, id: egui::Id) -> f32 {
        ctx.data(|d| d.get_temp::<Anim>(id)).map_or(0.0, |a| a.value)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn approach_arrives_in_time_and_never_overshoots() {
            for (from, to) in [(0.0f32, 1.0f32), (1.0, 0.0), (20.0, 340.0)] {
                let mut v = from;
                let mut last = (to - v).abs();
                for _ in 0..24 {
                    v = approach(v, to, 0.01, SLOW);
                    let left = (to - v).abs();
                    assert!(left <= last, "moves towards the target only");
                    assert!((v - from) * (to - from) >= 0.0 && (to - v) * (to - from) >= 0.0);
                    last = left;
                }
                assert!(last <= (to - from).abs() * 0.011, "there after SLOW seconds");
            }
        }

        #[test]
        fn approach_is_frame_rate_independent() {
            let mut a = 0.0;
            for _ in 0..12 {
                a = approach(a, 1.0, 0.01, BASE);
            }
            let mut b = 0.0;
            for _ in 0..3 {
                b = approach(b, 1.0, 0.04, BASE);
            }
            assert!((a - b).abs() < 1e-4);
            assert_eq!(approach(0.3, 1.0, 0.01, 0.0), 1.0);
            assert_eq!(approach(0.3, 1.0, 0.0, BASE), 0.3);
        }

        #[test]
        fn value_snaps_first_then_eases_and_settles() {
            let ctx = egui::Context::default();
            let id = egui::Id::unique("motion-test");
            let mut seen = Vec::new();
            for i in 0..80 {
                let input = egui::RawInput {
                    time: Some(i as f64 * 0.01),
                    ..Default::default()
                };
                let _ = ctx.run_ui(input, |ui| {
                    // Off for two frames, then on.
                    seen.push(flag(ui.ctx(), id, i >= 2, FAST));
                });
            }
            assert_eq!(seen[0], 0.0, "first sight: no animation");
            assert_eq!(seen[1], 0.0);
            assert!(seen[2] > 0.0 && seen[2] < 0.5, "starts gently, got {}", seen[2]);
            assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic");
            assert_eq!(*seen.last().unwrap(), 1.0, "settles exactly on the target");
            assert!(seen.iter().position(|v| *v == 1.0).unwrap() < 30);
            assert_eq!(peek(&ctx, id), 1.0);
        }

        #[test]
        fn ease_out_ends_are_exact() {
            assert_eq!(ease_out(0.0), 0.0);
            assert_eq!(ease_out(1.0), 1.0);
            assert_eq!(ease_out(7.0), 1.0);
            assert!(ease_out(0.5) > 0.5);
        }
    }
}

/// Blend two colours (`t` = 0 gives `a`, 1 gives `b`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        f(a.r(), b.r()),
        f(a.g(), b.g()),
        f(a.b(), b.b()),
        f(a.a(), b.a()),
    )
}

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
        // egui's own animations (menus opening, scrolling to a spot) use our timing too.
        s.animation_time = motion::BASE;
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 7.0);
        s.spacing.interact_size = vec2(40.0, 30.0);
        s.spacing.combo_width = 220.0;
        s.spacing.slider_width = SLIDER_WIDTH;
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
/// `back_fade` (0..=1) is how far a back pill that just appeared or changed has faded in.
pub fn top_bar(ui: &mut Ui, back: Option<&str>, right: Option<String>, back_fade: f32) -> bool {
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
                    ui.set_opacity(back_fade.clamp(0.0, 1.0));
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
        // Edge to edge like the top bar, text on the page's left margin.
        .frame(
            egui::Frame::new()
                .fill(GROUND)
                .inner_margin(Margin::symmetric(24, 0)),
        )
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().hline(
                r.x_range().expand(24.0),
                r.top() + 0.5,
                Stroke::new(1.0, EDGE),
            );
            // Cut a long line off at the right margin instead of letting it run out of the window.
            ui.painter().with_clip_rect(r).text(
                pos2(r.left(), r.center().y),
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
    let (hover, press) = hover_press(ui, &resp, true);
    let p = ui.painter();
    let fill = mix(mix(KEY, HOVER, hover), IDLE, press);
    p.rect(
        rect,
        CornerRadius::same(19),
        fill,
        Stroke::new(1.5, EDGE),
        StrokeKind::Inside,
    );
    // The arrow leans the way you are about to go.
    let c = pos2(rect.left() + 20.0 - 2.0 * hover, rect.center().y);
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

/// How far (0..=1) a clickable thing is hovered and pressed, eased with the shared timing.
fn hover_press(ui: &Ui, resp: &Response, enabled: bool) -> (f32, f32) {
    let ctx = ui.ctx();
    let down = enabled && resp.is_pointer_button_down_on();
    let hover = motion::flag(
        ctx,
        resp.id.with("hover"),
        enabled && (resp.hovered() || down),
        motion::FAST,
    );
    let press = motion::flag(ctx, resp.id.with("press"), down, motion::FAST);
    (hover, press)
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
        .corner_radius(CornerRadius::same(radius::CARD))
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        });
    let rect = r.response.rect;
    ui.painter().set(
        edge_idx,
        Shape::rect_filled(
            rect.translate(vec2(0.0, 3.0)),
            CornerRadius::same(radius::CARD),
            EDGE,
        ),
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
    let id = ui.scope_id().with(("row", title, sub));
    // The chevron is drawn before this frame's hover is known: it follows the value the
    // highlight reached a frame ago, which looks the same.
    let nudge = if opts.clickable && opts.chevron {
        motion::peek(ui.ctx(), id.with("hover"))
    } else {
        0.0
    };
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
                            chevron(ui, nudge * 3.0);
                        }
                        right(ui);
                    });
                },
            );
        });
    let rect = inner.response.rect;
    let resp = if opts.clickable {
        ui.interact(rect, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        inner.response
    };
    // Highlight: fades in on hover, deepens while pressed or selected.
    let ctx = ui.ctx();
    let (hover, press) = if opts.clickable {
        let down = resp.is_pointer_button_down_on();
        (
            motion::flag(ctx, id.with("hover"), resp.hovered() || down, motion::FAST),
            motion::flag(ctx, id.with("press"), down, motion::FAST),
        )
    } else {
        (0.0, 0.0)
    };
    let selected = motion::flag(ctx, id.with("selected"), opts.selected, motion::BASE);
    let strength = hover.max(selected);
    if strength > 0.0 {
        let fill = mix(HOVER, IDLE, selected.max(press * 0.6));
        ui.painter().set(
            bg_idx,
            Shape::rect_filled(
                rect.shrink(1.0),
                CornerRadius::same(radius::BUTTON),
                fill.gamma_multiply(strength),
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
            // The control goes in first so the row knows its height; the title is then
            // centred on the control whatever its size (a switch, a stepper, a text field).
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 0.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_min_height(34.0);
                    // The title keeps its room; the control gets the rest (a slider
                    // shrinks to fit it in a narrow window).
                    let title_w = ui
                        .painter()
                        .layout_no_wrap(title.to_string(), bold(16.0), INK)
                        .size()
                        .x;
                    let total = ui.available_width();
                    let for_control = (total - title_w - space::M).max(total * 0.45);
                    // (Room for the slider's value label and the gap next to it.)
                    ui.spacing_mut().slider_width = (for_control - 86.0 - 2.0 * space::S)
                        .clamp(96.0, SLIDER_WIDTH);
                    right(ui);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(title).font(bold(16.0)).color(INK))
                                .truncate(),
                        );
                    });
                },
            );
            if !sub.is_empty() {
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(sub).font(body(14.0)).color(MUTED)).wrap());
            }
        })
        .response
}

/// The "goes somewhere" arrow at the end of a row, moved `dx` points to the right.
pub fn chevron(ui: &mut Ui, dx: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
    let c = rect.center() + vec2(dx, 0.0);
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
    let (hover, press) = if clickable {
        hover_press(ui, &resp, true)
    } else {
        (0.0, 0.0)
    };
    // Going red (e.g. "press a key now") and back is a fade, not a flash.
    let red_t = if clickable {
        motion::flag(ui.ctx(), resp.id.with("red"), red, motion::BASE)
    } else if red {
        1.0
    } else {
        0.0
    };
    let top = Rect::from_min_size(rect.min + vec2(0.0, 2.0 * press), vec2(w, height));
    let fill = mix(mix(PAGE, KEY, hover), SIGNAL, red_t);
    let edge = mix(EDGE, SIGNAL_EDGE, red_t);
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 3.0), vec2(w, height)),
        CornerRadius::same(radius::KEY),
        edge,
    );
    p.rect(
        top,
        CornerRadius::same(radius::KEY),
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
    // Hover lifts the key a little and brightens it; a press sinks it onto its edge; turning
    // on or off is a fade.
    let (hover, press) = hover_press(ui, &resp, enabled);
    let on = motion::flag(ui.ctx(), resp.id.with("enabled"), enabled, motion::BASE);
    let shift = 3.0 * press - 1.0 * hover * (1.0 - press);
    let top = Rect::from_min_size(rect.min + vec2(0.0, shift), vec2(w, h));
    let p = ui.painter();
    let alpha = (110.0 + 145.0 * on).round() as u8;
    let a = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 5.0), vec2(w, h)),
        CornerRadius::same(radius::BUTTON),
        a(edge),
    );
    let fill = mix(fill, brighten(fill), hover);
    p.rect(
        top,
        CornerRadius::same(radius::BUTTON),
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

/// A 3D button for something that takes a while: while `busy` it is faded, can't be
/// clicked and shows the spinner next to its label.
pub fn busy_button(
    ui: &mut Ui,
    label: &str,
    width: f32,
    fill: Color32,
    edge: Color32,
    busy: bool,
) -> Response {
    let resp = button3d(ui, label, width, fill, edge, !busy);
    let t = motion::flag(ui.ctx(), resp.id.with("busy"), busy, motion::BASE);
    if t > 0.0 {
        let at = pos2(resp.rect.left() + 26.0, resp.rect.top() + 24.0);
        spinner_at(ui, at, 7.0, INK.gamma_multiply(0.75 * t));
    }
    resp
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
    // The knob slides and the track changes colour together.
    let t = motion::flag(ui.ctx(), resp.id.with("on"), *on, motion::BASE);
    let (hover, _) = hover_press(ui, &resp, true);
    let p = ui.painter();
    let line = mix(mix(MUTED, INK, hover * 0.5), INK, t);
    p.rect(
        rect,
        CornerRadius::same(14),
        mix(IDLE, INK, t),
        Stroke::new(1.5, line),
        StrokeKind::Inside,
    );
    let x = egui::lerp(rect.left() + 14.0..=rect.right() - 14.0, t);
    p.circle(pos2(x, rect.center().y), 10.0, KEY, Stroke::new(1.5, line));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Round radio indicator (painted only).
pub fn radio_dot(ui: &mut Ui, selected: bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    let c = rect.center();
    let t = motion::flag(ui.ctx(), resp.id.with("on"), selected, motion::BASE);
    let p = ui.painter();
    // Off: a muted ring. On: it fills with ink and the dot grows in.
    p.circle(
        c,
        10.0 + 0.5 * t,
        mix(KEY, INK, t),
        Stroke::new(2.0 - t, mix(MUTED, INK, t)),
    );
    if t > 0.0 {
        p.circle_filled(c, 4.0 * t, KEY);
    }
}

/// Our slider: graphite fill, round black knob, monospace value on the right.
fn knob_slider(ui: &mut Ui, t: f32) -> (Response, Option<f32>) {
    // Normally SLIDER_WIDTH; a row that is short of room asks for less (see `control_row`).
    let width = ui.spacing().slider_width.clamp(96.0, SLIDER_WIDTH);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click_and_drag());
    let rail = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 4.0));
    let p = ui.painter();
    p.rect_filled(rail, CornerRadius::same(2), EDGE);
    let x = egui::lerp(rail.left()..=rail.right(), t.clamp(0.0, 1.0));
    p.rect_filled(
        Rect::from_min_max(rail.min, pos2(x, rail.max.y)),
        CornerRadius::same(2),
        INK,
    );
    let grab = motion::flag(
        ui.ctx(),
        resp.id.with("grab"),
        resp.hovered() || resp.dragged(),
        motion::FAST,
    );
    p.circle(pos2(x, rect.center().y), 8.0 + grab, INK, Stroke::new(2.0, KEY));
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

/// An underlined text link. The underline is quiet until you point at it.
pub fn small_link(ui: &mut Ui, label: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), bold(14.5), INK);
    let size = galley.size();
    let (rect, resp) = ui.allocate_exact_size(size + vec2(0.0, 2.0), Sense::click());
    let (hover, press) = hover_press(ui, &resp, true);
    let p = ui.painter();
    let ink = mix(INK, SIGNAL_TEXT, press);
    p.galley(rect.left_top(), galley, ink);
    p.hline(
        rect.x_range(),
        rect.top() + size.y,
        Stroke::new(1.0 + 0.5 * hover, mix(mix(MUTED, INK, hover), SIGNAL_TEXT, press)),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The one style for explanations and secondary lines.
pub fn hint(ui: &mut Ui, t: &str) {
    ui.label(RichText::new(t).font(body(14.0)).color(MUTED));
}

/// [`hint`] lined up with the text inside the cards above and below it.
pub fn hint_indented(ui: &mut Ui, t: &str) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.add(egui::Label::new(RichText::new(t).font(body(14.0)).color(MUTED)).wrap());
    });
}

/// The one style for "that went wrong" lines.
pub fn error_text(ui: &mut Ui, t: &str) {
    ui.add(egui::Label::new(RichText::new(t).font(medium(14.0)).color(SIGNAL_TEXT)).wrap());
}

/// A calm "working on it" ring: a quarter arc going round a faint track.
pub fn spinner(ui: &mut Ui) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    spinner_at(ui, rect.center(), 7.0, MUTED);
    resp
}

/// [`spinner`] painted at a given spot (e.g. inside a busy button).
pub fn spinner_at(ui: &Ui, center: egui::Pos2, r: f32, color: Color32) {
    if !ui.is_rect_visible(Rect::from_center_size(center, Vec2::splat(r * 2.0 + 4.0))) {
        return;
    }
    let p = ui.painter();
    p.circle_stroke(center, r, Stroke::new(2.0, color.gamma_multiply(0.22)));
    // One turn every 1.1 s.
    let start = (ui.input(|i| i.time) / 1.1).fract() as f32 * std::f32::consts::TAU;
    let points: Vec<egui::Pos2> = (0..=12)
        .map(|i| {
            let a = start + i as f32 / 12.0 * std::f32::consts::FRAC_PI_2;
            center + r * vec2(a.cos(), a.sin())
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(2.0, color)));
    ui.ctx().request_repaint_after(motion::SPINNER_FRAME);
}

/// A text field with a soft focus ring that fades in when you click into it.
pub fn text_field(ui: &mut Ui, edit: egui::TextEdit<'_>) -> Response {
    let ring = ui.painter().add(Shape::Noop);
    let resp = ui.add(edit);
    let t = motion::flag(
        ui.ctx(),
        resp.id.with("focus"),
        resp.has_focus(),
        motion::BASE,
    );
    if t > 0.0 {
        ui.painter().set(
            ring,
            Shape::rect_stroke(
                resp.rect.expand(1.0),
                CornerRadius::same(radius::CONTROL + 2),
                Stroke::new(3.0 * t, SIGNAL.gamma_multiply(0.30 * t)),
                StrokeKind::Outside,
            ),
        );
    }
    resp
}

// ---------------------------------------------------------------------------- more widgets

/// A pill group of options; the selected one is raised like a key. Items may carry a small keycap badge.
pub fn segmented(
    ui: &mut Ui,
    id: &str,
    items: &[(String, Option<String>)],
    selected: usize,
) -> Option<usize> {
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
        .rect_filled(outer, CornerRadius::same(radius::BUTTON), GROUND);
    let base = ui.scope_id().with(("seg", id));
    let ctx = ui.ctx().clone();

    // The raised pill (and the line under its label) slides to the selected option. It is
    // tracked relative to the control, so scrolling or resizing never sets it off.
    let mut sx = 4.0;
    let mut target = (4.0, 0.0, 0.0, 0.0);
    for (i, (galley, _, w)) in parts.iter().enumerate() {
        if i == selected {
            target = (sx, *w, galley.size().x, galley.size().y);
        }
        sx += w + 2.0;
    }
    if target.1 > 0.0 {
        let glide = |what: &str, to: f32| {
            motion::value_eps(&ctx, base.with(what), to, motion::SLOW, 0.3)
        };
        let (px, pw, line_w) = (
            glide("pill-x", target.0),
            glide("pill-w", target.1),
            glide("line-w", target.2),
        );
        let pill = Rect::from_min_size(outer.min + vec2(px, 4.0), vec2(pw, 36.0));
        let p = ui.painter();
        p.rect_filled(
            pill.translate(vec2(0.0, 2.0)),
            CornerRadius::same(radius::CONTROL),
            EDGE,
        );
        p.rect(
            pill,
            CornerRadius::same(radius::CONTROL),
            KEY,
            Stroke::new(1.0, EDGE),
            StrokeKind::Inside,
        );
        let lx = pill.left() + 14.0;
        let ly = pill.center().y - target.3 / 2.0 + 21.0;
        p.hline(lx..=lx + line_w, ly, Stroke::new(1.5, INK));
    }

    let mut x = outer.left() + 4.0;
    let mut clicked = None;
    for (i, (galley, badge_g, w)) in parts.into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x, outer.top() + 4.0), vec2(w, 36.0));
        x += w + 2.0;
        let resp = ui
            .interact(rect, base.with(i), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = motion::flag(
            &ctx,
            resp.id.with("hover"),
            resp.hovered() && i != selected,
            motion::FAST,
        );
        let p = ui.painter();
        if hover > 0.0 {
            p.rect_filled(
                rect,
                CornerRadius::same(radius::CONTROL),
                HOVER.gamma_multiply(hover),
            );
        }
        let tx = rect.left() + 14.0;
        let ty = rect.center().y - galley.size().y / 2.0;
        let tw = galley.size().x;
        p.galley(pos2(tx, ty), galley, INK);
        if let Some(g) = badge_g {
            let r = Rect::from_min_size(
                pos2(tx + tw + 10.0, rect.center().y - 11.0),
                vec2(g.size().x + 10.0, 22.0),
            );
            p.rect(
                r,
                CornerRadius::same(radius::TINY),
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
    let (hover, press) = hover_press(ui, &resp, true);
    let top = Rect::from_min_size(rect.min + vec2(0.0, 3.0 * press), vec2(44.0, 40.0));
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 4.0), vec2(44.0, 40.0)),
        CornerRadius::same(radius::KEY),
        EDGE,
    );
    p.rect(
        top,
        CornerRadius::same(radius::KEY),
        mix(PAGE, KEY, hover),
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
    let (hover, _) = hover_press(ui, &resp, true);
    let p = ui.painter();
    p.galley(rect.left_top(), main.clone(), INK);
    let ax = rect.left() + main.size().x + arrow_w * 0.55;
    // The arrow dips towards the menu it opens.
    let ay = rect.center().y + 1.0 + 1.5 * hover;
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
    if hover > 0.0 {
        p.hline(
            rect.left()..=rect.left() + main.size().x,
            rect.bottom(),
            Stroke::new(1.0, INK.gamma_multiply(hover)),
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
    let (hover, press) = hover_press(ui, &resp, enabled);
    let on = motion::flag(ui.ctx(), resp.id.with("enabled"), enabled, motion::BASE);
    let top = Rect::from_min_size(
        rect.min + vec2(0.0, 5.0 * press - 1.5 * hover * (1.0 - press)),
        vec2(w, h),
    );
    let alpha = (110.0 + 145.0 * on).round() as u8;
    let a = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(w, h)),
        CornerRadius::same(24),
        a(SIGNAL_EDGE),
    );
    p.rect_filled(
        top,
        CornerRadius::same(24),
        a(mix(SIGNAL, brighten(SIGNAL), hover)),
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
        CornerRadius::same(radius::KEY),
        KEY,
        Stroke::new(1.5, INK),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, INK);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame at `time` with the given events; returns what the widget code returned.
    fn frame<R>(
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        mut add: impl FnMut(&mut Ui) -> R,
    ) -> R {
        let input = egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run_ui(input, |ui| out = Some(add(ui)));
        out.unwrap()
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn an_animated_button_sinks_smoothly_and_still_clicks() {
        let ctx = egui::Context::default();
        install(&ctx);
        let draw = |time: f64, events: Vec<egui::Event>| {
            frame(&ctx, time, events, |ui| primary(ui, "Play", 200.0))
        };
        let at = draw(0.0, vec![]).rect.center();
        let id = draw(0.01, vec![egui::Event::PointerMoved(at)]).id;
        draw(0.02, vec![]);
        assert_eq!(motion::peek(&ctx, id.with("press")), 0.0);
        draw(0.03, vec![button(at, true)]);
        let early = motion::peek(&ctx, id.with("press"));
        assert!(early < 0.6, "the press starts sinking, it doesn't jump: {early}");
        for i in 0..40 {
            draw(0.04 + i as f64 * 0.01, vec![]);
        }
        assert_eq!(motion::peek(&ctx, id.with("press")), 1.0, "fully down while held");
        assert_eq!(motion::peek(&ctx, id.with("hover")), 1.0);
        let up = draw(0.5, vec![button(at, false)]);
        assert!(up.clicked(), "the click arrives on release as before");
        for i in 0..40 {
            draw(0.51 + i as f64 * 0.01, vec![]);
        }
        assert_eq!(motion::peek(&ctx, id.with("press")), 0.0, "and it comes back up");
    }

    #[test]
    fn a_quick_click_registers_before_the_press_animation_ends() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut on = false;
        let mut draw = |time: f64, events: Vec<egui::Event>| {
            frame(&ctx, time, events, |ui| {
                let r = switch(ui, &mut on);
                (r.rect.center(), r.changed())
            })
        };
        let (at, _) = draw(0.0, vec![]);
        draw(0.01, vec![egui::Event::PointerMoved(at)]);
        draw(0.02, vec![button(at, true)]);
        let (_, changed) = draw(0.03, vec![button(at, false)]);
        assert!(changed, "a 10 ms click toggles the switch");
    }

    #[test]
    fn a_disabled_button_does_not_click() {
        let ctx = egui::Context::default();
        install(&ctx);
        let draw = |time: f64, events: Vec<egui::Event>| {
            frame(&ctx, time, events, |ui| button3d(ui, "Continue", 160.0, SIGNAL, SIGNAL_EDGE, false))
        };
        let at = draw(0.0, vec![]).rect.center();
        draw(0.01, vec![egui::Event::PointerMoved(at)]);
        draw(0.02, vec![button(at, true)]);
        assert!(!draw(0.03, vec![button(at, false)]).clicked());
    }

    #[test]
    fn segmented_reports_the_clicked_option_and_keeps_the_selection_until_told() {
        let ctx = egui::Context::default();
        install(&ctx);
        let items: Vec<(String, Option<String>)> = ["Off", "Light", "Strong"]
            .iter()
            .map(|s| (s.to_string(), None))
            .collect();
        let draw = |time: f64, events: Vec<egui::Event>| {
            frame(&ctx, time, events, |ui| {
                let before = ui.cursor().min;
                (before, segmented(ui, "t", &items, 0))
            })
        };
        let (origin, none) = draw(0.0, vec![]);
        assert_eq!(none, None);
        // Well inside the last option: the control is three short labels wide.
        let at = origin + vec2(150.0, 22.0);
        draw(0.01, vec![egui::Event::PointerMoved(at)]);
        draw(0.02, vec![button(at, true)]);
        let (_, clicked) = draw(0.03, vec![button(at, false)]);
        assert!(matches!(clicked, Some(1) | Some(2)), "got {clicked:?}");
    }

    #[test]
    fn mix_blends_and_clamps() {
        assert_eq!(mix(KEY, INK, 0.0), KEY);
        assert_eq!(mix(KEY, INK, 1.0), INK);
        assert_eq!(mix(KEY, INK, 9.0), INK);
        let mid = mix(Color32::from_rgb(0, 0, 0), Color32::from_rgb(200, 100, 50), 0.5);
        assert_eq!(mid, Color32::from_rgb(100, 50, 25));
    }
}
