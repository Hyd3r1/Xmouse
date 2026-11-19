//! Frame timing for the PC Play picture: which stream frames are repeats, how fast the game
//! really renders, and when a real or an in-between ("Smooth motion") frame goes on screen.
//!
//! Everything here is plain arithmetic on arrival times (seconds on one clock) and on small
//! luma thumbnails, so it can be tested without a console or a graphics card.

use std::collections::VecDeque;

/// Every `SIG_STEP`th pixel in both directions goes into a frame's signature.
pub const SIG_STEP: usize = 8;

/// A sparse luma thumbnail of a frame, enough to tell "the same picture again" from "moved".
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    w: usize,
    h: usize,
    data: Vec<u8>,
}

impl Signature {
    /// From a luma plane (the first `w`×`h` bytes of an NV12 frame).
    pub fn from_luma(y: &[u8], w: usize, h: usize) -> Self {
        let (sw, sh) = (w.div_ceil(SIG_STEP), h.div_ceil(SIG_STEP));
        let mut data = Vec::with_capacity(sw * sh);
        if y.len() >= w * h {
            for row in (0..h).step_by(SIG_STEP) {
                let line = &y[row * w..row * w + w];
                data.extend(line.iter().step_by(SIG_STEP));
            }
        }
        Self { w: sw, h: sh, data }
    }

    /// From RGBA pixels (the CPU colour path).
    pub fn from_rgba(px: &[egui::Color32], w: usize, h: usize) -> Self {
        let (sw, sh) = (w.div_ceil(SIG_STEP), h.div_ceil(SIG_STEP));
        let mut data = Vec::with_capacity(sw * sh);
        if px.len() >= w * h {
            for row in (0..h).step_by(SIG_STEP) {
                let line = &px[row * w..row * w + w];
                data.extend(line.iter().step_by(SIG_STEP).map(|c| {
                    ((c.r() as u32 + 2 * c.g() as u32 + c.b() as u32) / 4) as u8
                }));
            }
        }
        Self { w: sw, h: sh, data }
    }

    /// (mean absolute difference in 8-bit levels, share of samples that changed clearly).
    /// `None` when the two aren't the same size.
    pub fn diff(&self, other: &Signature) -> Option<(f32, f32)> {
        if self.w != other.w || self.h != other.h || self.data.len() != other.data.len() {
            return None;
        }
        if self.data.is_empty() {
            return Some((0.0, 0.0));
        }
        let (mut sum, mut changed) = (0u64, 0u64);
        for (a, b) in self.data.iter().zip(&other.data) {
            let d = a.abs_diff(*b) as u64;
            sum += d;
            changed += (d > CHANGED_LEVELS) as u64;
        }
        let n = self.data.len() as f32;
        Some((sum as f32 / n, changed as f32 / n))
    }
}

/// A sample that moved by more than this many levels really changed (a re-encoded repeat only
/// wobbles by a level or two).
const CHANGED_LEVELS: u64 = 8;
/// A repeat: almost no average difference and hardly any sample that really changed.
const DUP_MAD: f32 = 0.5;
const DUP_CHANGED: f32 = 0.003;
/// ...or clearly tiny next to what real frames of this game have been differing by.
const DUP_RELATIVE: f32 = 0.12;
const DUP_RELATIVE_MAD: f32 = 1.2;
const DUP_RELATIVE_CHANGED: f32 = 0.01;
/// A scene cut: most of the picture is different. Set high on purpose: a missed cut only gives
/// one cross-faded frame, a false one only skips one in-between frame.
const CUT_MAD: f32 = 45.0;
const CUT_CHANGED: f32 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Nothing to compare with (first frame, or the stream changed size).
    First,
    /// A new picture from the game.
    Unique,
    /// The picture before it, sent again (the game renders slower than the stream runs).
    Duplicate,
    /// A new picture that has nothing to do with the one before.
    Cut,
}

/// Tells repeated frames from new ones.
#[derive(Default)]
pub struct DupDetector {
    /// The last *new* frame. Repeats are compared with it, not with each other, so a slow
    /// fade can't hide as an endless run of "repeats": its difference adds up.
    last: Option<Signature>,
    /// Typical difference between new frames lately.
    unique_mad: f32,
}

impl DupDetector {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn classify(&mut self, sig: Signature) -> Kind {
        let Some((mad, changed)) = self.last.as_ref().and_then(|l| l.diff(&sig)) else {
            self.last = Some(sig);
            self.unique_mad = 0.0;
            return Kind::First;
        };
        let dup = (mad < DUP_MAD && changed < DUP_CHANGED)
            || (mad < DUP_RELATIVE * self.unique_mad
                && mad < DUP_RELATIVE_MAD
                && changed < DUP_RELATIVE_CHANGED);
        if dup {
            return Kind::Duplicate;
        }
        self.last = Some(sig);
        if mad > CUT_MAD && changed > CUT_CHANGED {
            return Kind::Cut;
        }
        self.unique_mad = if self.unique_mad == 0.0 {
            mad
        } else {
            self.unique_mad * 0.8 + mad * 0.2
        };
        Kind::Unique
    }
}

const STREAM_WINDOW: usize = 16;
const CONTENT_WINDOW: usize = 8;

/// Measures the stream's frame interval and the game's own ("content") frame interval.
#[derive(Default)]
pub struct Cadence {
    last_frame: Option<f64>,
    last_unique: Option<f64>,
    stream: VecDeque<f64>,
    content: VecDeque<f64>,
}

fn median(v: &VecDeque<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s: Vec<f64> = v.iter().copied().collect();
    s.sort_by(|a, b| a.total_cmp(b));
    Some(s[s.len() / 2])
}

impl Cadence {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A frame arrived at `t` seconds; `unique` = it is a new picture.
    pub fn on_frame(&mut self, t: f64, unique: bool) {
        if let Some(prev) = self.last_frame {
            push_capped(&mut self.stream, t - prev, STREAM_WINDOW);
        }
        self.last_frame = Some(t);
        if unique {
            if let Some(prev) = self.last_unique {
                push_capped(&mut self.content, t - prev, CONTENT_WINDOW);
            }
            self.last_unique = Some(t);
        }
    }

    /// Typical time between stream frames.
    pub fn stream_interval(&self) -> Option<f64> {
        median(&self.stream)
    }

    /// Time between new pictures, once it is steady: at least four in a row measured, and
    /// three quarters of the recent ones within a quarter of the typical value. A game that
    /// alternates (say 40 fps inside 60) never gets steady, and in-between frames can't be
    /// placed evenly there anyway.
    pub fn content_interval(&self) -> Option<f64> {
        let m = median(&self.content)?;
        if self.content.len() < 4 || m <= 0.0 {
            return None;
        }
        let near = self
            .content
            .iter()
            .filter(|d| (**d - m).abs() <= m * 0.25)
            .count();
        (near * 4 >= self.content.len() * 3).then_some(m)
    }
}

fn push_capped(q: &mut VecDeque<f64>, v: f64, cap: usize) {
    if v > 0.0 && v < 1.0 {
        q.push_back(v);
        while q.len() > cap {
            q.pop_front();
        }
    }
}

/// Why a new picture was shown as it is, without an in-between frame before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// No earlier picture to move from (start, resize, toggled, a frame was dropped).
    NoHistory,
    SceneCut,
    /// The game's frame rate isn't steady (yet).
    Unsteady,
    /// The game is too slow or too fast for in-between frames to help.
    OutOfRange,
    /// The display can't fit a frame between two game frames (60 fps game on a 60 Hz screen).
    DisplayTooSlow,
    /// The picture before this one is older than one game frame.
    Gap,
    /// The in-between frame's moment has already passed.
    Late,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Show {
    /// Show the new picture now.
    Real(Skip),
    /// Show the in-between frame now and the new picture at `real_at`.
    Mid { real_at: f64 },
}

/// What the scheduler needs to know about a new picture.
#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    /// When the frame was decoded.
    pub at: f64,
    /// When the UI got to it.
    pub now: f64,
    /// The steady content interval, if there is one (`Cadence::content_interval`).
    pub interval: Option<f64>,
    pub display_hz: f64,
    pub cut: bool,
    /// The picture before it is still there to move from.
    pub have_prev: bool,
}

/// Games slower than this would need in-between frames across too much motion.
const MAX_INTERVAL: f64 = 1.0 / 18.0;
/// Faster than this there is nothing left to smooth.
const MIN_INTERVAL: f64 = 1.0 / 130.0;

/// Decides when frames go on screen with Smooth motion on.
///
/// The one rule: a real frame is never held back by more than half the game's frame time.
/// When picture B arrives at `t`, the frame half-way between A and B is shown at once and B
/// itself at `t + T/2`. For a 30 fps game in a 60 fps stream that is exactly when B's repeat
/// arrives, so the stream paces itself.
#[derive(Default)]
pub struct Pacer {
    /// The real frame waiting behind the in-between frame on screen: when it is due.
    pending: Option<f64>,
    last_unique: Option<f64>,
    /// The content interval the pending frame was scheduled with.
    interval: f64,
}

impl Pacer {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// When the waiting real frame is due, if one is waiting.
    pub fn pending(&self) -> Option<f64> {
        self.pending
    }

    /// A new picture arrived. A real frame still waiting from before is dropped from the
    /// schedule: the caller shows either the new in-between frame or the new picture, both
    /// of which are newer.
    pub fn on_unique(&mut self, a: Arrival) -> Show {
        let prev = self.last_unique.replace(a.at);
        self.pending = None;
        let skip = |why| Show::Real(why);
        if !a.have_prev {
            return skip(Skip::NoHistory);
        }
        if a.cut {
            return skip(Skip::SceneCut);
        }
        let Some(t) = a.interval else {
            return skip(Skip::Unsteady);
        };
        if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&t) {
            return skip(Skip::OutOfRange);
        }
        // Two frames per game frame have to fit on the display (with some slack for a
        // display that runs a little under its nominal rate).
        if a.display_hz * t < 2.0 * 0.92 {
            return skip(Skip::DisplayTooSlow);
        }
        match prev {
            Some(p) if a.at - p <= t * 1.6 => {}
            _ => return skip(Skip::Gap),
        }
        if a.now - a.at > t * 0.25 {
            return skip(Skip::Late);
        }
        self.interval = t;
        let real_at = a.at + t * 0.5;
        self.pending = Some(real_at);
        Show::Mid { real_at }
    }

    /// A repeat arrived at `at`. True: show the waiting real frame now (the repeat is the
    /// stream's own tick for it, even if it came a little early).
    pub fn on_duplicate(&mut self, at: f64) -> bool {
        match self.pending {
            Some(due) if at >= due - self.interval * 0.25 => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }

    /// Called every UI frame. True: the waiting real frame is due, show it now.
    pub fn poll(&mut self, now: f64) -> bool {
        match self.pending {
            // (A millisecond early beats a whole UI frame late.)
            Some(due) if now >= due - 0.001 => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

/// Counts for the periodic "stream rate vs game rate" log line.
#[derive(Default)]
pub struct StreamStats {
    since: Option<f64>,
    frames: u32,
    unique: u32,
    pub generated: u32,
    pub skipped: [u32; 7],
}

impl StreamStats {
    pub fn on_frame(&mut self, t: f64, kind: Kind) {
        self.since.get_or_insert(t);
        self.frames += 1;
        self.unique += (kind != Kind::Duplicate) as u32;
    }

    pub fn on_show(&mut self, show: Show) {
        match show {
            Show::Mid { .. } => self.generated += 1,
            Show::Real(why) => self.skipped[why as usize] += 1,
        }
    }

    /// Every `every` seconds: (stream fps, unique fps) of the window that just ended, then
    /// starts a new one. The caller reads `generated` / `skipped` before they are cleared by
    /// `clear_counts`.
    pub fn report(&mut self, now: f64, every: f64) -> Option<(f64, f64)> {
        let since = self.since?;
        let secs = now - since;
        if secs < every {
            return None;
        }
        let out = (self.frames as f64 / secs, self.unique as f64 / secs);
        self.since = Some(now);
        self.frames = 0;
        self.unique = 0;
        Some(out)
    }

    pub fn clear_counts(&mut self) {
        self.generated = 0;
        self.skipped = [0; 7];
    }

    /// The most common reason in-between frames were skipped, with its count.
    pub fn main_skip(&self) -> Option<(Skip, u32)> {
        const ALL: [Skip; 7] = [
            Skip::NoHistory,
            Skip::SceneCut,
            Skip::Unsteady,
            Skip::OutOfRange,
            Skip::DisplayTooSlow,
            Skip::Gap,
            Skip::Late,
        ];
        ALL.into_iter()
            .map(|s| (s, self.skipped[s as usize]))
            .filter(|(_, n)| *n > 0)
            .max_by_key(|(_, n)| *n)
    }
}

/// "30", "60"... for the log: the common frame rate nearest to a measured one.
pub fn nominal_fps(fps: f64) -> u32 {
    const COMMON: [u32; 8] = [20, 24, 30, 40, 45, 50, 60, 120];
    COMMON
        .into_iter()
        .min_by(|a, b| (*a as f64 - fps).abs().total_cmp(&(*b as f64 - fps).abs()))
        .unwrap_or(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F60: f64 = 1.0 / 60.0;

    /// A 320x180 luma picture: a gradient with a bright bar at `shift`.
    fn picture(shift: usize) -> Vec<u8> {
        let (w, h) = (320usize, 180usize);
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for x in 0..w {
                let bar = ((x + w - shift % w) % w) < 60;
                y[row * w + x] = if bar { 200 } else { (40 + row / 3 + x / 8) as u8 };
            }
        }
        y
    }

    /// The picture again, as a re-encode would deliver it: a level of wobble here and there.
    fn wobble(y: &[u8], seed: u32) -> Vec<u8> {
        let mut s = seed.wrapping_mul(2654435761).max(1);
        y.iter()
            .map(|v| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                match s % 16 {
                    0 => v.saturating_add(1),
                    1 => v.saturating_sub(1),
                    _ => *v,
                }
            })
            .collect()
    }

    fn sig(y: &[u8]) -> Signature {
        Signature::from_luma(y, 320, 180)
    }

    #[test]
    fn signature_samples_every_eighth_pixel() {
        let s = sig(&picture(0));
        assert_eq!((s.w, s.h, s.data.len()), (40, 23, 40 * 23));
        // A size the step doesn't divide: the last partial column/row still counts.
        let odd = Signature::from_luma(&vec![7u8; 13 * 9], 13, 9);
        assert_eq!((odd.w, odd.h, odd.data.len()), (2, 2, 4));
        // Too little data never panics.
        assert!(Signature::from_luma(&[1, 2, 3], 320, 180).data.is_empty());
    }

    #[test]
    fn rgba_and_luma_signatures_agree_on_grey() {
        let px = vec![egui::Color32::from_gray(90); 64 * 32];
        let a = Signature::from_rgba(&px, 64, 32);
        let b = Signature::from_luma(&vec![90u8; 64 * 32], 64, 32);
        assert_eq!(a, b);
    }

    #[test]
    fn repeated_frames_with_encoder_wobble_are_duplicates() {
        let mut d = DupDetector::default();
        let a = picture(0);
        assert_eq!(d.classify(sig(&a)), Kind::First);
        assert_eq!(d.classify(sig(&wobble(&a, 1))), Kind::Duplicate);
        assert_eq!(d.classify(sig(&wobble(&a, 2))), Kind::Duplicate);
        assert_eq!(d.classify(sig(&a)), Kind::Duplicate);
    }

    #[test]
    fn motion_is_a_new_frame_even_when_small() {
        let mut d = DupDetector::default();
        d.classify(sig(&picture(0)));
        // The bar moved 8 pixels: only two sample columns of forty change.
        assert_eq!(d.classify(sig(&picture(8))), Kind::Unique);
        assert_eq!(d.classify(sig(&wobble(&picture(8), 3))), Kind::Duplicate);
        assert_eq!(d.classify(sig(&picture(16))), Kind::Unique);
    }

    #[test]
    fn a_slow_fade_is_not_an_endless_run_of_duplicates() {
        let mut d = DupDetector::default();
        let base = picture(0);
        d.classify(sig(&base));
        // Brightness creeps up one level every other frame: each step alone looks like a
        // repeat, but the difference to the last new frame adds up.
        let mut uniques = 0;
        for i in 1..=20u8 {
            let f: Vec<u8> = base.iter().map(|v| v.saturating_add(i / 2)).collect();
            uniques += (d.classify(sig(&f)) == Kind::Unique) as u32;
        }
        assert!(uniques >= 5, "only {uniques} new frames seen in a fade");
    }

    #[test]
    fn a_different_picture_is_a_cut_and_a_new_size_starts_over() {
        let mut d = DupDetector::default();
        d.classify(sig(&vec![20u8; 320 * 180]));
        assert_eq!(d.classify(sig(&vec![220u8; 320 * 180])), Kind::Cut);
        // After the cut, the new scene's repeat is still a repeat.
        assert_eq!(d.classify(sig(&vec![220u8; 320 * 180])), Kind::Duplicate);
        assert_eq!(
            d.classify(Signature::from_luma(&vec![220u8; 160 * 90], 160, 90)),
            Kind::First
        );
    }

    #[test]
    fn cadence_finds_30_fps_content_in_a_60_fps_stream() {
        let mut c = Cadence::default();
        assert_eq!(c.content_interval(), None);
        for n in 0..24 {
            // (a little arrival jitter)
            let jitter = if n % 3 == 0 { 0.0015 } else { -0.0007 };
            c.on_frame(n as f64 * F60 + jitter, n % 2 == 0);
        }
        let s = c.stream_interval().unwrap();
        let t = c.content_interval().unwrap();
        assert!((s - F60).abs() < 0.003, "stream interval {s}");
        assert!((t - 2.0 * F60).abs() < 0.003, "content interval {t}");
    }

    #[test]
    fn cadence_needs_a_few_frames_and_tolerates_one_hitch() {
        let mut c = Cadence::default();
        for n in 0..4 {
            c.on_frame(n as f64 * F60, true);
        }
        assert_eq!(c.content_interval(), None, "three intervals are not enough");
        c.on_frame(4.0 * F60, true);
        assert!(c.content_interval().is_some());
        // One long frame (a hitch) doesn't unsettle it.
        c.on_frame(4.0 * F60 + 0.050, true);
        for n in 1..4 {
            c.on_frame(4.0 * F60 + 0.050 + n as f64 * F60, true);
        }
        let t = c.content_interval().unwrap();
        assert!((t - F60).abs() < 0.001);
    }

    #[test]
    fn cadence_is_not_steady_for_an_alternating_frame_rate() {
        // 40 fps inside 60: new, new, repeat → intervals of 1 and 2 stream frames.
        let mut c = Cadence::default();
        for n in 0..36 {
            c.on_frame(n as f64 * F60, n % 3 != 2);
        }
        assert_eq!(c.content_interval(), None);
    }

    fn arrival(at: f64, interval: f64, hz: f64) -> Arrival {
        Arrival {
            at,
            now: at + 0.001,
            interval: Some(interval),
            display_hz: hz,
            cut: false,
            have_prev: true,
        }
    }

    /// Runs a stream through the pacer the way the UI does and returns what went on screen:
    /// (time, 'M' for an in-between frame or 'R' for a real one, index of the real frame).
    fn run(pattern: &[bool], content: f64, hz: f64, ui_tick: f64) -> Vec<(f64, char, usize)> {
        let mut p = Pacer::default();
        let mut shown = Vec::new();
        let mut unique_no = 0usize;
        let mut have_prev = false;
        let end = pattern.len() as f64 * F60 + 0.1;
        let mut next_frame = 0usize;
        let mut t = 0.0;
        while t < end {
            // Frames that arrived by now (one at most per tick here).
            if next_frame < pattern.len() && next_frame as f64 * F60 <= t + 1e-9 {
                let at = next_frame as f64 * F60;
                if pattern[next_frame] {
                    unique_no += 1;
                    let a = Arrival {
                        at,
                        now: t,
                        interval: Some(content),
                        display_hz: hz,
                        cut: false,
                        have_prev,
                    };
                    have_prev = true;
                    match p.on_unique(a) {
                        Show::Mid { .. } => shown.push((t, 'M', unique_no)),
                        Show::Real(_) => shown.push((t, 'R', unique_no)),
                    }
                } else if p.on_duplicate(at) {
                    shown.push((t, 'R', unique_no));
                }
                next_frame += 1;
            }
            if p.poll(t) {
                shown.push((t, 'R', unique_no));
            }
            t += ui_tick;
        }
        shown
    }

    #[test]
    fn thirty_in_sixty_alternates_in_between_and_real_frames() {
        let pattern: Vec<bool> = (0..20).map(|n| n % 2 == 0).collect();
        let shown = run(&pattern, 2.0 * F60, 60.0, 0.001);
        // First picture: nothing to move from. Then M, R, M, R...
        assert_eq!(shown[0].1, 'R');
        let kinds: String = shown[1..].iter().map(|s| s.1).collect();
        assert_eq!(kinds, "MR".repeat(9), "{shown:?}");
        // Each real frame goes on screen about half a game frame after it arrived, and
        // never later (the rule), and the steps on screen are even (60 a second).
        for s in shown[1..].iter().filter(|s| s.1 == 'R') {
            let arrived = (s.2 - 1) as f64 * 2.0 * F60;
            let delay = s.0 - arrived;
            assert!(delay <= F60 + 0.0015, "real frame {} held {delay}s", s.2);
            assert!(delay >= F60 * 0.75 - 0.0015);
        }
        for pair in shown[1..].windows(2) {
            let step = pair[1].0 - pair[0].0;
            assert!((step - F60).abs() < 0.006, "uneven step {step}");
        }
    }

    #[test]
    fn sixty_fps_content_needs_a_fast_display() {
        let mut p = Pacer::default();
        p.on_unique(arrival(0.0, F60, 60.0));
        assert_eq!(
            p.on_unique(arrival(F60, F60, 60.0)),
            Show::Real(Skip::DisplayTooSlow)
        );
        assert_eq!(p.pending(), None);
        // 120 Hz (also when it really runs at 119.88) and faster: in-between frames, the
        // real one 8 ms later, shown by the timer since no repeat comes.
        for hz in [119.88, 120.0, 144.0, 360.0] {
            let mut p = Pacer::default();
            p.on_unique(arrival(0.0, F60, hz));
            let show = p.on_unique(arrival(F60, F60, hz));
            assert_eq!(show, Show::Mid { real_at: F60 * 1.5 }, "{hz} Hz");
            assert!(!p.poll(F60 + 0.004));
            assert!(p.poll(F60 * 1.5));
            assert!(!p.poll(F60 * 1.6), "shown once");
        }
        let all = run(&[true; 30], F60, 120.0, 0.001);
        let mids = all.iter().filter(|s| s.1 == 'M').count();
        let reals = all.iter().filter(|s| s.1 == 'R').count();
        assert_eq!((mids, reals), (29, 30), "{all:?}");
    }

    #[test]
    fn thirty_fps_content_fits_a_60_hz_display_but_not_a_slower_one() {
        let mut p = Pacer::default();
        p.on_unique(arrival(0.0, 2.0 * F60, 59.94));
        assert!(matches!(
            p.on_unique(arrival(2.0 * F60, 2.0 * F60, 59.94)),
            Show::Mid { .. }
        ));
        let mut p = Pacer::default();
        p.on_unique(arrival(0.0, 2.0 * F60, 50.0));
        assert_eq!(
            p.on_unique(arrival(2.0 * F60, 2.0 * F60, 50.0)),
            Show::Real(Skip::DisplayTooSlow)
        );
    }

    #[test]
    fn no_in_between_frame_without_history_on_a_cut_or_while_unsteady() {
        let t = 2.0 * F60;
        let mut p = Pacer::default();
        // The very first picture.
        let mut a = arrival(0.0, t, 60.0);
        a.have_prev = false;
        assert_eq!(p.on_unique(a), Show::Real(Skip::NoHistory));
        let mut a = arrival(t, t, 60.0);
        a.cut = true;
        assert_eq!(p.on_unique(a), Show::Real(Skip::SceneCut));
        let mut a = arrival(2.0 * t, t, 60.0);
        a.interval = None;
        assert_eq!(p.on_unique(a), Show::Real(Skip::Unsteady));
        // Too slow a game (10 fps) or an absurdly fast one.
        assert_eq!(
            p.on_unique(arrival(3.0 * t, 0.1, 60.0)),
            Show::Real(Skip::OutOfRange)
        );
        assert_eq!(
            p.on_unique(arrival(4.0 * t, 0.004, 360.0)),
            Show::Real(Skip::OutOfRange)
        );
        // And back to normal.
        assert!(matches!(p.on_unique(arrival(5.0 * t, t, 60.0)), Show::Mid { .. }));
    }

    #[test]
    fn a_late_or_stale_frame_is_shown_at_once() {
        let t = 2.0 * F60;
        let mut p = Pacer::default();
        p.on_unique(arrival(0.0, t, 60.0));
        // The UI got to this frame 12 ms after it arrived: the in-between frame's slot is
        // mostly gone, the real frame must not wait behind it.
        let mut late = arrival(t, t, 60.0);
        late.now = late.at + 0.012;
        assert_eq!(p.on_unique(late), Show::Real(Skip::Late));
        // The picture before this one is two game frames old (one was lost on the way).
        assert_eq!(
            p.on_unique(arrival(t * 3.0, t, 60.0)),
            Show::Real(Skip::Gap)
        );
        assert!(matches!(p.on_unique(arrival(t * 4.0, t, 60.0)), Show::Mid { .. }));
    }

    #[test]
    fn a_real_frame_never_waits_longer_than_half_a_game_frame() {
        // Whatever the stream does (steady, repeats missing, bursts), with the UI checking
        // every 2 ms a real frame is on screen at most T/2 (+ one check) after it arrived,
        // unless something newer replaced it first.
        let t = 2.0 * F60;
        let patterns: [&[bool]; 4] = [
            &[true, false, true, false, true, false, true, false],
            &[true, true, false, true, false, false, true, true],
            &[true, false, false, false, true, false, true, false],
            &[true, true, true, true, true, true, true, true],
        ];
        for pattern in patterns {
            let shown = run(pattern, t, 60.0, 0.002);
            let mut unique_no = 0;
            for (n, new) in pattern.iter().enumerate() {
                if !new {
                    continue;
                }
                unique_no += 1;
                let at = n as f64 * F60;
                // The next new picture's arrival replaces this one on the schedule.
                let next_at = pattern[n + 1..]
                    .iter()
                    .position(|u| *u)
                    .map(|k| (n + 1 + k) as f64 * F60)
                    .unwrap_or(f64::MAX);
                let deadline = at + t * 0.5 + 0.0021;
                let on_screen = shown
                    .iter()
                    .any(|s| s.2 == unique_no && s.1 == 'R' && s.0 <= deadline);
                assert!(
                    on_screen || next_at <= deadline,
                    "frame {unique_no} of {pattern:?} was held back: {shown:?}"
                );
            }
        }
    }

    #[test]
    fn a_repeat_shows_the_waiting_frame_only_near_its_time() {
        let t = 3.0 * F60; // 20 fps game: two repeats per picture
        let mut p = Pacer::default();
        p.on_unique(arrival(0.0, t, 60.0));
        let Show::Mid { real_at } = p.on_unique(arrival(t, t, 60.0)) else {
            panic!("expected an in-between frame");
        };
        assert!((real_at - (t + t * 0.5)).abs() < 1e-9);
        // The first repeat (16.7 ms in, due at 25 ms) is within a quarter frame: show.
        assert!(p.on_duplicate(t + F60));
        assert!(!p.on_duplicate(t + 2.0 * F60), "nothing left to show");
        // A repeat that comes far too early doesn't cut the in-between frame short.
        p.on_unique(arrival(2.0 * t, t, 60.0));
        assert!(!p.on_duplicate(2.0 * t + 0.002));
        assert!(p.pending().is_some());
        p.reset();
        assert_eq!(p.pending(), None);
    }

    #[test]
    fn stats_report_stream_and_unique_rates() {
        let mut s = StreamStats::default();
        for n in 0..300 {
            let kind = if n % 2 == 0 { Kind::Unique } else { Kind::Duplicate };
            s.on_frame(n as f64 * F60, kind);
            assert_eq!(s.report(n as f64 * F60, 5.0), None);
        }
        s.on_show(Show::Mid { real_at: 0.0 });
        s.on_show(Show::Real(Skip::Late));
        s.on_show(Show::Real(Skip::Late));
        s.on_show(Show::Real(Skip::SceneCut));
        let (stream, unique) = s.report(5.0, 5.0).unwrap();
        assert!((stream - 60.0).abs() < 0.5 && (unique - 30.0).abs() < 0.5);
        assert_eq!(s.generated, 1);
        assert_eq!(s.main_skip(), Some((Skip::Late, 2)));
        s.clear_counts();
        assert_eq!((s.generated, s.main_skip()), (0, None));
        assert_eq!(nominal_fps(unique), 30);
        assert_eq!(nominal_fps(59.6), 60);
        assert_eq!(nominal_fps(23.7), 24);
    }
}
