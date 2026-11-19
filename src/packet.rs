//! Binary format of the xHome "input" data channel.

use crate::mapper::PadFrame;

pub const REPORT_METADATA: u16 = 1;
pub const REPORT_GAMEPAD: u16 = 2;
pub const REPORT_CLIENT_METADATA: u16 = 8;

/// Timing info for one received video frame; acknowledging frames keeps the stream healthy.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameMeta {
    pub server_data_key: u32,
    pub first_packet_arrival_ms: u32,
    pub submitted_ms: u32,
    pub decoded_ms: u32,
    pub rendered_ms: u32,
}

fn header(buf: &mut Vec<u8>, report: u16, seq: u32, now_ms: f64) {
    buf.extend_from_slice(&report.to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(&now_ms.to_le_bytes());
}

/// First packet on the input channel: announces the client.
pub fn client_metadata(seq: u32, max_touchpoints: u8, now_ms: f64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(15);
    header(&mut buf, REPORT_CLIENT_METADATA, seq, now_ms);
    buf.push(max_touchpoints);
    buf
}

fn axis(v: f32) -> i16 {
    (v * 32767.0).clamp(-32767.0, 32767.0) as i16
}

fn trigger(v: f32) -> u16 {
    (v.max(0.0) * 65535.0).min(65535.0) as u16
}

/// Regular input packet: optional frame acknowledgements plus one gamepad state.
pub fn input_packet(
    seq: u32,
    metadata: &[FrameMeta],
    pad: Option<&PadFrame>,
    now_ms: f64,
) -> Vec<u8> {
    let mut report = 0u16;
    if !metadata.is_empty() {
        report |= REPORT_METADATA;
    }
    if pad.is_some() {
        report |= REPORT_GAMEPAD;
    }
    let mut buf = Vec::with_capacity(14 + 1 + 28 * metadata.len() + 24);
    header(&mut buf, report, seq, now_ms);

    if !metadata.is_empty() {
        buf.push(metadata.len() as u8);
        let now = now_ms as u32;
        for m in metadata {
            for v in [
                m.server_data_key,
                m.first_packet_arrival_ms,
                m.submitted_ms,
                m.decoded_ms,
                m.rendered_ms,
                now,
                now,
            ] {
                buf.extend_from_slice(&v.to_le_bytes());
            }
        }
    }

    if let Some(p) = pad {
        buf.push(1); // one gamepad frame
        buf.push(0); // gamepad index
        buf.extend_from_slice(&p.buttons.to_le_bytes());
        buf.extend_from_slice(&axis(p.lx).to_le_bytes());
        buf.extend_from_slice(&axis(-p.ly).to_le_bytes()); // protocol: up is positive
        buf.extend_from_slice(&axis(p.rx).to_le_bytes());
        buf.extend_from_slice(&axis(-p.ry).to_le_bytes());
        buf.extend_from_slice(&trigger(p.lt).to_le_bytes());
        buf.extend_from_slice(&trigger(p.rt).to_le_bytes());
        // Both must be 1 (the browser client sends 1 LE + 1 BE; 0 is not accepted by the console).
        buf.extend_from_slice(&1u32.to_le_bytes()); // physical physicality
        buf.extend_from_slice(&1u32.to_be_bytes()); // virtual physicality
    }
    buf
}

/// Decoded gamepad state (used by tests and the fake console).
#[derive(Debug, PartialEq)]
pub struct DecodedPad {
    pub buttons: u16,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u16,
    pub rt: u16,
}

/// Parse a packet from `input_packet` / `client_metadata`.
pub fn parse(buf: &[u8]) -> Option<(u16, u32, usize, Option<DecodedPad>)> {
    if buf.len() < 14 {
        return None;
    }
    let report = u16::from_le_bytes([buf[0], buf[1]]);
    let seq = u32::from_le_bytes(buf[2..6].try_into().ok()?);
    let mut o = 14;
    let mut meta = 0;
    if report & REPORT_METADATA != 0 {
        meta = *buf.get(o)? as usize;
        o += 1 + meta * 28;
    }
    let mut pad = None;
    if report & REPORT_GAMEPAD != 0 {
        let n = *buf.get(o)?;
        o += 1;
        if n > 0 {
            let g = buf.get(o..o + 23)?;
            let i16at = |i: usize| i16::from_le_bytes([g[i], g[i + 1]]);
            let u16at = |i: usize| u16::from_le_bytes([g[i], g[i + 1]]);
            pad = Some(DecodedPad {
                buttons: u16at(1),
                lx: i16at(3),
                ly: i16at(5),
                rx: i16at(7),
                ry: i16at(9),
                lt: u16at(11),
                rt: u16at(13),
            });
        }
    }
    Some((report, seq, meta, pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_hello_is_15_bytes() {
        let p = client_metadata(0, 2, 123.0);
        assert_eq!(p.len(), 15);
        assert_eq!(u16::from_le_bytes([p[0], p[1]]), REPORT_CLIENT_METADATA);
        assert_eq!(p[14], 2);
    }

    #[test]
    fn gamepad_layout_matches_protocol() {
        let f = PadFrame {
            buttons: 16 | 4096,
            lx: 0.5,
            ly: -1.0,
            rx: -0.25,
            ry: 0.5,
            lt: 1.0,
            rt: 0.0,
        };
        let p = input_packet(7, &[], Some(&f), 1.0);
        assert_eq!(p.len(), 14 + 1 + 23);
        let (report, seq, meta, pad) = parse(&p).unwrap();
        assert_eq!(report, REPORT_GAMEPAD);
        assert_eq!(seq, 7);
        assert_eq!(meta, 0);
        let pad = pad.unwrap();
        assert_eq!(pad.buttons, 16 | 4096);
        assert_eq!(pad.lx, 16383);
        assert_eq!(pad.ly, 32767); // up
        assert_eq!(pad.rx, -8191);
        assert_eq!(pad.ry, -16383);
        assert_eq!(pad.lt, 65535);
        assert_eq!(pad.rt, 0);
    }

    #[test]
    fn metadata_and_gamepad_together() {
        let m = [FrameMeta {
            server_data_key: 99,
            ..Default::default()
        }; 3];
        let p = input_packet(1, &m, Some(&PadFrame::default()), 5.0);
        assert_eq!(p.len(), 14 + 1 + 28 * 3 + 1 + 23);
        let (report, _, meta, pad) = parse(&p).unwrap();
        assert_eq!(report, REPORT_METADATA | REPORT_GAMEPAD);
        assert_eq!(meta, 3);
        assert!(pad.is_some());
        assert_eq!(u32::from_le_bytes(p[15..19].try_into().unwrap()), 99);
    }
}
