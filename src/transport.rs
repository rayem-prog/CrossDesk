// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use crate::secure::SecureStream;
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};

use crate::protocol::{self, Frame, FrameKind};

#[derive(Clone)]
pub struct SharedWriter {
    inner: Arc<WriterInner>,
}

struct WriterInner {
    stream: Mutex<SecureStream>,
    gate: Mutex<WriteGate>,
    ready: Condvar,
}

#[derive(Default)]
struct WriteGate {
    active: bool,
    waiting_input: usize,
}

impl SharedWriter {
    pub fn new(stream: SecureStream) -> Self {
        Self {
            inner: Arc::new(WriterInner {
                stream: Mutex::new(stream),
                gate: Mutex::new(WriteGate::default()),
                ready: Condvar::new(),
            }),
        }
    }

    pub fn write(&self, frame: Frame) -> std::io::Result<()> {
        let input_priority = frame.kind == FrameKind::Input;
        let _permit = self.inner.acquire(input_priority);
        let mut encoded = Vec::with_capacity(9 + frame.payload.len());
        protocol::write_frame_unflushed(&mut encoded, &frame)?;
        self.inner.stream.lock().unwrap().write_all(&encoded)
    }

    pub fn write_input(&self, frame: Frame) -> std::io::Result<()> {
        let _permit = self.inner.acquire(true);
        let mut encoded = Vec::with_capacity(9 + frame.payload.len());
        protocol::write_frame_unflushed(&mut encoded, &frame)?;
        self.inner.stream.lock().unwrap().write_all(&encoded)
    }
}

impl WriterInner {
    fn acquire(&self, input_priority: bool) -> WritePermit<'_> {
        let mut gate = self.gate.lock().unwrap();
        if input_priority {
            gate.waiting_input += 1;
        }
        while gate.active || (!input_priority && gate.waiting_input != 0) {
            gate = self.ready.wait(gate).unwrap();
        }
        if input_priority {
            gate.waiting_input -= 1;
        }
        gate.active = true;
        WritePermit { inner: self }
    }
}

struct WritePermit<'a> {
    inner: &'a WriterInner,
}

impl Drop for WritePermit<'_> {
    fn drop(&mut self) {
        let mut gate = self.inner.gate.lock().unwrap();
        gate.active = false;
        self.inner.ready.notify_all();
    }
}

pub fn clipboard_enabled() -> bool {
    std::env::var("CROSSDESK_ENABLE_CLIPBOARD").is_ok_and(|v| v == "1")
}

pub fn image_clipboard_enabled() -> bool {
    cfg!(any(windows, target_os = "macos"))
        && std::env::var("CROSSDESK_IMAGE_CLIPBOARD").map_or(true, |v| v != "off")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{InputEvent, KeyState};
    use std::net::TcpListener;

    #[test]
    fn concurrent_input_writers_preserve_complete_authenticated_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let peer = std::thread::spawn(move || {
            let mut stream = SecureStream::accept(listener.accept().unwrap().0, &[7; 32]).unwrap();
            let mut keys = std::collections::BTreeSet::new();
            for _ in 0..80 {
                let frame = protocol::read_frame(&mut stream).unwrap();
                let InputEvent::Key {
                    scancode,
                    state: KeyState::Down,
                } = protocol::decode_input(&frame.payload).unwrap()
                else {
                    panic!("invalid input frame");
                };
                keys.insert(scancode);
            }
            assert_eq!(keys, (0u16..80).collect());
        });
        let writer = SharedWriter::new(SecureStream::connect(&address, &[7; 32]).unwrap());
        let threads: Vec<_> = (0..4)
            .map(|thread_index| {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    for number in 0..20 {
                        let event = InputEvent::Key {
                            scancode: thread_index * 20 + number,
                            state: KeyState::Down,
                        };
                        writer
                            .write_input(Frame::new(
                                FrameKind::Input,
                                protocol::encode_input(&event),
                            ))
                            .unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        peer.join().unwrap();
    }
}
