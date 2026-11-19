//! Diagnostic: let Chromium's WebRTC talk to the console through XMouse's own sign-in and
//! xHome signalling. If the browser gets video and XMouse doesn't, the difference is in the
//! WebRTC stack (webrtc-rs); if neither does, it is in sign-in / session setup.
//!
//! Run:  cargo run --release --example relay     then open http://127.0.0.1:8787
//! (Close XMouse's own session first; the console takes one remote session at a time.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use xmouse::auth::Auth;
use xmouse::storage::Storage;
use xmouse::xhome::{describe_failure, IceCandidate, XHome};

const ADDR: &str = "127.0.0.1:8787";

struct Session {
    api: XHome,
    id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    xmouse::init_crypto();
    let storage = Storage::new(Storage::default_dir());
    let auth = Arc::new(Auth::new(storage.token_file()));
    let token = auth
        .stream_token()
        .await
        .context("sign in with XMouse first")?;
    let api = XHome::new(&token.host, &token.token);

    let mut console = storage.settings().last_console;
    if console.is_empty() {
        let list = api.consoles().await?;
        console = list.first().context("no consoles")?.server_id.clone();
    }
    log::info!("console {console}: starting a session");
    let id = api.start(&console, "en-US").await?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut connected = false;
    loop {
        if Instant::now() > deadline {
            let _ = api.stop(&id).await;
            bail!("console did not respond");
        }
        let st = api.state(&id).await?;
        match st["state"].as_str().unwrap_or("") {
            "Provisioned" => break,
            "ReadyToConnect" if !connected => {
                let t = auth.console_transfer_token().await?;
                api.connect(&id, &t).await?;
                connected = true;
            }
            "Failed" => bail!("{}", describe_failure(&st["errorDetails"])),
            s => log::info!("state {s}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    log::info!("session {id} provisioned. Open http://{ADDR}");
    let session = Arc::new(Session { api, id });

    {
        let s = session.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if let Err(e) = s.api.keepalive(&s.id).await {
                    log::warn!("keepalive: {e:#}");
                }
            }
        });
    }

    let listener = TcpListener::bind(ADDR).await?;
    loop {
        let (sock, _) = listener.accept().await?;
        let s = session.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, &s).await {
                log::warn!("request: {e:#}");
            }
        });
    }
}

async fn handle(mut sock: TcpStream, s: &Session) -> Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    let first = head.lines().next().unwrap_or_default().to_string();
    let (status, ctype, out) = match first.split_whitespace().take(2).collect::<Vec<_>>()[..] {
        ["GET", "/"] => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ["POST", "/sdp"] => {
            log::info!("browser offer:\n{body}");
            match s.api.exchange_sdp(&s.id, &body).await {
                Ok(a) => {
                    log::info!("console answer:\n{a}");
                    ("200 OK", "text/plain", a)
                }
                Err(e) => ("500 Error", "text/plain", format!("{e:#}")),
            }
        }
        ["POST", "/ice"] => {
            let local: Vec<IceCandidate> = serde_json::from_str(&body)?;
            match s.api.exchange_ice(&s.id, &local).await {
                Ok(remote) => (
                    "200 OK",
                    "application/json",
                    serde_json::to_string(&remote)?,
                ),
                Err(e) => ("500 Error", "text/plain", format!("{e:#}")),
            }
        }
        ["POST", "/log"] => {
            log::info!("page: {body}");
            ("200 OK", "text/plain", String::new())
        }
        ["POST", "/stop"] => {
            let _ = s.api.stop(&s.id).await;
            log::info!("session stopped");
            ("200 OK", "text/plain", "stopped".into())
        }
        _ => ("404 Not Found", "text/plain", "not found".into()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        out.len()
    );
    sock.write_all(resp.as_bytes()).await?;
    sock.write_all(out.as_bytes()).await?;
    Ok(())
}

const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>XMouse relay</title>
<style>body{background:#111;color:#ddd;font:13px monospace;margin:0}video{width:100%;max-height:70vh;background:#000}
#log{white-space:pre-wrap;padding:8px;height:28vh;overflow:auto}button{margin:6px}</style>
<video id="v" autoplay playsinline muted></video>
<div><button id="go">Connect</button><button id="stop">Stop session</button><span id="stats"></span></div>
<div id="log"></div>
<script>
const $ = id => document.getElementById(id);
const log = m => { $('log').textContent += m + '\n'; $('log').scrollTop = 1e9; fetch('/log', {method:'POST', body:m}); };
const enc = o => new TextEncoder().encode(JSON.stringify(o));
const text = d => typeof d === 'string' ? d : new TextDecoder().decode(d);
$('stop').onclick = () => fetch('/stop', {method:'POST'}).then(() => log('stopped'));
$('go').onclick = async () => {
  const pc = new RTCPeerConnection({});
  pc.addTransceiver('audio', { direction: 'recvonly' });
  const video = pc.addTransceiver('video', { direction: 'recvonly' });
  const codecs = RTCRtpReceiver.getCapabilities('video').codecs;
  const rank = c => c.mimeType.toLowerCase() !== 'video/h264' ? 4 : ['4d','42e','420'].findIndex(p => c.sdpFmtpLine?.includes('profile-level-id=' + p));
  video.setCodecPreferences([...codecs].sort((a,b) => (rank(a) < 0 ? 3 : rank(a)) - (rank(b) < 0 ? 3 : rank(b))));
  const stream = new MediaStream();
  pc.ontrack = e => { log('track ' + e.track.kind); stream.addTrack(e.track); $('v').srcObject = stream; };
  pc.onconnectionstatechange = () => log('connection ' + pc.connectionState);
  const ch = {};
  for (const [name, protocol] of [['input','1.0'],['control','controlV1'],['message','messageV1'],['chat','chatV1']]) {
    ch[name] = pc.createDataChannel(name, { ordered: true, protocol }); ch[name].binaryType = 'arraybuffer';
    ch[name].addEventListener('open', () => log('channel ' + name + ' open'));
    ch[name].addEventListener('close', () => log('channel ' + name + ' closed'));
    ch[name].addEventListener('error', e => log('channel ' + name + ' error ' + (e.error?.message || '')));
  }
  window.pc = pc; window.ch = ch;
  pc.sctp && (pc.sctp.onstatechange = () => log('sctp ' + pc.sctp.state));
  pc.oniceconnectionstatechange = () => log('ice ' + pc.iceConnectionState);
  let seq = 0;
  const pad = () => { const b = new Uint8Array(38), v = new DataView(b.buffer);
    v.setUint16(0, 2, true); v.setUint32(2, seq++, true); v.setFloat64(6, performance.now(), true);
    b[14] = 1; v.setUint32(30, 1, true); v.setUint32(34, 1); return b; };
  ch.message.onopen = () => ch.message.send(enc({ type:'Handshake', version:'messageV1', id: crypto.randomUUID(), cv:'0' }));
  ch.message.onmessage = e => {
    let m; try { m = JSON.parse(text(e.data)); } catch (err) { log('message parse error ' + err); return; }
    log('message ' + (m.type || '') + ' ' + (m.target || ''));
    if (m.type === 'HandshakeAck') {
      ch.control.send(enc({ message:'authorizationRequest', accessKey:'4BDB3609-C1F1-4195-9B37-FEFF45DA8B8E' }));
      ch.control.send(enc({ message:'gamepadChanged', gamepadIndex:0, wasAdded:true }));
      const md = new Uint8Array(15), dv = new DataView(md.buffer);
      dv.setUint16(0, 8, true); dv.setUint32(2, seq++, true); dv.setFloat64(6, performance.now(), true); md[14] = 1;
      ch.input.send(md);
      for (const [target, content] of [
        ['/streaming/systemUi/configuration', { version:[0,2,0], systemUis:[] }],
        ['/streaming/properties/clientappinstallidchanged', { clientAppInstallId: crypto.randomUUID() }],
        ['/streaming/characteristics/orientationchanged', { orientation:0 }],
        ['/streaming/characteristics/touchinputenabledchanged', { touchInputEnabled:false }],
        ['/streaming/characteristics/clientdevicecapabilities', {}],
        ['/streaming/characteristics/dimensionschanged', { horizontal:1920, vertical:1080, preferredWidth:1920, preferredHeight:1080, safeAreaLeft:0, safeAreaTop:0, safeAreaRight:1920, safeAreaBottom:1080, supportsCustomResolution:true }],
      ]) ch.message.send(enc({ type:'Message', id: crypto.randomUUID(), cv:'', target, content: JSON.stringify(content) }));
      for (let i = 0; i < 3; i++) setTimeout(() => ch.input.send(pad()), 25 * i);
      log('handshake done');
    }
  };
  ch.control.onmessage = e => log('control ' + text(e.data).slice(0, 120));
  const offer = await pc.createOffer();
  offer.sdp = offer.sdp.replace('useinbandfec=1', 'useinbandfec=1;stereo=1');
  await pc.setLocalDescription(offer);
  const r = await fetch('/sdp', { method:'POST', body: offer.sdp });
  const answer = await r.text();
  if (!r.ok) { log('SDP failed: ' + answer); return; }
  await pc.setRemoteDescription({ type:'answer', sdp: answer });
  log('answer set');
  await new Promise(res => { if (pc.iceGatheringState === 'complete') res(); pc.onicegatheringstatechange = () => pc.iceGatheringState === 'complete' && res(); setTimeout(res, 5000); });
  const local = [];
  for (const l of pc.localDescription.sdp.split('\r\n')) if (l.startsWith('a=candidate:')) local.push({ candidate: l.slice(2), sdpMid: '0', sdpMLineIndex: 0 });
  const ir = await fetch('/ice', { method:'POST', body: JSON.stringify(local) });
  if (!ir.ok) { log('ICE failed: ' + await ir.text()); return; }
  for (const c of await ir.json()) {
    const cand = c.candidate.replace(/^a=/, '').trim();
    if (!cand || cand.startsWith('end-of-candidates')) continue;
    try { await pc.addIceCandidate({ candidate: cand, sdpMid: String(c.sdpMid ?? '0'), sdpMLineIndex: Number(c.sdpMLineIndex ?? 0) }); }
    catch (e) { log('candidate rejected: ' + cand + ' ' + e); }
  }
  log('remote ICE added');
  setInterval(async () => {
    const rep = await pc.getStats(); let line = '';
    rep.forEach(s => { if (s.type === 'inbound-rtp') line += ` ${s.kind}: ${s.packetsReceived} pkts ${s.bytesReceived} B` + (s.kind === 'video' ? ` ${s.framesDecoded ?? 0} frames ${s.frameWidth ?? ''}x${s.frameHeight ?? ''} ${s.decoderImplementation ?? ''}` : ''); });
    $('stats').textContent = line;
    if ((window.n = (window.n || 0) + 1) % 5 === 0) log('stats' + line);
  }, 1000);
};
</script>"#;
