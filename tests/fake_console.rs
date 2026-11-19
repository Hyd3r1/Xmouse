//! End-to-end test of the client against an in-process fake Xbox that speaks the console side
//! of the protocol: WebRTC answer, ICE in the console's format, message-channel handshake,
//! control messages, input packets and an H.264 video track.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

use xmouse::mapper::Mapper;
use xmouse::packet;
use xmouse::profile::builtin_profiles;
use xmouse::storage::PlayMode;
use xmouse::stream::{connect, Phase, Shared, Signaling, StreamConfig};
use xmouse::xhome::IceCandidate;

#[derive(Default)]
struct Seen {
    messages: Vec<Value>,
    control: Vec<Value>,
    input: Vec<Vec<u8>>,
}

struct FakeConsole {
    pc: AsyncMutex<Option<Arc<RTCPeerConnection>>>,
    seen: Arc<Mutex<Seen>>,
    keepalives: Mutex<u32>,
}

impl FakeConsole {
    fn new() -> Self {
        Self {
            pc: AsyncMutex::new(None),
            seen: Arc::new(Mutex::new(Seen::default())),
            keepalives: Mutex::new(0),
        }
    }
}

impl Signaling for FakeConsole {
    async fn exchange_sdp(&self, offer: String) -> Result<String> {
        assert!(offer.contains("m=video"), "offer has video");
        assert!(offer.contains("m=application"), "offer has data channels");
        assert!(offer.contains("b=AS:1000"), "offer carries the TV bitrate");

        let mut m = MediaEngine::default();
        m.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut m)?;
        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .build();
        let pc = Arc::new(api.new_peer_connection(Default::default()).await?);

        let seen = self.seen.clone();
        pc.on_data_channel(Box::new(move |dc| {
            let seen = seen.clone();
            let label = dc.label().to_string();
            let protocol = dc.protocol().to_string();
            Box::pin(async move {
                match (label.as_str(), protocol.as_str()) {
                    ("input", "1.0") | ("control", "controlV1") | ("message", "messageV1") | ("chat", "chatV1") => {}
                    other => panic!("unexpected channel {other:?}"),
                }
                let dc2 = dc.clone();
                dc.on_message(Box::new(move |msg| {
                    let seen = seen.clone();
                    let dc2 = dc2.clone();
                    let label = label.clone();
                    Box::pin(async move {
                        assert!(!msg.is_string, "{label}: console expects binary messages");
                        match label.as_str() {
                            "message" => {
                                let v: Value = serde_json::from_slice(&msg.data).unwrap();
                                if v["type"] == "Handshake" {
                                    let ack = json!({ "type": "HandshakeAck", "version": "messageV1", "id": v["id"], "cv": "" });
                                    dc2.send(&Bytes::from(ack.to_string())).await.unwrap();
                                }
                                seen.lock().unwrap().messages.push(v);
                            }
                            "control" => seen.lock().unwrap().control.push(serde_json::from_slice(&msg.data).unwrap()),
                            "input" => seen.lock().unwrap().input.push(msg.data.to_vec()),
                            _ => {}
                        }
                    })
                }));
            })
        }));

        // The console streams video; send small fake H.264 frames so the client acknowledges them.
        let track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.into(),
                clock_rate: 90000,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".into(),
                ..Default::default()
            },
            "video".into(),
            "xbox".into(),
        ));
        pc.add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        pc.set_remote_description(RTCSessionDescription::offer(offer)?)
            .await?;
        let answer = pc.create_answer(None).await?;
        let mut gathered = pc.gathering_complete_promise().await;
        pc.set_local_description(answer).await?;
        let _ = tokio::time::timeout(Duration::from_secs(3), gathered.recv()).await;

        tokio::spawn(async move {
            let mut n = 0u8;
            loop {
                tokio::time::sleep(Duration::from_millis(16)).await;
                n = n.wrapping_add(1);
                let frame = Bytes::from(vec![0, 0, 0, 1, 0x65, 0x88, n, 0x10, 0x20]);
                if track
                    .write_sample(&Sample {
                        data: frame,
                        duration: Duration::from_millis(16),
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let sdp = pc.local_description().await.unwrap().sdp;
        *self.pc.lock().await = Some(pc);
        // Like the real console, answer without candidates; they come through the ICE exchange.
        Ok(sdp
            .lines()
            .filter(|l| !l.starts_with("a=candidate"))
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n")
    }

    async fn exchange_ice(&self, local: Vec<IceCandidate>) -> Result<Vec<IceCandidate>> {
        let pc = self.pc.lock().await.clone().unwrap();
        assert!(!local.is_empty(), "client sent candidates");
        for c in &local {
            assert!(
                c.candidate.starts_with("candidate:"),
                "browser candidate format"
            );
            pc.add_ice_candidate(RTCIceCandidateInit {
                candidate: c.candidate.clone(),
                ..Default::default()
            })
            .await?;
        }
        let sdp = pc.local_description().await.unwrap().sdp;
        let mut out: Vec<IceCandidate> = sdp
            .lines()
            .filter(|l| l.starts_with("a=candidate"))
            .map(|l| IceCandidate {
                candidate: format!("{} ", l.trim()),
                sdp_mid: Some(json!("0")),
                sdp_mline_index: Some(json!("0")),
            })
            .collect();
        out.push(IceCandidate {
            candidate: "a=end-of-candidates".into(),
            sdp_mid: Some(json!("0")),
            sdp_mline_index: Some(json!("0")),
        });
        Ok(out)
    }

    async fn keepalive(&self) -> Result<()> {
        *self.keepalives.lock().unwrap() += 1;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plays_against_fake_console() {
    let _ = env_logger::builder().is_test(true).try_init();
    let console = Arc::new(FakeConsole::new());
    let profile = builtin_profiles()
        .into_iter()
        .find(|p| p.id == "gta-v")
        .unwrap();
    let shared = Shared::new(Mapper::new(profile));
    let cfg = StreamConfig {
        mode: PlayMode::Tv,
        video_bitrate: 1000,
        input_hz: 120,
        ice_servers: vec![],
        connect_timeout: Duration::from_secs(20),
        ..Default::default()
    };

    let task = tokio::spawn(connect(
        console.clone(),
        cfg,
        shared.clone(),
        Arc::new(|| {}),
    ));

    // wait for the handshake
    for _ in 0..200 {
        if shared.status().phase == Phase::Connected {
            break;
        }
        assert!(
            !task.is_finished(),
            "connection ended early: {:?}",
            shared.status()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(shared.status().phase, Phase::Connected);

    // Hold "shoot" and move the mouse right.
    {
        let mut m = shared.mapper.lock().unwrap();
        m.press("Mouse0", 0.0);
    }
    for _ in 0..20 {
        shared.mapper.lock().unwrap().mouse_move(12.0, 0.0);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    shared.stop();
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("stops")
        .expect("joins");
    assert!(result.is_ok(), "{result:?}");

    let seen = console.seen.lock().unwrap();
    // message channel: handshake + configuration
    assert_eq!(seen.messages[0]["type"], "Handshake");
    assert!(seen
        .messages
        .iter()
        .any(|m| m["target"] == "/streaming/systemUi/configuration"));
    assert!(seen
        .messages
        .iter()
        .any(|m| m["target"] == "/streaming/characteristics/dimensionschanged"));
    // control channel
    assert_eq!(seen.control[0]["message"], "authorizationRequest");
    assert_eq!(seen.control[1]["message"], "gamepadChanged");
    assert_eq!(seen.control[1]["wasAdded"], true);
    // input channel: hello, then gamepad frames
    let (first_report, _, _, _) = packet::parse(&seen.input[0]).unwrap();
    assert_eq!(first_report, packet::REPORT_CLIENT_METADATA);
    let pads: Vec<_> = seen
        .input
        .iter()
        .skip(1)
        .filter_map(|p| packet::parse(p))
        .collect();
    assert!(pads.len() > 30, "only {} input packets", pads.len());
    assert!(
        pads.iter()
            .any(|(_, _, _, pad)| pad.as_ref().is_some_and(|p| p.rt == 65535 && p.rx > 0)),
        "shoot + aim right reached the console"
    );
    let acks: usize = pads.iter().map(|(_, _, meta, _)| *meta).sum();
    assert!(acks > 5, "video frames acknowledged: {acks}");
    // last packet releases everything
    let last = packet::parse(seen.input.last().unwrap())
        .unwrap()
        .3
        .unwrap();
    assert_eq!((last.buttons, last.rt, last.rx), (0, 0, 0));
    // sequence numbers increase
    let seqs: Vec<u32> = pads.iter().map(|p| p.1).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]));
}
