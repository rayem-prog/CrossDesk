use std::collections::HashSet;
use std::env;
use std::io::ErrorKind;
use std::ops::{BitOr, BitOrAssign};
use std::os::raw::c_uchar;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use core_foundation::base::{Boolean, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};

use crate::platform::ConnectionProfile;
use crate::protocol::{InputEvent, KeyState, MouseButton};

#[repr(C)]
struct DeskbridgeHidContext {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn deskbridge_hid_context_create(
        context: *mut *mut DeskbridgeHidContext,
        keyboard_count: *mut usize,
        mouse_count: *mut usize,
    ) -> i32;
    fn deskbridge_hid_context_destroy(context: *mut DeskbridgeHidContext);
    fn deskbridge_hid_post_key(
        context: *mut DeskbridgeHidContext,
        keycode: u16,
        down: bool,
        flags: u64,
        autorepeat: bool,
    ) -> i32;
    fn deskbridge_hid_post_mouse(
        context: *mut DeskbridgeHidContext,
        kind: u8,
        button: u8,
        x: f64,
        y: f64,
        click_count: i64,
        dx: i32,
        dy: i32,
    ) -> i32;
    fn deskbridge_hid_post_scroll(
        context: *mut DeskbridgeHidContext,
        horizontal: i32,
        vertical: i32,
    ) -> i32;
    fn deskbridge_hid_cycle_keyboard_input_source(context: *mut DeskbridgeHidContext) -> i32;
    fn deskbridge_main_display_size(width: *mut u32, height: *mut u32);
    fn deskbridge_macos_hide_cursor() -> i32;
    fn deskbridge_macos_show_cursor() -> i32;
}

const COMMAND_KEYCODE: u16 = 55;
const RIGHT_COMMAND_KEYCODE: u16 = 54;
const CONTROL_KEYCODE: u16 = 59;
const OPTION_KEYCODE: u16 = 58;
const RIGHT_OPTION_KEYCODE: u16 = 61;
const LEFT_SHIFT_KEYCODE: u16 = 56;
const RIGHT_SHIFT_KEYCODE: u16 = 60;
const CAPS_LOCK_KEYCODE: u16 = 57;
const BACKSPACE_KEYCODE: u16 = 51;
const SPACE_KEYCODE: u16 = 49;
const NUMBER_4_KEYCODE: u16 = 21;
const PRINT_SCREEN_SCANCODE: u16 = 311;
const CAPS_LOCK_SCANCODE: u16 = 58;
const LEFT_SHIFT_SCANCODE: u16 = 42;
const RIGHT_SHIFT_SCANCODE: u16 = 54;
const BACKSPACE_INITIAL_REPEAT_INTERVAL: Duration = Duration::from_millis(60);
const BACKSPACE_ACCEL_REPEAT_INTERVAL: Duration = Duration::from_millis(45);
const BACKSPACE_ACCEL_DELAY: Duration = Duration::from_secs(3);
const INPUT_SOURCE_TOGGLE_DEBOUNCE: Duration = Duration::from_millis(180);
const DEFAULT_SCROLL_FRAME_MS: u64 = 8;
const DEFAULT_SCROLL_SCALE: f64 = 1.25;
const DEFAULT_SCROLL_RESPONSE: f64 = 0.34;
const DEFAULT_SCROLL_MAX_STEP: f64 = 96.0;
const SCROLL_ACCEL_WINDOW: Duration = Duration::from_millis(85);
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_DISTANCE: f64 = 5.0;
const NATIVE_MOUSE_MOVED: u8 = 1;
const NATIVE_MOUSE_LEFT_DOWN: u8 = 2;
const NATIVE_MOUSE_LEFT_UP: u8 = 3;
const NATIVE_MOUSE_RIGHT_DOWN: u8 = 4;
const NATIVE_MOUSE_RIGHT_UP: u8 = 5;
const NATIVE_MOUSE_OTHER_DOWN: u8 = 6;
const NATIVE_MOUSE_OTHER_UP: u8 = 7;
const NATIVE_MOUSE_LEFT_DRAGGED: u8 = 8;
const NATIVE_MOUSE_RIGHT_DRAGGED: u8 = 9;
const NATIVE_MOUSE_OTHER_DRAGGED: u8 = 10;
const NATIVE_BUTTON_LEFT: u8 = 0;
const NATIVE_BUTTON_RIGHT: u8 = 1;
const NATIVE_BUTTON_CENTER: u8 = 2;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct EventFlags(u64);

impl EventFlags {
    const ALPHA_SHIFT: Self = Self(0x0001_0000);
    const SHIFT: Self = Self(0x0002_0000);
    const CONTROL: Self = Self(0x0004_0000);
    const ALTERNATE: Self = Self(0x0008_0000);
    const COMMAND: Self = Self(0x0010_0000);

    fn empty() -> Self {
        Self(0)
    }

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for EventFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for EventFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

struct NativeInput {
    context: NonNull<DeskbridgeHidContext>,
    keyboard_count: usize,
    mouse_count: usize,
    hid_ready: bool,
}

impl NativeInput {
    fn new() -> std::io::Result<Self> {
        let mut context = std::ptr::null_mut();
        let mut keyboard_count = 0usize;
        let mut mouse_count = 0usize;
        let status = unsafe {
            deskbridge_hid_context_create(&mut context, &mut keyboard_count, &mut mouse_count)
        };
        let context = NonNull::new(context)
            .ok_or_else(|| event_err("failed to create macOS native HID context"))?;
        if status != 0 && status != 2 {
            unsafe {
                deskbridge_hid_context_destroy(context.as_ptr());
            }
            return Err(event_err("failed to initialize macOS native input"));
        }
        Ok(Self {
            context,
            keyboard_count,
            mouse_count,
            hid_ready: status == 0,
        })
    }

    fn post_key(
        &self,
        keycode: u16,
        down: bool,
        flags: EventFlags,
        autorepeat: bool,
    ) -> std::io::Result<()> {
        check_native_status(
            unsafe {
                deskbridge_hid_post_key(self.context.as_ptr(), keycode, down, flags.0, autorepeat)
            },
            "failed to post key event",
        )
    }

    fn post_mouse(
        &self,
        kind: u8,
        button: u8,
        position: Point,
        click_count: i64,
        dx: i32,
        dy: i32,
    ) -> std::io::Result<()> {
        check_native_status(
            unsafe {
                deskbridge_hid_post_mouse(
                    self.context.as_ptr(),
                    kind,
                    button,
                    position.x,
                    position.y,
                    click_count,
                    dx,
                    dy,
                )
            },
            "failed to post mouse event",
        )
    }

    fn post_scroll(&self, horizontal: i32, vertical: i32) -> std::io::Result<()> {
        check_native_status(
            unsafe { deskbridge_hid_post_scroll(self.context.as_ptr(), horizontal, vertical) },
            "failed to post scroll event",
        )
    }

    fn cycle_keyboard_input_source(&self) -> std::io::Result<()> {
        check_native_status(
            unsafe { deskbridge_hid_cycle_keyboard_input_source(self.context.as_ptr()) },
            "failed to switch macOS input source",
        )
    }
}

impl Drop for NativeInput {
    fn drop(&mut self) {
        unsafe {
            deskbridge_hid_context_destroy(self.context.as_ptr());
        }
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> c_uchar;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

pub struct InputSink {
    native: NativeInput,
    pressed_keys: HashSet<u16>,
    mouse_buttons: MouseButtons,
    screen_size: (i32, i32),
    mouse_position: Point,
    click_tracker: ClickTracker,
    scroll: SmoothScroller,
    shift_tap_candidate: Option<u16>,
    caps_lock_active: bool,
    last_input_source_toggle: Option<Instant>,
    last_backspace_repeat: Option<Instant>,
    backspace_down_since: Option<Instant>,
    remote_active: bool,
    cursor_hidden: bool,
    profile: ConnectionProfile,
    windows_source_compat: bool,
}

impl InputSink {
    pub fn new(profile: ConnectionProfile) -> std::io::Result<Self> {
        eprintln!("macOS input profile: {}", profile.as_str());
        let windows_source_compat = matches!(profile, ConnectionProfile::WindowsToMacOS);
        let native = NativeInput::new()?;
        if native.hid_ready {
            eprintln!(
                "macOS IOHIDManager ready ({} keyboard device(s), {} mouse device(s))",
                native.keyboard_count, native.mouse_count
            );
        } else {
            eprintln!(
                "warning: macOS IOHIDManager device snapshot unavailable; using CGEvent posting only"
            );
        }
        if !accessibility_trusted() {
            eprintln!(
                "warning: macOS Accessibility permission is not granted for this process; keyboard and mouse buttons may be ignored"
            );
            request_accessibility_permission();
        }
        if !accessibility_trusted() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Enable CrossDesk (or its launching Terminal) in System Settings > Privacy & Security > Accessibility, then relaunch.",
            ));
        }
        let cursor_hidden = false;
        Ok(Self {
            native,
            pressed_keys: HashSet::new(),
            mouse_buttons: MouseButtons::default(),
            screen_size: screen_size_i32(),
            mouse_position: Point::new(0.0, 0.0),
            click_tracker: ClickTracker::default(),
            scroll: SmoothScroller::spawn(),
            shift_tap_candidate: None,
            caps_lock_active: false,
            last_input_source_toggle: None,
            last_backspace_repeat: None,
            backspace_down_since: None,
            remote_active: false,
            cursor_hidden,
            profile,
            windows_source_compat,
        })
    }

    pub fn apply(&mut self, event: InputEvent) -> std::io::Result<()> {
        match event {
            InputEvent::MouseEnter { x, y } => self.mouse_enter(x, y),
            InputEvent::MouseLeave => self.mouse_leave(),
            InputEvent::ScreenSync { .. } => Ok(()),
            _ if !self.remote_active => Ok(()),
            InputEvent::Key { scancode, state } => self.key(scancode, state),
            InputEvent::MouseDelta { dx, dy } => self.mouse_delta(dx, dy),
            InputEvent::MouseButton { button, down } => self.mouse_button(button, down),
            InputEvent::MouseWheel {
                horizontal,
                vertical,
            } => self.mouse_wheel(horizontal, vertical),
        }
    }

    pub fn screen_size(&self) -> (u32, u32) {
        (self.screen_size.0 as u32, self.screen_size.1 as u32)
    }

    fn key(&mut self, scancode: u16, state: KeyState) -> std::io::Result<()> {
        if self.windows_source_compat && scancode == PRINT_SCREEN_SCANCODE {
            if state == KeyState::Down {
                self.screenshot_hotkey()?;
            }
            return Ok(());
        }
        if self.windows_source_compat && scancode == CAPS_LOCK_SCANCODE {
            if state == KeyState::Down {
                self.toggle_caps_lock()?;
            }
            return Ok(());
        }

        if self.windows_source_compat && self.handle_shift_tap(scancode, state)? {
            return Ok(());
        }
        if matches!(state, KeyState::Down | KeyState::Repeat) {
            self.flush_pending_shift_modifier()?;
        }

        let Some(keycode) = scancode_to_macos_key_for_profile(scancode, self.profile) else {
            return Ok(());
        };
        match state {
            KeyState::Down => {
                if keycode == BACKSPACE_KEYCODE {
                    let now = Instant::now();
                    self.last_backspace_repeat = Some(now);
                    self.backspace_down_since = Some(now);
                }
                if self.pressed_keys.insert(keycode) {
                    self.post_key(keycode, true)?;
                }
            }
            KeyState::Up => {
                if keycode == BACKSPACE_KEYCODE {
                    self.last_backspace_repeat = None;
                    self.backspace_down_since = None;
                }
                self.pressed_keys.remove(&keycode);
                self.post_key(keycode, false)?;
            }
            KeyState::Repeat => {
                if keycode == BACKSPACE_KEYCODE && !self.backspace_repeat_due() {
                    return Ok(());
                }
                if self.pressed_keys.contains(&keycode) {
                    self.post_key_repeat(keycode)?;
                } else {
                    self.pressed_keys.insert(keycode);
                    self.post_key(keycode, true)?;
                }
            }
        }
        Ok(())
    }

    fn backspace_repeat_due(&mut self) -> bool {
        let now = Instant::now();
        let held_for = self
            .backspace_down_since
            .map(|start| now.saturating_duration_since(start))
            .unwrap_or_default();
        let interval = if held_for >= BACKSPACE_ACCEL_DELAY {
            BACKSPACE_ACCEL_REPEAT_INTERVAL
        } else {
            BACKSPACE_INITIAL_REPEAT_INTERVAL
        };
        if self
            .last_backspace_repeat
            .map(|last| now.saturating_duration_since(last) < interval)
            .unwrap_or(false)
        {
            return false;
        }
        self.last_backspace_repeat = Some(now);
        true
    }

    fn handle_shift_tap(&mut self, scancode: u16, state: KeyState) -> std::io::Result<bool> {
        if !is_shift_scancode(scancode) {
            return Ok(false);
        }

        match state {
            KeyState::Down => {
                self.shift_tap_candidate = Some(scancode);
                Ok(true)
            }
            KeyState::Repeat => Ok(self.shift_tap_candidate == Some(scancode)),
            KeyState::Up if self.shift_tap_candidate == Some(scancode) => {
                self.shift_tap_candidate = None;
                self.toggle_input_source()?;
                Ok(true)
            }
            KeyState::Up => Ok(false),
        }
    }

    fn flush_pending_shift_modifier(&mut self) -> std::io::Result<()> {
        let Some(scancode) = self.shift_tap_candidate.take() else {
            return Ok(());
        };
        if let Some(keycode) = scancode_to_macos_key_for_profile(scancode, self.profile) {
            if self.pressed_keys.insert(keycode) {
                self.post_key(keycode, true)?;
            }
        }
        Ok(())
    }

    fn screenshot_hotkey(&self) -> std::io::Result<()> {
        let command = EventFlags::COMMAND;
        let command_control = EventFlags::COMMAND | EventFlags::CONTROL;
        let full_flags = command_control | EventFlags::SHIFT;
        self.post_key_with_flags(COMMAND_KEYCODE, true, command)?;
        self.post_key_with_flags(CONTROL_KEYCODE, true, command_control)?;
        self.post_key_with_flags(LEFT_SHIFT_KEYCODE, true, full_flags)?;
        self.post_key_with_flags(NUMBER_4_KEYCODE, true, full_flags)?;
        self.post_key_with_flags(NUMBER_4_KEYCODE, false, full_flags)?;
        self.post_key_with_flags(LEFT_SHIFT_KEYCODE, false, command_control)?;
        self.post_key_with_flags(CONTROL_KEYCODE, false, command)?;
        self.post_key_with_flags(COMMAND_KEYCODE, false, EventFlags::empty())?;
        Ok(())
    }

    fn toggle_input_source(&mut self) -> std::io::Result<()> {
        let now = Instant::now();
        if self
            .last_input_source_toggle
            .map(|last| now.saturating_duration_since(last) < INPUT_SOURCE_TOGGLE_DEBOUNCE)
            .unwrap_or(false)
        {
            return Ok(());
        }

        match self.native.cycle_keyboard_input_source() {
            Ok(()) => {
                self.last_input_source_toggle = Some(now);
                Ok(())
            }
            Err(_) => {
                self.toggle_input_source_hotkey()?;
                self.last_input_source_toggle = Some(now);
                Ok(())
            }
        }
    }

    fn toggle_input_source_hotkey(&self) -> std::io::Result<()> {
        self.post_key_with_flags(CONTROL_KEYCODE, true, EventFlags::CONTROL)?;
        self.post_key_with_flags(SPACE_KEYCODE, true, EventFlags::CONTROL)?;
        self.post_key_with_flags(SPACE_KEYCODE, false, EventFlags::CONTROL)?;
        self.post_key_with_flags(CONTROL_KEYCODE, false, EventFlags::empty())
    }

    fn toggle_caps_lock(&mut self) -> std::io::Result<()> {
        self.caps_lock_active = !self.caps_lock_active;
        let flags = if self.caps_lock_active {
            EventFlags::ALPHA_SHIFT
        } else {
            EventFlags::empty()
        };
        self.post_key_with_flags(CAPS_LOCK_KEYCODE, true, flags)?;
        self.post_key_with_flags(CAPS_LOCK_KEYCODE, false, flags)
    }

    fn post_key(&self, keycode: u16, down: bool) -> std::io::Result<()> {
        self.post_key_with_flags_and_repeat(keycode, down, self.flags_for_key(keycode), false)
    }

    fn post_key_repeat(&self, keycode: u16) -> std::io::Result<()> {
        self.post_key_with_flags_and_repeat(keycode, true, self.flags_for_key(keycode), true)
    }

    fn flags_for_key(&self, keycode: u16) -> EventFlags {
        let mut flags = flags_for_pressed_keys(&self.pressed_keys);
        if !self.caps_lock_active {
            return flags;
        }

        flags |= EventFlags::ALPHA_SHIFT;
        if is_letter_keycode(keycode) {
            if flags.contains(EventFlags::SHIFT) {
                flags.remove(EventFlags::SHIFT);
            } else {
                flags |= EventFlags::SHIFT;
            }
        }
        flags
    }

    fn post_key_with_flags(
        &self,
        keycode: u16,
        down: bool,
        flags: EventFlags,
    ) -> std::io::Result<()> {
        self.post_key_with_flags_and_repeat(keycode, down, flags, false)
    }

    fn post_key_with_flags_and_repeat(
        &self,
        keycode: u16,
        down: bool,
        flags: EventFlags,
        autorepeat: bool,
    ) -> std::io::Result<()> {
        self.native.post_key(keycode, down, flags, autorepeat)
    }

    fn mouse_enter(&mut self, x: i32, y: i32) -> std::io::Result<()> {
        if self.remote_active {
            self.release_remote_input();
        }
        if self.cursor_hidden {
            check_native_status(
                unsafe { deskbridge_macos_show_cursor() },
                "failed to show macOS client cursor",
            )?;
            self.cursor_hidden = false;
        }
        self.remote_active = true;
        self.screen_size = screen_size_i32();
        self.set_mouse_position(x, y);
        self.post_pointer_motion(0, 0)
    }

    fn mouse_leave(&mut self) -> std::io::Result<()> {
        self.release_remote_input();
        self.remote_active = false;
        Ok(())
    }

    fn mouse_delta(&mut self, dx: i32, dy: i32) -> std::io::Result<()> {
        let x = translated_mouse_axis(self.mouse_position.x as i32, dx, self.screen_size.0);
        let y = translated_mouse_axis(self.mouse_position.y as i32, dy, self.screen_size.1);
        self.set_mouse_position(x, y);
        self.post_pointer_motion(dx, dy)
    }

    fn set_mouse_position(&mut self, x: i32, y: i32) {
        let x = x.clamp(0, self.screen_size.0.saturating_sub(1));
        let y = y.clamp(0, self.screen_size.1.saturating_sub(1));
        self.mouse_position = Point::new(x as f64, y as f64);
    }

    fn post_pointer_motion(&self, dx: i32, dy: i32) -> std::io::Result<()> {
        if let Some((drag_ty, button)) = self.mouse_buttons.drag_event() {
            self.post_mouse_event(drag_ty, button, 0, dx, dy)?;
            return Ok(());
        }

        self.post_mouse_event(NATIVE_MOUSE_MOVED, NATIVE_BUTTON_LEFT, 0, dx, dy)
    }

    fn post_mouse_event(
        &self,
        event_type: u8,
        button: u8,
        click_count: i64,
        dx: i32,
        dy: i32,
    ) -> std::io::Result<()> {
        self.native
            .post_mouse(event_type, button, self.mouse_position, click_count, dx, dy)
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> std::io::Result<()> {
        let (button, down_ty, up_ty) = match button {
            MouseButton::Left => (
                NATIVE_BUTTON_LEFT,
                NATIVE_MOUSE_LEFT_DOWN,
                NATIVE_MOUSE_LEFT_UP,
            ),
            MouseButton::Right => (
                NATIVE_BUTTON_RIGHT,
                NATIVE_MOUSE_RIGHT_DOWN,
                NATIVE_MOUSE_RIGHT_UP,
            ),
            _ => (
                NATIVE_BUTTON_CENTER,
                NATIVE_MOUSE_OTHER_DOWN,
                NATIVE_MOUSE_OTHER_UP,
            ),
        };
        self.mouse_buttons.set(button, down);
        let click_count = self
            .click_tracker
            .click_count(button, down, self.mouse_position);
        self.post_mouse_event(
            if down { down_ty } else { up_ty },
            button,
            click_count,
            0,
            0,
        )?;
        Ok(())
    }

    fn mouse_wheel(&self, horizontal: i16, vertical: i16) -> std::io::Result<()> {
        self.scroll.push(horizontal as i32, vertical as i32)
    }

    fn release_remote_input(&mut self) {
        for keycode in self.pressed_keys.drain().collect::<Vec<_>>() {
            let _ = self
                .native
                .post_key(keycode, false, EventFlags::empty(), false);
        }
        for (event_type, button) in self.mouse_buttons.release_events() {
            let _ = self
                .native
                .post_mouse(event_type, button, self.mouse_position, 0, 0, 0);
        }
        self.shift_tap_candidate = None;
        self.last_backspace_repeat = None;
        self.backspace_down_since = None;
    }
}

impl Drop for InputSink {
    fn drop(&mut self) {
        self.release_remote_input();
        if self.cursor_hidden {
            if unsafe { deskbridge_macos_show_cursor() } == 0 {
                self.cursor_hidden = false;
            }
        }
    }
}

fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

fn request_accessibility_permission() {
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let prompt = CFBoolean::true_value();
    let options = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), prompt.as_CFType())]);
    let trusted = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 };
    if !trusted {
        eprintln!(
            "open System Settings > Privacy & Security > Accessibility and enable Deskbridge"
        );
    }
}

#[derive(Default)]
struct MouseButtons {
    left: bool,
    right: bool,
    other: bool,
}

impl MouseButtons {
    fn set(&mut self, button: u8, down: bool) {
        match button {
            NATIVE_BUTTON_LEFT => self.left = down,
            NATIVE_BUTTON_RIGHT => self.right = down,
            _ => self.other = down,
        }
    }

    fn drag_event(&self) -> Option<(u8, u8)> {
        if self.left {
            Some((NATIVE_MOUSE_LEFT_DRAGGED, NATIVE_BUTTON_LEFT))
        } else if self.right {
            Some((NATIVE_MOUSE_RIGHT_DRAGGED, NATIVE_BUTTON_RIGHT))
        } else if self.other {
            Some((NATIVE_MOUSE_OTHER_DRAGGED, NATIVE_BUTTON_CENTER))
        } else {
            None
        }
    }

    fn release_events(&mut self) -> Vec<(u8, u8)> {
        let mut events = Vec::new();
        if std::mem::take(&mut self.left) {
            events.push((NATIVE_MOUSE_LEFT_UP, NATIVE_BUTTON_LEFT));
        }
        if std::mem::take(&mut self.right) {
            events.push((NATIVE_MOUSE_RIGHT_UP, NATIVE_BUTTON_RIGHT));
        }
        if std::mem::take(&mut self.other) {
            events.push((NATIVE_MOUSE_OTHER_UP, NATIVE_BUTTON_CENTER));
        }
        events
    }
}

#[derive(Default)]
struct ClickTracker {
    last: Option<ClickRecord>,
}

#[derive(Clone, Copy)]
struct ClickRecord {
    button: u8,
    position: Point,
    at: Instant,
    count: i64,
}

impl ClickTracker {
    fn click_count(&mut self, button: u8, down: bool, position: Point) -> i64 {
        if down {
            let now = Instant::now();
            let count = self
                .last
                .filter(|last| same_click_sequence(last, button, position, now))
                .map(|last| (last.count + 1).min(2))
                .unwrap_or(1);
            self.last = Some(ClickRecord {
                button,
                position,
                at: now,
                count,
            });
            count
        } else {
            self.last.map(|last| last.count).unwrap_or(1)
        }
    }
}

fn same_click_sequence(last: &ClickRecord, button: u8, position: Point, now: Instant) -> bool {
    last.button == button
        && now.saturating_duration_since(last.at) <= DOUBLE_CLICK_WINDOW
        && distance_squared(last.position, position)
            <= DOUBLE_CLICK_DISTANCE * DOUBLE_CLICK_DISTANCE
}

fn distance_squared(a: Point, b: Point) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

#[derive(Clone)]
struct SmoothScroller {
    sender: Sender<ScrollDelta>,
}

#[derive(Debug, Clone, Copy)]
struct ScrollDelta {
    horizontal: i32,
    vertical: i32,
    at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct ScrollConfig {
    frame_interval: Duration,
    scale: f64,
    response: f64,
    max_step: f64,
}

impl SmoothScroller {
    fn spawn() -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || scroll_worker_loop(receiver, ScrollConfig::from_env()));
        Self { sender }
    }

    fn push(&self, horizontal: i32, vertical: i32) -> std::io::Result<()> {
        self.sender
            .send(ScrollDelta {
                horizontal,
                vertical,
                at: Instant::now(),
            })
            .map_err(|e| std::io::Error::new(ErrorKind::BrokenPipe, e))
    }
}

impl ScrollConfig {
    fn from_env() -> Self {
        let frame_ms = env_u64("DESKBRIDGE_SCROLL_FRAME_MS", DEFAULT_SCROLL_FRAME_MS).max(4);
        Self {
            frame_interval: Duration::from_millis(frame_ms),
            scale: env_f64("DESKBRIDGE_SCROLL_SCALE", DEFAULT_SCROLL_SCALE).clamp(0.2, 5.0),
            response: env_f64("DESKBRIDGE_SCROLL_RESPONSE", DEFAULT_SCROLL_RESPONSE)
                .clamp(0.12, 0.75),
            max_step: env_f64("DESKBRIDGE_SCROLL_MAX_STEP", DEFAULT_SCROLL_MAX_STEP)
                .clamp(12.0, 320.0),
        }
    }
}

fn scroll_worker_loop(receiver: Receiver<ScrollDelta>, config: ScrollConfig) {
    let native = match NativeInput::new() {
        Ok(native) => native,
        Err(e) => {
            eprintln!("smooth scroll disabled: {e}");
            return;
        }
    };
    let mut pending_h = 0.0;
    let mut pending_v = 0.0;
    let mut last_input = None;
    let mut last_emit = Instant::now();

    loop {
        let timeout = config
            .frame_interval
            .checked_sub(last_emit.elapsed())
            .unwrap_or(Duration::ZERO);
        match receiver.recv_timeout(timeout) {
            Ok(delta) => {
                add_scroll_delta(
                    delta,
                    &config,
                    &mut pending_h,
                    &mut pending_v,
                    &mut last_input,
                );
                loop {
                    match receiver.try_recv() {
                        Ok(delta) => add_scroll_delta(
                            delta,
                            &config,
                            &mut pending_h,
                            &mut pending_v,
                            &mut last_input,
                        ),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                emit_scroll_frame(&native, &config, &mut pending_h, &mut pending_v);
                last_emit = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if last_emit.elapsed() >= config.frame_interval {
            emit_scroll_frame(&native, &config, &mut pending_h, &mut pending_v);
            last_emit = Instant::now();
        }
    }
}

fn add_scroll_delta(
    delta: ScrollDelta,
    config: &ScrollConfig,
    pending_h: &mut f64,
    pending_v: &mut f64,
    last_input: &mut Option<Instant>,
) {
    let boost = last_input
        .map(|last| scroll_boost(delta.at.saturating_duration_since(last)))
        .unwrap_or(1.0);
    *pending_h += delta.horizontal as f64 * config.scale * boost;
    *pending_v += delta.vertical as f64 * config.scale * boost;
    *last_input = Some(delta.at);
}

fn scroll_boost(delta: Duration) -> f64 {
    if delta >= SCROLL_ACCEL_WINDOW {
        1.0
    } else {
        let closeness = 1.0 - delta.as_secs_f64() / SCROLL_ACCEL_WINDOW.as_secs_f64();
        1.0 + closeness * 0.45
    }
}

fn emit_scroll_frame(
    native: &NativeInput,
    config: &ScrollConfig,
    pending_h: &mut f64,
    pending_v: &mut f64,
) {
    let horizontal = take_scroll_step(pending_h, config);
    let vertical = take_scroll_step(pending_v, config);
    if horizontal == 0 && vertical == 0 {
        return;
    }
    if let Err(e) = native.post_scroll(horizontal, vertical) {
        eprintln!("scroll post failed: {e}");
    }
}

fn take_scroll_step(value: &mut f64, config: &ScrollConfig) -> i32 {
    if value.abs() < 0.75 {
        *value = 0.0;
        return 0;
    }
    let sign = value.signum();
    let mut step = (*value * config.response).clamp(-config.max_step, config.max_step);
    if step.abs() < 1.0 {
        step = sign;
    }
    let mut rounded = step.round() as i32;
    if rounded == 0 {
        rounded = sign as i32;
    }
    *value -= rounded as f64;
    rounded
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn is_shift_scancode(scancode: u16) -> bool {
    matches!(scancode, LEFT_SHIFT_SCANCODE | RIGHT_SHIFT_SCANCODE)
}

fn is_letter_keycode(keycode: u16) -> bool {
    matches!(
        keycode,
        0 | 1
            | 2
            | 3
            | 4
            | 5
            | 6
            | 7
            | 8
            | 9
            | 11
            | 12
            | 13
            | 14
            | 15
            | 16
            | 17
            | 31
            | 32
            | 34
            | 35
            | 37
            | 38
            | 40
            | 45
            | 46
    )
}

fn scancode_to_macos_key_for_profile(scancode: u16, profile: ConnectionProfile) -> Option<u16> {
    Some(match scancode {
        1 => 53,
        2 => 18,
        3 => 19,
        4 => 20,
        5 => NUMBER_4_KEYCODE,
        6 => 23,
        7 => 22,
        8 => 26,
        9 => 28,
        10 => 25,
        11 => 29,
        12 => 27,
        13 => 24,
        14 => 51,
        15 => 48,
        16 => 12,
        17 => 13,
        18 => 14,
        19 => 15,
        20 => 17,
        21 => 16,
        22 => 32,
        23 => 34,
        24 => 31,
        25 => 35,
        26 => 33,
        27 => 30,
        28 => 36,
        29 if matches!(profile, ConnectionProfile::MacOSToMacOS) => CONTROL_KEYCODE,
        29 => COMMAND_KEYCODE,
        30 => 0,
        31 => 1,
        32 => 2,
        33 => 3,
        34 => 5,
        35 => 4,
        36 => 38,
        37 => 40,
        38 => 37,
        39 => 41,
        40 => 39,
        41 => 50,
        42 => LEFT_SHIFT_KEYCODE,
        43 => 42,
        44 => 6,
        45 => 7,
        46 => 8,
        47 => 9,
        48 => 11,
        49 => 45,
        50 => 46,
        51 => 43,
        52 => 47,
        53 => 44,
        54 => RIGHT_SHIFT_KEYCODE,
        55 => 67,
        56 => OPTION_KEYCODE,
        57 => SPACE_KEYCODE,
        58 => 57,
        59 => 122,
        60 => 120,
        61 => 99,
        62 => 118,
        63 => 96,
        64 => 97,
        65 => 98,
        66 => 100,
        67 => 101,
        68 => 109,
        69 => 71,
        71 => 89,
        72 => 91,
        73 => 92,
        74 => 78,
        75 => 86,
        76 => 87,
        77 => 88,
        78 => 69,
        79 => 83,
        80 => 84,
        81 => 85,
        82 => 82,
        83 => 65,
        91 => COMMAND_KEYCODE,
        92 => RIGHT_COMMAND_KEYCODE,
        87 => 103,
        88 => 111,
        284 => 76,
        285 if matches!(profile, ConnectionProfile::MacOSToMacOS) => 62,
        285 => RIGHT_COMMAND_KEYCODE,
        309 => 75,
        312 => RIGHT_OPTION_KEYCODE,
        327 => 115,
        328 => 126,
        329 => 116,
        331 => 123,
        333 => 124,
        335 => 119,
        336 => 125,
        337 => 121,
        338 => 114,
        339 => 117,
        347 => COMMAND_KEYCODE,
        348 => RIGHT_COMMAND_KEYCODE,
        _ => return None,
    })
}

fn flags_for_pressed_keys(keys: &HashSet<u16>) -> EventFlags {
    let mut flags = EventFlags::empty();
    if keys.contains(&COMMAND_KEYCODE) || keys.contains(&RIGHT_COMMAND_KEYCODE) {
        flags |= EventFlags::COMMAND;
    }
    if keys.contains(&CONTROL_KEYCODE) {
        flags |= EventFlags::CONTROL;
    }
    if keys.contains(&OPTION_KEYCODE) || keys.contains(&RIGHT_OPTION_KEYCODE) {
        flags |= EventFlags::ALTERNATE;
    }
    if keys.contains(&LEFT_SHIFT_KEYCODE) || keys.contains(&RIGHT_SHIFT_KEYCODE) {
        flags |= EventFlags::SHIFT;
    }
    flags
}

fn check_native_status(status: i32, message: &str) -> std::io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            ErrorKind::Other,
            format!("{message} (native status {status})"),
        ))
    }
}

fn event_err(message: &str) -> std::io::Error {
    std::io::Error::new(ErrorKind::Other, message)
}

pub fn screen_size() -> (u32, u32) {
    let (width, height) = current_screen_size();
    (width.max(1), height.max(1))
}

// Preserve zero/invalid native samples for the live geometry validator. The
// startup fallback above must not turn a transient missing display into 1×1.
pub fn current_screen_size() -> (u32, u32) {
    let mut width = 0u32;
    let mut height = 0u32;
    unsafe {
        deskbridge_main_display_size(&mut width, &mut height);
    }
    (width, height)
}

fn screen_size_i32() -> (i32, i32) {
    let (width, height) = screen_size();
    (width.max(1) as i32, height.max(1) as i32)
}

// Pure position arithmetic; keeps native event deltas independent of clamping.
fn translated_mouse_axis(position: i32, delta: i32, extent: i32) -> i32 {
    position
        .saturating_add(delta)
        .clamp(0, extent.saturating_sub(1))
}

#[cfg(test)]
mod position_tests {
    use super::translated_mouse_axis;

    #[test]
    fn coalesced_extreme_motion_clamps_without_integer_overflow() {
        assert_eq!(translated_mouse_axis(8, i32::MAX, 1920), 1919);
        assert_eq!(translated_mouse_axis(8, i32::MIN, 1920), 0);
        assert_eq!(translated_mouse_axis(0, i32::MAX, 1), 0);
    }

    #[test]
    fn ordinary_motion_and_reversal_keep_stepwise_edge_clipping() {
        assert_eq!(translated_mouse_axis(8, 3, 20), 11);
        assert_eq!(translated_mouse_axis(8, -3, 20), 5);
        let edge = translated_mouse_axis(8, 4, 10);
        assert_eq!(edge, 9);
        assert_eq!(translated_mouse_axis(edge, -2, 10), 7);
    }
}
