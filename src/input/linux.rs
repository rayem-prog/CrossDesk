use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::mem::{size_of, zeroed};
use std::os::fd::AsRawFd;
use std::thread;
use std::time::Duration;

use crate::linux::{self, DisplayServer};
use crate::platform::ConnectionProfile;
use crate::protocol::{InputEvent, KeyState, MouseButton};

const UINPUT_PATH: &str = "/dev/uinput";
const UINPUT_DEVICE_NAME: &str = "Deskbridge Virtual Input";

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0x00;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_FORWARD: u16 = 0x115;
const BTN_BACK: u16 = 0x116;

const KEY_ESC: u16 = 1;
const KEY_MAX_COMMON: u16 = 255;
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

const BUS_USB: u16 = 0x03;
const UINPUT_MAX_NAME_SIZE: usize = 80;
const VIRTUAL_VENDOR_ID: u16 = 0x4453;
const VIRTUAL_PRODUCT_ID: u16 = 0x4247;
const VIRTUAL_VERSION: u16 = 0x0100;

const IOC_NRBITS: u64 = 8;
const IOC_TYPEBITS: u64 = 8;
const IOC_SIZEBITS: u64 = 14;

const IOC_NRSHIFT: u64 = 0;
const IOC_TYPESHIFT: u64 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u64 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u64 = IOC_SIZESHIFT + IOC_SIZEBITS;

const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;
const UINPUT_IOCTL_BASE: u8 = b'U';

const UI_DEV_CREATE: libc::c_ulong = ioc(IOC_NONE, UINPUT_IOCTL_BASE, 1, 0);
const UI_DEV_DESTROY: libc::c_ulong = ioc(IOC_NONE, UINPUT_IOCTL_BASE, 2, 0);
const UI_DEV_SETUP: libc::c_ulong = iow::<UinputSetup>(UINPUT_IOCTL_BASE, 3);
const UI_ABS_SETUP: libc::c_ulong = iow::<UinputAbsSetup>(UINPUT_IOCTL_BASE, 4);
const UI_SET_EVBIT: libc::c_ulong = iow::<libc::c_int>(UINPUT_IOCTL_BASE, 100);
const UI_SET_KEYBIT: libc::c_ulong = iow::<libc::c_int>(UINPUT_IOCTL_BASE, 101);
const UI_SET_RELBIT: libc::c_ulong = iow::<libc::c_int>(UINPUT_IOCTL_BASE, 102);
const UI_SET_ABSBIT: libc::c_ulong = iow::<libc::c_int>(UINPUT_IOCTL_BASE, 103);
const UI_SET_PROPBIT: libc::c_ulong = iow::<libc::c_int>(UINPUT_IOCTL_BASE, 110);

const INPUT_PROP_POINTER: u16 = 0x00;

pub struct InputSink {
    backend: NativeUinput,
    screen_size: (u32, u32),
    mouse_position: (i32, i32),
    pressed_keys: HashSet<u16>,
    pressed_buttons: HashSet<MouseButton>,
    remote_active: bool,
}

impl InputSink {
    pub fn new(_profile: ConnectionProfile) -> io::Result<Self> {
        let display = DisplayServer::detect();
        if linux::is_wsl() && !linux::allow_wsl_input_override() {
            return Err(linux::unsupported(
                "WSL/WSLg does not route Linux /dev/uinput events into the visible desktop cursor, so Deskbridge Linux client input injection will connect but cannot move the pointer. Use a real Linux desktop/VM, or set DESKBRIDGE_ALLOW_WSL_UINPUT=1 only for experimental testing.",
            ));
        }
        if linux::is_wsl() {
            eprintln!(
                "warning: running native-uinput in WSL/WSLg by explicit override; pointer movement may not be visible"
            );
        }
        let screen_size = detect_screen_size(display);
        let backend = NativeUinput::new(screen_size).map_err(|error| {
            if matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound) {
                linux::unsupported(format!(
                    "native Linux input injection requires write access to {UINPUT_PATH}; run `sudo modprobe uinput` and `sh scripts/setup_linux_uinput.sh`, then log out and back in ({error})"
                ))
            } else {
                error
            }
        })?;

        eprintln!(
            "linux input backend: native-uinput ({display}), screen {}x{}",
            screen_size.0,
            screen_size.1,
            display = display.as_str()
        );

        Ok(Self {
            backend,
            screen_size,
            mouse_position: (0, 0),
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
            remote_active: false,
        })
    }

    pub fn apply(&mut self, event: InputEvent) -> io::Result<()> {
        match event {
            InputEvent::MouseEnter { x, y } => self.mouse_enter(x, y),
            InputEvent::MouseLeave => {
                self.mouse_leave()?;
                Ok(())
            }
            _ if !self.remote_active => Ok(()),
            InputEvent::MouseDelta { dx, dy } => self.mouse_delta(dx, dy),
            InputEvent::MouseButton { button, down } => self.mouse_button(button, down),
            InputEvent::MouseWheel {
                horizontal,
                vertical,
            } => self.mouse_wheel(horizontal, vertical),
            InputEvent::Key { scancode, state } => self.key(scancode, state),
        }
    }

    pub fn screen_size(&self) -> (u32, u32) {
        self.screen_size
    }

    fn mouse_enter(&mut self, x: i32, y: i32) -> io::Result<()> {
        if self.remote_active {
            self.release_remote_input()?;
        }
        self.remote_active = true;
        self.mouse_position = clamp_point(x, y, self.screen_size);
        self.backend
            .move_abs(self.mouse_position.0, self.mouse_position.1)
    }

    fn mouse_delta(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        let next_x = self.mouse_position.0.saturating_add(dx);
        let next_y = self.mouse_position.1.saturating_add(dy);
        self.mouse_position = clamp_point(next_x, next_y, self.screen_size);
        self.backend.move_rel(dx, dy)
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> io::Result<()> {
        self.backend.button(button, down)?;
        if down {
            self.pressed_buttons.insert(button);
        } else {
            self.pressed_buttons.remove(&button);
        }
        Ok(())
    }

    fn mouse_wheel(&mut self, horizontal: i16, vertical: i16) -> io::Result<()> {
        self.backend
            .scroll(wheel_steps(horizontal), wheel_steps(vertical))
    }

    fn key(&mut self, scancode: u16, state: KeyState) -> io::Result<()> {
        let Some(keycode) = scancode_to_linux_key(scancode) else {
            eprintln!("linux native-uinput: unsupported scancode {scancode}");
            return Ok(());
        };
        match state {
            KeyState::Down => {
                if self.pressed_keys.insert(keycode) {
                    self.backend.key(keycode, 1)?;
                }
            }
            KeyState::Up => {
                self.pressed_keys.remove(&keycode);
                self.backend.key(keycode, 0)?;
            }
            KeyState::Repeat => {
                self.backend.key(keycode, 2)?;
            }
        }
        Ok(())
    }

    fn mouse_leave(&mut self) -> io::Result<()> {
        self.release_remote_input()?;
        self.remote_active = false;
        Ok(())
    }

    fn release_remote_input(&mut self) -> io::Result<()> {
        for keycode in self.pressed_keys.drain().collect::<Vec<_>>() {
            let _ = self.backend.key(keycode, 0);
        }
        for button in self.pressed_buttons.drain().collect::<Vec<_>>() {
            let _ = self.backend.button(button, false);
        }
        Ok(())
    }
}

impl Drop for InputSink {
    fn drop(&mut self) {
        let _ = self.release_remote_input();
    }
}

pub fn screen_size() -> (u32, u32) {
    detect_screen_size(DisplayServer::detect())
}

struct NativeUinput {
    file: File,
    created: bool,
}

impl NativeUinput {
    fn new(screen_size: (u32, u32)) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).open(UINPUT_PATH)?;
        let mut device = Self {
            file,
            created: false,
        };
        device.enable_capabilities()?;
        device.setup_device(screen_size)?;
        device.create()?;
        Ok(device)
    }

    fn enable_capabilities(&self) -> io::Result<()> {
        self.ioctl_int(UI_SET_EVBIT, EV_KEY)?;
        self.ioctl_int(UI_SET_EVBIT, EV_REL)?;
        self.ioctl_int(UI_SET_EVBIT, EV_ABS)?;
        self.ioctl_int(UI_SET_PROPBIT, INPUT_PROP_POINTER)?;

        for keycode in KEY_ESC..=KEY_MAX_COMMON {
            self.ioctl_int(UI_SET_KEYBIT, keycode)?;
        }
        for button in [
            BTN_LEFT,
            BTN_RIGHT,
            BTN_MIDDLE,
            BTN_SIDE,
            BTN_EXTRA,
            BTN_BACK,
            BTN_FORWARD,
        ] {
            self.ioctl_int(UI_SET_KEYBIT, button)?;
        }
        for rel in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
            self.ioctl_int(UI_SET_RELBIT, rel)?;
        }
        for abs in [ABS_X, ABS_Y] {
            self.ioctl_int(UI_SET_ABSBIT, abs)?;
        }
        Ok(())
    }

    fn setup_device(&self, screen_size: (u32, u32)) -> io::Result<()> {
        let mut setup: UinputSetup = unsafe { zeroed() };
        setup.id.bustype = BUS_USB;
        setup.id.vendor = VIRTUAL_VENDOR_ID;
        setup.id.product = VIRTUAL_PRODUCT_ID;
        setup.id.version = VIRTUAL_VERSION;
        write_c_name(&mut setup.name, UINPUT_DEVICE_NAME);
        self.ioctl_ref(UI_DEV_SETUP, &setup)?;

        self.setup_abs(ABS_X, 0, screen_size.0.saturating_sub(1).max(1) as i32)?;
        self.setup_abs(ABS_Y, 0, screen_size.1.saturating_sub(1).max(1) as i32)
    }

    fn setup_abs(&self, code: u16, minimum: i32, maximum: i32) -> io::Result<()> {
        let abs_setup = UinputAbsSetup {
            code,
            absinfo: InputAbsInfo {
                value: 0,
                minimum,
                maximum,
                fuzz: 0,
                flat: 0,
                resolution: 1,
            },
        };
        self.ioctl_ref(UI_ABS_SETUP, &abs_setup)
    }

    fn create(&mut self) -> io::Result<()> {
        self.ioctl_none(UI_DEV_CREATE)?;
        self.created = true;
        thread::sleep(Duration::from_millis(80));
        Ok(())
    }

    fn move_abs(&mut self, x: i32, y: i32) -> io::Result<()> {
        self.emit(EV_ABS, ABS_X, x)?;
        self.emit(EV_ABS, ABS_Y, y)?;
        self.sync()
    }

    fn move_rel(&mut self, dx: i32, dy: i32) -> io::Result<()> {
        if dx != 0 {
            self.emit(EV_REL, REL_X, dx)?;
        }
        if dy != 0 {
            self.emit(EV_REL, REL_Y, dy)?;
        }
        self.sync()
    }

    fn button(&mut self, button: MouseButton, down: bool) -> io::Result<()> {
        self.emit(EV_KEY, mouse_button_code(button), if down { 1 } else { 0 })?;
        self.sync()
    }

    fn scroll(&mut self, horizontal: i32, vertical: i32) -> io::Result<()> {
        if vertical != 0 {
            self.emit(EV_REL, REL_WHEEL, vertical)?;
        }
        if horizontal != 0 {
            self.emit(EV_REL, REL_HWHEEL, horizontal)?;
        }
        self.sync()
    }

    fn key(&mut self, keycode: u16, value: i32) -> io::Result<()> {
        self.emit(EV_KEY, keycode, value)?;
        self.sync()
    }

    fn emit(&mut self, event_type: u16, code: u16, value: i32) -> io::Result<()> {
        let event = InputEventRaw {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            event_type,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &event as *const InputEventRaw as *const u8,
                size_of::<InputEventRaw>(),
            )
        };
        self.file.write_all(bytes)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.emit(EV_SYN, SYN_REPORT, 0)
    }

    fn ioctl_none(&self, request: libc::c_ulong) -> io::Result<()> {
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), request) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn ioctl_int(&self, request: libc::c_ulong, value: u16) -> io::Result<()> {
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), request, value as libc::c_int) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn ioctl_ref<T>(&self, request: libc::c_ulong, value: &T) -> io::Result<()> {
        let result = unsafe { libc::ioctl(self.file.as_raw_fd(), request, value as *const T) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for NativeUinput {
    fn drop(&mut self) {
        if self.created {
            let _ = self.ioctl_none(UI_DEV_DESTROY);
        }
    }
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [libc::c_char; UINPUT_MAX_NAME_SIZE],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsInfo,
}

#[repr(C)]
struct InputEventRaw {
    time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

const fn ioc(dir: u64, kind: u8, nr: u8, size: u64) -> libc::c_ulong {
    ((dir << IOC_DIRSHIFT)
        | ((kind as u64) << IOC_TYPESHIFT)
        | ((nr as u64) << IOC_NRSHIFT)
        | (size << IOC_SIZESHIFT)) as libc::c_ulong
}

const fn iow<T>(kind: u8, nr: u8) -> libc::c_ulong {
    ioc(IOC_WRITE, kind, nr, size_of::<T>() as u64)
}

fn write_c_name(target: &mut [libc::c_char; UINPUT_MAX_NAME_SIZE], name: &str) {
    for (index, byte) in name.bytes().take(UINPUT_MAX_NAME_SIZE - 1).enumerate() {
        target[index] = byte as libc::c_char;
    }
}

const MOUSE_EXTRA_BACK_BUTTON: u8 = 4;
const MOUSE_EXTRA_FORWARD_BUTTON: u8 = 5;
const MOUSE_EXTRA_SIDE_BUTTON: u8 = 6;
const EXTENDED_SCANCODE_OFFSET: u16 = 256;
const SCANCODE_KEYPAD_ENTER: u16 = 284;
const SCANCODE_RIGHT_CTRL: u16 = 285;
const SCANCODE_RIGHT_ALT: u16 = 312;
const SCANCODE_HOME: u16 = 327;
const SCANCODE_ARROW_UP: u16 = 328;
const SCANCODE_PAGE_UP: u16 = 329;
const SCANCODE_ARROW_LEFT: u16 = 331;
const SCANCODE_ARROW_RIGHT: u16 = 333;
const SCANCODE_END: u16 = 335;
const SCANCODE_ARROW_DOWN: u16 = 336;
const SCANCODE_PAGE_DOWN: u16 = 337;
const SCANCODE_INSERT: u16 = 338;
const SCANCODE_DELETE: u16 = 339;
const SCANCODE_LEFT_META: u16 = 347;

fn mouse_button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => BTN_LEFT,
        MouseButton::Middle => BTN_MIDDLE,
        MouseButton::Right => BTN_RIGHT,
        MouseButton::Extra(MOUSE_EXTRA_BACK_BUTTON) => BTN_BACK,
        MouseButton::Extra(MOUSE_EXTRA_FORWARD_BUTTON) => BTN_FORWARD,
        MouseButton::Extra(MOUSE_EXTRA_SIDE_BUTTON) => BTN_SIDE,
        MouseButton::Extra(_) => BTN_EXTRA,
    }
}

fn scancode_to_linux_key(scancode: u16) -> Option<u16> {
    // Most Linux evdev key codes align with the low PC scancode range. Extended
    // keys arrive with a 256 offset and are normalized back into that range.
    Some(match scancode {
        SCANCODE_KEYPAD_ENTER => KEY_KPENTER,
        SCANCODE_RIGHT_CTRL => KEY_RIGHTCTRL,
        SCANCODE_RIGHT_ALT => KEY_RIGHTALT,
        SCANCODE_HOME => KEY_HOME,
        SCANCODE_ARROW_UP => KEY_UP,
        SCANCODE_PAGE_UP => KEY_PAGEUP,
        SCANCODE_ARROW_LEFT => KEY_LEFT,
        SCANCODE_ARROW_RIGHT => KEY_RIGHT,
        SCANCODE_END => KEY_END,
        SCANCODE_ARROW_DOWN => KEY_DOWN,
        SCANCODE_PAGE_DOWN => KEY_PAGEDOWN,
        SCANCODE_INSERT => KEY_INSERT,
        SCANCODE_DELETE => KEY_DELETE,
        SCANCODE_LEFT_META => KEY_LEFTMETA,
        code @ KEY_ESC..=KEY_MAX_COMMON => code,
        code if code >= EXTENDED_SCANCODE_OFFSET
            && code - EXTENDED_SCANCODE_OFFSET <= KEY_MAX_COMMON =>
        {
            code - EXTENDED_SCANCODE_OFFSET
        }
        _ => return None,
    })
}

fn wheel_steps(value: i16) -> i32 {
    let value = value as i32;
    if value == 0 {
        return 0;
    }
    let sign = value.signum();
    let magnitude = value.unsigned_abs().max(1);
    sign * (magnitude.div_ceil(120).max(1) as i32)
}

fn clamp_point(x: i32, y: i32, screen_size: (u32, u32)) -> (i32, i32) {
    let max_x = screen_size.0.saturating_sub(1).max(1) as i32;
    let max_y = screen_size.1.saturating_sub(1).max(1) as i32;
    (x.clamp(0, max_x), y.clamp(0, max_y))
}

fn detect_screen_size(display: DisplayServer) -> (u32, u32) {
    if let Some(size) = screen_size_from_env() {
        eprintln!("linux screen size override: {}x{}", size.0, size.1);
        return size;
    }

    let detected = match display {
        DisplayServer::Wayland => wayland_screen_size().or_else(x11_screen_size),
        DisplayServer::X11 => x11_screen_size().or_else(wayland_screen_size),
        DisplayServer::Unknown => wayland_screen_size().or_else(x11_screen_size),
    };
    detected.unwrap_or_else(|| {
        let fallback = fallback_screen_size();
        eprintln!(
            "linux screen size detection failed; using fallback {}x{}. Set DESKBRIDGE_SCREEN_SIZE=WIDTHxHEIGHT to override.",
            fallback.0, fallback.1
        );
        fallback
    })
}

fn screen_size_from_env() -> Option<(u32, u32)> {
    std::env::var("DESKBRIDGE_SCREEN_SIZE")
        .ok()
        .or_else(|| std::env::var("DESKBRIDGE_LINUX_SCREEN_SIZE").ok())
        .and_then(|value| parse_size_token(&value))
}

fn x11_screen_size() -> Option<(u32, u32)> {
    xrandr_current_size()
        .or_else(xrandr_monitor_size)
        .or_else(xdpyinfo_screen_size)
}

fn xrandr_current_size() -> Option<(u32, u32)> {
    if !linux::command_exists("xrandr") {
        return None;
    }
    let output = linux::run_output("xrandr", ["--current"]).ok()?;
    parse_xrandr_current(&String::from_utf8_lossy(&output))
}

fn xrandr_monitor_size() -> Option<(u32, u32)> {
    if !linux::command_exists("xrandr") {
        return None;
    }
    let output = linux::run_output("xrandr", ["--listmonitors"]).ok()?;
    parse_xrandr_monitor_list(&String::from_utf8_lossy(&output))
}

fn xdpyinfo_screen_size() -> Option<(u32, u32)> {
    if !linux::command_exists("xdpyinfo") {
        return None;
    }
    let output = linux::run_output("xdpyinfo", std::iter::empty::<&str>()).ok()?;
    parse_xdpyinfo_dimensions(&String::from_utf8_lossy(&output))
}

fn wayland_screen_size() -> Option<(u32, u32)> {
    wlr_randr_screen_size().or_else(wayland_xwayland_screen_size)
}

fn wlr_randr_screen_size() -> Option<(u32, u32)> {
    if !linux::command_exists("wlr-randr") {
        return None;
    }
    let output = linux::run_output("wlr-randr", std::iter::empty::<&str>()).ok()?;
    parse_wlr_randr(&String::from_utf8_lossy(&output))
}

fn wayland_xwayland_screen_size() -> Option<(u32, u32)> {
    if std::env::var_os("DISPLAY").is_some() {
        x11_screen_size()
    } else {
        None
    }
}

fn parse_xrandr_current(text: &str) -> Option<(u32, u32)> {
    for line in text.lines() {
        let Some((_, after_current)) = line.split_once("current") else {
            continue;
        };
        let tokens = clean_tokens(after_current);
        for window in tokens.windows(3) {
            if window[1] == "x" {
                if let (Ok(width), Ok(height)) =
                    (window[0].parse::<u32>(), window[2].parse::<u32>())
                {
                    return Some(valid_screen_size(width, height));
                }
            }
        }
    }
    None
}

fn parse_xrandr_monitor_list(text: &str) -> Option<(u32, u32)> {
    let mut max_right = 0u32;
    let mut max_bottom = 0u32;
    for line in text.lines().filter(|line| line.contains(':')) {
        for token in line.split_whitespace() {
            if let Some((width, height, x, y)) = parse_xrandr_monitor_geometry(token) {
                max_right = max_right.max(x.saturating_add(width));
                max_bottom = max_bottom.max(y.saturating_add(height));
            }
        }
    }
    if max_right != 0 && max_bottom != 0 {
        Some(valid_screen_size(max_right, max_bottom))
    } else {
        None
    }
}

fn parse_xrandr_monitor_geometry(token: &str) -> Option<(u32, u32, u32, u32)> {
    let token = token.trim_start_matches(['+', '*']);
    let (width_part, rest) = token.split_once('x')?;
    let width = width_part.split('/').next()?.parse::<u32>().ok()?;
    let mut parts = rest.split('+');
    let height = parts.next()?.split('/').next()?.parse::<u32>().ok()?;
    let x = parts.next()?.parse::<u32>().ok()?;
    let y = parts.next()?.parse::<u32>().ok()?;
    Some((width, height, x, y))
}

fn parse_xdpyinfo_dimensions(text: &str) -> Option<(u32, u32)> {
    for line in text.lines() {
        if !line.contains("dimensions:") {
            continue;
        }
        for token in line.split_whitespace() {
            if let Some(size) = parse_size_token(token) {
                return Some(size);
            }
        }
    }
    None
}

fn parse_wlr_randr(text: &str) -> Option<(u32, u32)> {
    let mut max_size: Option<(u32, u32)> = None;
    for line in text.lines() {
        if !line.contains("current") && !line.contains("preferred") {
            continue;
        }
        for token in line.split_whitespace() {
            if let Some(size) = parse_size_token(token) {
                max_size = Some(match max_size {
                    Some((width, height))
                        if width.saturating_mul(height) >= size.0.saturating_mul(size.1) =>
                    {
                        (width, height)
                    }
                    _ => size,
                });
            }
        }
    }
    max_size
}

fn parse_size_token(token: &str) -> Option<(u32, u32)> {
    let token = token
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != 'x' && ch != 'X')
        .trim_end_matches("px");
    let (width, height) = token.split_once(['x', 'X'])?;
    let width = width.parse::<u32>().ok()?;
    let height = height.parse::<u32>().ok()?;
    Some(valid_screen_size(width, height))
}

fn clean_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| ch == ',' || ch == ';')
                .to_string()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn valid_screen_size(width: u32, height: u32) -> (u32, u32) {
    (width.max(1), height.max(1))
}

fn fallback_screen_size() -> (u32, u32) {
    (1920, 1080)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_windows_extended_keys() {
        assert_eq!(scancode_to_linux_key(328), Some(KEY_UP));
        assert_eq!(scancode_to_linux_key(331), Some(KEY_LEFT));
        assert_eq!(scancode_to_linux_key(333), Some(KEY_RIGHT));
        assert_eq!(scancode_to_linux_key(336), Some(KEY_DOWN));
        assert_eq!(scancode_to_linux_key(347), Some(KEY_LEFTMETA));
    }

    #[test]
    fn converts_wheel_pixels_to_detents() {
        assert_eq!(wheel_steps(0), 0);
        assert_eq!(wheel_steps(1), 1);
        assert_eq!(wheel_steps(120), 1);
        assert_eq!(wheel_steps(240), 2);
        assert_eq!(wheel_steps(-120), -1);
    }
}
