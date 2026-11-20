//! fgprobe: standalone go/no-go probe for frame generation on video-like content
//! (Phase 0 of FRAMEGEN_PLAN.md). Not part of the app and never shipped.
//!
//! It opens a D3D12 window, plays a synthetic "decoded video" (a CPU-rendered 1080p clip:
//! panning background with text, a disc crossing in front of it, a static HUD box), derives
//! motion vectors from the pictures alone with a block-matching compute shader, and feeds
//! that to FSR Frame Generation or DLSS Frame Generation the way the app would have to:
//! flat depth, identity camera, no engine data. It then reports how many real and generated
//! frames reached the swapchain and compares generated frames with the analytically known
//! in-between picture.
//!
//!   fgprobe <none|fsr|dlss> [--fps 30|60] [--seconds N] [--mv truth|search|zero]
//!           [--out DIR] [--sdk DIR] [--speed N] [--mvsign 1|-1] [--tail gen|normal]
//!
//! The vendor DLLs are loaded by absolute path from `--sdk` / `XMOUSE_SDK_DIR` (default:
//! `<repo>/../sdks`); nothing from the vendors lives in this repository.
//!
//! The `ffx` and `sl` modules are our own minimal transcriptions of struct layouts and
//! constants from two MIT-licensed public header sets:
//!   AMD FidelityFX API        Copyright (C) Advanced Micro Devices, Inc.
//!   NVIDIA Streamline         Copyright (c) NVIDIA CORPORATION
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this
//! software and associated documentation files (the "Software"), to deal in the Software
//! without restriction, including without limitation the rights to use, copy, modify, merge,
//! publish, distribute, sublicense, and/or sell copies of the Software, and to permit
//! persons to whom the Software is furnished to do so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in all copies or
//! substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY
//! OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
//! THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
//! WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
//! CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

#[cfg(not(windows))]
fn main() {
    eprintln!("fgprobe only runs on Windows");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = probe::run() {
        eprintln!("fgprobe failed: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod probe {
    use std::collections::HashMap;
    use std::ffi::{c_char, c_void, CStr};
    use std::mem::{size_of, transmute_copy, zeroed, ManuallyDrop};
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, bail, Context, Result};
    use windows::core::{Interface, BOOL, GUID, HRESULT, PCSTR, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::System::Threading::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // A few system calls whose crate features are not worth enabling for a probe.
    #[link(name = "user32")]
    extern "system" {
        fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
        fn SetProcessDpiAwarenessContext(value: isize) -> BOOL;
    }
    #[link(name = "winmm")]
    extern "system" {
        fn timeBeginPeriod(ms: u32) -> u32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn D3DKMTOpenAdapterFromLuid(p: *mut KmtOpen) -> i32;
        fn D3DKMTQueryAdapterInfo(p: *mut KmtQuery) -> i32;
        fn D3DKMTCloseAdapter(p: *const u32) -> i32;
    }
    #[repr(C)]
    struct KmtOpen {
        luid: LUID,
        handle: u32,
    }
    #[repr(C)]
    struct KmtQuery {
        handle: u32,
        ty: u32,
        data: *mut c_void,
        size: u32,
    }

    // ------------------------------------------------------------------------------------
    // Arguments
    // ------------------------------------------------------------------------------------

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Mode {
        None,
        Fsr,
        Dlss,
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Mv {
        Truth,
        Search,
        Zero,
    }

    struct Args {
        mode: Mode,
        fps: u32,
        seconds: f64,
        mv: Mv,
        mv_sign: f32,
        out: PathBuf,
        sdk: PathBuf,
        speed: f64,
        width: u32,
        height: u32,
        app_id: u32,
        tail_generated_only: bool,
        depth: f32,
        poll: bool,
        fsr_callback: bool,
        focus: bool,
        host: Host,
    }

    fn parse_args() -> Result<Args> {
        let mut a = Args {
            mode: Mode::None,
            fps: 30,
            seconds: 10.0,
            mv: Mv::Search,
            mv_sign: 1.0,
            out: std::env::temp_dir().join("xmouse-fgprobe"),
            sdk: std::env::var_os("XMOUSE_SDK_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("sdks")),
            speed: 1.0,
            width: 1600,
            height: 900,
            app_id: 0,
            tail_generated_only: false,
            depth: 0.5,
            poll: true,
            fsr_callback: true,
            focus: false,
            host: Host::Top,
        };
        let mut tail: Option<bool> = None;
        let mut focus: Option<bool> = None;
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut val = || it.next().ok_or_else(|| anyhow!("{arg} needs a value"));
            match arg.as_str() {
                "none" => a.mode = Mode::None,
                "fsr" => a.mode = Mode::Fsr,
                "dlss" => a.mode = Mode::Dlss,
                "--fps" => a.fps = val()?.parse()?,
                "--seconds" => a.seconds = val()?.parse()?,
                "--mv" => {
                    a.mv = match val()?.as_str() {
                        "truth" => Mv::Truth,
                        "search" => Mv::Search,
                        "zero" => Mv::Zero,
                        o => bail!("unknown --mv {o}"),
                    }
                }
                "--mvsign" => a.mv_sign = val()?.parse()?,
                "--out" => a.out = PathBuf::from(val()?),
                "--sdk" => a.sdk = PathBuf::from(val()?),
                "--speed" => a.speed = val()?.parse()?,
                "--width" => a.width = val()?.parse()?,
                "--height" => a.height = val()?.parse()?,
                "--appid" => a.app_id = val()?.parse()?,
                "--depth" => a.depth = val()?.parse()?,
                "--tail" => tail = Some(val()? == "gen"),
                "--poll" => a.poll = val()? != "off",
                "--focus" => focus = Some(val()? != "off"),
                "--host" => {
                    a.host = match val()?.as_str() {
                        "top" => Host::Top,
                        "child" => Host::Child,
                        "popup" => Host::Popup,
                        o => bail!("unknown --host {o}"),
                    }
                }
                "--fsr-callback" => a.fsr_callback = val()? != "off",
                o => bail!("unknown argument {o}"),
            }
        }
        // DLSS-G does not hand out its generated frame, so its tail shows generated frames
        // only and we grab them from the window. FSR gives them to a callback instead.
        a.tail_generated_only = tail.unwrap_or(a.mode == Mode::Dlss);
        // DLSS-G only generates while its window is the foreground window, so that mode has
        // to take the focus; the others leave the user's focus alone.
        a.focus = focus.unwrap_or(a.mode == Mode::Dlss);
        if a.fps == 0 || a.seconds <= 0.0 {
            bail!("--fps and --seconds must be positive");
        }
        Ok(a)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn wide_path(p: &Path) -> Vec<u16> {
        wide(&p.to_string_lossy())
    }

    // ------------------------------------------------------------------------------------
    // The synthetic clip. Everything is a pure function of time so the true in-between
    // picture exists and a generated frame can be scored against it.
    // ------------------------------------------------------------------------------------

    const SRC_W: usize = 1920;
    const SRC_H: usize = 1080;
    const WORLD_W: usize = 3840;
    const OBJ_R: f64 = 120.0;
    /// Background pan and object speed in source pixels per source frame. All even, so the
    /// half-way picture sits on whole pixels and needs no resampling.
    const PAN: f64 = 16.0;
    const OBJ_VX: f64 = 24.0;
    const OBJ_VY: f64 = 8.0;
    /// x0, y0, x1, y1 in source pixels.
    const HUD: (usize, usize, usize, usize) = (40, 40, 520, 180);
    const CNT: (usize, usize, usize, usize) = (1700, 1004, 1900, 1064);
    const TEXT_ROWS: (usize, usize) = (640, 870);

    struct Scene {
        world: Vec<u8>,
        hud: Vec<u8>,
        digits: Vec<(usize, usize, Vec<u8>)>,
        speed: f64,
    }

    /// White-on-black text rendered by GDI, returned as a coverage mask.
    unsafe fn text_mask(text: &str, face: &str, px: i32, weight: i32) -> (usize, usize, Vec<u8>) {
        let hdc = CreateCompatibleDC(None);
        let facew = wide(face);
        let font = CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            PCWSTR(facew.as_ptr()),
        );
        let old_font = SelectObject(hdc, font.into());
        let tw: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &tw, &mut size);
        let (w, h) = (size.cx.max(1) as usize + 4, size.cy.max(1) as usize + 4);
        let mut bmi: BITMAPINFO = zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        let mut bits: *mut c_void = null_mut();
        let mut mask = vec![0u8; w * h];
        if let Ok(dib) = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old_bmp = SelectObject(hdc, dib.into());
            SetTextColor(hdc, COLORREF(0x00FF_FFFF));
            SetBkColor(hdc, COLORREF(0));
            let _ = TextOutW(hdc, 2, 2, &tw);
            let _ = GdiFlush();
            let px = std::slice::from_raw_parts(bits as *const u8, w * h * 4);
            for i in 0..w * h {
                mask[i] = px[i * 4 + 1];
            }
            SelectObject(hdc, old_bmp);
            let _ = DeleteObject(dib.into());
        }
        SelectObject(hdc, old_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(hdc);
        (w, h, mask)
    }

    fn hash2(x: usize, y: usize) -> u32 {
        let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^ (h >> 12)
    }

    /// Alpha-blends a coverage mask in one colour into an RGBA image, wrapping horizontally.
    fn blend_mask(
        img: &mut [u8],
        iw: usize,
        ih: usize,
        x0: usize,
        y0: usize,
        mask: &(usize, usize, Vec<u8>),
        color: [u8; 3],
    ) {
        let (mw, mh, m) = mask;
        for y in 0..*mh {
            if y0 + y >= ih {
                break;
            }
            for x in 0..*mw {
                let a = m[y * mw + x] as u32;
                if a == 0 {
                    continue;
                }
                let o = ((y0 + y) * iw + (x0 + x) % iw) * 4;
                for c in 0..3 {
                    img[o + c] = ((img[o + c] as u32 * (255 - a) + color[c] as u32 * a) / 255) as u8;
                }
            }
        }
    }

    impl Scene {
        unsafe fn new(speed: f64) -> Scene {
            let mut world = vec![0u8; WORLD_W * SRC_H * 4];
            let tau = std::f64::consts::TAU;
            // Smooth colour fields with periods that divide the world width (it wraps), a
            // grid, and per-pixel grain so no area is flat the way a rendered image can be.
            for y in 0..SRC_H {
                let yf = y as f64;
                for x in 0..WORLD_W {
                    let xf = x as f64;
                    let mut r = 95.0 + 55.0 * (tau * xf / 960.0).sin() + 30.0 * (tau * yf / 540.0).sin();
                    let mut g = 105.0 + 45.0 * (tau * (xf / 1280.0 + yf / 720.0)).sin();
                    let mut b = 120.0 + 55.0 * (tau * xf / 640.0).cos() + 20.0 * (tau * yf / 360.0).cos();
                    if x % 160 < 2 || y % 135 < 2 {
                        r += 40.0;
                        g += 40.0;
                        b += 40.0;
                    }
                    let n = (hash2(x, y) % 13) as f64 - 6.0;
                    let o = (y * WORLD_W + x) * 4;
                    world[o] = (r + n).clamp(0.0, 255.0) as u8;
                    world[o + 1] = (g + n).clamp(0.0, 255.0) as u8;
                    world[o + 2] = (b + n).clamp(0.0, 255.0) as u8;
                    world[o + 3] = 255;
                }
            }
            // Hard-edged boxes, some with a fine checker, standing in for scenery.
            let mut seed = 0x1234_5678u32;
            let mut rnd = |m: u32| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) % m
            };
            for i in 0..70 {
                let (x0, y0) = (rnd(WORLD_W as u32) as usize, rnd(560) as usize);
                let (w, h) = (60 + rnd(260) as usize, 40 + rnd(220) as usize);
                let col = [60 + rnd(190) as u8, 60 + rnd(190) as u8, 60 + rnd(190) as u8];
                for y in y0..(y0 + h).min(SRC_H) {
                    for x in x0..x0 + w {
                        let edge = x - x0 < 3 || x0 + w - x <= 3 || y - y0 < 3 || y0 + h - y <= 3;
                        let dark = edge || (i % 3 == 0 && ((x - x0) / 16 + (y - y0) / 16) % 2 == 0);
                        let o = (y * WORLD_W + x % WORLD_W) * 4;
                        for c in 0..3 {
                            world[o + c] = if dark { col[c] / 3 } else { col[c] };
                        }
                    }
                }
            }
            // Text that travels with the background, large to small.
            let line = "XMOUSE frame generation probe 0123456789  The quick brown fox jumps over the lazy dog";
            let rows: [(usize, i32, [u8; 3]); 3] =
                [(TEXT_ROWS.0 + 10, 72, [255, 255, 255]), (TEXT_ROWS.0 + 110, 40, [255, 230, 90]), (TEXT_ROWS.0 + 175, 22, [200, 255, 255])];
            for (y, px, col) in rows {
                let m = text_mask(line, "Segoe UI", px, 600);
                let mut x = 40;
                while x + m.0 + 40 < WORLD_W {
                    blend_mask(&mut world, WORLD_W, SRC_H, x, y, &m, col);
                    x += m.0 + 80;
                }
            }

            // The HUD never moves: an opaque panel with a border, text and a bar.
            let (hw, hh) = (HUD.2 - HUD.0, HUD.3 - HUD.1);
            let mut hud = vec![0u8; hw * hh * 4];
            for y in 0..hh {
                for x in 0..hw {
                    let edge = x < 3 || y < 3 || x >= hw - 3 || y >= hh - 3;
                    let c: [u8; 3] = if edge { [210, 210, 210] } else { [16, 18, 26] };
                    let o = (y * hw + x) * 4;
                    hud[o..o + 3].copy_from_slice(&c);
                    hud[o + 3] = 255;
                }
            }
            blend_mask(&mut hud, hw, hh, 16, 10, &text_mask("HP 100   AMMO 30/90", "Segoe UI", 34, 700), [255, 255, 255]);
            blend_mask(&mut hud, hw, hh, 16, 58, &text_mask("static HUD - must not move", "Segoe UI", 22, 400), [150, 220, 255]);
            for y in 100..122 {
                for x in 16..hw - 16 {
                    let o = (y * hw + x) * 4;
                    let c: [u8; 3] = if x < 300 { [60, 200, 80] } else { [200, 50, 50] };
                    hud[o..o + 3].copy_from_slice(&c);
                }
            }

            let digits = (0..10).map(|d| text_mask(&d.to_string(), "Consolas", 44, 700)).collect();
            Scene { world, hud, digits, speed }
        }

        fn obj_pos(&self, t: f64) -> (f64, f64) {
            let span = SRC_W as f64 + 2.0 * OBJ_R;
            let cx = -OBJ_R + (OBJ_VX * self.speed * t).rem_euclid(span);
            let p = (OBJ_VY * self.speed * t).rem_euclid(1440.0);
            let cy = 120.0 + if p < 720.0 { p } else { 1440.0 - p };
            (cx.round(), cy.round())
        }

        fn obj_visible(&self, t: f64) -> bool {
            let (cx, _) = self.obj_pos(t);
            (300.0..=1620.0).contains(&cx)
        }

        /// The picture at time `t` (in source frames, fractional allowed) as RGBA.
        fn render(&self, t: f64, out: &mut [u8]) {
            let pan = ((PAN * self.speed * t).round() as i64).rem_euclid(WORLD_W as i64) as usize;
            for y in 0..SRC_H {
                let row = &self.world[y * WORLD_W * 4..(y + 1) * WORLD_W * 4];
                let dst = &mut out[y * SRC_W * 4..(y + 1) * SRC_W * 4];
                let first = (WORLD_W - pan).min(SRC_W);
                dst[..first * 4].copy_from_slice(&row[pan * 4..(pan + first) * 4]);
                if first < SRC_W {
                    dst[first * 4..].copy_from_slice(&row[..(SRC_W - first) * 4]);
                }
            }
            // The disc: checkered, white rim, magenta core. It passes in front of the
            // background and behind the HUD.
            let (cx, cy) = self.obj_pos(t);
            let (cx, cy, r) = (cx as i64, cy as i64, OBJ_R as i64);
            for y in (cy - r).max(0)..=(cy + r).min(SRC_H as i64 - 1) {
                for x in (cx - r).max(0)..=(cx + r).min(SRC_W as i64 - 1) {
                    let (dx, dy) = (x - cx, y - cy);
                    let d2 = dx * dx + dy * dy;
                    if d2 > r * r {
                        continue;
                    }
                    let c: [u8; 3] = if dx.abs() <= 24 && dy.abs() <= 24 {
                        [255, 0, 255]
                    } else if d2 > (r - 6) * (r - 6) {
                        [255, 255, 255]
                    } else if (((dx + r) / 24 + (dy + r) / 24) & 1) == 0 {
                        [240, 220, 40]
                    } else {
                        [20, 30, 120]
                    };
                    let o = (y as usize * SRC_W + x as usize) * 4;
                    out[o..o + 3].copy_from_slice(&c);
                }
            }
            let (hw, hh) = (HUD.2 - HUD.0, HUD.3 - HUD.1);
            for y in 0..hh {
                let o = ((HUD.1 + y) * SRC_W + HUD.0) * 4;
                out[o..o + hw * 4].copy_from_slice(&self.hud[y * hw * 4..(y + 1) * hw * 4]);
            }
            // A frame counter: a HUD element that does change every frame.
            for y in CNT.1..CNT.3 {
                for x in CNT.0..CNT.2 {
                    let o = (y * SRC_W + x) * 4;
                    out[o..o + 3].copy_from_slice(&[10, 10, 10]);
                }
            }
            let n = t.floor().max(0.0) as u64 % 100_000;
            for i in 0..5 {
                let d = (n / 10u64.pow(4 - i as u32) % 10) as usize;
                blend_mask(out, SRC_W, SRC_H, CNT.0 + 10 + i * 36, CNT.1 + 4, &self.digits[d], [255, 255, 255]);
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Pictures on the CPU: scaling, scoring, PNG
    // ------------------------------------------------------------------------------------

    #[derive(Clone)]
    struct Img {
        w: usize,
        h: usize,
        rgb: Vec<u8>,
    }

    type Rect = (usize, usize, usize, usize);

    /// Bilinear resize with the same sample positions the GPU blit uses, so a real frame
    /// read back from the swapchain and the CPU reference agree to within rounding.
    fn scale_rgba(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Img {
        let axis = |d: usize, s: usize, n: usize| -> Vec<(usize, usize, f32)> {
            (0..n)
                .map(|i| {
                    let f = ((i as f32 + 0.5) * s as f32 / d as f32 - 0.5).clamp(0.0, s as f32 - 1.0);
                    let i0 = f.floor() as usize;
                    (i0, (i0 + 1).min(s - 1), f - i0 as f32)
                })
                .collect()
        };
        let xs = axis(dw, sw, dw);
        let ys = axis(dh, sh, dh);
        let mut rgb = vec![0u8; dw * dh * 3];
        for (y, &(y0, y1, wy)) in ys.iter().enumerate() {
            for (x, &(x0, x1, wx)) in xs.iter().enumerate() {
                for c in 0..3 {
                    let p = |xx: usize, yy: usize| src[(yy * sw + xx) * 4 + c] as f32;
                    let top = p(x0, y0) * (1.0 - wx) + p(x1, y0) * wx;
                    let bot = p(x0, y1) * (1.0 - wx) + p(x1, y1) * wx;
                    rgb[(y * dw + x) * 3 + c] = (top * (1.0 - wy) + bot * wy + 0.5) as u8;
                }
            }
        }
        Img { w: dw, h: dh, rgb }
    }

    fn psnr(a: &Img, b: &Img, rect: Rect, exclude: Option<Rect>) -> f64 {
        let (mut se, mut n) = (0f64, 0u64);
        for y in rect.1..rect.3.min(a.h) {
            for x in rect.0..rect.2.min(a.w) {
                if let Some(e) = exclude {
                    if x >= e.0 && x < e.2 && y >= e.1 && y < e.3 {
                        continue;
                    }
                }
                let o = (y * a.w + x) * 3;
                for c in 0..3 {
                    let d = a.rgb[o + c] as f64 - b.rgb[o + c] as f64;
                    se += d * d;
                }
                n += 3;
            }
        }
        if n == 0 {
            return 0.0;
        }
        let mse = se / n as f64;
        if mse < 1e-9 {
            99.0
        } else {
            10.0 * (255.0 * 255.0 / mse).log10()
        }
    }

    fn blend(a: &Img, b: &Img) -> Img {
        let rgb = a.rgb.iter().zip(&b.rgb).map(|(x, y)| ((*x as u16 + *y as u16 + 1) / 2) as u8).collect();
        Img { w: a.w, h: a.h, rgb }
    }

    fn crop(a: &Img, r: Rect) -> Img {
        let (w, h) = (r.2 - r.0, r.3 - r.1);
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in r.1..r.3 {
            rgb.extend_from_slice(&a.rgb[(y * a.w + r.0) * 3..(y * a.w + r.2) * 3]);
        }
        Img { w, h, rgb }
    }

    /// Panels side by side with a thin separator.
    fn hcat(parts: &[Img]) -> Img {
        let h = parts[0].h;
        let w: usize = parts.iter().map(|p| p.w).sum::<usize>() + 4 * (parts.len() - 1);
        let mut rgb = vec![255u8; w * h * 3];
        let mut x0 = 0;
        for p in parts {
            for y in 0..h {
                rgb[(y * w + x0) * 3..(y * w + x0 + p.w) * 3].copy_from_slice(&p.rgb[y * p.w * 3..(y + 1) * p.w * 3]);
            }
            x0 += p.w + 4;
        }
        Img { w, h, rgb }
    }

    fn crc32(parts: &[&[u8]]) -> u32 {
        let mut c = !0u32;
        for part in parts {
            for &b in *part {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
                }
            }
        }
        !c
    }

    /// PNG with stored (uncompressed) deflate blocks: no encoder dependency for a probe.
    fn write_png(path: &Path, img: &Img) -> Result<()> {
        let mut raw = Vec::with_capacity((img.w * 3 + 1) * img.h);
        for y in 0..img.h {
            raw.push(0);
            raw.extend_from_slice(&img.rgb[y * img.w * 3..(y + 1) * img.w * 3]);
        }
        let mut z = vec![0x78u8, 0x01];
        let mut chunks = raw.chunks(65535).peekable();
        while let Some(c) = chunks.next() {
            z.push(if chunks.peek().is_none() { 1 } else { 0 });
            let l = c.len() as u16;
            z.extend_from_slice(&l.to_le_bytes());
            z.extend_from_slice(&(!l).to_le_bytes());
            z.extend_from_slice(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for part in raw.chunks(5552) {
            for &x in part {
                a += x as u32;
                b += a;
            }
            a %= 65521;
            b %= 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());

        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut chunk = |ty: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(ty);
            out.extend_from_slice(data);
            out.extend_from_slice(&crc32(&[ty, data]).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(img.w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(img.h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
    }

    // ------------------------------------------------------------------------------------
    // AMD FidelityFX API (frame generation + its swapchain), minimal declarations
    // ------------------------------------------------------------------------------------

    mod ffx {
        use std::ffi::c_void;

        pub const CREATE_FG: u64 = 0x0002_0001;
        pub const CONFIGURE_FG: u64 = 0x0002_0002;
        pub const PREPARE_V1: u64 = 0x0002_0004;
        pub const PREPARE_V2: u64 = 0x0002_000c;
        pub const CREATE_FG_VERSION: u64 = 0x0002_000e;
        pub const FG_VERSION: u32 = (4 << 22) | 2;
        pub const CREATE_SWAPCHAIN_FOR_HWND: u64 = 0x0003_0006;
        pub const SWAPCHAIN_WAIT_FOR_PRESENTS: u64 = 0x0003_0007;
        pub const CREATE_SWAPCHAIN_VERSION: u64 = 0x0003_000b;
        pub const SWAPCHAIN_VERSION: u32 = 4 << 22;
        pub const CREATE_BACKEND_DX12: u64 = 0x0000_0002;
        pub const QUERY_GET_VERSIONS: u64 = 4;
        pub const CONFIGURE_GLOBAL_DEBUG: u64 = 7;

        pub const FORMAT_R8G8B8A8_UNORM: u32 = 10;
        pub const FORMAT_R16G16_FLOAT: u32 = 18;
        pub const FORMAT_R32_FLOAT: u32 = 28;
        pub const RESOURCE_TYPE_TEXTURE2D: u32 = 2;
        pub const USAGE_UAV: u32 = 2;
        pub const STATE_COMPUTE_READ: u32 = 4;

        #[repr(C)]
        pub struct Header {
            pub ty: u64,
            pub next: *mut Header,
        }
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        pub struct Dim2 {
            pub width: u32,
            pub height: u32,
        }
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        pub struct Float2 {
            pub x: f32,
            pub y: f32,
        }
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        pub struct Rect {
            pub left: i32,
            pub top: i32,
            pub width: i32,
            pub height: i32,
        }
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        pub struct ResourceDesc {
            pub ty: u32,
            pub format: u32,
            pub width: u32,
            pub height: u32,
            pub depth: u32,
            pub mip_count: u32,
            pub flags: u32,
            pub usage: u32,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct Resource {
            pub resource: *mut c_void,
            pub desc: ResourceDesc,
            pub state: u32,
        }
        impl Resource {
            pub fn empty() -> Resource {
                Resource { resource: std::ptr::null_mut(), desc: ResourceDesc::default(), state: 0 }
            }
        }

        #[repr(C)]
        pub struct CreateSwapChainForHwnd {
            pub header: Header,
            pub swapchain: *mut *mut c_void,
            pub hwnd: *mut c_void,
            pub desc: *const c_void,
            pub fullscreen_desc: *const c_void,
            pub dxgi_factory: *mut c_void,
            pub game_queue: *mut c_void,
        }
        #[repr(C)]
        pub struct CreateVersion {
            pub header: Header,
            pub version: u32,
        }
        #[repr(C)]
        pub struct CreateBackendDx12 {
            pub header: Header,
            pub device: *mut c_void,
        }
        #[repr(C)]
        pub struct CreateFg {
            pub header: Header,
            pub flags: u32,
            pub display_size: Dim2,
            pub max_render_size: Dim2,
            pub back_buffer_format: u32,
        }

        /// What the swapchain hands to the present callback for every frame it shows.
        #[repr(C)]
        pub struct PresentCallbackDesc {
            pub header: Header,
            pub device: *mut c_void,
            pub command_list: *mut c_void,
            pub current_back_buffer: Resource,
            pub current_ui: Resource,
            pub output_swap_chain_buffer: Resource,
            pub is_generated_frame: bool,
            pub frame_id: u64,
        }
        pub type PresentCallback = unsafe extern "C" fn(*mut PresentCallbackDesc, *mut c_void) -> u32;
        pub type GenerateCallback = unsafe extern "C" fn(*mut Header, *mut c_void) -> u32;

        #[repr(C)]
        pub struct ConfigureFg {
            pub header: Header,
            pub swap_chain: *mut c_void,
            pub present_callback: Option<PresentCallback>,
            pub present_callback_user_context: *mut c_void,
            pub frame_generation_callback: Option<GenerateCallback>,
            pub frame_generation_callback_user_context: *mut c_void,
            pub frame_generation_enabled: bool,
            pub allow_async_workloads: bool,
            pub hudless_color: Resource,
            pub flags: u32,
            pub only_present_generated: bool,
            pub generation_rect: Rect,
            pub frame_id: u64,
        }

        /// The older prepare descriptor is a strict prefix of this one, so a runtime that
        /// does not know V2 can be given the same bytes under the older type id.
        #[repr(C)]
        pub struct PrepareV2 {
            pub header: Header,
            pub frame_id: u64,
            pub flags: u32,
            pub command_list: *mut c_void,
            pub render_size: Dim2,
            pub jitter_offset: Float2,
            pub motion_vector_scale: Float2,
            pub frame_time_delta: f32,
            pub reset: bool,
            pub camera_near: f32,
            pub camera_far: f32,
            pub camera_fov_angle_vertical: f32,
            pub view_space_to_meters_factor: f32,
            pub depth: Resource,
            pub motion_vectors: Resource,
            pub camera_position: [f32; 3],
            pub camera_up: [f32; 3],
            pub camera_right: [f32; 3],
            pub camera_forward: [f32; 3],
        }

        #[repr(C)]
        pub struct QueryGetVersions {
            pub header: Header,
            pub create_desc_type: u64,
            pub device: *mut c_void,
            pub output_count: *mut u64,
            pub version_ids: *mut u64,
            pub version_names: *mut *const std::ffi::c_char,
        }
        #[repr(C)]
        pub struct ConfigureGlobalDebug {
            pub header: Header,
            pub effect_id: u64,
            pub message: Option<unsafe extern "C" fn(u32, *const u16)>,
            pub debug_level: u32,
        }

        pub type CreateContext = unsafe extern "C" fn(*mut *mut c_void, *mut Header, *const c_void) -> u32;
        pub type DestroyContext = unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> u32;
        pub type Configure = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;
        pub type Query = unsafe extern "C" fn(*mut *mut c_void, *mut Header) -> u32;
        pub type Dispatch = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;

        const _: () = assert!(std::mem::size_of::<Resource>() == 48);
        const _: () = assert!(std::mem::size_of::<PresentCallbackDesc>() == 192);

        pub fn code_name(c: u32) -> &'static str {
            match c {
                0 => "OK",
                1 => "ERROR",
                2 => "ERROR_UNKNOWN_DESCTYPE",
                3 => "ERROR_RUNTIME_ERROR",
                4 => "NO_PROVIDER",
                5 => "ERROR_MEMORY",
                6 => "ERROR_PARAMETER",
                7 => "PROVIDER_NO_SUPPORT_NEW_DESCTYPE",
                _ => "?",
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // NVIDIA Streamline (core, DLSS-G, Reflex, PCL), minimal declarations.
    // Streamline's structs are C++ classes deriving from one base; with the x64 MSVC ABI the
    // derived members simply follow the 32-byte base, which `#[repr(C)]` reproduces. The
    // sizes are asserted against values worked out from the headers.
    // ------------------------------------------------------------------------------------

    mod sl {
        use std::ffi::{c_char, c_void};

        pub const SDK_VERSION: u64 = (2u64 << 48) | (14u64 << 32) | (16u64 << 16) | 0xfedc;
        pub const FEATURE_REFLEX: u32 = 3;
        pub const FEATURE_PCL: u32 = 4;
        pub const FEATURE_DLSS_G: u32 = 1000;
        pub const BUFFER_DEPTH: u32 = 0;
        pub const BUFFER_MOTION_VECTORS: u32 = 1;
        pub const LIFECYCLE_VALID_UNTIL_PRESENT: u32 = 1;
        pub const PREF_DISABLE_CL_STATE_TRACKING: u64 = 1 << 0;
        pub const PREF_FRAME_BASED_TAGGING: u64 = 1 << 7;
        pub const REQ_HARDWARE_SCHEDULING: u32 = 1 << 4;
        pub const DLSSG_MODE_OFF: u32 = 0;
        pub const DLSSG_MODE_ON: u32 = 1;
        pub const DLSSG_FLAG_SHOW_ONLY_INTERPOLATED: u32 = 1 << 0;
        pub const REFLEX_LOW_LATENCY: u32 = 1;
        pub const INVALID_FLOAT: f32 = f32::MAX;
        pub const FALSE: u8 = 0;
        pub const TRUE: u8 = 1;
        pub const INVALID: u8 = 2;

        pub const PCL_SIMULATION_START: u32 = 0;
        pub const PCL_SIMULATION_END: u32 = 1;
        pub const PCL_RENDER_SUBMIT_START: u32 = 2;
        pub const PCL_RENDER_SUBMIT_END: u32 = 3;
        pub const PCL_PRESENT_START: u32 = 4;
        pub const PCL_PRESENT_END: u32 = 5;

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

        #[repr(C)]
        pub struct Base {
            pub next: *mut c_void,
            pub ty: Guid,
            pub version: usize,
        }
        impl Base {
            pub const fn new(ty: Guid, version: usize) -> Base {
                Base { next: std::ptr::null_mut(), ty, version }
            }
        }

        const G_PREFERENCES: Guid = Guid(0x1ca10965, 0xbf8e, 0x432b, [0x8d, 0xa1, 0x67, 0x16, 0xd8, 0x79, 0xfb, 0x14]);
        const G_RESOURCE: Guid = Guid(0x3a9d70cf, 0x2418, 0x4b72, [0x83, 0x91, 0x13, 0xf8, 0x72, 0x1c, 0x72, 0x61]);
        const G_RESOURCE_TAG: Guid = Guid(0x4c6a5aad, 0xb445, 0x496c, [0x87, 0xff, 0x1a, 0xf3, 0x84, 0x5b, 0xe6, 0x53]);
        const G_VIEWPORT: Guid = Guid(0x171b6435, 0x9b3c, 0x4fc8, [0x99, 0x94, 0xfb, 0xe5, 0x25, 0x69, 0xaa, 0xa4]);
        const G_REQUIREMENTS: Guid = Guid(0x66714097, 0xac6d, 0x4bc6, [0x89, 0x15, 0x1e, 0x0f, 0x55, 0xa6, 0xb6, 0x1f]);
        const G_ADAPTER_INFO: Guid = Guid(0x0677315f, 0xa746, 0x4492, [0x9f, 0x42, 0xcb, 0x61, 0x42, 0xc9, 0xc3, 0xd4]);
        const G_CONSTANTS: Guid = Guid(0xdcd35ad7, 0x4e4a, 0x4bad, [0xa9, 0x0c, 0xe0, 0xc4, 0x9e, 0xb2, 0x3a, 0xfe]);
        const G_DLSSG_OPTIONS: Guid = Guid(0xfac5f1cb, 0x2dfd, 0x4f36, [0xa1, 0xe6, 0x3a, 0x9e, 0x86, 0x52, 0x56, 0xc5]);
        const G_DLSSG_STATE: Guid = Guid(0xcc8ac8e1, 0xa179, 0x44f5, [0x97, 0xfa, 0xe7, 0x41, 0x12, 0xf9, 0xbc, 0x61]);
        const G_REFLEX_OPTIONS: Guid = Guid(0xf03af81a, 0x6d0b, 0x4902, [0xa6, 0x51, 0xc4, 0x96, 0x5e, 0x21, 0x54, 0x34]);

        pub type LogCallback = unsafe extern "C" fn(u32, *const c_char);

        #[repr(C)]
        pub struct Preferences {
            pub base: Base,
            pub show_console: bool,
            pub log_level: u32,
            pub paths_to_plugins: *const *const u16,
            pub num_paths_to_plugins: u32,
            pub path_to_logs_and_data: *const u16,
            pub allocate_callback: *const c_void,
            pub release_callback: *const c_void,
            pub log_message_callback: Option<LogCallback>,
            pub flags: u64,
            pub features_to_load: *const u32,
            pub num_features_to_load: u32,
            pub application_id: u32,
            pub engine: u32,
            pub engine_version: *const c_char,
            pub project_id: *const c_char,
            pub render_api: u32,
        }
        impl Preferences {
            pub fn new() -> Preferences {
                let mut p: Preferences = unsafe { std::mem::zeroed() };
                p.base = Base::new(G_PREFERENCES, 1);
                p.log_level = 1;
                p.render_api = 1; // D3D12
                p
            }
        }

        #[repr(C)]
        pub struct Resource {
            pub base: Base,
            pub ty: u8,
            pub native: *mut c_void,
            pub memory: *mut c_void,
            pub view: *mut c_void,
            pub state: u32,
            pub width: u32,
            pub height: u32,
            pub native_format: u32,
            pub mip_levels: u32,
            pub array_layers: u32,
            pub gpu_virtual_address: u64,
            pub flags: u32,
            pub usage: u32,
            pub internal_flags: u16,
            pub reserved: u16,
        }
        impl Resource {
            pub fn tex2d(native: *mut c_void, state: u32, width: u32, height: u32, format: u32) -> Resource {
                let mut r: Resource = unsafe { std::mem::zeroed() };
                r.base = Base::new(G_RESOURCE, 1);
                r.native = native;
                r.state = state;
                r.width = width;
                r.height = height;
                r.native_format = format;
                r.mip_levels = 1;
                r.array_layers = 1;
                r
            }
        }

        #[repr(C)]
        pub struct ResourceTag {
            pub base: Base,
            pub resource: *mut Resource,
            pub ty: u32,
            pub lifecycle: u32,
            pub extent: [u32; 4],
        }
        impl ResourceTag {
            pub fn new(resource: *mut Resource, ty: u32, lifecycle: u32) -> ResourceTag {
                ResourceTag { base: Base::new(G_RESOURCE_TAG, 1), resource, ty, lifecycle, extent: [0; 4] }
            }
        }

        #[repr(C)]
        pub struct ViewportHandle {
            pub base: Base,
            pub value: u32,
        }
        impl ViewportHandle {
            pub fn new(value: u32) -> ViewportHandle {
                ViewportHandle { base: Base::new(G_VIEWPORT, 1), value }
            }
        }

        #[repr(C)]
        #[derive(Clone, Copy, Default, Debug)]
        pub struct Version {
            pub major: u32,
            pub minor: u32,
            pub build: u32,
        }

        #[repr(C)]
        pub struct FeatureRequirements {
            pub base: Base,
            pub flags: u32,
            pub max_num_cpu_threads: u32,
            pub max_num_viewports: u32,
            pub num_required_tags: u32,
            pub required_tags: *const u32,
            pub os_version_detected: Version,
            pub os_version_required: Version,
            pub driver_version_detected: Version,
            pub driver_version_required: Version,
            pub vk_num_compute_queues_required: u32,
            pub vk_num_graphics_queues_required: u32,
            pub vk_num_device_extensions: u32,
            pub vk_device_extensions: *const *const c_char,
            pub vk_num_instance_extensions: u32,
            pub vk_instance_extensions: *const *const c_char,
            pub vk_num_features12: u32,
            pub vk_features12: *const *const c_char,
            pub vk_num_features13: u32,
            pub vk_features13: *const *const c_char,
            pub vk_num_optical_flow_queues_required: u32,
        }
        impl FeatureRequirements {
            pub fn new() -> FeatureRequirements {
                let mut r: FeatureRequirements = unsafe { std::mem::zeroed() };
                r.base = Base::new(G_REQUIREMENTS, 2);
                r
            }
        }

        #[repr(C)]
        pub struct AdapterInfo {
            pub base: Base,
            pub device_luid: *const u8,
            pub device_luid_size_in_bytes: u32,
            pub vk_physical_device: *mut c_void,
        }
        impl AdapterInfo {
            pub fn new(luid: *const u8, size: u32) -> AdapterInfo {
                AdapterInfo {
                    base: Base::new(G_ADAPTER_INFO, 1),
                    device_luid: luid,
                    device_luid_size_in_bytes: size,
                    vk_physical_device: std::ptr::null_mut(),
                }
            }
        }

        #[repr(C)]
        pub struct Constants {
            pub base: Base,
            pub camera_view_to_clip: [[f32; 4]; 4],
            pub clip_to_camera_view: [[f32; 4]; 4],
            pub clip_to_lens_clip: [[f32; 4]; 4],
            pub clip_to_prev_clip: [[f32; 4]; 4],
            pub prev_clip_to_clip: [[f32; 4]; 4],
            pub jitter_offset: [f32; 2],
            pub mvec_scale: [f32; 2],
            pub camera_pinhole_offset: [f32; 2],
            pub camera_pos: [f32; 3],
            pub camera_up: [f32; 3],
            pub camera_right: [f32; 3],
            pub camera_fwd: [f32; 3],
            pub camera_near: f32,
            pub camera_far: f32,
            pub camera_fov: f32,
            pub camera_aspect_ratio: f32,
            pub motion_vectors_invalid_value: f32,
            pub depth_inverted: u8,
            pub camera_motion_included: u8,
            pub motion_vectors_3d: u8,
            pub reset: u8,
            pub orthographic_projection: u8,
            pub motion_vectors_dilated: u8,
            pub motion_vectors_jittered: u8,
            pub min_relative_linear_depth_object_separation: f32,
        }
        impl Constants {
            pub fn new() -> Constants {
                let mut c: Constants = unsafe { std::mem::zeroed() };
                c.base = Base::new(G_CONSTANTS, 2);
                c.min_relative_linear_depth_object_separation = 40.0;
                c
            }
        }

        #[repr(C)]
        pub struct ApiError {
            pub hres: i32,
        }

        #[repr(C)]
        pub struct DlssgOptions {
            pub base: Base,
            pub mode: u32,
            pub num_frames_to_generate: u32,
            pub flags: u32,
            pub dynamic_res_width: u32,
            pub dynamic_res_height: u32,
            pub num_back_buffers: u32,
            pub mvec_depth_width: u32,
            pub mvec_depth_height: u32,
            pub color_width: u32,
            pub color_height: u32,
            pub color_buffer_format: u32,
            pub mvec_buffer_format: u32,
            pub depth_buffer_format: u32,
            pub hud_less_buffer_format: u32,
            pub ui_buffer_format: u32,
            pub on_error_callback: Option<unsafe extern "C" fn(*const ApiError)>,
            pub reserved15: u8,
            pub queue_parallelism_mode: u32,
            pub enable_user_interface_recomposition: u8,
            pub dynamic_target_frame_rate: f32,
        }
        impl DlssgOptions {
            pub fn new() -> DlssgOptions {
                let mut o: DlssgOptions = unsafe { std::mem::zeroed() };
                o.base = Base::new(G_DLSSG_OPTIONS, 5);
                o.num_frames_to_generate = 1;
                o.reserved15 = INVALID;
                o
            }
        }

        #[repr(C)]
        pub struct DlssgState {
            pub base: Base,
            pub estimated_vram_usage_in_bytes: u64,
            pub status: u32,
            pub min_width_or_height: u32,
            pub num_frames_actually_presented: u32,
            pub num_frames_to_generate_max: u32,
            pub reserved4: u8,
            pub is_vsync_support_available: u8,
            pub inputs_processing_completion_fence: *mut c_void,
            pub last_present_inputs_processing_completion_fence_value: u64,
            pub is_dynamic_mfg_supported: u8,
        }
        impl DlssgState {
            pub fn new() -> DlssgState {
                let mut s: DlssgState = unsafe { std::mem::zeroed() };
                s.base = Base::new(G_DLSSG_STATE, 4);
                s
            }
        }

        #[repr(C)]
        pub struct ReflexOptions {
            pub base: Base,
            pub mode: u32,
            pub frame_limit_us: u32,
            pub use_markers_to_optimize: bool,
            pub virtual_key: u16,
            pub id_thread: u32,
        }
        impl ReflexOptions {
            pub fn new(mode: u32) -> ReflexOptions {
                let mut o: ReflexOptions = unsafe { std::mem::zeroed() };
                o.base = Base::new(G_REFLEX_OPTIONS, 1);
                o.mode = mode;
                o
            }
        }

        const _: () = assert!(std::mem::size_of::<Base>() == 32);
        const _: () = assert!(std::mem::size_of::<Preferences>() == 144);
        const _: () = assert!(std::mem::size_of::<Resource>() == 112);
        const _: () = assert!(std::mem::size_of::<ResourceTag>() == 64);
        const _: () = assert!(std::mem::size_of::<ViewportHandle>() == 40);
        const _: () = assert!(std::mem::size_of::<FeatureRequirements>() == 184);
        const _: () = assert!(std::mem::size_of::<Constants>() == 456);
        const _: () = assert!(std::mem::size_of::<DlssgOptions>() == 120);
        const _: () = assert!(std::mem::size_of::<DlssgState>() == 88);
        const _: () = assert!(std::mem::size_of::<ReflexOptions>() == 48);

        // References in the C++ signatures are pointers at the ABI level; a frame token is
        // only ever passed back, so it stays an opaque pointer.
        pub type Init = unsafe extern "C" fn(*const Preferences, u64) -> i32;
        pub type Shutdown = unsafe extern "C" fn() -> i32;
        pub type IsFeatureSupported = unsafe extern "C" fn(u32, *const AdapterInfo) -> i32;
        pub type GetFeatureRequirements = unsafe extern "C" fn(u32, *mut FeatureRequirements) -> i32;
        pub type SetD3DDevice = unsafe extern "C" fn(*mut c_void) -> i32;
        pub type GetNativeInterface = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
        pub type GetFeatureFunction = unsafe extern "C" fn(u32, *const c_char, *mut *mut c_void) -> i32;
        pub type GetNewFrameToken = unsafe extern "C" fn(*mut *mut c_void, *const u32) -> i32;
        pub type SetConstants = unsafe extern "C" fn(*const Constants, *const c_void, *const ViewportHandle) -> i32;
        pub type SetTagForFrame =
            unsafe extern "C" fn(*const c_void, *const ViewportHandle, *const ResourceTag, u32, *mut c_void) -> i32;
        pub type DlssgGetState =
            unsafe extern "C" fn(*const ViewportHandle, *mut DlssgState, *const DlssgOptions) -> i32;
        pub type DlssgSetOptions = unsafe extern "C" fn(*const ViewportHandle, *const DlssgOptions) -> i32;
        pub type ReflexSetOptions = unsafe extern "C" fn(*const ReflexOptions) -> i32;
        pub type ReflexSleep = unsafe extern "C" fn(*const c_void) -> i32;
        pub type PclSetMarker = unsafe extern "C" fn(u32, *const c_void) -> i32;

        pub fn result_name(r: i32) -> &'static str {
            const NAMES: [&str; 40] = [
                "eOk",
                "eErrorIO",
                "eErrorDriverOutOfDate",
                "eErrorOSOutOfDate",
                "eErrorOSDisabledHWS",
                "eErrorDeviceNotCreated",
                "eErrorNoSupportedAdapterFound",
                "eErrorAdapterNotSupported",
                "eErrorNoPlugins",
                "eErrorVulkanAPI",
                "eErrorDXGIAPI",
                "eErrorD3DAPI",
                "eErrorNRDAPI",
                "eErrorNVAPI",
                "eErrorReflexAPI",
                "eErrorNGXFailed",
                "eErrorJSONParsing",
                "eErrorMissingProxy",
                "eErrorMissingResourceState",
                "eErrorInvalidIntegration",
                "eErrorMissingInputParameter",
                "eErrorNotInitialized",
                "eErrorComputeFailed",
                "eErrorInitNotCalled",
                "eErrorExceptionHandler",
                "eErrorInvalidParameter",
                "eErrorMissingConstants",
                "eErrorDuplicatedConstants",
                "eErrorMissingOrInvalidAPI",
                "eErrorCommonConstantsMissing",
                "eErrorUnsupportedInterface",
                "eErrorFeatureMissing",
                "eErrorFeatureNotSupported",
                "eErrorFeatureMissingHooks",
                "eErrorFeatureFailedToLoad",
                "eErrorFeatureWrongPriority",
                "eErrorFeatureMissingDependency",
                "eErrorFeatureManagerInvalidState",
                "eErrorInvalidState",
                "eWarnOutOfVRAM",
            ];
            NAMES.get(r as usize).copied().unwrap_or("?")
        }

        pub fn status_names(s: u32) -> String {
            if s == 0 {
                return "eOk".into();
            }
            let bits = [
                (1, "eFailResolutionTooLow"),
                (2, "eFailReflexNotDetectedAtRuntime"),
                (4, "eFailHDRFormatNotSupported"),
                (8, "eFailCommonConstantsInvalid"),
                (16, "eFailGetCurrentBackBufferIndexNotCalled"),
                (32, "eReserved5"),
            ];
            bits.iter().filter(|(b, _)| s & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join("|")
        }
    }

    // ------------------------------------------------------------------------------------
    // Report: every line goes to the console at once and to a file at the end, so a hang or
    // crash inside a vendor library still leaves the findings so far.
    // ------------------------------------------------------------------------------------

    struct Report {
        lines: Vec<String>,
    }
    impl Report {
        fn say(&mut self, s: impl Into<String>) {
            let s = s.into();
            println!("{s}");
            self.lines.push(s);
        }
    }

    static QUIT: AtomicBool = AtomicBool::new(false);
    static SL_LOG: Mutex<Vec<(u32, String)>> = Mutex::new(Vec::new());
    static SL_API_ERRORS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    static FFX_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

    unsafe extern "C" fn sl_log_callback(ty: u32, msg: *const c_char) {
        if msg.is_null() {
            return;
        }
        let s = CStr::from_ptr(msg).to_string_lossy().trim_end().to_string();
        if let Ok(mut l) = SL_LOG.lock() {
            if l.len() < 20_000 {
                l.push((ty, s));
            }
        }
    }

    unsafe extern "C" fn sl_api_error_callback(e: *const sl::ApiError) {
        if let (Ok(mut l), false) = (SL_API_ERRORS.lock(), e.is_null()) {
            if l.len() < 1000 {
                l.push((*e).hres);
            }
        }
    }

    unsafe extern "C" fn ffx_message_callback(ty: u32, msg: *const u16) {
        if msg.is_null() {
            return;
        }
        let mut n = 0;
        while *msg.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(msg, n));
        if let Ok(mut l) = FFX_LOG.lock() {
            if l.len() < 2000 {
                l.push(format!("{} {s}", if ty == 0 { "error" } else { "warning" }));
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Window
    // ------------------------------------------------------------------------------------

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_CLOSE {
            QUIT.store(true, Ordering::Relaxed);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// Where the swapchain's window sits: on its own, as a child of a host window, or as a
    /// popup owned by one. The last two are how a picture surface would sit over the app's
    /// existing player window.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Host {
        Top,
        Child,
        Popup,
    }

    /// Returns the top-level window and the window the swapchain is created for.
    unsafe fn create_window(w: u32, h: u32, title: &str, focus: bool, host: Host) -> Result<(HWND, HWND)> {
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = wide("XMouseFgProbe");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A plain captioned window that cannot be resized: the probe keeps one size.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
        let mut rc = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        AdjustWindowRectEx(&mut rc, style, false, WINDOW_EX_STYLE(0))?;
        let titlew = wide(title);
        let top = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(titlew.as_ptr()),
            style,
            80,
            60,
            rc.right - rc.left,
            rc.bottom - rc.top,
            None,
            None,
            Some(hinst),
            None,
        )?;
        if focus {
            // DLSS-G only generates while its window is in the foreground. Windows refuses a
            // background process the foreground unless it shares input state with the
            // thread that has it, hence the brief attach.
            let _ = ShowWindow(top, SW_SHOWNORMAL);
            let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let me = GetCurrentThreadId();
            let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
            let _ = BringWindowToTop(top);
            let _ = SetForegroundWindow(top);
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
        } else {
            // Shown without taking the focus away from whatever the user is doing.
            let _ = ShowWindow(top, SW_SHOWNOACTIVATE);
        }
        let surface = match host {
            Host::Top => top,
            Host::Child => CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                w as i32,
                h as i32,
                Some(top),
                None,
                Some(hinst),
                None,
            )?,
            Host::Popup => {
                let mut pt = POINT::default();
                let _ = ClientToScreen(top, &mut pt);
                let s = CreateWindowExW(
                    WS_EX_NOACTIVATE,
                    PCWSTR(class.as_ptr()),
                    PCWSTR::null(),
                    WS_POPUP,
                    pt.x,
                    pt.y,
                    w as i32,
                    h as i32,
                    Some(top),
                    None,
                    Some(hinst),
                    None,
                )?;
                let _ = ShowWindow(s, SW_SHOWNOACTIVATE);
                s
            }
        };
        Ok((top, surface))
    }
    unsafe fn pump() {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    /// Sleeps coarsely, then spins: the source frames must leave on an even clock, because
    /// both frame generators pace their output from the spacing of our presents.
    unsafe fn wait_until(deadline: Instant) {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            if deadline - now > Duration::from_micros(2500) {
                pump();
                std::thread::sleep(Duration::from_millis(1));
            } else {
                std::hint::spin_loop();
            }
        }
    }

    /// The window's client area as DWM shows it, i.e. whatever frame is on screen now.
    unsafe fn capture_window(hwnd: HWND, w: usize, h: usize) -> Option<Img> {
        let wdc = GetDC(Some(hwnd));
        let mdc = CreateCompatibleDC(Some(wdc));
        let mut bmi: BITMAPINFO = zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        let mut bits: *mut c_void = null_mut();
        let mut out = None;
        if let Ok(dib) = CreateDIBSection(Some(wdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(mdc, dib.into());
            // 1 = client area only, 2 = include content DWM composes from a swapchain.
            let mut ok = PrintWindow(hwnd, mdc, 3).as_bool();
            let _ = GdiFlush();
            let px = std::slice::from_raw_parts(bits as *const u8, w * h * 4);
            if !ok || px.iter().step_by(97).all(|&b| b == 0) {
                // Fall back to copying the screen where the window is.
                let mut pt = POINT::default();
                let _ = ClientToScreen(hwnd, &mut pt);
                let sdc = GetDC(None);
                ok = BitBlt(mdc, 0, 0, w as i32, h as i32, Some(sdc), pt.x, pt.y, SRCCOPY).is_ok();
                ReleaseDC(None, sdc);
                let _ = GdiFlush();
            }
            if ok {
                let mut rgb = vec![0u8; w * h * 3];
                for i in 0..w * h {
                    rgb[i * 3] = px[i * 4 + 2];
                    rgb[i * 3 + 1] = px[i * 4 + 1];
                    rgb[i * 3 + 2] = px[i * 4];
                }
                out = Some(Img { w, h, rgb });
            }
            SelectObject(mdc, old);
            let _ = DeleteObject(dib.into());
        }
        let _ = DeleteDC(mdc);
        ReleaseDC(Some(hwnd), wdc);
        out
    }

    /// Hardware-accelerated GPU scheduling as the kernel reports it for this adapter
    /// (WDDM 2.7 capability bits: supported, enabled, enabled by default).
    unsafe fn hags_state(luid: LUID) -> String {
        let mut open = KmtOpen { luid, handle: 0 };
        if D3DKMTOpenAdapterFromLuid(&mut open) != 0 {
            return "unknown (adapter could not be opened)".into();
        }
        let mut caps = 0u32;
        let mut q = KmtQuery { handle: open.handle, ty: 70, data: &mut caps as *mut u32 as *mut c_void, size: 4 };
        let st = D3DKMTQueryAdapterInfo(&mut q);
        D3DKMTCloseAdapter(&open.handle);
        if st != 0 {
            return format!("unknown (query failed, status {st:#x})");
        }
        format!(
            "{} (supported {}, enabled {}, enabled by default {})",
            if caps & 2 != 0 { "ON" } else { "OFF" },
            caps & 1,
            (caps >> 1) & 1,
            (caps >> 2) & 1
        )
    }

    // ------------------------------------------------------------------------------------
    // D3D12 plumbing
    // ------------------------------------------------------------------------------------

    const HLSL: &str = r#"
cbuffer C : register(b0) {
    int2 dispSize; int2 gridSize;
    int2 qSize; int coarseRange; int pad0;
    float2 panV; float2 objV;
    float2 objC; float objR; float pad1;
    float4 hudRect;
    float4 hud2Rect;
};
Texture2D<float4> t0 : register(t0);
Texture2D<float4> t1 : register(t1);
Texture2D<float4> t2 : register(t2);
Texture2D<float4> t3 : register(t3);
RWTexture2D<float4> u0 : register(u0);
SamplerState sLin : register(s0);

float luma(float3 c) { return dot(c, float3(0.299, 0.587, 0.114)); }

struct VO { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VO vsMain(uint id : SV_VertexID) {
    VO o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
// The "video frame" scaled to the window, as the app's resample pass would.
float4 psBlit(VO i) : SV_Target { return float4(t0.SampleLevel(sLin, i.uv, 0).rgb, 1); }

// Quarter-resolution luma for the coarse search.
[numthreads(8, 8, 1)]
void csLuma(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)qSize.x || id.y >= (uint)qSize.y) return;
    float s = 0;
    for (int y = 0; y < 4; y++)
        for (int x = 0; x < 4; x++)
            s += luma(t0.Load(int3(min(int2(id.xy) * 4 + int2(x, y), dispSize - 1), 0)).rgb);
    u0[id.xy] = float4(s / 16.0, 0, 0, 0);
}

// One vector per 8x8 block: exhaustive search at quarter resolution over a 24x24 pixel
// window. A small cost on length keeps flat areas at rest.
[numthreads(8, 8, 1)]
void csCoarse(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)gridSize.x || id.y >= (uint)gridSize.y) return;
    int2 cq = int2(id.xy) * 2 + 1;
    float cur[36];
    int i, j;
    for (j = 0; j < 6; j++)
        for (i = 0; i < 6; i++)
            cur[j * 6 + i] = t0.Load(int3(clamp(cq + int2(i - 3, j - 3), 0, qSize - 1), 0)).r;
    float best = 1e9;
    int2 bd = int2(0, 0);
    [loop] for (int dy = -coarseRange; dy <= coarseRange; dy++) {
        [loop] for (int dx = -coarseRange; dx <= coarseRange; dx++) {
            float sad = 0.004 * (abs(dx) + abs(dy));
            for (j = 0; j < 6; j++)
                for (i = 0; i < 6; i++)
                    sad += abs(cur[j * 6 + i] - t1.Load(int3(clamp(cq + int2(i - 3 + dx, j - 3 + dy), 0, qSize - 1), 0)).r);
            if (sad < best) { best = sad; bd = int2(dx, dy); }
        }
    }
    u0[id.xy] = float4(bd * 4.0, 0, 0);
}

// Refines each block vector to whole pixels at full resolution.
[numthreads(8, 8, 1)]
void csRefine(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)gridSize.x || id.y >= (uint)gridSize.y) return;
    int2 c = int2(id.xy) * 8 + 4;
    int2 base = int2(round(t2.Load(int3(id.xy, 0)).xy));
    float cur[36];
    int i, j;
    for (j = 0; j < 6; j++)
        for (i = 0; i < 6; i++)
            cur[j * 6 + i] = luma(t0.Load(int3(clamp(c + int2(i - 3, j - 3) * 2, 0, dispSize - 1), 0)).rgb);
    float best = 1e9;
    int2 bd = base;
    [loop] for (int dy = -3; dy <= 3; dy++) {
        [loop] for (int dx = -3; dx <= 3; dx++) {
            int2 d = base + int2(dx, dy);
            float sad = 0.002 * (abs(d.x) + abs(d.y));
            for (j = 0; j < 6; j++)
                for (i = 0; i < 6; i++)
                    sad += abs(cur[j * 6 + i] - luma(t1.Load(int3(clamp(c + int2(i - 3, j - 3) * 2 + d, 0, dispSize - 1), 0)).rgb));
            if (sad < best) { best = sad; bd = d; }
        }
    }
    u0[id.xy] = float4(bd, 0, 0);
}

float med9(float v[9]) {
    for (int a = 0; a < 5; a++)
        for (int b = a + 1; b < 9; b++)
            if (v[b] < v[a]) { float t = v[a]; v[a] = v[b]; v[b] = t; }
    return v[4];
}

// Per-component 3x3 median: removes lone wrong blocks.
[numthreads(8, 8, 1)]
void csMedian(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)gridSize.x || id.y >= (uint)gridSize.y) return;
    float xs[9]; float ys[9];
    int k = 0;
    for (int j = -1; j <= 1; j++)
        for (int i = -1; i <= 1; i++) {
            float2 v = t0.Load(int3(clamp(int2(id.xy) + int2(i, j), 0, gridSize - 1), 0)).xy;
            xs[k] = v.x; ys[k] = v.y; k++;
        }
    u0[id.xy] = float4(med9(xs), med9(ys), 0, 0);
}

// Dense field: every pixel takes whichever of the four surrounding block vectors explains
// it best, so motion edges follow the picture instead of the block grid.
[numthreads(8, 8, 1)]
void csUpsample(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)dispSize.x || id.y >= (uint)dispSize.y) return;
    int2 p = int2(id.xy);
    float2 bf = (float2(p) + 0.5) / 8.0 - 0.5;
    int2 b0 = int2(floor(bf));
    float best = 1e9;
    float2 bv = float2(0, 0);
    for (int k = 0; k < 4; k++) {
        int2 b = clamp(b0 + int2(k & 1, k >> 1), 0, gridSize - 1);
        float2 v = t2.Load(int3(b, 0)).xy;
        int2 d = int2(round(v));
        float2 dist = abs(bf - float2(b));
        float e = 0.01 * (dist.x + dist.y);
        for (int j = -1; j <= 1; j++)
            for (int i = -1; i <= 1; i++) {
                int2 q = clamp(p + int2(i, j) * 2, 0, dispSize - 1);
                e += abs(luma(t0.Load(int3(q, 0)).rgb) - luma(t1.Load(int3(clamp(q + d, 0, dispSize - 1), 0)).rgb));
            }
        if (e < best) { best = e; bv = v; }
    }
    u0[p] = float4(bv, 0, 0);
}

// Comparison mode only: the vectors the clip was built from.
[numthreads(8, 8, 1)]
void csTruth(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)dispSize.x || id.y >= (uint)dispSize.y) return;
    float2 p = float2(id.xy) + 0.5;
    float2 v = panV;
    if (length(p - objC) <= objR) v = objV;
    if (all(p >= hudRect.xy) && all(p < hudRect.zw)) v = float2(0, 0);
    if (all(p >= hud2Rect.xy) && all(p < hud2Rect.zw)) v = float2(0, 0);
    u0[id.xy] = float4(v, 0, 0);
}
"#;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CsConsts {
        disp: [i32; 2],
        grid: [i32; 2],
        q: [i32; 2],
        coarse_range: i32,
        pad0: i32,
        pan_v: [f32; 2],
        obj_v: [f32; 2],
        obj_c: [f32; 2],
        obj_r: f32,
        pad1: f32,
        hud: [f32; 4],
        hud2: [f32; 4],
    }

    struct Tex {
        res: ID3D12Resource,
        state: D3D12_RESOURCE_STATES,
    }

    /// A COM pointer lent to a D3D12 struct field without touching its reference count.
    unsafe fn lend<T: Interface>(x: &T) -> ManuallyDrop<Option<T>> {
        transmute_copy(x)
    }

    unsafe fn barrier(
        res: ManuallyDrop<Option<ID3D12Resource>>,
        before: D3D12_RESOURCE_STATES,
        after: D3D12_RESOURCE_STATES,
    ) -> D3D12_RESOURCE_BARRIER {
        D3D12_RESOURCE_BARRIER {
            Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: D3D12_RESOURCE_BARRIER_0 {
                Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: res,
                    Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        }
    }

    unsafe fn to_state(list: &ID3D12GraphicsCommandList, t: &mut Tex, after: D3D12_RESOURCE_STATES) {
        if t.state != after {
            list.ResourceBarrier(&[barrier(lend(&t.res), t.state, after)]);
            t.state = after;
        }
    }

    unsafe fn make_tex(
        dev: &ID3D12Device,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
    ) -> Result<Tex> {
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: w as u64,
            Height: h,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Flags: flags,
            ..Default::default()
        };
        let mut res: Option<ID3D12Resource> = None;
        dev.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut res)?;
        Ok(Tex { res: res.ok_or_else(|| anyhow!("no texture"))?, state })
    }

    unsafe fn make_buffer(dev: &ID3D12Device, size: u64, heap_type: D3D12_HEAP_TYPE) -> Result<ID3D12Resource> {
        let heap = D3D12_HEAP_PROPERTIES { Type: heap_type, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let state = if heap_type == D3D12_HEAP_TYPE_UPLOAD {
            D3D12_RESOURCE_STATE_GENERIC_READ
        } else {
            D3D12_RESOURCE_STATE_COPY_DEST
        };
        let mut res: Option<ID3D12Resource> = None;
        dev.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut res)?;
        res.ok_or_else(|| anyhow!("no buffer"))
    }

    unsafe fn compile(entry: &str, target: &str) -> Result<ID3DBlob> {
        let entry_c = format!("{entry}\0");
        let target_c = format!("{target}\0");
        let mut code: Option<ID3DBlob> = None;
        let mut errors: Option<ID3DBlob> = None;
        let r = D3DCompile(
            HLSL.as_ptr() as *const c_void,
            HLSL.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr()),
            PCSTR(target_c.as_ptr()),
            1 << 15, // optimisation level 3
            0,
            &mut code,
            Some(&mut errors),
        );
        if let Err(e) = r {
            let msg = errors
                .map(|b| {
                    String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()))
                        .to_string()
                })
                .unwrap_or_default();
            bail!("HLSL {entry}: {e} {msg}");
        }
        code.ok_or_else(|| anyhow!("no bytecode for {entry}"))
    }

    fn bytecode(b: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
        unsafe { D3D12_SHADER_BYTECODE { pShaderBytecode: b.GetBufferPointer(), BytecodeLength: b.GetBufferSize() } }
    }

    // Descriptor slots in the one shader-visible heap.
    const SRV_SRC: u32 = 0;
    const SRV_FRAME: u32 = 1; // +0, +1
    const SRV_LUMA: u32 = 3; // +0, +1
    const SRV_MVC: u32 = 5;
    const SRV_MVB: u32 = 6;
    const SRV_MVM: u32 = 7;
    const UAV_LUMA: u32 = 9; // +0, +1
    const UAV_MVC: u32 = 11;
    const UAV_MVB: u32 = 12;
    const UAV_MVM: u32 = 13;
    const UAV_MV: u32 = 14;
    const HEAP_SLOTS: u32 = 16;

    struct Gfx {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        alloc: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        fence_value: u64,
        fence_event: HANDLE,
        rtv_heap: ID3D12DescriptorHeap,
        rtv_inc: usize,
        heap: ID3D12DescriptorHeap,
        heap_inc: u32,
        root: ID3D12RootSignature,
        pso_blit: ID3D12PipelineState,
        cs_luma: ID3D12PipelineState,
        cs_coarse: ID3D12PipelineState,
        cs_refine: ID3D12PipelineState,
        cs_median: ID3D12PipelineState,
        cs_upsample: ID3D12PipelineState,
        cs_truth: ID3D12PipelineState,
        src: Tex,
        upload: ID3D12Resource,
        upload_ptr: *mut u8,
        src_footprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
        frame: [Tex; 2],
        luma: [Tex; 2],
        mvc: Tex,
        mvb: Tex,
        mvm: Tex,
        mv: Tex,
        depth: Tex,
        w: u32,
        h: u32,
        gw: u32,
        gh: u32,
        qw: u32,
        qh: u32,
    }

    impl Gfx {
        unsafe fn new(device: &ID3D12Device, queue: &ID3D12CommandQueue, w: u32, h: u32, depth_value: f32) -> Result<Gfx> {
            let alloc: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc, None::<&ID3D12PipelineState>)?;
            list.Close()?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, PCWSTR::null())?;

            let rtv_heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: 3,
                ..Default::default()
            })?;
            let rtv_inc = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
            let heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: HEAP_SLOTS,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                ..Default::default()
            })?;
            let heap_inc = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);

            // One root signature for everything: constants, four one-texture tables, one
            // output table, a linear sampler.
            let ranges: Vec<D3D12_DESCRIPTOR_RANGE> = (0..5)
                .map(|i| D3D12_DESCRIPTOR_RANGE {
                    RangeType: if i < 4 { D3D12_DESCRIPTOR_RANGE_TYPE_SRV } else { D3D12_DESCRIPTOR_RANGE_TYPE_UAV },
                    NumDescriptors: 1,
                    BaseShaderRegister: if i < 4 { i } else { 0 },
                    RegisterSpace: 0,
                    OffsetInDescriptorsFromTableStart: 0,
                })
                .collect();
            let mut params = vec![D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: 0,
                        RegisterSpace: 0,
                        Num32BitValues: (size_of::<CsConsts>() / 4) as u32,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            }];
            for r in &ranges {
                params.push(D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 1, pDescriptorRanges: r },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                });
            }
            let sampler = D3D12_STATIC_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                MaxLOD: f32::MAX,
                ShaderRegister: 0,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                ..Default::default()
            };
            let root_desc = D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
                NumStaticSamplers: 1,
                pStaticSamplers: &sampler,
                Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
            };
            let mut blob: Option<ID3DBlob> = None;
            let mut err: Option<ID3DBlob> = None;
            D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, Some(&mut err))
                .context("root signature")?;
            let blob = blob.ok_or_else(|| anyhow!("no root signature blob"))?;
            let root: ID3D12RootSignature = device.CreateRootSignature(
                0,
                std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()),
            )?;

            let vs = compile("vsMain", "vs_5_0")?;
            let ps = compile("psBlit", "ps_5_0")?;
            let mut pso = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: lend(&root),
                VS: bytecode(&vs),
                PS: bytecode(&ps),
                SampleMask: u32::MAX,
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            pso.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
            pso.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
            pso.BlendState.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
            pso.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
            let pso_blit: ID3D12PipelineState = device.CreateGraphicsPipelineState(&pso).context("blit pipeline")?;
            let compute = |entry: &str| -> Result<ID3D12PipelineState> {
                let cs = compile(entry, "cs_5_0")?;
                let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: lend(&root),
                    CS: bytecode(&cs),
                    ..Default::default()
                };
                device.CreateComputePipelineState(&desc).with_context(|| format!("{entry} pipeline"))
            };
            let (cs_luma, cs_coarse, cs_refine) = (compute("csLuma")?, compute("csCoarse")?, compute("csRefine")?);
            let (cs_median, cs_upsample, cs_truth) = (compute("csMedian")?, compute("csUpsample")?, compute("csTruth")?);

            let (gw, gh) = (w.div_ceil(8), h.div_ceil(8));
            let (qw, qh) = (w.div_ceil(4), h.div_ceil(4));
            let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
            let rgba = DXGI_FORMAT_R8G8B8A8_UNORM;
            let uav = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
            let rt = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
            let src = make_tex(device, SRC_W as u32, SRC_H as u32, rgba, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST)?;
            let frame = [make_tex(device, w, h, rgba, rt, read)?, make_tex(device, w, h, rgba, rt, read)?];
            let luma = [
                make_tex(device, qw, qh, DXGI_FORMAT_R16_FLOAT, uav, read)?,
                make_tex(device, qw, qh, DXGI_FORMAT_R16_FLOAT, uav, read)?,
            ];
            let mvf = DXGI_FORMAT_R16G16_FLOAT;
            let mvc = make_tex(device, gw, gh, mvf, uav, read)?;
            let mvb = make_tex(device, gw, gh, mvf, uav, read)?;
            let mvm = make_tex(device, gw, gh, mvf, uav, read)?;
            let mv = make_tex(device, w, h, mvf, uav, read)?;
            let depth = make_tex(device, w, h, DXGI_FORMAT_R32_FLOAT, rt, D3D12_RESOURCE_STATE_RENDER_TARGET)?;

            let mut src_footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let mut total = 0u64;
            device.GetCopyableFootprints(&src.res.GetDesc(), 0, 1, 0, Some(&mut src_footprint), None, None, Some(&mut total));
            let upload = make_buffer(device, total, D3D12_HEAP_TYPE_UPLOAD)?;
            let mut upload_ptr: *mut c_void = null_mut();
            upload.Map(0, None, Some(&mut upload_ptr))?;

            let mut g = Gfx {
                device: device.clone(),
                queue: queue.clone(),
                alloc,
                list,
                fence,
                fence_value: 0,
                fence_event,
                rtv_heap,
                rtv_inc,
                heap,
                heap_inc,
                root,
                pso_blit,
                cs_luma,
                cs_coarse,
                cs_refine,
                cs_median,
                cs_upsample,
                cs_truth,
                src,
                upload,
                upload_ptr: upload_ptr as *mut u8,
                src_footprint,
                frame,
                luma,
                mvc,
                mvb,
                mvm,
                mv,
                depth,
                w,
                h,
                gw,
                gh,
                qw,
                qh,
            };

            for i in 0..2 {
                device.CreateRenderTargetView(&g.frame[i].res, None, g.rtv(i));
                device.CreateShaderResourceView(&g.frame[i].res, None, g.cpu(SRV_FRAME + i as u32));
                device.CreateShaderResourceView(&g.luma[i].res, None, g.cpu(SRV_LUMA + i as u32));
                device.CreateUnorderedAccessView(&g.luma[i].res, None::<&ID3D12Resource>, None, g.cpu(UAV_LUMA + i as u32));
            }
            device.CreateRenderTargetView(&g.depth.res, None, g.rtv(2));
            device.CreateShaderResourceView(&g.src.res, None, g.cpu(SRV_SRC));
            for (t, srv, uav) in [(&g.mvc, SRV_MVC, UAV_MVC), (&g.mvb, SRV_MVB, UAV_MVB), (&g.mvm, SRV_MVM, UAV_MVM)] {
                device.CreateShaderResourceView(&t.res, None, g.cpu(srv));
                device.CreateUnorderedAccessView(&t.res, None::<&ID3D12Resource>, None, g.cpu(uav));
            }
            device.CreateUnorderedAccessView(&g.mv.res, None::<&ID3D12Resource>, None, g.cpu(UAV_MV));

            // Depth is one constant plane for the whole run: a video has none.
            g.begin()?;
            g.list.ClearRenderTargetView(g.rtv(2), &[depth_value, 0.0, 0.0, 0.0], None);
            to_state(&g.list, &mut g.depth, read);
            g.submit()?;
            g.wait_idle()?;
            Ok(g)
        }

        unsafe fn rtv(&self, i: usize) -> D3D12_CPU_DESCRIPTOR_HANDLE {
            D3D12_CPU_DESCRIPTOR_HANDLE { ptr: self.rtv_heap.GetCPUDescriptorHandleForHeapStart().ptr + i * self.rtv_inc }
        }
        unsafe fn cpu(&self, slot: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
            D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: self.heap.GetCPUDescriptorHandleForHeapStart().ptr + (slot * self.heap_inc) as usize,
            }
        }
        unsafe fn gpu(&self, slot: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
            D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: self.heap.GetGPUDescriptorHandleForHeapStart().ptr + (slot * self.heap_inc) as u64,
            }
        }

        unsafe fn begin(&mut self) -> Result<()> {
            self.alloc.Reset()?;
            self.list.Reset(&self.alloc, None::<&ID3D12PipelineState>)?;
            Ok(())
        }

        unsafe fn submit(&mut self) -> Result<()> {
            self.list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast::<ID3D12CommandList>()?)]);
            Ok(())
        }

        unsafe fn wait_idle(&mut self) -> Result<()> {
            self.fence_value += 1;
            self.queue.Signal(&self.fence, self.fence_value)?;
            if self.fence.GetCompletedValue() < self.fence_value {
                self.fence.SetEventOnCompletion(self.fence_value, self.fence_event)?;
                if WaitForSingleObject(self.fence_event, 10_000) != WAIT_OBJECT_0 {
                    bail!("GPU did not finish a frame within 10 s (device removed: {:?})", self.device.GetDeviceRemovedReason());
                }
            }
            Ok(())
        }

        /// Records one source frame: upload, scale to the window size, estimate motion
        /// against the previous frame, copy to the backbuffer. Leaves the list open so a
        /// frame generator can add its own preparation before `submit`.
        unsafe fn record_frame(
            &mut self,
            rgba: &[u8],
            cur: usize,
            consts: &CsConsts,
            mv_mode: Mv,
            backbuffer: &ID3D12Resource,
        ) -> Result<()> {
            let fp = self.src_footprint;
            let pitch = fp.Footprint.RowPitch as usize;
            for y in 0..SRC_H {
                std::ptr::copy_nonoverlapping(
                    rgba.as_ptr().add(y * SRC_W * 4),
                    self.upload_ptr.add(fp.Offset as usize + y * pitch),
                    SRC_W * 4,
                );
            }
            self.begin()?;
            let list = self.list.clone();
            let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
            let uav = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
            list.SetDescriptorHeaps(&[Some(self.heap.clone())]);

            to_state(&list, &mut self.src, D3D12_RESOURCE_STATE_COPY_DEST);
            let dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: lend(&self.src.res),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: lend(&self.upload),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp },
            };
            list.CopyTextureRegion(&dst, 0, 0, 0, &from, None);
            to_state(&list, &mut self.src, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE);

            to_state(&list, &mut self.frame[cur], D3D12_RESOURCE_STATE_RENDER_TARGET);
            let rtv = self.rtv(cur);
            list.OMSetRenderTargets(1, Some(&rtv), false, None);
            list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: self.w as f32,
                Height: self.h as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            list.RSSetScissorRects(&[RECT { left: 0, top: 0, right: self.w as i32, bottom: self.h as i32 }]);
            list.SetGraphicsRootSignature(&self.root);
            list.SetPipelineState(&self.pso_blit);
            list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            list.SetGraphicsRootDescriptorTable(1, self.gpu(SRV_SRC));
            list.DrawInstanced(3, 1, 0, 0);
            to_state(&list, &mut self.frame[cur], read);

            list.SetComputeRootSignature(&self.root);
            list.SetComputeRoot32BitConstants(0, (size_of::<CsConsts>() / 4) as u32, consts as *const _ as *const c_void, 0);
            let prev = 1 - cur;
            let groups = |n: u32| n.div_ceil(8);
            match mv_mode {
                Mv::Search => {
                    to_state(&list, &mut self.luma[cur], uav);
                    list.SetPipelineState(&self.cs_luma);
                    list.SetComputeRootDescriptorTable(1, self.gpu(SRV_FRAME + cur as u32));
                    list.SetComputeRootDescriptorTable(5, self.gpu(UAV_LUMA + cur as u32));
                    list.Dispatch(groups(self.qw), groups(self.qh), 1);
                    to_state(&list, &mut self.luma[cur], read);

                    to_state(&list, &mut self.mvc, uav);
                    list.SetPipelineState(&self.cs_coarse);
                    list.SetComputeRootDescriptorTable(1, self.gpu(SRV_LUMA + cur as u32));
                    list.SetComputeRootDescriptorTable(2, self.gpu(SRV_LUMA + prev as u32));
                    list.SetComputeRootDescriptorTable(5, self.gpu(UAV_MVC));
                    list.Dispatch(groups(self.gw), groups(self.gh), 1);
                    to_state(&list, &mut self.mvc, read);

                    to_state(&list, &mut self.mvb, uav);
                    list.SetPipelineState(&self.cs_refine);
                    list.SetComputeRootDescriptorTable(1, self.gpu(SRV_FRAME + cur as u32));
                    list.SetComputeRootDescriptorTable(2, self.gpu(SRV_FRAME + prev as u32));
                    list.SetComputeRootDescriptorTable(3, self.gpu(SRV_MVC));
                    list.SetComputeRootDescriptorTable(5, self.gpu(UAV_MVB));
                    list.Dispatch(groups(self.gw), groups(self.gh), 1);
                    to_state(&list, &mut self.mvb, read);

                    to_state(&list, &mut self.mvm, uav);
                    list.SetPipelineState(&self.cs_median);
                    list.SetComputeRootDescriptorTable(1, self.gpu(SRV_MVB));
                    list.SetComputeRootDescriptorTable(5, self.gpu(UAV_MVM));
                    list.Dispatch(groups(self.gw), groups(self.gh), 1);
                    to_state(&list, &mut self.mvm, read);

                    to_state(&list, &mut self.mv, uav);
                    list.SetPipelineState(&self.cs_upsample);
                    list.SetComputeRootDescriptorTable(1, self.gpu(SRV_FRAME + cur as u32));
                    list.SetComputeRootDescriptorTable(2, self.gpu(SRV_FRAME + prev as u32));
                    list.SetComputeRootDescriptorTable(3, self.gpu(SRV_MVM));
                    list.SetComputeRootDescriptorTable(5, self.gpu(UAV_MV));
                    list.Dispatch(groups(self.w), groups(self.h), 1);
                    to_state(&list, &mut self.mv, read);
                }
                Mv::Truth | Mv::Zero => {
                    to_state(&list, &mut self.mv, uav);
                    list.SetPipelineState(&self.cs_truth);
                    list.SetComputeRootDescriptorTable(5, self.gpu(UAV_MV));
                    list.Dispatch(groups(self.w), groups(self.h), 1);
                    to_state(&list, &mut self.mv, read);
                }
            }

            // The backbuffer is the video frame itself: there is no separate UI layer.
            to_state(&list, &mut self.frame[cur], D3D12_RESOURCE_STATE_COPY_SOURCE);
            list.ResourceBarrier(&[barrier(lend(backbuffer), D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_COPY_DEST)]);
            list.CopyResource(backbuffer, &self.frame[cur].res);
            list.ResourceBarrier(&[barrier(lend(backbuffer), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_PRESENT)]);
            to_state(&list, &mut self.frame[cur], read);
            Ok(())
        }
    }

    // ------------------------------------------------------------------------------------
    // FSR frame generation
    // ------------------------------------------------------------------------------------

    struct Slot {
        buffer: ID3D12Resource,
        frame_id: u64,
        generated: bool,
        /// Set by the callback once the copy is recorded.
        filled: Option<(D3D12_PLACED_SUBRESOURCE_FOOTPRINT, DXGI_FORMAT)>,
    }

    #[derive(Default)]
    struct FsrCallbackState {
        /// (seconds since start, generated?, frame id) for every frame the swapchain shows.
        events: Vec<(f64, bool, u64)>,
        slots: Vec<Slot>,
        first_call: Option<String>,
        errors: Vec<String>,
    }

    /// What the two FSR callbacks need. Leaked on purpose: the swapchain's own threads may
    /// still call in while the probe is tearing down.
    struct FsrShared {
        dispatch: ffx::Dispatch,
        fg_context: *mut c_void,
        device: ID3D12Device,
        t0: Instant,
        state: Mutex<FsrCallbackState>,
    }

    fn ffx_state_to_d3d(s: u32) -> D3D12_RESOURCE_STATES {
        match s {
            2 => D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            4 => D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            8 => D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            12 => D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE | D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            16 => D3D12_RESOURCE_STATE_COPY_SOURCE,
            20 => D3D12_RESOURCE_STATE_GENERIC_READ,
            32 => D3D12_RESOURCE_STATE_COPY_DEST,
            256 => D3D12_RESOURCE_STATE_RENDER_TARGET,
            _ => D3D12_RESOURCE_STATE_COMMON, // common and present are the same state
        }
    }

    /// FSR asks us to run the interpolation it has scheduled: hand it straight back.
    unsafe extern "C" fn fsr_generate_callback(params: *mut ffx::Header, user: *mut c_void) -> u32 {
        let sh = &*(user as *const FsrShared);
        let mut ctx = sh.fg_context;
        (sh.dispatch)(&mut ctx, params)
    }

    /// Called for every frame on its way to the screen, generated or real. Having a
    /// callback makes us responsible for the final copy; in exchange we can count the
    /// frames and take a copy of the ones we want to look at.
    unsafe extern "C" fn fsr_present_callback(p: *mut ffx::PresentCallbackDesc, user: *mut c_void) -> u32 {
        let sh = &*(user as *const FsrShared);
        let p = &*p;
        let (src_raw, dst_raw) = (p.current_back_buffer.resource, p.output_swap_chain_buffer.resource);
        let (Some(list), Some(src), Some(dst)) = (
            ID3D12GraphicsCommandList::from_raw_borrowed(&p.command_list),
            ID3D12Resource::from_raw_borrowed(&src_raw),
            ID3D12Resource::from_raw_borrowed(&dst_raw),
        ) else {
            return 6;
        };
        let (sd, dd) = (src.GetDesc(), dst.GetDesc());
        let Ok(mut st) = sh.state.lock() else { return 1 };
        st.events.push((sh.t0.elapsed().as_secs_f64(), p.is_generated_frame, p.frame_id));
        if st.first_call.is_none() {
            st.first_call = Some(format!(
                "picture {}x{} format {} state {}, output {}x{} format {} state {}",
                sd.Width, sd.Height, sd.Format.0, p.current_back_buffer.state, dd.Width, dd.Height, dd.Format.0,
                p.output_swap_chain_buffer.state
            ));
        }
        let same = src_raw == dst_raw;
        let (s_before, d_before) =
            (ffx_state_to_d3d(p.current_back_buffer.state), ffx_state_to_d3d(p.output_swap_chain_buffer.state));
        let mut there = Vec::new();
        let mut back = Vec::new();
        if s_before != D3D12_RESOURCE_STATE_COPY_SOURCE {
            there.push(barrier(lend(src), s_before, D3D12_RESOURCE_STATE_COPY_SOURCE));
            back.push(barrier(lend(src), D3D12_RESOURCE_STATE_COPY_SOURCE, s_before));
        }
        if !same && d_before != D3D12_RESOURCE_STATE_COPY_DEST {
            there.push(barrier(lend(dst), d_before, D3D12_RESOURCE_STATE_COPY_DEST));
            back.push(barrier(lend(dst), D3D12_RESOURCE_STATE_COPY_DEST, d_before));
        }
        if !there.is_empty() {
            list.ResourceBarrier(&there);
        }
        if !same {
            if sd.Format == dd.Format && sd.Width == dd.Width && sd.Height == dd.Height {
                list.CopyResource(dst, src);
            } else if st.errors.len() < 8 {
                st.errors.push("picture and output differ in size or format; frame not copied".into());
            }
        }
        if let Some(i) = st.slots.iter().position(|s| s.filled.is_none() && s.frame_id == p.frame_id && s.generated == p.is_generated_frame) {
            let mut fp = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
            let mut total = 0u64;
            sh.device.GetCopyableFootprints(&sd, 0, 1, 0, Some(&mut fp), None, None, Some(&mut total));
            if total <= st.slots[i].buffer.GetDesc().Width {
                let to = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: lend(&st.slots[i].buffer),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp },
                };
                let from = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: lend(src),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
                st.slots[i].filled = Some((fp, sd.Format));
            } else {
                st.errors.push(format!("frame {} needs {total} bytes, more than the readback buffer", p.frame_id));
            }
        }
        if !back.is_empty() {
            list.ResourceBarrier(&back);
        }
        0
    }

    struct Fsr {
        create: ffx::CreateContext,
        destroy: ffx::DestroyContext,
        configure: ffx::Configure,
        query: ffx::Query,
        dispatch: ffx::Dispatch,
        swap_context: *mut c_void,
        fg_context: *mut c_void,
        shared: *const FsrShared,
        prepare_type: u64,
    }

    unsafe fn proc<T: Copy>(module: HMODULE, name: &str) -> Result<T> {
        let n = format!("{name}\0");
        let p = GetProcAddress(module, PCSTR(n.as_ptr())).ok_or_else(|| anyhow!("export {name} not found"))?;
        Ok(transmute_copy(&p))
    }

    impl Fsr {
        unsafe fn load(sdk: &Path, rep: &mut Report) -> Result<Fsr> {
            let dll = sdk.join("ffx").join("amd_fidelityfx_loader_dx12.dll");
            if !dll.exists() {
                bail!("{} not found", dll.display());
            }
            // The loader looks for the effect DLL next to itself.
            let module = LoadLibraryExW(PCWSTR(wide_path(&dll).as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
                .with_context(|| format!("loading {}", dll.display()))?;
            rep.say(format!("FSR: loaded {}", dll.display()));
            Ok(Fsr {
                create: proc(module, "ffxCreateContext")?,
                destroy: proc(module, "ffxDestroyContext")?,
                configure: proc(module, "ffxConfigure")?,
                query: proc(module, "ffxQuery")?,
                dispatch: proc(module, "ffxDispatch")?,
                swap_context: null_mut(),
                fg_context: null_mut(),
                shared: null(),
                prepare_type: ffx::PREPARE_V2,
            })
        }

        unsafe fn list_versions(&self, device: &ID3D12Device, create_type: u64, what: &str, rep: &mut Report) {
            let mut count = 0u64;
            let mut q = ffx::QueryGetVersions {
                header: ffx::Header { ty: ffx::QUERY_GET_VERSIONS, next: null_mut() },
                create_desc_type: create_type,
                device: device.as_raw(),
                output_count: &mut count,
                version_ids: null_mut(),
                version_names: null_mut(),
            };
            let r = (self.query)(null_mut(), &mut q.header);
            if r != 0 || count == 0 {
                rep.say(format!("FSR: {what} versions: query returned {} ({}), count {count}", r, ffx::code_name(r)));
                return;
            }
            let mut ids = vec![0u64; count as usize];
            let mut names = vec![null::<c_char>(); count as usize];
            q.version_ids = ids.as_mut_ptr();
            q.version_names = names.as_mut_ptr();
            let r = (self.query)(null_mut(), &mut q.header);
            let list: Vec<String> = names
                .iter()
                .take(count as usize)
                .map(|n| if n.is_null() { "?".into() } else { CStr::from_ptr(*n).to_string_lossy().to_string() })
                .collect();
            rep.say(format!("FSR: {what} versions offered by the DLL ({}): {}", ffx::code_name(r), list.join(", ")));
        }

        /// Creates the frame-pacing swapchain for the window. The returned object behaves
        /// like an ordinary swapchain towards us.
        unsafe fn create_swapchain(
            &mut self,
            factory: &IDXGIFactory4,
            queue: &ID3D12CommandQueue,
            hwnd: HWND,
            desc: &DXGI_SWAP_CHAIN_DESC1,
            rep: &mut Report,
        ) -> Result<IDXGISwapChain4> {
            let mut out: *mut c_void = null_mut();
            // The descriptors must stay alive as long as the context does.
            let version = Box::leak(Box::new(ffx::CreateVersion {
                header: ffx::Header { ty: ffx::CREATE_SWAPCHAIN_VERSION, next: null_mut() },
                version: ffx::SWAPCHAIN_VERSION,
            }));
            let d = Box::leak(Box::new(ffx::CreateSwapChainForHwnd {
                header: ffx::Header { ty: ffx::CREATE_SWAPCHAIN_FOR_HWND, next: &mut version.header },
                swapchain: &mut out,
                hwnd: hwnd.0,
                desc: Box::leak(Box::new(*desc)) as *const _ as *const c_void,
                fullscreen_desc: null(),
                dxgi_factory: factory.as_raw(),
                game_queue: queue.as_raw(),
            }));
            let mut r = (self.create)(&mut self.swap_context, &mut d.header, null());
            if r != 0 {
                rep.say(format!("FSR: swapchain create with version descriptor returned {} ({}); retrying without", r, ffx::code_name(r)));
                d.header.next = null_mut();
                r = (self.create)(&mut self.swap_context, &mut d.header, null());
            }
            if r != 0 || out.is_null() {
                bail!("FSR swapchain context: {} ({})", r, ffx::code_name(r));
            }
            rep.say("FSR: frame-generation swapchain created for the window");
            Ok(IDXGISwapChain4::from_raw(out))
        }

        unsafe fn create_fg(&mut self, device: &ID3D12Device, w: u32, h: u32, t0: Instant, rep: &mut Report) -> Result<()> {
            let version = Box::leak(Box::new(ffx::CreateVersion {
                header: ffx::Header { ty: ffx::CREATE_FG_VERSION, next: null_mut() },
                version: ffx::FG_VERSION,
            }));
            let backend = Box::leak(Box::new(ffx::CreateBackendDx12 {
                header: ffx::Header { ty: ffx::CREATE_BACKEND_DX12, next: &mut version.header },
                device: device.as_raw(),
            }));
            let d = Box::leak(Box::new(ffx::CreateFg {
                header: ffx::Header { ty: ffx::CREATE_FG, next: &mut backend.header },
                flags: 0, // SDR, normal depth range, vectors at render size
                display_size: ffx::Dim2 { width: w, height: h },
                max_render_size: ffx::Dim2 { width: w, height: h },
                back_buffer_format: ffx::FORMAT_R8G8B8A8_UNORM,
            }));
            let mut r = (self.create)(&mut self.fg_context, &mut d.header, null());
            if r != 0 {
                rep.say(format!("FSR: frame-generation create with version descriptor returned {} ({}); retrying without", r, ffx::code_name(r)));
                backend.header.next = null_mut();
                r = (self.create)(&mut self.fg_context, &mut d.header, null());
            }
            if r != 0 {
                bail!("FSR frame-generation context: {} ({})", r, ffx::code_name(r));
            }
            rep.say("FSR: frame-generation context created");
            self.shared = Box::leak(Box::new(FsrShared {
                dispatch: self.dispatch,
                fg_context: self.fg_context,
                device: device.clone(),
                t0,
                state: Mutex::new(FsrCallbackState::default()),
            }));
            let dbg = ffx::ConfigureGlobalDebug {
                header: ffx::Header { ty: ffx::CONFIGURE_GLOBAL_DEBUG, next: null_mut() },
                effect_id: 0x0002_0000,
                message: Some(ffx_message_callback),
                debug_level: 2,
            };
            let r = (self.configure)(null_mut(), &dbg.header);
            if r != 0 {
                rep.say(format!("FSR: message callback not installed ({})", ffx::code_name(r)));
            }
            Ok(())
        }

        unsafe fn configure_frame(
            &mut self,
            swap: &IDXGISwapChain4,
            enabled: bool,
            use_callback: bool,
            only_generated: bool,
            w: u32,
            h: u32,
            frame_id: u64,
        ) -> u32 {
            let c = ffx::ConfigureFg {
                header: ffx::Header { ty: ffx::CONFIGURE_FG, next: null_mut() },
                swap_chain: swap.as_raw(),
                present_callback: if enabled && use_callback { Some(fsr_present_callback) } else { None },
                present_callback_user_context: self.shared as *mut c_void,
                frame_generation_callback: if enabled { Some(fsr_generate_callback) } else { None },
                frame_generation_callback_user_context: self.shared as *mut c_void,
                frame_generation_enabled: enabled,
                allow_async_workloads: false,
                hudless_color: ffx::Resource::empty(),
                flags: 0,
                only_present_generated: only_generated,
                generation_rect: ffx::Rect { left: 0, top: 0, width: w as i32, height: h as i32 },
                frame_id,
            };
            (self.configure)(&mut self.fg_context, &c.header)
        }

        /// Hands this frame's depth and motion vectors to FSR on our own command list.
        unsafe fn prepare(&mut self, g: &Gfx, frame_id: u64, frame_ms: f32, reset: bool, mv_sign: f32) -> u32 {
            let tex = |res: &ID3D12Resource, format: u32, usage: u32| ffx::Resource {
                resource: res.as_raw(),
                desc: ffx::ResourceDesc {
                    ty: ffx::RESOURCE_TYPE_TEXTURE2D,
                    format,
                    width: g.w,
                    height: g.h,
                    depth: 1,
                    mip_count: 1,
                    flags: 0,
                    usage,
                },
                state: ffx::STATE_COMPUTE_READ,
            };
            let mut p = ffx::PrepareV2 {
                header: ffx::Header { ty: self.prepare_type, next: null_mut() },
                frame_id,
                flags: 0,
                command_list: g.list.as_raw(),
                render_size: ffx::Dim2 { width: g.w, height: g.h },
                jitter_offset: ffx::Float2::default(),
                // Our vectors are already in pixels, pointing from this frame to the last.
                motion_vector_scale: ffx::Float2 { x: mv_sign, y: mv_sign },
                frame_time_delta: frame_ms,
                reset,
                camera_near: 0.1,
                camera_far: 1000.0,
                camera_fov_angle_vertical: 1.0472,
                view_space_to_meters_factor: 1.0,
                depth: tex(&g.depth.res, ffx::FORMAT_R32_FLOAT, 0),
                motion_vectors: tex(&g.mv.res, ffx::FORMAT_R16G16_FLOAT, ffx::USAGE_UAV),
                camera_position: [0.0; 3],
                camera_up: [0.0, 1.0, 0.0],
                camera_right: [1.0, 0.0, 0.0],
                camera_forward: [0.0, 0.0, 1.0],
            };
            let mut r = (self.dispatch)(&mut self.fg_context, &p.header);
            if (r == 2 || r == 7) && self.prepare_type == ffx::PREPARE_V2 {
                self.prepare_type = ffx::PREPARE_V1;
                p.header.ty = self.prepare_type;
                r = (self.dispatch)(&mut self.fg_context, &p.header);
            }
            r
        }

        unsafe fn wait_for_presents(&mut self) -> u32 {
            let h = ffx::Header { ty: ffx::SWAPCHAIN_WAIT_FOR_PRESENTS, next: null_mut() };
            (self.dispatch)(&mut self.swap_context, &h)
        }
    }

    // ------------------------------------------------------------------------------------
    // Streamline
    // ------------------------------------------------------------------------------------

    struct Sl {
        module: HMODULE,
        shutdown: sl::Shutdown,
        is_feature_supported: sl::IsFeatureSupported,
        get_feature_requirements: sl::GetFeatureRequirements,
        set_d3d_device: sl::SetD3DDevice,
        get_native_interface: sl::GetNativeInterface,
        get_feature_function: sl::GetFeatureFunction,
        get_new_frame_token: sl::GetNewFrameToken,
        set_constants: sl::SetConstants,
        set_tag_for_frame: sl::SetTagForFrame,
        dlssg_get_state: Option<sl::DlssgGetState>,
        dlssg_set_options: Option<sl::DlssgSetOptions>,
        reflex_set_options: Option<sl::ReflexSetOptions>,
        reflex_sleep: Option<sl::ReflexSleep>,
        pcl_set_marker: Option<sl::PclSetMarker>,
        viewport: sl::ViewportHandle,
    }

    type CreateFactory2Fn = unsafe extern "system" fn(u32, *const GUID, *mut *mut c_void) -> HRESULT;
    type CreateDeviceFn = unsafe extern "system" fn(*mut c_void, D3D_FEATURE_LEVEL, *const GUID, *mut *mut c_void) -> HRESULT;

    impl Sl {
        /// Loads the interposer and initialises Streamline. Must run before the process
        /// touches DXGI or D3D12.
        unsafe fn load(sdk: &Path, out: &Path, app_id: u32, rep: &mut Report) -> Result<Sl> {
            let bin = sdk.join("streamline").join("sdk").join("bin").join("x64");
            let dll = bin.join("sl.interposer.dll");
            if !dll.exists() {
                bail!("{} not found", dll.display());
            }
            let module = LoadLibraryExW(PCWSTR(wide_path(&dll).as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
                .with_context(|| format!("loading {}", dll.display()))?;
            rep.say(format!("DLSS: loaded {}", dll.display()));

            let init: sl::Init = proc(module, "slInit")?;
            let bin_w: &'static [u16] = Box::leak(wide_path(&bin).into_boxed_slice());
            let paths: &'static [*const u16] = Box::leak(vec![bin_w.as_ptr()].into_boxed_slice());
            let logs: &'static [u16] = Box::leak(wide_path(out).into_boxed_slice());
            let features: &'static [u32] =
                Box::leak(vec![sl::FEATURE_DLSS_G, sl::FEATURE_REFLEX, sl::FEATURE_PCL].into_boxed_slice());
            let mut pref = sl::Preferences::new();
            pref.paths_to_plugins = paths.as_ptr();
            pref.num_paths_to_plugins = 1;
            pref.path_to_logs_and_data = logs.as_ptr();
            pref.log_message_callback = Some(sl_log_callback);
            // No over-the-air updates: the probe must not download anything.
            pref.flags = sl::PREF_DISABLE_CL_STATE_TRACKING | sl::PREF_FRAME_BASED_TAGGING;
            pref.features_to_load = features.as_ptr();
            pref.num_features_to_load = features.len() as u32;
            pref.application_id = app_id;
            pref.engine = 0; // custom engine
            pref.engine_version = c"1.0".as_ptr();
            pref.project_id = c"6d0f8a52-3c1e-4b7a-9f43-2a7c5e81d0b9".as_ptr();
            let r = init(&pref, sl::SDK_VERSION);
            rep.say(format!("DLSS: slInit -> {} ({})", r, sl::result_name(r)));
            if r != 0 {
                bail!("slInit failed: {} ({})", r, sl::result_name(r));
            }
            Ok(Sl {
                module,
                shutdown: proc(module, "slShutdown")?,
                is_feature_supported: proc(module, "slIsFeatureSupported")?,
                get_feature_requirements: proc(module, "slGetFeatureRequirements")?,
                set_d3d_device: proc(module, "slSetD3DDevice")?,
                get_native_interface: proc(module, "slGetNativeInterface")?,
                get_feature_function: proc(module, "slGetFeatureFunction")?,
                get_new_frame_token: proc(module, "slGetNewFrameToken")?,
                set_constants: proc(module, "slSetConstants")?,
                set_tag_for_frame: proc(module, "slSetTagForFrame")?,
                dlssg_get_state: None,
                dlssg_set_options: None,
                reflex_set_options: None,
                reflex_sleep: None,
                pcl_set_marker: None,
                viewport: sl::ViewportHandle::new(0),
            })
        }

        /// The interposer's own factory and device entry points: objects made through them
        /// are the proxies Streamline needs to see the swapchain and its presents.
        unsafe fn create_factory(&self) -> Result<IDXGIFactory4> {
            let f: CreateFactory2Fn = proc(self.module, "CreateDXGIFactory2")?;
            let mut out: *mut c_void = null_mut();
            f(0, &IDXGIFactory4::IID, &mut out).ok().context("interposer CreateDXGIFactory2")?;
            Ok(IDXGIFactory4::from_raw(out))
        }

        unsafe fn create_device(&self, adapter: &IDXGIAdapter1) -> Result<ID3D12Device> {
            let f: CreateDeviceFn = proc(self.module, "D3D12CreateDevice")?;
            let mut out: *mut c_void = null_mut();
            f(adapter.as_raw(), D3D_FEATURE_LEVEL_12_0, &ID3D12Device::IID, &mut out).ok().context("interposer D3D12CreateDevice")?;
            Ok(ID3D12Device::from_raw(out))
        }

        unsafe fn feature_fn<T: Copy>(&self, feature: u32, name: &str, rep: &mut Report) -> Option<T> {
            let n = format!("{name}\0");
            let mut p: *mut c_void = null_mut();
            let r = (self.get_feature_function)(feature, n.as_ptr() as *const c_char, &mut p);
            if r != 0 || p.is_null() {
                rep.say(format!("DLSS: {name} unavailable: {} ({})", r, sl::result_name(r)));
                return None;
            }
            Some(transmute_copy(&p))
        }

        unsafe fn marker(&self, marker: u32, token: *const c_void) {
            if let Some(f) = self.pcl_set_marker {
                f(marker, token);
            }
        }
    }

    /// Constants for a picture with no camera: a fixed valid projection, no camera motion,
    /// orthonormal axes (the scene-change detector refuses to run otherwise).
    fn sl_constants(w: u32, h: u32, mv_sign: f32, reset: bool) -> sl::Constants {
        let mut c = sl::Constants::new();
        let (near, far, fov) = (0.1f32, 1000.0f32, 1.0472f32);
        let aspect = w as f32 / h as f32;
        let ys = 1.0 / (fov * 0.5).tan();
        let xs = ys / aspect;
        let a = far / (far - near);
        let b = -near * far / (far - near);
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        c.camera_view_to_clip = [[xs, 0.0, 0.0, 0.0], [0.0, ys, 0.0, 0.0], [0.0, 0.0, a, 1.0], [0.0, 0.0, b, 0.0]];
        c.clip_to_camera_view =
            [[1.0 / xs, 0.0, 0.0, 0.0], [0.0, 1.0 / ys, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0 / b], [0.0, 0.0, 1.0, -a / b]];
        c.clip_to_lens_clip = identity;
        c.clip_to_prev_clip = identity;
        c.prev_clip_to_clip = identity;
        c.jitter_offset = [0.0, 0.0];
        // Vectors are in pixels; this brings them into the -1..1 range Streamline wants.
        c.mvec_scale = [mv_sign / w as f32, mv_sign / h as f32];
        c.camera_pinhole_offset = [0.0, 0.0];
        c.camera_pos = [0.0, 0.0, 0.0];
        c.camera_up = [0.0, 1.0, 0.0];
        c.camera_right = [1.0, 0.0, 0.0];
        c.camera_fwd = [0.0, 0.0, 1.0];
        c.camera_near = near;
        c.camera_far = far;
        c.camera_fov = fov;
        c.camera_aspect_ratio = aspect;
        c.motion_vectors_invalid_value = sl::INVALID_FLOAT;
        c.depth_inverted = sl::FALSE;
        c.camera_motion_included = sl::TRUE;
        c.motion_vectors_3d = sl::FALSE;
        c.reset = if reset { sl::TRUE } else { sl::FALSE };
        c.orthographic_projection = sl::FALSE;
        c.motion_vectors_dilated = sl::FALSE;
        c.motion_vectors_jittered = sl::FALSE;
        c
    }

    // ------------------------------------------------------------------------------------
    // Measuring what reaches the swapchain
    // ------------------------------------------------------------------------------------

    struct SendPtr(*mut c_void);
    unsafe impl Send for SendPtr {}

    /// Watches the swapchain's present counter from a side thread. Both frame generators
    /// present from their own threads, so timing our `Present` calls says nothing about
    /// their output; the counter on the real swapchain does.
    fn spawn_present_poll(swap: SendPtr, t0: Instant, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<Vec<(f64, u32)>> {
        std::thread::spawn(move || {
            let swap = swap;
            let mut out: Vec<(f64, u32)> = Vec::new();
            let Some(sc) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swap.0) }) else { return out };
            let mut last = u32::MAX;
            while !stop.load(Ordering::Relaxed) {
                match unsafe { sc.GetLastPresentCount() } {
                    Ok(c) if c != last => {
                        out.push((t0.elapsed().as_secs_f64(), c));
                        last = c;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
                let until = Instant::now() + Duration::from_micros(120);
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
            out
        })
    }

    fn interval_stats(mut v: Vec<f64>) -> String {
        if v.len() < 2 {
            return "too few samples".into();
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = v.len();
        let mean = v.iter().sum::<f64>() / n as f64;
        let sd = (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64).sqrt();
        let pct = |p: f64| v[((n - 1) as f64 * p).round() as usize];
        format!(
            "n {} mean {:.2} ms, sd {:.2}, min {:.2}, p1 {:.2}, p50 {:.2}, p99 {:.2}, max {:.2}",
            n,
            mean,
            sd,
            v[0],
            pct(0.01),
            pct(0.5),
            pct(0.99),
            v[n - 1]
        )
    }

    // ------------------------------------------------------------------------------------
    // Scoring a captured frame against the clip
    // ------------------------------------------------------------------------------------

    struct Judge<'a> {
        scene: &'a Scene,
        w: usize,
        h: usize,
        cache: HashMap<i64, Img>,
        scratch: Vec<u8>,
        out: PathBuf,
        tag: String,
    }

    impl<'a> Judge<'a> {
        fn truth(&mut self, t: f64) -> Img {
            let key = (t * 2.0).round() as i64;
            if let Some(i) = self.cache.get(&key) {
                return i.clone();
            }
            self.scene.render(t, &mut self.scratch);
            let img = scale_rgba(&self.scratch, SRC_W, SRC_H, self.w, self.h);
            if self.cache.len() > 64 {
                self.cache.clear();
            }
            self.cache.insert(key, img.clone());
            img
        }

        fn disp(&self, r: Rect, grow: usize) -> Rect {
            let (sx, sy) = (self.w as f64 / SRC_W as f64, self.h as f64 / SRC_H as f64);
            (
                ((r.0 as f64 * sx) as usize).saturating_sub(grow),
                ((r.1 as f64 * sy) as usize).saturating_sub(grow),
                ((r.2 as f64 * sx) as usize + grow).min(self.w),
                ((r.3 as f64 * sy) as usize + grow).min(self.h),
            )
        }

        /// Finds which moment of the clip the capture shows (whole number: a real frame;
        /// half: an in-between one), scores it there, and for in-between frames compares
        /// against two things that need no frame generator: blending the neighbours and
        /// repeating the earlier one.
        fn judge(&mut self, cap: &Img, label: &str, around: f64, save_full: bool) -> String {
            let counter = self.disp(CNT, 4);
            let full = (0, 0, self.w, self.h);
            let mut best = (f64::MIN, 0.0);
            let mut t = around - 4.0;
            while t <= around + 1.0 {
                if t >= 0.0 {
                    let p = psnr(cap, &self.truth(t), full, Some(counter));
                    if p > best.0 {
                        best = (p, t);
                    }
                }
                t += 0.5;
            }
            let (p_full, t) = best;
            let name = format!("{}_{}", self.tag, label);
            if save_full {
                let _ = write_png(&self.out.join(format!("{name}_full.png")), cap);
            }
            if (t * 2.0).round() as i64 % 2 == 0 {
                return format!("{name}: shows real frame {t:.0}; PSNR vs reference {p_full:.1} dB (this is the ceiling of the comparison)");
            }
            let (a, b, mid) = (self.truth(t - 0.5), self.truth(t + 0.5), self.truth(t));
            let mix = blend(&a, &b);
            let (cx, cy) = self.scene.obj_pos(t);
            let r = OBJ_R as usize + 30;
            let obj_src: Rect = (
                (cx as usize).saturating_sub(r),
                (cy as usize).saturating_sub(r),
                ((cx as usize) + r).min(SRC_W),
                ((cy as usize) + r).min(SRC_H),
            );
            let regions: [(&str, Rect); 3] = [
                ("object", self.disp(obj_src, 0)),
                ("text", self.disp((360, TEXT_ROWS.0, 1560, TEXT_ROWS.1), 0)),
                ("hud", self.disp(HUD, 40)),
            ];
            let mut line = format!(
                "{name}: in-between frame t={t:.1}; PSNR generated/blend/repeat: full {:.1}/{:.1}/{:.1}",
                p_full,
                psnr(&mix, &mid, full, Some(counter)),
                psnr(&a, &mid, full, Some(counter))
            );
            for (what, rc) in regions {
                if rc.2 <= rc.0 + 8 || rc.3 <= rc.1 + 8 {
                    continue;
                }
                line += &format!(
                    ", {what} {:.1}/{:.1}/{:.1}",
                    psnr(cap, &mid, rc, None),
                    psnr(&mix, &mid, rc, None),
                    psnr(&a, &mid, rc, None)
                );
                // Earlier real frame | generated | true in-between | later real frame.
                let strip = hcat(&[crop(&a, rc), crop(cap, rc), crop(&mid, rc), crop(&b, rc)]);
                let _ = write_png(&self.out.join(format!("{name}_{what}.png")), &strip);
            }
            line
        }
    }

    // ------------------------------------------------------------------------------------
    // The run
    // ------------------------------------------------------------------------------------

    pub fn run() -> Result<()> {
        unsafe { run_inner() }
    }

    unsafe fn run_inner() -> Result<()> {
        let args = parse_args()?;
        std::fs::create_dir_all(&args.out)?;
        let _ = SetProcessDpiAwarenessContext(-4); // per-monitor: one window pixel is one screen pixel
        timeBeginPeriod(1);
        let tag = format!("{:?}_{}fps_{:?}{}", args.mode, args.fps, args.mv, if args.host == Host::Top { String::new() } else { format!("_{:?}", args.host) }).to_lowercase();
        let mut rep = Report { lines: Vec::new() };
        rep.say(format!(
            "fgprobe: mode {:?}, content {} fps, {:.0} s, vectors {:?} (sign {}), window {}x{}, pid {}",
            args.mode,
            args.fps,
            args.seconds,
            args.mv,
            args.mv_sign,
            args.width,
            args.height,
            GetCurrentProcessId()
        ));

        // A vendor library that hangs must not leave a window behind for the user.
        let limit = Duration::from_secs_f64(args.seconds + 90.0);
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            eprintln!("fgprobe: watchdog, still running after {limit:?}; exiting");
            std::process::exit(3);
        });

        let (w, h) = (args.width, args.height);
        let sl = if args.mode == Mode::Dlss { Some(Sl::load(&args.sdk, &args.out, args.app_id, &mut rep)?) } else { None };
        let mut sl = sl;

        let factory: IDXGIFactory4 = match &sl {
            Some(s) => s.create_factory()?,
            None => CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?,
        };
        // The discrete adapter with the most memory.
        let mut adapter: Option<(IDXGIAdapter1, DXGI_ADAPTER_DESC1)> = None;
        let mut i = 0;
        while let Ok(a) = factory.EnumAdapters1(i) {
            let d = a.GetDesc1()?;
            if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0
                && adapter.as_ref().map_or(true, |(_, b)| d.DedicatedVideoMemory > b.DedicatedVideoMemory)
            {
                adapter = Some((a, d));
            }
            i += 1;
        }
        let (adapter, adesc) = adapter.ok_or_else(|| anyhow!("no hardware adapter"))?;
        let name = String::from_utf16_lossy(&adesc.Description);
        rep.say(format!("adapter: {} (vendor {:#06x})", name.trim_end_matches('\0'), adesc.VendorId));
        rep.say(format!("hardware-accelerated GPU scheduling (kernel): {}", hags_state(adesc.AdapterLuid)));

        let mut dlss_ready = false;
        if let Some(s) = &sl {
            let info = sl::AdapterInfo::new(&adesc.AdapterLuid as *const LUID as *const u8, size_of::<LUID>() as u32);
            for (feature, what) in [(sl::FEATURE_DLSS_G, "DLSS-G"), (sl::FEATURE_REFLEX, "Reflex"), (sl::FEATURE_PCL, "PCL")] {
                let r = (s.is_feature_supported)(feature, &info);
                rep.say(format!("DLSS: slIsFeatureSupported({what}) -> {} ({})", r, sl::result_name(r)));
                if feature == sl::FEATURE_DLSS_G {
                    dlss_ready = r == 0;
                }
            }
            let mut req = sl::FeatureRequirements::new();
            let r = (s.get_feature_requirements)(sl::FEATURE_DLSS_G, &mut req);
            rep.say(format!(
                "DLSS: requirements -> {} ({}): flags {:#x} (needs GPU scheduling: {}), driver detected {}.{}.{} required {}.{}.{}, OS detected {}.{}.{} required {}.{}.{}, viewports {}",
                r,
                sl::result_name(r),
                req.flags,
                req.flags & sl::REQ_HARDWARE_SCHEDULING != 0,
                req.driver_version_detected.major,
                req.driver_version_detected.minor,
                req.driver_version_detected.build,
                req.driver_version_required.major,
                req.driver_version_required.minor,
                req.driver_version_required.build,
                req.os_version_detected.major,
                req.os_version_detected.minor,
                req.os_version_detected.build,
                req.os_version_required.major,
                req.os_version_required.minor,
                req.os_version_required.build,
                req.max_num_viewports
            ));
        }

        let device: ID3D12Device = match &sl {
            Some(s) => s.create_device(&adapter)?,
            None => {
                let mut d: Option<ID3D12Device> = None;
                D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut d)?;
                d.ok_or_else(|| anyhow!("no device"))?
            }
        };
        if let Some(s) = &mut sl {
            let mut native: *mut c_void = null_mut();
            let r = (s.get_native_interface)(device.as_raw(), &mut native);
            let r2 = (s.set_d3d_device)(if r == 0 && !native.is_null() { native } else { device.as_raw() });
            rep.say(format!(
                "DLSS: slGetNativeInterface(device) -> {} ({}), slSetD3DDevice -> {} ({})",
                r,
                sl::result_name(r),
                r2,
                sl::result_name(r2)
            ));
            s.dlssg_get_state = s.feature_fn(sl::FEATURE_DLSS_G, "slDLSSGGetState", &mut rep);
            s.dlssg_set_options = s.feature_fn(sl::FEATURE_DLSS_G, "slDLSSGSetOptions", &mut rep);
            s.reflex_set_options = s.feature_fn(sl::FEATURE_REFLEX, "slReflexSetOptions", &mut rep);
            s.reflex_sleep = s.feature_fn(sl::FEATURE_REFLEX, "slReflexSleep", &mut rep);
            s.pcl_set_marker = s.feature_fn(sl::FEATURE_PCL, "slPCLSetMarker", &mut rep);
            dlss_ready &= s.dlssg_get_state.is_some() && s.dlssg_set_options.is_some();
        }

        let queue: ID3D12CommandQueue =
            device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })?;

        let previous_foreground = GetForegroundWindow();
        let (top, hwnd) = create_window(w, h, &format!("fgprobe {tag}"), args.focus, args.host)?;
        let mut tearing = BOOL(0);
        if let Ok(f5) = factory.cast::<IDXGIFactory5>() {
            let _ = f5.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut tearing as *mut BOOL as *mut c_void, 4);
        }
        let tearing = tearing.as_bool();
        let sc_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 3,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if tearing { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
            ..Default::default()
        };
        let t0 = Instant::now();
        let mut fsr = if args.mode == Mode::Fsr { Some(Fsr::load(&args.sdk, &mut rep)?) } else { None };
        let swap: IDXGISwapChain4 = match &mut fsr {
            Some(f) => {
                f.list_versions(&device, ffx::CREATE_FG, "frame generation", &mut rep);
                f.list_versions(&device, ffx::CREATE_SWAPCHAIN_FOR_HWND, "swapchain", &mut rep);
                f.create_swapchain(&factory, &queue, hwnd, &sc_desc, &mut rep)?
            }
            None => factory.CreateSwapChainForHwnd(&queue, hwnd, &sc_desc, None, None::<&IDXGIOutput>)?.cast()?,
        };
        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
        let mut backbuffers: Vec<ID3D12Resource> = Vec::new();
        for i in 0..sc_desc.BufferCount {
            backbuffers.push(swap.GetBuffer(i)?);
        }
        rep.say(format!("swapchain: {}x{} RGBA8, 3 buffers, flip-discard, tearing allowed: {tearing}, window: {:?}", w, h, args.host));

        let mut gfx = Gfx::new(&device, &queue, w, h, args.depth)?;
        if let Some(f) = &mut fsr {
            f.create_fg(&device, w, h, t0, &mut rep)?;
        }

        // DLSS-G: Reflex first (it is mandatory), then the feature itself.
        let mut dlss_options = sl::DlssgOptions::new();
        if let Some(s) = &sl {
            if let Some(f) = s.reflex_set_options {
                let r = f(&sl::ReflexOptions::new(sl::REFLEX_LOW_LATENCY));
                rep.say(format!("DLSS: slReflexSetOptions(low latency) -> {} ({})", r, sl::result_name(r)));
            }
            if let Some(get) = s.dlssg_get_state {
                let mut st = sl::DlssgState::new();
                let r = get(&s.viewport, &mut st, null());
                rep.say(format!(
                    "DLSS: state before enabling -> {} ({}): status {}, min size {}, max generated per real frame {}, dynamic MFG {}, vsync support {}",
                    r,
                    sl::result_name(r),
                    sl::status_names(st.status),
                    st.min_width_or_height,
                    st.num_frames_to_generate_max,
                    st.is_dynamic_mfg_supported,
                    st.is_vsync_support_available
                ));
            }
            dlss_options.mode = sl::DLSSG_MODE_ON;
            dlss_options.num_back_buffers = sc_desc.BufferCount;
            dlss_options.mvec_depth_width = w;
            dlss_options.mvec_depth_height = h;
            dlss_options.color_width = w;
            dlss_options.color_height = h;
            dlss_options.color_buffer_format = DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32;
            dlss_options.mvec_buffer_format = DXGI_FORMAT_R16G16_FLOAT.0 as u32;
            dlss_options.depth_buffer_format = DXGI_FORMAT_R32_FLOAT.0 as u32;
            dlss_options.on_error_callback = Some(sl_api_error_callback);
            if let (true, Some(set)) = (dlss_ready, s.dlssg_set_options) {
                let r = set(&s.viewport, &dlss_options);
                rep.say(format!("DLSS: slDLSSGSetOptions(on, 1 generated frame) -> {} ({})", r, sl::result_name(r)));
                dlss_ready = r == 0;
            } else {
                rep.say("DLSS: frame generation is NOT available here; running without it to finish the report");
            }
        }

        // The present counter lives on the real swapchain; with Streamline that is the one
        // behind the proxy.
        let stop_poll = Arc::new(AtomicBool::new(false));
        let poll = if args.poll {
            let mut raw = swap.as_raw();
            if let Some(s) = &sl {
                let mut native: *mut c_void = null_mut();
                if (s.get_native_interface)(swap.as_raw(), &mut native) == 0 && !native.is_null() {
                    raw = native;
                }
            }
            Some(spawn_present_poll(SendPtr(raw), t0, stop_poll.clone()))
        } else {
            None
        };

        let scene = Scene::new(args.speed);
        let total = (args.fps as f64 * args.seconds).round() as u64;
        let stats_begin = args.fps as u64;
        let stats_end = ((total as f64) * 0.65) as u64;
        let (sx, sy) = (w as f32 / SRC_W as f32, h as f32 / SRC_H as f32);

        // Frames FSR should hand a copy of to us: an earlier real one, the generated one
        // and the later real one, three times over the measured stretch.
        let mut fsr_events: Vec<u64> = Vec::new();
        if let (Some(f), true) = (&fsr, args.fsr_callback) {
            let mut k = stats_begin + 10;
            while k < stats_end && fsr_events.len() < 3 {
                if scene.obj_visible(k as f64) && scene.obj_visible(k as f64 - 1.0) {
                    fsr_events.push(k);
                    k += (stats_end - stats_begin) / 4;
                } else {
                    k += 1;
                }
            }
            let mut st = (*f.shared).state.lock().unwrap();
            for &e in &fsr_events {
                for (frame_id, generated) in [(e - 1, false), (e, true), (e, false)] {
                    st.slots.push(Slot {
                        buffer: make_buffer(&device, (w as u64 * 4 + 512) * h as u64 * 2, D3D12_HEAP_TYPE_READBACK)?,
                        frame_id,
                        generated,
                        filled: None,
                    });
                }
            }
        }
        // Frames at which the window itself is captured, all after the measured stretch
        // because a capture stalls the loop for a moment.
        let mut window_shots: Vec<u64> = Vec::new();
        let mut k = stats_end + (args.fps as u64) / 2;
        while k + 4 < total && window_shots.len() < 4 {
            if scene.obj_visible(k as f64) && scene.obj_visible(k as f64 - 2.0) {
                window_shots.push(k);
                k += 9;
            } else {
                k += 1;
            }
        }

        let mut cpu_frame = vec![0u8; SRC_W * SRC_H * 4];
        let period = Duration::from_secs_f64(1.0 / args.fps as f64);
        let start = Instant::now() + Duration::from_millis(300);
        let mut present_at: Vec<f64> = Vec::new();
        let mut present_ms: Vec<f64> = Vec::new();
        let mut dlss_frames: Vec<(u32, u32)> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut captures: Vec<(String, f64, Img)> = Vec::new();
        let note = |errors: &mut Vec<String>, s: String| {
            if errors.len() < 12 && !errors.contains(&s) {
                errors.push(s);
            }
        };
        let (mut win_begin, mut win_end) = (0.0f64, 0.0f64);
        let mut frames_done = 0u64;
        let mut foreground_frames = 0u64;
        let mut tail_started = false;

        for k in 0..total {
            if QUIT.load(Ordering::Relaxed) {
                rep.say("window closed by the user; stopping early");
                break;
            }
            scene.render(k as f64, &mut cpu_frame);

            // Streamline wants a token per frame, a sleep call and latency markers around
            // the simulate / submit / present steps.
            let mut token: *mut c_void = null_mut();
            if let Some(s) = &sl {
                let index = k as u32;
                let r = (s.get_new_frame_token)(&mut token, &index);
                if r != 0 {
                    note(&mut errors, format!("slGetNewFrameToken -> {} ({})", r, sl::result_name(r)));
                }
                if let (Some(f), false) = (s.reflex_sleep, token.is_null()) {
                    let r = f(token);
                    if r != 0 {
                        note(&mut errors, format!("slReflexSleep -> {} ({})", r, sl::result_name(r)));
                    }
                }
            }
            wait_until(start + period * k as u32);
            if k == stats_begin {
                win_begin = t0.elapsed().as_secs_f64();
            }
            if k == stats_end {
                win_end = t0.elapsed().as_secs_f64();
            }
            if let (Some(s), false) = (&sl, token.is_null()) {
                s.marker(sl::PCL_SIMULATION_START, token);
                s.marker(sl::PCL_SIMULATION_END, token);
                s.marker(sl::PCL_RENDER_SUBMIT_START, token);
            }

            // The tail of the run shows generated frames only (where asked), so a capture
            // of the window is known to be a generated frame.
            if k == stats_end && args.tail_generated_only && !tail_started {
                tail_started = true;
                if let (Some(s), true) = (&sl, dlss_ready) {
                    dlss_options.flags |= sl::DLSSG_FLAG_SHOW_ONLY_INTERPOLATED;
                    if let Some(set) = s.dlssg_set_options {
                        let r = set(&s.viewport, &dlss_options);
                        rep.say(format!("DLSS: show generated frames only -> {} ({})", r, sl::result_name(r)));
                    }
                }
            }
            let fsr_only_generated = tail_started && fsr.is_some();

            let (px, py) = scene.obj_pos(k as f64);
            let (qx, qy) = scene.obj_pos(k as f64 - 1.0);
            let mut obj_v = [(qx - px) as f32 * sx, (qy - py) as f32 * sy];
            if obj_v[0].abs() > 300.0 {
                obj_v = [0.0, 0.0]; // the disc wrapped around while off screen
            }
            let zero = args.mv == Mv::Zero;
            let consts = CsConsts {
                disp: [w as i32, h as i32],
                grid: [gfx.gw as i32, gfx.gh as i32],
                q: [gfx.qw as i32, gfx.qh as i32],
                coarse_range: 10,
                pad0: 0,
                pan_v: if zero { [0.0, 0.0] } else { [(PAN * args.speed) as f32 * sx, 0.0] },
                obj_v: if zero { [0.0, 0.0] } else { obj_v },
                obj_c: [px as f32 * sx, py as f32 * sy],
                obj_r: OBJ_R as f32 * sx,
                pad1: 0.0,
                hud: [HUD.0 as f32 * sx, HUD.1 as f32 * sy, HUD.2 as f32 * sx, HUD.3 as f32 * sy],
                hud2: [CNT.0 as f32 * sx, CNT.1 as f32 * sy, CNT.2 as f32 * sx, CNT.3 as f32 * sy],
            };
            let cur = (k % 2) as usize;
            // Streamline requires this call every frame; it is also simply the right index.
            let bb = swap.GetCurrentBackBufferIndex() as usize;
            gfx.record_frame(&cpu_frame, cur, &consts, args.mv, &backbuffers[bb])?;

            if let Some(f) = &mut fsr {
                let r = f.configure_frame(&swap, true, args.fsr_callback, fsr_only_generated, w, h, k);
                if r != 0 {
                    note(&mut errors, format!("ffxConfigure(frame generation) -> {} ({})", r, ffx::code_name(r)));
                }
                let r = f.prepare(&gfx, k, 1000.0 / args.fps as f32, k == 0, args.mv_sign);
                if r != 0 {
                    note(&mut errors, format!("ffxDispatch(prepare) -> {} ({})", r, ffx::code_name(r)));
                }
            }
            if let (Some(s), false) = (&sl, token.is_null()) {
                let c = sl_constants(w, h, args.mv_sign, k == 0);
                let r = (s.set_constants)(&c, token, &s.viewport);
                if r != 0 {
                    note(&mut errors, format!("slSetConstants -> {} ({})", r, sl::result_name(r)));
                }
                let state = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0 as u32;
                let mut depth = sl::Resource::tex2d(gfx.depth.res.as_raw(), state, w, h, DXGI_FORMAT_R32_FLOAT.0 as u32);
                let mut mvec = sl::Resource::tex2d(gfx.mv.res.as_raw(), state, w, h, DXGI_FORMAT_R16G16_FLOAT.0 as u32);
                let tags = [
                    sl::ResourceTag::new(&mut depth, sl::BUFFER_DEPTH, sl::LIFECYCLE_VALID_UNTIL_PRESENT),
                    sl::ResourceTag::new(&mut mvec, sl::BUFFER_MOTION_VECTORS, sl::LIFECYCLE_VALID_UNTIL_PRESENT),
                ];
                let r = (s.set_tag_for_frame)(token, &s.viewport, tags.as_ptr(), tags.len() as u32, null_mut());
                if r != 0 {
                    note(&mut errors, format!("slSetTagForFrame -> {} ({})", r, sl::result_name(r)));
                }
            }

            gfx.submit()?;
            if let (Some(s), false) = (&sl, token.is_null()) {
                s.marker(sl::PCL_RENDER_SUBMIT_END, token);
                s.marker(sl::PCL_PRESENT_START, token);
            }
            let before = Instant::now();
            let hr = swap.Present(0, if tearing { DXGI_PRESENT_ALLOW_TEARING } else { DXGI_PRESENT(0) });
            present_at.push(t0.elapsed().as_secs_f64());
            present_ms.push(before.elapsed().as_secs_f64() * 1000.0);
            if hr.is_err() {
                note(&mut errors, format!("Present -> {hr:?}"));
            }
            if let Some(s) = &sl {
                if !token.is_null() {
                    s.marker(sl::PCL_PRESENT_END, token);
                }
                if let Some(get) = s.dlssg_get_state {
                    let mut st = sl::DlssgState::new();
                    let r = get(&s.viewport, &mut st, null());
                    if r != 0 {
                        note(&mut errors, format!("slDLSSGGetState -> {} ({})", r, sl::result_name(r)));
                    }
                    if st.status != 0 {
                        note(&mut errors, format!("DLSS-G status {}", sl::status_names(st.status)));
                    }
                    dlss_frames.push((st.status, st.num_frames_actually_presented));
                }
            }
            gfx.wait_idle()?;
            frames_done = k + 1;
            if GetForegroundWindow() == top {
                foreground_frames += 1;
            }

            if window_shots.contains(&k) {
                // A frame generator shows its in-between picture first and the real frame
                // half a period later, so look early and look again late.
                for (part, at) in [("a", 0.22), ("b", 0.75)] {
                    wait_until(before + Duration::from_secs_f64(at / args.fps as f64));
                    if let Some(img) = capture_window(hwnd, w as usize, h as usize) {
                        captures.push((format!("window{}{part}", captures.len() / 2), k as f64, img));
                    } else {
                        note(&mut errors, "window capture failed".into());
                    }
                }
            }
        }
        if win_end == 0.0 {
            win_end = t0.elapsed().as_secs_f64();
        }
        stop_poll.store(true, Ordering::Relaxed);
        let polled = poll.map(|p| p.join().unwrap_or_default()).unwrap_or_default();

        // ---- numbers --------------------------------------------------------------------
        let span = (win_end - win_begin).max(1e-3);
        let in_win = |t: f64| t >= win_begin && t < win_end;
        let ours: Vec<f64> = present_at.iter().copied().filter(|t| in_win(*t)).collect();
        let real_fps = ours.len() as f64 / span;
        rep.say(format!("--- results over the measured stretch ({span:.2} s, frames {stats_begin}..{stats_end} of {frames_done}) ---"));
        rep.say(format!(
            "our presents (real frames): {:.2}/s; spacing {}",
            real_fps,
            interval_stats(ours.windows(2).map(|p| (p[1] - p[0]) * 1000.0).collect())
        ));
        rep.say(format!("time inside Present: {}", interval_stats(present_ms.clone())));
        rep.say(format!("window was the foreground window for {foreground_frames} of {frames_done} frames"));
        if polled.len() > 2 {
            let inw: Vec<(f64, u32)> = polled.iter().copied().filter(|(t, _)| in_win(*t)).collect();
            if inw.len() > 2 {
                let count = inw[inw.len() - 1].1.wrapping_sub(inw[0].1) as f64;
                let dt = inw[inw.len() - 1].0 - inw[0].0;
                // A jump of n in the counter between two looks is n presents spread evenly.
                let mut gaps = Vec::new();
                for p in inw.windows(2) {
                    let n = p[1].1.wrapping_sub(p[0].1).max(1);
                    for _ in 0..n.min(8) {
                        gaps.push((p[1].0 - p[0].0) * 1000.0 / n as f64);
                    }
                }
                rep.say(format!(
                    "swapchain present counter: {:.2} presents/s in total -> {:.2} generated/s beside {:.2} real/s",
                    count / dt,
                    (count / dt - real_fps).max(0.0),
                    real_fps
                ));
                rep.say(format!("present-to-present on the real swapchain: {}", interval_stats(gaps)));
            }
        } else if args.poll {
            rep.say("swapchain present counter: not readable on this swapchain");
        }
        if let Some(f) = &fsr {
            let st = (*f.shared).state.lock().unwrap();
            let gen = st.events.iter().filter(|e| in_win(e.0) && e.1).count() as f64;
            let real = st.events.iter().filter(|e| in_win(e.0) && !e.1).count() as f64;
            rep.say(format!(
                "FSR present callback: {:.2} generated/s, {:.2} real/s (whole run: {} generated, {} real)",
                gen / span,
                real / span,
                st.events.iter().filter(|e| e.1).count(),
                st.events.iter().filter(|e| !e.1).count()
            ));
            if let Some(s) = &st.first_call {
                rep.say(format!("FSR present callback, first call: {s}"));
            }
            for e in &st.errors {
                rep.say(format!("FSR callback error: {e}"));
            }
        }
        if sl.is_some() && !dlss_frames.is_empty() {
            let lo = (stats_begin as usize).min(dlss_frames.len());
            let hi = (stats_end as usize).min(dlss_frames.len());
            let part = &dlss_frames[lo..hi];
            let presented: u64 = part.iter().map(|f| f.1 as u64).sum();
            let twos = part.iter().filter(|f| f.1 >= 2).count();
            let bad = part.iter().filter(|f| f.0 != 0).count();
            rep.say(format!(
                "DLSS-G counter (numFramesActuallyPresented): {:.2} frames/s presented -> {:.2} generated/s; {} of {} calls reported 2 or more; calls with a non-OK status: {}",
                presented as f64 / span,
                (presented as f64 / span - real_fps).max(0.0),
                twos,
                part.len(),
                bad
            ));
        }
        for e in &errors {
            rep.say(format!("error: {e}"));
        }
        if let Ok(l) = SL_API_ERRORS.lock() {
            if !l.is_empty() {
                rep.say(format!("DLSS: API errors reported by the pacer: {:x?}", &l[..l.len().min(8)]));
            }
        }
        if let Ok(l) = FFX_LOG.lock() {
            let mut seen: Vec<&String> = Vec::new();
            for m in l.iter() {
                if !seen.contains(&m) && seen.len() < 12 {
                    seen.push(m);
                    rep.say(format!("FSR message: {m}"));
                }
            }
        }
        if let Ok(l) = SL_LOG.lock() {
            let mut seen: Vec<String> = Vec::new();
            for (ty, m) in l.iter() {
                // Drop the timestamp so repeats collapse.
                let key: String = m.split(']').last().unwrap_or(m).trim().chars().take(200).collect();
                if *ty >= 1 && !seen.contains(&key) && seen.len() < 25 {
                    seen.push(key.clone());
                    rep.say(format!("DLSS log {}: {key}", if *ty == 1 { "warning" } else { "error" }));
                }
            }
            let all: Vec<String> = l.iter().map(|(t, m)| format!("{t} {m}")).collect();
            let _ = std::fs::write(args.out.join(format!("{tag}_sl_messages.log")), all.join("\n"));
        }

        // ---- pictures -------------------------------------------------------------------
        if let Some(f) = &mut fsr {
            let r = f.wait_for_presents();
            if r != 0 {
                rep.say(format!("FSR: wait for presents -> {} ({})", r, ffx::code_name(r)));
            }
            std::thread::sleep(Duration::from_millis(300));
            let st = (*f.shared).state.lock().unwrap();
            for (i, s) in st.slots.iter().enumerate() {
                let Some((fp, format)) = s.filled else {
                    rep.say(format!(
                        "FSR: no {} frame was shown for source frame {}",
                        if s.generated { "generated" } else { "real" },
                        s.frame_id
                    ));
                    continue;
                };
                let bgra = format == DXGI_FORMAT_B8G8R8A8_UNORM || format == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;
                if !(bgra || format == DXGI_FORMAT_R8G8B8A8_UNORM || format == DXGI_FORMAT_R8G8B8A8_UNORM_SRGB) {
                    rep.say(format!("FSR: captured frame has format {} which the probe does not decode", format.0));
                    continue;
                }
                let mut ptr: *mut c_void = null_mut();
                if s.buffer.Map(0, None, Some(&mut ptr)).is_err() {
                    continue;
                }
                let (cw, ch) = (fp.Footprint.Width as usize, fp.Footprint.Height as usize);
                let mut rgb = vec![0u8; cw * ch * 3];
                for y in 0..ch {
                    let row = std::slice::from_raw_parts(
                        (ptr as *const u8).add(fp.Offset as usize + y * fp.Footprint.RowPitch as usize),
                        cw * 4,
                    );
                    for x in 0..cw {
                        let (r, b) = if bgra { (row[x * 4 + 2], row[x * 4]) } else { (row[x * 4], row[x * 4 + 2]) };
                        rgb[(y * cw + x) * 3..(y * cw + x) * 3 + 3].copy_from_slice(&[r, row[x * 4 + 1], b]);
                    }
                }
                s.buffer.Unmap(0, None);
                let label = format!("cb{}_{}{}", i / 3, if s.generated { "gen" } else { "real" }, s.frame_id);
                let around = if s.generated { s.frame_id as f64 - 0.5 } else { s.frame_id as f64 };
                captures.push((label, around, Img { w: cw, h: ch, rgb }));
            }
        }
        {
            let mut judge = Judge {
                scene: &scene,
                w: w as usize,
                h: h as usize,
                cache: HashMap::new(),
                scratch: vec![0u8; SRC_W * SRC_H * 4],
                out: args.out.clone(),
                tag: tag.clone(),
            };
            rep.say("--- captured frames against the clip (PSNR in dB, higher is closer) ---");
            let mut saved_full = 0;
            for (label, around, img) in &captures {
                if img.w != w as usize || img.h != h as usize {
                    rep.say(format!("{label}: capture is {}x{}, expected {}x{}", img.w, img.h, w, h));
                    continue;
                }
                let full = saved_full < 3;
                saved_full += 1;
                rep.say(judge.judge(img, label, *around, full));
            }
            if captures.is_empty() {
                rep.say("no frames captured");
            }
        }
        rep.say(format!("pictures and logs: {}", args.out.display()));
        let _ = std::fs::write(args.out.join(format!("{tag}_report.txt")), rep.lines.join("\r\n"));

        // ---- teardown -------------------------------------------------------------------
        // Everything worth knowing is on disk; if a vendor library hangs on the way out the
        // process still ends.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(15));
            eprintln!("fgprobe: teardown took more than 15 s; exiting");
            std::process::exit(0);
        });
        let _ = gfx.wait_idle();
        if let Some(mut f) = fsr.take() {
            f.configure_frame(&swap, false, false, false, w, h, frames_done);
            f.wait_for_presents();
            let mut ctx = f.fg_context;
            let r = (f.destroy)(&mut ctx, null());
            backbuffers.clear();
            drop(swap);
            let mut sctx = f.swap_context;
            let r2 = (f.destroy)(&mut sctx, null());
            println!("FSR: contexts destroyed ({}, {})", ffx::code_name(r), ffx::code_name(r2));
        } else if let Some(s) = sl.take() {
            if let Some(set) = s.dlssg_set_options {
                dlss_options.mode = sl::DLSSG_MODE_OFF;
                dlss_options.flags = 0;
                set(&s.viewport, &dlss_options);
            }
            let _ = gfx.wait_idle();
            backbuffers.clear();
            drop(swap);
            let r = (s.shutdown)();
            println!("DLSS: slShutdown -> {} ({})", r, sl::result_name(r));
        } else {
            backbuffers.clear();
            drop(swap);
        }
        drop(gfx);
        // Hand the foreground back to whatever had it.
        if args.focus && GetForegroundWindow() == top && !previous_foreground.is_invalid() {
            let _ = SetForegroundWindow(previous_foreground);
        }
        let _ = DestroyWindow(top);
        pump();
        println!("fgprobe: done");
        // Vendor threads may still be winding down; leave without running their atexit paths
        // into objects we have already released.
        std::process::exit(0);
    }
}
