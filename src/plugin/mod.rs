//! Console plugins: other programs that add a console platform to XMouse.
//!
//! A plugin is a separate executable with its own licence and its own source repository.
//! XMouse never links plugin code: it starts the program and talks to it over the program's
//! stdin and stdout. The plugin does everything specific to its platform (finding consoles,
//! pairing, the network protocol) and hands over plain H.264 video and Opus audio; XMouse does
//! everything else (decoding, GPU decoding, upscaling, sound, the mouse/keyboard mapper, UI).
//!
//! # Installing
//! `<config dir>\plugins\<id>\plugin.json` (see [`manifest`]):
//! ```json
//! { "id": "example", "name": "Example", "version": "1.0.0", "platform": "Example Console",
//!   "executable": "example-plugin", "args": [], "api": 1,
//!   "license": "MIT", "source": "https://example.org/example-plugin" }
//! ```
//!
//! # Protocol (API 1), in short
//! XMouse runs `<executable> --xmouse-plugin [args]` in the plugin's folder. Both directions
//! carry frames: `u32 LE length | type byte | payload` (length = 1 + payload, at most 8 MiB).
//! - type 1, control: one JSON object with a `"type"` field (both directions);
//! - type 2, video (plugin to host): `codec u8 (0 = H.264) | flags u8 (bit 0 = keyframe) |
//!   pts u64 LE in microseconds | one Annex-B access unit`;
//! - type 3, audio (plugin to host): `codec u8 (0 = Opus) | channels u8 | sample rate u32 LE |
//!   one Opus packet`.
//!
//! Host to plugin: `hello`, `list_consoles`, `pair`, `wake`, `start`, `input`,
//! `request_keyframe`, `stop`. Plugin to host: `hello`, `consoles`, `pair_result`,
//! `wake_result`, `status`, `rumble`, `log`. The fields are the ones of [`proto::ToPlugin`] and
//! [`proto::FromPlugin`]. Unknown message and frame types are ignored by both sides. stderr
//! goes to XMouse's log.
//!
//! # Using it from the app
//! - [`discover`] lists installed plugins;
//! - [`list_consoles`], [`pair`] and [`wake`] start the plugin for one question (blocking: call
//!   them from a background thread);
//! - [`run_plugin_session`] plays, reporting through the same `Shared` an Xbox session uses.

pub mod host;
pub mod install;
pub mod manifest;
pub mod proto;
pub mod session;

pub use host::{list_consoles, pair, wake, PluginClient, PluginConsole, PluginHello};
pub use install::{install_psnbridge, install_zip, psnbridge_source, PSNBRIDGE_ID, PSNBRIDGE_NAME};
pub use manifest::{discover, discover_in, plugins_dir, PluginInfo};
pub use proto::{ConsoleState, API_VERSION};
pub use session::{run_plugin_session, PluginSessionConfig, PluginStats};
