// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::platform::Platform;

pub const VERSION: u16 = 6;
pub const CHUNK_SIZE: usize = 16 * 1024;
pub const MAX_FRAME_PAYLOAD_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Hello = 1,
    Input = 2,
    Clipboard = 3,
    FileStart = 4,
    FileChunk = 5,
    FileEnd = 6,
    DragStart = 7,
    DragUpdate = 8,
    DragEnd = 9,
    Error = 10,
    Ping = 11,
    Pong = 12,
    ImageStart = 13,
    ImageChunk = 14,
    ImageEnd = 15,
    FileOffer = 16,
    FileData = 17,
    FileSeal = 18,
    FileCommit = 19,
    FileAck = 20,
    FileCancel = 21,
    TransferResult = 22,
}

impl FrameKind {
    fn from_byte(byte: u8) -> std::io::Result<Self> {
        match byte {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Input),
            3 => Ok(Self::Clipboard),
            4 => Ok(Self::FileStart),
            5 => Ok(Self::FileChunk),
            6 => Ok(Self::FileEnd),
            7 => Ok(Self::DragStart),
            8 => Ok(Self::DragUpdate),
            9 => Ok(Self::DragEnd),
            10 => Ok(Self::Error),
            11 => Ok(Self::Ping),
            12 => Ok(Self::Pong),
            13 => Ok(Self::ImageStart),
            14 => Ok(Self::ImageChunk),
            15 => Ok(Self::ImageEnd),
            16 => Ok(Self::FileOffer),
            17 => Ok(Self::FileData),
            18 => Ok(Self::FileSeal),
            19 => Ok(Self::FileCommit),
            20 => Ok(Self::FileAck),
            21 => Ok(Self::FileCancel),
            22 => Ok(Self::TransferResult),
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
    pub image_clipboard: bool,
    pub file_clipboard: bool,
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
    if frame.payload.len() > MAX_FRAME_PAYLOAD_SIZE {
        return Err(invalid(
            "frame payload exceeds 64 KiB; use chunked transfer",
        ));
    }
    writer.write_all(&[frame.kind as u8])?;
    writer.write_all(&(frame.payload.len() as u64).to_be_bytes())?;
    writer.write_all(&frame.payload)
}

pub fn hello_payload() -> Vec<u8> {
    let mut out = VERSION.to_be_bytes().to_vec();
    out.push(Platform::local().as_byte());
    out.push(
        u8::from(crate::transport::image_clipboard_enabled())
            | (u8::from(crate::transport::file_clipboard_enabled()) << 1),
    );
    out
}

pub fn hello_payload_with_screen(width: u32, height: u32) -> Vec<u8> {
    let mut out = hello_payload();
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out
}

pub fn decode_hello(payload: &[u8]) -> std::io::Result<Hello> {
    if payload.len() < 2 {
        return Err(invalid("short hello payload"));
    }
    let version = u16::from_be_bytes([payload[0], payload[1]]);
    let platform = payload
        .get(2)
        .copied()
        .map(Platform::from_byte)
        .unwrap_or(Platform::Unknown);
    let dimensions_offset = if version >= 5 { 4 } else { 3 };
    if version == VERSION && !matches!(payload.len(), 4 | 12) {
        return Err(invalid("invalid v6 hello length"));
    }
    let (screen_width, screen_height) = if payload.len() >= dimensions_offset + 8 {
        (
            Some(u32::from_be_bytes(
                payload[dimensions_offset..dimensions_offset + 4]
                    .try_into()
                    .unwrap(),
            )),
            Some(u32::from_be_bytes(
                payload[dimensions_offset + 4..dimensions_offset + 8]
                    .try_into()
                    .unwrap(),
            )),
        )
    } else if payload.len() >= 10 {
        (
            Some(u32::from_be_bytes(payload[2..6].try_into().unwrap())),
            Some(u32::from_be_bytes(payload[6..10].try_into().unwrap())),
        )
    } else {
        (None, None)
    };
    Ok(Hello {
        version,
        platform,
        screen_width,
        screen_height,
        image_clipboard: version >= 5 && payload.get(3).is_some_and(|v| v & 1 != 0),
        file_clipboard: version >= 6 && payload.get(3).is_some_and(|v| v & 2 != 0),
    })
}

pub fn validate_version(hello: Hello) -> std::io::Result<()> {
    if hello.version == VERSION {
        Ok(())
    } else {
        Err(invalid_owned(format!(
            "protocol version mismatch: peer={}, local={VERSION}; update CrossDesk on BOTH machines",
            hello.version
        )))
    }
}

pub fn encode_input(event: &InputEvent) -> Vec<u8> {
    let mut payload = Vec::new();
    match *event {
        InputEvent::Key { scancode, state } => {
            payload.push(1);
            payload.extend_from_slice(&scancode.to_be_bytes());
            payload.push(match state {
                KeyState::Down => 1,
                KeyState::Up => 2,
                KeyState::Repeat => 3,
            });
        }
        InputEvent::MouseEnter { x, y } => {
            payload.push(2);
            payload.extend_from_slice(&x.to_be_bytes());
            payload.extend_from_slice(&y.to_be_bytes());
        }
        InputEvent::MouseLeave => payload.push(6),
        InputEvent::MouseButton { button, down } => {
            payload.push(3);
            payload.push(match button {
                MouseButton::Left => 1,
                MouseButton::Middle => 2,
                MouseButton::Right => 3,
                MouseButton::Extra(id) => id,
            });
            payload.push(u8::from(down));
        }
        InputEvent::MouseWheel {
            horizontal,
            vertical,
        } => {
            payload.push(4);
            payload.extend_from_slice(&horizontal.to_be_bytes());
            payload.extend_from_slice(&vertical.to_be_bytes());
        }
        InputEvent::MouseDelta { dx, dy } => {
            payload.push(5);
            payload.extend_from_slice(&dx.to_be_bytes());
            payload.extend_from_slice(&dy.to_be_bytes());
        }
    }
    payload
}

pub fn decode_input(payload: &[u8]) -> std::io::Result<InputEvent> {
    let kind = *payload
        .first()
        .ok_or_else(|| invalid("short input event"))?;
    match kind {
        1 => {
            if payload.len() != 4 {
                return Err(invalid("invalid key event"));
            }
            let scancode = u16::from_be_bytes([payload[1], payload[2]]);
            let state = match payload[3] {
                1 => KeyState::Down,
                2 => KeyState::Up,
                3 => KeyState::Repeat,
                _ => return Err(invalid("invalid key state")),
            };
            Ok(InputEvent::Key { scancode, state })
        }
        2 => {
            if payload.len() != 9 {
                return Err(invalid("invalid mouse enter event"));
            }
            let x = i32::from_be_bytes(payload[1..5].try_into().unwrap());
            let y = i32::from_be_bytes(payload[5..9].try_into().unwrap());
            Ok(InputEvent::MouseEnter { x, y })
        }
        3 => {
            if payload.len() != 3 {
                return Err(invalid("invalid mouse button event"));
            }
            let button = match payload[1] {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                id => MouseButton::Extra(id),
            };
            Ok(InputEvent::MouseButton {
                button,
                down: payload[2] != 0,
            })
        }
        4 => {
            if payload.len() != 5 {
                return Err(invalid("invalid wheel event"));
            }
            Ok(InputEvent::MouseWheel {
                horizontal: i16::from_be_bytes([payload[1], payload[2]]),
                vertical: i16::from_be_bytes([payload[3], payload[4]]),
            })
        }
        5 => {
            if payload.len() != 9 {
                return Err(invalid("invalid mouse delta event"));
            }
            let dx = i32::from_be_bytes(payload[1..5].try_into().unwrap());
            let dy = i32::from_be_bytes(payload[5..9].try_into().unwrap());
            Ok(InputEvent::MouseDelta { dx, dy })
        }
        6 if payload.len() == 1 => Ok(InputEvent::MouseLeave),
        6 => Err(invalid("invalid mouse leave event")),
        _ => Err(invalid("unknown input event kind")),
    }
}

pub fn encode_clipboard(payload: &ClipboardPayload) -> Vec<u8> {
    let mut out = Vec::new();
    match payload {
        ClipboardPayload::Text(text) => {
            out.push(1);
            write_bytes(&mut out, text.as_bytes());
        }
        ClipboardPayload::Bitmap(bitmap) => {
            out.push(2);
            write_bytes(&mut out, bitmap);
        }
        ClipboardPayload::Files(files) => {
            out.push(3);
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
        1 => Ok(ClipboardPayload::Text(
            String::from_utf8(read_bytes(payload, &mut offset)?.to_vec())
                .map_err(|e| invalid_owned(e.to_string()))?,
        )),
        2 => Ok(ClipboardPayload::Bitmap(
            read_bytes(payload, &mut offset)?.to_vec(),
        )),
        3 => {
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
    let len = u64::from_be_bytes(payload[offset..offset + 8].try_into().unwrap());
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
    let end = *offset + 4;
    let bytes = payload
        .get(*offset..end)
        .ok_or_else(|| invalid("short u32"))?;
    *offset = end;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
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
    fn v6_hello_decodes_capabilities_without_shifting_screen_dimensions() {
        let hello = decode_hello(&[0, 6, 2, 3, 0, 0, 5, 232, 0, 0, 3, 214]).unwrap();
        assert!(hello.image_clipboard && hello.file_clipboard);
        assert_eq!(hello.screen_width, Some(1512));
        assert_eq!(hello.screen_height, Some(982));
        validate_version(hello).unwrap();
    }

    #[test]
    fn image_frames_can_be_read_without_raising_the_frame_limit() {
        for kind in [13, 14, 15] {
            let bytes = [kind, 0, 0, 0, 0, 0, 0, 0, 0];
            assert!(read_frame(&mut &bytes[..]).is_ok());
        }
    }

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
                image_clipboard: false,
                file_clipboard: false,
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
