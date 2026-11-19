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

/// Opus packets in, sound out: the decoder plus the jitter buffer the output device reads from.
/// The Xbox RTP loop ([`play`]) and sources that deliver bare Opus packets both feed this.
pub struct AudioOut {
    ring: SharedRing,
    dec: opus::Decoder,
    pcm: Vec<f32>,
    errors: u64,
}

impl AudioOut {
    /// Open the default output device (on its own thread, until the session stops) and get
    /// ready to decode. Whatever the sender's sample rate or channel count, Opus decodes to
    /// the 48 kHz stereo the output expects.
    pub fn new(shared: Arc<Shared>) -> Result<Self, opus::Error> {
        let out = Self::without_device();
        if let Ok(out) = &out {
            spawn_output(out.ring.clone(), shared);
        }
        out
    }

    /// Decode into the buffer without opening an output device (muted sessions and tests).
    pub fn without_device() -> Result<Self, opus::Error> {
        Ok(Self {
            ring: Arc::new(Mutex::new(Ring {
                buf: VecDeque::with_capacity(samples(MAX_MS) * 2),
                primed: false,
            })),
            dec: opus::Decoder::new(RATE, opus::Channels::Stereo)?,
            pcm: vec![0f32; 5760 * 2], // 120 ms, the largest Opus frame
            errors: 0,
        })
    }

    /// Decode one Opus packet and queue it for playback. A packet that doesn't decode is
    /// skipped (and logged now and then); it never stops the sound.
    pub fn packet(&mut self, opus: &[u8]) {
        match self.dec.decode_float(opus, &mut self.pcm, false) {
            Ok(n) => push(&self.ring, &self.pcm[..n * 2]),
            Err(e) => {
                self.errors += 1;
                if self.errors == 1 || self.errors % 100 == 0 {
                    log::warn!("Opus decode error ({}): {e}", self.errors);
                }
            }
        }
    }

    /// One 20 ms packet went missing: let the decoder fill the hole (packet-loss concealment).
    pub fn conceal(&mut self) {
        if let Ok(n) = self.dec.decode_float(&[], &mut self.pcm[..960 * 2], false) {
            push(&self.ring, &self.pcm[..n * 2]);
        }
    }

    /// Samples (interleaved stereo) waiting to be played.
    pub fn buffered(&self) -> usize {
        self.ring.lock().unwrap().buf.len()
    }
}

/// What to do with an RTP packet, given the sequence number of the one before it.
#[derive(Debug, PartialEq)]
enum SeqStep {
    /// Duplicate or late: ignore it.
    Skip,
    /// Play it, after concealing this many lost packets (at most 3).
    Play { conceal: u16 },
}

fn seq_step(last: Option<u16>, seq: u16) -> SeqStep {
    let Some(prev) = last else {
        return SeqStep::Play { conceal: 0 };
    };
    let gap = seq.wrapping_sub(prev);
    if gap == 0 || gap > 0x8000 {
        SeqStep::Skip
    } else {
        SeqStep::Play {
            conceal: gap.min(4) - 1,
        }
    }
}

/// Read the audio track until it ends, playing it on this PC.
pub async fn play(track: Arc<TrackRemote>, shared: Arc<Shared>) {
    let mut out = match AudioOut::new(shared.clone()) {
        Ok(out) => out,
        Err(e) => {
            log::error!("Opus decoder unavailable: {e}");
            while track.read_rtp().await.is_ok() {}
            return;
        }
    };
    let mut last_seq: Option<u16> = None;
    let mut packets = 0u64;
    while let Ok((pkt, _)) = track.read_rtp().await {
        packets += 1;
        if packets == 1 || packets % 3000 == 0 {
            log::info!("audio RTP packets: {packets}");
        }
        let seq = pkt.header.sequence_number;
        match seq_step(last_seq, seq) {
            SeqStep::Skip => continue,
            SeqStep::Play { conceal } => {
                for _ in 0..conceal {
                    out.conceal();
                }
            }
        }
        last_seq = Some(seq);
        out.packet(&pkt.payload);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A quiet 440 Hz tone as 20 ms Opus packets (48 kHz stereo).
    fn tone_packets(n: usize) -> Vec<Vec<u8>> {
        let mut enc =
            opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        let mut phase = 0f32;
        (0..n)
            .map(|_| {
                let mut pcm = Vec::with_capacity(960 * 2);
                for _ in 0..960 {
                    phase += 440.0 * std::f32::consts::TAU / RATE as f32;
                    let s = phase.sin() * 0.1;
                    pcm.extend_from_slice(&[s, s]);
                }
                let mut out = vec![0u8; 1500];
                let len = enc.encode_float(&pcm, &mut out).unwrap();
                out.truncate(len);
                out
            })
            .collect()
    }

    /// The RTP sequence rules of the Xbox audio loop, unchanged by the split.
    #[test]
    fn rtp_sequence_rules() {
        assert_eq!(seq_step(None, 7), SeqStep::Play { conceal: 0 });
        assert_eq!(seq_step(Some(7), 8), SeqStep::Play { conceal: 0 });
        assert_eq!(seq_step(Some(7), 7), SeqStep::Skip, "duplicate");
        assert_eq!(seq_step(Some(7), 5), SeqStep::Skip, "late");
        assert_eq!(seq_step(Some(7), 9), SeqStep::Play { conceal: 1 });
        assert_eq!(seq_step(Some(7), 10), SeqStep::Play { conceal: 2 });
        assert_eq!(seq_step(Some(7), 11), SeqStep::Play { conceal: 3 });
        assert_eq!(seq_step(Some(7), 500), SeqStep::Play { conceal: 3 }, "at most 3");
        assert_eq!(seq_step(Some(65535), 0), SeqStep::Play { conceal: 0 }, "wraps");
        assert_eq!(seq_step(Some(65535), 1), SeqStep::Play { conceal: 1 });
        assert_eq!(seq_step(Some(0), 65535), SeqStep::Skip, "late across the wrap");
    }

    #[test]
    fn opus_packets_become_buffered_stereo_samples() {
        let mut out = AudioOut::without_device().unwrap();
        let packets = tone_packets(3);
        out.packet(&packets[0]);
        assert_eq!(out.buffered(), 960 * 2, "20 ms of stereo");
        out.conceal();
        assert_eq!(out.buffered(), 960 * 4, "a concealed packet is 20 ms too");
        out.packet(&packets[1]);
        // Rubbish is skipped without disturbing what is queued.
        out.packet(&[0xff; 7]);
        assert!(out.buffered() >= 960 * 6);
        let loud = out.ring.lock().unwrap().buf.iter().any(|s| s.abs() > 0.01);
        assert!(loud, "the tone came through");
    }

    #[test]
    fn buffer_never_grows_past_the_backlog_limit() {
        let mut out = AudioOut::without_device().unwrap();
        for p in tone_packets(40) {
            out.packet(&p); // 800 ms with nothing reading
            assert!(out.buffered() <= samples(MAX_MS));
        }
        assert!(out.buffered() >= samples(TARGET_MS));
    }
}