use std::collections::HashSet;
use std::mem::size_of;

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SetCursorPos, SetProcessDPIAware,
};

use crate::platform::ConnectionProfile;
use crate::protocol::{InputEvent, KeyState, MouseButton};

const XBUTTON1_DATA: u32 = 1;
const XBUTTON2_DATA: u32 = 2;

pub struct InputSink {
    pressed_keys: HashSet<u16>,
    pressed_buttons: HashSet<MouseButton>,
    mouse_position: (i32, i32),
    screen_size: (i32, i32),
    remote_active: bool,
}

impl InputSink {
    pub fn new(profile: ConnectionProfile) -> std::io::Result<Self> {
        eprintln!("Windows input profile: {}", profile.as_str());
        unsafe {
            SetProcessDPIAware();
        }
        let screen_size = screen_size_i32();
        let mut point = POINT { x: 0, y: 0 };
        let mouse_position = if unsafe { GetCursorPos(&mut point) } != 0 {
            (
                point.x.clamp(0, screen_size.0 - 1),
                point.y.clamp(0, screen_size.1 - 1),
            )
        } else {
            (0, 0)
        };
        Ok(Self {
            pressed_keys: HashSet::new(),
            pressed_buttons: HashSet::new(),
            mouse_position,
            screen_size,
            remote_active: false,
        })
    }

    pub fn apply(&mut self, event: InputEvent) -> std::io::Result<()> {
        match event {
            InputEvent::MouseEnter { x, y } => self.mouse_enter(x, y),
            InputEvent::ScreenSync { .. } => Ok(()),
            InputEvent::MouseLeave => {
                self.mouse_leave();
                Ok(())
            }
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
        match state {
            KeyState::Down => {
                if self.pressed_keys.insert(scancode) {
                    send_key(scancode, false)?;
                }
            }
            KeyState::Up => {
                self.pressed_keys.remove(&scancode);
                send_key(scancode, true)?;
            }
            KeyState::Repeat => {
                send_key(scancode, false)?;
            }
        }
        Ok(())
    }

    fn mouse_enter(&mut self, x: i32, y: i32) -> std::io::Result<()> {
        if self.remote_active {
            self.release_remote_input();
        }
        self.remote_active = true;
        self.screen_size = screen_size_i32();
        self.mouse_position = (
            x.clamp(0, self.screen_size.0 - 1),
            y.clamp(0, self.screen_size.1 - 1),
        );
        if unsafe { SetCursorPos(self.mouse_position.0, self.mouse_position.1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn mouse_delta(&mut self, dx: i32, dy: i32) -> std::io::Result<()> {
        self.mouse_position.0 = self
            .mouse_position
            .0
            .saturating_add(dx)
            .clamp(0, self.screen_size.0 - 1);
        self.mouse_position.1 = self
            .mouse_position
            .1
            .saturating_add(dy)
            .clamp(0, self.screen_size.1 - 1);
        if unsafe { SetCursorPos(self.mouse_position.0, self.mouse_position.1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn mouse_button(&mut self, button: MouseButton, down: bool) -> std::io::Result<()> {
        let (flags, data) = match button {
            MouseButton::Left => (
                if down {
                    MOUSEEVENTF_LEFTDOWN
                } else {
                    MOUSEEVENTF_LEFTUP
                },
                0,
            ),
            MouseButton::Middle => (
                if down {
                    MOUSEEVENTF_MIDDLEDOWN
                } else {
                    MOUSEEVENTF_MIDDLEUP
                },
                0,
            ),
            MouseButton::Right => (
                if down {
                    MOUSEEVENTF_RIGHTDOWN
                } else {
                    MOUSEEVENTF_RIGHTUP
                },
                0,
            ),
            MouseButton::Extra(id) => (
                if down {
                    MOUSEEVENTF_XDOWN
                } else {
                    MOUSEEVENTF_XUP
                },
                if id == 5 {
                    XBUTTON2_DATA
                } else {
                    XBUTTON1_DATA
                },
            ),
        };
        send_mouse(flags, 0, 0, data)?;
        if down {
            self.pressed_buttons.insert(button);
        } else {
            self.pressed_buttons.remove(&button);
        }
        Ok(())
    }

    fn mouse_wheel(&self, horizontal: i16, vertical: i16) -> std::io::Result<()> {
        if vertical != 0 {
            send_mouse(MOUSEEVENTF_WHEEL, 0, 0, vertical as i32 as u32)?;
        }
        if horizontal != 0 {
            send_mouse(MOUSEEVENTF_HWHEEL, 0, 0, horizontal as i32 as u32)?;
        }
        Ok(())
    }

    fn mouse_leave(&mut self) {
        self.release_remote_input();
        self.remote_active = false;
    }

    fn release_remote_input(&mut self) {
        for scancode in self.pressed_keys.drain().collect::<Vec<_>>() {
            let _ = send_key(scancode, true);
        }
        for button in self.pressed_buttons.drain().collect::<Vec<_>>() {
            let _ = release_mouse_button(button);
        }
    }
}

impl Drop for InputSink {
    fn drop(&mut self) {
        self.release_remote_input();
    }
}

fn release_mouse_button(button: MouseButton) -> std::io::Result<()> {
    let (flags, data) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTUP, 0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTUP, 0),
        MouseButton::Extra(id) => (
            MOUSEEVENTF_XUP,
            if id == 5 {
                XBUTTON2_DATA
            } else {
                XBUTTON1_DATA
            },
        ),
    };
    send_mouse(flags, 0, 0, data)
}

fn send_key(scancode: u16, key_up: bool) -> std::io::Result<()> {
    let (scan, extended) = decode_scancode(scancode);
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }

    send_input(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}

fn decode_scancode(scancode: u16) -> (u16, bool) {
    if scancode >= 256 {
        (scancode - 256, true)
    } else {
        (scancode, false)
    }
}

fn send_mouse(flags: u32, dx: i32, dy: i32, data: u32) -> std::io::Result<()> {
    send_input(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}

fn send_input(input: INPUT) -> std::io::Result<()> {
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub fn screen_size() -> (u32, u32) {
    let (width, height) = current_screen_size();
    (width.max(1), height.max(1))
}

pub fn current_screen_size() -> (u32, u32) {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(0) as u32;
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(0) as u32;
    (width, height)
}

fn screen_size_i32() -> (i32, i32) {
    let (width, height) = screen_size();
    (width as i32, height as i32)
}
