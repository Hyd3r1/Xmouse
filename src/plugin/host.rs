//! Running a plugin: start its program, talk to it over stdin/stdout, and survive whatever it
//! does (garbage, silence, crashing, never exiting).
//!
//! Three small threads per plugin, so nothing else ever blocks on it:
//! - the reader turns stdout into messages and hands them to a callback;
//! - the writer owns stdin and takes frames from a short queue (a plugin that stops reading
//!   fills the queue, and `send` reports that instead of blocking);
//! - the logger copies stderr into XMouse's log.

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use super::manifest::PluginInfo;
use super::proto::{
    self, clean_text, AudioPacket, ConsoleState, Decoded, FromPlugin, ToPlugin, VideoUnit,
    API_VERSION,
};

/// The argument that tells the program it is being run as a plugin.
pub const PLUGIN_ARG: &str = "--xmouse-plugin";
/// Frames waiting for the plugin to read them. At a 250 Hz input rate this is a quarter of a
/// second; a healthy plugin keeps it empty.
const WRITE_QUEUE: usize = 64;
/// A plugin that keeps sending frames that make no sense is stopped after this many.
const MAX_BAD_FRAMES: u32 = 32;
/// How long a plugin gets to answer `hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a plugin gets to list consoles or wake one (it may search the network).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Pairing talks to the console and can take a while.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(60);
/// At most this many consoles are taken from one plugin.
const MAX_CONSOLES: usize = 32;

/// What the reader thread hands over.
#[derive(Debug)]
pub enum Incoming {
    Control(FromPlugin),
    Video(VideoUnit),
    Audio(AudioPacket),
    /// The conversation is over (the plugin exited, or broke the protocol and was stopped).
    /// Always the last thing delivered; the text says why.
    Closed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// The plugin isn't reading its input (the queue is full). Try again later; if this
    /// lasts, the plugin hangs.
    Busy,
    /// The plugin is gone.
    Closed,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Busy => write!(f, "the plugin is not reading its input"),
            SendError::Closed => write!(f, "the plugin has stopped"),
        }
    }
}

impl std::error::Error for SendError {}

type SharedChild = Arc<Mutex<Option<Child>>>;

/// A running plugin program (or, in tests, any pair of byte streams speaking the protocol).
pub struct PluginProcess {
    label: String,
    child: SharedChild,
    tx: mpsc::SyncSender<Vec<u8>>,
}

impl PluginProcess {
    /// Start the plugin's program. `sink` is called on the reader thread for everything the
    /// plugin sends (`log` messages are written to the log and not passed on); it should
    /// return quickly, and blocking in it simply makes the plugin wait.
    pub fn spawn(
        info: &PluginInfo,
        sink: impl FnMut(Incoming) + Send + 'static,
    ) -> Result<Self> {
        // A place for the plugin's own files. If it can't be made the plugin will say so.
        let _ = std::fs::create_dir_all(&info.data_dir);
        let mut cmd = Command::new(&info.exe);
        cmd.arg(PLUGIN_ARG)
            .args(&info.args)
            .current_dir(&info.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: a console program started from the (windowed) app would
            // otherwise flash a black console window.
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Could not start the {} plugin", info.name))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Could not connect to the {} plugin", info.name);
        };
        let label = info.id.clone();
        if let Some(stderr) = stderr {
            let label = label.clone();
            let _ = std::thread::Builder::new()
                .name("xmouse-plugin-log".into())
                .spawn(move || copy_stderr(stderr, &label));
        }
        Ok(Self::start(label, Some(child), stdout, stdin, sink))
    }

    /// Speak the protocol over any two byte streams (no program). For tests.
    pub fn from_io(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        sink: impl FnMut(Incoming) + Send + 'static,
    ) -> Self {
        Self::start("test".into(), None, reader, writer, sink)
    }

    fn start(
        label: String,
        child: Option<Child>,
        reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
        sink: impl FnMut(Incoming) + Send + 'static,
    ) -> Self {
        let child: SharedChild = Arc::new(Mutex::new(child));
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE);
        let _ = std::thread::Builder::new()
            .name("xmouse-plugin-out".into())
            .spawn(move || {
                // Ends when the queue is dropped (PluginProcess gone) or the pipe breaks
                // (plugin gone); dropping `writer` closes the plugin's stdin either way.
                while let Ok(frame) = rx.recv() {
                    if writer.write_all(&frame).and_then(|_| writer.flush()).is_err() {
                        break;
                    }
                }
            });
        {
            let (label, child) = (label.clone(), child.clone());
            let _ = std::thread::Builder::new()
                .name("xmouse-plugin-in".into())
                .spawn(move || read_loop(reader, sink, &label, &child));
        }
        Self { label, child, tx }
    }

    /// Queue a message for the plugin. Never blocks.
    pub fn send(&self, msg: &ToPlugin) -> Result<(), SendError> {
        let frame = proto::control_frame(msg).map_err(|e| {
            log::warn!("plugin {}: message not sent: {e}", self.label);
            SendError::Closed
        })?;
        match self.tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SendError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(SendError::Closed),
        }
    }

    /// Whether the program has exited (true for `from_io`: there is no program to wait for).
    pub fn exited(&self) -> bool {
        match self.child.lock().unwrap().as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => true,
        }
    }

    /// Stop the program now. Safe to call more than once.
    pub fn kill(&self) {
        kill(&self.child);
    }

    /// Give the program up to `grace` to exit by itself (after a `stop`), then stop it.
    /// Blocks for at most `grace`.
    pub fn shutdown(&self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while !self.exited() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        // Never leave a plugin running without its host.
        self.kill();
    }
}

fn kill(child: &SharedChild) {
    if let Some(c) = child.lock().unwrap().as_mut() {
        // Errors mean it is already gone. `wait` after `kill` returns at once and frees the
        // process handle.
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// The reader thread: frames in, `Incoming` out, until the stream ends or stops making sense.
fn read_loop(
    reader: impl Read,
    mut sink: impl FnMut(Incoming),
    label: &str,
    child: &SharedChild,
) {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut bad = 0u32;
    let mut unknown_logged = false;
    let reason = loop {
        let (kind, payload) = match proto::read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break exit_reason(child),
            // A bad length means we no longer know where frames start: there is no way to
            // carry on with this plugin.
            Err(e) => break format!("it {e}"),
        };
        match proto::decode(kind, payload) {
            Ok(Some(Decoded::Control(FromPlugin::Log { level, text }))) => {
                let text = clean_text(&text, 2000);
                match level.as_str() {
                    "error" => log::error!("[plugin {label}] {text}"),
                    "warn" | "warning" => log::warn!("[plugin {label}] {text}"),
                    "debug" | "trace" => log::debug!("[plugin {label}] {text}"),
                    _ => log::info!("[plugin {label}] {text}"),
                }
            }
            Ok(Some(Decoded::Control(FromPlugin::Unknown))) => {
                log::debug!("plugin {label}: unknown message type ignored");
            }
            Ok(Some(Decoded::Control(m))) => sink(Incoming::Control(m)),
            Ok(Some(Decoded::Video(v))) => sink(Incoming::Video(v)),
            Ok(Some(Decoded::Audio(a))) => sink(Incoming::Audio(a)),
            Ok(None) => {
                if !unknown_logged {
                    unknown_logged = true;
                    log::warn!("plugin {label}: unknown frame type {kind} ignored");
                }
            }
            Err(e) => {
                bad += 1;
                if bad <= 3 {
                    log::warn!("plugin {label}: bad frame ignored: {e}");
                }
                if bad >= MAX_BAD_FRAMES {
                    break "it kept sending data that makes no sense".to_string();
                }
            }
        }
    };
    log::info!("plugin {label} closed: {reason}");
    kill(child);
    sink(Incoming::Closed(reason));
}

/// Why the output ended: the exit code when there is a program to ask.
fn exit_reason(child: &SharedChild) -> String {
    let mut guard = child.lock().unwrap();
    let Some(c) = guard.as_mut() else {
        return "it closed the connection".into();
    };
    // Output closed means the program is exiting; give it a moment to finish doing so.
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match c.try_wait() {
            Ok(Some(status)) if status.success() => return "it exited".into(),
            Ok(Some(status)) => return format!("it exited unexpectedly ({status})"),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => return "it closed the connection".into(),
        }
    }
}

/// Copy the plugin's stderr to the log, line by line. Lines are cut at 2000 bytes so a plugin
/// that never sends a newline can't make one grow without end.
fn copy_stderr(stderr: impl Read, label: &str) {
    const MAX_LINE: usize = 2000;
    let mut reader = BufReader::new(stderr);
    let mut line: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let flush = |line: &mut Vec<u8>| {
        let text = clean_text(&String::from_utf8_lossy(line), MAX_LINE);
        if !text.is_empty() {
            log::info!("[plugin {label}] {text}");
        }
        line.clear();
    };
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &chunk[..n] {
            if b == b'\n' || line.len() >= MAX_LINE {
                flush(&mut line);
            }
            if b != b'\n' {
                line.push(b);
            }
        }
    }
    flush(&mut line);
}

/// What a plugin said about itself in `hello`.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginHello {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

/// The host's opening message for a plugin.
pub fn host_hello(info: &PluginInfo) -> ToPlugin {
    ToPlugin::Hello {
        api: API_VERSION,
        config_dir: info.data_dir.to_string_lossy().into_owned(),
        host: "XMouse".into(),
        host_version: env!("CARGO_PKG_VERSION").into(),
        // HEVC is not decodable yet; plugins must send H.264.
        video_codecs: vec!["h264".into()],
        audio_codecs: vec!["opus".into()],
    }
}

/// Check a plugin's `hello` answer.
pub fn check_hello(name: String, version: String, api: u32, capabilities: Vec<String>) -> Result<PluginHello> {
    if api != API_VERSION {
        bail!("The plugin speaks plugin API {api}; this XMouse speaks API {API_VERSION}.");
    }
    Ok(PluginHello {
        name: clean_text(&name, 80),
        version: clean_text(&version, 40),
        capabilities: capabilities
            .iter()
            .take(32)
            .map(|c| clean_text(c, 40))
            .collect(),
    })
}

/// A console offered by a plugin, cleaned up for display.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginConsole {
    /// The plugin's own identifier: pass it back unchanged in pair / wake / start.
    pub id: String,
    pub name: String,
    pub model: String,
    pub state: ConsoleState,
    /// The console must be paired (PIN) before a session can start.
    pub needs_pairing: bool,
}

fn clean_consoles(list: Vec<proto::ConsoleEntry>) -> Vec<PluginConsole> {
    list.into_iter()
        .filter(|c| !c.id.is_empty() && c.id.len() <= 256)
        .take(MAX_CONSOLES)
        .map(|c| {
            let name = clean_text(&c.name, 80);
            PluginConsole {
                name: if name.is_empty() { "Console".into() } else { name },
                model: clean_text(&c.model, 40),
                state: c.state,
                needs_pairing: c.needs_pairing,
                id: c.id,
            }
        })
        .collect()
}

/// A plugin used in the simple ask-and-wait way (list consoles, pair, wake). Every call
/// blocks until the answer or its timeout, so use it from a background thread.
pub struct PluginClient {
    proc: PluginProcess,
    rx: mpsc::Receiver<Incoming>,
    pub hello: PluginHello,
    name: String,
}

impl PluginClient {
    /// Start the plugin and exchange `hello`.
    pub fn connect(info: &PluginInfo) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(64);
        let proc = PluginProcess::spawn(info, move |msg| {
            // Outside a session media means nothing; only messages are of interest.
            if matches!(msg, Incoming::Control(_) | Incoming::Closed(_)) {
                let _ = tx.send(msg);
            }
        })?;
        Self::handshake(proc, rx, info, HELLO_TIMEOUT)
    }

    fn handshake(
        proc: PluginProcess,
        rx: mpsc::Receiver<Incoming>,
        info: &PluginInfo,
        timeout: Duration,
    ) -> Result<Self> {
        let mut client = Self {
            proc,
            rx,
            hello: PluginHello {
                name: info.name.clone(),
                version: String::new(),
                capabilities: Vec::new(),
            },
            name: info.name.clone(),
        };
        client.send(&host_hello(info))?;
        let (name, version, api, caps) = client.wait(timeout, "say hello", |m| match m {
            FromPlugin::Hello { name, version, api, capabilities } => {
                Some((name, version, api, capabilities))
            }
            _ => None,
        })?;
        client.hello = check_hello(name, version, api, caps)?;
        Ok(client)
    }

    fn send(&self, msg: &ToPlugin) -> Result<()> {
        self.proc
            .send(msg)
            .map_err(|e| anyhow!("The {} plugin can't be reached: {e}.", self.name))
    }

    /// Wait for the message `pick` accepts; anything else that arrives meanwhile is skipped.
    fn wait<T>(
        &mut self,
        timeout: Duration,
        what: &str,
        pick: impl Fn(FromPlugin) -> Option<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Incoming::Control(m)) => {
                    if let Some(v) = pick(m) {
                        return Ok(v);
                    }
                }
                Ok(Incoming::Closed(reason)) => {
                    bail!("The {} plugin stopped: {reason}.", self.name)
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    self.proc.kill();
                    bail!("The {} plugin did not {what} in time.", self.name)
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("The {} plugin stopped.", self.name)
                }
            }
        }
    }

    pub fn list_consoles(&mut self) -> Result<Vec<PluginConsole>> {
        self.send(&ToPlugin::ListConsoles)?;
        let list = self.wait(REQUEST_TIMEOUT, "list its consoles", |m| match m {
            FromPlugin::Consoles { consoles } => Some(consoles),
            _ => None,
        })?;
        Ok(clean_consoles(list))
    }

    /// Pair with a console. `Err` carries the plugin's explanation (e.g. "Wrong PIN").
    pub fn pair(
        &mut self,
        console_id: &str,
        pin: &str,
        extra: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.send(&ToPlugin::Pair {
            console_id: console_id.into(),
            pin: pin.into(),
            extra: extra.clone(),
        })?;
        let (ok, error) = self.wait(PAIR_TIMEOUT, "finish pairing", |m| match m {
            FromPlugin::PairResult { ok, error } => Some((ok, error)),
            _ => None,
        })?;
        outcome(ok, error, "Pairing failed.")
    }

    pub fn wake(&mut self, console_id: &str) -> Result<()> {
        self.send(&ToPlugin::Wake {
            console_id: console_id.into(),
        })?;
        let (ok, error) = self.wait(REQUEST_TIMEOUT, "wake the console", |m| match m {
            FromPlugin::WakeResult { ok, error } => Some((ok, error)),
            _ => None,
        })?;
        outcome(ok, error, "The console could not be woken.")
    }
}

fn outcome(ok: bool, error: Option<String>, fallback: &str) -> Result<()> {
    if ok {
        return Ok(());
    }
    let text = error.map(|e| clean_text(&e, 300)).unwrap_or_default();
    bail!("{}", if text.is_empty() { fallback } else { text.as_str() })
}

impl Drop for PluginClient {
    fn drop(&mut self) {
        // Ask nicely, wait briefly, then make sure.
        let _ = self.proc.send(&ToPlugin::Stop);
        self.proc.shutdown(Duration::from_millis(500));
    }
}

/// Start the plugin, ask for its consoles, stop it. Blocks (see [`PluginClient`]).
pub fn list_consoles(info: &PluginInfo) -> Result<Vec<PluginConsole>> {
    PluginClient::connect(info)?.list_consoles()
}

/// Start the plugin, pair with a console, stop it. Blocks.
pub fn pair(
    info: &PluginInfo,
    console_id: &str,
    pin: &str,
    extra: &BTreeMap<String, String>,
) -> Result<()> {
    PluginClient::connect(info)?.pair(console_id, pin, extra)
}

/// Start the plugin, wake a console, stop it. Blocks.
pub fn wake(info: &PluginInfo, console_id: &str) -> Result<()> {
    PluginClient::connect(info)?.wake(console_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::proto::{control_frame, write_frame, FRAME_CONTROL, FRAME_VIDEO, MAX_FRAME};
    use std::io::Cursor;
    use std::path::PathBuf;

    fn info() -> PluginInfo {
        PluginInfo {
            id: "test".into(),
            name: "Test".into(),
            version: "1".into(),
            platform: "Test".into(),
            license: String::new(),
            source: String::new(),
            dir: PathBuf::from("."),
            exe: PathBuf::from("none"),
            args: vec![],
            data_dir: PathBuf::from("data"),
        }
    }

    /// Everything the host makes of a fixed byte stream "from the plugin".
    fn feed(bytes: Vec<u8>) -> Vec<Incoming> {
        let (tx, rx) = mpsc::channel();
        let _proc = PluginProcess::from_io(Cursor::new(bytes), std::io::sink(), move |m| {
            let _ = tx.send(m);
        });
        let mut out = Vec::new();
        loop {
            let m = rx.recv_timeout(Duration::from_secs(5)).expect("reader finishes");
            let done = matches!(m, Incoming::Closed(_));
            out.push(m);
            if done {
                return out;
            }
        }
    }

    fn closed(events: &[Incoming]) -> &str {
        match events.last() {
            Some(Incoming::Closed(r)) => r,
            other => panic!("expected Closed last, got {other:?}"),
        }
    }

    fn status_frame() -> Vec<u8> {
        control_frame(&FromPlugin::Status {
            phase: proto::StatusPhase::Connected,
            detail: String::new(),
            error: None,
        })
        .unwrap()
    }

    #[test]
    fn malformed_frames_are_skipped_and_the_stream_goes_on() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, FRAME_CONTROL, b"this is not json").unwrap();
        write_frame(&mut bytes, FRAME_VIDEO, &[0, 1]).unwrap(); // header cut short
        write_frame(&mut bytes, 77, b"future frame type").unwrap();
        write_frame(&mut bytes, FRAME_CONTROL, br#"{"type":"from_the_future"}"#).unwrap();
        write_frame(&mut bytes, FRAME_CONTROL, br#"{"type":"log","text":"hello"}"#).unwrap();
        bytes.extend(status_frame());
        let events = feed(bytes);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(
            &events[0],
            Incoming::Control(FromPlugin::Status { phase: proto::StatusPhase::Connected, .. })
        ));
        assert_eq!(closed(&events), "it closed the connection");
    }

    #[test]
    fn an_oversize_length_ends_the_conversation() {
        let mut bytes = status_frame();
        bytes.extend_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        bytes.extend(status_frame()); // never looked at
        let events = feed(bytes);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(closed(&events).contains("limit"), "{}", closed(&events));

        let events = feed(vec![0xff; 64]);
        assert_eq!(events.len(), 1);
        let events = feed(vec![0, 0, 0, 0]);
        assert!(closed(&events).contains("empty"));
    }

    #[test]
    fn a_stream_cut_mid_frame_ends_the_conversation() {
        let mut bytes = status_frame();
        bytes.truncate(bytes.len() - 3);
        let events = feed(bytes);
        assert_eq!(events.len(), 1);
        assert!(closed(&events).contains("middle of a frame"));
    }

    #[test]
    fn endless_nonsense_is_cut_off() {
        let mut bytes = Vec::new();
        for _ in 0..1000 {
            write_frame(&mut bytes, FRAME_CONTROL, b"{").unwrap();
        }
        bytes.extend(status_frame()); // never reached
        let events = feed(bytes);
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(closed(&events).contains("makes no sense"));
    }

    /// A writer that never accepts a byte, like a plugin that stopped reading its input.
    struct Stuck;
    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_secs(3600));
            Ok(0)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn send_reports_a_plugin_that_does_not_read_instead_of_blocking() {
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        // A reader that stays open until the test is done.
        struct Open(mpsc::Receiver<()>);
        impl Read for Open {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Ok(0)
            }
        }
        let proc = PluginProcess::from_io(Open(hold_rx), Stuck, |_| {});
        let started = Instant::now();
        let mut busy = false;
        for _ in 0..WRITE_QUEUE + 8 {
            if proc.send(&ToPlugin::RequestKeyframe) == Err(SendError::Busy) {
                busy = true;
                break;
            }
        }
        assert!(busy, "the queue is bounded");
        assert!(started.elapsed() < Duration::from_secs(2), "send never blocks");
        drop(hold_tx);
    }

    /// A scripted "plugin": answers are written up front, as if it replied instantly.
    fn client(answers: &[FromPlugin], timeout: Duration) -> Result<PluginClient> {
        let mut bytes = Vec::new();
        for a in answers {
            bytes.extend(control_frame(a).unwrap());
        }
        // Keep the stream open after the scripted answers so "no answer" is a timeout, not
        // an exit.
        struct ThenSilent(Cursor<Vec<u8>>);
        impl Read for ThenSilent {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.read(buf)? {
                    0 => {
                        std::thread::sleep(Duration::from_secs(3600));
                        Ok(0)
                    }
                    n => Ok(n),
                }
            }
        }
        let (tx, rx) = mpsc::sync_channel(64);
        let proc = PluginProcess::from_io(ThenSilent(Cursor::new(bytes)), std::io::sink(), move |m| {
            let _ = tx.send(m);
        });
        PluginClient::handshake(proc, rx, &info(), timeout)
    }

    fn hello(api: u32) -> FromPlugin {
        FromPlugin::Hello {
            name: "Scripted\u{7}".into(),
            version: "0.1".into(),
            api,
            capabilities: vec!["pairing".into()],
        }
    }

    #[test]
    fn handshake_checks_the_api_version_and_times_out() {
        let c = client(&[hello(API_VERSION)], Duration::from_secs(5)).unwrap();
        assert_eq!(c.hello.name, "Scripted", "control characters removed");
        assert_eq!(c.hello.capabilities, vec!["pairing".to_string()]);

        let e = client(&[hello(API_VERSION + 1)], Duration::from_secs(5)).err().unwrap();
        assert!(e.to_string().contains("API"), "{e}");

        let started = Instant::now();
        let e = client(&[], Duration::from_millis(200)).err().unwrap();
        assert!(e.to_string().contains("in time"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn requests_get_their_answers_and_console_lists_are_cleaned() {
        let entry = |id: &str, name: &str| proto::ConsoleEntry {
            id: id.into(),
            name: name.into(),
            model: "M\n1".into(),
            state: ConsoleState::Standby,
            needs_pairing: true,
        };
        let mut many = vec![entry("a", "Living\troom"), entry("", "no id"), entry("b", "")];
        many.extend((0..100).map(|i| entry(&format!("x{i}"), "more")));
        let mut c = client(
            &[
                hello(API_VERSION),
                // An unrelated message before the answer is skipped.
                FromPlugin::Rumble { left: 1.0, right: 0.0 },
                FromPlugin::Consoles { consoles: many },
                FromPlugin::PairResult { ok: false, error: Some("Wrong PIN".into()) },
                FromPlugin::PairResult { ok: true, error: None },
                FromPlugin::WakeResult { ok: false, error: None },
            ],
            Duration::from_secs(5),
        )
        .unwrap();
        let consoles = c.list_consoles().unwrap();
        assert_eq!(consoles.len(), MAX_CONSOLES);
        assert_eq!(consoles[0].name, "Livingroom");
        assert_eq!(consoles[0].model, "M1");
        assert!(consoles[0].needs_pairing);
        assert_eq!((consoles[1].id.as_str(), consoles[1].name.as_str()), ("b", "Console"));
        let none = BTreeMap::new();
        assert_eq!(c.pair("a", "0000", &none).unwrap_err().to_string(), "Wrong PIN");
        c.pair("a", "12345678", &none).unwrap();
        assert_eq!(c.wake("a").unwrap_err().to_string(), "The console could not be woken.");
    }
}
