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

/// Where the decoder thread gets complete H.264 access units from. The decode loop is the same
/// for every source; only "how do bytes become access units" differs (RTP reassembly for the
/// Xbox, nothing at all for a source that already delivers whole access units).
trait FrameSource {
    type Item: Send + 'static;
    fn push(&mut self, item: Self::Item);
    /// Complete access units, oldest first. Returns true when a frame was lost for good.
    fn pop(&mut self, out: &mut Vec<Vec<u8>>) -> bool;
}

impl FrameSource for Assembler {
    type Item = Packet;
    fn push(&mut self, item: Packet) {
        Assembler::push(self, item)
    }
    fn pop(&mut self, out: &mut Vec<Vec<u8>>) -> bool {
        Assembler::pop(self, out)
    }
}

/// Input of [`spawn_unit_decoder`]: video that is already split into access units.
pub enum VideoIn {
    /// One complete access unit in Annex-B form (start codes between NAL units).
    Unit(Vec<u8>),
    /// A frame was dropped before it got here: the following ones can't be trusted until the
    /// next full picture, so the decoder asks for one.
    Lost,
}

/// Pass-through source: access units arrive ready-made.
#[derive(Default)]
struct Units {
    ready: Vec<Vec<u8>>,
    lost: bool,
}

impl FrameSource for Units {
    type Item = VideoIn;
    fn push(&mut self, item: VideoIn) {
        match item {
            VideoIn::Unit(data) if !data.is_empty() => self.ready.push(data),
            VideoIn::Unit(_) => {}
            VideoIn::Lost => self.lost = true,
        }
    }
    fn pop(&mut self, out: &mut Vec<Vec<u8>>) -> bool {
        out.append(&mut self.ready);
        std::mem::take(&mut self.lost)
    }
}

/// How many access units may wait for the decoder of [`spawn_unit_decoder`] (2 s at 60 fps).
/// The decoder drains its queue on every pass, so this only fills if decoding has stalled; the
/// sender then drops frames (`try_send`) instead of letting memory grow.
pub const UNIT_QUEUE: usize = 120;

/// Start the decoder thread for a source that delivers whole access units (no RTP). Same
/// decoding, GPU decoding, newest-frame-wins and keyframe-request behaviour as [`spawn_decoder`].
/// Use `try_send`: when the queue is full, drop the unit and send [`VideoIn::Lost`] once there
/// is room again.
pub fn spawn_unit_decoder(shared: Arc<Shared>, repaint: Repaint) -> mpsc::SyncSender<VideoIn> {
    let (tx, rx) = mpsc::sync_channel::<VideoIn>(UNIT_QUEUE);
    spawn_decode_loop(rx, Units::default, shared, repaint);
    tx
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

/// NV12 (BT.709, limited range, as the console sends it) → RGBA.
fn nv12_to_rgba(nv12: &[u8], w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 255);
    let uv = &nv12[w * h..];
    for row in 0..h {
        let y_row = &nv12[row * w..row * w + w];
        let uv_row = &uv[(row / 2) * w..];
        let px = &mut out[row * w * 4..(row + 1) * w * 4];
        for x in 0..w {
            let c = (y_row[x] as i32 - 16) * 298;
            let u = uv_row[x & !1] as i32 - 128;
            let v = uv_row[(x & !1) + 1] as i32 - 128;
            let clamp = |n: i32| ((n + 128) >> 8).clamp(0, 255) as u8;
            px[x * 4] = clamp(c + 459 * v);
            px[x * 4 + 1] = clamp(c - 55 * u - 136 * v);
            px[x * 4 + 2] = clamp(c + 541 * u);
        }
    }
}

/// Hand a decoded picture to the UI (one-slot mailbox: the newest wins).
fn publish(shared: &Shared, repaint: &Repaint, frame: VideoFrame, w: usize, h: usize) {
    if shared.video_frames.load(Ordering::Relaxed) == 0 {
        log::info!("first decoded video frame: {w}x{h}");
    }
    *shared.video.lock().unwrap() = Some(frame);
    shared.video_frames.fetch_add(1, Ordering::Relaxed);
    repaint();
}

pub fn spawn_decoder(shared: Arc<Shared>, repaint: Repaint) -> mpsc::Sender<Packet> {
    let (tx, rx) = mpsc::channel::<Packet>();
    spawn_decode_loop(rx, Assembler::new, shared, repaint);
    tx
}

/// The decoder thread. `make_source` runs on the new thread (the RTP assembler is built there,
/// as it always was).
fn spawn_decode_loop<S: FrameSource + 'static>(
    rx: mpsc::Receiver<S::Item>,
    make_source: fn() -> S,
    shared: Arc<Shared>,
    repaint: Repaint,
) {
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
            // GPU decoding (Windows, when turned on): falls back to OpenH264 if it can't start
            // or keeps failing.
            #[cfg(windows)]
            let mut gpu = if shared.gpu_decode.load(Ordering::Relaxed) {
                match crate::hwdecode::HwDecoder::new() {
                    Ok(d) => {
                        log::info!("video decoder: GPU (Windows H.264 decoder on D3D11)");
                        Some(d)
                    }
                    Err(e) => {
                        log::warn!("GPU video decoding unavailable, using the CPU decoder: {e:#}");
                        None
                    }
                }
            } else {
                None
            };
            #[cfg(windows)]
            let mut gpu_errors = 0u32;
            // Frames fed to the GPU decoder in a row that should have given a picture but didn't.
            #[cfg(windows)]
            let mut gpu_silent = 0u32;
            let mut builder = make_source();
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
                    #[cfg(windows)]
                    if let Some(g) = gpu.as_mut() {
                        match g.decode(sample, i == last) {
                            Ok(picture) => {
                                gpu_errors = 0;
                                decoded += 1;
                                decode_ms += t.elapsed().as_secs_f64() * 1000.0;
                                // A decoder that takes frames but never hands one back would
                                // leave the screen black: after ~2 s of that, use the CPU.
                                if i == last {
                                    if picture.is_some() {
                                        gpu_silent = 0;
                                    } else {
                                        gpu_silent += 1;
                                        if gpu_silent >= 120 {
                                            log::warn!("GPU video decoding gives no pictures: switching to the CPU decoder");
                                            gpu = None;
                                            request_keyframe(&shared);
                                            continue;
                                        }
                                    }
                                }
                                if let Some(f) = picture {
                                    let (w, h) = (f.width, f.height);
                                    let frame = if shared.want_nv12.load(Ordering::Relaxed) {
                                        VideoFrame::Nv12 {
                                            width: w,
                                            height: h,
                                            data: f.data,
                                        }
                                    } else {
                                        nv12_to_rgba(&f.data, w, h, &mut rgba);
                                        VideoFrame::Rgba(egui::ColorImage::from_rgba_premultiplied(
                                            [w, h],
                                            &rgba,
                                        ))
                                    };
                                    publish(&shared, &repaint, frame, w, h);
                                    shown += 1;
                                }
                            }
                            Err(e) => {
                                gpu_errors += 1;
                                log::warn!("GPU decode error ({gpu_errors}): {e:#}");
                                if gpu_errors >= 10 {
                                    log::warn!("GPU video decoding keeps failing: switching to the CPU decoder");
                                    gpu = None;
                                }
                                request_keyframe(&shared);
                            }
                        }
                        continue;
                    }
                    match decoder.decode(sample) {
                        Ok(Some(yuv)) => {
                            errors = 0;
                            decoded += 1;
                            decode_ms += t.elapsed().as_secs_f64() * 1000.0;
                            if i != last {
                                continue; // a newer frame is already here: don't draw this one
                            }
                            let (w, h) = yuv.dimensions();
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
                            publish(&shared, &repaint, frame, w, h);
                            shown += 1;
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
                    #[cfg(windows)]
                    let engine = if gpu.is_some() { "GPU" } else { "CPU" };
                    #[cfg(not(windows))]
                    let engine = "CPU";
                    log::info!(
                        "video ({engine}): decoded {:.0} fps, shown {:.0} fps, decode {:.1} ms/frame, lost frames {lost_frames}",
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
    fn nv12_to_rgba_maps_limited_range_black_white_and_red() {
        // 2x2 picture: black, white / grey, red-ish chroma shared by all four.
        let nv12 = [16, 235, 126, 126, 128, 128];
        let mut out = Vec::new();
        nv12_to_rgba(&nv12, 2, 2, &mut out);
        assert_eq!(&out[0..4], &[0, 0, 0, 255], "Y=16 is black");
        assert_eq!(&out[4..8], &[255, 255, 255, 255], "Y=235 is white");
        let g = out[8];
        assert!(g > 120 && g < 135 && out[9] == g && out[10] == g, "grey stays grey");
        let mut red = Vec::new();
        nv12_to_rgba(&[81, 81, 81, 81, 90, 240], 2, 2, &mut red);
        assert!(red[0] > 200 && red[1] < 40 && red[2] < 40, "BT.709 red: {:?}", &red[0..3]);
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

    /// A few encoded frames of a small grey picture (the first one is a full picture).
    fn encoded_frames(n: usize) -> Vec<Vec<u8>> {
        use openh264::encoder::Encoder;
        use openh264::formats::YUVBuffer;
        let (w, h) = (320usize, 180usize);
        let mut enc = Encoder::new().expect("encoder");
        (0..n)
            .map(|i| {
                let mut i420 = vec![128u8; w * h * 3 / 2];
                i420[..w * h].fill(60 + i as u8 * 10);
                enc.encode(&YUVBuffer::from_vec(i420, w, h))
                    .expect("encode")
                    .to_vec()
            })
            .collect()
    }

    fn wait_for_picture(shared: &Shared) -> (usize, usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(f) = shared.video.lock().unwrap().as_ref() {
                return match f {
                    VideoFrame::Rgba(img) => (img.size[0], img.size[1]),
                    VideoFrame::Nv12 { width, height, .. } => (*width, *height),
                };
            }
            assert!(Instant::now() < deadline, "no decoded picture");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The Xbox path as a whole: RTP packets in through `spawn_decoder`, pictures out in
    /// `Shared.video`. Pins the behaviour the access-unit split must not change.
    #[test]
    fn rtp_decoder_still_turns_packets_into_pictures() {
        use webrtc::rtp::codecs::h264::H264Payloader;
        use webrtc::rtp::packetizer::Payloader;
        let shared = Shared::new(crate::mapper::Mapper::new(
            crate::profile::builtin_profiles().remove(0),
        ));
        let tx: mpsc::Sender<Packet> = spawn_decoder(shared.clone(), Arc::new(|| {}));
        let mut payloader = H264Payloader::default();
        let mut seq = 65530u16; // wraps during the test
        for (i, au) in encoded_frames(6).iter().enumerate() {
            let parts = payloader.payload(1200, &Bytes::from(au.clone())).unwrap();
            let last = parts.len() - 1;
            for (n, payload) in parts.into_iter().enumerate() {
                seq = seq.wrapping_add(1);
                tx.send(Packet {
                    header: Header {
                        sequence_number: seq,
                        marker: n == last,
                        timestamp: i as u32 * 3000,
                        ..Default::default()
                    },
                    payload,
                })
                .unwrap();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(wait_for_picture(&shared), (320, 180));
        assert!(shared.video_frames.load(Ordering::Relaxed) >= 1);
        assert!(!shared.take_keyframe_request(), "nothing was lost");
        shared.stop();
    }

    /// The same frames straight in as access units give the same pictures.
    #[test]
    fn unit_decoder_turns_access_units_into_pictures() {
        let shared = Shared::new(crate::mapper::Mapper::new(
            crate::profile::builtin_profiles().remove(0),
        ));
        let tx = spawn_unit_decoder(shared.clone(), Arc::new(|| {}));
        for au in encoded_frames(6) {
            tx.send(VideoIn::Unit(au)).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(wait_for_picture(&shared), (320, 180));
        assert!(!shared.take_keyframe_request());
        // A reported loss asks the source for a fresh full picture.
        tx.send(VideoIn::Lost).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.take_keyframe_request() {
            assert!(Instant::now() < deadline, "no keyframe request after a loss");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Garbage is refused by the decoder, not fatal: the thread keeps running.
        tx.send(VideoIn::Unit(vec![0, 0, 0, 1, 0x65, 0xff, 0xff, 0xff]))
            .unwrap();
        tx.send(VideoIn::Unit(vec![1, 2, 3])).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(tx.send(VideoIn::Unit(Vec::new())).is_ok(), "decoder thread is still alive");
        shared.stop();
    }

    #[test]
    fn unit_source_passes_units_through_in_order_and_reports_loss_once() {
        let mut u = Units::default();
        let mut out = Vec::new();
        u.push(VideoIn::Unit(vec![1]));
        u.push(VideoIn::Unit(Vec::new())); // empty: ignored
        u.push(VideoIn::Lost);
        u.push(VideoIn::Unit(vec![2]));
        assert!(u.pop(&mut out));
        assert_eq!(out, vec![vec![1], vec![2]]);
        out.clear();
        assert!(!u.pop(&mut out), "loss is reported once");
        assert!(out.is_empty());
    }

    /// The bytes the assembler hands the decoder: each NAL unit behind a 4-byte start code.
    #[test]
    fn assembled_frame_is_annex_b() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        a.push(pkt(10, false, 1));
        a.push(pkt(11, true, 2));
        assert!(!a.pop(&mut out));
        assert_eq!(
            out,
            vec![vec![0, 0, 0, 1, 0x41, 1, 0, 0, 0, 0, 1, 0x41, 2, 0]]
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
