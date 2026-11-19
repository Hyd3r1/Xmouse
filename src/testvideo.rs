//! A moving test picture in place of a console's video (`XMOUSE_TEST_VIDEO=30` or `60`, in a
//! demo session), for checking the picture settings by eye.
//!
//! It goes through the same path as decoded video: 1080p frames at the stream's 60 fps, each
//! picture sent `60 / fps` times, the repeats with a little wobble as an encoder would leave.
//! The scene pans sideways (a sky gradient that bands, buildings with fine detail, a textured
//! ground, a bouncing box) under a HUD that stands still.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::stream::{Repaint, Shared, VideoFrame};

const W: usize = 1920;
const H: usize = 1080;
const STREAM_FPS: u32 = 60;

/// 5x7 glyphs, one row per byte (bit 4 = left column).
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        '6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        _ => [0; 7],
    }
}

fn hash(a: u32, b: u32, c: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ c.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h
}

/// An NV12 picture being drawn.
struct Canvas {
    data: Vec<u8>,
}

impl Canvas {
    fn rect(&mut self, x0: usize, y0: usize, w: usize, h: usize, y: u8, uv: Option<(u8, u8)>) {
        for row in y0..(y0 + h).min(H) {
            let x1 = (x0 + w).min(W);
            if x0 < x1 {
                self.data[row * W + x0..row * W + x1].fill(y);
            }
        }
        if let Some((u, v)) = uv {
            for row in y0 / 2..((y0 + h).min(H)).div_ceil(2) {
                for cx in x0 / 2..((x0 + w).min(W)).div_ceil(2) {
                    let i = W * H + row * W + cx * 2;
                    self.data[i] = u;
                    self.data[i + 1] = v;
                }
            }
        }
    }

    fn text(&mut self, s: &str, x: usize, y: usize, scale: usize, luma: u8) {
        for (n, ch) in s.chars().enumerate() {
            let g = glyph(ch);
            for (r, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        self.rect(
                            x + (n * 6 + col) * scale,
                            y + r * scale,
                            scale,
                            scale,
                            luma,
                            None,
                        );
                    }
                }
            }
        }
    }
}

/// The scene the camera pans over, one screen wide (it wraps around).
fn background() -> Canvas {
    let mut c = Canvas {
        data: vec![128u8; W * H * 3 / 2],
    };
    // Sky: a slow dark gradient, the kind that shows bands in 8 bits.
    c.rect(0, 0, W, 440, 0, Some((150, 118)));
    for row in 0..440 {
        for x in 0..W {
            // (wraps cleanly: brightest in the middle of the screen)
            let side = (x as i32 - 960).unsigned_abs() as usize;
            c.data[row * W + x] = (28 + row * 44 / 440 + (960 - side) * 10 / 960) as u8;
        }
    }
    // Ground: checks with fine diagonal lines.
    for row in 760..H {
        for x in 0..W {
            // (Sizes that the pan's step doesn't divide, or the pattern would land on itself
            // and look as if it stood still.)
            let check = ((x / 40) + (row / 40)) % 2;
            let line = ((x + row) % 9 == 0) as usize;
            c.data[row * W + x] = (86 + check * 36 + line * 30) as u8;
        }
    }
    for row in 380..540 {
        for cx in 0..W / 2 {
            let i = W * H + row * W + cx * 2;
            c.data[i] = 112;
            c.data[i + 1] = 122;
        }
    }
    // Buildings with lit windows.
    let mut x = 0usize;
    let mut n = 0u32;
    while x < W {
        let w = (60 + hash(n, 1, 0) % 110) as usize;
        let w = w.min(W - x);
        let h = (120 + hash(n, 2, 0) % 300) as usize;
        let top = 760 - h;
        let wall = (46 + hash(n, 3, 0) % 60) as u8;
        let tint = if n % 3 == 0 { (120, 140) } else { (128, 128) };
        c.rect(x, top, w.saturating_sub(4), h, wall, Some(tint));
        let mut wy = top + 10;
        while wy + 12 < 760 {
            let mut wx = x + 8;
            while wx + 8 < x + w.saturating_sub(8) {
                if hash(n, wx as u32, wy as u32) % 3 != 0 {
                    c.rect(wx, wy, 6, 9, 210, None);
                }
                wx += 17;
            }
            wy += 19;
        }
        x += w;
        n += 1;
    }
    // A sign that moves with the scene: text in motion.
    c.rect(300, 600, 300, 70, 30, Some((128, 128)));
    c.text("XMOUSE", 316, 614, 6, 235);
    c.rect(1250, 640, 150, 36, 235, Some((128, 128)));
    c.text("TEST 0123", 1258, 648, 2, 20);
    c
}

fn compose(bg: &Canvas, content: u32, content_fps: u32) -> Canvas {
    // 360 pixels a second, in whole (even, for the chroma) pixels per picture.
    let speed = ((360 / content_fps.max(1)) as usize).max(2) & !1;
    let sx = (content as usize * speed) % W;
    let mut c = Canvas {
        data: Vec::with_capacity(W * H * 3 / 2),
    };
    for row in 0..H + H / 2 {
        let line = &bg.data[row * W..(row + 1) * W];
        c.data.extend_from_slice(&line[sx..]);
        c.data.extend_from_slice(&line[..sx]);
    }
    // What an encoder leaves behind: every 8x8 block a little off, differently each picture.
    for by in 0..H / 8 {
        for bx in 0..W / 8 {
            let d = (hash(bx as u32, by as u32, content) % 5) as i16 - 2;
            if d == 0 {
                continue;
            }
            for row in by * 8..by * 8 + 8 {
                for p in &mut c.data[row * W + bx * 8..row * W + bx * 8 + 8] {
                    *p = (*p as i16 + d).clamp(16, 235) as u8;
                }
            }
        }
    }
    // A box that moves on its own, against the pan.
    let t = content as f32 * 30.0 / content_fps.max(1) as f32;
    let by = (220.0 + 150.0 * (t * 0.12).sin()) as usize & !1;
    let bx = (900.0 + 260.0 * (t * 0.05).cos()) as usize & !1;
    c.rect(bx, by, 96, 96, 180, Some((90, 230)));
    c.rect(bx + 24, by + 24, 48, 48, 235, None);
    // The HUD stands still.
    c.rect(24, 24, 470, 64, 24, Some((128, 128)));
    c.text(&format!("XMOUSE TEST {content_fps} FPS"), 36, 34, 3, 235);
    c.text(&format!("{:06}", content % 1_000_000), 36, 64, 2, 200);
    c.rect(1700, 860, 180, 180, 40, Some((128, 128)));
    c.rect(1704, 864, 172, 172, 110, None);
    c.rect(1786, 946, 8, 8, 235, Some((128, 128)));
    c
}

/// Start the source. It stops with the session.
pub fn spawn(shared: Arc<Shared>, repaint: Repaint, content_fps: u32) {
    let content_fps = content_fps.clamp(10, STREAM_FPS);
    let repeat = (STREAM_FPS / content_fps).max(1);
    log::info!("test video: {content_fps} fps pictures in a {STREAM_FPS} fps stream");
    let _ = std::thread::Builder::new()
        .name("xmouse-testvideo".into())
        .spawn(move || {
            let bg = background();
            let start = Instant::now();
            let mut current: Option<Canvas> = None;
            let mut rgba = Vec::new();
            let mut n = 0u32;
            while !shared.stopped() {
                let due = start + Duration::from_secs_f64(n as f64 / STREAM_FPS as f64);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let mut data = if n % repeat == 0 || current.is_none() {
                    let c = compose(&bg, n / repeat, STREAM_FPS / repeat);
                    let d = c.data.clone();
                    current = Some(c);
                    d
                } else {
                    // A repeat: the same picture, a level off here and there.
                    let mut d = current.as_ref().unwrap().data.clone();
                    for i in (hash(n, 0, 7) as usize % 37..W * H).step_by(37) {
                        d[i] = match hash(i as u32, n, 9) % 4 {
                            0 => d[i].saturating_add(1),
                            1 => d[i].saturating_sub(1),
                            _ => d[i],
                        };
                    }
                    d
                };
                let frame = if shared.want_nv12.load(Ordering::Relaxed) {
                    VideoFrame::Nv12 {
                        width: W,
                        height: H,
                        data: std::mem::take(&mut data),
                    }
                } else {
                    crate::video::nv12_to_rgba(&data, W, H, &mut rgba);
                    VideoFrame::Rgba(egui::ColorImage::from_rgba_premultiplied([W, H], &rgba))
                };
                crate::video::publish(&shared, &repaint, frame, W, H);
                n += 1;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacing::{DupDetector, Kind, Signature};

    /// The test picture behaves like the stream it stands in for: pictures differ, and the
    /// duplicate detector agrees with how it was built.
    #[test]
    fn pictures_move_and_are_full_frames() {
        let bg = background();
        let (a, b) = (compose(&bg, 10, 30), compose(&bg, 11, 30));
        assert_eq!(a.data.len(), W * H * 3 / 2);
        let mut d = DupDetector::default();
        assert_eq!(d.classify(Signature::from_luma(&a.data, W, H)), Kind::First);
        assert_eq!(d.classify(Signature::from_luma(&b.data, W, H)), Kind::Unique);
        assert_eq!(d.classify(Signature::from_luma(&b.data, W, H)), Kind::Duplicate);
    }
}
