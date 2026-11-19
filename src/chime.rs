//! A short notification chime for on-screen tips: two soft bell tones a fifth apart, the
//! second a beat after the first. Synthesized here (an original sound, no audio files),
//! played once on its own output stream so it never touches the game audio.

use std::time::Duration;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

/// How long the chime rings, in seconds.
const LENGTH: f32 = 0.8;
/// Overall loudness (the game keeps playing underneath).
const VOLUME: f32 = 0.16;

/// Play the chime in the background. Failing to play it (no audio device) is not an error
/// worth showing anyone.
pub fn play_notification() {
    let _ = std::thread::Builder::new()
        .name("xmouse-chime".into())
        .spawn(|| {
            if let Err(e) = play() {
                log::debug!("notification sound: {e:#}");
            }
        });
}

/// The chime's waveform at `t` seconds.
pub fn sample_at(t: f32) -> f32 {
    // One bell tone: quick attack, smooth decay, a little octave and a fast-fading twelfth.
    let bell = |t: f32, start: f32, freq: f32| -> f32 {
        let t = t - start;
        if t < 0.0 {
            return 0.0;
        }
        let w = std::f32::consts::TAU * freq * t;
        let attack = (t / 0.006).min(1.0);
        let decay = (-t * 6.5).exp();
        attack * decay * (w.sin() + 0.35 * (2.0 * w).sin() + 0.12 * (3.0 * w).sin() * (-t * 14.0).exp())
    };
    // A5 then E6.
    VOLUME * (bell(t, 0.0, 880.0) + 0.9 * bell(t, 0.095, 1318.5))
}

fn play() -> Result<()> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device")?;
    let supported = device.default_output_config().context("audio output config")?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::I16 => build::<i16>(&device, config),
        SampleFormat::U16 => build::<u16>(&device, config),
        SampleFormat::I32 => build::<i32>(&device, config),
        _ => build::<f32>(&device, config),
    }
    .context("open audio output")?;
    stream.play().context("start audio output")?;
    std::thread::sleep(Duration::from_secs_f32(LENGTH + 0.15));
    Ok(())
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let rate = config.sample_rate.max(1) as f32;
    let mut n = 0u64;
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let t = n as f32 / rate;
                let s = if t < LENGTH { sample_at(t) } else { 0.0 };
                n += 1;
                for out in frame.iter_mut() {
                    *out = T::from_sample(s);
                }
            }
        },
        |e| log::debug!("notification sound output error: {e}"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chime_is_soft_short_and_never_clips() {
        let rate = 48_000.0;
        let samples: Vec<f32> = (0..(rate * LENGTH) as usize).map(|i| sample_at(i as f32 / rate)).collect();
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05 && peak < 0.5, "audible but soft, peak {peak}");
        // Rings out: the last 50 ms are near silence (no click when the stream stops).
        let tail = &samples[samples.len() - 2400..];
        assert!(tail.iter().all(|s| s.abs() < 0.01), "fades out");
        assert_eq!(sample_at(-0.1), 0.0);
    }

    /// Plays the chime on this machine's default output (run with `--ignored`).
    #[test]
    #[ignore]
    fn plays_on_the_default_output() {
        play().expect("chime played");
    }
}
