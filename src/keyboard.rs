//! A drawn keyboard for the key editor: every key is a keycap showing what it does.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, Rect, Sense, Shape, Stroke, StrokeKind, Ui};

use crate::theme::{bold, mono, EDGE, IDLE, INK, KEY, LABEL, MUTED, PAGE, SIGNAL};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Usable {
    /// Can be bound.
    Yes,
    /// Used by XMouse itself (hotkeys).
    AppKey,
    /// Not reported separately by the window system.
    No,
}

#[derive(Clone, Debug)]
pub struct KeyDef {
    /// Input code used in profiles ("" for unusable keys).
    pub code: &'static str,
    pub label: &'static str,
    pub x: f32,
    pub row: usize,
    pub w: f32,
    pub usable: Usable,
}

/// Width of the drawn keyboard in key units.
pub const UNITS: f32 = 16.6;

fn row(out: &mut Vec<KeyDef>, row: usize, start: f32, keys: &[(&'static str, &'static str, f32)]) {
    let mut x = start;
    for (code, label, w) in keys {
        if code.is_empty() && label.is_empty() {
            x += w; // gap
            continue;
        }
        let usable = match *code {
            "" => Usable::No,
            "F6" | "F7" | "F8" | "F11" => Usable::AppKey,
            _ => Usable::Yes,
        };
        out.push(KeyDef {
            code,
            label,
            x,
            row,
            w: *w,
            usable,
        });
        x += w;
    }
}

/// US QWERTY layout with a navigation column on the right.
pub fn layout() -> Vec<KeyDef> {
    let mut k = Vec::new();
    row(
        &mut k,
        0,
        0.0,
        &[
            ("Escape", "Esc", 1.0),
            ("", "", 1.0),
            ("F1", "F1", 1.0),
            ("F2", "F2", 1.0),
            ("F3", "F3", 1.0),
            ("F4", "F4", 1.0),
            ("", "", 0.5),
            ("F5", "F5", 1.0),
            ("F6", "F6", 1.0),
            ("F7", "F7", 1.0),
            ("F8", "F8", 1.0),
            ("", "", 0.5),
            ("F9", "F9", 1.0),
            ("F10", "F10", 1.0),
            ("F11", "F11", 1.0),
            ("F12", "F12", 1.0),
        ],
    );
    row(
        &mut k,
        1,
        0.0,
        &[
            ("Backtick", "`", 1.0),
            ("1", "1", 1.0),
            ("2", "2", 1.0),
            ("3", "3", 1.0),
            ("4", "4", 1.0),
            ("5", "5", 1.0),
            ("6", "6", 1.0),
            ("7", "7", 1.0),
            ("8", "8", 1.0),
            ("9", "9", 1.0),
            ("0", "0", 1.0),
            ("Minus", "-", 1.0),
            ("Equals", "=", 1.0),
            ("Backspace", "Backspace", 2.0),
        ],
    );
    row(
        &mut k,
        2,
        0.0,
        &[
            ("Tab", "Tab", 1.5),
            ("Q", "Q", 1.0),
            ("W", "W", 1.0),
            ("E", "E", 1.0),
            ("R", "R", 1.0),
            ("T", "T", 1.0),
            ("Y", "Y", 1.0),
            ("U", "U", 1.0),
            ("I", "I", 1.0),
            ("O", "O", 1.0),
            ("P", "P", 1.0),
            ("OpenBracket", "[", 1.0),
            ("CloseBracket", "]", 1.0),
            ("Backslash", "\\", 1.5),
        ],
    );
    row(
        &mut k,
        3,
        0.0,
        &[
            ("", "Caps", 1.75),
            ("A", "A", 1.0),
            ("S", "S", 1.0),
            ("D", "D", 1.0),
            ("F", "F", 1.0),
            ("G", "G", 1.0),
            ("H", "H", 1.0),
            ("J", "J", 1.0),
            ("K", "K", 1.0),
            ("L", "L", 1.0),
            ("Semicolon", ";", 1.0),
            ("Quote", "'", 1.0),
            ("Enter", "Enter", 2.25),
        ],
    );
    row(
        &mut k,
        4,
        0.0,
        &[
            ("Shift", "Shift", 2.25),
            ("Z", "Z", 1.0),
            ("X", "X", 1.0),
            ("C", "C", 1.0),
            ("V", "V", 1.0),
            ("B", "B", 1.0),
            ("N", "N", 1.0),
            ("M", "M", 1.0),
            ("Comma", ",", 1.0),
            ("Period", ".", 1.0),
            ("Slash", "/", 1.0),
            ("Shift", "Shift", 1.25),
            ("Up", "↑", 1.0),
        ],
    );
    row(
        &mut k,
        5,
        0.0,
        &[
            ("Ctrl", "Ctrl", 1.5),
            ("", "Win", 1.25),
            ("Alt", "Alt", 1.25),
            ("Space", "Space", 6.0),
            ("Alt", "Alt", 1.25),
            ("Ctrl", "Ctrl", 1.25),
            ("Left", "←", 1.0),
            ("Down", "↓", 1.0),
            ("Right", "→", 1.0),
        ],
    );
    // navigation column
    for (i, (code, label)) in [
        ("Delete", "Del"),
        ("Home", "Home"),
        ("PageUp", "PgUp"),
        ("PageDown", "PgDn"),
        ("Insert", "Ins"),
        ("End", "End"),
    ]
    .into_iter()
    .enumerate()
    {
        k.push(KeyDef {
            code,
            label,
            x: 15.6,
            row: i,
            w: 1.0,
            usable: Usable::Yes,
        });
    }
    k
}

/// What to show on one key.
pub struct KeyLook {
    pub bound: bool,
    /// What it does in the game, e.g. "Reload".
    pub action: String,
    /// Controller button, e.g. "RT".
    pub pad: String,
}

/// Draw the keyboard; returns the code of a clicked key.
pub fn draw(
    ui: &mut Ui,
    width: f32,
    defs: &[KeyDef],
    look: impl Fn(&KeyDef) -> KeyLook,
    selected: Option<&str>,
) -> Option<&'static str> {
    let u = width / UNITS;
    let gap = (u * 0.08).max(3.0);
    let row_h = |r: usize| if r == 0 { u * 0.8 } else { u * 0.92 };
    let row_y = |r: usize| {
        (0..r).map(|i| row_h(i) + gap).sum::<f32>() + if r > 0 { gap * 2.0 } else { 0.0 }
    };
    let total_h = row_y(5) + row_h(5) + 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, total_h), Sense::hover());
    let mut clicked = None;
    let corner = CornerRadius::same((u * 0.16) as u8);

    // left / right hand split
    let split_x = rect.left() + u * 7.0 - gap / 2.0;
    let dash = Stroke::new(1.5, MUTED);
    let mut y = rect.top() + row_y(1) - gap;
    while y < rect.top() + row_y(5) + row_h(5) {
        ui.painter().line_segment(
            [
                pos2(split_x, y),
                pos2(split_x, (y + 6.0).min(rect.bottom())),
            ],
            dash,
        );
        y += 11.0;
    }

    for (i, d) in defs.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rect.left() + d.x * u, rect.top() + row_y(d.row)),
            vec2(d.w * u - gap, row_h(d.row)),
        );
        let id = ui.scope_id().with(("key", i));
        let resp = ui.interact(
            r,
            id,
            if d.usable == Usable::Yes {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let lk = if d.usable == Usable::Yes {
            look(d)
        } else {
            KeyLook {
                bound: false,
                action: String::new(),
                pad: String::new(),
            }
        };
        let is_sel = selected == Some(d.code) && d.usable == Usable::Yes && !d.code.is_empty();
        let pressed = resp.is_pointer_button_down_on();
        let top = r.translate(vec2(0.0, if pressed { 2.0 } else { 0.0 }));
        let p = ui.painter();

        let (fill, edge, text_col, outline) = match (d.usable, lk.bound) {
            (Usable::AppKey, _) => (INK, Color32::BLACK, KEY, Stroke::NONE),
            (Usable::No, _) => (PAGE, PAGE, MUTED, Stroke::NONE),
            (Usable::Yes, true) => (KEY, EDGE, INK, Stroke::new(1.5, INK)),
            (Usable::Yes, false) => (
                if resp.hovered() { KEY } else { IDLE },
                EDGE,
                LABEL,
                Stroke::new(1.0, EDGE),
            ),
        };
        if d.usable == Usable::No {
            // dashed outline: cannot be used
            dashed_rect(p, r, MUTED);
        } else {
            p.rect_filled(r.translate(vec2(0.0, 3.0)), corner, edge);
            p.rect(top, corner, fill, outline, StrokeKind::Inside);
        }
        if is_sel {
            p.rect_stroke(
                top.expand(1.0),
                corner,
                Stroke::new(3.0, SIGNAL),
                StrokeKind::Outside,
            );
        }

        let pad = u * 0.1;
        let key_rect = p.text(
            top.left_top() + vec2(pad, pad * 0.7),
            Align2::LEFT_TOP,
            d.label,
            mono((u * 0.24).max(9.0)),
            text_col,
        );
        if !lk.pad.is_empty() {
            let pad_g = p.layout_no_wrap(lk.pad.clone(), bold((u * 0.18).max(8.0)), MUTED);
            if key_rect.width() + pad_g.size().x + pad * 3.0 <= top.width() {
                p.galley(
                    pos2(top.right() - pad - pad_g.size().x, top.top() + pad * 0.7),
                    pad_g,
                    MUTED,
                );
            }
        }
        if d.usable == Usable::AppKey {
            let what = match d.code {
                "F6" => "Deadzone",
                "F7" => "Overlay",
                "F8" => "Release",
                _ => "Fullscreen",
            };
            let mut size = (u * 0.2).max(8.0);
            while size > 7.0
                && p.layout_no_wrap(what.to_string(), bold(size), KEY).size().x
                    > top.width() - pad * 2.0
            {
                size -= 0.5;
            }
            p.text(
                top.left_bottom() + vec2(pad, -pad * 0.6),
                Align2::LEFT_BOTTOM,
                what,
                bold(size),
                KEY,
            );
        }
        if lk.bound && !lk.action.is_empty() {
            let avail = top.width() - pad * 2.0;
            let longest = lk
                .action
                .split_whitespace()
                .max_by_key(|w| w.len())
                .unwrap_or("")
                .to_string();
            let mut size = (u * 0.19).max(8.5);
            while size > 7.5
                && p.layout_no_wrap(longest.clone(), bold(size), text_col)
                    .size()
                    .x
                    > avail
            {
                size -= 0.5;
            }
            let mut job =
                egui::text::LayoutJob::simple(lk.action.clone(), bold(size), text_col, avail);
            job.wrap.max_rows = if d.row == 0 { 1 } else { 2 };
            job.wrap.break_anywhere = false;
            job.wrap.overflow_character = Some('…');
            let galley = p.layout_job(job);
            let pos = top.left_bottom()
                + vec2(pad, -pad * 0.5 - galley.size().y.min(top.height() * 0.62));
            p.with_clip_rect(top.shrink(1.0))
                .galley(pos, galley, text_col);
        }
        if resp.clicked() && d.usable == Usable::Yes {
            clicked = Some(d.code);
        }
        if d.usable == Usable::Yes {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }

    // hand labels
    let ly = rect.bottom() + 2.0;
    let (lr, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    let _ = ly;
    ui.painter().text(
        pos2(split_x - 10.0, lr.center().y),
        Align2::RIGHT_CENTER,
        "← LEFT HAND",
        mono(11.5),
        LABEL,
    );
    ui.painter().text(
        pos2(split_x + 10.0, lr.center().y),
        Align2::LEFT_CENTER,
        "RIGHT HAND →",
        mono(11.5),
        LABEL,
    );
    clicked
}

fn dashed_rect(p: &egui::Painter, r: Rect, color: Color32) {
    let s = Stroke::new(1.0, color.gamma_multiply(0.6));
    let step = 6.0;
    let mut x = r.left();
    while x < r.right() {
        let x2 = (x + 3.0).min(r.right());
        p.line_segment([pos2(x, r.top()), pos2(x2, r.top())], s);
        p.line_segment([pos2(x, r.bottom()), pos2(x2, r.bottom())], s);
        x += step;
    }
    let mut y = r.top();
    while y < r.bottom() {
        let y2 = (y + 3.0).min(r.bottom());
        p.line_segment([pos2(r.left(), y), pos2(r.left(), y2)], s);
        p.line_segment([pos2(r.right(), y), pos2(r.right(), y2)], s);
        y += step;
    }
}

/// Legend under the keyboard.
pub fn legend(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (fill, stroke, label, dashed) in [
            (KEY, Stroke::new(1.5, INK), "bound", false),
            (IDLE, Stroke::new(1.0, EDGE), "free", false),
            (INK, Stroke::NONE, "app key", false),
            (PAGE, Stroke::NONE, "cannot be used", true),
        ] {
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 20.0), Sense::hover());
            if dashed {
                dashed_rect(ui.painter(), r, MUTED);
            } else {
                ui.painter()
                    .rect(r, CornerRadius::same(5), fill, stroke, StrokeKind::Inside);
            }
            ui.label(
                egui::RichText::new(label)
                    .font(crate::theme::body(14.5))
                    .color(INK),
            );
            ui.add_space(10.0);
        }
    });
    let _ = Shape::Noop;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_codes_are_known_inputs() {
        for d in layout() {
            if d.code.is_empty() || ["Shift", "Ctrl", "Alt"].contains(&d.code) {
                continue;
            }
            assert!(
                egui::Key::from_name(d.code).is_some(),
                "unknown key code {}",
                d.code
            );
            assert_eq!(
                egui::Key::from_name(d.code).unwrap().name(),
                d.code,
                "name round trip for {}",
                d.code
            );
        }
    }

    #[test]
    fn rows_fit() {
        for d in layout() {
            assert!(d.x + d.w <= UNITS + 0.01, "{} too wide", d.label);
        }
    }
}
