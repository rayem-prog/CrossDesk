use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};

use crate::protocol::{self, Frame, FrameKind};

#[derive(Clone)]
pub struct SharedWriter {
    inner: Arc<WriterInner>,
}

struct WriterInner {
    stream: Mutex<TcpStream>,
    gate: Mutex<WriteGate>,
    ready: Condvar,
}

#[derive(Default)]
struct WriteGate {
    active: bool,
    waiting_input_frames: usize,
}

impl SharedWriter {
    pub fn new(stream: TcpStream) -> Self {
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
        protocol::write_frame(&mut *self.inner.stream.lock().unwrap(), &frame)
    }

    pub fn write_input(&self, frame: Frame) -> std::io::Result<()> {
        let _permit = self.inner.acquire(true);
        protocol::write_frame_unflushed(&mut *self.inner.stream.lock().unwrap(), &frame)
    }
}

impl WriterInner {
    fn acquire(&self, input_priority: bool) -> WritePermit<'_> {
        let mut gate = self.gate.lock().unwrap();
        if input_priority {
            gate.waiting_input_frames += 1;
        }
        // Clipboard and file frames can be large, so input frames get a small
        // priority lane to keep mouse/keyboard latency predictable.
        while gate.active || (!input_priority && gate.waiting_input_frames != 0) {
            gate = self.ready.wait(gate).unwrap();
        }
        if input_priority {
            gate.waiting_input_frames -= 1;
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
