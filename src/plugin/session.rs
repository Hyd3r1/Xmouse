//! A play session through a plugin. Does for a plugin console what `stream::run_session` does
//! for an Xbox: reports progress through `Shared`, feeds the video into the decoder and the
//! sound into the audio output, sends the mapper's controller state at the input rate, passes
//! on keyframe requests, and stops cleanly when the UI asks.
//!
//! The plugin is another program and may misbehave in any way. Whatever it does ends, at
//! worst, in `Phase::Ended` with an explanation: never in a panic or a hung session.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::TrySendError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use tokio::sync::mpsc as tmpsc;

use super::host::{check_hello, host_hello, Incoming, PluginProcess, SendError};
use super::manifest::PluginInfo;
use super::proto::{clean_text, AudioPacket, FromPlugin, StatusPhase, ToPlugin, VideoCodec, VideoUnit};
use crate::audio::AudioOut;
use crate::storage::PlayMode;
use crate::stream::{request_keyframe, Phase, Repaint, Shared};
use crate::video::{spawn_unit_decoder, VideoIn};

/// Numbers a session keeps for the HUD, the log and tests.
#[derive(Default)]
pub struct PluginStats {
    /// Video access units received from the plugin.
    pub video_units: AtomicU64,
    /// Access units thrown away because the decoder could not keep up.
    pub video_dropped: AtomicU64,
    /// Audio packets received from the plugin.
    pub audio_packets: AtomicU64,
    /// Audio packets that were decoded (PC Play only).
    pub audio_decoded: AtomicU64,
    /// How many vibration messages arrived, and the latest one (left, right; 0..1).
    pub rumble_events: AtomicU64,
    pub rumble: Mutex<(f32, f32)>,
}

#[derive(Clone)]
pub struct PluginSessionConfig {
    /// TV Play: the console's own screen is used, media from the plugin is thrown away.
    /// PC Play: the stream is decoded and shown here.
    pub mode: PlayMode,
    /// The largest picture to ask the plugin for.
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    /// Video bitrate in kbps (0 = let the plugin decide).
    pub bitrate_kbps: u32,
    /// Controller updates per second (30-250, as for the Xbox).
    pub input_hz: u32,
    /// Play the sound on this PC (PC Play). Off: it is decoded but not played.
    pub play_audio: bool,
    /// How long the plugin gets to answer `hello`.
    pub handshake_timeout: Duration,
    /// How long the plugin gets from `start` to `connected` (a sleeping console takes a while).
    pub connect_timeout: Duration,
    /// A plugin that hasn't read its input for this long is considered hung.
    pub stall_timeout: Duration,
    pub stats: Arc<PluginStats>,
}

impl Default for PluginSessionConfig {
    fn default() -> Self {
        Self {
            mode: PlayMode::Pc,
            max_width: 1920,
            max_height: 1080,
            fps: 60,
            bitrate_kbps: 0,
            input_hz: 60,
            play_audio: true,
            handshake_timeout: super::host::HELLO_TIMEOUT,
            connect_timeout: Duration::from_secs(120),
            stall_timeout: Duration::from_secs(5),
            stats: Arc::new(PluginStats::default()),
        }
    }
}

/// What the reader thread tells the session loop.
enum Ctl {
    Msg(FromPlugin),
    /// The plugin is gone; why.
    Closed(String),
    /// The session can't go on (said in the user's terms).
    Fatal(String),
}

/// Full session with a plugin console: start the plugin, connect, play until stopped or ended.
/// Spawn it on the runtime like `run_session`; watch `shared.status()` for progress.
pub async fn run_plugin_session(
    plugin: PluginInfo,
    console_id: String,
    cfg: PluginSessionConfig,
    shared: Arc<Shared>,
    repaint: Repaint,
) {
    let result = run_inner(&plugin, &console_id, cfg, &shared, &repaint).await;
    if let Err(e) = result {
        if !shared.stopped() {
            log::warn!("plugin session ({}) failed: {e:#}", plugin.id);
            shared.fail(format!("{e:#}"));
        }
    }
    shared.set(Phase::Ended, "Session ended");
    repaint();
}

async fn run_inner(
    plugin: &PluginInfo,
    console_id: &str,
    cfg: PluginSessionConfig,
    shared: &Arc<Shared>,
    repaint: &Repaint,
) -> Result<()> {
    let name = plugin.name.as_str();
    let mut stop_rx = shared.stop_signal();
    if shared.stopped() {
        return Ok(());
    }
    shared.set(Phase::Starting, &format!("Starting the {name} plugin…"));
    repaint();

    // Messages go to this loop through a short queue (the reader thread waits when it is
    // full, which in turn makes the plugin wait: nothing piles up). Media never comes
    // through here; the reader thread hands it straight to the decoders.
    let (ctl_tx, mut ctl_rx) = tmpsc::channel::<Ctl>(64);
    let streaming = Arc::new(AtomicBool::new(false));
    let mut media = Media {
        shared: shared.clone(),
        repaint: repaint.clone(),
        decode: cfg.mode == PlayMode::Pc,
        play_audio: cfg.play_audio,
        stats: cfg.stats.clone(),
        streaming: streaming.clone(),
        ctl: ctl_tx.clone(),
        video_tx: None,
        dropping: false,
        audio: None,
        audio_failed: false,
        refused: false,
    };
    let proc = PluginProcess::spawn(plugin, move |msg| match msg {
        Incoming::Video(v) => media.video(v),
        Incoming::Audio(a) => media.audio(a),
        // (If the session loop is gone these fail, which is fine: nobody is listening.)
        Incoming::Control(m) => drop(ctl_tx.blocking_send(Ctl::Msg(m))),
        Incoming::Closed(why) => drop(ctl_tx.blocking_send(Ctl::Closed(why))),
    })?;
    let send = |msg: &ToPlugin| {
        proc.send(msg)
            .map_err(|e| anyhow!("The {name} plugin can't be reached: {e}."))
    };

    // Hello, both ways.
    send(&host_hello(plugin))?;
    let hello_deadline = tokio::time::Instant::now() + cfg.handshake_timeout;
    loop {
        tokio::select! {
            m = ctl_rx.recv() => match m {
                Some(Ctl::Msg(FromPlugin::Hello { name, version, api, capabilities })) => {
                    let hello = check_hello(name, version, api, capabilities)?;
                    log::info!(
                        "plugin {}: {} {} ({})",
                        plugin.id, hello.name, hello.version, hello.capabilities.join(", ")
                    );
                    break;
                }
                Some(Ctl::Msg(_)) => {} // nothing else means anything before hello
                Some(Ctl::Closed(why)) => bail!("The {name} plugin stopped: {why}."),
                Some(Ctl::Fatal(e)) => bail!(e),
                None => bail!("The {name} plugin stopped."),
            },
            _ = tokio::time::sleep_until(hello_deadline) => {
                bail!("The {name} plugin did not answer. It may be broken or made for another version.")
            }
            _ = stop_rx.changed() => return Ok(()),
        }
    }

    send(&ToPlugin::Start {
        console_id: console_id.to_string(),
        mode: cfg.mode,
        max_width: cfg.max_width,
        max_height: cfg.max_height,
        fps: cfg.fps,
        bitrate_kbps: cfg.bitrate_kbps,
    })?;
    streaming.store(true, Ordering::SeqCst);

    let period = Duration::from_micros(1_000_000 / cfg.input_hz.clamp(30, 250) as u64);
    let mut input_tick = tokio::time::interval(period);
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(5));
    stats_tick.tick().await;
    let connect_deadline = tokio::time::Instant::now() + cfg.connect_timeout;
    let mut connected = false;
    // Since when the plugin has not been taking our messages.
    let mut stalled: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = input_tick.tick(), if connected => {
                let f = shared.frame();
                let unit = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
                let sent = proc.send(&ToPlugin::Input {
                    buttons: f.buttons as u32,
                    lx: unit(f.lx),
                    ly: unit(f.ly),
                    rx: unit(f.rx),
                    ry: unit(f.ry),
                    lt: unit(f.lt).max(0.0),
                    rt: unit(f.rt).max(0.0),
                });
                match sent {
                    Ok(()) => {
                        stalled = None;
                        shared.frames_sent.fetch_add(1, Ordering::Relaxed);
                    }
                    // Busy: it isn't reading. Closed: it is going away, and the reader
                    // thread will say why in a moment. Either way, don't wait for ever.
                    Err(SendError::Busy) | Err(SendError::Closed) => {
                        let since = *stalled.get_or_insert_with(Instant::now);
                        if since.elapsed() > cfg.stall_timeout {
                            bail!("The {name} plugin stopped responding.");
                        }
                    }
                }
                if shared.take_keyframe_request() && proc.send(&ToPlugin::RequestKeyframe).is_err() {
                    request_keyframe(shared); // not sent: ask again on the next tick
                }
            }
            m = ctl_rx.recv() => match m {
                Some(Ctl::Msg(FromPlugin::Status { phase, detail, error })) => {
                    let detail = clean_text(&detail, 200);
                    let or = |fallback: &str| if detail.is_empty() { fallback.to_string() } else { detail.clone() };
                    match phase {
                        StatusPhase::Ended => {
                            let error = error.map(|e| clean_text(&e, 300)).filter(|e| !e.is_empty());
                            return match error {
                                Some(e) => Err(anyhow!(e)),
                                None => Ok(()),
                            };
                        }
                        // Once playing, only `ended` changes anything.
                        _ if connected => log::debug!("plugin {}: status {phase:?} ignored while connected", plugin.id),
                        StatusPhase::Starting => shared.set(Phase::Starting, &or("Starting…")),
                        StatusPhase::Waiting => shared.set(
                            Phase::WaitingForConsole,
                            &or(&format!("Waiting for your {}…", plugin.platform)),
                        ),
                        StatusPhase::Connecting => shared.set(Phase::Connecting, &or("Connecting…")),
                        StatusPhase::Connected => {
                            connected = true;
                            input_tick.reset();
                            shared.set(Phase::Connected, "Connected");
                        }
                    }
                    repaint();
                }
                Some(Ctl::Msg(FromPlugin::Rumble { left, right })) => {
                    let level = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                    *cfg.stats.rumble.lock().unwrap() = (level(left), level(right));
                    cfg.stats.rumble_events.fetch_add(1, Ordering::Relaxed);
                }
                Some(Ctl::Msg(other)) => log::debug!("plugin {}: unexpected {other:?}", plugin.id),
                Some(Ctl::Closed(why)) => bail!("The {name} plugin stopped: {why}."),
                Some(Ctl::Fatal(e)) => bail!(e),
                None => bail!("The {name} plugin stopped."),
            },
            _ = tokio::time::sleep_until(connect_deadline), if !connected => {
                bail!("Your {} didn't respond in time.", plugin.platform);
            }
            _ = stats_tick.tick() => {
                let s = &cfg.stats;
                log::info!(
                    "plugin stats: input sent={} video units={} (dropped {}) decoded frames={} audio packets={}",
                    shared.frames_sent.load(Ordering::Relaxed),
                    s.video_units.load(Ordering::Relaxed),
                    s.video_dropped.load(Ordering::Relaxed),
                    shared.video_frames.load(Ordering::Relaxed),
                    s.audio_packets.load(Ordering::Relaxed),
                );
            }
            _ = stop_rx.changed() => {
                // Let go of every button, then ask the plugin to end the session and give it
                // a moment to say goodbye to the console before it is stopped for good.
                let _ = proc.send(&ToPlugin::Input { buttons: 0, lx: 0.0, ly: 0.0, rx: 0.0, ry: 0.0, lt: 0.0, rt: 0.0 });
                let _ = proc.send(&ToPlugin::Stop);
                let deadline = Instant::now() + Duration::from_millis(1500);
                while !proc.exited() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                proc.kill();
                return Ok(());
            }
        }
    }
    // (Every way out drops `proc`, which stops the plugin's program.)
}

/// Lives on the plugin's reader thread: takes video and audio straight to the decoders,
/// without a hop through the session loop (a hop is latency).
struct Media {
    shared: Arc<Shared>,
    repaint: Repaint,
    /// PC Play. In TV Play media is only counted.
    decode: bool,
    play_audio: bool,
    stats: Arc<PluginStats>,
    /// Set once `start` was sent; anything before that is ignored.
    streaming: Arc<AtomicBool>,
    ctl: tmpsc::Sender<Ctl>,
    /// The decoder thread, started with the first picture.
    video_tx: Option<std::sync::mpsc::SyncSender<VideoIn>>,
    /// The decoder fell behind and frames were dropped: wait for a full picture to resume.
    dropping: bool,
    audio: Option<AudioOut>,
    audio_failed: bool,
    /// The plugin sent video this build can't decode (reported once).
    refused: bool,
}

impl Media {
    fn video(&mut self, unit: VideoUnit) {
        if !self.streaming.load(Ordering::Relaxed) {
            return;
        }
        self.shared.video_packets.fetch_add(1, Ordering::Relaxed);
        self.stats.video_units.fetch_add(1, Ordering::Relaxed);
        if !self.decode {
            return;
        }
        if unit.codec != VideoCodec::H264 {
            if !self.refused {
                self.refused = true;
                let _ = self.ctl.blocking_send(Ctl::Fatal(
                    "The plugin sent HEVC video, which this version of XMouse can't decode yet.".into(),
                ));
            }
            return;
        }
        let (shared, repaint) = (&self.shared, &self.repaint);
        let tx = self
            .video_tx
            .get_or_insert_with(|| spawn_unit_decoder(shared.clone(), repaint.clone()));
        if self.dropping && !(unit.keyframe || has_idr(&unit.data)) {
            // These build on frames that were thrown away: they would only decode to rubbish.
            self.stats.video_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match tx.try_send(VideoIn::Unit(unit.data)) {
            Ok(()) => self.dropping = false,
            Err(TrySendError::Full(_)) => {
                // The decoder is far behind. Drop frames rather than queue them (memory, and
                // a queue is latency), and resume from the next full picture.
                if !self.dropping {
                    log::warn!("video decoder can't keep up: dropping frames until the next full picture");
                }
                self.dropping = true;
                self.stats.video_dropped.fetch_add(1, Ordering::Relaxed);
                request_keyframe(&self.shared);
            }
            // The decoder thread has ended (session stopping, or no decoder available).
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    fn audio(&mut self, packet: AudioPacket) {
        if !self.streaming.load(Ordering::Relaxed) {
            return;
        }
        self.stats.audio_packets.fetch_add(1, Ordering::Relaxed);
        // TV Play: the sound is on the TV. An empty packet would be taken for a lost one.
        if !self.decode || self.audio_failed || packet.data.is_empty() {
            return;
        }
        if self.audio.is_none() {
            let out = if self.play_audio {
                AudioOut::new(self.shared.clone())
            } else {
                AudioOut::without_device()
            };
            match out {
                Ok(out) => {
                    log::info!(
                        "plugin audio: Opus, {} Hz, {} channels",
                        packet.sample_rate, packet.channels
                    );
                    self.audio = Some(out);
                }
                Err(e) => {
                    log::error!("Opus decoder unavailable: {e}");
                    self.audio_failed = true;
                    return;
                }
            }
        }
        if let Some(out) = self.audio.as_mut() {
            out.packet(&packet.data);
            self.stats.audio_decoded.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Whether an Annex-B access unit contains an IDR slice (a full picture), for plugins that
/// don't set the keyframe flag.
fn has_idr(data: &[u8]) -> bool {
    data.windows(4)
        .any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_idr_slices_in_annex_b() {
        assert!(has_idr(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x65, 9]));
        assert!(has_idr(&[0, 0, 1, 0x25]));
        assert!(!has_idr(&[0, 0, 0, 1, 0x41, 9, 9, 9]));
        assert!(!has_idr(&[0, 0, 1]));
        assert!(!has_idr(&[]));
    }
}
