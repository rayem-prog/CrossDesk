// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use crate::secure::SecureStream;
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::clipboard::session::ImageSession;
use crate::clipboard::{Clipboard, ClipboardApi};
use crate::file_transfer;
use crate::input::InputSink;
use crate::platform::{ConnectionProfile, Platform};
use crate::protocol::{self, ClipboardPayload, Frame, FrameKind, InputEvent};
use crate::transport::SharedWriter;

const REMOTE_CLIPBOARD_SUPPRESS_WINDOW: Duration = Duration::from_millis(1200);
const DEFAULT_INPUT_FLUSH_MS: u64 = 4;

pub fn run(server: &str) -> std::io::Result<()> {
    let key = crate::secure::load_key()?;
    while !crate::shutdown::requested() {
        match run_session(server, &key) {
            Ok(()) => return Ok(()),
            Err(error) if crate::shutdown::requested() => {
                let _ = error;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(error);
            }
            Err(error) => eprintln!("Disconnected: {error}. Retrying in one second; Ctrl+C exits."),
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn run_session(server: &str, key: &[u8; 32]) -> std::io::Result<()> {
    eprintln!("connecting to {server}");
    let mut stream = SecureStream::connect(server, key)?;
    stream.set_nodelay(true)?;
    let writer = SharedWriter::new(stream.try_clone()?);
    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_stream = stream.try_clone()?;
    let stop_flag = Arc::clone(&stop_requested);
    crate::shutdown::spawn_gui_stop_watcher(move || {
        stop_flag.store(true, Ordering::Release);
        let _ = stop_stream.shutdown(Shutdown::Both);
    });
    eprintln!("connected; waiting for server hello");
    let server_hello = protocol::read_frame(&mut stream)?;
    if server_hello.kind != FrameKind::Hello {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "server did not complete the protocol handshake",
        ));
    }
    let server_hello = protocol::decode_hello(&server_hello.payload)?;
    protocol::validate_version(server_hello)?;
    let profile = ConnectionProfile::local_client(server_hello.platform);
    eprintln!(
        "connection profile: {} server -> {} client ({})",
        server_hello.platform.as_str(),
        Platform::local().as_str(),
        profile.as_str()
    );

    eprintln!("initializing local input sink");
    let (input, (width, height)) = InputApplier::spawn(profile)?;
    eprintln!("sending client hello with screen {width}x{height}");
    writer.write(crate::protocol::Frame::new(
        FrameKind::Hello,
        protocol::hello_payload_with_screen(width, height),
    ))?;

    eprintln!("initializing local clipboard");
    let mut clipboard = Clipboard::new()?;

    eprintln!("client ready (protocol v{})", protocol::VERSION);
    let receive_root = std::env::temp_dir().join("deskbridge-received");
    let mut incoming_files = file_transfer::IncomingBundle::new(receive_root);
    let clipboard_state = Arc::new(Mutex::new(ClipboardSyncState::default()));
    let mut images = ImageSession::start(
        writer.clone(),
        crate::transport::image_clipboard_enabled() && server_hello.image_clipboard,
    )?;
    if !images.enabled() {
        spawn_clipboard_watcher(writer.clone(), Arc::clone(&clipboard_state));
    }
    let mut input_log = InputLog::new();

    loop {
        if crate::shutdown::requested() {
            return Ok(());
        }
        let frame = match protocol::read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(_) if stop_requested.load(Ordering::Acquire) => return Ok(()),
            Err(e) => return Err(e),
        };
        if ImageSession::handles(frame.kind) {
            if let Err(error) = images.receive(frame) {
                eprintln!("image transfer rejected: {error}");
            }
            continue;
        }
        if !crate::transport::clipboard_enabled()
            && !matches!(frame.kind, FrameKind::Ping | FrameKind::Input)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unexpected server frame in input-only mode",
            ));
        }
        match frame.kind {
            FrameKind::Ping => {
                writer.write(protocol::Frame::new(FrameKind::Pong, frame.payload))?;
            }
            FrameKind::Input => {
                let event = protocol::decode_input(&frame.payload)?;
                input_log.observe(&event);
                input.send(event);
            }
            FrameKind::Clipboard => {
                let payload = protocol::decode_clipboard(&frame.payload)?;
                eprintln!("received clipboard {}", clipboard_summary(&payload));
                if let Err(e) = clipboard.write(&payload) {
                    eprintln!("clipboard write failed: {e}");
                } else {
                    note_remote_clipboard(&clipboard_state, &payload);
                }
            }
            FrameKind::FileStart => {
                let (relative, len) = protocol::decode_file_start(&frame.payload)?;
                incoming_files.start_file(&relative, len)?;
            }
            FrameKind::FileChunk => {
                incoming_files.write_chunk(&frame.payload)?;
            }
            FrameKind::DragEnd => {
                let files = incoming_files.finish()?;
                let payload = ClipboardPayload::Files(files);
                eprintln!("received clipboard {}", clipboard_summary(&payload));
                if let Err(e) = clipboard.write(&payload) {
                    eprintln!("file clipboard write failed: {e}");
                } else {
                    note_remote_clipboard(&clipboard_state, &payload);
                }
            }
            _ => {}
        }
    }
}

// The server already coalesces motion. Keeping every event in one ordered
// queue prevents a later motion from jumping ahead of a mouse-down/up.
struct InputApplier {
    sender: Option<mpsc::SyncSender<InputEvent>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl InputApplier {
    fn spawn(profile: ConnectionProfile) -> std::io::Result<(Self, (u32, u32))> {
        let (sender, receiver) = mpsc::sync_channel(256);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || match InputSink::new(profile) {
            Ok(mut input) => {
                let _ = ready_tx.send(Ok(input.screen_size()));
                for event in receiver {
                    if let Err(error) = input.apply(event) {
                        eprintln!("input apply failed: {error}");
                    }
                }
                // Dropping InputSink releases all remotely pressed keys/buttons.
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error));
            }
        });
        let size = ready_rx.recv().map_err(std::io::Error::other)??;
        Ok((
            Self {
                sender: Some(sender),
                worker: Some(worker),
            },
            size,
        ))
    }
    fn send(&self, event: InputEvent) {
        if let Some(sender) = &self.sender {
            if let Err(error) = sender.send(event) {
                eprintln!("input queue closed: {error}");
            }
        }
    }
}
impl Drop for InputApplier {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

struct InputLog {
    enabled: bool,
    count: u64,
    last_print: Instant,
}

impl Default for InputLog {
    fn default() -> Self {
        Self {
            enabled: false,
            count: 0,
            last_print: Instant::now() - Duration::from_secs(2),
        }
    }
}

impl InputLog {
    fn new() -> Self {
        Self {
            enabled: std::env::var_os("DESKBRIDGE_INPUT_LOG").is_some(),
            ..Self::default()
        }
    }

    fn observe(&mut self, event: &InputEvent) {
        if !self.enabled {
            return;
        }
        self.count += 1;
        if self.count == 1 || self.last_print.elapsed() >= Duration::from_secs(1) {
            eprintln!("received input event #{}: {:?}", self.count, event);
            self.last_print = Instant::now();
        }
    }
}

fn spawn_clipboard_watcher(writer: SharedWriter, clipboard_state: Arc<Mutex<ClipboardSyncState>>) {
    if !crate::transport::clipboard_enabled() {
        return;
    }
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(e) => {
                eprintln!("clipboard watcher disabled: {e}");
                return;
            }
        };
        eprintln!("clipboard watcher active");
        loop {
            thread::sleep(Duration::from_millis(450));
            let payload = match clipboard.read() {
                Ok(Some(payload)) => payload,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("clipboard read failed: {e}");
                    continue;
                }
            };
            let encoded = protocol::encode_clipboard(&payload);
            if !clipboard_state
                .lock()
                .unwrap()
                .accept_local_change(&payload, encoded)
            {
                continue;
            }
            eprintln!("local clipboard changed {}", clipboard_summary(&payload));
            if let Err(e) = send_clipboard_payload(&writer, &payload) {
                eprintln!("clipboard send failed: {e}");
            } else {
                eprintln!("sent clipboard {}", clipboard_summary(&payload));
            }
        }
    });
}

fn send_clipboard_payload(
    writer: &SharedWriter,
    payload: &ClipboardPayload,
) -> std::io::Result<()> {
    match payload {
        ClipboardPayload::Files(files) => {
            file_transfer::send_files(writer, files)?;
            writer.write(Frame::new(FrameKind::DragEnd, Vec::new()))
        }
        _ => writer.write(Frame::new(
            FrameKind::Clipboard,
            protocol::encode_clipboard(payload),
        )),
    }
}

#[derive(Default)]
struct ClipboardSyncState {
    last_observed: Option<Vec<u8>>,
    suppress_next_kind: Option<ClipboardKind>,
    suppress_until: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipboardKind {
    Text,
    Bitmap,
    Files,
}

impl ClipboardSyncState {
    fn accept_local_change(&mut self, payload: &ClipboardPayload, encoded: Vec<u8>) -> bool {
        if self.last_observed.as_ref() == Some(&encoded) {
            return false;
        }

        let kind = ClipboardKind::from_payload(payload);
        if self.should_suppress(kind) {
            eprintln!("suppressed remote clipboard echo");
            self.last_observed = Some(encoded);
            return false;
        }

        self.clear_suppression();
        self.last_observed = Some(encoded);
        true
    }

    fn note_remote_write(&mut self, payload: &ClipboardPayload) {
        self.last_observed = Some(protocol::encode_clipboard(payload));
        self.suppress_next_kind = Some(ClipboardKind::from_payload(payload));
        self.suppress_until = Some(Instant::now() + REMOTE_CLIPBOARD_SUPPRESS_WINDOW);
    }

    fn should_suppress(&mut self, kind: ClipboardKind) -> bool {
        if self.suppress_next_kind != Some(kind) {
            return false;
        }
        if self
            .suppress_until
            .map(|until| Instant::now() <= until)
            .unwrap_or(false)
        {
            self.clear_suppression();
            return true;
        }
        self.clear_suppression();
        false
    }

    fn clear_suppression(&mut self) {
        self.suppress_next_kind = None;
        self.suppress_until = None;
    }
}

impl ClipboardKind {
    fn from_payload(payload: &ClipboardPayload) -> Self {
        match payload {
            ClipboardPayload::Text(_) => Self::Text,
            ClipboardPayload::Bitmap(_) => Self::Bitmap,
            ClipboardPayload::Files(_) => Self::Files,
        }
    }
}

fn note_remote_clipboard(
    clipboard_state: &Arc<Mutex<ClipboardSyncState>>,
    payload: &ClipboardPayload,
) {
    clipboard_state.lock().unwrap().note_remote_write(payload);
}

fn clipboard_summary(payload: &ClipboardPayload) -> String {
    match payload {
        ClipboardPayload::Text(text) => format!("text ({} chars)", text.chars().count()),
        ClipboardPayload::Bitmap(bitmap) => format!("image ({} bytes)", bitmap.len()),
        ClipboardPayload::Files(files) => format!("files ({} item(s))", files.len()),
    }
}
