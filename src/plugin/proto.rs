//! The plugin wire protocol: frames on the plugin's stdin/stdout, and the messages inside them.
//!
//! Every frame is `u32 LE length | 1 type byte | payload`, where the length counts the type
//! byte and the payload. Control frames carry one JSON object; video and audio frames carry a
//! small binary header and the encoded data. Everything read here comes from another program
//! and is treated as untrusted: lengths are bounded before anything is allocated, and nothing
//! in this file panics on bad input.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::storage::PlayMode;

/// The protocol version this build speaks. A plugin (and its manifest) must name the same one.
pub const API_VERSION: u32 = 1;
/// Largest frame accepted or sent (type byte + payload). A 4K keyframe is far below this.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Largest control (JSON) frame accepted. Control messages are small; this stops a plugin from
/// making the host parse megabytes of JSON.
pub const MAX_CONTROL: usize = 256 * 1024;

/// Frame type: one JSON control message (either direction).
pub const FRAME_CONTROL: u8 = 1;
/// Frame type: one video access unit (plugin to host).
pub const FRAME_VIDEO: u8 = 2;
/// Frame type: one audio packet (plugin to host).
pub const FRAME_AUDIO: u8 = 3;

/// Bytes before the access unit in a video frame: codec, flags, presentation time (u64 LE, µs).
pub const VIDEO_HEADER: usize = 10;
/// Bytes before the packet in an audio frame: codec, channels, sample rate (u32 LE).
pub const AUDIO_HEADER: usize = 6;
/// Video flag bit: this access unit is a full picture (decoding can start here).
pub const VIDEO_FLAG_KEYFRAME: u8 = 1;

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// A length of zero: there is always at least the type byte.
    Empty,
    /// A length above [`MAX_FRAME`]. After this the stream can't be trusted any more.
    TooLarge(u32),
    /// The stream ended in the middle of a frame.
    Truncated,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "read error: {e}"),
            FrameError::Empty => write!(f, "sent an empty frame"),
            FrameError::TooLarge(n) => {
                write!(f, "sent a frame of {n} bytes (the limit is {MAX_FRAME})")
            }
            FrameError::Truncated => write!(f, "stopped in the middle of a frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Build one frame from a type byte and the pieces of its payload.
pub fn encode_frame(kind: u8, parts: &[&[u8]]) -> Result<Vec<u8>, FrameError> {
    let len = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len.min(u32::MAX as usize) as u32));
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.push(kind);
    for p in parts {
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// Write one frame (used by tests and by plugins written against this crate).
pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let frame = encode_frame(kind, &[payload])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    w.write_all(&frame)
}

/// Read one frame: its type byte and payload. `Ok(None)` is a clean end of the stream (it
/// ended exactly between two frames).
pub fn read_frame(r: &mut impl Read) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_le_bytes(head);
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len as usize > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut kind = [0u8; 1];
    match r.read_exact(&mut kind) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Truncated),
        Err(e) => return Err(FrameError::Io(e)),
    }
    // Grow the buffer as the bytes actually arrive, so a large length followed by nothing
    // doesn't cost 8 MiB.
    let want = len as usize - 1;
    let mut payload = Vec::with_capacity(want.min(64 * 1024));
    match r.take(want as u64).read_to_end(&mut payload) {
        Ok(n) if n == want => Ok(Some((kind[0], payload))),
        Ok(_) => Err(FrameError::Truncated),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Messages XMouse sends to a plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPlugin {
    /// Always first. `config_dir` is a folder the plugin may keep its own files in (pairing
    /// keys, settings); it survives restarts.
    Hello {
        api: u32,
        config_dir: String,
        host: String,
        host_version: String,
        /// What the host can decode, so a plugin doesn't pick something else.
        video_codecs: Vec<String>,
        audio_codecs: Vec<String>,
    },
    ListConsoles,
    Pair {
        console_id: String,
        pin: String,
        /// Anything else a platform needs for pairing (account ID, ...), by name.
        #[serde(default)]
        extra: BTreeMap<String, String>,
    },
    Wake {
        console_id: String,
    },
    Start {
        console_id: String,
        /// `tv`: the picture and sound stay on the TV, the host throws media away (send as
        /// little as possible). `pc`: the host decodes and shows the stream.
        mode: PlayMode,
        max_width: u32,
        max_height: u32,
        fps: u32,
        /// 0 = let the plugin / console decide.
        bitrate_kbps: u32,
    },
    /// The controller state, sent at the input rate while connected. `buttons` uses the Xbox
    /// bit layout (see `Action::button_bit`). Sticks are -1..1 with up = -1; triggers 0..1.
    Input {
        buttons: u32,
        lx: f32,
        ly: f32,
        rx: f32,
        ry: f32,
        lt: f32,
        rt: f32,
    },
    /// The decoder lost its place: send a full picture as soon as possible.
    RequestKeyframe,
    /// End the session (or the conversation) and exit.
    Stop,
}

/// Messages a plugin sends to XMouse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromPlugin {
    /// Answer to the host's `hello`.
    Hello {
        name: String,
        version: String,
        api: u32,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    /// Answer to `list_consoles`.
    Consoles {
        #[serde(default)]
        consoles: Vec<ConsoleEntry>,
    },
    /// Answer to `pair`.
    PairResult {
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    },
    /// Answer to `wake`.
    WakeResult {
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    },
    /// Where the session is. `ended` is final; with `error` set it is shown to the user.
    Status {
        phase: StatusPhase,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// Controller vibration, both 0..1.
    Rumble {
        left: f32,
        right: f32,
    },
    /// A line for XMouse's log.
    Log {
        #[serde(default)]
        level: String,
        text: String,
    },
    /// A message type this build doesn't know (from a newer plugin): ignored.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusPhase {
    Starting,
    Waiting,
    Connecting,
    Connected,
    Ended,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleState {
    On,
    Standby,
    Off,
    #[default]
    #[serde(other)]
    Unknown,
}

/// One console as the plugin reports it (see `host::PluginConsole` for the cleaned-up form).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub state: ConsoleState,
    #[serde(default)]
    pub needs_pairing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

/// One video access unit in Annex-B form.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoUnit {
    pub codec: VideoCodec,
    pub keyframe: bool,
    /// Presentation time in microseconds on the plugin's own clock (not used for pacing yet:
    /// frames are shown as they arrive).
    pub pts_us: u64,
    pub data: Vec<u8>,
}

/// One Opus packet.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioPacket {
    /// As the plugin states them (informational: Opus is always decoded to 48 kHz stereo).
    pub sample_rate: u32,
    pub channels: u8,
    pub data: Vec<u8>,
}

/// What a frame from the plugin turned out to be.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Control(FromPlugin),
    Video(VideoUnit),
    Audio(AudioPacket),
}

/// Interpret one frame from a plugin. `Ok(None)`: a frame type this build doesn't know, to be
/// ignored. `Err`: the frame is malformed (the framing itself is still intact).
pub fn decode(kind: u8, mut payload: Vec<u8>) -> Result<Option<Decoded>, String> {
    match kind {
        FRAME_CONTROL => {
            if payload.len() > MAX_CONTROL {
                return Err(format!("control message of {} bytes", payload.len()));
            }
            serde_json::from_slice::<FromPlugin>(&payload)
                .map(|m| Some(Decoded::Control(m)))
                .map_err(|e| format!("control message is not valid: {e}"))
        }
        FRAME_VIDEO => {
            if payload.len() <= VIDEO_HEADER {
                return Err("video frame without data".into());
            }
            let codec = match payload[0] {
                0 => VideoCodec::H264,
                1 => VideoCodec::Hevc,
                other => return Err(format!("unknown video codec {other}")),
            };
            let keyframe = payload[1] & VIDEO_FLAG_KEYFRAME != 0;
            let mut pts = [0u8; 8];
            pts.copy_from_slice(&payload[2..VIDEO_HEADER]);
            payload.drain(..VIDEO_HEADER);
            Ok(Some(Decoded::Video(VideoUnit {
                codec,
                keyframe,
                pts_us: u64::from_le_bytes(pts),
                data: payload,
            })))
        }
        FRAME_AUDIO => {
            if payload.len() <= AUDIO_HEADER {
                return Err("audio frame without data".into());
            }
            if payload[0] != 0 {
                return Err(format!("unknown audio codec {}", payload[0]));
            }
            let channels = payload[1];
            let sample_rate = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
            payload.drain(..AUDIO_HEADER);
            Ok(Some(Decoded::Audio(AudioPacket {
                sample_rate,
                channels,
                data: payload,
            })))
        }
        _ => Ok(None),
    }
}

/// A control message as a ready-to-write frame.
pub fn control_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(msg)
        .map_err(|e| FrameError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
    encode_frame(FRAME_CONTROL, &[&json])
}

/// A video access unit as a ready-to-write frame (the plugin side; used by tests).
pub fn video_frame(unit: &VideoUnit) -> Result<Vec<u8>, FrameError> {
    let codec = match unit.codec {
        VideoCodec::H264 => 0u8,
        VideoCodec::Hevc => 1,
    };
    let flags = if unit.keyframe { VIDEO_FLAG_KEYFRAME } else { 0 };
    encode_frame(
        FRAME_VIDEO,
        &[&[codec, flags], &unit.pts_us.to_le_bytes(), &unit.data],
    )
}

/// An audio packet as a ready-to-write frame (the plugin side; used by tests).
pub fn audio_frame(packet: &AudioPacket) -> Result<Vec<u8>, FrameError> {
    encode_frame(
        FRAME_AUDIO,
        &[
            &[0u8, packet.channels],
            &packet.sample_rate.to_le_bytes(),
            &packet.data,
        ],
    )
}

/// Text from a plugin that ends up on screen or in the log: no control characters, and not
/// longer than `max` characters.
pub fn clean_text(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, FRAME_CONTROL, b"{}").unwrap();
        write_frame(&mut buf, 9, b"").unwrap();
        assert_eq!(&buf[..7], &[3, 0, 0, 0, 1, b'{', b'}'], "length counts the type byte");
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap(), Some((FRAME_CONTROL, b"{}".to_vec())));
        assert_eq!(read_frame(&mut r).unwrap(), Some((9, Vec::new())));
        assert_eq!(read_frame(&mut r).unwrap(), None, "clean end between frames");
    }

    #[test]
    fn bad_lengths_are_refused_before_reading_the_payload() {
        // Zero length.
        let e = read_frame(&mut Cursor::new(vec![0, 0, 0, 0, 1])).unwrap_err();
        assert!(matches!(e, FrameError::Empty));
        // One byte over the limit, and the largest possible value.
        let over = (MAX_FRAME as u32 + 1).to_le_bytes().to_vec();
        let e = read_frame(&mut Cursor::new(over)).unwrap_err();
        assert!(matches!(e, FrameError::TooLarge(n) if n as usize == MAX_FRAME + 1));
        let e = read_frame(&mut Cursor::new(vec![0xff; 16])).unwrap_err();
        assert!(matches!(e, FrameError::TooLarge(u32::MAX)));
        // Exactly the limit is fine.
        let mut big = (MAX_FRAME as u32).to_le_bytes().to_vec();
        big.push(FRAME_VIDEO);
        big.resize(4 + MAX_FRAME, 7);
        let (kind, payload) = read_frame(&mut Cursor::new(big)).unwrap().unwrap();
        assert_eq!((kind, payload.len()), (FRAME_VIDEO, MAX_FRAME - 1));
        // The sending side refuses to build an oversize frame too.
        assert!(encode_frame(1, &[&vec![0u8; MAX_FRAME]]).is_err());
    }

    #[test]
    fn a_stream_that_ends_mid_frame_is_truncated() {
        for bytes in [vec![5u8, 0], vec![5, 0, 0, 0], vec![5, 0, 0, 0, 1, b'{']] {
            let e = read_frame(&mut Cursor::new(bytes.clone())).unwrap_err();
            assert!(matches!(e, FrameError::Truncated), "{bytes:?} gave {e:?}");
        }
        // A huge announced length with nothing behind it fails without a huge allocation.
        let mut lie = (MAX_FRAME as u32).to_le_bytes().to_vec();
        lie.extend_from_slice(&[FRAME_VIDEO, 1, 2, 3]);
        assert!(matches!(
            read_frame(&mut Cursor::new(lie)).unwrap_err(),
            FrameError::Truncated
        ));
    }

    #[test]
    fn host_messages_have_the_documented_json_shape() {
        let v = |m: &ToPlugin| serde_json::to_value(m).unwrap();
        assert_eq!(v(&ToPlugin::ListConsoles), json!({ "type": "list_consoles" }));
        assert_eq!(v(&ToPlugin::RequestKeyframe), json!({ "type": "request_keyframe" }));
        assert_eq!(v(&ToPlugin::Stop), json!({ "type": "stop" }));
        assert_eq!(
            v(&ToPlugin::Start {
                console_id: "c1".into(),
                mode: PlayMode::Pc,
                max_width: 1920,
                max_height: 1080,
                fps: 60,
                bitrate_kbps: 0,
            }),
            json!({ "type": "start", "console_id": "c1", "mode": "pc", "max_width": 1920,
                    "max_height": 1080, "fps": 60, "bitrate_kbps": 0 })
        );
        assert_eq!(
            v(&ToPlugin::Input { buttons: 16, lx: 0.5, ly: -1.0, rx: 0.0, ry: 0.0, lt: 0.0, rt: 1.0 }),
            json!({ "type": "input", "buttons": 16, "lx": 0.5, "ly": -1.0, "rx": 0.0,
                    "ry": 0.0, "lt": 0.0, "rt": 1.0 })
        );
        let pair = ToPlugin::Pair {
            console_id: "c1".into(),
            pin: "12345678".into(),
            extra: BTreeMap::from([("account".to_string(), "abc".to_string())]),
        };
        assert_eq!(
            v(&pair),
            json!({ "type": "pair", "console_id": "c1", "pin": "12345678",
                    "extra": { "account": "abc" } })
        );
        // And back (a plugin written in Rust can use the same types).
        let back: ToPlugin = serde_json::from_value(v(&pair)).unwrap();
        assert_eq!(back, pair);
    }

    #[test]
    fn plugin_messages_parse_with_defaults_and_unknowns() {
        let p = |v: serde_json::Value| serde_json::from_value::<FromPlugin>(v);
        assert_eq!(
            p(json!({ "type": "hello", "name": "P", "version": "1", "api": 1 })).unwrap(),
            FromPlugin::Hello { name: "P".into(), version: "1".into(), api: 1, capabilities: vec![] }
        );
        let consoles = p(json!({ "type": "consoles", "consoles": [
            { "id": "a", "name": "Living room", "model": "X", "state": "standby", "needs_pairing": true },
            { "id": "b", "name": "Den", "state": "some-future-state", "extra_field": 1 },
        ] }))
        .unwrap();
        let FromPlugin::Consoles { consoles } = consoles else { panic!("not consoles") };
        assert_eq!(consoles[0].state, ConsoleState::Standby);
        assert!(consoles[0].needs_pairing);
        assert_eq!(consoles[1].state, ConsoleState::Unknown, "unknown states are tolerated");
        assert_eq!(consoles[1].model, "");
        assert_eq!(
            p(json!({ "type": "status", "phase": "ended", "error": "console turned off" })).unwrap(),
            FromPlugin::Status {
                phase: StatusPhase::Ended,
                detail: String::new(),
                error: Some("console turned off".into()),
            }
        );
        assert_eq!(
            p(json!({ "type": "pair_result", "ok": false, "error": "Wrong PIN" })).unwrap(),
            FromPlugin::PairResult { ok: false, error: Some("Wrong PIN".into()) }
        );
        assert_eq!(
            p(json!({ "type": "something_from_the_future", "x": 1 })).unwrap(),
            FromPlugin::Unknown
        );
        // Wrong shapes are errors, not panics.
        assert!(p(json!({ "type": "status", "phase": "dancing" })).is_err());
        assert!(p(json!({ "type": "rumble", "left": "loud", "right": 0 })).is_err());
        assert!(p(json!({ "no_type": true })).is_err());
        assert!(p(json!([1, 2, 3])).is_err());
    }

    #[test]
    fn media_frames_round_trip_and_reject_bad_headers() {
        let unit = VideoUnit {
            codec: VideoCodec::H264,
            keyframe: true,
            pts_us: 0x0102_0304_0506_0708,
            data: vec![0, 0, 0, 1, 0x65, 9],
        };
        let frame = video_frame(&unit).unwrap();
        let (kind, payload) = read_frame(&mut Cursor::new(frame)).unwrap().unwrap();
        assert_eq!(kind, FRAME_VIDEO);
        assert_eq!(&payload[..2], &[0, 1], "codec h264, keyframe flag");
        assert_eq!(decode(kind, payload).unwrap(), Some(Decoded::Video(unit)));

        let packet = AudioPacket { sample_rate: 48_000, channels: 2, data: vec![1, 2, 3] };
        let frame = audio_frame(&packet).unwrap();
        let (kind, payload) = read_frame(&mut Cursor::new(frame)).unwrap().unwrap();
        assert_eq!(decode(kind, payload).unwrap(), Some(Decoded::Audio(packet)));

        assert!(decode(FRAME_VIDEO, vec![0; VIDEO_HEADER]).is_err(), "header only");
        assert!(decode(FRAME_VIDEO, vec![]).is_err());
        assert!(decode(FRAME_VIDEO, vec![7; 20]).is_err(), "unknown codec");
        assert!(decode(FRAME_AUDIO, vec![0; AUDIO_HEADER]).is_err());
        assert!(decode(FRAME_AUDIO, vec![3; 20]).is_err(), "unknown codec");
        assert!(decode(FRAME_CONTROL, b"not json".to_vec()).is_err());
        assert!(decode(FRAME_CONTROL, vec![b' '; MAX_CONTROL + 1]).is_err());
        assert_eq!(decode(200, vec![1, 2, 3]).unwrap(), None, "unknown frame types are skipped");
    }

    #[test]
    fn plugin_text_is_cleaned() {
        assert_eq!(clean_text("  ok\u{7}\n\tthen  ", 100), "okthen");
        assert_eq!(clean_text(&"x".repeat(500), 10), "xxxxxxxxxx");
    }
}
