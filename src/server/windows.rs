use crate::desktop::{DesktopLayout, MonitorRect};
use crate::secure::SecureStream;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::net::{Shutdown, SocketAddr, TcpListener};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SCROLL,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, ClipCursor, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetCursorPos, GetMessageW, GetSystemMetrics, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_UP, LLMHF_INJECTED, LWA_ALPHA, MSG, MSLLHOOKSTRUCT, PostMessageW, PostQuitMessage,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNA, SetCursorPos, SetLayeredWindowAttributes,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_APP, WM_DROPFILES, WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    WNDCLASSW, WS_EX_ACCEPTFILES, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, XBUTTON1, XBUTTON2,
};

use super::{Edge, ServerConfig};
use crate::clipboard::{Clipboard, ClipboardApi};
use crate::file_transfer;
use crate::platform::{ConnectionProfile, Platform};
use crate::pointer::{MotionAction, PointerRouter};
use crate::protocol::{
    self, ClipboardPayload, Frame, FrameKind, InputEvent, KeyState, MouseButton,
};
use crate::transport::SharedWriter;

const DEFAULT_REMOTE_WIDTH: i32 = 1366;
const DEFAULT_REMOTE_HEIGHT: i32 = 768;
const WHEEL_PIXELS_PER_DETENT: i32 = 120;
const DEFAULT_INPUT_FLUSH_MS: u64 = 2;
const INPUT_BATCH_LIMIT: usize = 64;
const DROP_STRIP_WIDTH: i32 = 18;
const DROP_STRIP_ALPHA: u8 = 48;
const CURSOR_LOCK_RADIUS: i32 = 24;
const REMOTE_CLIPBOARD_SUPPRESS_WINDOW: Duration = Duration::from_millis(1200);
const WM_DESKBRIDGE_STOP: u32 = WM_APP + 0x51;

static CONNECTION_ALIVE: AtomicBool = AtomicBool::new(false);
static STOP_WINDOW: AtomicUsize = AtomicUsize::new(0);

pub fn request_stop() {
    let window = STOP_WINDOW.load(Ordering::Acquire);
    if window != 0 {
        unsafe {
            PostMessageW(window as HWND, WM_DESKBRIDGE_STOP, 0, 0);
        }
    }
}

fn connection_lost() {
    CONNECTION_ALIVE.store(false, Ordering::Release);
    request_stop();
}

static CAPTURE_STATE: OnceLock<Arc<Mutex<CaptureState>>> = OnceLock::new();
static TRANSFER_WRITER: OnceLock<SharedWriter> = OnceLock::new();

pub fn run(config: ServerConfig) -> std::io::Result<()> {
    let key = crate::secure::load_key()?;
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let listener = TcpListener::bind(&config.bind)?;
    listener.set_nonblocking(true)?;
    eprintln!(
        "CrossDesk listening on {}; entry edges: {}",
        config.bind,
        config.edge.as_str()
    );
    eprintln!("Waiting for the Mac with the matching pairing key. Ctrl+C exits.");
    while !crate::shutdown::requested() {
        let (socket, address) = match listener.accept() {
            Ok(peer) => peer,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut stream = match SecureStream::accept(socket, &key) {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("Rejected connection from {address}: {error}");
                continue;
            }
        };
        let writer = SharedWriter::new(stream.try_clone()?);
        let hello = writer.write(Frame::new(FrameKind::Hello, protocol::hello_payload()));
        let peer = hello.and_then(|_| read_client_hello(&mut stream));
        match peer {
            Ok((remote_size, platform)) => {
                if let Err(error) =
                    run_session(&config, stream, address, writer, remote_size, platform)
                {
                    eprintln!("Session ended: {error}");
                }
                if !crate::shutdown::requested() {
                    eprintln!("Windows control restored. Waiting for Mac to reconnect.");
                }
            }
            Err(error) => eprintln!("Mac handshake incomplete: {error}"),
        }
    }
    Ok(())
}

fn run_session(
    config: &ServerConfig,
    stream: SecureStream,
    addr: SocketAddr,
    writer: SharedWriter,
    remote_size: (i32, i32),
    client_platform: Platform,
) -> std::io::Result<()> {
    let stop_stream = stream.try_clone()?;
    let remote_profile = ConnectionProfile::local_server(client_platform);
    eprintln!(
        "connection profile: {} -> {} ({})",
        Platform::local().as_str(),
        client_platform.as_str(),
        remote_profile.as_str()
    );
    let _ = TRANSFER_WRITER.set(writer.clone());
    eprintln!(
        "remote screen {}x{}, edge {:?}, client {}",
        remote_size.0, remote_size.1, config.edge, addr
    );

    let clipboard_state = Arc::new(Mutex::new(ClipboardSyncState::default()));
    let input = InputEmitter::spawn(writer.clone());
    let state = CaptureState::new(input, config.edge, remote_size);
    if let Some(existing) = CAPTURE_STATE.get() {
        *existing.lock().unwrap() = state;
    } else {
        let _ = CAPTURE_STATE.set(Arc::new(Mutex::new(state)));
    }

    let drop_strip = if drop_strip_enabled() {
        Some(DropStrip::create(config.edge)?)
    } else {
        eprintln!("file drop strip disabled; use the GUI drop zone for file drag transfer");
        None
    };
    let raw_input = RawInputWindow::create()?;
    let hooks = Hooks::install()?;
    let stop_window = raw_input.hwnd as usize;
    STOP_WINDOW.store(stop_window, Ordering::Release);
    CONNECTION_ALIVE.store(true, Ordering::Release);
    let reader = spawn_inbound_reader(stream, Arc::clone(&clipboard_state));
    let ping_writer = writer.clone();
    let heartbeat = thread::spawn(move || {
        while CONNECTION_ALIVE.load(Ordering::Acquire) {
            if ping_writer
                .write(Frame::new(FrameKind::Ping, Vec::new()))
                .is_err()
            {
                connection_lost();
                break;
            }
            for _ in 0..10 {
                if !CONNECTION_ALIVE.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    spawn_clipboard_watcher(writer, clipboard_state);
    crate::shutdown::spawn_gui_stop_watcher(move || unsafe {
        PostMessageW(stop_window as HWND, WM_DESKBRIDGE_STOP, 0, 0);
    });
    eprintln!("raw mouse input registered for relative movement");
    eprintln!(
        "input hooks installed; move the mouse through the configured edge to control the client"
    );
    if drop_strip.is_some() {
        eprintln!("file drop strip active on the {:?} edge", config.edge);
    }
    message_loop();
    CONNECTION_ALIVE.store(false, Ordering::Release);
    STOP_WINDOW.store(0, Ordering::Release);
    let _ = stop_stream.shutdown(Shutdown::Both);
    if let Some(state) = CAPTURE_STATE.get() {
        state.lock().unwrap().shutdown();
    } else {
        unlock_cursor();
    }
    drop(hooks);
    drop(raw_input);
    drop(drop_strip);
    let _ = reader.join();
    let _ = heartbeat.join();
    Ok(())
}

fn read_client_hello(stream: &mut SecureStream) -> std::io::Result<((i32, i32), Platform)> {
    let frame = protocol::read_frame(stream)?;
    if frame.kind != FrameKind::Hello {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "client must send Hello before input capture starts",
        ));
    }
    let hello = protocol::decode_hello(&frame.payload)?;
    protocol::validate_version(hello)?;
    let width = hello.screen_width.unwrap_or(0);
    let height = hello.screen_height.unwrap_or(0);
    if !(1..=32768).contains(&width) || !(1..=32768).contains(&height) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid remote screen dimensions",
        ));
    }
    Ok(((width as i32, height as i32), hello.platform))
}

fn spawn_inbound_reader(
    mut stream: SecureStream,
    clipboard_state: Arc<Mutex<ClipboardSyncState>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(e) => {
                eprintln!("clipboard receiver disabled: {e}");
                connection_lost();
                return;
            }
        };
        let receive_root = std::env::temp_dir().join("deskbridge-received");
        let mut incoming_files = file_transfer::IncomingBundle::new(receive_root);

        loop {
            let frame = match protocol::read_frame(&mut stream) {
                Ok(frame) => frame,
                Err(e) => {
                    eprintln!("connection closed: {e}");
                    connection_lost();
                    break;
                }
            };
            if !crate::transport::clipboard_enabled() && frame.kind != FrameKind::Pong {
                eprintln!("Unexpected client frame in input-only mode");
                connection_lost();
                break;
            }
            match frame.kind {
                FrameKind::Clipboard => match protocol::decode_clipboard(&frame.payload) {
                    Ok(payload) => {
                        eprintln!("received clipboard {}", clipboard_summary(&payload));
                        if let Err(e) = clipboard.write(&payload) {
                            eprintln!("clipboard write failed: {e}");
                        } else {
                            note_remote_clipboard(&clipboard_state, &payload);
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
                    if let Err(e) = clipboard.write(&payload) {
                        eprintln!("file clipboard write failed: {e}");
                    } else {
                        note_remote_clipboard(&clipboard_state, &payload);
                    }
                }
                _ => {}
            }
        }
    })
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
        let mut last_sequence = unsafe { GetClipboardSequenceNumber() };
        loop {
            thread::sleep(Duration::from_millis(350));
            let sequence = unsafe { GetClipboardSequenceNumber() };
            if sequence == last_sequence {
                continue;
            }
            last_sequence = sequence;

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

struct Hooks {
    keyboard: HHOOK,
    mouse: HHOOK,
}

impl Hooks {
    fn install() -> std::io::Result<Self> {
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) };
        if keyboard.is_null() {
            return Err(last_os_error("SetWindowsHookExW keyboard failed"));
        }
        let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0) };
        if mouse.is_null() {
            unsafe {
                UnhookWindowsHookEx(keyboard);
            }
            return Err(last_os_error("SetWindowsHookExW mouse failed"));
        }
        Ok(Self { keyboard, mouse })
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        unsafe {
            UnhookWindowsHookEx(self.keyboard);
            UnhookWindowsHookEx(self.mouse);
        }
    }
}

struct RawInputWindow {
    hwnd: HWND,
}

impl RawInputWindow {
    fn create() -> std::io::Result<Self> {
        let class_name = wide_null("DeskbridgeRawInput");
        let title = wide_null("Deskbridge Raw Input");
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let wnd_class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(raw_input_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        unsafe {
            RegisterClassW(&wnd_class);
        }

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                module,
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(last_os_error("CreateWindowExW raw input failed"));
        }

        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        };
        let ok = unsafe {
            RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>().try_into().unwrap())
        };
        if ok == 0 {
            unsafe {
                DestroyWindow(hwnd);
            }
            return Err(last_os_error("RegisterRawInputDevices mouse failed"));
        }
        Ok(Self { hwnd })
    }
}

impl Drop for RawInputWindow {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}

struct DropStrip {
    hwnd: HWND,
}

impl DropStrip {
    fn create(edge: Edge) -> std::io::Result<Self> {
        let class_name = wide_null("DeskbridgeDropStrip");
        let title = wide_null("Deskbridge File Drop");
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let wnd_class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(drop_strip_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        unsafe {
            RegisterClassW(&wnd_class);
        }

        let screen = screen_size();
        let x = match edge {
            Edge::Right | Edge::Both => screen.0.saturating_sub(DROP_STRIP_WIDTH),
            Edge::Left => 0,
        };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_ACCEPTFILES
                    | WS_EX_LAYERED,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                x,
                0,
                DROP_STRIP_WIDTH,
                screen.1,
                ptr::null_mut(),
                ptr::null_mut(),
                module,
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(last_os_error("CreateWindowExW drop strip failed"));
        }
        unsafe {
            SetLayeredWindowAttributes(hwnd, 0, DROP_STRIP_ALPHA, LWA_ALPHA);
            DragAcceptFiles(hwnd, 1);
            ShowWindow(hwnd, SW_SHOWNA);
        }
        Ok(Self { hwnd })
    }
}

impl Drop for DropStrip {
    fn drop(&mut self) {
        unsafe {
            DragAcceptFiles(self.hwnd, 0);
            DestroyWindow(self.hwnd);
        }
    }
}

fn message_loop() {
    let mut msg: MSG = unsafe { zeroed() };
    while unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn drop_strip_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DROPFILES {
        let hdrop = wparam as HDROP;
        let files = dropped_files_from_hdrop(hdrop);
        unsafe {
            DragFinish(hdrop);
        }
        if !files.is_empty() {
            send_dropped_files(files);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

unsafe extern "system" fn raw_input_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESKBRIDGE_STOP {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    if message == WM_INPUT {
        handle_raw_mouse_input(lparam);
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn handle_raw_mouse_input(lparam: LPARAM) {
    let header_size = size_of::<RAWINPUTHEADER>() as u32;
    let mut raw = unsafe { zeroed::<RAWINPUT>() };
    let mut size = size_of::<RAWINPUT>() as u32;
    let read = unsafe {
        GetRawInputData(
            lparam as HRAWINPUT,
            RID_INPUT,
            (&mut raw as *mut RAWINPUT).cast(),
            &mut size,
            header_size,
        )
    };
    if read == u32::MAX || read == 0 {
        return;
    }

    if raw.header.dwType != RIM_TYPEMOUSE {
        return;
    }
    let mouse = unsafe { raw.data.mouse };
    if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
        return;
    }
    if mouse.lLastX == 0 && mouse.lLastY == 0 {
        return;
    }

    if let Some(state) = CAPTURE_STATE.get() {
        state
            .lock()
            .unwrap()
            .handle_raw_mouse_delta(mouse.lLastX, mouse.lLastY);
    }
}

fn dropped_files_from_hdrop(hdrop: HDROP) -> Vec<PathBuf> {
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, ptr::null_mut(), 0) };
    let mut files = Vec::new();
    for i in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, i, ptr::null_mut(), 0) };
        if len == 0 {
            continue;
        }
        let mut buffer = vec![0u16; len as usize + 1];
        let written = unsafe { DragQueryFileW(hdrop, i, buffer.as_mut_ptr(), buffer.len() as u32) };
        if written != 0 {
            files.push(PathBuf::from(String::from_utf16_lossy(
                &buffer[..written as usize],
            )));
        }
    }
    files
}

fn send_dropped_files(files: Vec<PathBuf>) {
    let Some(writer) = TRANSFER_WRITER.get().cloned() else {
        eprintln!("drop ignored: no connected client");
        return;
    };
    eprintln!("sending {} dropped file(s) to client", files.len());
    thread::spawn(move || {
        let result = file_transfer::send_files(&writer, &files)
            .and_then(|_| writer.write(Frame::new(FrameKind::DragEnd, Vec::new())));
        if let Err(e) = result {
            eprintln!("dropped file transfer failed: {e}");
        }
    });
}

struct CaptureState {
    input: InputEmitter,
    pointer: PointerRouter,
    win_size: (i32, i32),
    desktop: DesktopLayout,
    desktop_updated: Instant,
    local_keys: HashSet<u16>,
    pressed_keys: HashSet<u16>,
    pressed_buttons: HashSet<MouseButton>,
}

impl CaptureState {
    fn new(input: InputEmitter, edge: Edge, remote_size: (i32, i32)) -> Self {
        let desktop = desktop_layout();
        let win_size = desktop.size();
        Self {
            input,
            pointer: PointerRouter::new(edge, win_size, remote_size),
            win_size,
            desktop,
            desktop_updated: Instant::now(),
            local_keys: HashSet::new(),
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
        }
    }

    fn handle_keyboard(&mut self, message: u32, hook: &KBDLLHOOKSTRUCT) -> bool {
        if !self.pointer.is_remote() {
            let code = normalized_scancode(hook);
            if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                self.local_keys.insert(code);
            }
            if matches!(message, WM_KEYUP | WM_SYSKEYUP) {
                self.local_keys.remove(&code);
            }
            return false;
        }
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(message, WM_KEYUP | WM_SYSKEYUP) || (hook.flags & LLKHF_UP) != 0;
        if !down && !up {
            return true;
        }
        let emergency_chord = hook.vkCode == VK_ESCAPE as u32
            && unsafe { GetAsyncKeyState(VK_CONTROL as i32) } < 0
            && unsafe { GetAsyncKeyState(VK_MENU as i32) } < 0;
        if down && (hook.vkCode == VK_SCROLL as u32 || emergency_chord) {
            self.deactivate();
            return true;
        }

        let scancode = normalized_scancode(hook);
        let state = if down {
            if self.pressed_keys.insert(scancode) {
                KeyState::Down
            } else {
                KeyState::Repeat
            }
        } else {
            self.pressed_keys.remove(&scancode);
            KeyState::Up
        };
        self.send_input(InputEvent::Key { scancode, state });
        true
    }

    fn handle_mouse(&mut self, message: u32, hook: &MSLLHOOKSTRUCT) -> bool {
        if message == WM_MOUSEMOVE {
            return self.handle_mouse_move(hook);
        }
        if !self.pointer.is_remote() {
            if let Some((_, down)) = mouse_button_transition(message, hook.mouseData) {
                self.pointer.observe_local_button(down);
            }
            return false;
        }

        match mouse_button_transition(message, hook.mouseData) {
            Some((button, down)) => self.send_button(button, down),
            None => match message {
                WM_MOUSEWHEEL => self.send_wheel(0, wheel_pixels(hook.mouseData)),
                WM_MOUSEHWHEEL => self.send_wheel(wheel_pixels(hook.mouseData), 0),
                _ => {}
            },
        }
        true
    }

    fn handle_mouse_move(&mut self, hook: &MSLLHOOKSTRUCT) -> bool {
        if self.pointer.is_remote() {
            if (hook.flags & LLMHF_INJECTED) != 0 {
                return true;
            }
            self.recenter_cursor_if_needed(hook);
            return true;
        }

        if (hook.flags & LLMHF_INJECTED) != 0 {
            return false;
        }
        if self.desktop_updated.elapsed() >= Duration::from_secs(1) {
            self.desktop = desktop_layout();
            self.win_size = self.desktop.size();
            self.pointer.update_local_size(self.win_size);
            self.desktop_updated = Instant::now();
        }
        let (local_x, local_y) = self.desktop.local_point(hook.pt.x, hook.pt.y);
        if !self.local_keys.is_empty() {
            self.pointer.calibrate_local_position(local_x, local_y);
            return false;
        }
        if let MotionAction::EnterRemote { x, y } =
            self.pointer.observe_local_motion(local_x, local_y)
        {
            self.activate(x, y);
            true
        } else {
            false
        }
    }

    fn handle_raw_mouse_delta(&mut self, dx: i32, dy: i32) {
        if !self.pointer.is_remote() {
            // Windows clamps pixel coordinates at an edge. Raw motion still
            // reports an outward push, including after releasing a held key.
            if self.local_keys.is_empty() {
                let mut point = POINT { x: 0, y: 0 };
                if unsafe { GetCursorPos(&mut point) } != 0 {
                    let (x, y) = self.desktop.local_point(point.x, point.y);
                    if (x == 0 && dx < 0) || (x == self.win_size.0 - 1 && dx > 0) {
                        self.pointer.calibrate_local_position(x, y);
                        if let MotionAction::EnterRemote { x, y } =
                            self.pointer.observe_local_delta(dx, 0)
                        {
                            self.activate(x, y);
                        }
                    }
                }
            }
            return;
        }
        match self
            .pointer
            .observe_remote_motion(dx, dy, self.pressed_buttons.is_empty())
        {
            MotionAction::MoveRemote { dx, dy } => {
                self.send_input(InputEvent::MouseDelta { dx, dy });
            }
            MotionAction::ReturnLocal { x, y } => self.finish_remote_session(x, y),
            MotionAction::Local | MotionAction::EnterRemote { .. } => {}
        }
    }

    fn activate(&mut self, remote_x: i32, remote_y: i32) {
        self.send_input(InputEvent::MouseEnter {
            x: remote_x,
            y: remote_y,
        });
        self.lock_cursor_to_anchor();
        eprintln!(
            "entered client control at {},{}; press Scroll Lock to release",
            remote_x, remote_y
        );
    }

    fn deactivate(&mut self) {
        let Some((x, y)) = self.pointer.force_local() else {
            return;
        };
        self.finish_remote_session(x, y);
    }

    fn finish_remote_session(&mut self, x: i32, y: i32) {
        self.local_keys.extend(self.pressed_keys.iter().copied());
        for scancode in self.pressed_keys.drain().collect::<Vec<_>>() {
            self.send_input(InputEvent::Key {
                scancode,
                state: KeyState::Up,
            });
        }
        for button in self.pressed_buttons.drain().collect::<Vec<_>>() {
            self.input.send(InputEvent::MouseButton {
                button,
                down: false,
            });
        }
        self.send_input(InputEvent::MouseLeave);
        unlock_cursor();
        let (x, y) = self.desktop.return_point(x, y);
        unsafe {
            SetCursorPos(x, y);
        }
        eprintln!("released control back to Windows");
    }

    fn anchor(&self) -> (i32, i32) {
        unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN).max(1) / 2,
                GetSystemMetrics(SM_CYSCREEN).max(1) / 2,
            )
        }
    }

    fn lock_cursor_to_anchor(&self) {
        let anchor = self.anchor();
        let rect = RECT {
            left: anchor.0 - CURSOR_LOCK_RADIUS,
            top: anchor.1 - CURSOR_LOCK_RADIUS,
            right: anchor.0 + CURSOR_LOCK_RADIUS + 1,
            bottom: anchor.1 + CURSOR_LOCK_RADIUS + 1,
        };
        unsafe {
            SetCursorPos(anchor.0, anchor.1);
            ClipCursor(&rect);
        }
    }

    fn recenter_cursor_if_needed(&self, hook: &MSLLHOOKSTRUCT) {
        let anchor = self.anchor();
        if (hook.pt.x - anchor.0).abs() > CURSOR_LOCK_RADIUS
            || (hook.pt.y - anchor.1).abs() > CURSOR_LOCK_RADIUS
        {
            unsafe {
                SetCursorPos(anchor.0, anchor.1);
            }
        }
    }

    fn send_button(&mut self, button: MouseButton, down: bool) {
        if down {
            self.pressed_buttons.insert(button);
        } else {
            self.pressed_buttons.remove(&button);
        }
        self.send_input(InputEvent::MouseButton { button, down });
    }

    fn shutdown(&mut self) {
        if self.pointer.is_remote() {
            self.deactivate();
        } else {
            unlock_cursor();
        }
        self.input.stop();
    }

    fn send_wheel(&self, horizontal: i16, vertical: i16) {
        self.send_input(InputEvent::MouseWheel {
            horizontal,
            vertical,
        });
    }

    fn send_input(&self, event: InputEvent) {
        self.input.send(event);
    }
}

struct InputEmitter {
    sender: Option<Sender<InputEvent>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl InputEmitter {
    fn spawn(writer: SharedWriter) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || input_writer_loop(writer, receiver));
        Self {
            sender: Some(sender),
            worker: Some(worker),
        }
    }

    fn send(&self, event: InputEvent) {
        if let Some(sender) = &self.sender {
            if let Err(e) = sender.send(event) {
                eprintln!("input queue closed: {e}");
            }
        }
    }
    fn stop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for InputEmitter {
    fn drop(&mut self) {
        self.stop();
    }
}

fn input_writer_loop(writer: SharedWriter, receiver: Receiver<InputEvent>) {
    let mut pending_delta = (0i32, 0i32);
    let mut pending_wheel = (0i32, 0i32);
    let mut pending_since = None;
    let mut log = InputSendLog::new();

    loop {
        let event = match recv_input_event(&receiver, pending_since) {
            Ok(Some(event)) => event,
            Ok(None) => {
                flush_pending_input(
                    &writer,
                    &mut pending_delta,
                    &mut pending_wheel,
                    &mut pending_since,
                    &mut log,
                );
                continue;
            }
            Err(()) => break,
        };

        match event {
            event @ (InputEvent::MouseDelta { .. } | InputEvent::MouseWheel { .. }) => {
                queue_pending_input(
                    event,
                    &mut pending_delta,
                    &mut pending_wheel,
                    &mut pending_since,
                );
                drain_queued_input(
                    &writer,
                    &receiver,
                    &mut pending_delta,
                    &mut pending_wheel,
                    &mut pending_since,
                    &mut log,
                );
            }
            event => {
                flush_pending_input(
                    &writer,
                    &mut pending_delta,
                    &mut pending_wheel,
                    &mut pending_since,
                    &mut log,
                );
                write_input_event(&writer, event, &mut log);
            }
        }
    }
}

fn recv_input_event(
    receiver: &Receiver<InputEvent>,
    pending_since: Option<Instant>,
) -> Result<Option<InputEvent>, ()> {
    match pending_since {
        Some(_) => match receiver.recv_timeout(input_flush_timeout(pending_since)) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(()),
        },
        None => receiver.recv().map(Some).map_err(|_| ()),
    }
}

fn queue_pending_input(
    event: InputEvent,
    pending_delta: &mut (i32, i32),
    pending_wheel: &mut (i32, i32),
    pending_since: &mut Option<Instant>,
) {
    if pending_since.is_none() {
        *pending_since = Some(Instant::now());
    }
    match event {
        InputEvent::MouseDelta { dx, dy } => {
            pending_delta.0 = pending_delta.0.saturating_add(dx);
            pending_delta.1 = pending_delta.1.saturating_add(dy);
        }
        InputEvent::MouseWheel {
            horizontal,
            vertical,
        } => {
            pending_wheel.0 = pending_wheel.0.saturating_add(horizontal as i32);
            pending_wheel.1 = pending_wheel.1.saturating_add(vertical as i32);
        }
        _ => {}
    }
}

fn drain_queued_input(
    writer: &SharedWriter,
    receiver: &Receiver<InputEvent>,
    pending_delta: &mut (i32, i32),
    pending_wheel: &mut (i32, i32),
    pending_since: &mut Option<Instant>,
    log: &mut InputSendLog,
) {
    for _ in 0..INPUT_BATCH_LIMIT {
        match receiver.try_recv() {
            Ok(event @ (InputEvent::MouseDelta { .. } | InputEvent::MouseWheel { .. })) => {
                queue_pending_input(event, pending_delta, pending_wheel, pending_since);
            }
            Ok(event) => {
                flush_pending_input(writer, pending_delta, pending_wheel, pending_since, log);
                write_input_event(writer, event, log);
                return;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return,
        }
    }

    if pending_ready(*pending_since) {
        flush_pending_input(writer, pending_delta, pending_wheel, pending_since, log);
    }
}

fn input_flush_timeout(pending_since: Option<Instant>) -> Duration {
    pending_since
        .map(|since| {
            input_flush_interval()
                .checked_sub(since.elapsed())
                .unwrap_or(Duration::ZERO)
        })
        .unwrap_or_else(input_flush_interval)
}

fn pending_ready(pending_since: Option<Instant>) -> bool {
    pending_since
        .map(|since| since.elapsed() >= input_flush_interval())
        .unwrap_or(false)
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
    pending_delta: &mut (i32, i32),
    pending_wheel: &mut (i32, i32),
    pending_since: &mut Option<Instant>,
    log: &mut InputSendLog,
) {
    if pending_delta.0 != 0 || pending_delta.1 != 0 {
        write_input_event(
            writer,
            InputEvent::MouseDelta {
                dx: pending_delta.0,
                dy: pending_delta.1,
            },
            log,
        );
        *pending_delta = (0, 0);
    }
    if pending_wheel.0 != 0 || pending_wheel.1 != 0 {
        write_input_event(
            writer,
            InputEvent::MouseWheel {
                horizontal: clamp(pending_wheel.0, i16::MIN as i32, i16::MAX as i32) as i16,
                vertical: clamp(pending_wheel.1, i16::MIN as i32, i16::MAX as i32) as i16,
            },
            log,
        );
        *pending_wheel = (0, 0);
    }
    *pending_since = None;
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
    if let Err(e) = writer.write(Frame::new(FrameKind::Input, encoded)) {
        eprintln!("input send failed after {} sent event(s): {e}", log.count);
        connection_lost();
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

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(state) = CAPTURE_STATE.get() {
            let hook = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            if state.lock().unwrap().handle_keyboard(wparam as u32, hook) {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(state) = CAPTURE_STATE.get() {
            let hook = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
            if state.lock().unwrap().handle_mouse(wparam as u32, hook) {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

fn normalized_scancode(hook: &KBDLLHOOKSTRUCT) -> u16 {
    let base = hook.scanCode as u16;
    if (hook.flags & LLKHF_EXTENDED) != 0 {
        base + 256
    } else {
        base
    }
}

fn xbutton(mouse_data: u32) -> MouseButton {
    match ((mouse_data >> 16) & 0xffff) as u16 {
        XBUTTON1 => MouseButton::Extra(4),
        XBUTTON2 => MouseButton::Extra(5),
        value => MouseButton::Extra(value as u8),
    }
}

fn mouse_button_transition(message: u32, mouse_data: u32) -> Option<(MouseButton, bool)> {
    match message {
        WM_LBUTTONDOWN => Some((MouseButton::Left, true)),
        WM_LBUTTONUP => Some((MouseButton::Left, false)),
        WM_RBUTTONDOWN => Some((MouseButton::Right, true)),
        WM_RBUTTONUP => Some((MouseButton::Right, false)),
        WM_MBUTTONDOWN => Some((MouseButton::Middle, true)),
        WM_MBUTTONUP => Some((MouseButton::Middle, false)),
        WM_XBUTTONDOWN => Some((xbutton(mouse_data), true)),
        WM_XBUTTONUP => Some((xbutton(mouse_data), false)),
        _ => None,
    }
}

fn wheel_pixels(mouse_data: u32) -> i16 {
    let delta = ((mouse_data >> 16) & 0xffff) as u16 as i16 as i32;
    clamp(
        delta * WHEEL_PIXELS_PER_DETENT / 120,
        i16::MIN as i32,
        i16::MAX as i32,
    ) as i16
}

fn screen_size() -> (i32, i32) {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1);
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1);
    (width, height)
}

fn unlock_cursor() {
    unsafe {
        ClipCursor(ptr::null());
    }
}

fn drop_strip_enabled() -> bool {
    std::env::var("DESKBRIDGE_DROP_STRIP")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn wide_null(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn clamp(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max)
}

fn last_os_error(context: &str) -> std::io::Error {
    let error = std::io::Error::last_os_error();
    std::io::Error::new(error.kind(), format!("{context}: {error}"))
}

fn desktop_layout() -> DesktopLayout {
    unsafe extern "system" fn collect(
        _monitor: HMONITOR,
        _dc: HDC,
        rect: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        if !rect.is_null() {
            let r = unsafe { *rect };
            let monitors = unsafe { &mut *(data as *mut Vec<MonitorRect>) };
            monitors.push(MonitorRect {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            });
        }
        1
    }
    let mut monitors = Vec::<MonitorRect>::new();
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(collect),
            (&mut monitors as *mut Vec<MonitorRect>) as LPARAM,
        );
    }
    if monitors.is_empty() {
        let (width, height) = screen_size();
        monitors.push(MonitorRect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        });
    }
    DesktopLayout::new(monitors)
}

#[cfg(test)]
mod control_tests {
    use super::*;
    #[test]
    fn native_hook_registration_and_hidden_window_cleanup_work() {
        // No capture state is installed in this test: callbacks pass all user
        // input through, and the cursor is never moved or constrained.
        let window = RawInputWindow::create().unwrap();
        let hooks = Hooks::install().unwrap();
        drop(hooks);
        drop(window);
    }
}
