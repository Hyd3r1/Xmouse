//! A pretend console plugin for testing XMouse's plugin system. No real console is involved:
//! it offers one "Test Console", accepts the PIN 12345678, and streams a generated picture
//! (a white bar sliding over a blue background, 640x360 at 30 fps) with silent audio.
//! Holding A turns the background red, so input can be seen to make the round trip.
//!
//! It deliberately uses nothing from the `xmouse` crate: the protocol is written out here the
//! way a plugin in its own repository would write it, which makes this file both the test
//! partner for `tests/plugin_host.rs` and a worked example for plugin authors.
//!
//!   cargo build --example fake_plugin
//!   then copy the program and `examples/fake_plugin/plugin.json` to
//!   `<XMouse config dir>\plugins\fake\`
//!
//! `--fault <name>` (through "args" in plugin.json) makes it misbehave on purpose:
//! `hang` (never answers hello), `bad-api`, `no-connect` (never reaches "connected"),
//! `bad-json` (nonsense control messages between good frames), `oversize` (announces a
//! gigantic frame), `truncate` (stops mid-frame), `exit` (dies mid-session), `stall` (stops
//! reading its input), `hevc` (labels its video as HEVC).

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType};
use openh264::formats::YUVBuffer;
use openh264::OpenH264API;
use serde_json::{json, Value};

const API: u64 = 1;
const MAX_FRAME: usize = 8 * 1024 * 1024;
const FRAME_CONTROL: u8 = 1;
const FRAME_VIDEO: u8 = 2;
const FRAME_AUDIO: u8 = 3;
/// The A button in the Xbox bit layout XMouse sends.
const BUTTON_A: u32 = 16;
const CONSOLE_ID: &str = "test-console-1";
const PIN: &str = "12345678";

/// stdout is shared by the main thread (answers) and the streaming thread (media): a frame
/// must go out whole, so every write takes this lock.
type Out = Arc<Mutex<std::io::Stdout>>;

fn write_frame(out: &Out, kind: u8, parts: &[&[u8]]) {
    let len = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_le_bytes());
    buf.push(kind);
    for p in parts {
        buf.extend_from_slice(p);
    }
    let mut o = out.lock().unwrap();
    // XMouse closing the pipe means the session is over: nothing left to do but leave.
    if o.write_all(&buf).and_then(|_| o.flush()).is_err() {
        std::process::exit(0);
    }
}

fn send(out: &Out, msg: Value) {
    write_frame(out, FRAME_CONTROL, &[msg.to_string().as_bytes()]);
}

fn log(out: &Out, text: String) {
    send(out, json!({ "type": "log", "level": "info", "text": text }));
}

/// One frame from XMouse: type byte and payload. `None` when XMouse closed the pipe (or sent
/// something that can't be a frame).
fn read_frame(input: &mut impl Read) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 4];
    input.read_exact(&mut head).ok()?;
    let len = u32::from_le_bytes(head) as usize;
    if len == 0 || len > MAX_FRAME {
        return None;
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body).ok()?;
    let payload = body.split_off(1);
    Some((body[0], payload))
}

/// What the streaming thread needs to know from the main thread.
#[derive(Default)]
struct Live {
    stop: AtomicBool,
    keyframe: AtomicBool,
    buttons: AtomicU32,
}

struct StreamSettings {
    width: usize,
    height: usize,
    fps: u32,
    bitrate_kbps: u32,
    fault: String,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "--xmouse-plugin") {
        eprintln!("This is an XMouse test plugin; XMouse starts it with --xmouse-plugin.");
        std::process::exit(2);
    }
    let fault = args
        .iter()
        .position(|a| a == "--fault")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    eprintln!("fake plugin started (fault: {fault:?})"); // stderr ends up in XMouse's log

    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut input = std::io::stdin().lock();
    let live = Arc::new(Live::default());
    let mut config_dir = std::path::PathBuf::new();
    let mut stream: Option<std::thread::JoinHandle<()>> = None;
    let mut inputs = 0u64;
    let mut last_buttons = 0u32;

    while let Some((kind, payload)) = read_frame(&mut input) {
        if kind != FRAME_CONTROL {
            continue; // a frame type from a newer XMouse: skip it
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&payload) else {
            continue;
        };
        match msg["type"].as_str().unwrap_or("") {
            "hello" => {
                if fault == "hang" {
                    std::thread::sleep(Duration::from_secs(3600));
                }
                config_dir = msg["config_dir"].as_str().unwrap_or(".").into();
                send(&out, json!({
                    "type": "hello", "name": "Fake Plugin", "version": "0.1.0",
                    "api": if fault == "bad-api" { 99 } else { API },
                    "capabilities": ["pairing", "wake", "rumble"],
                }));
            }
            "list_consoles" => {
                let paired = config_dir.join("paired").exists();
                send(&out, json!({ "type": "consoles", "consoles": [{
                    "id": CONSOLE_ID, "name": "Test Console", "model": "Fake",
                    "state": "on", "needs_pairing": !paired,
                }] }));
            }
            "pair" => {
                let ok = msg["console_id"] == CONSOLE_ID && msg["pin"] == PIN;
                if ok {
                    // Remember the pairing in the folder XMouse gave us, like a real plugin
                    // would keep its keys there.
                    let _ = std::fs::create_dir_all(&config_dir);
                    let _ = std::fs::write(config_dir.join("paired"), b"1");
                    send(&out, json!({ "type": "pair_result", "ok": true }));
                } else {
                    send(&out, json!({ "type": "pair_result", "ok": false, "error": "Wrong PIN" }));
                }
            }
            "wake" => {
                let ok = msg["console_id"] == CONSOLE_ID;
                send(&out, json!({ "type": "wake_result", "ok": ok,
                    "error": if ok { Value::Null } else { json!("Unknown console") } }));
            }
            "start" => {
                if msg["console_id"] != CONSOLE_ID {
                    send(&out, json!({ "type": "status", "phase": "ended", "error": "Unknown console" }));
                    continue;
                }
                if !config_dir.join("paired").exists() {
                    send(&out, json!({ "type": "status", "phase": "ended",
                        "error": "This console is not paired yet." }));
                    continue;
                }
                if stream.is_some() {
                    continue;
                }
                // Stay within what XMouse asked for (even sizes: 4:2:0 video).
                let fit = |want: u64, ours: usize| (want.clamp(64, ours as u64) as usize) & !1;
                let settings = StreamSettings {
                    width: fit(msg["max_width"].as_u64().unwrap_or(640), 640),
                    height: fit(msg["max_height"].as_u64().unwrap_or(360), 360),
                    fps: msg["fps"].as_u64().unwrap_or(30).clamp(5, 30) as u32,
                    bitrate_kbps: msg["bitrate_kbps"].as_u64().unwrap_or(0) as u32,
                    fault: fault.clone(),
                };
                log(&out, format!(
                    "start: mode={} {}x{}@{}",
                    msg["mode"], settings.width, settings.height, settings.fps
                ));
                let (out2, live2) = (out.clone(), live.clone());
                stream = Some(std::thread::spawn(move || run_stream(out2, live2, settings)));
                if fault == "stall" {
                    // Keep streaming but never read another message.
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            "input" => {
                let buttons = msg["buttons"].as_u64().unwrap_or(0) as u32;
                live.buttons.store(buttons, Ordering::Relaxed);
                inputs += 1;
                if buttons != last_buttons || inputs % 300 == 1 {
                    log(&out, format!(
                        "input #{inputs}: buttons={buttons:#06x} l=({:.2},{:.2}) r=({:.2},{:.2}) lt={:.2} rt={:.2}",
                        msg["lx"].as_f64().unwrap_or(0.0), msg["ly"].as_f64().unwrap_or(0.0),
                        msg["rx"].as_f64().unwrap_or(0.0), msg["ry"].as_f64().unwrap_or(0.0),
                        msg["lt"].as_f64().unwrap_or(0.0), msg["rt"].as_f64().unwrap_or(0.0),
                    ));
                }
                // Vibrate while A is held, so XMouse sees its input acknowledged.
                let (was, is) = (last_buttons & BUTTON_A != 0, buttons & BUTTON_A != 0);
                if was != is {
                    let level = if is { 1.0 } else { 0.0 };
                    send(&out, json!({ "type": "rumble", "left": level, "right": level / 2.0 }));
                }
                last_buttons = buttons;
            }
            "request_keyframe" => live.keyframe.store(true, Ordering::Relaxed),
            "stop" => break,
            _ => {} // a message from a newer XMouse: ignore it
        }
    }

    // Asked to stop, or XMouse went away: end the stream and leave.
    live.stop.store(true, Ordering::Relaxed);
    if let Some(t) = stream.take() {
        let _ = t.join();
        send(&out, json!({ "type": "status", "phase": "ended" }));
    }
}

fn run_stream(out: Out, live: Arc<Live>, s: StreamSettings) {
    for (phase, detail) in [("starting", "Starting the pretend console…"), ("connecting", "Connecting…")] {
        send(&out, json!({ "type": "status", "phase": phase, "detail": detail }));
        std::thread::sleep(Duration::from_millis(50));
    }
    if s.fault == "no-connect" {
        send(&out, json!({ "type": "status", "phase": "waiting", "detail": "Waiting for ever…" }));
        while !live.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
        }
        return;
    }

    let bitrate = if s.bitrate_kbps == 0 { 1500 } else { s.bitrate_kbps.clamp(200, 8000) };
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate * 1000))
        .max_frame_rate(FrameRate::from_hz(s.fps as f32));
    let mut encoder = match Encoder::with_api_config(OpenH264API::from_source(), config) {
        Ok(e) => e,
        Err(e) => {
            send(&out, json!({ "type": "status", "phase": "ended", "error": format!("No video encoder: {e}") }));
            return;
        }
    };
    let mut opus = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).ok();
    send(&out, json!({ "type": "status", "phase": "connected" }));

    let (w, h) = (s.width, s.height);
    let start = Instant::now();
    let (mut frames, mut packets) = (0u64, 0u64);
    let mut phase = 0f32;
    let mut pcm = Vec::with_capacity(960 * 2);
    let mut packet = vec![0u8; 1500];
    while !live.stop.load(Ordering::Relaxed) {
        let elapsed = start.elapsed();

        // Video: one frame every 1/fps seconds.
        if elapsed >= Duration::from_micros(frames * 1_000_000 / s.fps as u64) {
            let held = live.buttons.load(Ordering::Relaxed) & BUTTON_A != 0;
            let picture = test_picture(w, h, frames, held);
            if live.keyframe.swap(false, Ordering::Relaxed) {
                encoder.force_intra_frame();
            }
            let (data, key) = match encoder.encode(&picture) {
                Ok(bits) => (
                    bits.to_vec(),
                    matches!(bits.frame_type(), FrameType::IDR | FrameType::I),
                ),
                Err(e) => {
                    send(&out, json!({ "type": "status", "phase": "ended", "error": format!("Encoder failed: {e}") }));
                    return;
                }
            };
            frames += 1;
            if !data.is_empty() {
                let codec = if s.fault == "hevc" { 1u8 } else { 0 };
                let pts = (elapsed.as_micros() as u64).to_le_bytes();
                write_frame(&out, FRAME_VIDEO, &[&[codec, key as u8], &pts, &data]);
            }
            misbehave(&out, &s.fault, frames);
        }

        // Audio: a 20 ms Opus packet every 20 ms. Silent on purpose: a test tone played through
        // someone's speakers for a whole session is painful. (Tests still decode it.)
        if elapsed >= Duration::from_millis(packets * 20) {
            if let Some(enc) = opus.as_mut() {
                pcm.clear();
                for _ in 0..960 {
                    phase = (phase + 440.0 * std::f32::consts::TAU / 48_000.0) % std::f32::consts::TAU;
                    let v = phase.sin() * 0.0;
                    pcm.extend_from_slice(&[v, v]);
                }
                if let Ok(n) = enc.encode_float(&pcm, &mut packet) {
                    write_frame(&out, FRAME_AUDIO, &[&[0u8, 2], &48_000u32.to_le_bytes(), &packet[..n]]);
                }
            }
            packets += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The deliberate protocol violations, for the host's tests.
fn misbehave(out: &Out, fault: &str, frames: u64) {
    match fault {
        // Nonsense inside valid frames, again and again: XMouse should skip it and carry on.
        "bad-json" if frames % 10 == 0 && frames <= 100 => {
            write_frame(out, FRAME_CONTROL, &[b"{ this is not json"]);
            write_frame(out, FRAME_VIDEO, &[&[0u8, 0]]); // video frame without a picture
            write_frame(out, 250, &[b"a frame type nobody knows"]);
            send(out, json!({ "type": "a_message_nobody_knows", "x": 1 }));
        }
        "oversize" if frames == 15 => {
            let mut o = out.lock().unwrap();
            let _ = o.write_all(&0x7fff_ffffu32.to_le_bytes());
            let _ = o.flush();
        }
        "truncate" if frames == 15 => {
            let mut o = out.lock().unwrap();
            let _ = o.write_all(&1000u32.to_le_bytes());
            let _ = o.write_all(&[FRAME_VIDEO, 0, 0, 1, 2, 3]);
            let _ = o.flush();
            std::process::exit(0);
        }
        "exit" if frames == 15 => std::process::exit(3),
        _ => {}
    }
}

/// I420 test picture: a white bar sliding across the top quarter; the rest one flat colour
/// (blue normally, red while A is held).
fn test_picture(w: usize, h: usize, frame: u64, a_held: bool) -> YUVBuffer {
    let (y, u, v) = if a_held { (81u8, 90u8, 240u8) } else { (110, 180, 100) };
    let (cw, ch) = (w / 2, h / 2);
    let mut data = vec![y; w * h + 2 * cw * ch];
    data[w * h..w * h + cw * ch].fill(u);
    data[w * h + cw * ch..].fill(v);
    let bar = (frame as usize * 8) % w;
    for row in 0..h / 4 {
        for x in bar..(bar + 40).min(w) {
            data[row * w + x] = 235;
        }
    }
    YUVBuffer::from_vec(data, w, h)
}
