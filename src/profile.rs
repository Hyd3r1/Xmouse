//! Profiles: which key does what, and how the mouse drives a stick.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    A,
    B,
    X,
    Y,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    View,
    Menu,
    Nexus,
    LeftThumb,
    RightThumb,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
    /// Slow down the left stick while held.
    Walk,
    /// Does nothing (useful for a key that only switches layers).
    None,
}

impl Action {
    pub const ALL: [Action; 27] = [
        Action::A,
        Action::B,
        Action::X,
        Action::Y,
        Action::LeftShoulder,
        Action::RightShoulder,
        Action::LeftTrigger,
        Action::RightTrigger,
        Action::View,
        Action::Menu,
        Action::Nexus,
        Action::LeftThumb,
        Action::RightThumb,
        Action::DPadUp,
        Action::DPadDown,
        Action::DPadLeft,
        Action::DPadRight,
        Action::LeftStickUp,
        Action::LeftStickDown,
        Action::LeftStickLeft,
        Action::LeftStickRight,
        Action::RightStickUp,
        Action::RightStickDown,
        Action::RightStickLeft,
        Action::RightStickRight,
        Action::Walk,
        Action::None,
    ];

    /// Pad buttons that can be the "aiming" button.
    pub const BUTTONS: [Action; 17] = [
        Action::A,
        Action::B,
        Action::X,
        Action::Y,
        Action::LeftShoulder,
        Action::RightShoulder,
        Action::LeftTrigger,
        Action::RightTrigger,
        Action::View,
        Action::Menu,
        Action::Nexus,
        Action::LeftThumb,
        Action::RightThumb,
        Action::DPadUp,
        Action::DPadDown,
        Action::DPadLeft,
        Action::DPadRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::A => "A",
            Action::B => "B",
            Action::X => "X",
            Action::Y => "Y",
            Action::LeftShoulder => "LB (left bumper)",
            Action::RightShoulder => "RB (right bumper)",
            Action::LeftTrigger => "LT (left trigger)",
            Action::RightTrigger => "RT (right trigger)",
            Action::View => "View",
            Action::Menu => "Menu",
            Action::Nexus => "Xbox button",
            Action::LeftThumb => "L3 (press left stick)",
            Action::RightThumb => "R3 (press right stick)",
            Action::DPadUp => "D-pad up",
            Action::DPadDown => "D-pad down",
            Action::DPadLeft => "D-pad left",
            Action::DPadRight => "D-pad right",
            Action::LeftStickUp => "Left stick up",
            Action::LeftStickDown => "Left stick down",
            Action::LeftStickLeft => "Left stick left",
            Action::LeftStickRight => "Left stick right",
            Action::RightStickUp => "Right stick up",
            Action::RightStickDown => "Right stick down",
            Action::RightStickLeft => "Right stick left",
            Action::RightStickRight => "Right stick right",
            Action::Walk => "Walk (slow left stick)",
            Action::None => "Nothing (layer switch only)",
        }
    }

    /// Bit in the xHome gamepad button mask (0 for analog/special actions).
    pub fn button_bit(self) -> u16 {
        match self {
            Action::Nexus => 2,
            Action::Menu => 4,
            Action::View => 8,
            Action::A => 16,
            Action::B => 32,
            Action::X => 64,
            Action::Y => 128,
            Action::DPadUp => 256,
            Action::DPadDown => 512,
            Action::DPadLeft => 1024,
            Action::DPadRight => 2048,
            Action::LeftShoulder => 4096,
            Action::RightShoulder => 8192,
            Action::LeftThumb => 16384,
            Action::RightThumb => 32768,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindMode {
    /// Active while held.
    #[default]
    Hold,
    /// Each press flips it on/off.
    Toggle,
    /// A short pulse on press.
    Tap,
    /// Rapid repeated presses while held (e.g. GTA sprint).
    Turbo,
}

impl BindMode {
    pub const ALL: [BindMode; 4] = [
        BindMode::Hold,
        BindMode::Toggle,
        BindMode::Tap,
        BindMode::Turbo,
    ];
    pub fn label(self) -> &'static str {
        match self {
            BindMode::Hold => "Hold",
            BindMode::Toggle => "Toggle",
            BindMode::Tap => "Tap",
            BindMode::Turbo => "Turbo (repeat)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    /// Key name (egui key name, e.g. "W", "Space"), "Shift"/"Ctrl"/"Alt",
    /// "Mouse0".."Mouse4", "WheelUp" or "WheelDown".
    pub input: String,
    pub action: Action,
    #[serde(default)]
    pub mode: BindMode,
    /// When pressed, switch to this layer as well.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_layer: Option<String>,
    /// What it does in the game, e.g. "Shoot" (optional, shown in the key list).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Binding {
    /// Whether pressing the key does anything: presses a controller button or switches layer.
    /// A key bound to "Nothing" in a layer only stops the base layer's binding from applying.
    pub fn does_something(&self) -> bool {
        self.action != Action::None || self.switch_layer.is_some()
    }

    /// Give the binding an in-game name.
    pub fn named(mut self, label: &str) -> Self {
        self.label = label.to_string();
        self
    }

    /// Title for lists: the in-game name, or the controller button.
    pub fn title(&self) -> &str {
        if self.label.is_empty() {
            self.action.label()
        } else {
            &self.label
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stick {
    #[default]
    Right,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeadzoneShape {
    #[default]
    Circle,
    Square,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MouseSettings {
    pub enabled: bool,
    pub stick: Stick,
    /// Stick deflection per (mouse count / ms).
    pub sensitivity: f32,
    /// Multiplier while the aim button is held.
    pub aim_multiplier: f32,
    pub aim_button: Action,
    pub yx_ratio: f32,
    /// Response curve exponent. 1 = linear.
    pub curve: f32,
    /// Anti-deadzone: smallest stick value the game reacts to.
    pub deadzone: f32,
    pub deadzone_shape: DeadzoneShape,
    /// 0 = raw, 0.9 = very smooth.
    pub smoothing: f32,
    /// Keep the last stick value this long after the mouse stops (ms).
    pub hold_ms: f32,
    pub invert_y: bool,
}

impl Default for MouseSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            stick: Stick::Right,
            sensitivity: 0.45,
            aim_multiplier: 0.6,
            aim_button: Action::LeftTrigger,
            yx_ratio: 0.9,
            curve: 1.0,
            deadzone: 0.22,
            deadzone_shape: DeadzoneShape::Circle,
            smoothing: 0.25,
            hold_ms: 20.0,
            invert_y: false,
        }
    }
}

fn default_walk() -> f32 {
    0.4
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub game: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default, skip_serializing)]
    pub builtin: bool,
    #[serde(default)]
    pub mouse: MouseSettings,
    #[serde(default = "default_walk")]
    pub walk_magnitude: f32,
    #[serde(default)]
    pub layers: Vec<Layer>,
}

impl Profile {
    /// Make sure the profile is usable (at least one layer, valid layer references).
    pub fn normalize(mut self) -> Self {
        if self.layers.is_empty() {
            self.layers.push(Layer {
                name: "Default".into(),
                bindings: vec![],
            });
        }
        for (i, l) in self.layers.iter_mut().enumerate() {
            if l.name.trim().is_empty() {
                l.name = format!("Layer {}", i + 1);
            }
        }
        let names: Vec<String> = self.layers.iter().map(|l| l.name.clone()).collect();
        for l in &mut self.layers {
            for b in &mut l.bindings {
                if let Some(s) = &b.switch_layer {
                    if !names.contains(s) {
                        b.switch_layer = None;
                    }
                }
            }
        }
        if self.name.trim().is_empty() {
            self.name = "Profile".into();
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Built-in profiles

fn b(input: &str, action: Action) -> Binding {
    Binding {
        input: input.into(),
        action,
        mode: BindMode::Hold,
        switch_layer: None,
        label: String::new(),
    }
}
fn bm(input: &str, action: Action, mode: BindMode) -> Binding {
    Binding {
        input: input.into(),
        action,
        mode,
        switch_layer: None,
        label: String::new(),
    }
}
fn bl(input: &str, action: Action, mode: BindMode, layer: &str) -> Binding {
    Binding {
        input: input.into(),
        action,
        mode,
        switch_layer: Some(layer.into()),
        label: String::new(),
    }
}

fn wasd() -> Vec<Binding> {
    vec![
        b("W", Action::LeftStickUp).named("Forward"),
        b("S", Action::LeftStickDown).named("Back"),
        b("A", Action::LeftStickLeft).named("Left"),
        b("D", Action::LeftStickRight).named("Right"),
    ]
}
fn arrows_dpad() -> Vec<Binding> {
    vec![
        b("Up", Action::DPadUp),
        b("Down", Action::DPadDown),
        b("Left", Action::DPadLeft),
        b("Right", Action::DPadRight),
    ]
}
fn system() -> Vec<Binding> {
    vec![
        b("Escape", Action::Menu).named("Pause menu"),
        b("P", Action::Menu).named("Pause menu"),
        b("Home", Action::Nexus).named("Xbox guide"),
    ]
}

fn gta_on_foot() -> Vec<Binding> {
    let mut v = wasd();
    v.extend([
        bm("Shift", Action::A, BindMode::Turbo).named("Sprint (tap A)"),
        b("Space", Action::X).named("Jump"),
        bl("F", Action::Y, BindMode::Hold, "Vehicle").named("Enter vehicle"),
        b("Mouse2", Action::LeftTrigger).named("Aim"),
        b("Mouse0", Action::RightTrigger).named("Shoot"),
        b("R", Action::B).named("Reload / melee"),
        b("Q", Action::RightShoulder).named("Cover"),
        b("Tab", Action::LeftShoulder).named("Weapon wheel"),
        bm("Ctrl", Action::LeftThumb, BindMode::Tap).named("Stealth"),
        b("C", Action::RightThumb).named("Look behind"),
        b("V", Action::View).named("Camera view"),
        b("M", Action::View).named("Interaction menu (hold)"),
        b("E", Action::DPadRight).named("Context / pick up"),
        b("T", Action::DPadUp).named("Phone"),
        b("Z", Action::DPadDown).named("Character wheel"),
        b("X", Action::DPadLeft).named("Special ability"),
        b("Alt", Action::Walk).named("Walk slowly"),
        bl("Backtick", Action::None, BindMode::Tap, "Vehicle").named("Manual layer switch"),
    ]);
    v.extend(arrows_dpad());
    v.extend(system());
    v
}

fn gta_vehicle() -> Vec<Binding> {
    vec![
        b("W", Action::RightTrigger).named("Accelerate"),
        b("S", Action::LeftTrigger).named("Brake / reverse"),
        b("A", Action::LeftStickLeft).named("Left"),
        b("D", Action::LeftStickRight).named("Right"),
        b("Shift", Action::LeftStickUp).named("Lean forward"),
        b("Ctrl", Action::LeftStickDown).named("Lean back"),
        b("Space", Action::RightShoulder).named("Handbrake"),
        bl("F", Action::Y, BindMode::Hold, "On foot").named("Exit vehicle"),
        b("Mouse0", Action::RightShoulder).named("Drive-by shoot"),
        b("Mouse2", Action::LeftShoulder).named("Drive-by aim"),
        b("H", Action::LeftThumb).named("Horn"),
        b("C", Action::RightThumb).named("Look behind"),
        b("V", Action::View).named("Camera"),
        b("E", Action::DPadRight).named("Headlights"),
        b("Q", Action::DPadLeft).named("Radio"),
        b("R", Action::B).named("Cinematic camera"),
        bl("Backtick", Action::None, BindMode::Tap, "On foot"),
    ]
}

/// GTA VI on foot. One key per controller button, one button per key.
fn gta6_on_foot() -> Vec<Binding> {
    let mut v = wasd();
    v.extend([
        b("Shift", Action::A).named("Sprint"),
        b("Space", Action::X).named("Jump"),
        bl("F", Action::Y, BindMode::Hold, "Vehicle").named("Enter vehicle"),
        b("Mouse2", Action::LeftTrigger).named("Aim"),
        b("Mouse0", Action::RightTrigger).named("Shoot"),
        b("Mouse3", Action::LeftShoulder).named("Weapon wheel (LB)"),
        b("Mouse4", Action::RightShoulder).named("Cover (RB)"),
        b("R", Action::B).named("Reload / melee"),
        bm("Ctrl", Action::LeftThumb, BindMode::Tap).named("Stealth"),
        b("C", Action::RightThumb).named("Look behind"),
        b("M", Action::View).named("Map"),
        b("Up", Action::DPadUp).named("D-pad up"),
        b("Down", Action::DPadDown).named("D-pad down"),
        b("Left", Action::DPadLeft).named("D-pad left"),
        b("Right", Action::DPadRight).named("D-pad right"),
        b("Alt", Action::Walk).named("Walk slowly"),
        b("Escape", Action::Menu).named("Pause menu"),
        b("Home", Action::Nexus).named("Xbox guide"),
        bl("Backtick", Action::None, BindMode::Tap, "Vehicle").named("Manual layer switch"),
    ]);
    v
}

/// GTA VI in vehicles. Same rule; the side buttons stay LB/RB (drive-by aim / shoot), and the
/// mouse buttons are explicitly unused so they don't fall back to on-foot aim/shoot (LT/RT,
/// which W/S already use here).
fn gta6_vehicle() -> Vec<Binding> {
    vec![
        b("W", Action::RightTrigger).named("Accelerate"),
        b("S", Action::LeftTrigger).named("Brake / reverse"),
        b("A", Action::LeftStickLeft).named("Left"),
        b("D", Action::LeftStickRight).named("Right"),
        b("Shift", Action::A).named("A"),
        b("Space", Action::X).named("X"),
        b("Ctrl", Action::LeftStickDown).named("Lean back"),
        bl("F", Action::Y, BindMode::Hold, "On foot").named("Exit vehicle"),
        b("Mouse3", Action::LeftShoulder).named("Drive-by aim (LB)"),
        b("Mouse4", Action::RightShoulder).named("Handbrake / drive-by shoot (RB)"),
        b("Mouse0", Action::None).named("Unused in vehicles"),
        b("Mouse2", Action::None).named("Unused in vehicles"),
        b("H", Action::LeftThumb).named("Horn"),
        b("C", Action::RightThumb).named("Look behind"),
        b("M", Action::View).named("Map"),
        b("Up", Action::DPadUp).named("D-pad up"),
        b("Down", Action::DPadDown).named("D-pad down"),
        b("Left", Action::DPadLeft).named("D-pad left (radio)"),
        b("Right", Action::DPadRight).named("D-pad right (headlights)"),
        b("R", Action::B).named("Cinematic camera"),
        bl("Backtick", Action::None, BindMode::Tap, "On foot"),
    ]
}

pub fn builtin_profiles() -> Vec<Profile> {
    let gta_mouse = MouseSettings {
        sensitivity: 0.42,
        deadzone: 0.2,
        aim_multiplier: 0.55,
        ..Default::default()
    };
    let gta_v = Profile {
        id: "gta-v".into(),
        name: "GTA V".into(),
        game: "Grand Theft Auto V".into(),
        notes: "F enters/exits vehicles and switches between on-foot and vehicle keys. The ` key (left of 1) switches manually if they get out of sync.".into(),
        builtin: true,
        mouse: gta_mouse.clone(),
        walk_magnitude: 0.4,
        layers: vec![
            Layer { name: "On foot".into(), bindings: gta_on_foot() },
            Layer { name: "Vehicle".into(), bindings: gta_vehicle() },
        ],
    };
    // GTA VI: Shift = A, Space = X, mouse back button ("mouse 4") = LB, forward ("mouse 5") = RB,
    // and every key does exactly one thing.
    let gta_vi = Profile {
        id: "gta-vi".into(),
        name: "GTA VI".into(),
        game: "Grand Theft Auto VI".into(),
        notes: "Shift = A, Space = X, M = View (map), arrow keys = D-pad, mouse back button (mouse 4) = LB, forward button (mouse 5) = RB. One key per button. F enters/exits vehicles and switches keys. Use the deadzone finder (F6) in game.".into(),
        layers: vec![
            Layer { name: "On foot".into(), bindings: gta6_on_foot() },
            Layer { name: "Vehicle".into(), bindings: gta6_vehicle() },
        ],
        ..gta_v.clone()
    };

    let mut rdr_foot = wasd();
    rdr_foot.extend([
        bm("Shift", Action::A, BindMode::Turbo).named("Sprint"),
        b("Space", Action::X).named("Jump"),
        bl("F", Action::Y, BindMode::Hold, "Horse"),
        b("Mouse2", Action::LeftTrigger).named("Aim"),
        b("Mouse0", Action::RightTrigger).named("Shoot"),
        b("R", Action::B).named("Reload / melee"),
        b("Q", Action::RightShoulder).named("Cover"),
        b("Tab", Action::LeftShoulder).named("Weapon wheel"),
        bm("Ctrl", Action::LeftThumb, BindMode::Tap).named("Crouch"),
        b("E", Action::RightThumb).named("Dead Eye"),
        b("C", Action::View).named("Camera view"),
        b("M", Action::Menu).named("Map"),
        b("I", Action::DPadUp).named("Satchel"),
        b("X", Action::DPadDown).named("Whistle horse"),
        b("G", Action::DPadRight).named("Context"),
        b("Z", Action::DPadLeft).named("Emote"),
        b("Alt", Action::Walk).named("Walk"),
        bl("Backtick", Action::None, BindMode::Tap, "Horse"),
    ]);
    rdr_foot.extend(arrows_dpad());
    rdr_foot.extend(system());
    let mut rdr_horse = wasd();
    rdr_horse.extend([
        bm("Shift", Action::A, BindMode::Turbo).named("Sprint"),
        b("Space", Action::X).named("Jump"),
        bl("F", Action::Y, BindMode::Hold, "On foot"),
        b("Ctrl", Action::B).named("Stop"),
        b("X", Action::LeftThumb).named("Match speed"),
        bl("Backtick", Action::None, BindMode::Tap, "On foot"),
    ]);
    let rdr2 = Profile {
        id: "rdr2".into(),
        name: "Red Dead Redemption 2".into(),
        game: "Red Dead Redemption 2".into(),
        notes: "F mounts/dismounts and switches to horse keys.".into(),
        builtin: true,
        mouse: MouseSettings {
            sensitivity: 0.4,
            deadzone: 0.24,
            aim_multiplier: 0.55,
            ..Default::default()
        },
        walk_magnitude: 0.4,
        layers: vec![
            Layer {
                name: "On foot".into(),
                bindings: rdr_foot,
            },
            Layer {
                name: "Horse".into(),
                bindings: rdr_horse,
            },
        ],
    };

    let mut shooter_b = wasd();
    shooter_b.extend([
        b("Mouse2", Action::LeftTrigger).named("Aim"),
        b("Mouse0", Action::RightTrigger).named("Shoot"),
        b("Space", Action::A).named("Jump"),
        b("Ctrl", Action::B).named("Crouch / slide"),
        b("C", Action::B).named("Crouch"),
        b("R", Action::X).named("Reload"),
        b("E", Action::X).named("Interact"),
        bm("WheelUp", Action::Y, BindMode::Tap).named("Next weapon"),
        bm("WheelDown", Action::Y, BindMode::Tap).named("Next weapon"),
        bm("1", Action::Y, BindMode::Tap).named("Switch weapon"),
        bm("2", Action::Y, BindMode::Tap).named("Switch weapon"),
        b("Q", Action::LeftShoulder).named("Tactical"),
        b("G", Action::RightShoulder).named("Lethal"),
        b("Shift", Action::LeftThumb).named("Sprint"),
        b("V", Action::RightThumb).named("Melee"),
        b("Mouse1", Action::RightThumb).named("Melee"),
        b("Tab", Action::View).named("Scoreboard"),
        b("M", Action::View).named("Map"),
        b("3", Action::DPadLeft),
        b("4", Action::DPadRight),
        b("Z", Action::DPadUp),
        b("X", Action::DPadDown),
        b("Alt", Action::Walk).named("Walk"),
    ]);
    shooter_b.extend(arrows_dpad());
    shooter_b.extend(system());
    let shooter = Profile {
        id: "generic-shooter".into(),
        name: "Shooter (standard layout)".into(),
        game: "Any first-person shooter".into(),
        notes: "Standard Xbox shooter layout. Many shooters on Xbox support mouse and keyboard natively – check the game first.".into(),
        builtin: true,
        mouse: MouseSettings { sensitivity: 0.5, deadzone: 0.18, aim_multiplier: 0.6, smoothing: 0.15, ..Default::default() },
        walk_magnitude: 0.45,
        layers: vec![Layer { name: "Default".into(), bindings: shooter_b }],
    };

    let mut tp = wasd();
    tp.extend([
        b("Space", Action::A).named("Jump"),
        b("Q", Action::B),
        b("Ctrl", Action::B).named("Dodge / crouch"),
        b("E", Action::X).named("Interact"),
        b("F", Action::Y),
        b("Mouse0", Action::RightTrigger).named("Shoot"),
        b("Mouse2", Action::LeftTrigger).named("Aim"),
        b("Mouse4", Action::RightShoulder),
        b("Mouse3", Action::LeftShoulder),
        b("R", Action::RightShoulder),
        b("Tab", Action::LeftShoulder).named("Weapon wheel"),
        b("Shift", Action::LeftThumb).named("Sprint"),
        b("Mouse1", Action::RightThumb).named("Melee"),
        b("C", Action::RightThumb),
        b("M", Action::View).named("Map"),
        bm("WheelUp", Action::DPadUp, BindMode::Tap),
        bm("WheelDown", Action::DPadDown, BindMode::Tap),
        b("1", Action::DPadLeft),
        b("2", Action::DPadRight),
        b("Alt", Action::Walk).named("Walk"),
    ]);
    tp.extend(arrows_dpad());
    tp.extend(system());
    let third_person = Profile {
        id: "generic-third-person".into(),
        name: "Action / third-person".into(),
        game: "Any third-person action game".into(),
        notes: String::new(),
        builtin: true,
        mouse: MouseSettings::default(),
        walk_magnitude: 0.4,
        layers: vec![Layer {
            name: "Default".into(),
            bindings: tp,
        }],
    };

    let mut menu = arrows_dpad();
    menu.extend([
        b("W", Action::DPadUp),
        b("S", Action::DPadDown),
        b("A", Action::DPadLeft),
        b("D", Action::DPadRight),
        b("Enter", Action::A).named("Select"),
        b("Space", Action::A).named("Select"),
        b("Mouse0", Action::A),
        b("Backspace", Action::B).named("Back"),
        b("Mouse2", Action::B),
        b("X", Action::X),
        b("Y", Action::Y),
        b("Q", Action::LeftShoulder).named("Previous tab"),
        b("E", Action::RightShoulder).named("Next tab"),
        b("PageUp", Action::LeftTrigger),
        b("PageDown", Action::RightTrigger),
        bm("WheelUp", Action::DPadUp, BindMode::Tap),
        bm("WheelDown", Action::DPadDown, BindMode::Tap),
        b("Tab", Action::View).named("View"),
        b("Escape", Action::Menu).named("Pause menu"),
        b("M", Action::Menu).named("Map"),
        b("Home", Action::Nexus).named("Xbox guide"),
        b("G", Action::Nexus),
    ]);
    let dashboard = Profile {
        id: "xbox-dashboard".into(),
        name: "Xbox menus".into(),
        game: "Xbox home, Guide and game menus".into(),
        notes: "Navigate the console with the keyboard. Mouse wheel scrolls.".into(),
        builtin: true,
        mouse: MouseSettings {
            enabled: false,
            ..Default::default()
        },
        walk_magnitude: 0.5,
        layers: vec![Layer {
            name: "Default".into(),
            bindings: menu,
        }],
    };

    vec![gta_v, gta_vi, rdr2, shooter, third_person, dashboard]
}

/// Human-friendly name for an input code.
pub fn input_label(code: &str) -> String {
    match code {
        "Mouse0" => "Left click".into(),
        "Mouse1" => "Middle click".into(),
        "Mouse2" => "Right click".into(),
        "Mouse3" => "Mouse back".into(),
        "Mouse4" => "Mouse forward".into(),
        "WheelUp" => "Wheel up".into(),
        "WheelDown" => "Wheel down".into(),
        "Up" => "Arrow up".into(),
        "Down" => "Arrow down".into(),
        "Left" => "Arrow left".into(),
        "Right" => "Arrow right".into(),
        "Backtick" => "` (left of 1)".into(),
        "OpenBracket" => "[".into(),
        "CloseBracket" => "]".into(),
        "" => "—".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn gta_vi_one_key_per_button() {
        let p = builtin_profiles()
            .into_iter()
            .find(|p| p.id == "gta-vi")
            .unwrap();
        for layer in &p.layers {
            let mut inputs = HashSet::new();
            let mut actions = HashSet::new();
            for bnd in &layer.bindings {
                assert!(
                    inputs.insert(bnd.input.clone()),
                    "{}: key {} bound twice",
                    layer.name,
                    bnd.input
                );
                if bnd.action != Action::None {
                    assert!(
                        actions.insert(bnd.action),
                        "{}: {:?} on two keys",
                        layer.name,
                        bnd.action
                    );
                }
            }
        }
        let foot = &p.layers[0].bindings;
        let find = |k: &str| foot.iter().find(|bnd| bnd.input == k).unwrap().action;
        assert_eq!(find("Shift"), Action::A);
        assert_eq!(find("Space"), Action::X);
        assert_eq!(find("Mouse3"), Action::LeftShoulder);
        assert_eq!(find("Mouse4"), Action::RightShoulder);
        assert_eq!(find("M"), Action::View);
        assert!(foot.iter().all(|bnd| bnd.input != "V"));
        for layer in &p.layers {
            let key = |k: &str| {
                layer
                    .bindings
                    .iter()
                    .find(|bnd| bnd.input == k)
                    .map(|bnd| bnd.action)
            };
            assert_eq!(key("Up"), Some(Action::DPadUp));
            assert_eq!(key("Down"), Some(Action::DPadDown));
            assert_eq!(key("Left"), Some(Action::DPadLeft));
            assert_eq!(key("Right"), Some(Action::DPadRight));
        }
    }
}
