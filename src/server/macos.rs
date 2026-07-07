use std::collections::HashSet;
use std::ffi::c_void;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use super::macos_capture::{KeyboardRouter, ModifierMapping};
use super::{Edge, ServerConfig};
use crate::clipboard::{Clipboard, ClipboardApi};
use crate::clipboard_sync::{ClipboardSyncState, clipboard_summary};
use crate::file_transfer;
use crate::platform::{ConnectionProfile, Platform};
use crate::pointer::{MotionAction, PointerRouter};
use crate::protocol::{self, ClipboardPayload, Frame, FrameKind, InputEvent, MouseButton};
use crate::transport::SharedWriter;

const DEFAULT_REMOTE_WIDTH: i32 = 1366;
const DEFAULT_REMOTE_HEIGHT: i32 = 768;
const DEFAULT_INPUT_FLUSH_MS: u64 = 4;
const REMOTE_CURSOR_REPIN_INTERVAL: Duration = Duration::from_millis(16);
const TAP_MOUSE_MOVED: u32 = 1;
const TAP_MOUSE_LEFT_DOWN: u32 = 2;
const TAP_MOUSE_LEFT_UP: u32 = 3;
const TAP_MOUSE_RIGHT_DOWN: u32 = 4;
const TAP_MOUSE_RIGHT_UP: u32 = 5;
const TAP_MOUSE_OTHER_DOWN: u32 = 6;
const TAP_MOUSE_OTHER_UP: u32 = 7;
const TAP_SCROLL: u32 = 20;
const TAP_KEY_DOWN: u32 = 21;
const TAP_KEY_UP: u32 = 22;
const TAP_FLAGS_CHANGED: u32 = 23;

static CAPTURE_STATE: OnceLock<Arc<Mutex<CaptureState>>> = OnceLock::new();

unsafe extern "C" {
    fn deskbridge_event_tap_run(
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void, u32, i64, i64, i64, i64, f64, f64) -> bool,
    ) -> i32;
    fn deskbridge_event_tap_stop();
    fn deskbridge_macos_set_cursor_position(x: f64, y: f64) -> i32;
    fn deskbridge_macos_capture_cursor_association() -> i32;
    fn deskbridge_macos_restore_cursor_association() -> i32;
}

pub fn run(config: ServerConfig) -> std::io::Result<()> {
    let listener = TcpListener::bind(&config.bind)?;
    eprintln!("deskbridge macOS server listening on {}", config.bind);
    let (stream, addr, writer, remote_size, client_platform) = loop {
        let (mut stream, addr) = listener.accept()?;
        stream.set_nodelay(true)?;
        eprintln!("client connected from {addr}");

        let writer = SharedWriter::new(stream.try_clone()?);
        if let Err(e) = writer.write(Frame::new(FrameKind::Hello, protocol::hello_payload())) {
            eprintln!("failed to send hello to {addr}: {e}; waiting for another client");
            continue;
        }

        match read_client_hello(&mut stream) {
            Ok((remote_size, client_platform)) => {
                break (stream, addr, writer, remote_size, client_platform);
            }
            Err(e) => {
                eprintln!(
                    "client {addr} disconnected during handshake: {e}; waiting for another client"
                );
                continue;
            }
        }
    };
    eprintln!(
        "remote screen {}x{}, edge {:?}, client {}",
        remote_size.0, remote_size.1, config.edge, addr
    );

    let clipboard_state = Arc::new(Mutex::new(ClipboardSyncState::default()));
    let remote_profile = ConnectionProfile::local_server(client_platform);
    let input = InputEmitter::spawn(writer.clone());
    let state = Arc::new(Mutex::new(CaptureState::new(
        input,
        config.edge,
        remote_size,
        remote_profile,
    )));
    if CAPTURE_STATE.set(state).is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "capture state already initialized",
        ));
    }
    crate::shutdown::spawn_gui_stop_watcher(|| {
        restore_capture_to_local();
        unsafe {
            deskbridge_event_tap_stop();
        }
    });

    let connection_error = Arc::new(Mutex::new(None));
    spawn_inbound_reader(
        stream,
        Arc::clone(&clipboard_state),
        Arc::clone(&connection_error),
    );
    spawn_clipboard_watcher(writer, clipboard_state);

    eprintln!(
        "macOS event tap starting; move through the configured edge to control the remote device"
    );
    let status = unsafe { deskbridge_event_tap_run(std::ptr::null_mut(), event_tap_callback) };
    restore_capture_to_local();
    if let Some(message) = connection_error.lock().unwrap().take() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            message,
        ));
    }
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "failed to start macOS event tap (native status {status}); grant Accessibility/Input Monitoring permission"
            ),
        ))
    }
}

fn read_client_hello(stream: &mut TcpStream) -> std::io::Result<((i32, i32), Platform)> {
    let frame = protocol::read_frame(stream)?;
    if frame.kind != FrameKind::Hello {
        return Ok((
            (DEFAULT_REMOTE_WIDTH, DEFAULT_REMOTE_HEIGHT),
            Platform::Unknown,
        ));
    }
    let hello = protocol::decode_hello(&frame.payload)?;
    protocol::validate_version(hello)?;
    let width = hello.screen_width.unwrap_or(DEFAULT_REMOTE_WIDTH as u32) as i32;
    let height = hello.screen_height.unwrap_or(DEFAULT_REMOTE_HEIGHT as u32) as i32;
    Ok(((width.max(1), height.max(1)), hello.platform))
}

fn spawn_inbound_reader(
    mut stream: TcpStream,
    clipboard_state: Arc<Mutex<ClipboardSyncState>>,
    connection_error: Arc<Mutex<Option<String>>>,
) {
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => Some(clipboard),
            Err(e) => {
                eprintln!("clipboard receiver disabled, input connection remains active: {e}");
                None
            }
        };
        let receive_root = std::env::temp_dir().join("deskbridge-received");
        let mut incoming_files = file_transfer::IncomingBundle::new(receive_root);

        loop {
            let frame = match protocol::read_frame(&mut stream) {
                Ok(frame) => frame,
                Err(e) => {
                    eprintln!("connection closed: {e}");
                    *connection_error.lock().unwrap() = Some(e.to_string());
                    restore_capture_to_local();
                    unsafe {
                        deskbridge_event_tap_stop();
                    }
                    return;
                }
            };
            match frame.kind {
                FrameKind::Clipboard => match protocol::decode_clipboard(&frame.payload) {
                    Ok(payload) => {
                        eprintln!("received clipboard {}", clipboard_summary(&payload));
                        if let Some(clipboard) = clipboard.as_mut() {
                            if let Err(e) = clipboard.write(&payload) {
                                eprintln!("clipboard write failed: {e}");
                            } else {
                                note_remote_clipboard(&clipboard_state, &payload);
                            }
                        }
                    }
                    Err(e) => eprintln!("clipboard decode failed: {e}"),
                },
                FrameKind::FileStart => match protocol::decode_file_start(&frame.payload) {
                    Ok((relative, len)) => {
                        if let Err(e) = incoming_files.start_file(&relative, len) {
                            eprintln!("file receive failed: {e}");
                        }
                    }
                    Err(e) => eprintln!("file start decode failed: {e}"),
                },
                FrameKind::FileChunk => {
                    if let Err(e) = incoming_files.write_chunk(&frame.payload) {
                        eprintln!("file chunk failed: {e}");
                    }
                }
                FrameKind::DragEnd => {
                    let payload = ClipboardPayload::Files(incoming_files.finish());
                    eprintln!("received clipboard {}", clipboard_summary(&payload));
                    if let Some(clipboard) = clipboard.as_mut() {
                        if let Err(e) = clipboard.write(&payload) {
                            eprintln!("file clipboard write failed: {e}");
                        } else {
                            note_remote_clipboard(&clipboard_state, &payload);
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

fn restore_capture_to_local() {
    if let Some(state) = CAPTURE_STATE.get() {
        state.lock().unwrap().restore_to_local();
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

#[derive(Clone)]
struct InputEmitter {
    sender: Sender<InputEvent>,
    pending_motion: Arc<Mutex<PendingInput>>,
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

impl InputEmitter {
    fn spawn(writer: SharedWriter) -> Self {
        let (sender, receiver) = mpsc::channel();
        let pending_motion = Arc::new(Mutex::new(PendingInput::default()));
        thread::spawn({
            let pending_motion = Arc::clone(&pending_motion);
            move || input_writer_loop(writer, receiver, pending_motion)
        });
        Self {
            sender,
            pending_motion,
        }
    }

    fn send(&self, event: InputEvent) {
        match event {
            event @ (InputEvent::MouseDelta { .. } | InputEvent::MouseWheel { .. }) => {
                self.pending_motion.lock().unwrap().queue(event);
            }
            event => {
                if let Err(e) = self.sender.send(event) {
                    eprintln!("input queue closed: {e}");
                }
            }
        }
    }
}

fn input_writer_loop(
    writer: SharedWriter,
    receiver: Receiver<InputEvent>,
    pending_motion: Arc<Mutex<PendingInput>>,
) {
    let mut log = InputSendLog::new();

    loop {
        match receiver.recv_timeout(input_flush_interval()) {
            Ok(event) => write_ordered_input_event(&writer, event, &pending_motion, &mut log),
            Err(RecvTimeoutError::Timeout) => {
                flush_pending_input(&writer, &pending_motion, &mut log);
            }
            Err(RecvTimeoutError::Disconnected) => {
                flush_pending_input(&writer, &pending_motion, &mut log);
                break;
            }
        }
    }
}

fn write_ordered_input_event(
    writer: &SharedWriter,
    event: InputEvent,
    pending_motion: &Arc<Mutex<PendingInput>>,
    log: &mut InputSendLog,
) {
    if matches!(event, InputEvent::MouseEnter { .. }) {
        write_input_event(writer, event, log);
        flush_pending_input(writer, pending_motion, log);
    } else {
        flush_pending_input(writer, pending_motion, log);
        write_input_event(writer, event, log);
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

fn flush_pending_input(
    writer: &SharedWriter,
    pending_motion: &Arc<Mutex<PendingInput>>,
    log: &mut InputSendLog,
) {
    let events = pending_motion.lock().unwrap().take_events();
    for event in events {
        write_input_event(writer, event, log);
    }
}

struct InputSendLog {
    enabled: bool,
    count: u64,
    last_print: Instant,
}

impl Default for InputSendLog {
    fn default() -> Self {
        Self {
            enabled: false,
            count: 0,
            last_print: Instant::now() - Duration::from_secs(2),
        }
    }
}

impl InputSendLog {
    fn new() -> Self {
        Self {
            enabled: std::env::var_os("DESKBRIDGE_INPUT_LOG").is_some(),
            ..Self::default()
        }
    }
}

fn write_input_event(writer: &SharedWriter, event: InputEvent, log: &mut InputSendLog) {
    let encoded = protocol::encode_input(&event);
    if let Err(e) = writer.write_input(Frame::new(FrameKind::Input, encoded)) {
        eprintln!("input send failed after {} sent event(s): {e}", log.count);
        return;
    }
    log.count += 1;
    if !log.enabled {
        return;
    }
    if log.count == 1 || log.last_print.elapsed() >= Duration::from_secs(1) {
        eprintln!("sent input event #{}: {:?}", log.count, event);
        log.last_print = Instant::now();
    }
}

#[derive(Clone, Copy)]
struct TapEvent {
    kind: u32,
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    x: f64,
    y: f64,
}

extern "C" fn event_tap_callback(
    _context: *mut c_void,
    kind: u32,
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    x: f64,
    y: f64,
) -> bool {
    let event = TapEvent {
        kind,
        a,
        b,
        c,
        d,
        x,
        y,
    };
    CAPTURE_STATE
        .get()
        .map(|state| state.lock().unwrap().handle_tap(event))
        .unwrap_or(false)
}

struct CaptureState {
    input: InputEmitter,
    pointer: PointerRouter,
    keyboard: KeyboardRouter,
    profile: ConnectionProfile,
    remote_buttons: HashSet<MouseButton>,
    remote_cursor_lock: RemoteCursorLock,
}

#[derive(Debug, Default)]
struct RemoteCursorLock {
    anchor: Option<(i32, i32)>,
    last_pin: Option<Instant>,
}

impl RemoteCursorLock {
    fn begin(&mut self, edge_x: i32, edge_y: i32, size: (i32, i32)) -> (i32, i32) {
        let anchor = (
            edge_x.clamp(0, size.0.saturating_sub(1)),
            edge_y.clamp(0, size.1.saturating_sub(1)),
        );
        self.anchor = Some(anchor);
        self.last_pin = None;
        anchor
    }

    fn position(&self) -> Option<(i32, i32)> {
        self.anchor
    }

    fn mark_pinned(&mut self) {
        if self.anchor.is_some() {
            self.last_pin = Some(Instant::now());
        }
    }

    fn repin_position_if_due(&mut self) -> Option<(i32, i32)> {
        let anchor = self.anchor?;
        let due = self
            .last_pin
            .map(|last| last.elapsed() >= REMOTE_CURSOR_REPIN_INTERVAL)
            .unwrap_or(true);
        if due {
            self.last_pin = Some(Instant::now());
            Some(anchor)
        } else {
            None
        }
    }

    fn end(&mut self) {
        self.anchor = None;
        self.last_pin = None;
    }
}

impl CaptureState {
    fn new(
        input: InputEmitter,
        edge: Edge,
        remote_size: (i32, i32),
        profile: ConnectionProfile,
    ) -> Self {
        Self {
            input,
            pointer: PointerRouter::new(edge, screen_size_i32(), remote_size),
            keyboard: KeyboardRouter::new(ModifierMapping::for_profile(profile)),
            profile,
            remote_buttons: HashSet::new(),
            remote_cursor_lock: RemoteCursorLock::default(),
        }
    }

    fn handle_tap(&mut self, event: TapEvent) -> bool {
        match event.kind {
            TAP_MOUSE_MOVED => self.handle_mouse_move(event),
            TAP_MOUSE_LEFT_DOWN | TAP_MOUSE_LEFT_UP | TAP_MOUSE_RIGHT_DOWN | TAP_MOUSE_RIGHT_UP
            | TAP_MOUSE_OTHER_DOWN | TAP_MOUSE_OTHER_UP => self.handle_mouse_button(event),
            TAP_SCROLL => self.handle_scroll(event),
            TAP_KEY_DOWN | TAP_KEY_UP | TAP_FLAGS_CHANGED => self.handle_keyboard(event),
            _ => false,
        }
    }

    fn handle_mouse_move(&mut self, event: TapEvent) -> bool {
        if self.pointer.is_remote() {
            let dx = event.b as i32;
            let dy = event.c as i32;
            if dx == 0 && dy == 0 {
                self.repin_remote_cursor_if_due();
                return true;
            }

            match self
                .pointer
                .observe_remote_motion(dx, dy, self.remote_buttons.is_empty())
            {
                MotionAction::MoveRemote { dx, dy } => {
                    self.send_input(InputEvent::MouseDelta { dx, dy });
                    self.repin_remote_cursor_if_due();
                }
                MotionAction::ReturnLocal { x, y } => {
                    self.finish_remote_session();
                    self.restore_local_cursor_at(x, y);
                    eprintln!("released control back to macOS");
                }
                MotionAction::Local | MotionAction::EnterRemote { .. } => {}
            }
            return true;
        }

        self.pointer.update_local_size(screen_size_i32());
        if let MotionAction::EnterRemote { x, y } = self
            .pointer
            .observe_local_motion(event.x.round() as i32, event.y.round() as i32)
        {
            self.send_input(InputEvent::MouseEnter { x, y });
            for input in self.keyboard.sync_flags(event.d as u64) {
                self.send_input(input);
            }
            self.begin_remote_cursor_capture(event.x.round() as i32, event.y.round() as i32);
            eprintln!("entered remote control at {x},{y}; push back through the edge to release");
            return true;
        }
        false
    }

    fn handle_mouse_button(&mut self, event: TapEvent) -> bool {
        let Some((button, down)) = mouse_button_from_tap(event) else {
            return false;
        };
        if !self.pointer.is_remote() {
            self.pointer.observe_local_button(down);
            return false;
        }
        if down {
            self.remote_buttons.insert(button);
        } else {
            self.remote_buttons.remove(&button);
        }
        self.send_input(InputEvent::MouseButton { button, down });
        true
    }

    fn handle_scroll(&mut self, event: TapEvent) -> bool {
        if !self.pointer.is_remote() {
            return false;
        }
        self.send_input(InputEvent::MouseWheel {
            horizontal: mac_scroll_axis_to_remote(event.a, self.profile),
            vertical: mac_scroll_axis_to_remote(event.b, self.profile),
        });
        true
    }

    fn handle_keyboard(&mut self, event: TapEvent) -> bool {
        if !self.pointer.is_remote() {
            return false;
        }

        let mac_keycode = event.a as u16;
        match event.kind {
            TAP_KEY_DOWN => {
                let repeat = event.b != 0;
                for input in self.keyboard.key_down(mac_keycode, repeat) {
                    self.send_input(input);
                }
            }
            TAP_KEY_UP => {
                for input in self.keyboard.key_up(mac_keycode) {
                    self.send_input(input);
                }
            }
            TAP_FLAGS_CHANGED => {
                for input in self.keyboard.flags_changed(mac_keycode, event.c as u64) {
                    self.send_input(input);
                }
            }
            _ => {}
        }
        true
    }

    fn repin_remote_cursor_if_due(&mut self) {
        if let Some((x, y)) = self.remote_cursor_lock.repin_position_if_due() {
            self.set_cursor_position(x, y);
        }
    }

    fn set_cursor_position(&self, x: i32, y: i32) {
        let status = unsafe { deskbridge_macos_set_cursor_position(x as f64, y as f64) };
        if status != 0 {
            eprintln!("failed to warp macOS cursor (native status {status})");
        }
    }

    fn begin_remote_cursor_capture(&mut self, edge_x: i32, edge_y: i32) {
        let size = screen_size_i32();
        let (x, y) = self.remote_cursor_lock.begin(edge_x, edge_y, size);
        self.set_cursor_position(x, y);
        self.remote_cursor_lock.mark_pinned();
        self.capture_cursor_association();
    }

    fn end_remote_cursor_capture(&mut self) {
        self.remote_cursor_lock.end();
        self.restore_cursor_association();
    }

    fn restore_local_cursor_at(&mut self, x: i32, y: i32) {
        self.remote_cursor_lock.end();
        self.set_cursor_position(x, y);
        self.restore_cursor_association();
    }

    fn capture_cursor_association(&self) {
        let status = unsafe { deskbridge_macos_capture_cursor_association() };
        if status != 0 {
            eprintln!("failed to capture macOS cursor association (native status {status})");
        }
    }

    fn restore_cursor_association(&self) {
        let status = unsafe { deskbridge_macos_restore_cursor_association() };
        if status != 0 {
            eprintln!("failed to restore macOS cursor association (native status {status})");
        }
    }

    fn release_remote_keys(&mut self) {
        for input in self.keyboard.release_all() {
            self.send_input(input);
        }
    }

    fn release_remote_buttons(&mut self) {
        for button in self.remote_buttons.drain().collect::<Vec<_>>() {
            self.input.send(InputEvent::MouseButton {
                button,
                down: false,
            });
        }
    }

    fn finish_remote_session(&mut self) {
        self.release_remote_keys();
        self.release_remote_buttons();
        self.send_input(InputEvent::MouseLeave);
    }

    fn restore_to_local(&mut self) {
        let was_remote = self.pointer.is_remote();
        if was_remote {
            self.finish_remote_session();
        } else {
            self.release_remote_keys();
            self.release_remote_buttons();
        }
        let local_position = self.pointer.force_local();
        if let Some((x, y)) = local_position {
            self.restore_local_cursor_at(x, y);
        } else {
            self.end_remote_cursor_capture();
        }
    }

    fn send_input(&self, event: InputEvent) {
        self.input.send(event);
    }
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        self.restore_to_local();
    }
}

fn mouse_button_from_tap(event: TapEvent) -> Option<(MouseButton, bool)> {
    match event.kind {
        TAP_MOUSE_LEFT_DOWN => Some((MouseButton::Left, true)),
        TAP_MOUSE_LEFT_UP => Some((MouseButton::Left, false)),
        TAP_MOUSE_RIGHT_DOWN => Some((MouseButton::Right, true)),
        TAP_MOUSE_RIGHT_UP => Some((MouseButton::Right, false)),
        TAP_MOUSE_OTHER_DOWN => Some((mac_other_button(event.a), true)),
        TAP_MOUSE_OTHER_UP => Some((mac_other_button(event.a), false)),
        _ => None,
    }
}

fn mac_other_button(button: i64) -> MouseButton {
    match button {
        2 => MouseButton::Middle,
        3 => MouseButton::Extra(4),
        4 => MouseButton::Extra(5),
        value => MouseButton::Extra(value.clamp(4, u8::MAX as i64) as u8),
    }
}

fn screen_size_i32() -> (i32, i32) {
    let (width, height) = crate::input::screen_size();
    (width.max(1) as i32, height.max(1) as i32)
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn mac_scroll_axis_to_remote(value: i64, profile: ConnectionProfile) -> i16 {
    let mapped = if matches!(profile, ConnectionProfile::MacOSToMacOS) {
        value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
    } else {
        value
            .saturating_neg()
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32
    };
    clamp_i16(mapped)
}

#[cfg(test)]
mod tests {
    use super::RemoteCursorLock;

    #[test]
    fn remote_cursor_lock_stays_at_edge_until_session_ends() {
        let mut lock = RemoteCursorLock::default();

        assert_eq!(lock.begin(999, 400, (1000, 800)), (999, 400));
        assert_eq!(lock.position(), Some((999, 400)));
        assert_eq!(lock.position(), Some((999, 400)));

        lock.end();
        assert_eq!(lock.position(), None);
    }

    #[test]
    fn remote_cursor_lock_clamps_anchor_to_local_screen() {
        let mut lock = RemoteCursorLock::default();

        assert_eq!(lock.begin(1000, -1, (1000, 800)), (999, 0));
        assert_eq!(lock.position(), Some((999, 0)));
    }
}
