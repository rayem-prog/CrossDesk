use super::*;
use crate::file_transfer::session::Receiver as FileReceiver;
use crate::file_transfer::wire::{Entry, Offer};
use crate::secure::SecureStream;
use sha2::{Digest, Sha256};
use std::net::{Shutdown, TcpListener};
use std::sync::mpsc;
#[derive(Clone)]
pub(super) struct Memory(pub(super) Arc<Mutex<(u64, ClipboardPayload, usize)>>);
impl ClipboardApi for Memory {
    fn change_id(&self) -> io::Result<u64> {
        Ok(self.0.lock().unwrap().0)
    }
    fn read(&mut self) -> io::Result<Option<ClipboardPayload>> {
        Ok(Some(self.0.lock().unwrap().1.clone()))
    }
    fn write(&mut self, p: &ClipboardPayload) -> io::Result<()> {
        let mut s = self.0.lock().unwrap();
        s.0 += 1;
        s.1 = p.clone();
        s.2 += 1;
        Ok(())
    }
}
fn root() -> PathBuf {
    let mut id = [0; 8];
    getrandom::getrandom(&mut id).unwrap();
    std::env::temp_dir().join(format!("crossdesk-observer-{:x}", u64::from_le_bytes(id)))
}
fn empty_batch(root: PathBuf, id: [u8; 16]) -> CompletedBatch {
    let mut receiver = FileReceiver::new(root);
    receiver
        .handle(
            Offer {
                id,
                entries: vec![Entry {
                    parent: None,
                    directory: false,
                    name: "empty.pdf".into(),
                    len: 0,
                }],
            }
            .frame(),
            0,
        )
        .unwrap();
    receiver
        .handle(
            wire::request(FrameKind::FileSeal, id, 1, 0, 0, &Sha256::digest(b"")),
            0,
        )
        .unwrap();
    receiver
        .handle(wire::request(FrameKind::FileCommit, id, 2, 0, 0, &[]), 0)
        .unwrap()
        .unwrap()
}
pub(super) fn streams() -> (SecureStream, SecureStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = thread::spawn(move || {
        SecureStream::accept(listener.accept().unwrap().0, &[66; 32]).unwrap()
    });
    let client = SecureStream::connect(&addr, &[66; 32]).unwrap();
    (client, server.join().unwrap())
}
struct Observer {
    memory: Memory,
    mailbox: Arc<Mailbox>,
    socket: SecureStream,
    worker: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    results: mpsc::Receiver<u8>,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.mailbox.stop.store(true, Ordering::Release);
        self.mailbox.changed.notify_all();
        let _ = self.socket.shutdown(Shutdown::Both);
        if let Some(t) = self.reader.take() {
            t.join().unwrap();
        }
        if let Some(t) = self.worker.take() {
            t.join().unwrap();
        }
    }
}
fn observer(mut stream: SecureStream, root: PathBuf) -> Observer {
    let memory = Memory(Arc::new(Mutex::new((
        0,
        ClipboardPayload::Text(String::new()),
        0,
    ))));
    let mailbox = Arc::new(Mailbox::default());
    mailbox.images_enabled.store(false, Ordering::Release);
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
    let box2 = mailbox.clone();
    let worker = thread::spawn(move || worker_loop(fake, 0, writer, box2));
    let probe = memory.clone();
    let (tx, results) = mpsc::channel();
    let reader = thread::spawn(move || {
        while let Ok(frame) = crate::protocol::read_frame(&mut stream) {
            if frame.kind == FrameKind::TransferResult {
                tx.send(wire::parse_result(&frame.payload).unwrap().1)
                    .unwrap();
            } else {
                let revision = probe.change_id().unwrap();
                files.receive(frame, revision).unwrap();
            }
        }
    });
    Observer {
        memory,
        mailbox,
        socket,
        worker: Some(worker),
        reader: Some(reader),
        results,
    }
}
#[test]
fn file_only_observers_publish_mixed_files_once_and_explicit_recopy_retries() {
    let root = root();
    std::fs::create_dir(&root).unwrap();
    let pdf = root.join("报告.pdf");
    let zip = root.join("archive.zip");
    std::fs::write(&pdf, b"%PDF\0\xff").unwrap();
    std::fs::write(&zip, b"PK\x03\x04binary").unwrap();
    let (a, b) = streams();
    let left = observer(a, root.join("left"));
    let right = observer(b, root.join("right"));
    {
        let mut m = left.memory.0.lock().unwrap();
        m.0 = 1;
        m.1 = ClipboardPayload::Files(vec![pdf, zip]);
    }
    left.mailbox.changed.notify_all();
    assert_eq!(
        left.results.recv_timeout(Duration::from_secs(3)).unwrap(),
        0
    );
    {
        let m = right.memory.0.lock().unwrap();
        assert_eq!(m.2, 1);
        let ClipboardPayload::Files(files) = &m.1 else {
            panic!("files missing")
        };
        assert_eq!(files.len(), 2);
        assert_eq!(std::fs::read(&files[0]).unwrap(), b"%PDF\0\xff");
        assert_eq!(std::fs::read(&files[1]).unwrap(), b"PK\x03\x04binary");
    }
    assert!(
        right
            .results
            .recv_timeout(Duration::from_millis(300))
            .is_err(),
        "remote file publication echoed"
    );
    {
        left.memory.0.lock().unwrap().0 += 1;
    }
    left.mailbox.changed.notify_all();
    assert_eq!(
        left.results.recv_timeout(Duration::from_secs(3)).unwrap(),
        0
    );
    assert_eq!(right.memory.0.lock().unwrap().2, 2);
    drop(left);
    drop(right);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn received_file_batch_cannot_replace_a_newer_local_copy() {
    let root = root();
    let mut batch = empty_batch(root.clone(), [5; 16]);
    let files = batch.files.clone();
    let mut clipboard = Memory(Arc::new(Mutex::new((
        1,
        ClipboardPayload::Text("new local".into()),
        0,
    ))));
    assert!(
        apply_file_delivery(&mut clipboard, &mut batch, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(clipboard.0.lock().unwrap().2, 0);
    drop(batch);
    assert!(!files[0].exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_observers_publish_roots_without_echo_and_explicit_recopy_retries() {
    let root = root();
    let folder = root.join("资料");
    std::fs::create_dir_all(folder.join("空目录")).unwrap();
    std::fs::write(folder.join(".hidden"), b"folder data").unwrap();
    let (a, b) = streams();
    let left = observer(a, root.join("left"));
    let right = observer(b, root.join("right"));
    {
        let mut m = left.memory.0.lock().unwrap();
        m.0 += 1;
        m.1 = ClipboardPayload::Files(vec![folder]);
    }
    for expected_writes in 1..=2 {
        left.mailbox.changed.notify_all();
        assert_eq!(
            left.results.recv_timeout(Duration::from_secs(3)).unwrap(),
            0
        );
        {
            let m = right.memory.0.lock().unwrap();
            let ClipboardPayload::Files(paths) = &m.1 else {
                panic!("missing folder")
            };
            assert_eq!(paths.len(), 1);
            assert!(paths[0].join("空目录").is_dir());
            assert_eq!(
                std::fs::read(paths[0].join(".hidden")).unwrap(),
                b"folder data"
            );
            assert_eq!(m.2, expected_writes);
        }
        assert!(
            right
                .results
                .recv_timeout(Duration::from_millis(150))
                .is_err(),
            "remote folder echoed"
        );
        left.memory.0.lock().unwrap().0 += 1;
    }
    drop(left);
    drop(right);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn latest_remote_delivery_replaces_older_files_across_content_kinds() {
    let root = root();
    let batch = empty_batch(root.clone(), [7; 16]);
    let old_path = batch.files[0].clone();
    let (stream, mut peer) = streams();
    let writer = SharedWriter::new(stream);
    let mailbox = Mailbox::default();
    enqueue(&mailbox, RemoteDelivery::Files(batch), &writer);
    let pixels = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
    let mut data = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut data, image::ImageFormat::Png).unwrap();
    let transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(data.into_inner())).unwrap();
    enqueue(
        &mailbox,
        RemoteDelivery::Image(Delivery {
            transfer,
            revision: 0,
        }),
        &writer,
    );
    assert!(matches!(
        *mailbox.pending.lock().unwrap(),
        Some(RemoteDelivery::Image(_))
    ));
    assert!(!old_path.exists());
    let status = crate::protocol::read_frame(&mut peer).unwrap();
    assert_eq!(wire::parse_result(&status.payload).unwrap(), ([7; 16], 1));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_completed_files_cannot_cross_a_local_copy_boundary_after_an_image_publishes() {
    let cache = root();
    let old = empty_batch(cache.clone(), [29; 16]); // Started before local copy.
    let (stream, mut peer) = streams();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let writer = SharedWriter::new(stream);
    let mailbox = Arc::new(Mailbox::default());
    let memory = Memory(Arc::new(Mutex::new((
        1,
        ClipboardPayload::Text("new local".into()),
        0,
    ))));
    let fake = memory.clone();
    let target = mailbox.clone();
    let replies = writer.clone();
    let worker = thread::spawn(move || worker_loop(fake, 1, writer, target));
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(1, 1, image::Rgba([9, 2, 3, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let image = Transfer::from_clipboard(&ClipboardPayload::Bitmap(png.into_inner())).unwrap();
    enqueue(
        &mailbox,
        RemoteDelivery::Image(Delivery {
            transfer: image,
            revision: 1,
        }),
        &replies,
    );
    let first = crate::protocol::read_frame(&mut peer).unwrap();
    assert_eq!(wire::parse_result(&first.payload).unwrap().1, 0);
    enqueue(&mailbox, RemoteDelivery::Files(old), &replies);
    let second = crate::protocol::read_frame(&mut peer).unwrap();
    let status = wire::parse_result(&second.payload).unwrap().1;
    let state = memory.0.lock().unwrap().clone();
    mailbox.stop.store(true, Ordering::Release);
    mailbox.changed.notify_all();
    worker.join().unwrap();
    std::fs::remove_dir_all(cache).unwrap();
    assert_eq!(
        status, 1,
        "an older copy must be skipped after a new local copy"
    );
    assert_eq!(state.0, 2);
    assert!(matches!(state.1, ClipboardPayload::Bitmap(_)));
}
