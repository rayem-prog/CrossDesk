use std::io::{Read, Write};
use std::path::PathBuf;

use crate::platform::Platform;

pub const VERSION: u16 = 3;
pub const CHUNK_SIZE: usize = 16 * 1024;
pub const MAX_FRAME_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;

// Wire-format tags. Keeping them named makes protocol changes auditable and
// avoids scattering magic numbers through the encoders and decoders.
const FRAME_HELLO: u8 = 1;
const FRAME_INPUT: u8 = 2;
const FRAME_CLIPBOARD: u8 = 3;
const FRAME_FILE_START: u8 = 4;
const FRAME_FILE_CHUNK: u8 = 5;
const FRAME_FILE_END: u8 = 6;
const FRAME_DRAG_START: u8 = 7;
const FRAME_DRAG_UPDATE: u8 = 8;
const FRAME_DRAG_END: u8 = 9;
const FRAME_ERROR: u8 = 10;

const INPUT_KEY: u8 = 1;
const INPUT_MOUSE_ENTER: u8 = 2;
const INPUT_MOUSE_BUTTON: u8 = 3;
const INPUT_MOUSE_WHEEL: u8 = 4;
const INPUT_MOUSE_DELTA: u8 = 5;
const INPUT_MOUSE_LEAVE: u8 = 6;

const KEY_DOWN: u8 = 1;
const KEY_UP: u8 = 2;
const KEY_REPEAT: u8 = 3;

const MOUSE_LEFT: u8 = 1;
const MOUSE_MIDDLE: u8 = 2;
const MOUSE_RIGHT: u8 = 3;

const CLIPBOARD_TEXT: u8 = 1;
const CLIPBOARD_BITMAP: u8 = 2;
const CLIPBOARD_FILES: u8 = 3;

const KEY_EVENT_LEN: usize = 4;
const MOUSE_BUTTON_EVENT_LEN: usize = 3;
const MOUSE_WHEEL_EVENT_LEN: usize = 5;
const POINTER_COORD_EVENT_LEN: usize = 9;
const MOUSE_LEAVE_EVENT_LEN: usize = 1;
const INPUT_KIND_OFFSET: usize = 0;
const KEY_SCANCODE_OFFSET: usize = 1;
const KEY_STATE_OFFSET: usize = 3;
const POINTER_X_OFFSET: usize = 1;
const POINTER_Y_OFFSET: usize = 5;
const MOUSE_BUTTON_OFFSET: usize = 1;
const MOUSE_BUTTON_STATE_OFFSET: usize = 2;
const WHEEL_HORIZONTAL_OFFSET: usize = 1;
const WHEEL_VERTICAL_OFFSET: usize = 3;
const HELLO_VERSION_OFFSET: usize = 0;
const HELLO_PLATFORM_OFFSET: usize = 2;
const HELLO_WITH_PLATFORM_SCREEN_WIDTH_OFFSET: usize = 3;
const HELLO_WITH_PLATFORM_SCREEN_HEIGHT_OFFSET: usize = 7;
const LEGACY_HELLO_SCREEN_WIDTH_OFFSET: usize = 2;
const LEGACY_HELLO_SCREEN_HEIGHT_OFFSET: usize = 6;
const HELLO_MIN_VERSION_LEN: usize = 2;
const HELLO_WITH_PLATFORM_AND_SCREEN_LEN: usize = 11;
const LEGACY_HELLO_WITH_SCREEN_LEN: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Hello = FRAME_HELLO,
    Input = FRAME_INPUT,
    Clipboard = FRAME_CLIPBOARD,
    FileStart = FRAME_FILE_START,
    FileChunk = FRAME_FILE_CHUNK,
    FileEnd = FRAME_FILE_END,
    DragStart = FRAME_DRAG_START,
    DragUpdate = FRAME_DRAG_UPDATE,
    DragEnd = FRAME_DRAG_END,
    Error = FRAME_ERROR,
}

impl FrameKind {
    fn from_byte(byte: u8) -> std::io::Result<Self> {
        match byte {
            FRAME_HELLO => Ok(Self::Hello),
            FRAME_INPUT => Ok(Self::Input),
            FRAME_CLIPBOARD => Ok(Self::Clipboard),
            FRAME_FILE_START => Ok(Self::FileStart),
            FRAME_FILE_CHUNK => Ok(Self::FileChunk),
            FRAME_FILE_END => Ok(Self::FileEnd),
            FRAME_DRAG_START => Ok(Self::DragStart),
            FRAME_DRAG_UPDATE => Ok(Self::DragUpdate),
            FRAME_DRAG_END => Ok(Self::DragEnd),
            FRAME_ERROR => Ok(Self::Error),
            _ => Err(invalid("unknown frame kind")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: FrameKind, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPayload {
    Text(String),
    Bitmap(Vec<u8>),
    Files(Vec<PathBuf>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Down,
    Up,
    Repeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Extra(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key { scancode: u16, state: KeyState },
    MouseEnter { x: i32, y: i32 },
    MouseLeave,
    MouseDelta { dx: i32, dy: i32 },
    MouseButton { button: MouseButton, down: bool },
    MouseWheel { horizontal: i16, vertical: i16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub platform: Platform,
    pub screen_width: Option<u32>,
    pub screen_height: Option<u32>,
}

pub fn read_frame(reader: &mut impl Read) -> std::io::Result<Frame> {
    let mut kind = [0u8; 1];
    reader.read_exact(&mut kind)?;
    let kind = FrameKind::from_byte(kind[0])?;

    let mut len = [0u8; 8];
    reader.read_exact(&mut len)?;
    let len = u64::from_be_bytes(len);
    if len > MAX_FRAME_PAYLOAD_SIZE as u64 {
        return Err(invalid_owned(format!(
            "frame payload too large: {len} bytes exceeds limit of {MAX_FRAME_PAYLOAD_SIZE} bytes"
        )));
    }

    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload)?;
    Ok(Frame::new(kind, payload))
}

pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    write_frame_unflushed(writer, frame)?;
    writer.flush()
}

pub fn write_frame_unflushed(writer: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    writer.write_all(&[frame.kind as u8])?;
    writer.write_all(&(frame.payload.len() as u64).to_be_bytes())?;
    writer.write_all(&frame.payload)
}

pub fn hello_payload() -> Vec<u8> {
    let mut out = VERSION.to_be_bytes().to_vec();
    out.push(Platform::local().as_byte());
    out
}

pub fn hello_payload_with_screen(width: u32, height: u32) -> Vec<u8> {
    let mut out = hello_payload();
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out
}

pub fn decode_hello(payload: &[u8]) -> std::io::Result<Hello> {
    if payload.len() < HELLO_MIN_VERSION_LEN {
        return Err(invalid("short hello payload"));
    }
    let version = read_u16_at(payload, HELLO_VERSION_OFFSET)?;
    let platform = payload
        .get(HELLO_PLATFORM_OFFSET)
        .copied()
        .map(Platform::from_byte)
        .unwrap_or(Platform::Unknown);
    let (screen_width, screen_height) = if payload.len() >= HELLO_WITH_PLATFORM_AND_SCREEN_LEN {
        (
            Some(read_u32_at(
                payload,
                HELLO_WITH_PLATFORM_SCREEN_WIDTH_OFFSET,
            )?),
            Some(read_u32_at(
                payload,
                HELLO_WITH_PLATFORM_SCREEN_HEIGHT_OFFSET,
            )?),
        )
    } else if payload.len() >= LEGACY_HELLO_WITH_SCREEN_LEN {
        // Compatibility with older peers that sent screen size before platform.
        (
            Some(read_u32_at(payload, LEGACY_HELLO_SCREEN_WIDTH_OFFSET)?),
            Some(read_u32_at(payload, LEGACY_HELLO_SCREEN_HEIGHT_OFFSET)?),
        )
    } else {
        (None, None)
    };
    Ok(Hello {
        version,
        platform,
        screen_width,
        screen_height,
    })
}

pub fn validate_version(hello: Hello) -> std::io::Result<()> {
    if hello.version == VERSION {
        Ok(())
    } else {
        Err(invalid_owned(format!(
            "protocol version mismatch: peer={}, local={VERSION}",
            hello.version
        )))
    }
}

pub fn encode_input(event: &InputEvent) -> Vec<u8> {
    let mut payload = Vec::new();
    match *event {
        InputEvent::Key { scancode, state } => {
            payload.push(INPUT_KEY);
            payload.extend_from_slice(&scancode.to_be_bytes());
            payload.push(match state {
                KeyState::Down => KEY_DOWN,
                KeyState::Up => KEY_UP,
                KeyState::Repeat => KEY_REPEAT,
            });
        }
        InputEvent::MouseEnter { x, y } => {
            payload.push(INPUT_MOUSE_ENTER);
            payload.extend_from_slice(&x.to_be_bytes());
            payload.extend_from_slice(&y.to_be_bytes());
        }
        InputEvent::MouseLeave => payload.push(INPUT_MOUSE_LEAVE),
        InputEvent::MouseButton { button, down } => {
            payload.push(INPUT_MOUSE_BUTTON);
            payload.push(match button {
                MouseButton::Left => MOUSE_LEFT,
                MouseButton::Middle => MOUSE_MIDDLE,
                MouseButton::Right => MOUSE_RIGHT,
                MouseButton::Extra(id) => id,
            });
            payload.push(u8::from(down));
        }
        InputEvent::MouseWheel {
            horizontal,
            vertical,
        } => {
            payload.push(INPUT_MOUSE_WHEEL);
            payload.extend_from_slice(&horizontal.to_be_bytes());
            payload.extend_from_slice(&vertical.to_be_bytes());
        }
        InputEvent::MouseDelta { dx, dy } => {
            payload.push(INPUT_MOUSE_DELTA);
            payload.extend_from_slice(&dx.to_be_bytes());
            payload.extend_from_slice(&dy.to_be_bytes());
        }
    }
    payload
}

pub fn decode_input(payload: &[u8]) -> std::io::Result<InputEvent> {
    let kind = *payload
        .get(INPUT_KIND_OFFSET)
        .ok_or_else(|| invalid("short input event"))?;
    match kind {
        INPUT_KEY => {
            if payload.len() != KEY_EVENT_LEN {
                return Err(invalid("invalid key event"));
            }
            let scancode = read_u16_at(payload, KEY_SCANCODE_OFFSET)?;
            let state = match payload[KEY_STATE_OFFSET] {
                KEY_DOWN => KeyState::Down,
                KEY_UP => KeyState::Up,
                KEY_REPEAT => KeyState::Repeat,
                _ => return Err(invalid("invalid key state")),
            };
            Ok(InputEvent::Key { scancode, state })
        }
        INPUT_MOUSE_ENTER => {
            if payload.len() != POINTER_COORD_EVENT_LEN {
                return Err(invalid("invalid mouse enter event"));
            }
            let x = read_i32_at(payload, POINTER_X_OFFSET)?;
            let y = read_i32_at(payload, POINTER_Y_OFFSET)?;
            Ok(InputEvent::MouseEnter { x, y })
        }
        INPUT_MOUSE_BUTTON => {
            if payload.len() != MOUSE_BUTTON_EVENT_LEN {
                return Err(invalid("invalid mouse button event"));
            }
            let button = match payload[MOUSE_BUTTON_OFFSET] {
                MOUSE_LEFT => MouseButton::Left,
                MOUSE_MIDDLE => MouseButton::Middle,
                MOUSE_RIGHT => MouseButton::Right,
                id => MouseButton::Extra(id),
            };
            Ok(InputEvent::MouseButton {
                button,
                down: payload[MOUSE_BUTTON_STATE_OFFSET] != 0,
            })
        }
        INPUT_MOUSE_WHEEL => {
            if payload.len() != MOUSE_WHEEL_EVENT_LEN {
                return Err(invalid("invalid wheel event"));
            }
            Ok(InputEvent::MouseWheel {
                horizontal: read_i16_at(payload, WHEEL_HORIZONTAL_OFFSET)?,
                vertical: read_i16_at(payload, WHEEL_VERTICAL_OFFSET)?,
            })
        }
        INPUT_MOUSE_DELTA => {
            if payload.len() != POINTER_COORD_EVENT_LEN {
                return Err(invalid("invalid mouse delta event"));
            }
            let dx = read_i32_at(payload, POINTER_X_OFFSET)?;
            let dy = read_i32_at(payload, POINTER_Y_OFFSET)?;
            Ok(InputEvent::MouseDelta { dx, dy })
        }
        INPUT_MOUSE_LEAVE if payload.len() == MOUSE_LEAVE_EVENT_LEN => Ok(InputEvent::MouseLeave),
        INPUT_MOUSE_LEAVE => Err(invalid("invalid mouse leave event")),
        _ => Err(invalid("unknown input event kind")),
    }
}

pub fn encode_clipboard(payload: &ClipboardPayload) -> Vec<u8> {
    let mut out = Vec::new();
    match payload {
        ClipboardPayload::Text(text) => {
            out.push(CLIPBOARD_TEXT);
            write_bytes(&mut out, text.as_bytes());
        }
        ClipboardPayload::Bitmap(bitmap) => {
            out.push(CLIPBOARD_BITMAP);
            write_bytes(&mut out, bitmap);
        }
        ClipboardPayload::Files(files) => {
            out.push(CLIPBOARD_FILES);
            out.extend_from_slice(&(files.len() as u32).to_be_bytes());
            for file in files {
                write_bytes(&mut out, file.to_string_lossy().as_bytes());
            }
        }
    }
    out
}

pub fn decode_clipboard(payload: &[u8]) -> std::io::Result<ClipboardPayload> {
    let kind = *payload
        .first()
        .ok_or_else(|| invalid("short clipboard payload"))?;
    let mut offset = 1;
    match kind {
        CLIPBOARD_TEXT => Ok(ClipboardPayload::Text(
            String::from_utf8(read_bytes(payload, &mut offset)?.to_vec())
                .map_err(|e| invalid_owned(e.to_string()))?,
        )),
        CLIPBOARD_BITMAP => Ok(ClipboardPayload::Bitmap(
            read_bytes(payload, &mut offset)?.to_vec(),
        )),
        CLIPBOARD_FILES => {
            let count = read_u32(payload, &mut offset)?;
            let mut files = Vec::new();
            for _ in 0..count {
                files.push(PathBuf::from(
                    String::from_utf8(read_bytes(payload, &mut offset)?.to_vec())
                        .map_err(|e| invalid_owned(e.to_string()))?,
                ));
            }
            Ok(ClipboardPayload::Files(files))
        }
        _ => Err(invalid("unknown clipboard payload kind")),
    }
}

pub fn encode_file_start(relative: &str, len: u64) -> Vec<u8> {
    let mut out = Vec::new();
    write_bytes(&mut out, relative.as_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out
}

pub fn decode_file_start(payload: &[u8]) -> std::io::Result<(String, u64)> {
    let mut offset = 0;
    let relative = String::from_utf8(read_bytes(payload, &mut offset)?.to_vec())
        .map_err(|e| invalid_owned(e.to_string()))?;
    if payload.len() < offset + 8 {
        return Err(invalid("short file start payload"));
    }
    let len = read_u64_at(payload, offset)?;
    Ok((relative, len))
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn read_bytes<'a>(payload: &'a [u8], offset: &mut usize) -> std::io::Result<&'a [u8]> {
    let len = read_u32(payload, offset)? as usize;
    let end = *offset + len;
    let bytes = payload
        .get(*offset..end)
        .ok_or_else(|| invalid("truncated bytes"))?;
    *offset = end;
    Ok(bytes)
}

fn read_u32(payload: &[u8], offset: &mut usize) -> std::io::Result<u32> {
    let value = read_u32_at(payload, *offset)?;
    *offset += 4;
    Ok(value)
}

fn read_i16_at(payload: &[u8], start: usize) -> std::io::Result<i16> {
    let bytes = payload
        .get(start..start + 2)
        .ok_or_else(|| invalid("short i16"))?;
    Ok(i16::from_be_bytes(bytes.try_into().unwrap()))
}

fn read_u16_at(payload: &[u8], start: usize) -> std::io::Result<u16> {
    let bytes = payload
        .get(start..start + 2)
        .ok_or_else(|| invalid("short u16"))?;
    Ok(u16::from_be_bytes(bytes.try_into().unwrap()))
}

fn read_i32_at(payload: &[u8], start: usize) -> std::io::Result<i32> {
    let bytes = payload
        .get(start..start + 4)
        .ok_or_else(|| invalid("short i32"))?;
    Ok(i32::from_be_bytes(bytes.try_into().unwrap()))
}

fn read_u32_at(payload: &[u8], start: usize) -> std::io::Result<u32> {
    let bytes = payload
        .get(start..start + 4)
        .ok_or_else(|| invalid("short u32"))?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

fn read_u64_at(payload: &[u8], start: usize) -> std::io::Result<u64> {
    let bytes = payload
        .get(start..start + 8)
        .ok_or_else(|| invalid("short u64"))?;
    Ok(u64::from_be_bytes(bytes.try_into().unwrap()))
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn invalid_owned(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_events_round_trip() {
        let events = [
            InputEvent::MouseEnter { x: 0, y: 450 },
            InputEvent::MouseDelta { dx: 12, dy: -7 },
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: true,
            },
            InputEvent::MouseWheel {
                horizontal: -2,
                vertical: 8,
            },
            InputEvent::Key {
                scancode: 29,
                state: KeyState::Down,
            },
            InputEvent::MouseLeave,
        ];

        for event in events {
            assert_eq!(decode_input(&encode_input(&event)).unwrap(), event);
        }
    }

    #[test]
    fn leave_rejects_trailing_data() {
        assert!(decode_input(&[6, 0]).is_err());
    }

    #[test]
    fn protocol_version_mismatch_is_rejected() {
        assert!(
            validate_version(Hello {
                version: VERSION - 1,
                platform: Platform::Unknown,
                screen_width: None,
                screen_height: None,
            })
            .is_err()
        );
    }

    #[test]
    fn read_frame_rejects_unknown_kind_before_allocating() {
        let bytes = [255u8, 0, 0, 0, 0, 0, 0, 0, 4, b't', b'e', b's', b't'];
        let err = read_frame(&mut std::io::Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_frame_rejects_oversized_payload_before_allocating() {
        let mut bytes = vec![FrameKind::Hello as u8];
        bytes.extend_from_slice(&((MAX_FRAME_PAYLOAD_SIZE as u64) + 1).to_be_bytes());
        let err = read_frame(&mut std::io::Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("frame payload too large"));
    }
}
