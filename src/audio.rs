//! PC Play audio: decode the console's Opus stream and play it on the default output device.
//!
//! RTP → Opus decode (48 kHz stereo, packet-loss concealment on sequence gaps) → a small
//! jitter buffer → cpal output stream (resampled linearly if the device is not at 48 kHz).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use webrtc::track::track_remote::TrackRemote;

use crate::stream::Shared;

const RATE: u32 = 48_000;
/// Buffered audio to build up before (re)starting playback.
const TARGET_MS: usize = 40;
/// More than this is a backlog: skip back down to the target.
const MAX_MS: usize = 120;
const fn samples(ms: usize) -> usize {
    ms * RATE as usize / 1000 * 2 // interleaved stereo
}

struct Ring {
    buf: VecDeque<f32>,
    primed: bool,
}

type SharedRing = Arc<Mutex<Ring>>;

fn push(ring: &SharedRing, pcm: &[f32]) {
    let mut r = ring.lock().unwrap();
    r.buf.extend(pcm.iter().copied());
    if r.buf.len() > samples(MAX_MS) {
        let drop = r.buf.len() - samples(TARGET_MS);
        r.buf.drain(..drop);
    }
}

/// Read the audio track until it ends, playing it on this PC.
pub async fn play(track: Arc<TrackRemote>, shared: Arc<Shared>) {
    let ring: SharedRing = Arc::new(Mutex::new(Ring {
        buf: VecDeque::with_capacity(samples(MAX_MS) * 2),
        primed: false,
    }));
    spawn_output(ring.clone(), shared.clone());

    let mut dec = match opus::Decoder::new(RATE, opus::Channels::Stereo) {
        Ok(d) => d,
        Err(e) => {
            log::error!("Opus decoder unavailable: {e}");
            while track.read_rtp().await.is_ok() {}
            return;
        }
    };
    let mut pcm = vec![0f32; 5760 * 2]; // 120 ms, the largest Opus frame
    let mut last_seq: Option<u16> = None;
    let mut packets = 0u64;
    let mut errors = 0u64;
    while let Ok((pkt, _)) = track.read_rtp().await {
        packets += 1;
        if packets == 1 || packets % 3000 == 0 {
            log::info!("audio RTP packets: {packets}");
        }
        let seq = pkt.header.sequence_number;
        if let Some(prev) = last_seq {
            let gap = seq.wrapping_sub(prev);
            if gap == 0 || gap > 0x8000 {
                continue; // duplicate or late
            }
            // Conceal up to 3 lost 20 ms packets.
            for _ in 1..gap.min(4) {
                if let Ok(n) = dec.decode_float(&[], &mut pcm[..960 * 2], false) {
                    push(&ring, &pcm[..n * 2]);
                }
            }
        }
        last_seq = Some(seq);
        match dec.decode_float(&pkt.payload, &mut pcm, false) {
            Ok(n) => push(&ring, &pcm[..n * 2]),
            Err(e) => {
                errors += 1;
                if errors == 1 || errors % 100 == 0 {
                    log::warn!("Opus decode error ({errors}): {e}");
                }
            }
        }
        if shared.stopped() {
            break;
        }
    }
    log::info!("audio track ended after {packets} packets");
}

/// The cpal stream is not `Send`: own it on its own thread until the session stops.
fn spawn_output(ring: SharedRing, shared: Arc<Shared>) {
    let _ = std::thread::Builder::new()
        .name("xmouse-audio".into())
        .spawn(move || {
            let host = cpal::default_host();
            let Some(device) = host.default_output_device() else {
                log::warn!("no audio output device");
                return;
            };
            let supported = match device.default_output_config() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("audio output config: {e}");
                    return;
                }
            };
            let config = supported.config();
            log::info!(
                "audio output: {} Hz, {} channels, {:?}",
                config.sample_rate,
                config.channels,
                supported.sample_format()
            );
            let stream = match supported.sample_format() {
                SampleFormat::I16 => build::<i16>(&device, config, ring),
                SampleFormat::U16 => build::<u16>(&device, config, ring),
                SampleFormat::I32 => build::<i32>(&device, config, ring),
                _ => build::<f32>(&device, config, ring),
            };
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("could not open audio output: {e}");
                    return;
                }
            };
            if let Err(e) = stream.play() {
                log::warn!("could not start audio output: {e}");
                return;
            }
            while !shared.stopped() {
                std::thread::sleep(Duration::from_millis(100));
            }
            drop(stream);
        });
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ring: SharedRing,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let step = RATE as f64 / config.sample_rate.max(1) as f64;
    // Linear interpolation between `cur` and `next` (stereo frames at 48 kHz).
    let mut frac = 1.0f64;
    let mut cur = [0f32; 2];
    let mut next = [0f32; 2];
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            let mut r = ring.lock().unwrap();
            if !r.primed && r.buf.len() >= samples(TARGET_MS) {
                r.primed = true;
            }
            for frame in data.chunks_mut(channels) {
                let (l, rr) = if r.primed {
                    while frac >= 1.0 {
                        cur = next;
                        if r.buf.len() >= 2 {
                            next = [r.buf.pop_front().unwrap(), r.buf.pop_front().unwrap()];
                        } else {
                            // Underrun: fade to silence and build the buffer up again.
                            r.primed = false;
                            next = [0.0, 0.0];
                        }
                        frac -= 1.0;
                    }
                    let t = frac as f32;
                    let s = (
                        cur[0] + (next[0] - cur[0]) * t,
                        cur[1] + (next[1] - cur[1]) * t,
                    );
                    frac += step;
                    s
                } else {
                    (0.0, 0.0)
                };
                if channels == 1 {
                    frame[0] = T::from_sample((l + rr) * 0.5);
                } else {
                    for (i, out) in frame.iter_mut().enumerate() {
                        *out = T::from_sample(match i {
                            0 => l,
                            1 => rr,
                            _ => 0.0,
                        });
                    }
                }
            }
        },
        |e| log::warn!("audio output error: {e}"),
        None,
    )
}
