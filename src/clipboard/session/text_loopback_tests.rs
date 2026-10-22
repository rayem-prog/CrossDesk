use super::file_tests::{Memory, streams};
use super::*;
use crate::secure::SecureStream;
use std::net::Shutdown;
use std::sync::mpsc;
use std::time::Instant;

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let mut id = [0; 8];
        getrandom::getrandom(&mut id).unwrap();
        let path =
            std::env::temp_dir().join(format!("crossdesk-text-loop-{:x}", u64::from_le_bytes(id)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Both endpoints use the common clipboard worker and a real encrypted stream.
// Memory is the only substituted boundary: tests never touch the native clipboard.
struct Endpoint {
    memory: Memory,
    mailbox: Arc<Mailbox>,
    writer: SharedWriter,
    socket: SecureStream,
    worker: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    frames: Arc<Mutex<Vec<FrameKind>>>,
    errors: Arc<Mutex<Vec<String>>>,
    results: mpsc::Receiver<([u8; 16], u8)>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.mailbox.stop.store(true, Ordering::Release);
        self.mailbox.changed.notify_all();
        let _ = self.socket.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Endpoint {
    fn new(mut stream: SecureStream, root: PathBuf, memory: Memory, texts: bool) -> Self {
        let mailbox = Arc::new(Mailbox::default());
        mailbox.texts_enabled.store(texts, Ordering::Release);
        let socket = stream.try_clone().unwrap();
        let writer = SharedWriter::new(stream.try_clone().unwrap());
        let target = mailbox.clone();
        let replies = writer.clone();
        let files = FileSession::start(writer.clone(), root, move |batch| {
            enqueue(&target, RemoteDelivery::Files(batch), &replies)
        })
        .unwrap();
        *mailbox.file_sender.lock().unwrap() = Some(files.sender.clone());
        let fake = memory.clone();
        let baseline = fake.change_id().unwrap();
        let target = mailbox.clone();
        let outgoing = writer.clone();
        let worker = thread::spawn(move || worker_loop(fake, baseline, outgoing, target));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let observed = frames.clone();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let failures = errors.clone();
        let probe = memory.clone();
        let target = mailbox.clone();
        let replies = writer.clone();
        let (tx, results) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut image = Incoming::default();
            let mut image_revision = 0;
            while let Ok(frame) = crate::protocol::read_frame(&mut stream) {
                let kind = frame.kind;
                let revision = probe.change_id().unwrap();
                let outcome = if kind == FrameKind::TransferResult {
                    let _ = tx.send(wire::parse_result(&frame.payload).unwrap());
                    Ok(())
                } else if text_transfer::handles(kind) {
                    receive_text(frame, revision, &target, &replies)
                } else if wire::handles(kind) {
                    files.receive(frame, revision)
                } else {
                    if kind == FrameKind::ImageStart {
                        image_revision = revision;
                    }
                    image.receive(frame).map(|completed| {
                        if let Some(transfer) = completed {
                            enqueue(
                                &target,
                                RemoteDelivery::Image(Delivery {
                                    transfer,
                                    revision: image_revision,
                                }),
                                &replies,
                            );
                        }
                    })
                };
                if let Err(error) = outcome {
                    failures.lock().unwrap().push(error.to_string());
                }
                observed.lock().unwrap().push(kind);
            }
        });
        Self {
            memory,
            mailbox,
            writer,
            socket,
            worker: Some(worker),
            reader: Some(reader),
            frames,
            errors,
            results,
        }
    }
    fn copy(&self, payload: ClipboardPayload) {
        let mut memory = self.memory.0.lock().unwrap();
        memory.0 += 1;
        memory.1 = payload;
        drop(memory);
        self.mailbox.changed.notify_all();
    }
    fn result(&self, status: u8) {
        assert_eq!(
            self.results.recv_timeout(Duration::from_secs(5)).unwrap().1,
            status
        );
    }
    fn count(&self, kind: FrameKind) -> usize {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|v| **v == kind)
            .count()
    }
    fn payload(&self) -> ClipboardPayload {
        self.memory.0.lock().unwrap().1.clone()
    }
    fn writes(&self) -> usize {
        self.memory.0.lock().unwrap().2
    }
}
fn memory(text: &str) -> Memory {
    Memory(Arc::new(Mutex::new((
        0,
        ClipboardPayload::Text(text.into()),
        0,
    ))))
}
fn pair(root: &Path, enabled: bool) -> (Endpoint, Endpoint) {
    let (a, b) = streams();
    (
        Endpoint::new(a, root.join("left"), memory("left startup"), enabled),
        Endpoint::new(b, root.join("right"), memory("right startup"), enabled),
    )
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "encrypted clipboard endpoint made no progress"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
fn png(red: u8) -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(1, 1, image::Rgba([red, 2, 3, 255]))
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}
fn frames(text: &str) -> Vec<Frame> {
    let mut frames = Vec::new();
    text_transfer::Transfer::from_text(text)
        .unwrap()
        .visit_frames(|f| {
            frames.push(f);
            Ok(())
        })
        .unwrap();
    frames
}

#[test]
fn encrypted_text_is_bidirectional_bounded_and_explicit_recopy_retries_without_echo() {
    let root = Sandbox::new();
    let (left, right) = pair(&root.0, true);
    thread::sleep(Duration::from_millis(250));
    assert!(left.frames.lock().unwrap().is_empty());
    assert!(
        right.frames.lock().unwrap().is_empty(),
        "startup contents replayed"
    );
    let maximum = format!("{}猫", "x".repeat(text_transfer::MAX_TEXT_BYTES - 3));
    left.copy(ClipboardPayload::Text(maximum.clone()));
    left.result(0);
    assert_eq!(right.payload(), ClipboardPayload::Text(maximum.clone()));
    assert_eq!(right.writes(), 1);
    assert!(right.count(FrameKind::TextChunk) > 1);
    thread::sleep(Duration::from_millis(250));
    assert_eq!(left.count(FrameKind::TextStart), 0, "received text echoed");
    left.copy(ClipboardPayload::Text(maximum));
    left.result(0);
    assert_eq!(
        right.writes(),
        2,
        "same-content explicit recopy was suppressed"
    );
    let unicode = " \t中文🙂\r\ncode();\r\n\n ".repeat(1500);
    right.copy(ClipboardPayload::Text(unicode));
    right.result(0);
    assert_eq!(
        left.payload(),
        ClipboardPayload::Text(" \t中文🙂\ncode();\n\n ".repeat(1500))
    );
    assert_eq!(left.writes(), 1);
    thread::sleep(Duration::from_millis(250));
    assert_eq!(right.count(FrameKind::TextStart), 2, "reverse text echoed");
    assert!(left.errors.lock().unwrap().is_empty());
    assert!(right.errors.lock().unwrap().is_empty());
}

#[test]
fn encrypted_text_completion_cannot_overwrite_a_copy_made_after_start() {
    let root = Sandbox::new();
    let (left, right) = pair(&root.0, true);
    let mut delayed = frames("old remote");
    left.writer.write(delayed.remove(0)).unwrap();
    wait_until(|| right.count(FrameKind::TextStart) == 1);
    right.copy(ClipboardPayload::Text("new local copy".into()));
    right.result(0);
    for frame in delayed {
        left.writer.write(frame).unwrap();
    }
    left.result(1);
    assert_eq!(
        right.payload(),
        ClipboardPayload::Text("new local copy".into())
    );
    assert_eq!(right.writes(), 0);
}

#[test]
fn encrypted_text_image_and_folder_share_copy_lineage_and_reconnect_baseline() {
    let root = Sandbox::new();
    let folder = root.0.join("source-folder");
    std::fs::create_dir_all(folder.join("空目录")).unwrap();
    std::fs::write(folder.join("报告.bin"), b"\0folder\xff").unwrap();
    let (left, right) = pair(&root.0, true);
    left.copy(ClipboardPayload::Text("first text".into()));
    left.result(0);
    left.copy(ClipboardPayload::Bitmap(png(7)));
    left.result(0);
    left.copy(ClipboardPayload::Files(vec![folder]));
    left.result(0);
    let ClipboardPayload::Files(paths) = right.payload() else {
        panic!("folder missing")
    };
    assert!(paths[0].join("空目录").is_dir());
    assert_eq!(
        std::fs::read(paths[0].join("报告.bin")).unwrap(),
        b"\0folder\xff"
    );
    assert_eq!(right.writes(), 3);
    let mut delayed = frames("stale text before local copy");
    left.writer.write(delayed.remove(0)).unwrap();
    wait_until(|| right.count(FrameKind::TextStart) == 2);
    right.copy(ClipboardPayload::Text("local boundary".into()));
    right.result(0);
    left.copy(ClipboardPayload::Bitmap(png(9)));
    left.result(0);
    for frame in delayed {
        left.writer.write(frame).unwrap();
    }
    left.result(1);
    let ClipboardPayload::Bitmap(data) = right.payload() else {
        panic!("stale text replaced image")
    };
    assert_eq!(
        image::load_from_memory(&data)
            .unwrap()
            .into_rgba8()
            .into_raw(),
        vec![9, 2, 3, 255]
    );
    let left_memory = left.memory.clone();
    let right_memory = right.memory.clone();
    drop(left);
    drop(right);
    let (a, b) = streams();
    let left = Endpoint::new(a, root.0.join("reconnected-left"), left_memory, true);
    let right = Endpoint::new(b, root.0.join("reconnected-right"), right_memory, true);
    thread::sleep(Duration::from_millis(300));
    assert!(
        left.frames.lock().unwrap().is_empty(),
        "reconnect replayed existing clipboard"
    );
    assert!(right.frames.lock().unwrap().is_empty());
    left.copy(ClipboardPayload::Text("after reconnect".into()));
    left.result(0);
    assert_eq!(
        right.payload(),
        ClipboardPayload::Text("after reconnect".into())
    );
}

#[test]
fn disabled_text_and_old_capability_bits_emit_no_text_while_files_continue() {
    let root = Sandbox::new();
    let old = crate::protocol::decode_hello(&[0, 7, 2, 3]).unwrap();
    assert!(!old.text_clipboard);
    assert!(old.file_clipboard);
    let (left, right) = pair(&root.0, old.text_clipboard);
    left.copy(ClipboardPayload::Text("disabled text".into()));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(right.count(FrameKind::TextStart), 0);
    assert_eq!(right.writes(), 0);
    let file = root.0.join("ordinary.pdf");
    std::fs::write(&file, b"%PDF test").unwrap();
    left.copy(ClipboardPayload::Files(vec![file]));
    left.result(0);
    let ClipboardPayload::Files(paths) = right.payload() else {
        panic!("files disabled with text")
    };
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"%PDF test");
    assert_eq!(right.count(FrameKind::TextStart), 0);
    assert_eq!(right.count(FrameKind::TextChunk), 0);
    assert_eq!(right.count(FrameKind::TextEnd), 0);
}
