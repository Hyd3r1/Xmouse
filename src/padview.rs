//! Live controller drawing: shows exactly what is being sent to the console.
//!
//! The controller is a line drawing (assets/controller.bin for Xbox, controller-ps.bin for a
//! DualSense, made by tools/make-controller-asset.ps1). Each control's area inside its outline
//! is stored as a mask and lit in the signal colour under the lines while it's pressed. Which
//! drawing is used follows the console type (see `crate::profile::playstation_names`).

use egui::{pos2, vec2, Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui};

use crate::mapper::PadFrame;
use crate::profile::Action;
use crate::theme::{bold, EDGE, IDLE, INK, KEY, SIGNAL};

/// One controller drawing and where things are on it (in its own pixels).
struct Layout {
    /// Texture cache key.
    name: &'static str,
    asset: &'static [u8],
    /// Room above the drawing for the trigger bars.
    top: f32,
    left_stick: (f32, f32),
    right_stick: (f32, f32),
    /// The fixed rings around each stick (drawn, since the sticks are cut out of the art).
    wells: &'static [f32],
    well_line: f32,
    /// The moving cap's outer and inner rings and their line widths.
    cap_r: f32,
    cap_inner_r: f32,
    cap_line: f32,
    cap_inner_line: f32,
    /// How far the cap moves at full tilt.
    travel: f32,
    /// Trigger bars: centre x over each bumper, size, label size, names.
    lt_x: f32,
    rt_x: f32,
    bar: (f32, f32),
    bar_text: f32,
    trigger_names: (&'static str, &'static str),
}

const XBOX: Layout = Layout {
    name: "xbox",
    asset: include_bytes!("../assets/controller.bin"),
    top: 46.0,
    left_stick: (158.5, 127.5),
    right_stick: (411.0, 228.5),
    wells: &[48.5],
    well_line: 3.4,
    cap_r: 36.5,
    cap_inner_r: 26.0,
    cap_line: 4.2,
    cap_inner_line: 2.6,
    // A little over the ring, so a tilt is easy to see.
    travel: 15.0,
    lt_x: 167.0,
    rt_x: 487.0,
    bar: (96.0, 26.0),
    bar_text: 15.0,
    trigger_names: ("LT", "RT"),
};

/// The DualSense drawing: two base rings per stick. Its lines are thickened when the asset is
/// made and the ring widths here are the Xbox ones scaled to this drawing, so both controllers
/// look equally bold at the same size.
const DUALSENSE: Layout = Layout {
    name: "dualsense",
    asset: include_bytes!("../assets/controller-ps.bin"),
    top: 38.0,
    left_stick: (183.0, 177.0),
    right_stick: (348.0, 177.0),
    wells: &[39.5, 33.0],
    well_line: 2.8,
    cap_r: 29.5,
    cap_inner_r: 20.0,
    cap_line: 3.4,
    cap_inner_line: 2.1,
    travel: 9.0,
    lt_x: 106.0,
    rt_x: 425.0,
    bar: (76.0, 20.0),
    bar_text: 12.0,
    trigger_names: ("L2", "R2"),
};

/// The drawing for the console type in use.
fn layout() -> &'static Layout {
    if crate::profile::playstation_names() {
        &DUALSENSE
    } else {
        &XBOX
    }
}

/// The masks in an asset, in file order (named for Xbox; on a DualSense they are L1, R1,
/// Create, Options, PS, Triangle, Square, Cross, Circle). The stick masks (`Ls`, `Rs`) stay in
/// the file for its layout but the sticks themselves are drawn in code.
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

/// Number of masks in an asset file.
const PART_COUNT: usize = 15;

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

/// White, alpha-only textures (tinted when drawn), uploaded once per egui context and layout.
fn textures(ctx: &egui::Context, layout: &Layout) -> Textures {
    let id = egui::Id::unique(("xmouse-controller-textures", layout.name));
    if let Some(t) = ctx.data(|d| d.get_temp::<Textures>(id)) {
        return t;
    }
    let asset = parse(layout.asset);
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
    let lines = ctx.load_texture(
        format!("controller-lines-{}", layout.name),
        image(asset.size, &asset.lines),
        opts,
    );
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
                ctx.load_texture(
                    format!("controller-part-{}-{i}", layout.name),
                    image(m.size, &m.alpha),
                    opts,
                ),
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
    let l = layout();
    let tex = textures(ui.ctx(), l);
    let (dw, dh) = (tex.size[0] as f32, tex.size[1] as f32);
    let s = width / dw;
    let (rect, _) = ui.allocate_exact_size(vec2(dw * s, (dh + l.top) * s), Sense::hover());
    let origin = rect.min + vec2(0.0, l.top * s);
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

    // Sticks: fixed rings and a cap that moves with the stick.
    for ((cx, cy), x, y, click) in [
        (l.left_stick, f.lx, f.ly, Action::LeftThumb),
        (l.right_stick, f.rx, f.ry, Action::RightThumb),
    ] {
        for &well in l.wells {
            painter.circle_stroke(at(cx, cy), well * s, Stroke::new(l.well_line * s, INK));
        }
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        let cap = at(cx + x * l.travel, cy + y * l.travel);
        // Looks like the drawing (white cap) and just moves; lit only when clicked in.
        let fill = if f.pressed(click) { SIGNAL } else { KEY };
        painter.circle_filled(cap, l.cap_r * s, fill);
        painter.circle_stroke(cap, l.cap_r * s, Stroke::new(l.cap_line * s, INK));
        painter.circle_stroke(cap, l.cap_inner_r * s, Stroke::new(l.cap_inner_line * s, INK));
    }

    // Triggers: bars above the bumpers that fill with the trigger value.
    let r = |v: f32| CornerRadius::same((v * s).min(255.0) as u8);
    let (lt_name, rt_name) = l.trigger_names;
    for (cx, v, label) in [(l.lt_x, f.lt, lt_name), (l.rt_x, f.rt, rt_name)] {
        let bar = Rect::from_center_size(
            origin + vec2(cx * s, -l.top * s / 2.0),
            vec2(l.bar.0 * s, l.bar.1 * s),
        );
        painter.rect_filled(bar.translate(vec2(0.0, 3.0 * s)), r(9.0), EDGE);
        painter.rect(bar, r(9.0), IDLE, Stroke::new(1.0, EDGE), StrokeKind::Inside);
        if v > 0.0 {
            let fill = Rect::from_min_size(bar.min, vec2(bar.width() * v.min(1.0), bar.height()));
            painter.rect_filled(fill, r(9.0), SIGNAL);
        }
        painter.text(bar.center(), Align2::CENTER_CENTER, label, bold(l.bar_text * s), INK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_assets_have_every_part_inside_the_drawing() {
        for l in [&XBOX, &DUALSENSE] {
            check_layout(l);
        }
    }

    fn check_layout(l: &Layout) {
        let a = parse(l.asset);
        assert_eq!(a.lines.len(), a.size[0] * a.size[1], "{}", l.name);
        assert_eq!(a.parts.len(), PART_COUNT, "{}", l.name);
        for m in &a.parts {
            assert!(m.min[0] + m.size[0] <= a.size[0] && m.min[1] + m.size[1] <= a.size[1]);
            assert!(m.alpha.iter().any(|&v| v > 0), "empty mask in {}", l.name);
        }
        for (cx, cy) in [l.left_stick, l.right_stick] {
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
