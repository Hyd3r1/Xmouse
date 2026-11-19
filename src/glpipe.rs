//! The PC Play picture pipeline: clean-up, in-between frames, upscaling, sharpening and colour,
//! as OpenGL fragment-shader passes in egui's own GL context (so it runs on any graphics card).
//!
//! Stream resolution:  upload → (YUV→RGB) → clean-up → [motion search → in-between frame]
//! Picture size:       upscale → sharpen → colour → the texture egui draws
//!
//! Passes whose switch is off are not run. Throughout, texture row 0 is the top of the picture
//! (as the frames arrive and as egui samples them), so nothing is ever flipped.
//!
//! Everything runs on the UI thread with egui's GL context current. egui sets up its own GL
//! state before it paints; `Pipeline` only has to leave the default framebuffer bound.

use anyhow::{anyhow, bail, Result};
use eframe::glow::{self, HasContext};

use crate::stream::VideoFrame;

/// How the picture should look (from the settings).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Look {
    /// Clean up compression, with this strength (0 = light … 1 = strong).
    pub cleanup: Option<f32>,
    /// Sharpen by this much (0–1).
    pub sharpen: Option<f32>,
    /// (vibrance, contrast, brightness); zeros change nothing.
    pub colour: Option<[f32; 3]>,
    /// Enlarge with the edge-aware scaler instead of plain filtering.
    pub edge_upscale: bool,
    /// Plain filtering is smooth (bilinear) rather than nearest-pixel.
    pub smooth: bool,
}

/// What `present` should put on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// The newest real frame.
    Current,
    /// The in-between frame made by `interpolate`.
    Mid,
    /// A texture from elsewhere that is already at the picture's size (RTX Video's output):
    /// `uv` is the part of it that holds the picture, `version` changes with its content.
    External {
        texture: glow::Texture,
        uv: [f32; 2],
        version: u64,
    },
}

const VS: &str = r#"
out vec2 vUv;
void main() {
    // One triangle that covers the target; no vertex buffers needed.
    vec2 p = vec2(float((gl_VertexID & 1) << 2) - 1.0, float((gl_VertexID & 2) << 1) - 1.0);
    vUv = p * 0.5 + 0.5;
    gl_Position = vec4(p, 0.0, 1.0);
}
"#;

/// NV12 (BT.709, limited range) → RGB.
const FS_CONVERT: &str = r#"
uniform sampler2D uY;
uniform sampler2D uC;
in vec2 vUv;
out vec4 o;
void main() {
    float y = (texture(uY, vUv).r - 16.0 / 255.0) * (255.0 / 219.0);
    // Chroma samples sit on the left edge of each pair of luma pixels, not between them:
    // a quarter chroma texel to the right lines them up.
    vec2 cs = vec2(textureSize(uC, 0));
    vec2 c = (texture(uC, vUv + vec2(0.25 / cs.x, 0.0)).rg - 128.0 / 255.0) * (255.0 / 224.0);
    o = vec4(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x, 1.0);
}
"#;

/// Clean up compression in one pass: a small edge-preserving denoise, block-edge softening and
/// gradient smoothing ("debanding") with dither.
const FS_CLEANUP: &str = r#"
uniform sampler2D uS;
uniform float uK;      // 0 light … 1 strong
uniform int uSeed;
in vec2 vUv;
out vec4 o;
const vec3 LW = vec3(0.2126, 0.7152, 0.0722);

vec3 tap(ivec2 p) {
    return texelFetch(uS, clamp(p, ivec2(0), textureSize(uS, 0) - 1), 0).rgb;
}

float rnd(ivec2 p, int salt) {
    uint h = uint(p.x) * 0x9E3779B1u ^ uint(p.y) * 0x85EBCA77u ^ uint(uSeed + salt) * 0xC2B2AE3Du;
    h ^= h >> 15; h *= 0x2C1B3C6Du;
    h ^= h >> 12; h *= 0x297A2D39u;
    h ^= h >> 15;
    return float(h) * (1.0 / 4294967296.0);
}

// The correction for one pixel next to a block boundary that runs across `dir`.
// `m` is the pixel's position inside its 8-pixel block along `dir`.
vec3 blockEdge(ivec2 p, ivec2 dir, int m) {
    if (m > 1 && m < 6) return vec3(0.0);
    // p0 | q0 are the two pixels at the boundary, p1 and q1 the ones behind them.
    ivec2 p0 = m >= 6 ? p + (7 - m) * dir : p - (m + 1) * dir;
    vec3 P1 = tap(p0 - dir), P0 = tap(p0), Q0 = tap(p0 + dir), Q1 = tap(p0 + 2 * dir);
    float step_ = abs(dot(Q0 - P0, LW));
    float inside = max(abs(dot(P0 - P1, LW)), abs(dot(Q1 - Q0, LW)));
    // A compression edge is a small step between two flat sides. A real edge is either a big
    // step or has detail next to it; those are left alone.
    float limit = mix(5.0, 9.0, uK) / 255.0;
    float w = (1.0 - smoothstep(0.6 * limit, limit, step_))
            * smoothstep(1.0, 2.0, step_ / (inside + 0.5 / 255.0));
    // Turn the step into a ramp over the four pixels.
    float amount = (m == 7 || m == 0) ? 0.375 : 0.125;
    float side = m >= 6 ? 1.0 : -1.0;
    return (Q0 - P0) * amount * side * w;
}

void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    vec3 c = tap(p);
    float lc = dot(c, LW);

    // Denoise: average with the neighbours that have nearly the same brightness. Across an
    // edge (text, outlines) the weights fall to zero, so edges stay as they are.
    float sigma = mix(2.5, 4.5, uK) / 255.0;
    float inv = 1.0 / (2.0 * sigma * sigma);
    vec3 acc = c;
    float ws = 1.0;
    for (int j = -1; j <= 1; j++) {
        for (int i = -1; i <= 1; i++) {
            if (i == 0 && j == 0) continue;
            vec3 s = tap(p + ivec2(i, j));
            float d = dot(s, LW) - lc;
            float w = exp(-d * d * inv) * ((i != 0 && j != 0) ? 0.7 : 1.0);
            acc += s * w;
            ws += w;
        }
    }
    vec3 r = acc / ws;

    // Deblock: the encoder works in 8x8 blocks counted from the top-left corner.
    r += blockEdge(p, ivec2(1, 0), p.x & 7) + blockEdge(p, ivec2(0, 1), p.y & 7);

    // Deband: in flat areas (skies, dark gradients) replace the pixel by the average of four
    // points around it, at a random angle and distance so no pattern shows. Anything that
    // isn't flat (one of the four differs) is left alone.
    float radius = mix(10.0, 16.0, uK) * (0.5 + 0.5 * rnd(p, 1));
    float ang = 6.2831853 * rnd(p, 2);
    vec2 d = vec2(cos(ang), sin(ang)) * radius;
    ivec2 d0 = ivec2(round(d)), d1 = ivec2(round(vec2(-d.y, d.x)));
    vec3 s0 = tap(p + d0), s1 = tap(p - d0), s2 = tap(p + d1), s3 = tap(p - d1);
    vec3 spread = max(max(abs(s0 - r), abs(s1 - r)), max(abs(s2 - r), abs(s3 - r)));
    float limit = mix(1.6, 2.6, uK) / 255.0;
    float flat_ = 1.0 - smoothstep(0.7 * limit, limit, max(spread.r, max(spread.g, spread.b)));
    r = mix(r, (s0 + s1 + s2 + s3) * 0.25, flat_);
    // The smoothed value falls between two 8-bit levels: half a level of noise keeps the
    // average right instead of snapping back into bands.
    r += (rnd(p, 3) - 0.5) / 255.0 * flat_;

    o = vec4(r, 1.0);
}
"#;

/// Half-resolution luma (the base of the pyramid the motion search runs on).
const FS_LUMA: &str = r#"
uniform sampler2D uS;
in vec2 vUv;
out vec4 o;
void main() {
    o = vec4(dot(texture(uS, vUv).rgb, vec3(0.2126, 0.7152, 0.0722)));
}
"#;

/// One level of the motion search. For every 8x8 block of the frame half-way between A and B
/// it looks for the motion v (in pixels, A→B) for which A, half a step back, looks most like
/// B, half a step ahead. Searching from the middle frame gives every one of its pixels exactly
/// one vector: no holes and no overlaps to patch up afterwards.
const FS_MOTION: &str = r#"
uniform sampler2D uA;      // luma pyramids
uniform sampler2D uB;
uniform sampler2D uPrev;   // the coarser level's result
uniform vec2 uRes;         // frame size in pixels
uniform float uLod;        // pyramid level to compare on
uniform float uTexel;      // size of that level's texel, in frame pixels
uniform float uStep;       // spacing of the candidates, in frame pixels
uniform int uRadius;       // candidates this many steps around the start
uniform int uWin;          // compare uWin x uWin samples
uniform bool uHasPrev;
uniform bool uSubpixel;
uniform float uPrior;      // how much better a match must be to move away from the start
uniform bool uNeighbours;  // also try the coarser level's answers next to this block
in vec2 vUv;
out vec4 o;

float cost(vec2 v) {
    vec2 h = 0.5 * v / uRes;
    float c0 = 0.5 * float(uWin - 1);
    float s = 0.0;
    for (int j = 0; j < uWin; j++) {
        for (int i = 0; i < uWin; i++) {
            vec2 q = vUv + (vec2(float(i), float(j)) - c0) * uTexel / uRes;
            s += abs(textureLod(uA, q - h, uLod).r - textureLod(uB, q + h, uLod).r);
        }
    }
    return s / float(uWin * uWin);
}

void main() {
    vec2 best = vec2(0.0);
    float bestCost;
    if (uHasPrev) {
        // Start from the coarser level's answer here. It saw a larger area, so it is the one
        // to believe where the picture repeats (windows, fences, tiles) and several motions
        // match equally well up close. A neighbour's answer (often the right one near the
        // outline of a moving object) and "nothing moved" (HUD, text over a moving scene)
        // have to match clearly better to replace it.
        vec2 t = 1.0 / vec2(textureSize(uPrev, 0));
        best = texture(uPrev, vUv).xy;
        bestCost = cost(best);
        for (int k = uNeighbours ? 0 : 4; k < 5; k++) {
            vec2 off = k == 0 ? vec2(t.x, 0.0) : k == 1 ? vec2(-t.x, 0.0)
                     : k == 2 ? vec2(0.0, t.y) : vec2(0.0, -t.y);
            vec2 v = k == 4 ? vec2(0.0) : texture(uPrev, vUv + off).xy;
            float c = cost(v) + (k == 4 ? 2.0 : 1.0) * uPrior;
            if (c < bestCost) { bestCost = c; best = v; }
        }
    } else {
        // The first look, over every motion in reach. Smaller motions are preferred when
        // several match alike: a regular pattern (tiles, a fence) looks the same one period
        // further on, and the short way is nearly always the true one. For the same reason
        // "nothing moved" wins in flat areas, where everything matches.
        bestCost = cost(best) - uPrior;
    }
    vec2 centre = best;
    for (int j = -uRadius; j <= uRadius; j++) {
        for (int i = -uRadius; i <= uRadius; i++) {
            if (i == 0 && j == 0) continue;
            vec2 v = centre + vec2(float(i), float(j)) * uStep;
            // A candidate has to be clearly better to pull the vector away: keeps the field
            // smooth where the picture gives little to hold on to (a block that only sees
            // one straight edge can't tell how far it slid along it).
            float c = cost(v) + uPrior * length(vec2(float(i), float(j)));
            if (c < bestCost) { bestCost = c; best = v; }
        }
    }
    float match_ = cost(best);
    if (uSubpixel) {
        // Fit a parabola through the cost on either side for the last fraction of a step.
        float l = cost(best - vec2(uStep, 0.0)), r = cost(best + vec2(uStep, 0.0));
        float u = cost(best - vec2(0.0, uStep)), d = cost(best + vec2(0.0, uStep));
        float dx = l - 2.0 * match_ + r, dy = u - 2.0 * match_ + d;
        if (dx > 1e-5) best.x += clamp(0.5 * (l - r) / dx, -0.5, 0.5) * uStep;
        if (dy > 1e-5) best.y += clamp(0.5 * (u - d) / dy, -0.5, 0.5) * uStep;
    }
    o = vec4(best, match_, 1.0);
}
"#;

/// Tidy the motion field: each block takes the vector, out of its own and its eight
/// neighbours', that agrees best with all the others (a vector median) and matched well. One
/// stray vector can't win; a real motion boundary stays sharp because nothing is averaged.
const FS_MEDIAN: &str = r#"
uniform sampler2D uF;
in vec2 vUv;
out vec4 o;
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    ivec2 hi = textureSize(uF, 0) - 1;
    vec3 v[9];
    for (int j = 0; j < 3; j++)
        for (int i = 0; i < 3; i++)
            v[j * 3 + i] = texelFetch(uF, clamp(p + ivec2(i - 1, j - 1), ivec2(0), hi), 0).xyz;
    int best = 4;
    float bestScore = 1e9;
    for (int a = 0; a < 9; a++) {
        float s = v[a].z * 60.0;
        for (int b = 0; b < 9; b++) s += length(v[a].xy - v[b].xy);
        if (s < bestScore) { bestScore = s; best = a; }
    }
    o = vec4(v[best], 1.0);
}
"#;

/// The in-between frame: A moved half-way forward and B half-way back, averaged. Where the
/// motion can't be trusted the two frames are simply blended (a soft double image for one
/// frame) instead of putting pixels in the wrong place.
const FS_INTERP: &str = r#"
uniform sampler2D uA;
uniform sampler2D uB;
uniform sampler2D uLA;
uniform sampler2D uLB;
uniform sampler2D uF;
uniform vec2 uRes;
uniform bool uShowField;
in vec2 vUv;
out vec4 o;
const vec3 LW = vec3(0.2126, 0.7152, 0.0722);

float inside(vec2 uv) {
    return (uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0) ? 1.0 : 0.0;
}

void main() {
    vec2 h = 0.5 * texture(uF, vUv).xy / uRes;
    vec2 ua = vUv - h, ub = vUv + h;
    vec3 a0 = texture(uA, vUv).rgb, b0 = texture(uB, vUv).rgb;
    vec3 a1 = texture(uA, ua).rgb, b1 = texture(uB, ub).rgb;
    // How well the two frames agree at this pixel if nothing moved, and if the block's
    // motion is right (per pixel, and over a 2x2 area from the pyramid's finest level).
    float still = 0.5 * abs(dot(a0 - b0, LW))
                + 0.5 * abs(textureLod(uLA, vUv, 0.0).r - textureLod(uLB, vUv, 0.0).r);
    float moved = 0.5 * abs(dot(a1 - b1, LW))
                + 0.5 * abs(textureLod(uLA, ua, 0.0).r - textureLod(uLB, ub, 0.0).r);
    vec3 plain = 0.5 * (a0 + b0);
    vec3 warped = 0.5 * (a1 + b1);
    // Trust the motion when the moved frames agree, except where "nothing moved" clearly
    // fits better: HUD and text that stand still over a moving scene stay put and sharp.
    // (Where both fit, the motion field is the steadier guide: choosing per pixel between
    // two near-equal answers would speckle.)
    float w = (1.0 - smoothstep(0.04, 0.12, moved)) * smoothstep(-0.03, -0.008, still - moved);
    // At the border one of the two has nothing to show (the camera is turning and that part
    // just came into view, or is leaving): take the frame that has it.
    float ia = inside(ua), ib = inside(ub);
    if (ia != ib) {
        warped = ia > 0.5 ? a1 : b1;
        w = 0.75;
    }
    o = vec4(mix(plain, warped, w), 1.0);
    if (uShowField) {
        // For tuning: the motion as colour (grey = none), brighter blue = poorer match.
        vec3 f = texture(uF, vUv).xyz;
        o = vec4(0.5 + f.xy / 64.0, f.z * 10.0, 1.0);
    }
}
"#;

/// XMOUSE_PICTURE_FIELD=1: in-between frames show the motion field instead (for tuning).
fn show_field() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("XMOUSE_PICTURE_FIELD").is_some())
}

/// Colour adjustments, shared by the two output passes.
const GLSL_GRADE: &str = r#"
uniform bool uColour;
uniform vec3 uCol;   // vibrance, contrast, brightness
const vec3 LW = vec3(0.2126, 0.7152, 0.0722);
vec3 grade(vec3 c) {
    if (!uColour) return c;
    float l = dot(c, LW);
    float hi = max(c.r, max(c.g, c.b)), lo = min(c.r, min(c.g, c.b));
    float sat = hi - lo;
    // Skin sits between red and yellow at moderate saturation; boosting it looks sunburnt.
    float skin = 0.0;
    if (c.r > c.g && c.g > c.b) {
        float hue = (c.g - c.b) / max(c.r - c.b, 1e-4);   // 0 = red … 1 = yellow
        skin = (1.0 - smoothstep(0.12, 0.34, abs(hue - 0.42)))
             * smoothstep(0.05, 0.2, sat) * (1.0 - smoothstep(0.5, 0.8, sat));
    }
    // Vibrance: lift muted colours the most, leave already vivid ones alone.
    float boost = uCol.x * (1.0 - sat) * (1.0 - 0.75 * skin);
    c = l + (c - l) * (1.0 + boost);
    c = (c - 0.5) * (1.0 + uCol.y) + 0.5;
    return c + uCol.z;
}
"#;

/// Scale the picture to its size on screen (and grade it, if no sharpening pass follows).
const FS_RESAMPLE: &str = r#"
uniform sampler2D uS;
uniform vec2 uScale;   // the part of uS that holds the picture
uniform int uMode;     // 0 = the texture's own (bilinear) filter, 1 = edge-aware, 2 = nearest
in vec2 vUv;
out vec4 o;

// A two-lobe windowed sinc.
float kernel(float r) {
    if (r < 1e-4) return 1.0;
    if (r >= 2.0) return 0.0;
    float a = 3.14159265 * r;
    return sin(a) * sin(a * 0.5) / (a * a * 0.5);
}

// Edge-aware enlarging. Twelve source pixels around the sample are weighted with a windowed
// sinc, but distance is measured in a frame turned to the local edge: stretched across it
// (the kernel gets narrower there, so the edge stays crisp) and squeezed along it (wider, so
// a diagonal comes out as a line instead of a staircase). In flat or noisy areas there is no
// clear direction and the kernel is round. The result is kept inside the range of the four
// nearest pixels, so no halos ring around edges.
vec3 edgeAware(vec2 uv) {
    ivec2 size = textureSize(uS, 0);
    vec2 pos = uv * vec2(size) - 0.5;
    vec2 base = floor(pos);
    vec2 f = pos - base;
    vec3 c[16];
    float l[16];
    for (int j = 0; j < 4; j++) {
        for (int i = 0; i < 4; i++) {
            ivec2 q = clamp(ivec2(base) + ivec2(i - 1, j - 1), ivec2(0), size - 1);
            vec3 s = texelFetch(uS, q, 0).rgb;
            c[j * 4 + i] = s;
            l[j * 4 + i] = dot(s, LW);
        }
    }
    // Brightness slope at the four pixels around the sample, blended to where the sample is.
    vec2 g00 = vec2(l[6] - l[4], l[9] - l[1]);
    vec2 g10 = vec2(l[7] - l[5], l[10] - l[2]);
    vec2 g01 = vec2(l[10] - l[8], l[13] - l[5]);
    vec2 g11 = vec2(l[11] - l[9], l[14] - l[6]);
    vec2 g = 0.5 * mix(mix(g00, g10, f.x), mix(g01, g11, f.x), f.y);
    float steep = length(g);
    vec2 n = steep > 1e-4 ? g / steep : vec2(1.0, 0.0);
    float edge = smoothstep(0.03, 0.15, steep);
    float across = mix(1.0, 1.2, edge), along = mix(1.0, 0.72, edge);
    vec3 acc = vec3(0.0);
    float ws = 0.0;
    for (int j = 0; j < 4; j++) {
        for (int i = 0; i < 4; i++) {
            if ((i == 0 || i == 3) && (j == 0 || j == 3)) continue;
            vec2 d = vec2(float(i - 1), float(j - 1)) - f;
            float da = dot(d, n) * across, dl = dot(d, vec2(-n.y, n.x)) * along;
            float w = kernel(sqrt(da * da + dl * dl));
            acc += c[j * 4 + i] * w;
            ws += w;
        }
    }
    vec3 lo = min(min(c[5], c[6]), min(c[9], c[10]));
    vec3 hi = max(max(c[5], c[6]), max(c[9], c[10]));
    return clamp(acc / max(ws, 1e-3), lo, hi);
}

void main() {
    vec2 uv = vUv * uScale;
    vec3 c;
    if (uMode == 1) c = edgeAware(uv);
    else if (uMode == 2) c = texelFetch(uS, ivec2(uv * vec2(textureSize(uS, 0))), 0).rgb;
    else c = texture(uS, uv).rgb;
    o = vec4(grade(c), 1.0);
}
"#;

/// Sharpen at the picture's size on screen, then grade.
const FS_SHARPEN: &str = r#"
uniform sampler2D uS;
uniform vec2 uScale;
uniform float uAmount;
in vec2 vUv;
out vec4 o;
void main() {
    vec2 px = 1.0 / vec2(textureSize(uS, 0));
    vec2 top = uScale - 0.5 * px;
    vec2 uv = vUv * uScale;
    vec3 c = texture(uS, uv).rgb;
    float lc = dot(c, LW);
    float ln = dot(texture(uS, clamp(uv - vec2(0.0, px.y), vec2(0.0), top)).rgb, LW);
    float ls = dot(texture(uS, clamp(uv + vec2(0.0, px.y), vec2(0.0), top)).rgb, LW);
    float lw = dot(texture(uS, clamp(uv - vec2(px.x, 0.0), vec2(0.0), top)).rgb, LW);
    float le = dot(texture(uS, clamp(uv + vec2(px.x, 0.0), vec2(0.0), top)).rgb, LW);
    float lo = min(lc, min(min(ln, ls), min(lw, le)));
    float hi = max(lc, max(max(ln, ls), max(lw, le)));
    float contrast = hi - lo;
    // What the pixel has that its neighbours don't: the detail to bring out.
    float detail = lc - 0.25 * (ln + ls + lw + le);
    // The strength follows the local contrast: full on soft detail, backing off on edges
    // that are strong already (pushing those gives halos), and off in nearly flat areas
    // (there is only noise to bring out).
    float gain = 2.0 * uAmount * (1.0 - smoothstep(0.15, 0.55, contrast))
               * smoothstep(0.004, 0.03, contrast);
    // A little overshoot reads as crisp; more than that as a halo.
    float sharp = clamp(lc + detail * gain, lo - 0.15 * contrast, hi + 0.15 * contrast);
    // The same shift on all three channels: brightness changes, colours don't fringe.
    o = vec4(grade(c + (sharp - lc)), 1.0);
}
"#;

struct Program(glow::Program);

impl Program {
    fn new(gl: &glow::Context, header: &str, name: &str, parts: &[&str]) -> Result<Self> {
        unsafe {
            let program = gl.create_program().map_err(|e| anyhow!("{name}: {e}"))?;
            let fs: String = parts.concat();
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, fs.as_str())] {
                let s = gl.create_shader(kind).map_err(|e| anyhow!("{name}: {e}"))?;
                gl.shader_source(s, &format!("{header}{src}"));
                gl.compile_shader(s);
                if !gl.get_shader_compile_status(s) {
                    let log = gl.get_shader_info_log(s);
                    gl.delete_shader(s);
                    gl.delete_program(program);
                    bail!("shader '{name}' did not compile: {log}");
                }
                gl.attach_shader(program, s);
                shaders.push(s);
            }
            gl.link_program(program);
            let ok = gl.get_program_link_status(program);
            let log = if ok {
                String::new()
            } else {
                gl.get_program_info_log(program)
            };
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !ok {
                gl.delete_program(program);
                bail!("shader '{name}' did not link: {log}");
            }
            Ok(Self(program))
        }
    }

    unsafe fn bind(&self, gl: &glow::Context) {
        gl.use_program(Some(self.0));
    }

    unsafe fn int(&self, gl: &glow::Context, name: &str, v: i32) {
        gl.uniform_1_i32(gl.get_uniform_location(self.0, name).as_ref(), v);
    }

    unsafe fn float(&self, gl: &glow::Context, name: &str, v: f32) {
        gl.uniform_1_f32(gl.get_uniform_location(self.0, name).as_ref(), v);
    }

    unsafe fn vec2(&self, gl: &glow::Context, name: &str, x: f32, y: f32) {
        gl.uniform_2_f32(gl.get_uniform_location(self.0, name).as_ref(), x, y);
    }

    unsafe fn vec3(&self, gl: &glow::Context, name: &str, v: [f32; 3]) {
        gl.uniform_3_f32(gl.get_uniform_location(self.0, name).as_ref(), v[0], v[1], v[2]);
    }

    /// Bind `tex` to texture unit `unit` and point the sampler `name` at it.
    unsafe fn texture(&self, gl: &glow::Context, name: &str, unit: u32, tex: glow::Texture) {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        self.int(gl, name, unit as i32);
    }
}

/// Pixel format of a texture: (internal format, format, type).
type Format = (u32, u32, u32);
const R8: Format = (glow::R8, glow::RED, glow::UNSIGNED_BYTE);
const RG8: Format = (glow::RG8, glow::RG, glow::UNSIGNED_BYTE);
const RGBA8: Format = (glow::RGBA8, glow::RGBA, glow::UNSIGNED_BYTE);
const RGBA16F: Format = (glow::RGBA16F, glow::RGBA, glow::HALF_FLOAT);

/// A texture, with a framebuffer when passes render into it.
struct Target {
    tex: glow::Texture,
    fbo: Option<glow::Framebuffer>,
    w: i32,
    h: i32,
}

impl Target {
    /// Make sure `slot` holds a `w`×`h` texture of this format. True when it was (re)made
    /// and so holds nothing yet. The texture object itself is kept across resizes (egui
    /// knows the output texture by its name).
    fn ensure(
        gl: &glow::Context,
        slot: &mut Option<Target>,
        (w, h): (i32, i32),
        format: Format,
        min_filter: u32,
        render: bool,
    ) -> Result<bool> {
        if slot.as_ref().is_some_and(|t| t.w == w && t.h == h) {
            return Ok(false);
        }
        unsafe {
            if slot.is_none() {
                let tex = gl.create_texture().map_err(|e| anyhow!("GL texture: {e}"))?;
                let fbo = if render {
                    Some(gl.create_framebuffer().map_err(|e| anyhow!("GL framebuffer: {e}"))?)
                } else {
                    None
                };
                *slot = Some(Target { tex, fbo, w: 0, h: 0 });
            }
            let t = slot.as_mut().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(t.tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                format.0 as i32,
                w,
                h,
                0,
                format.1,
                format.2,
                glow::PixelUnpackData::Slice(None),
            );
            let mag = if min_filter == glow::NEAREST {
                glow::NEAREST
            } else {
                glow::LINEAR
            };
            for (p, v) in [
                (glow::TEXTURE_MIN_FILTER, min_filter),
                (glow::TEXTURE_MAG_FILTER, mag),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }
            if min_filter == glow::LINEAR_MIPMAP_NEAREST {
                // Give the smaller levels storage now; they are filled after each render.
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            (t.w, t.h) = (w, h);
            if let Some(fbo) = t.fbo {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(t.tex),
                    0,
                );
                let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                if status != glow::FRAMEBUFFER_COMPLETE {
                    bail!("this graphics driver can't render to texture format {:#x}", format.0);
                }
            }
        }
        Ok(true)
    }

    unsafe fn upload(&self, gl: &glow::Context, format: Format, data: &[u8]) {
        gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
        gl.tex_sub_image_2d(
            glow::TEXTURE_2D,
            0,
            0,
            0,
            self.w,
            self.h,
            format.1,
            format.2,
            glow::PixelUnpackData::Slice(Some(data)),
        );
    }

    /// Run the bound program over the whole target.
    unsafe fn draw(&self, gl: &glow::Context) {
        gl.bind_framebuffer(glow::FRAMEBUFFER, self.fbo);
        gl.viewport(0, 0, self.w, self.h);
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
    }

    unsafe fn delete(self, gl: &glow::Context) {
        if let Some(f) = self.fbo {
            gl.delete_framebuffer(f);
        }
        gl.delete_texture(self.tex);
    }
}

/// One real frame on the GPU.
#[derive(Default)]
struct Slot {
    /// The frame in RGB, cleaned up if that is on.
    colour: Option<Target>,
    /// Half-size luma with a mip chain, for the motion search.
    luma: Option<Target>,
    valid: bool,
    luma_valid: bool,
}

/// GPU time of a group of passes, measured by the driver (desktop GL only).
struct Timer {
    query: Option<glow::Query>,
    waiting: bool,
    ms: f32,
}

/// Which group of passes a GPU time belongs to.
#[derive(Clone, Copy)]
enum Stage {
    Clean = 0,
    Motion = 1,
    Output = 2,
}

pub struct Pipeline {
    vao: glow::VertexArray,
    convert: Program,
    cleanup: Program,
    luma: Program,
    motion: Program,
    median: Program,
    interp: Program,
    resample: Program,
    sharpen: Program,
    /// Stream size.
    size: (i32, i32),
    y: Option<Target>,
    uv: Option<Target>,
    /// The converted (or uploaded RGBA) frame before clean-up.
    raw: Option<Target>,
    slots: [Slot; 2],
    cur: usize,
    /// Motion fields: two at 1/16 of the frame, two at 1/8.
    field_a: Option<Target>,
    field_a2: Option<Target>,
    field_b: Option<Target>,
    field_c: Option<Target>,
    mid: Option<Target>,
    mid_valid: bool,
    /// The upscaled picture before sharpening.
    scaled: Option<Target>,
    out: Option<Target>,
    out_id: Option<egui::TextureId>,
    /// What `out` currently shows, to skip identical work: (source, version, size, look).
    shown: Option<(u8, u64, [u32; 2], Look)>,
    version: u64,
    seed: i32,
    timers: [Timer; 3],
    /// Frame generation can't run on this driver (no float render targets); said once.
    pub motion_unavailable: bool,
}

/// The smallest stream the motion search makes sense on (its coarsest level is 1/32 size).
const MIN_MOTION_SIZE: (i32, i32) = (320, 180);

impl Pipeline {
    /// Needs egui's GL context to be current (true inside `App::ui`).
    pub fn new(gl: &glow::Context) -> Result<Self> {
        let v = gl.version();
        let header = if v.is_embedded {
            if v.major < 3 {
                bail!("OpenGL ES {}.{} is too old (3.0 needed)", v.major, v.minor);
            }
            "#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\n"
        } else {
            if (v.major, v.minor) < (3, 3) {
                bail!("OpenGL {}.{} is too old (3.3 needed)", v.major, v.minor);
            }
            "#version 330 core\n"
        };
        let prog = |name: &str, parts: &[&str]| Program::new(gl, header, name, parts);
        let pipeline = Self {
            vao: unsafe { gl.create_vertex_array() }.map_err(|e| anyhow!("GL vertex array: {e}"))?,
            convert: prog("convert", &[FS_CONVERT])?,
            cleanup: prog("clean-up", &[FS_CLEANUP])?,
            luma: prog("luma", &[FS_LUMA])?,
            motion: prog("motion", &[FS_MOTION])?,
            median: prog("median", &[FS_MEDIAN])?,
            interp: prog("in-between", &[FS_INTERP])?,
            resample: prog("resample", &[GLSL_GRADE, FS_RESAMPLE])?,
            sharpen: prog("sharpen", &[GLSL_GRADE, FS_SHARPEN])?,
            size: (0, 0),
            y: None,
            uv: None,
            raw: None,
            slots: Default::default(),
            cur: 0,
            field_a: None,
            field_a2: None,
            field_b: None,
            field_c: None,
            mid: None,
            mid_valid: false,
            scaled: None,
            out: None,
            out_id: None,
            shown: None,
            version: 0,
            seed: 0,
            timers: std::array::from_fn(|_| Timer {
                query: if v.is_embedded {
                    None
                } else {
                    unsafe { gl.create_query() }.ok()
                },
                waiting: false,
                ms: 0.0,
            }),
            motion_unavailable: false,
        };
        log::info!(
            "picture pipeline ready (OpenGL{} {}.{}, {})",
            if v.is_embedded { " ES" } else { "" },
            v.major,
            v.minor,
            unsafe { gl.get_parameter_string(glow::RENDERER) }
        );
        Ok(pipeline)
    }

    /// Smoothed GPU time in milliseconds of (clean-up, motion + in-between frame, output).
    pub fn gpu_ms(&self) -> [f32; 3] {
        [self.timers[0].ms, self.timers[1].ms, self.timers[2].ms]
    }

    /// True once a frame has been taken in (there is something to present).
    pub fn has_picture(&self) -> bool {
        self.slots[self.cur].valid
    }

    /// Changes whenever a frame is taken in or an in-between frame is made.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// True when the frame before the current one is still there to move from.
    pub fn has_prev(&self) -> bool {
        let (a, b) = (&self.slots[1 - self.cur], &self.slots[self.cur]);
        a.valid && a.luma_valid && b.valid && b.luma_valid
    }

    /// Forget the earlier frame (stream restart, switch toggled).
    pub fn reset_history(&mut self) {
        for s in &mut self.slots {
            s.valid = false;
            s.luma_valid = false;
        }
        self.mid_valid = false;
    }

    unsafe fn begin(&mut self, gl: &glow::Context, stage: Stage) {
        // egui leaves these on from its own painting; the passes write whole targets.
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::BLEND);
        gl.bind_vertex_array(Some(self.vao));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        let t = &mut self.timers[stage as usize];
        if let Some(q) = t.query {
            if t.waiting {
                // (From an earlier UI frame, so the result is there: this doesn't stall.)
                let ns = gl.get_query_parameter_u32(q, glow::QUERY_RESULT);
                let ms = ns as f32 / 1e6;
                t.ms = if t.ms == 0.0 { ms } else { t.ms * 0.9 + ms * 0.1 };
            }
            gl.begin_query(glow::TIME_ELAPSED, q);
            t.waiting = true;
        }
    }

    unsafe fn end(&mut self, gl: &glow::Context, stage: Stage) {
        if self.timers[stage as usize].query.is_some() {
            gl.end_query(glow::TIME_ELAPSED);
        }
        // Back to what egui expects to find.
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.bind_vertex_array(None);
        gl.use_program(None);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }

    /// Take in a decoded frame: upload, convert, clean up.
    ///
    /// `rotate`: this is a new picture, keep the one before it for the motion search.
    /// Otherwise it replaces the current one (a repeat of the same picture).
    /// `motion`: also prepare what the motion search needs.
    pub fn submit(
        &mut self,
        gl: &glow::Context,
        frame: &VideoFrame,
        cleanup: Option<f32>,
        rotate: bool,
        motion: bool,
    ) -> Result<()> {
        let (w, h) = match frame {
            VideoFrame::Rgba(img) => (img.size[0] as i32, img.size[1] as i32),
            VideoFrame::Nv12 { width, height, .. } => (*width as i32, *height as i32),
        };
        if w < 16 || h < 16 {
            bail!("unsupported frame size {w}x{h}");
        }
        if (w, h) != self.size {
            self.size = (w, h);
            self.reset_history();
            log::info!("picture pipeline: {w}x{h} stream");
        }
        if rotate {
            self.cur = 1 - self.cur;
        }
        let cur = self.cur;
        unsafe {
            self.begin(gl, Stage::Clean);
            let r = self.submit_passes(gl, frame, cleanup, motion);
            self.end(gl, Stage::Clean);
            if let Err(e) = r {
                self.slots[cur].valid = false;
                return Err(e);
            }
        }
        self.slots[cur].valid = true;
        self.version += 1;
        Ok(())
    }

    unsafe fn submit_passes(
        &mut self,
        gl: &glow::Context,
        frame: &VideoFrame,
        cleanup: Option<f32>,
        motion: bool,
    ) -> Result<()> {
        let size = self.size;
        let cur = self.cur;
        Target::ensure(gl, &mut self.slots[cur].colour, size, RGBA8, glow::LINEAR, true)?;
        if cleanup.is_some() {
            Target::ensure(gl, &mut self.raw, size, RGBA8, glow::LINEAR, true)?;
        }
        // Where the frame goes before clean-up (straight into its slot when that is off).
        let first = if cleanup.is_some() {
            self.raw.as_ref().unwrap()
        } else {
            self.slots[cur].colour.as_ref().unwrap()
        };
        match frame {
            VideoFrame::Rgba(img) => first.upload(gl, RGBA8, img.as_raw()),
            VideoFrame::Nv12 { data, .. } => {
                let (w, h) = (size.0 as usize, size.1 as usize);
                if w % 2 != 0 || h % 2 != 0 || data.len() < w * h * 3 / 2 {
                    bail!("unsupported NV12 frame ({w}x{h}, {} bytes)", data.len());
                }
                Target::ensure(gl, &mut self.y, size, R8, glow::NEAREST, false)?;
                Target::ensure(gl, &mut self.uv, (size.0 / 2, size.1 / 2), RG8, glow::LINEAR, false)?;
                let (y, uv) = (self.y.as_ref().unwrap(), self.uv.as_ref().unwrap());
                y.upload(gl, R8, &data[..w * h]);
                uv.upload(gl, RG8, &data[w * h..w * h * 3 / 2]);
                self.convert.bind(gl);
                self.convert.texture(gl, "uY", 0, y.tex);
                self.convert.texture(gl, "uC", 1, uv.tex);
                first.draw(gl);
            }
        }
        if let Some(k) = cleanup {
            self.seed = (self.seed + 1) & 0xffff;
            self.cleanup.bind(gl);
            self.cleanup.texture(gl, "uS", 0, self.raw.as_ref().unwrap().tex);
            self.cleanup.float(gl, "uK", k.clamp(0.0, 1.0));
            self.cleanup.int(gl, "uSeed", self.seed);
            self.slots[cur].colour.as_ref().unwrap().draw(gl);
        }
        self.slots[cur].luma_valid = false;
        if motion && !self.motion_unavailable && size.0 >= MIN_MOTION_SIZE.0 && size.1 >= MIN_MOTION_SIZE.1
        {
            let half = (size.0 / 2, size.1 / 2);
            Target::ensure(
                gl,
                &mut self.slots[cur].luma,
                half,
                R8,
                glow::LINEAR_MIPMAP_NEAREST,
                true,
            )?;
            let slot = &self.slots[cur];
            let luma = slot.luma.as_ref().unwrap();
            self.luma.bind(gl);
            self.luma.texture(gl, "uS", 0, slot.colour.as_ref().unwrap().tex);
            luma.draw(gl);
            gl.bind_texture(glow::TEXTURE_2D, Some(luma.tex));
            gl.generate_mipmap(glow::TEXTURE_2D);
            self.slots[cur].luma_valid = true;
        }
        Ok(())
    }

    /// Make the frame half-way between the previous and the current one (`Source::Mid`).
    /// Call only when `has_prev()`.
    pub fn interpolate(&mut self, gl: &glow::Context) -> Result<()> {
        if !self.has_prev() {
            bail!("no earlier frame to move from");
        }
        unsafe {
            self.begin(gl, Stage::Motion);
            let r = self.interpolate_passes(gl);
            self.end(gl, Stage::Motion);
            if r.is_err() {
                // Float render targets are the one thing here a driver may lack.
                self.motion_unavailable = true;
                self.mid_valid = false;
            }
            r?;
        }
        self.mid_valid = true;
        self.version += 1;
        Ok(())
    }

    unsafe fn interpolate_passes(&mut self, gl: &glow::Context) -> Result<()> {
        let (w, h) = self.size;
        let coarse = ((w + 15) / 16, (h + 15) / 16);
        let fine = ((w + 7) / 8, (h + 7) / 8);
        Target::ensure(gl, &mut self.field_a, coarse, RGBA16F, glow::LINEAR, true)?;
        Target::ensure(gl, &mut self.field_a2, coarse, RGBA16F, glow::LINEAR, true)?;
        Target::ensure(gl, &mut self.field_b, fine, RGBA16F, glow::LINEAR, true)?;
        Target::ensure(gl, &mut self.field_c, fine, RGBA16F, glow::LINEAR, true)?;
        Target::ensure(gl, &mut self.mid, self.size, RGBA8, glow::LINEAR, true)?;
        let (a, b) = (&self.slots[1 - self.cur], &self.slots[self.cur]);
        let (la, lb) = (a.luma.as_ref().unwrap().tex, b.luma.as_ref().unwrap().tex);
        let (fa, fa2, fb, fc) = (
            self.field_a.as_ref().unwrap(),
            self.field_a2.as_ref().unwrap(),
            self.field_b.as_ref().unwrap(),
            self.field_c.as_ref().unwrap(),
        );

        // Coarse to fine. The pyramid's level 0 is half size, so a texel of level n covers
        // 2·2ⁿ frame pixels. Every stage halves the spacing of the candidates:
        //   1/16-size luma: every motion up to ±96 px in steps of 16
        //   then ±8, ±4, ±2, ±1 around the best so far, with a sub-step fit at the end.
        // The compared area stays well larger than the block (48, 32, 16, then 12 pixels
        // across for an 8-pixel block): a small area on a regular texture matches in many
        // places. The finer the stage, the more it has to gain to leave the answer so far.
        // (level, texel, step, radius, window, prior, source field, destination)
        let m = &self.motion;
        m.bind(gl);
        m.texture(gl, "uA", 0, la);
        m.texture(gl, "uB", 1, lb);
        m.vec2(gl, "uRes", w as f32, h as f32);
        let stages: [(f32, f32, f32, i32, i32, f32, Option<&Target>, &Target); 5] = [
            (3.0, 16.0, 16.0, 6, 3, 0.0015, None, fa),
            (2.0, 8.0, 8.0, 1, 4, 0.003, Some(fa2), fb),
            (1.0, 4.0, 4.0, 1, 4, 0.004, Some(fb), fc),
            (1.0, 4.0, 2.0, 1, 4, 0.004, Some(fc), fb),
            (0.0, 2.0, 1.0, 1, 6, 0.005, Some(fb), fc),
        ];
        let last = stages.len() - 1;
        for (n, (lod, texel, step, radius, win, prior, prev, dst)) in stages.into_iter().enumerate() {
            m.float(gl, "uLod", lod);
            m.float(gl, "uTexel", texel);
            m.float(gl, "uStep", step);
            m.int(gl, "uRadius", radius);
            m.int(gl, "uWin", win);
            m.float(gl, "uPrior", prior);
            m.int(gl, "uHasPrev", prev.is_some() as i32);
            m.int(gl, "uSubpixel", (n == last) as i32);
            // (The last stage compares the most samples; its neighbours were tried before.)
            m.int(gl, "uNeighbours", (n != last) as i32);
            // (The sampler always needs a texture, used or not; never the one drawn into.)
            m.texture(gl, "uPrev", 2, prev.unwrap_or(fb).tex);
            dst.draw(gl);
            if n == 0 {
                // Tidy the first look before building on it: a stray vector here would
                // grow into a patch of 4x4 blocks further down.
                self.median.bind(gl);
                self.median.texture(gl, "uF", 0, fa.tex);
                fa2.draw(gl);
                m.bind(gl);
                m.texture(gl, "uA", 0, la);
            }
        }
        // Twice: the first pass removes single stray vectors, the second what was left of
        // small clusters.
        self.median.bind(gl);
        self.median.texture(gl, "uF", 0, fc.tex);
        fb.draw(gl);
        self.median.texture(gl, "uF", 0, fb.tex);
        fc.draw(gl);
        let field = fc;

        let i = &self.interp;
        i.bind(gl);
        i.texture(gl, "uA", 0, a.colour.as_ref().unwrap().tex);
        i.texture(gl, "uB", 1, b.colour.as_ref().unwrap().tex);
        i.texture(gl, "uLA", 2, la);
        i.texture(gl, "uLB", 3, lb);
        i.texture(gl, "uF", 4, field.tex);
        i.vec2(gl, "uRes", w as f32, h as f32);
        i.int(gl, "uShowField", show_field() as i32);
        self.mid.as_ref().unwrap().draw(gl);
        Ok(())
    }

    /// Put `src` on screen at `target` pixels: scale, sharpen, grade. Returns the texture for
    /// egui and the part of it (UV size) that holds the picture. Does nothing new when called
    /// again with the same picture, size and look.
    pub fn present(
        &mut self,
        gl: &glow::Context,
        frame: &mut eframe::Frame,
        src: Source,
        target: [u32; 2],
        look: Look,
    ) -> Result<(egui::TextureId, egui::Vec2)> {
        let target = [target[0].clamp(16, 8192), target[1].clamp(16, 8192)];
        let (kind, version) = match src {
            Source::Current => (0, self.version),
            Source::Mid => (1, self.version),
            Source::External { version, .. } => (2, version),
        };
        let key = (kind, version, target, look);
        if let (Some(id), true) = (self.out_id, self.shown == Some(key)) {
            return Ok((id, egui::vec2(1.0, 1.0)));
        }
        let (tex, src_size, scale) = match src {
            Source::Current => {
                let s = &self.slots[self.cur];
                match (&s.colour, s.valid) {
                    (Some(t), true) => (t.tex, Some((t.w, t.h)), [1.0, 1.0]),
                    _ => bail!("no picture yet"),
                }
            }
            Source::Mid => match (&self.mid, self.mid_valid) {
                (Some(t), true) => (t.tex, Some((t.w, t.h)), [1.0, 1.0]),
                _ => bail!("no in-between frame"),
            },
            Source::External { texture, uv, .. } => (texture, None, uv),
        };
        let size = (target[0] as i32, target[1] as i32);
        unsafe {
            self.begin(gl, Stage::Output);
            let r = self.present_passes(gl, tex, src_size, scale, size, look);
            self.end(gl, Stage::Output);
            r?;
        }
        let out = self.out.as_ref().unwrap();
        let id = *self
            .out_id
            .get_or_insert_with(|| frame.register_native_glow_texture(out.tex));
        self.shown = Some(key);
        Ok((id, egui::vec2(1.0, 1.0)))
    }

    unsafe fn present_passes(
        &mut self,
        gl: &glow::Context,
        tex: glow::Texture,
        src_size: Option<(i32, i32)>,
        scale: [f32; 2],
        size: (i32, i32),
        look: Look,
    ) -> Result<()> {
        Target::ensure(gl, &mut self.out, size, RGBA8, glow::LINEAR, true)?;
        // A source of our own is scaled here; one from outside already has the right size.
        let resize = src_size.is_some_and(|s| s != size);
        let mode = match src_size {
            Some((w, h)) if resize => {
                if look.edge_upscale && (size.0 > w || size.1 > h) {
                    1
                } else if look.smooth {
                    0
                } else {
                    2
                }
            }
            _ => 0,
        };
        let colour = look.colour.unwrap_or_default();
        let grade = |p: &Program, on: bool| {
            p.int(gl, "uColour", (on && look.colour.is_some()) as i32);
            p.vec3(gl, "uCol", colour);
        };
        let Some(amount) = look.sharpen else {
            let p = &self.resample;
            p.bind(gl);
            p.texture(gl, "uS", 0, tex);
            p.vec2(gl, "uScale", scale[0], scale[1]);
            p.int(gl, "uMode", mode);
            grade(p, true);
            self.out.as_ref().unwrap().draw(gl);
            return Ok(());
        };
        // Sharpening works on screen pixels, so the picture is scaled first.
        let (tex, scale) = if resize {
            Target::ensure(gl, &mut self.scaled, size, RGBA8, glow::LINEAR, true)?;
            let p = &self.resample;
            p.bind(gl);
            p.texture(gl, "uS", 0, tex);
            p.vec2(gl, "uScale", scale[0], scale[1]);
            p.int(gl, "uMode", mode);
            grade(p, false);
            let scaled = self.scaled.as_ref().unwrap();
            scaled.draw(gl);
            (scaled.tex, [1.0, 1.0])
        } else {
            (tex, scale)
        };
        let p = &self.sharpen;
        p.bind(gl);
        p.texture(gl, "uS", 0, tex);
        p.vec2(gl, "uScale", scale[0], scale[1]);
        p.float(gl, "uAmount", amount.clamp(0.0, 1.0));
        grade(p, true);
        self.out.as_ref().unwrap().draw(gl);
        Ok(())
    }

    /// Copy the texture `present` last filled back to the CPU as RGBA rows, top first
    /// (for checking the output by eye in a test run).
    pub fn read_output(&self, gl: &glow::Context) -> Option<(u32, u32, Vec<u8>)> {
        let out = self.out.as_ref()?;
        let mut px = vec![0u8; (out.w * out.h * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, out.fbo);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.read_pixels(
                0,
                0,
                out.w,
                out.h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut px)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        Some((out.w as u32, out.h as u32, px))
    }

    /// Free the GPU objects (the context must be current). egui keeps the output texture's
    /// id; it is not drawn again.
    pub fn destroy(mut self, gl: &glow::Context) {
        unsafe {
            let slots = std::mem::take(&mut self.slots);
            let targets = [
                self.y.take(),
                self.uv.take(),
                self.raw.take(),
                self.field_a.take(),
                self.field_a2.take(),
                self.field_b.take(),
                self.field_c.take(),
                self.mid.take(),
                self.scaled.take(),
                self.out.take(),
            ]
            .into_iter()
            .chain(slots.into_iter().flat_map(|s| [s.colour, s.luma]));
            for t in targets.flatten() {
                t.delete(gl);
            }
            for p in [
                &self.convert,
                &self.cleanup,
                &self.luma,
                &self.motion,
                &self.median,
                &self.interp,
                &self.resample,
                &self.sharpen,
            ] {
                gl.delete_program(p.0);
            }
            for t in &self.timers {
                if let Some(q) = t.query {
                    gl.delete_query(q);
                }
            }
            gl.delete_vertex_array(self.vao);
        }
    }
}
