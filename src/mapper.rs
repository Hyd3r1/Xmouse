//! The mapping engine: turns mouse/keyboard events into Xbox controller frames.

use std::collections::{HashMap, HashSet};

use crate::profile::{Action, BindMode, Binding, DeadzoneShape, Profile, Stick};

pub const TAP_MS: f64 = 80.0;
/// Release time between two taps of the same button (fast wheel notches), so each one counts.
pub const TAP_GAP_MS: f64 = 40.0;
pub const TURBO_PERIOD_MS: f64 = 120.0;

/// One controller state, browser axis convention (stick up = -1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PadFrame {
    pub buttons: u16,
    pub lx: f32,
    pub ly: f32,
    pub rx: f32,
    pub ry: f32,
    pub lt: f32,
    pub rt: f32,
}

impl PadFrame {
    pub fn pressed(&self, a: Action) -> bool {
        match a {
            Action::LeftTrigger => self.lt > 0.0,
            Action::RightTrigger => self.rt > 0.0,
            _ => a.button_bit() != 0 && self.buttons & a.button_bit() != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct V2 {
    x: f32,
    y: f32,
}

pub struct Mapper {
    profile: Profile,
    held: HashMap<String, f64>,
    /// Pressed since the last tick: a release is held back until one tick has sent the press.
    unseen: HashSet<String>,
    /// Released before any tick saw the press; dropped right after the next tick.
    late_release: HashSet<String>,
    toggles: HashSet<String>,
    /// Tap pulses: (button, from, until). Several taps of one button queue up with a gap.
    taps: Vec<(Action, f64, f64)>,
    layer: usize,
    acc_x: f64,
    acc_y: f64,
    last_tick: Option<f64>,
    last_move: f64,
    last_target: V2,
    out: V2,
    pub last_frame: PadFrame,
    /// Mouse speed (counts/ms) of the last tick with movement.
    pub last_speed: f32,
    pub last_tick_moved: bool,
}

impl Mapper {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            held: HashMap::new(),
            unseen: HashSet::new(),
            late_release: HashSet::new(),
            toggles: HashSet::new(),
            taps: Vec::new(),
            layer: 0,
            acc_x: 0.0,
            acc_y: 0.0,
            last_tick: None,
            last_move: f64::NEG_INFINITY,
            last_target: V2::default(),
            out: V2::default(),
            last_frame: PadFrame::default(),
            last_speed: 0.0,
            last_tick_moved: false,
        }
    }

    pub fn set_profile(&mut self, profile: Profile) {
        if self.layer >= profile.layers.len() {
            self.layer = 0;
        }
        self.profile = profile;
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The bindings of the active layer (for the on-screen controls legend).
    pub fn layer_bindings(&self) -> &[crate::profile::Binding] {
        self.profile
            .layers
            .get(self.layer)
            .map(|l| l.bindings.as_slice())
            .unwrap_or(&[])
    }

    pub fn layer_name(&self) -> &str {
        self.profile
            .layers
            .get(self.layer)
            .map(|l| l.name.as_str())
            .unwrap_or("")
    }

    /// Release everything (e.g. when capture ends) so no key gets stuck on the console.
    pub fn reset(&mut self) {
        self.held.clear();
        self.unseen.clear();
        self.late_release.clear();
        self.toggles.clear();
        self.taps.clear();
        self.acc_x = 0.0;
        self.acc_y = 0.0;
        self.out = V2::default();
        self.last_target = V2::default();
        self.last_move = f64::NEG_INFINITY;
    }

    /// Bindings for an input: the active layer wins, the first (base) layer is the fallback.
    fn resolve(&self, input: &str) -> Vec<Binding> {
        let layers = &self.profile.layers;
        let own: Vec<Binding> = layers
            .get(self.layer)
            .map(|l| {
                l.bindings
                    .iter()
                    .filter(|b| b.input == input)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if !own.is_empty() || self.layer == 0 {
            return own;
        }
        layers
            .first()
            .map(|l| {
                l.bindings
                    .iter()
                    .filter(|b| b.input == input)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn binding_id(b: &Binding) -> String {
        format!(
            "{}|{:?}|{:?}|{}",
            b.input,
            b.action,
            b.mode,
            b.switch_layer.as_deref().unwrap_or("")
        )
    }

    /// Toggles belong to the layer they were switched on in: changing layer turns them off,
    /// so a latched button can't leak into a layer where it means something else (or where
    /// its key does something else and can't switch it back off).
    fn switch_layer(&mut self, name: &str) {
        if let Some(i) = self.profile.layers.iter().position(|l| l.name == name) {
            if i != self.layer {
                self.toggles.clear();
            }
            self.layer = i;
        }
    }

    fn flip_toggle(&mut self, b: &Binding) {
        let id = Self::binding_id(b);
        if !self.toggles.remove(&id) {
            self.toggles.insert(id);
        }
    }

    /// Queue one tap of `a`: right away, or after the taps of `a` already queued.
    fn tap(&mut self, a: Action, now: f64) {
        let from = self
            .taps
            .iter()
            .filter(|(t, _, until)| *t == a && *until > now)
            .map(|(_, _, until)| until + TAP_GAP_MS)
            .fold(now, f64::max);
        self.taps.push((a, from, from + TAP_MS));
    }

    /// Run a press (key, button or wheel notch) through its bindings.
    fn trigger(&mut self, input: &str, now: f64, wheel: bool) {
        let bindings = self.resolve(input);
        let mut next_layer = None;
        for b in &bindings {
            match b.mode {
                BindMode::Toggle => self.flip_toggle(b),
                BindMode::Tap => self.tap(b.action, now),
                _ if wheel => self.tap(b.action, now),
                _ => {}
            }
            if let Some(l) = &b.switch_layer {
                next_layer = Some(l.clone());
            }
        }
        if let Some(l) = next_layer {
            self.switch_layer(&l);
        }
    }

    pub fn press(&mut self, input: &str, now: f64) {
        if self.late_release.remove(input) {
            return; // pressed again before a tick sent the earlier press: it's still down
        }
        if self.held.contains_key(input) {
            return; // key repeat
        }
        self.held.insert(input.to_string(), now);
        self.unseen.insert(input.to_string());
        self.trigger(input, now, false);
    }

    pub fn release(&mut self, input: &str) {
        if self.unseen.contains(input) {
            // A press and release between two ticks: keep it down for one tick so it's sent.
            self.late_release.insert(input.to_string());
        } else {
            self.held.remove(input);
        }
    }

    pub fn is_held(&self, input: &str) -> bool {
        self.held.contains_key(input) && !self.late_release.contains(input)
    }

    /// One scroll wheel notch: always a tap (or a toggle flip).
    pub fn wheel(&mut self, up: bool, now: f64) {
        let input = if up { "WheelUp" } else { "WheelDown" };
        self.trigger(input, now, true);
    }

    /// Whether the aim button is down because of the key that aims in the base layer (in other
    /// layers the same button can mean something else, e.g. braking with LT in a vehicle).
    fn aiming(&self, active: &HashSet<Action>) -> bool {
        let aim = self.profile.mouse.aim_button;
        if !active.contains(&aim) {
            return false;
        }
        let Some(base) = self.profile.layers.first() else {
            return true;
        };
        base.bindings
            .iter()
            .filter(|b| b.action == aim)
            .any(|b| self.is_held(&b.input) && self.resolve(&b.input).iter().any(|r| r.action == aim))
    }

    pub fn mouse_move(&mut self, dx: f64, dy: f64) {
        self.acc_x += dx;
        self.acc_y += dy;
    }

    pub fn active_actions(&mut self, now: f64) -> HashSet<Action> {
        let mut active = HashSet::new();
        let held: Vec<(String, f64)> = self.held.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (input, since) in held {
            for b in self.resolve(&input) {
                match b.mode {
                    BindMode::Hold => {
                        active.insert(b.action);
                    }
                    BindMode::Turbo => {
                        if ((now - since) / (TURBO_PERIOD_MS / 2.0)).floor() as i64 % 2 == 0 {
                            active.insert(b.action);
                        }
                    }
                    _ => {}
                }
            }
        }
        for l in &self.profile.layers {
            for b in &l.bindings {
                if self.toggles.contains(&Self::binding_id(b)) {
                    active.insert(b.action);
                }
            }
        }
        self.taps.retain(|(_, _, until)| *until > now);
        for (a, from, _) in &self.taps {
            if *from <= now {
                active.insert(*a);
            }
        }
        active.remove(&Action::None);
        active
    }

    /// Produce the controller frame for this moment. Call at a fixed rate.
    pub fn tick(&mut self, now: f64) -> PadFrame {
        let active = self.active_actions(now);
        let aiming = self.aiming(&active);
        // Every press has now been sent at least once; apply the releases that waited for that.
        self.unseen.clear();
        for input in std::mem::take(&mut self.late_release) {
            self.held.remove(&input);
        }
        let mut f = PadFrame::default();
        for a in &active {
            f.buttons |= a.button_bit();
        }
        if active.contains(&Action::LeftTrigger) {
            f.lt = 1.0;
        }
        if active.contains(&Action::RightTrigger) {
            f.rt = 1.0;
        }

        let mut left = key_stick(&active, true);
        if active.contains(&Action::Walk) {
            left.x *= self.profile.walk_magnitude;
            left.y *= self.profile.walk_magnitude;
        }
        let right_keys = key_stick(&active, false);

        let mouse = self.mouse_stick(now, aiming);

        let (mut l, mut r) = (left, right_keys);
        if self.profile.mouse.enabled {
            match self.profile.mouse.stick {
                Stick::Right => r = louder(right_keys, mouse),
                Stick::Left => l = louder(left, mouse),
            }
        }
        f.lx = l.x;
        f.ly = l.y;
        f.rx = r.x;
        f.ry = r.y;
        self.last_frame = f;
        f
    }

    fn mouse_stick(&mut self, now: f64, aiming: bool) -> V2 {
        let m = self.profile.mouse.clone();
        let dt = match self.last_tick {
            None => 16.0,
            Some(t) => (now - t).clamp(1.0, 50.0),
        };
        self.last_tick = Some(now);

        let (dx, dy) = (self.acc_x, self.acc_y);
        self.acc_x = 0.0;
        self.acc_y = 0.0;
        self.last_tick_moved = dx != 0.0 || dy != 0.0;
        if !m.enabled {
            return V2::default();
        }

        let target = if dx != 0.0 || dy != 0.0 {
            let mult = m.sensitivity as f64 * if aiming { m.aim_multiplier as f64 } else { 1.0 };
            let vx = dx / dt;
            let vy = dy / dt;
            self.last_speed = (vx * vx + vy * vy).sqrt() as f32;
            let sy = vy * mult * m.yx_ratio as f64 * if m.invert_y { -1.0 } else { 1.0 };
            let t = shape_stick(
                (vx * mult) as f32,
                sy as f32,
                m.curve,
                m.deadzone,
                m.deadzone_shape,
            );
            self.last_target = t;
            self.last_move = now;
            t
        } else if now - self.last_move <= m.hold_ms as f64 {
            self.last_target
        } else {
            V2::default()
        };

        let s = m.smoothing.clamp(0.0, 0.95);
        let mut x = self.out.x * s + target.x * (1.0 - s);
        let mut y = self.out.y * s + target.y * (1.0 - s);
        if target == V2::default() && (x * x + y * y).sqrt() < m.deadzone * 0.5 {
            x = 0.0;
            y = 0.0;
        }
        self.out = V2 { x, y };
        clamp(self.out)
    }
}

fn key_stick(active: &HashSet<Action>, left: bool) -> V2 {
    let (u, d, l, r) = if left {
        (
            Action::LeftStickUp,
            Action::LeftStickDown,
            Action::LeftStickLeft,
            Action::LeftStickRight,
        )
    } else {
        (
            Action::RightStickUp,
            Action::RightStickDown,
            Action::RightStickLeft,
            Action::RightStickRight,
        )
    };
    let mut v = V2::default();
    if active.contains(&l) {
        v.x -= 1.0;
    }
    if active.contains(&r) {
        v.x += 1.0;
    }
    if active.contains(&u) {
        v.y -= 1.0;
    }
    if active.contains(&d) {
        v.y += 1.0;
    }
    let mag = (v.x * v.x + v.y * v.y).sqrt();
    if mag > 1.0 {
        v.x /= mag;
        v.y /= mag;
    }
    v
}

fn louder(a: V2, b: V2) -> V2 {
    if a.x.hypot(a.y) >= b.x.hypot(b.y) {
        a
    } else {
        b
    }
}

fn clamp(v: V2) -> V2 {
    V2 {
        x: v.x.clamp(-1.0, 1.0),
        y: v.y.clamp(-1.0, 1.0),
    }
}

/// Apply response curve and anti-deadzone to a raw stick vector.
fn shape_stick(x: f32, y: f32, curve: f32, deadzone: f32, shape: DeadzoneShape) -> V2 {
    let dz = deadzone.clamp(0.0, 0.9);
    let c = curve.max(0.2);
    match shape {
        DeadzoneShape::Square => {
            let f = |v: f32| {
                if v == 0.0 {
                    0.0
                } else {
                    v.signum() * (dz + (1.0 - dz) * v.abs().min(1.0).powf(c))
                }
            };
            V2 { x: f(x), y: f(y) }
        }
        DeadzoneShape::Circle => {
            let mag = x.hypot(y);
            if mag == 0.0 {
                return V2::default();
            }
            let out = dz + (1.0 - dz) * mag.min(1.0).powf(c);
            V2 {
                x: x / mag * out,
                y: y / mag * out,
            }
        }
    }
}

pub struct Suggestion {
    pub sensitivity: f32,
    pub p50: f32,
    pub p95: f32,
}

/// Pick a sensitivity so the player's fastest comfortable turns just reach a full stick.
pub fn suggest_sensitivity(speeds: &[f32]) -> Option<Suggestion> {
    let mut moving: Vec<f32> = speeds.iter().copied().filter(|s| *s > 0.05).collect();
    if moving.len() < 20 {
        return None;
    }
    moving.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f32| moving[((p * moving.len() as f32) as usize).min(moving.len() - 1)];
    let p50 = pct(0.5);
    let p95 = pct(0.95);
    Some(Suggestion {
        sensitivity: ((1.0 / p95) * 1000.0).round() / 1000.0,
        p50,
        p95,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{builtin_profiles, Layer, MouseSettings};

    fn simple(m: MouseSettings) -> Profile {
        let bind = |i: &str, a: Action, mode: BindMode, sl: Option<&str>| Binding {
            input: i.into(),
            action: a,
            mode,
            switch_layer: sl.map(|s| s.into()),
            label: String::new(),
        };
        Profile {
            id: "t".into(),
            name: "t".into(),
            game: String::new(),
            notes: String::new(),
            builtin: false,
            mouse: m,
            walk_magnitude: 0.4,
            layers: vec![
                Layer {
                    name: "Base".into(),
                    bindings: vec![
                        bind("W", Action::LeftStickUp, BindMode::Hold, None),
                        bind("D", Action::LeftStickRight, BindMode::Hold, None),
                        bind("Mouse2", Action::LeftTrigger, BindMode::Hold, None),
                        bind("T", Action::A, BindMode::Toggle, None),
                        bind("Q", Action::B, BindMode::Tap, None),
                        bind("Shift", Action::X, BindMode::Turbo, None),
                        bind("Alt", Action::Walk, BindMode::Hold, None),
                        bind("F", Action::Y, BindMode::Hold, Some("Car")),
                        bind("WheelUp", Action::DPadUp, BindMode::Hold, None),
                    ],
                },
                Layer {
                    name: "Car".into(),
                    bindings: vec![
                        bind("W", Action::RightTrigger, BindMode::Hold, None),
                        bind("F", Action::Y, BindMode::Hold, Some("Base")),
                    ],
                },
            ],
        }
    }
    fn mouse(f: impl FnOnce(&mut MouseSettings)) -> MouseSettings {
        let mut m = MouseSettings {
            smoothing: 0.0,
            hold_ms: 0.0,
            ..Default::default()
        };
        f(&mut m);
        m
    }

    #[test]
    fn button_modes() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.press("Mouse2", 0.0);
        assert_eq!(m.tick(0.0).lt, 1.0);
        m.release("Mouse2");
        assert_eq!(m.tick(16.0).lt, 0.0);

        m.press("T", 20.0);
        m.release("T");
        assert!(m.tick(30.0).pressed(Action::A));
        m.press("T", 40.0);
        m.release("T");
        assert!(!m.tick(50.0).pressed(Action::A));

        m.press("Q", 100.0);
        m.release("Q");
        assert!(m.tick(110.0).pressed(Action::B));
        assert!(!m.tick(100.0 + TAP_MS + 1.0).pressed(Action::B));

        m.press("Shift", 1000.0);
        let p: Vec<bool> = [1000.0, 1030.0, 1060.0, 1090.0, 1120.0]
            .iter()
            .map(|t| m.tick(*t).pressed(Action::X))
            .collect();
        assert_eq!(p, vec![true, true, false, false, true]);
    }

    #[test]
    fn diagonal_and_walk() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.press("W", 0.0);
        m.press("D", 0.0);
        let f = m.tick(0.0);
        assert!((f.lx.hypot(f.ly) - 1.0).abs() < 1e-5);
        assert!(f.ly < 0.0);
        m.press("Alt", 1.0);
        let w = m.tick(16.0);
        assert!((w.lx.hypot(w.ly) - 0.4).abs() < 1e-5);
    }

    #[test]
    fn layers_and_fallback() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.press("F", 0.0);
        m.release("F");
        assert_eq!(m.layer_name(), "Car");
        m.press("W", 10.0);
        let f = m.tick(10.0);
        assert_eq!(f.rt, 1.0);
        assert_eq!(f.ly, 0.0);
        m.press("Mouse2", 12.0);
        assert_eq!(m.tick(14.0).lt, 1.0);
        m.release("W");
        m.release("Mouse2");
        m.press("F", 20.0);
        m.release("F");
        assert_eq!(m.layer_name(), "Base");
    }

    #[test]
    fn wheel_tap() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.wheel(true, 0.0);
        assert!(m.tick(1.0).pressed(Action::DPadUp));
        assert!(!m.tick(TAP_MS + 5.0).pressed(Action::DPadUp));
    }

    #[test]
    fn a_press_released_before_any_tick_is_still_sent() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.tick(0.0);
        m.press("Mouse2", 5.0);
        m.release("Mouse2");
        assert_eq!(m.tick(8.0).lt, 1.0, "sent once");
        assert_eq!(m.tick(16.0).lt, 0.0, "then released");
        // Pressed again before the tick: it stays down.
        m.press("Mouse2", 20.0);
        m.release("Mouse2");
        m.press("Mouse2", 21.0);
        assert_eq!(m.tick(24.0).lt, 1.0);
        assert_eq!(m.tick(32.0).lt, 1.0);
    }

    #[test]
    fn toggles_end_when_the_layer_changes() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.press("T", 0.0);
        m.release("T");
        assert!(m.tick(1.0).pressed(Action::A));
        m.press("F", 2.0); // to the Car layer
        m.release("F");
        m.tick(3.0);
        assert!(!m.tick(4.0).pressed(Action::A), "toggle stays in its layer");
    }

    #[test]
    fn the_aim_button_from_another_key_is_not_aiming() {
        let mut p = simple(mouse(|m| {
            m.sensitivity = 1.0;
            m.deadzone = 0.0;
            m.aim_multiplier = 0.5;
            m.yx_ratio = 1.0;
        }));
        p.layers[1].bindings.push(Binding {
            input: "S".into(),
            action: Action::LeftTrigger,
            mode: BindMode::Hold,
            switch_layer: None,
            label: "Brake".into(),
        });
        let mut m = Mapper::new(p);
        m.press("F", 0.0);
        m.release("F");
        m.tick(0.0);
        m.press("S", 1.0); // brake (LT) in the Car layer
        m.mouse_move(8.0, 0.0);
        let f = m.tick(16.0);
        assert_eq!(f.lt, 1.0);
        assert!((f.rx - 0.5).abs() < 1e-5, "full sensitivity, got {}", f.rx);
    }

    #[test]
    fn fast_wheel_notches_are_separate_presses() {
        let mut m = Mapper::new(simple(mouse(|_| {})));
        m.wheel(true, 0.0);
        m.wheel(true, 10.0);
        let at = |m: &mut Mapper, t: f64| m.tick(t).pressed(Action::DPadUp);
        assert!(at(&mut m, 1.0));
        assert!(!at(&mut m, TAP_MS + TAP_GAP_MS / 2.0), "released between the two");
        assert!(at(&mut m, TAP_MS + TAP_GAP_MS + 5.0), "second press");
        assert!(!at(&mut m, 2.0 * TAP_MS + TAP_GAP_MS + 5.0));
    }

    #[test]
    fn mouse_to_stick() {
        let mut m = Mapper::new(simple(mouse(|m| {
            m.sensitivity = 0.5;
            m.deadzone = 0.2;
            m.yx_ratio = 1.0;
        })));
        m.tick(0.0);
        m.mouse_move(16.0, 0.0);
        let f = m.tick(16.0);
        assert!((f.rx - (0.2 + 0.8 * 0.5)).abs() < 1e-5);
        assert_eq!(f.ry, 0.0);
        m.mouse_move(1.0, 0.0);
        assert!(m.tick(32.0).rx >= 0.2);
        assert_eq!(m.tick(48.0).rx, 0.0);
    }

    #[test]
    fn aim_invert_clamp_hold() {
        let mut m = Mapper::new(simple(mouse(|m| {
            m.sensitivity = 1.0;
            m.deadzone = 0.0;
            m.aim_multiplier = 0.5;
            m.invert_y = true;
            m.yx_ratio = 1.0;
        })));
        m.tick(0.0);
        m.press("Mouse2", 0.0);
        m.mouse_move(8.0, 0.0);
        assert!((m.tick(16.0).rx - 0.25).abs() < 1e-5);
        m.release("Mouse2");
        m.mouse_move(0.0, 160.0);
        assert_eq!(m.tick(32.0).ry, -1.0);

        let mut h = Mapper::new(simple(mouse(|m| {
            m.hold_ms = 20.0;
            m.deadzone = 0.0;
            m.sensitivity = 1.0;
        })));
        h.tick(0.0);
        h.mouse_move(8.0, 0.0);
        let a = h.tick(16.0).rx;
        assert!((h.tick(30.0).rx - a).abs() < 1e-6);
        assert_eq!(h.tick(60.0).rx, 0.0);
    }

    #[test]
    fn left_stick_and_disabled() {
        let mut m = Mapper::new(simple(mouse(|m| {
            m.stick = Stick::Left;
            m.deadzone = 0.0;
            m.sensitivity = 1.0;
        })));
        m.tick(0.0);
        m.mouse_move(8.0, 0.0);
        assert!(m.tick(16.0).lx > 0.0);
        let mut off = Mapper::new(simple(mouse(|m| m.enabled = false)));
        off.tick(0.0);
        off.mouse_move(50.0, 50.0);
        assert_eq!(off.tick(16.0).rx, 0.0);
    }

    #[test]
    fn square_deadzone_and_suggest() {
        let s = shape_stick(0.5, 0.0, 1.0, 0.2, DeadzoneShape::Square);
        assert!((s.x - 0.6).abs() < 1e-5 && s.y == 0.0);
        let speeds: Vec<f32> = (1..=200).map(|i| i as f32 / 50.0).collect();
        let r = suggest_sensitivity(&speeds).unwrap();
        assert!((r.p95 * r.sensitivity - 1.0).abs() < 0.05);
        assert!(suggest_sensitivity(&[1.0, 2.0]).is_none());
    }

    #[test]
    fn builtins_are_valid() {
        let special = [
            "Shift",
            "Ctrl",
            "Alt",
            "Mouse0",
            "Mouse1",
            "Mouse2",
            "Mouse3",
            "Mouse4",
            "WheelUp",
            "WheelDown",
        ];
        for p in builtin_profiles() {
            let n = p.clone().normalize();
            assert_eq!(n, p, "{} changed on normalize", p.id);
            for l in &p.layers {
                for b in &l.bindings {
                    assert!(
                        special.contains(&b.input.as_str())
                            || egui::Key::from_name(&b.input).is_some(),
                        "unknown key {} in {}",
                        b.input,
                        p.id
                    );
                }
            }
        }
        let mut m = Mapper::new(builtin_profiles()[0].clone());
        m.press("F", 0.0);
        m.release("F");
        assert_eq!(m.layer_name(), "Vehicle");
    }
}
