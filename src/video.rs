//! PC Play video: reassemble H.264 from RTP and decode it with OpenH264 on a background thread.
//!
//! Latency rules: the decoder never builds a backlog. Every frame is decoded (H.264 needs the
//! reference frames), but only the newest decoded frame of each batch is converted to pixels and
//! handed to the UI; older ones are skipped. The UI shows whatever is newest (a one-slot mailbox).

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use webrtc::rtp::codecs::h264::H264Packet;
use webrtc::rtp::packet::Packet;
use webrtc::rtp::packetizer::Depacketizer;

use crate::stream::{request_keyframe, Repaint, Shared, VideoFrame};

/// A frame missing a packet for this long is given up (a resend on the LAN takes a few ms).
const GAP_TIMEOUT: Duration = Duration::from_millis(60);
/// ...or once this many newer packets are waiting behind the gap.
const MAX_PENDING: usize = 1500;

/// Builds H.264 access units from RTP and releases each one the moment its last packet (marker
/// bit) is in and nothing before it is missing. webrtc-rs's SampleBuilder instead waits for the
/// first packet of the *next* frame, which cost a whole frame (16.7 ms at 60 fps) every time.
/// Packets may arrive out of order (NACK resends); they are kept by sequence number.
struct Assembler {
    pending: BTreeMap<u64, Packet>,
    /// Extended sequence number of the first packet of the next frame.
    next: Option<u64>,
    last_ext: Option<u64>,
    gap_since: Option<Instant>,
    depack: H264Packet,
}

impl Assembler {
    fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            next: None,
            last_ext: None,
            gap_since: None,
            depack: H264Packet::default(),
        }
    }

    /// 16-bit RTP sequence number → monotonically increasing 64-bit one.
    fn extend(&mut self, seq: u16) -> u64 {
        let ext = match self.last_ext {
            None => seq as u64 + (1 << 32),
            Some(last) => {
                let diff = seq.wrapping_sub(last as u16) as i16 as i64;
                (last as i64 + diff) as u64
            }
        };
        if self.last_ext.is_none_or(|l| ext > l) {
            self.last_ext = Some(ext);
        }
        ext
    }

    fn push(&mut self, pkt: Packet) {
        let ext = self.extend(pkt.header.sequence_number);
        if self.next.is_some_and(|n| ext < n) {
            return; // late duplicate of a frame already released
        }
        if self.next.is_none() {
            self.next = Some(ext);
        }
        self.pending.insert(ext, pkt);
    }

    /// Complete frames, oldest first. `lost` is set when a frame had to be given up.
    fn pop(&mut self, out: &mut Vec<Vec<u8>>) -> bool {
        let mut lost = false;
        loop {
            let Some(start) = self.next else { return lost };
            // Walk contiguous packets from the frame start to a marker bit.
            let mut seq = start;
            let mut end = None;
            while let Some(p) = self.pending.get(&seq) {
                if p.header.marker {
                    end = Some(seq);
                    break;
                }
                seq += 1;
            }
            if let Some(end) = end {
                self.gap_since = None;
                let mut frame = Vec::new();
                let mut ok = true;
                for s in start..=end {
                    let p = self.pending.remove(&s).expect("contiguous");
                    match self.depack.depacketize(&p.payload) {
                        Ok(b) => frame.extend_from_slice(&b),
                        Err(_) => ok = false,
                    }
                }
                self.next = Some(end + 1);
                if ok && !frame.is_empty() {
                    out.push(frame);
                }
                continue;
            }
            // Missing packet: wait a little for a resend, then skip to the next frame start.
            let since = *self.gap_since.get_or_insert_with(Instant::now);
            if since.elapsed() < GAP_TIMEOUT && self.pending.len() < MAX_PENDING {
                return lost;
            }
            let Some(marker) = self
                .pending
                .iter()
                .find(|(_, p)| p.header.marker)
                .map(|(s, _)| *s)
            else {
                return lost;
            };
            self.pending.retain(|s, _| *s > marker);
            self.next = Some(marker + 1);
            self.depack = H264Packet::default();
            self.gap_since = None;
            lost = true;
        }
    }
}

/// I420 planes (with strides) → NV12: the Y plane, then U and V interleaved at half resolution.
fn to_nv12(yuv: &impl YUVSource, w: usize, h: usize) -> Vec<u8> {
    let (ys, us, vs) = yuv.strides();
    let (y, u, v) = (yuv.y(), yuv.u(), yuv.v());
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = Vec::with_capacity(w * h + cw * ch * 2);
    for row in 0..h {
        out.extend_from_slice(&y[row * ys..row * ys + w]);
    }
    for row in 0..ch {
        let (ur, vr) = (&u[row * us..row * us + cw], &v[row * vs..row * vs + cw]);
        for i in 0..cw {
            out.push(ur[i]);
            out.push(vr[i]);
        }
    }
    out
}

pub fn spawn_decoder(shared: Arc<Shared>, repaint: Repaint) -> mpsc::Sender<Packet> {
    let (tx, rx) = mpsc::channel::<Packet>();
    std::thread::Builder::new()
        .name("xmouse-video".into())
        .spawn(move || {
            let mut decoder = match Decoder::new() {
                Ok(d) => d,
                Err(e) => {
                    log::error!("H.264 decoder unavailable: {e}");
                    return;
                }
            };
            let mut builder = Assembler::new();
            let mut errors = 0u32;
            let mut rgba: Vec<u8> = Vec::new();
            let mut samples: Vec<Vec<u8>> = Vec::new();
            let mut lost_frames = 0u64;
            let (mut decoded, mut shown, mut decode_ms) = (0u64, 0u64, 0f64);
            let mut window = Instant::now();
            while let Ok(pkt) = rx.recv() {
                // Take everything that has arrived, so a slow frame never leaves a queue behind.
                builder.push(pkt);
                while let Ok(more) = rx.try_recv() {
                    builder.push(more);
                }
                samples.clear();
                if builder.pop(&mut samples) {
                    // A frame was lost for good: the next ones reference it, ask for a fresh picture.
                    lost_frames += 1;
                    request_keyframe(&shared);
                }
                let last = samples.len().saturating_sub(1);
                for (i, sample) in samples.iter().enumerate() {
                    let t = Instant::now();
                    match decoder.decode(sample) {
                        Ok(Some(yuv)) => {
                            errors = 0;
                            decoded += 1;
                            decode_ms += t.elapsed().as_secs_f64() * 1000.0;
                            if i != last {
                                continue; // a newer frame is already here: don't draw this one
                            }
                            let (w, h) = yuv.dimensions();
                            if shared.video_frames.load(Ordering::Relaxed) == 0 {
                                log::info!("first decoded video frame: {w}x{h}");
                            }
                            let frame = if shared.want_nv12.load(Ordering::Relaxed) {
                                VideoFrame::Nv12 {
                                    width: w,
                                    height: h,
                                    data: to_nv12(&yuv, w, h),
                                }
                            } else {
                                rgba.resize(w * h * 4, 0);
                                yuv.write_rgba8(&mut rgba);
                                VideoFrame::Rgba(egui::ColorImage::from_rgba_premultiplied(
                                    [w, h],
                                    &rgba,
                                ))
                            };
                            *shared.video.lock().unwrap() = Some(frame);
                            shared.video_frames.fetch_add(1, Ordering::Relaxed);
                            shown += 1;
                            repaint();
                        }
                        Ok(None) => {}
                        Err(e) => {
                            errors += 1;
                            if errors == 1 || errors % 30 == 0 {
                                log::warn!("decode error ({errors}): {e}");
                                request_keyframe(&shared);
                            }
                        }
                    }
                }
                if window.elapsed().as_secs() >= 5 {
                    let secs = window.elapsed().as_secs_f64();
                    log::info!(
                        "video: decoded {:.0} fps, shown {:.0} fps, decode {:.1} ms/frame, lost frames {lost_frames}",
                        decoded as f64 / secs,
                        shown as f64 / secs,
                        if decoded > 0 {
                            decode_ms / decoded as f64
                        } else {
                            0.0
                        }
                    );
                    (decoded, shown, decode_ms) = (0, 0, 0.0);
                    window = Instant::now();
                }
                if shared.stopped() {
                    break;
                }
            }
        })
        .expect("spawn video thread");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use webrtc::rtp::header::Header;

    fn pkt(seq: u16, marker: bool, nal: u8) -> Packet {
        Packet {
            header: Header {
                sequence_number: seq,
                marker,
                timestamp: seq as u32,
                ..Default::default()
            },
            // Single NAL unit packet (type 1 = non-IDR slice); the depacketizer needs > 2 bytes.
            payload: Bytes::from(vec![0x41, nal, 0x00]),
        }
    }

    struct FakeYuv {
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    }
    impl YUVSource for FakeYuv {
        fn dimensions(&self) -> (usize, usize) {
            (4, 2)
        }
        fn strides(&self) -> (usize, usize, usize) {
            (6, 3, 3) // padded rows, like OpenH264's
        }
        fn y(&self) -> &[u8] {
            &self.y
        }
        fn u(&self) -> &[u8] {
            &self.u
        }
        fn v(&self) -> &[u8] {
            &self.v
        }
    }

    #[test]
    fn nv12_drops_stride_padding_and_interleaves_uv() {
        let f = FakeYuv {
            y: vec![1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0],
            u: vec![10, 11, 0],
            v: vec![20, 21, 0],
        };
        assert_eq!(
            to_nv12(&f, 4, 2),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 10, 20, 11, 21]
        );
    }

    #[test]
    fn releases_a_frame_on_its_marker_without_waiting_for_the_next() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        a.push(pkt(10, false, 1));
        assert!(!a.pop(&mut out));
        assert!(out.is_empty());
        a.push(pkt(11, true, 2));
        a.pop(&mut out);
        assert_eq!(
            out.len(),
            1,
            "frame released as soon as the marker packet arrived"
        );
    }

    #[test]
    fn waits_for_a_resent_packet_that_arrives_out_of_order() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        a.push(pkt(65534, false, 1));
        a.push(pkt(0, true, 3)); // 65535 missing; sequence wraps
        a.pop(&mut out);
        assert!(out.is_empty());
        a.push(pkt(65535, false, 2)); // the resend
        a.pop(&mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn gives_up_on_a_lost_packet_and_moves_on() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        a.push(pkt(1, false, 1));
        a.push(pkt(3, true, 3)); // 2 never comes
        a.push(pkt(4, true, 4)); // next frame, complete
        assert!(!a.pop(&mut out));
        std::thread::sleep(GAP_TIMEOUT + Duration::from_millis(10));
        assert!(a.pop(&mut out), "loss reported");
        assert_eq!(out.len(), 1, "the following frame still comes out");
    }
}
