//! Live controller drawing: shows exactly what is being sent to the Xbox.
//!
//! The controller is a line drawing (assets/controller.bin, made by
//! tools/make-controller-asset.ps1). Each control's area inside its outline is stored as a mask
//! and lit in the signal colour under the lines while it's pressed.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui};

use crate::mapper::PadFrame;
use crate::profile::Action;
use crate::theme::{bold, EDGE, IDLE, INK, KEY, SIGNAL};

const ASSET: &[u8] = include_bytes!("../assets/controller.bin");
/// Room above the drawing for the trigger bars, in drawing pixels.
const TOP: f32 = 46.0;

/// The masks in `ASSET`, in file order. The stick masks (`Ls`, `Rs`) stay in the file for its
/// layout but the sticks themselves are drawn in code.
#[derive(Clone, Copy)]
#[allow(dead_code)]
enum Part {
    Lb,
    Rb,
    Ls,
    Rs,
    View,
    Menu,
    Xbox,
    Y,
    X,
    A,
    B,
    Up,
    Down,
    Left,
    Right,
}

/// Masks lit when their button is down (the sticks are drawn separately, see `stick`).
const PARTS: [(Part, Action); 13] = [
    (Part::Lb, Action::LeftShoulder),
    (Part::Rb, Action::RightShoulder),
    (Part::View, Action::View),
    (Part::Menu, Action::Menu),
    (Part::Xbox, Action::Nexus),
    (Part::Y, Action::Y),
    (Part::X, Action::X),
    (Part::A, Action::A),
    (Part::B, Action::B),
    (Part::Up, Action::DPadUp),
    (Part::Down, Action::DPadDown),
    (Part::Left, Action::DPadLeft),
    (Part::Right, Action::DPadRight),
];

/// The sticks, cut out of the line art and drawn here so they can move (drawing pixels,
/// relative to the crop; measured from the drawing's rings).
const LEFT_STICK: (f32, f32) = (158.5, 127.5);
const RIGHT_STICK: (f32, f32) = (411.0, 228.5);
/// The fixed ring around each stick.
const WELL_R: f32 = 48.5;
/// The cap's outer and inner rings.
const CAP_R: f32 = 36.5;
const CAP_INNER_R: f32 = 26.0;
/// How far the cap moves at full tilt (a little over the ring, so a tilt is easy to see).
const STICK_TRAVEL: f32 = 15.0;
/// Number of masks in the asset file.
const PART_COUNT: usize = 15;
/// Trigger bars: centre x over each bumper.
const LT_X: f32 = 167.0;
const RT_X: f32 = 487.0;

struct Mask {
    min: [usize; 2],
    size: [usize; 2],
    alpha: Vec<u8>,
}

struct Asset {
    size: [usize; 2],
    lines: Vec<u8>,
    parts: Vec<Mask>,
}

fn parse(b: &[u8]) -> Asset {
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    assert_eq!(&b[..4], b"XPAD", "controller asset");
    let (w, h) = (u16_at(4), u16_at(6));
    let lines = b[8..8 + w * h].to_vec();
    let mut o = 8 + w * h;
    let n = b[o] as usize;
    assert_eq!(n, PART_COUNT, "controller asset parts");
    o += 1;
    let mut parts = Vec::with_capacity(n);
    for _ in 0..n {
        let (x, y, rw, rh) = (u16_at(o), u16_at(o + 2), u16_at(o + 4), u16_at(o + 6));
        o += 8;
        parts.push(Mask {
            min: [x, y],
            size: [rw, rh],
            alpha: b[o..o + rw * rh].to_vec(),
        });
        o += rw * rh;
    }
    Asset {
        size: [w, h],
        lines,
        parts,
    }
}

#[derive(Clone)]
struct Textures {
    size: [usize; 2],
    lines: egui::TextureHandle,
    parts: Vec<(Rect, egui::TextureHandle)>,
}

/// White, alpha-only textures (tinted when drawn), uploaded once per egui context.
fn textures(ctx: &egui::Context) -> Textures {
    let id = egui::Id::unique("xmouse-controller-textures");
    if let Some(t) = ctx.data(|d| d.get_temp::<Textures>(id)) {
        return t;
    }
    let asset = parse(ASSET);
    let opts = egui::TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..egui::TextureOptions::LINEAR
    };
    let image = |size: [usize; 2], alpha: &[u8]| {
        egui::ColorImage::new(
            size,
            alpha
                .iter()
                .map(|&a| Color32::from_rgba_premultiplied(a, a, a, a))
                .collect(),
        )
    };
    let lines = ctx.load_texture("controller-lines", image(asset.size, &asset.lines), opts);
    let parts = asset
        .parts
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let r = Rect::from_min_size(
                pos2(m.min[0] as f32, m.min[1] as f32),
                vec2(m.size[0] as f32, m.size[1] as f32),
            );
            (
                r,
                ctx.load_texture(format!("controller-part-{i}"), image(m.size, &m.alpha), opts),
            )
        })
        .collect();
    let t = Textures {
        size: asset.size,
        lines,
        parts,
    };
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    t
}

/// Draw the controller `width` points wide.
pub fn pad_view(ui: &mut Ui, f: &PadFrame, width: f32) {
    let tex = textures(ui.ctx());
    let (dw, dh) = (tex.size[0] as f32, tex.size[1] as f32);
    let s = width / dw;
    let (rect, _) = ui.allocate_exact_size(vec2(dw * s, (dh + TOP) * s), Sense::hover());
    let origin = rect.min + vec2(0.0, TOP * s);
    let at = |x: f32, y: f32| origin + vec2(x * s, y * s);
    let painter = ui.painter_at(rect.expand(2.0));
    let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

    // Pressed controls light up under the lines.
    for (part, action) in PARTS {
        if f.pressed(action) {
            let (r, t) = &tex.parts[part as usize];
            let shown = Rect::from_min_max(at(r.min.x, r.min.y), at(r.max.x, r.max.y));
            painter.image(t.id(), shown, full_uv, SIGNAL);
        }
    }

    let drawing = Rect::from_min_size(origin, vec2(dw * s, dh * s));
    painter.image(tex.lines.id(), drawing, full_uv, INK);

    // Sticks: a fixed ring and a cap that moves with the stick.
    for ((cx, cy), x, y, click) in [
        (LEFT_STICK, f.lx, f.ly, Action::LeftThumb),
        (RIGHT_STICK, f.rx, f.ry, Action::RightThumb),
    ] {
        painter.circle_stroke(at(cx, cy), WELL_R * s, Stroke::new(3.4 * s, INK));
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        let cap = at(cx + x * STICK_TRAVEL, cy + y * STICK_TRAVEL);
        // Looks like the drawing (white cap) and just moves; lit only when clicked in.
        let fill = if f.pressed(click) { SIGNAL } else { KEY };
        painter.circle_filled(cap, CAP_R * s, fill);
        painter.circle_stroke(cap, CAP_R * s, Stroke::new(4.2 * s, INK));
        painter.circle_stroke(cap, CAP_INNER_R * s, Stroke::new(2.6 * s, INK));
    }

    // Triggers: bars above the bumpers that fill with the trigger value.
    let r = |v: f32| CornerRadius::same((v * s).min(255.0) as u8);
    for (cx, v, label) in [(LT_X, f.lt, "LT"), (RT_X, f.rt, "RT")] {
        let bar = Rect::from_center_size(
            origin + vec2(cx * s, -TOP * s / 2.0),
            vec2(96.0 * s, 26.0 * s),
        );
        painter.rect_filled(bar.translate(vec2(0.0, 3.0 * s)), r(9.0), EDGE);
        painter.rect(bar, r(9.0), IDLE, Stroke::new(1.0, EDGE), StrokeKind::Inside);
        if v > 0.0 {
            let fill = Rect::from_min_size(bar.min, vec2(bar.width() * v.min(1.0), bar.height()));
            painter.rect_filled(fill, r(9.0), SIGNAL);
        }
        painter.text(bar.center(), Align2::CENTER_CENTER, label, bold(15.0 * s), INK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_asset_has_every_part_inside_the_drawing() {
        let a = parse(ASSET);
        assert_eq!(a.lines.len(), a.size[0] * a.size[1]);
        assert_eq!(a.parts.len(), PART_COUNT);
        for m in &a.parts {
            assert!(m.min[0] + m.size[0] <= a.size[0] && m.min[1] + m.size[1] <= a.size[1]);
            assert!(m.alpha.iter().any(|&v| v > 0), "empty mask");
        }
        for (cx, cy) in [LEFT_STICK, RIGHT_STICK] {
            let ls = &a.parts[Part::Ls as usize];
            let rs = &a.parts[Part::Rs as usize];
            let inside = |m: &Mask| {
                (m.min[0] as f32..(m.min[0] + m.size[0]) as f32).contains(&cx)
                    && (m.min[1] as f32..(m.min[1] + m.size[1]) as f32).contains(&cy)
            };
            assert!(inside(ls) || inside(rs), "stick centre inside a stick");
        }
    }
}
