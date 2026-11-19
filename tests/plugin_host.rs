//! End-to-end test of the plugin system against a real plugin program: `examples/fake_plugin`
//! is built, installed into a temporary config folder next to its sample `plugin.json`, found
//! by discovery, and then driven the way the app drives a plugin: list, pair, wake, play.
//! The same program is also started in its misbehaving modes to check that nothing a plugin
//! does takes the host down with it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::task::JoinHandle;

use xmouse::mapper::Mapper;
use xmouse::plugin::{self, ConsoleState, PluginInfo, PluginSessionConfig, PluginStats};
use xmouse::profile::{builtin_profiles, Action, BindMode, Binding, Layer};
use xmouse::storage::PlayMode;
use xmouse::stream::{Phase, Shared, VideoFrame};

const CONSOLE: &str = "test-console-1";

/// The fake plugin's program. `cargo test` builds examples, but `cargo test --test
/// plugin_host` does not, so cargo is asked for it when needed.
fn fake_exe() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        // target/<profile>/deps/plugin_host-<hash>.exe -> target/<profile>
        let me = std::env::current_exe().expect("test program path");
        let profile_dir = me.parent().and_then(Path::parent).expect("target profile folder");
        let exe = profile_dir
            .join("examples")
            .join(format!("fake_plugin{}", std::env::consts::EXE_SUFFIX));
        // Build only when it is missing or older than its source: a nested cargo run is slow.
        let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fake_plugin.rs");
        let fresh = match (modified(&exe), modified(&source)) {
            (Some(built), Some(edited)) => built >= edited,
            _ => false,
        };
        if !fresh {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut build = Command::new(cargo);
            build
                .args(["build", "--example", "fake_plugin", "--manifest-path"])
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
            if profile_dir.file_name().is_some_and(|n| n == "release") {
                build.arg("--release");
            }
            let built = build.status().map(|s| s.success()).unwrap_or(false);
            assert!(
                built && exe.is_file(),
                "could not build the fake plugin (cargo build --example fake_plugin), expected at {}",
                exe.display()
            );
        }
        exe
    })
    .clone()
}

/// A temporary config folder with the fake plugin installed in `plugins/fake`.
struct Install {
    root: PathBuf,
    info: PluginInfo,
}

impl Install {
    /// `fault`: one of the fake plugin's misbehaviours ("" for none), passed through "args"
    /// in the manifest.
    fn new(tag: &str, fault: &str) -> Self {
        let root = std::env::temp_dir().join(format!("xmouse-plugin-e2e-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("plugins").join("fake");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(
            fake_exe(),
            dir.join(format!("fake_plugin{}", std::env::consts::EXE_SUFFIX)),
        )
        .unwrap();
        // The sample manifest shipped with the example, as it is (so the sample is tested too).
        let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fake_plugin/plugin.json");
        let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(sample).unwrap()).unwrap();
        if !fault.is_empty() {
            manifest["args"] = json!(["--fault", fault]);
        }
        std::fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();

        let (mut found, skipped) = plugin::discover_in(&root.join("plugins"));
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(found.len(), 1);
        Self { root, info: found.remove(0) }
    }

    fn paired(tag: &str, fault: &str) -> Self {
        let me = Self::new(tag, fault);
        if fault != "hang" && fault != "bad-api" {
            plugin::pair(&me.info, CONSOLE, "12345678", &BTreeMap::new()).expect("pairing");
        } else {
            // These never get as far as pairing; write what pairing would have left behind.
            std::fs::create_dir_all(&me.info.data_dir).unwrap();
            std::fs::write(me.info.data_dir.join("paired"), b"1").unwrap();
        }
        me
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        // Best effort: Windows may hold the program file for a moment after it was stopped.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A mapper where Space is the A button.
fn shared() -> Arc<Shared> {
    let mut profile = builtin_profiles().remove(0);
    profile.layers = vec![Layer {
        name: "Default".into(),
        bindings: vec![Binding {
            input: "Space".into(),
            action: Action::A,
            mode: BindMode::Hold,
            switch_layer: None,
            label: String::new(),
        }],
    }];
    Shared::new(Mapper::new(profile))
}

fn config() -> PluginSessionConfig {
    PluginSessionConfig {
        mode: PlayMode::Pc,
        input_hz: 120,
        play_audio: false, // decode the sound, but keep the test machine quiet
        stats: Arc::new(PluginStats::default()),
        ..Default::default()
    }
}

fn start(install: &Install, cfg: PluginSessionConfig, shared: &Arc<Shared>) -> JoinHandle<()> {
    tokio::spawn(plugin::run_plugin_session(
        install.info.clone(),
        CONSOLE.to_string(),
        cfg,
        shared.clone(),
        Arc::new(|| {}),
    ))
}

async fn wait_until(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Size and the colour in the lower middle of the newest decoded picture (the flat part of
/// the fake plugin's test picture).
fn picture(shared: &Shared) -> Option<([usize; 2], [i32; 3])> {
    match shared.video.lock().unwrap().as_ref()? {
        VideoFrame::Rgba(img) => {
            let [w, h] = img.size;
            let p = img.pixels[(h * 3 / 4) * w + w / 2];
            Some(([w, h], [p.r() as i32, p.g() as i32, p.b() as i32]))
        }
        VideoFrame::Nv12 { .. } => None,
    }
}

/// Run a session that is expected to end by itself with an error; returns the error text.
async fn failing_session(install: &Install, cfg: PluginSessionConfig) -> String {
    let shared = shared();
    let task = start(install, cfg, &shared);
    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("the session ends by itself")
        .expect("the session task did not panic");
    let status = shared.status();
    assert_eq!(status.phase, Phase::Ended);
    status.error.expect("an error is reported")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lists_pairs_and_plays_through_the_fake_plugin() {
    let _ = env_logger::builder().is_test(true).try_init();
    let install = Install::new("play", "");
    let info = install.info.clone();
    assert_eq!((info.id.as_str(), info.platform.as_str()), ("fake", "Test Console"));
    assert_eq!(info.license, "CC-BY-NC-SA-4.0");

    // The blocking calls go on a blocking thread, as the app would do it.
    let blocking = {
        let info = info.clone();
        tokio::task::spawn_blocking(move || {
            let consoles = plugin::list_consoles(&info).unwrap();
            assert_eq!(consoles.len(), 1);
            assert_eq!(consoles[0].id, CONSOLE);
            assert_eq!(consoles[0].name, "Test Console");
            assert_eq!(consoles[0].state, ConsoleState::On);
            assert!(consoles[0].needs_pairing);

            let none = BTreeMap::new();
            let wrong = plugin::pair(&info, CONSOLE, "00000000", &none).unwrap_err();
            assert_eq!(wrong.to_string(), "Wrong PIN");
            // Still unpaired after the wrong PIN.
            assert!(plugin::list_consoles(&info).unwrap()[0].needs_pairing);
        })
    };
    blocking.await.unwrap();

    // Starting before pairing: the plugin refuses, and the session ends with its reason.
    let refused = failing_session(&install, config()).await;
    assert!(refused.contains("not paired"), "{refused}");

    let blocking = {
        let info = info.clone();
        tokio::task::spawn_blocking(move || {
            plugin::pair(&info, CONSOLE, "12345678", &BTreeMap::new()).unwrap();
            // The pairing was kept by the plugin (in its data folder), across restarts.
            assert!(!plugin::list_consoles(&info).unwrap()[0].needs_pairing);
            plugin::wake(&info, CONSOLE).unwrap();
            assert_eq!(
                plugin::wake(&info, "nope").unwrap_err().to_string(),
                "Unknown console"
            );
        })
    };
    blocking.await.unwrap();
    assert!(info.data_dir.join("paired").is_file());

    // Play.
    let shared = shared();
    let cfg = config();
    let stats = cfg.stats.clone();
    let task = start(&install, cfg, &shared);
    wait_until("connected", 20, || {
        assert!(!task.is_finished(), "session ended early: {:?}", shared.status());
        shared.status().phase == Phase::Connected
    })
    .await;

    // Decoded pictures arrive in the same mailbox the Xbox path fills.
    wait_until("a decoded picture", 20, || picture(&shared).is_some()).await;
    let (size, [r, _, b]) = picture(&shared).unwrap();
    assert_eq!(size, [640, 360]);
    assert!(b > r + 40, "blue background while nothing is pressed (r={r} b={b})");
    wait_until("sound", 10, || stats.audio_decoded.load(Ordering::Relaxed) > 10).await;
    assert_eq!(stats.rumble_events.load(Ordering::Relaxed), 0);

    // Hold A: the plugin answers with vibration and paints the picture red.
    shared.mapper.lock().unwrap().press("Space", shared.now_ms());
    wait_until("input acknowledged (rumble)", 10, || {
        *stats.rumble.lock().unwrap() == (1.0, 0.5)
    })
    .await;
    wait_until("the picture turning red", 10, || {
        picture(&shared).is_some_and(|(_, [r, _, b])| r > b + 80)
    })
    .await;

    // Let go: back to blue, vibration off.
    shared.mapper.lock().unwrap().release("Space");
    wait_until("rumble off", 10, || *stats.rumble.lock().unwrap() == (0.0, 0.0)).await;
    wait_until("the picture turning blue again", 10, || {
        picture(&shared).is_some_and(|(_, [r, _, b])| b > r + 40)
    })
    .await;

    // A keyframe request from the decoder side reaches the plugin and the stream carries on.
    xmouse::stream::request_keyframe(&shared);
    let before = shared.video_frames.load(Ordering::Relaxed);
    wait_until("more pictures after a keyframe request", 10, || {
        shared.video_frames.load(Ordering::Relaxed) > before + 10
    })
    .await;

    assert!(shared.frames_sent.load(Ordering::Relaxed) > 30);
    assert!(stats.video_units.load(Ordering::Relaxed) > 10);
    assert_eq!(shared.status().phase, Phase::Connected);

    // Stop from the UI side: ends promptly and without an error.
    shared.stop();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("stops")
        .expect("joins");
    let status = shared.status();
    assert_eq!(status.phase, Phase::Ended);
    assert_eq!(status.error, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tv_play_sends_input_but_decodes_nothing() {
    let install = Install::paired("tv", "");
    let shared = shared();
    let cfg = PluginSessionConfig { mode: PlayMode::Tv, ..config() };
    let stats = cfg.stats.clone();
    let task = start(&install, cfg, &shared);
    wait_until("connected", 20, || shared.status().phase == Phase::Connected).await;
    wait_until("media being received", 20, || stats.video_units.load(Ordering::Relaxed) > 15).await;
    assert!(shared.frames_sent.load(Ordering::Relaxed) > 0);
    assert_eq!(shared.video_frames.load(Ordering::Relaxed), 0, "no decoding in TV Play");
    assert_eq!(stats.audio_decoded.load(Ordering::Relaxed), 0);
    assert!(shared.video.lock().unwrap().is_none());
    shared.stop();
    tokio::time::timeout(Duration::from_secs(10), task).await.expect("stops").unwrap();
    assert_eq!(shared.status().error, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nonsense_between_good_frames_is_skipped() {
    let install = Install::paired("badjson", "bad-json");
    let shared = shared();
    let cfg = config();
    let stats = cfg.stats.clone();
    let task = start(&install, cfg, &shared);
    // The plugin slips rubbish in after every 10th picture; play well past several rounds.
    wait_until("the stream to carry on", 30, || {
        assert!(!task.is_finished(), "session ended: {:?}", shared.status());
        stats.video_units.load(Ordering::Relaxed) > 60
    })
    .await;
    assert_eq!(shared.status().phase, Phase::Connected);
    assert!(picture(&shared).is_some());
    shared.stop();
    tokio::time::timeout(Duration::from_secs(10), task).await.expect("stops").unwrap();
    assert_eq!(shared.status().error, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_that_breaks_the_protocol_or_dies_ends_the_session_with_an_error() {
    for (fault, expect) in [
        ("oversize", "limit"),
        ("truncate", "middle of a frame"),
        ("exit", "exited unexpectedly"),
        ("hevc", "HEVC"),
        ("bad-api", "API 99"),
    ] {
        let install = Install::paired(fault, fault);
        let error = failing_session(&install, config()).await;
        assert!(error.contains(expect), "{fault}: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_that_hangs_is_given_up_on() {
    // Never answers hello.
    let install = Install::paired("hang", "hang");
    let started = Instant::now();
    let cfg = PluginSessionConfig { handshake_timeout: Duration::from_millis(700), ..config() };
    let error = failing_session(&install, cfg).await;
    assert!(error.contains("did not answer"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(15));
    // The blocking calls give up as well (they use the standard 10 s here, so only check
    // that a broken plugin produces an error rather than a list).
    drop(install);

    // Says hello, accepts start, but never gets connected.
    let install = Install::paired("noconnect", "no-connect");
    let cfg = PluginSessionConfig { connect_timeout: Duration::from_secs(1), ..config() };
    let error = failing_session(&install, cfg).await;
    assert!(error.contains("didn't respond in time"), "{error}");
    drop(install);

    // Connects and streams, but stops reading what the host sends.
    let install = Install::paired("stall", "stall");
    let cfg = PluginSessionConfig {
        input_hz: 250,
        stall_timeout: Duration::from_millis(500),
        ..config()
    };
    let error = failing_session(&install, cfg).await;
    assert!(error.contains("stopped responding"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_while_the_plugin_is_still_starting_is_clean() {
    let install = Install::paired("earlystop", "no-connect");
    let shared = shared();
    let task = start(&install, config(), &shared);
    wait_until("the plugin to report progress", 20, || {
        shared.status().phase == Phase::WaitingForConsole
    })
    .await;
    assert_eq!(shared.status().detail, "Waiting for ever…");
    shared.stop();
    tokio::time::timeout(Duration::from_secs(10), task).await.expect("stops").unwrap();
    assert_eq!(shared.status().phase, Phase::Ended);
    assert_eq!(shared.status().error, None);
}
