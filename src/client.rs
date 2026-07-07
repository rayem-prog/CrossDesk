use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::clipboard::{Clipboard, ClipboardApi};
use crate::clipboard_sync::{ClipboardSyncState, clipboard_summary};
use crate::file_transfer;
use crate::input::InputSink;
use crate::platform::{ConnectionProfile, Platform};
use crate::protocol::{self, ClipboardPayload, Frame, FrameKind, InputEvent};
use crate::transport::SharedWriter;

const DEFAULT_INPUT_FLUSH_MS: u64 = 4;

pub fn run(server: &str) -> std::io::Result<()> {
    eprintln!("connecting to {server}");
    let mut stream = TcpStream::connect(server)?;
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

    let clipboard_state = Arc::new(Mutex::new(ClipboardSyncState::default()));
    eprintln!("initializing local clipboard");
    let mut clipboard = match Clipboard::new() {
        Ok(clipboard) => {
            spawn_clipboard_watcher(writer.clone(), Arc::clone(&clipboard_state));
            Some(clipboard)
        }
        Err(e) => {
            eprintln!("clipboard disabled: {e}");
            eprintln!("input sharing will continue without clipboard sync");
            None
        }
    };

    eprintln!("client ready (protocol v{})", protocol::VERSION);
    let receive_root = std::env::temp_dir().join("deskbridge-received");
    let mut incoming_files = file_transfer::IncomingBundle::new(receive_root);
    let mut input_log = InputLog::new();

    loop {
        let frame = match protocol::read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(_) if stop_requested.load(Ordering::Acquire) => return Ok(()),
            Err(e) => return Err(e),
        };
        match frame.kind {
            FrameKind::Input => {
                let event = protocol::decode_input(&frame.payload)?;
                input_log.observe(&event);
                input.send(event);
            }
            FrameKind::Clipboard => {
                let payload = protocol::decode_clipboard(&frame.payload)?;
                eprintln!("received clipboard {}", clipboard_summary(&payload));
                if let Some(clipboard) = clipboard.as_mut() {
                    if let Err(e) = clipboard.write(&payload) {
                        eprintln!("clipboard write failed: {e}");
                    } else {
                        note_remote_clipboard(&clipboard_state, &payload);
                    }
                } else {
                    eprintln!("clipboard write skipped because local clipboard is disabled");
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
                let files = incoming_files.finish();
                let payload = ClipboardPayload::Files(files);
                eprintln!("received clipboard {}", clipboard_summary(&payload));
                if let Some(clipboard) = clipboard.as_mut() {
                    if let Err(e) = clipboard.write(&payload) {
                        eprintln!("file clipboard write failed: {e}");
                    } else {
                        note_remote_clipboard(&clipboard_state, &payload);
                    }
                } else {
                    eprintln!("file clipboard write skipped because local clipboard is disabled");
                }
            }
            _ => {}
        }
    }
}

// Applies remote input on a dedicated worker thread. High-frequency relative
// mouse and wheel events are coalesced before touching the platform input API.
struct InputApplier {
    sender: Option<Sender<InputEvent>>,
    pending_motion: Arc<Mutex<PendingInput>>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct PendingInput {
    delta: (i32, i32),
    wheel: (i32, i32),
}

impl PendingInput {
    fn queue(&mut self, event: InputEvent) {
        match event {
            InputEvent::MouseDelta { dx, dy } => {
                self.delta.0 = self.delta.0.saturating_add(dx);
                self.delta.1 = self.delta.1.saturating_add(dy);
            }
            InputEvent::MouseWheel {
                horizontal,
                vertical,
            } => {
                self.wheel.0 = self.wheel.0.saturating_add(horizontal as i32);
                self.wheel.1 = self.wheel.1.saturating_add(vertical as i32);
            }
            _ => {}
        }
    }

    fn take_events(&mut self) -> Vec<InputEvent> {
        let mut events = Vec::with_capacity(2);
        if self.delta.0 != 0 || self.delta.1 != 0 {
            events.push(InputEvent::MouseDelta {
                dx: self.delta.0,
                dy: self.delta.1,
            });
            self.delta = (0, 0);
        }
        if self.wheel.0 != 0 || self.wheel.1 != 0 {
            events.push(InputEvent::MouseWheel {
                horizontal: clamp_i16(self.wheel.0),
                vertical: clamp_i16(self.wheel.1),
            });
            self.wheel = (0, 0);
        }
        events
    }
}

impl InputApplier {
    fn spawn(profile: ConnectionProfile) -> std::io::Result<(Self, (u32, u32))> {
        let (sender, receiver) = mpsc::channel();
        let pending_motion = Arc::new(Mutex::new(PendingInput::default()));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn({
            let pending_motion = Arc::clone(&pending_motion);
            move || match InputSink::new(profile) {
                Ok(mut input) => {
                    let screen_size = input.screen_size();
                    let _ = ready_sender.send(Ok(screen_size));
                    input_worker_loop(&mut input, receiver, pending_motion);
                }
                Err(e) => {
                    let _ = ready_sender.send(Err(e.to_string()));
                }
            }
        });
        let screen_size = match ready_receiver.recv() {
            Ok(Ok(screen_size)) => screen_size,
            Ok(Err(e)) => return Err(std::io::Error::other(e)),
            Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)),
        };
        Ok((
            Self {
                sender: Some(sender),
                pending_motion,
                worker: Some(worker),
            },
            screen_size,
        ))
    }

    fn send(&self, event: InputEvent) {
        match event {
            event @ (InputEvent::MouseDelta { .. } | InputEvent::MouseWheel { .. }) => {
                self.pending_motion.lock().unwrap().queue(event);
            }
            event => {
                let Some(sender) = self.sender.as_ref() else {
                    return;
                };
                if let Err(e) = sender.send(event) {
                    eprintln!("input apply queue closed: {e}");
                }
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

fn input_worker_loop(
    input: &mut InputSink,
    receiver: Receiver<InputEvent>,
    pending_motion: Arc<Mutex<PendingInput>>,
) {
    loop {
        match receiver.recv_timeout(input_flush_interval()) {
            Ok(event) => apply_ordered_input_event(input, event, &pending_motion),
            Err(RecvTimeoutError::Timeout) => flush_pending_input(input, &pending_motion),
            Err(RecvTimeoutError::Disconnected) => {
                flush_pending_input(input, &pending_motion);
                break;
            }
        }
    }
}

fn apply_ordered_input_event(
    input: &mut InputSink,
    event: InputEvent,
    pending_motion: &Arc<Mutex<PendingInput>>,
) {
    // MouseEnter establishes the remote cursor position first; other ordered
    // events must wait until pending relative motion has been applied.
    if matches!(event, InputEvent::MouseEnter { .. }) {
        apply_input_event(input, event);
        flush_pending_input(input, pending_motion);
    } else {
        flush_pending_input(input, pending_motion);
        apply_input_event(input, event);
    }
}

fn input_flush_interval() -> Duration {
    static INTERVAL: OnceLock<Duration> = OnceLock::new();
    *INTERVAL.get_or_init(|| {
        let ms = std::env::var("DESKBRIDGE_INPUT_FLUSH_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_INPUT_FLUSH_MS)
            .clamp(1, 16);
        Duration::from_millis(ms)
    })
}

fn flush_pending_input(input: &mut InputSink, pending_motion: &Arc<Mutex<PendingInput>>) {
    let events = pending_motion.lock().unwrap().take_events();
    for event in events {
        apply_input_event(input, event);
    }
}

fn apply_input_event(input: &mut InputSink, event: InputEvent) {
    if let Err(e) = input.apply(event) {
        eprintln!("input apply failed: {e}");
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

fn note_remote_clipboard(
    clipboard_state: &Arc<Mutex<ClipboardSyncState>>,
    payload: &ClipboardPayload,
) {
    clipboard_state.lock().unwrap().note_remote_write(payload);
}
