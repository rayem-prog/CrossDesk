// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use crate::secure::SecureStream;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::clipboard::session::ClipboardSession;
use crate::clipboard::{Clipboard, ClipboardApi};
use crate::file_transfer;
use crate::input::InputSink;
use crate::input::latency::{LatencyStats, Stage};
use crate::input::queue::EventQueue;
use crate::platform::{ConnectionProfile, Platform};
use crate::protocol::{self, ClipboardPayload, Frame, FrameKind, InputEvent};
use crate::transport::SharedWriter;

const REMOTE_CLIPBOARD_SUPPRESS_WINDOW: Duration = Duration::from_millis(1200);
const DEFAULT_INPUT_FLUSH_MS: u64 = 4;

fn apply_input_queue(queue: &EventQueue, mut apply: impl FnMut(InputEvent) -> std::io::Result<()>) {
    let mut stats = LatencyStats::from_env("receive");
    while let Some(item) = queue.recv() {
        if stats.enabled() {
            stats.record(Stage::ReceiveQueue, item.enqueued.elapsed());
            stats.queue(queue.take_peak_depth(), item.samples);
        }
        let started = stats.enabled().then(Instant::now);
        if let Err(error) = apply(item.event) {
            eprintln!("input apply failed: {error}");
        }
        if let Some(started) = started {
            stats.record(Stage::Inject, started.elapsed());
        }
        if let Some(report) = stats.maybe_report() {
            eprintln!("{report}");
        }
    }
}

#[cfg(test)]
mod input_queue_tests {
    use super::*;
    use crate::protocol::MouseButton;
    #[test]
    fn receiver_applies_queued_motion_once_before_the_click_without_new_waits() {
        let queue = EventQueue::new(256);
        queue.push(InputEvent::MouseEnter { x: 10, y: 10 }).unwrap();
        for _ in 0..8000 {
            queue.push(InputEvent::MouseDelta { dx: 1, dy: 1 }).unwrap();
        }
        queue
            .push(InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
            })
            .unwrap();
        queue
            .push(InputEvent::MouseDelta { dx: -3, dy: 0 })
            .unwrap();
        queue
            .push(InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false,
            })
            .unwrap();
        let mut actual = Vec::new();
        apply_input_queue(&queue, |event| {
            actual.push(event);
            if actual.len() == 5 {
                queue.close();
            }
            Ok(())
        });
        assert_eq!(
            actual,
            vec![
                InputEvent::MouseEnter { x: 10, y: 10 },
                InputEvent::MouseDelta { dx: 8000, dy: 8000 },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: true
                },
                InputEvent::MouseDelta { dx: -3, dy: 0 },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: false
                }
            ]
        );
    }
    #[test]
    fn receiver_close_discards_old_motion_and_does_not_apply_it_again() {
        let queue = EventQueue::new(256);
        queue.push(InputEvent::MouseDelta { dx: 4, dy: 2 }).unwrap();
        queue.close();
        apply_input_queue(&queue, |_| panic!("replayed input after close"));
    }
}

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
            Err(error) => {
                crate::service_event::emit(
                    crate::service_event::ServiceState::Retrying,
                    Some(error.to_string()),
                );
                eprintln!("Disconnected: {error}. Retrying in one second; Ctrl+C exits.");
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

fn run_session(server: &str, key: &[u8; 32]) -> std::io::Result<()> {
    crate::service_event::emit(crate::service_event::ServiceState::Connecting, None);
    eprintln!("connecting to {server}");
    let mut stream = SecureStream::connect(server, key)?;
    stream.set_nodelay(true)?;
    let endpoint = stream.peer_addr()?;
    let writer = SharedWriter::new(stream.try_clone()?);
    let _transport = crate::shutdown::register_transport(&stream)?;
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
    let mut images = ClipboardSession::start(
        writer.clone(),
        crate::transport::image_clipboard_enabled() && server_hello.image_clipboard,
        crate::transport::file_clipboard_enabled() && server_hello.file_clipboard,
        crate::transport::text_clipboard_enabled() && server_hello.text_clipboard,
    )?;
    crate::service_event::capabilities(
        crate::transport::text_clipboard_enabled() && server_hello.text_clipboard,
    );
    // Drop input before clipboard teardown can wait for an outstanding disk/network operation.
    let input = input;
    let _diagnostic_session = crate::diagnostics::begin_session();
    let mut input_log = InputLog::new();
    crate::service_event::emit(crate::service_event::ServiceState::Connected, None);
    crate::edge_drop::primary_connected(server_hello.edge_drop);
    let _edge_session = ClientEdgeSession;
    let lane = (server_hello.edge_drop && crate::edge_channel::supported())
        .then(|| crate::edge_channel::ClientLane::start(endpoint, *key, writer.clone()));

    loop {
        if crate::shutdown::requested() {
            return Ok(());
        }
        let frame = match protocol::read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(_) if crate::shutdown::requested() => return Ok(()),
            Err(e) => return Err(e),
        };
        if frame.kind == FrameKind::DropTicket {
            if let Some(lane) = &lane {
                lane.receive(frame.payload);
            }
            continue;
        }
        if ClipboardSession::handles(frame.kind) {
            if let Err(error) = images.receive(frame) {
                eprintln!("clipboard transfer rejected: {error}");
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
                input.send(event)?;
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

// Coalesce only already-queued monotonic motion; native controls retain their order.
struct InputApplier {
    queue: Arc<EventQueue>,
    worker: Option<thread::JoinHandle<()>>,
}
impl InputApplier {
    fn spawn(profile: ConnectionProfile) -> std::io::Result<(Self, (u32, u32))> {
        let queue = Arc::new(EventQueue::new(256));
        let target = queue.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("input-apply".into())
            .spawn(move || match InputSink::new(profile) {
                Ok(mut input) => {
                    let _ = ready_tx.send(Ok(input.screen_size()));
                    apply_input_queue(&target, |event| input.apply(event));
                    // Dropping InputSink releases all remotely pressed keys/buttons.
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        let size = match ready_rx
            .recv()
            .map_err(std::io::Error::other)
            .and_then(|result| result)
        {
            Ok(size) => size,
            Err(error) => {
                queue.close();
                let _ = worker.join();
                return Err(error);
            }
        };
        Ok((
            Self {
                queue,
                worker: Some(worker),
            },
            size,
        ))
    }
    fn send(&self, event: InputEvent) -> std::io::Result<()> {
        self.queue.push(event)
    }
}
impl Drop for InputApplier {
    fn drop(&mut self) {
        self.queue.close();
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

struct ClientEdgeSession;
impl Drop for ClientEdgeSession {
    fn drop(&mut self) {
        crate::edge_drop::primary_disconnected();
    }
}
