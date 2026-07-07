use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::ServerConfig;
use crate::clipboard::{Clipboard, ClipboardApi};
use crate::clipboard_sync::{ClipboardSyncState, clipboard_summary};
use crate::file_transfer;
use crate::input::{self, InputSink};
use crate::linux::{self, DisplayServer};
use crate::platform::{ConnectionProfile, Platform};
use crate::pointer::{MotionAction, PointerRouter};
use crate::protocol::{
    self, ClipboardPayload, Frame, FrameKind, InputEvent, KeyState, MouseButton,
};
use crate::transport::SharedWriter;

const DEFAULT_REMOTE_WIDTH: i32 = 1366;
const DEFAULT_REMOTE_HEIGHT: i32 = 768;
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(2);
const INPUT_HOTPLUG_SCAN_INTERVAL: Duration = Duration::from_secs(2);
const INPUT_DEVICE_DIR: &str = "/dev/input";
const INPUT_PROC_DEVICES: &str = "/proc/bus/input/devices";
const DESKBRIDGE_VIRTUAL_INPUT_NAME: &str = "Deskbridge Virtual Input";
const POINTER_START_ENV: &str = "DESKBRIDGE_POINTER_START";

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const SYN_REPORT: u16 = 0x00;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_FORWARD: u16 = 0x115;
const BTN_BACK: u16 = 0x116;
const KEY_KPENTER: u16 = 96;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_HOME: u16 = 102;
const KEY_UP: u16 = 103;
const KEY_PAGEUP: u16 = 104;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_END: u16 = 107;
const KEY_DOWN: u16 = 108;
const KEY_PAGEDOWN: u16 = 109;
const KEY_INSERT: u16 = 110;
const KEY_DELETE: u16 = 111;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;
const KEY_MAX_COMMON: u16 = 255;

const IOC_NRBITS: u64 = 8;
const IOC_TYPEBITS: u64 = 8;
const IOC_SIZEBITS: u64 = 14;
const IOC_NRSHIFT: u64 = 0;
const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;
const IOC_WRITE: u64 = 1;
const EVDEV_IOCTL_BASE: u8 = b'E';
const EVIOCGRAB: libc::c_ulong = iow::<libc::c_int>(EVDEV_IOCTL_BASE, 0x90);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawInputEvent {
    time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

pub fn run(config: ServerConfig) -> std::io::Result<()> {
    let display = DisplayServer::detect();
    eprintln!(
        "linux server backend active ({display}); clipboard sync and evdev input capture are enabled",
        display = display.as_str()
    );
    if linux::is_wsl() {
        eprintln!(
            "warning: WSL/WSLg is not a reliable Linux server target: /dev/input/event* capture is usually unavailable, and LAN clients need Windows portproxy/firewall rules to reach this service"
        );
    }

    let listener = TcpListener::bind(&config.bind).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to listen on {}: {error}", config.bind),
        )
    })?;
    eprintln!("deskbridge linux server listening on {}", config.bind);

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

    let remote_profile = ConnectionProfile::local_server(client_platform);
    eprintln!(
        "connection profile: {} -> {} ({})",
        Platform::local().as_str(),
        client_platform.as_str(),
        remote_profile.as_str()
    );
    eprintln!(
        "remote screen {}x{}, edge {:?}, client {}",
        remote_size.0, remote_size.1, config.edge, addr
    );

    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_stream = stream.try_clone()?;
    let stop_flag = Arc::clone(&stop_requested);
    crate::shutdown::spawn_gui_stop_watcher(move || {
        stop_flag.store(true, Ordering::Release);
        let _ = stop_stream.shutdown(Shutdown::Both);
    });

    let clipboard_state = Arc::new(Mutex::new(ClipboardSyncState::default()));
    spawn_inbound_reader(
        stream.try_clone()?,
        Arc::clone(&clipboard_state),
        Arc::clone(&stop_requested),
    );
    spawn_clipboard_watcher(writer.clone(), clipboard_state, Arc::clone(&stop_requested));

    match LinuxInputCapture::new(config.edge, remote_size, writer) {
        Ok(mut capture) => {
            eprintln!(
                "linux evdev input capture active; move through the configured {:?} edge to control the client",
                config.edge
            );
            capture.run_until_stopped(Arc::clone(&stop_requested));
        }
        Err(error) => {
            eprintln!("linux input capture disabled: {error}");
            eprintln!(
                "grant access to /dev/input/event* and /dev/uinput, then restart Deskbridge; clipboard sync remains active"
            );
            while !stop_requested.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(250));
            }
        }
    }

    Ok(())
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

struct LinuxInputCapture {
    writer: SharedWriter,
    router: PointerRouter,
    devices: Vec<CaptureDevice>,
    known_device_paths: BTreeSet<PathBuf>,
    local_warp: Option<InputSink>,
    pointer_locator: PointerLocator,
    local_layout: LocalScreenLayout,
    remote_buttons_down: usize,
    grabbed: bool,
    last_device_scan: Instant,
}

impl LinuxInputCapture {
    fn new(edge: super::Edge, remote_size: (i32, i32), writer: SharedWriter) -> io::Result<Self> {
        let display = DisplayServer::detect();
        let local_layout = LocalScreenLayout::detect(display);
        let local_size = local_layout.size();
        let devices = discover_input_devices()?;
        let known_device_paths = devices.iter().map(|device| device.path.clone()).collect();
        let pointer_locator = PointerLocator::new(display);
        let local_warp = match InputSink::new(ConnectionProfile::LinuxToLinux) {
            Ok(input) => Some(input),
            Err(error) => {
                eprintln!("local pointer reposition disabled: {error}");
                None
            }
        };
        let mut router = PointerRouter::new(edge, local_size, remote_size);
        let start = pointer_locator
            .global_position()
            .and_then(|pos| Some(local_layout.normalize(pos)))
            .or_else(pointer_start_from_env)
            .unwrap_or((local_size.0 / 2, local_size.1 / 2));
        router.calibrate_local_position(start.0, start.1);
        eprintln!(
            "linux input capture devices: {} event device(s), local layout {}x{} at +{}+{}, start {}x{}",
            devices.len(),
            local_size.0,
            local_size.1,
            local_layout.origin_x,
            local_layout.origin_y,
            start.0,
            start.1
        );
        Ok(Self {
            writer,
            router,
            devices,
            known_device_paths,
            local_warp,
            pointer_locator,
            local_layout,
            remote_buttons_down: 0,
            grabbed: false,
            last_device_scan: Instant::now(),
        })
    }

    fn run_until_stopped(&mut self, stop_requested: Arc<AtomicBool>) {
        while !stop_requested.load(Ordering::Acquire) {
            self.refresh_devices_if_needed();
            for index in 0..self.devices.len() {
                while let Some(event) = self.devices[index].read_event() {
                    if let Err(error) = self.handle_event(index, event) {
                        eprintln!("linux input capture event failed: {error}");
                    }
                }
            }
            thread::sleep(INPUT_POLL_INTERVAL);
        }
        let _ = self.set_grabbed(false);
        let _ = self.send_input(InputEvent::MouseLeave);
    }

    fn refresh_devices_if_needed(&mut self) {
        if self.grabbed || self.last_device_scan.elapsed() < INPUT_HOTPLUG_SCAN_INTERVAL {
            return;
        }
        self.last_device_scan = Instant::now();
        let Ok(next_devices) = discover_input_devices() else {
            return;
        };
        let next_paths: BTreeSet<_> = next_devices
            .iter()
            .map(|device| device.path.clone())
            .collect();
        if next_paths != self.known_device_paths {
            eprintln!(
                "linux input hotplug: refreshed capture devices {} -> {}",
                self.devices.len(),
                next_devices.len()
            );
            self.devices = next_devices;
            self.known_device_paths = next_paths;
        }
    }

    fn handle_event(&mut self, index: usize, event: RawInputEvent) -> io::Result<()> {
        match event.event_type {
            EV_REL => {
                self.devices[index].pending_rel(event.code, event.value);
                Ok(())
            }
            EV_SYN if event.code == SYN_REPORT => {
                let pending = self.devices[index].take_pending();
                self.handle_pending_pointer(pending)
            }
            EV_KEY if is_mouse_button(event.code) => {
                self.handle_mouse_button(mouse_button_from_code(event.code), event.value != 0)
            }
            EV_KEY => self.handle_key(event.code, key_state_from_value(event.value)),
            _ => Ok(()),
        }
    }

    fn handle_pending_pointer(&mut self, pending: PendingPointer) -> io::Result<()> {
        if pending.dx != 0 || pending.dy != 0 {
            if self.router.is_remote() {
                match self.router.observe_remote_motion(
                    pending.dx,
                    pending.dy,
                    self.remote_buttons_down == 0,
                ) {
                    MotionAction::MoveRemote { dx, dy } => {
                        self.send_input(InputEvent::MouseDelta { dx, dy })?;
                    }
                    MotionAction::ReturnLocal { x, y } => {
                        self.send_input(InputEvent::MouseLeave)?;
                        self.set_grabbed(false)?;
                        self.remote_buttons_down = 0;
                        self.warp_local_pointer(x, y);
                    }
                    MotionAction::Local | MotionAction::EnterRemote { .. } => {}
                }
            } else {
                let action = self
                    .pointer_locator
                    .global_position()
                    .map(|pos| self.local_layout.normalize(pos))
                    .map(|(x, y)| self.router.observe_local_motion(x, y))
                    .unwrap_or_else(|| self.router.observe_local_delta(pending.dx, pending.dy));
                if let MotionAction::EnterRemote { x, y } = action {
                    if let Err(error) = self.set_grabbed(true) {
                        eprintln!("failed to grab local input devices: {error}");
                        let _ = self.router.force_local();
                        return Ok(());
                    }
                    self.remote_buttons_down = 0;
                    self.send_input(InputEvent::MouseEnter { x, y })?;
                }
            }
        }

        if self.router.is_remote() && (pending.hwheel != 0 || pending.wheel != 0) {
            self.send_input(InputEvent::MouseWheel {
                horizontal: clamp_i16(pending.hwheel.saturating_mul(120)),
                vertical: clamp_i16(pending.wheel.saturating_mul(120)),
            })?;
        }
        Ok(())
    }

    fn handle_mouse_button(&mut self, button: MouseButton, down: bool) -> io::Result<()> {
        if self.router.is_remote() {
            if down {
                self.remote_buttons_down = self.remote_buttons_down.saturating_add(1);
            } else {
                self.remote_buttons_down = self.remote_buttons_down.saturating_sub(1);
            }
            self.send_input(InputEvent::MouseButton { button, down })
        } else {
            self.router.observe_local_button(down);
            Ok(())
        }
    }

    fn handle_key(&mut self, linux_key: u16, state: KeyState) -> io::Result<()> {
        if !self.router.is_remote() {
            return Ok(());
        }
        let Some(scancode) = linux_key_to_protocol_scancode(linux_key) else {
            eprintln!("linux server capture: unsupported key code {linux_key}");
            return Ok(());
        };
        self.send_input(InputEvent::Key { scancode, state })
    }

    fn set_grabbed(&mut self, grabbed: bool) -> io::Result<()> {
        if self.grabbed == grabbed {
            return Ok(());
        }
        for device in &self.devices {
            device.grab(grabbed)?;
        }
        self.grabbed = grabbed;
        Ok(())
    }

    fn send_input(&self, event: InputEvent) -> io::Result<()> {
        self.writer
            .write(Frame::new(FrameKind::Input, protocol::encode_input(&event)))
    }

    fn warp_local_pointer(&mut self, x: i32, y: i32) {
        if let Some(input) = self.local_warp.as_mut() {
            if let Err(error) = input.apply(InputEvent::MouseEnter { x, y }) {
                eprintln!("failed to reposition local pointer: {error}");
                return;
            }
            let _ = input.apply(InputEvent::MouseLeave);
        }
    }
}

impl Drop for LinuxInputCapture {
    fn drop(&mut self) {
        let _ = self.set_grabbed(false);
    }
}

struct CaptureDevice {
    path: PathBuf,
    file: File,
    pending: PendingPointer,
}

#[derive(Default, Clone, Copy)]
struct PendingPointer {
    dx: i32,
    dy: i32,
    wheel: i32,
    hwheel: i32,
}

impl CaptureDevice {
    fn new(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(&path)?;
        set_nonblocking(&file)?;
        Ok(Self {
            path,
            file,
            pending: PendingPointer::default(),
        })
    }

    fn read_event(&mut self) -> Option<RawInputEvent> {
        let mut event: RawInputEvent = unsafe { std::mem::zeroed() };
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                &mut event as *mut RawInputEvent as *mut u8,
                std::mem::size_of::<RawInputEvent>(),
            )
        };
        match self.file.read_exact(bytes) {
            Ok(()) => Some(event),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::UnexpectedEof
                ) =>
            {
                None
            }
            Err(error) => {
                eprintln!("failed to read {}: {error}", self.path.display());
                None
            }
        }
    }

    fn pending_rel(&mut self, code: u16, value: i32) {
        match code {
            REL_X => self.pending.dx = self.pending.dx.saturating_add(value),
            REL_Y => self.pending.dy = self.pending.dy.saturating_add(value),
            REL_WHEEL => self.pending.wheel = self.pending.wheel.saturating_add(value),
            REL_HWHEEL => self.pending.hwheel = self.pending.hwheel.saturating_add(value),
            _ => {}
        }
    }

    fn take_pending(&mut self) -> PendingPointer {
        std::mem::take(&mut self.pending)
    }

    fn grab(&self, grabbed: bool) -> io::Result<()> {
        let flag: libc::c_int = if grabbed { 1 } else { 0 };
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB, flag) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn discover_input_devices() -> io::Result<Vec<CaptureDevice>> {
    let infos = parse_proc_input_devices();
    let mut devices = Vec::new();
    let mut errors = Vec::new();
    for info in infos {
        if info.name.contains(DESKBRIDGE_VIRTUAL_INPUT_NAME) || !info.capture_kind.is_capture() {
            continue;
        }
        let path = PathBuf::from(INPUT_DEVICE_DIR).join(&info.event);
        match CaptureDevice::new(path.clone()) {
            Ok(device) => {
                eprintln!(
                    "linux capture opened {} ({}, {})",
                    path.display(),
                    info.name,
                    info.capture_kind.as_str()
                );
                devices.push(device);
            }
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    if devices.is_empty() {
        let details = if errors.is_empty() {
            "no keyboard or pointer event devices found".to_string()
        } else {
            errors.join("; ")
        };
        return Err(linux::unsupported(format!(
            "Linux server input capture requires readable /dev/input/event* devices ({details})"
        )));
    }
    Ok(devices)
}

struct InputDeviceInfo {
    event: String,
    name: String,
    capture_kind: CaptureKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureKind {
    Keyboard,
    Pointer,
    Touchpad,
    Ignore,
}

impl CaptureKind {
    fn is_capture(self) -> bool {
        !matches!(self, Self::Ignore)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Pointer => "pointer",
            Self::Touchpad => "touchpad",
            Self::Ignore => "ignored",
        }
    }
}

fn parse_proc_input_devices() -> Vec<InputDeviceInfo> {
    let Ok(text) = fs::read_to_string(INPUT_PROC_DEVICES) else {
        return Vec::new();
    };
    text.split("\n\n").filter_map(parse_input_block).collect()
}

fn parse_input_block(block: &str) -> Option<InputDeviceInfo> {
    let mut name = String::from("unknown");
    let mut handlers = String::new();
    let mut ev_bits = String::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("N: Name=") {
            name = value.trim().trim_matches('"').to_string();
        } else if let Some(value) = line.strip_prefix("H: Handlers=") {
            handlers = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("B: EV=") {
            ev_bits = value.trim().to_string();
        }
    }
    let event = handlers
        .split_whitespace()
        .find(|handler| handler.starts_with("event"))?
        .to_string();
    let lower_name = name.to_ascii_lowercase();
    let handler_tokens: Vec<_> = handlers.split_whitespace().collect();
    let has_event_key = evdev_bit_is_set(&ev_bits, EV_KEY as usize);
    let has_event_rel = evdev_bit_is_set(&ev_bits, EV_REL as usize);
    let capture_kind = if lower_name.contains("power")
        || lower_name.contains("sleep")
        || lower_name.contains("lid")
        || lower_name.contains("video bus")
        || lower_name.contains("hdmi")
    {
        CaptureKind::Ignore
    } else if lower_name.contains("touchpad") || lower_name.contains("trackpad") {
        CaptureKind::Touchpad
    } else if handler_tokens
        .iter()
        .any(|handler| handler.starts_with("mouse"))
        || lower_name.contains("mouse")
        || lower_name.contains("trackpoint")
        || (has_event_rel && !handler_tokens.iter().any(|handler| *handler == "js"))
    {
        CaptureKind::Pointer
    } else if handler_tokens.iter().any(|handler| *handler == "kbd")
        || lower_name.contains("keyboard")
        || (has_event_key && !has_event_rel)
    {
        CaptureKind::Keyboard
    } else {
        CaptureKind::Ignore
    };
    Some(InputDeviceInfo {
        event,
        name,
        capture_kind,
    })
}

fn evdev_bit_is_set(hex: &str, bit: usize) -> bool {
    let mut value = 0u128;
    for chunk in hex.split_whitespace() {
        let Ok(parsed) = u128::from_str_radix(chunk, 16) else {
            continue;
        };
        value = (value << 64) | parsed;
    }
    bit < 128 && ((value >> bit) & 1) == 1
}

#[derive(Clone, Debug)]
struct LocalScreenLayout {
    origin_x: i32,
    origin_y: i32,
    width: i32,
    height: i32,
}

impl LocalScreenLayout {
    fn detect(display: DisplayServer) -> Self {
        let layout = screen_layout_from_env()
            .or_else(|| match display {
                DisplayServer::Wayland => wlr_randr_layout().or_else(xrandr_layout),
                DisplayServer::X11 => xrandr_layout().or_else(wlr_randr_layout),
                DisplayServer::Unknown => xrandr_layout().or_else(wlr_randr_layout),
            })
            .unwrap_or_else(|| {
                let size = input::screen_size();
                Self::new(0, 0, size.0 as i32, size.1 as i32)
            });
        eprintln!(
            "linux local monitor layout: {}x{} at +{}+{}",
            layout.width, layout.height, layout.origin_x, layout.origin_y
        );
        layout
    }

    fn new(origin_x: i32, origin_y: i32, width: i32, height: i32) -> Self {
        Self {
            origin_x,
            origin_y,
            width: width.max(1),
            height: height.max(1),
        }
    }

    fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    fn normalize(&self, point: (i32, i32)) -> (i32, i32) {
        (
            point
                .0
                .saturating_sub(self.origin_x)
                .clamp(0, self.width - 1),
            point
                .1
                .saturating_sub(self.origin_y)
                .clamp(0, self.height - 1),
        )
    }
}

fn screen_layout_from_env() -> Option<LocalScreenLayout> {
    let value = std::env::var("DESKBRIDGE_SCREEN_LAYOUT").ok()?;
    parse_layout_token(&value)
}

fn pointer_start_from_env() -> Option<(i32, i32)> {
    let value = std::env::var(POINTER_START_ENV).ok()?;
    let (x, y) = value.split_once([',', 'x', 'X'])?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

fn xrandr_layout() -> Option<LocalScreenLayout> {
    let output = Command::new("xrandr").arg("--listmonitors").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for token in text.split_whitespace() {
        if let Some((x, y, right, bottom)) = parse_monitor_geometry(token) {
            bounds = Some(match bounds {
                Some((min_x, min_y, max_x, max_y)) => (
                    min_x.min(x),
                    min_y.min(y),
                    max_x.max(right),
                    max_y.max(bottom),
                ),
                None => (x, y, right, bottom),
            });
        }
    }
    bounds.map(|(min_x, min_y, max_x, max_y)| {
        LocalScreenLayout::new(min_x, min_y, max_x - min_x, max_y - min_y)
    })
}

fn wlr_randr_layout() -> Option<LocalScreenLayout> {
    let output = Command::new("wlr-randr").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    let mut current_size: Option<(i32, i32)> = None;
    for line in text.lines() {
        for token in line.split_whitespace() {
            if let Some((width, height)) = parse_size_pair(token) {
                current_size = Some((width, height));
            } else if token.starts_with('+') {
                if let (Some((width, height)), Some((x, y))) =
                    (current_size, parse_position_token(token))
                {
                    bounds = Some(match bounds {
                        Some((min_x, min_y, max_x, max_y)) => (
                            min_x.min(x),
                            min_y.min(y),
                            max_x.max(x + width),
                            max_y.max(y + height),
                        ),
                        None => (x, y, x + width, y + height),
                    });
                }
            }
        }
    }
    bounds.map(|(min_x, min_y, max_x, max_y)| {
        LocalScreenLayout::new(min_x, min_y, max_x - min_x, max_y - min_y)
    })
}

fn parse_layout_token(token: &str) -> Option<LocalScreenLayout> {
    let (size, pos) = token.split_once('@').or_else(|| token.split_once('+'))?;
    let (width, height) = parse_size_pair(size)?;
    let (x, y) = if token.contains('@') {
        parse_position_token(pos)?
    } else {
        let plus_pos = format!("+{pos}");
        parse_position_token(&plus_pos)?
    };
    Some(LocalScreenLayout::new(x, y, width, height))
}

fn parse_monitor_geometry(token: &str) -> Option<(i32, i32, i32, i32)> {
    let token = token.trim_start_matches(['+', '*']);
    let (width, rest) = token.split_once('x')?;
    let width = width.split('/').next()?.parse::<i32>().ok()?;
    let mut parts = rest.split('+');
    let height = parts.next()?.split('/').next()?.parse::<i32>().ok()?;
    let x = parts.next()?.parse::<i32>().ok()?;
    let y = parts.next()?.parse::<i32>().ok()?;
    Some((x, y, x + width.max(1), y + height.max(1)))
}

fn parse_size_pair(token: &str) -> Option<(i32, i32)> {
    let token = token.trim_matches(|ch: char| !ch.is_ascii_digit() && ch != 'x' && ch != 'X');
    let (width, height) = token.split_once(['x', 'X'])?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn parse_position_token(token: &str) -> Option<(i32, i32)> {
    let token = token.trim();
    let rest = token.strip_prefix('+')?;
    let (x, y) = rest.split_once('+')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

struct PointerLocator {
    x11: Option<X11PointerLocator>,
}

impl PointerLocator {
    fn new(display: DisplayServer) -> Self {
        let x11 = match display {
            DisplayServer::X11 => X11PointerLocator::open(),
            DisplayServer::Wayland | DisplayServer::Unknown
                if std::env::var_os("DISPLAY").is_some() =>
            {
                X11PointerLocator::open()
            }
            _ => None,
        };
        if x11.is_some() {
            eprintln!("linux pointer locator: Xlib global pointer query enabled");
        } else {
            eprintln!(
                "linux pointer locator: using evdev relative calibration; set {POINTER_START_ENV}=x,y if the initial pointer position is wrong"
            );
        }
        Self { x11 }
    }

    fn global_position(&self) -> Option<(i32, i32)> {
        self.x11.as_ref()?.query_pointer()
    }
}

struct X11PointerLocator {
    display: *mut libc::c_void,
    root: libc::c_ulong,
}

unsafe impl Send for X11PointerLocator {}
unsafe impl Sync for X11PointerLocator {}

impl X11PointerLocator {
    fn open() -> Option<Self> {
        let display = unsafe { XOpenDisplay(std::ptr::null()) };
        if display.is_null() {
            return None;
        }
        let root = unsafe { XDefaultRootWindow(display) };
        Some(Self { display, root })
    }

    fn query_pointer(&self) -> Option<(i32, i32)> {
        let mut root_return = 0;
        let mut child_return = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut win_x = 0;
        let mut win_y = 0;
        let mut mask = 0;
        let ok = unsafe {
            XQueryPointer(
                self.display,
                self.root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        (ok != 0).then_some((root_x, root_y))
    }
}

impl Drop for X11PointerLocator {
    fn drop(&mut self) {
        if !self.display.is_null() {
            unsafe { XCloseDisplay(self.display) };
        }
    }
}

#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(display_name: *const libc::c_char) -> *mut libc::c_void;
    fn XDefaultRootWindow(display: *mut libc::c_void) -> libc::c_ulong;
    fn XQueryPointer(
        display: *mut libc::c_void,
        window: libc::c_ulong,
        root_return: *mut libc::c_ulong,
        child_return: *mut libc::c_ulong,
        root_x_return: *mut libc::c_int,
        root_y_return: *mut libc::c_int,
        win_x_return: *mut libc::c_int,
        win_y_return: *mut libc::c_int,
        mask_return: *mut libc::c_uint,
    ) -> libc::c_int;
    fn XCloseDisplay(display: *mut libc::c_void) -> libc::c_int;
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn is_mouse_button(code: u16) -> bool {
    (BTN_LEFT..=BTN_BACK).contains(&code)
}

fn mouse_button_from_code(code: u16) -> MouseButton {
    match code {
        BTN_LEFT => MouseButton::Left,
        BTN_MIDDLE => MouseButton::Middle,
        BTN_RIGHT => MouseButton::Right,
        BTN_BACK => MouseButton::Extra(4),
        BTN_FORWARD => MouseButton::Extra(5),
        BTN_SIDE => MouseButton::Extra(6),
        BTN_EXTRA => MouseButton::Extra(7),
        _ => MouseButton::Extra(8),
    }
}

fn key_state_from_value(value: i32) -> KeyState {
    match value {
        0 => KeyState::Up,
        2 => KeyState::Repeat,
        _ => KeyState::Down,
    }
}

fn linux_key_to_protocol_scancode(code: u16) -> Option<u16> {
    Some(match code {
        KEY_KPENTER => 284,
        KEY_RIGHTCTRL => 285,
        KEY_RIGHTALT => 312,
        KEY_HOME => 327,
        KEY_UP => 328,
        KEY_PAGEUP => 329,
        KEY_LEFT => 331,
        KEY_RIGHT => 333,
        KEY_END => 335,
        KEY_DOWN => 336,
        KEY_PAGEDOWN => 337,
        KEY_INSERT => 338,
        KEY_DELETE => 339,
        KEY_LEFTMETA => 347,
        KEY_RIGHTMETA => 348,
        code @ 1..=KEY_MAX_COMMON => code,
        _ => return None,
    })
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

const fn ioc(dir: u64, kind: u8, nr: u8, size: u64) -> libc::c_ulong {
    ((dir << IOC_DIRSHIFT)
        | ((kind as u64) << IOC_TYPESHIFT)
        | ((nr as u64) << IOC_NRSHIFT)
        | (size << IOC_SIZESHIFT)) as libc::c_ulong
}

const fn iow<T>(kind: u8, nr: u8) -> libc::c_ulong {
    ioc(IOC_WRITE, kind, nr, std::mem::size_of::<T>() as u64)
}

fn spawn_inbound_reader(
    mut stream: TcpStream,
    clipboard_state: Arc<Mutex<ClipboardSyncState>>,
    stop_requested: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(e) => {
                eprintln!("clipboard receiver disabled: {e}");
                return;
            }
        };
        let receive_root = std::env::temp_dir().join("deskbridge-received");
        let mut incoming_files = file_transfer::IncomingBundle::new(receive_root);

        loop {
            let frame = match protocol::read_frame(&mut stream) {
                Ok(frame) => frame,
                Err(e) if stop_requested.load(Ordering::Acquire) => {
                    eprintln!("linux server stopped: {e}");
                    return;
                }
                Err(e) => {
                    eprintln!("connection closed: {e}");
                    stop_requested.store(true, Ordering::Release);
                    return;
                }
            };
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
    });
}

fn spawn_clipboard_watcher(
    writer: SharedWriter,
    clipboard_state: Arc<Mutex<ClipboardSyncState>>,
    stop_requested: Arc<AtomicBool>,
) {
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
            if stop_requested.load(Ordering::Acquire) {
                return;
            }
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

fn note_remote_clipboard(state: &Arc<Mutex<ClipboardSyncState>>, payload: &ClipboardPayload) {
    state.lock().unwrap().note_remote_write(payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_linux_extended_keys_to_protocol_scancodes() {
        assert_eq!(linux_key_to_protocol_scancode(KEY_UP), Some(328));
        assert_eq!(linux_key_to_protocol_scancode(KEY_LEFT), Some(331));
        assert_eq!(linux_key_to_protocol_scancode(KEY_RIGHT), Some(333));
        assert_eq!(linux_key_to_protocol_scancode(KEY_DOWN), Some(336));
        assert_eq!(linux_key_to_protocol_scancode(KEY_LEFTMETA), Some(347));
    }
}
