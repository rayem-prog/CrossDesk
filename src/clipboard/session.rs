use super::{Clipboard, ClipboardApi};
use crate::image_transfer::{self, Incoming, Transfer};
use crate::protocol::{ClipboardPayload, Frame, FrameKind};
use crate::transport::SharedWriter;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

#[derive(Default)]
struct SyncState {
    sent: Option<[u8; 32]>,
    remote: Vec<[u8; 32]>,
}
impl SyncState {
    fn should_send(&self, value: [u8; 32]) -> bool {
        self.sent != Some(value) && !self.remote.contains(&value)
    }
    fn note_sent(&mut self, value: [u8; 32]) {
        self.sent = Some(value);
        self.remote.clear();
    }
    fn note_remote(&mut self, value: [u8; 32]) {
        self.sent = None;
        self.remote = vec![value];
    }
}
fn may_publish_remote(start: u64, current: u64, last_remote: Option<u64>) -> bool {
    start == current || last_remote == Some(current)
}

struct Delivery {
    transfer: Transfer,
    revision: u64,
}
#[derive(Default)]
struct Mailbox {
    pending: Mutex<Option<Delivery>>,
    changed: Condvar,
    stop: AtomicBool,
}

pub struct ImageSession {
    enabled: bool,
    incoming: Incoming,
    revision: u64,
    probe: Option<Clipboard>,
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
}

impl ImageSession {
    pub fn start(writer: SharedWriter, enabled: bool) -> io::Result<Self> {
        let mailbox = Arc::new(Mailbox::default());
        let (probe, worker) = if enabled {
            let clipboard = Clipboard::new()?;
            let probe = Clipboard::new()?;
            let baseline = clipboard.change_id()?;
            let worker_mailbox = Arc::clone(&mailbox);
            let worker = thread::Builder::new()
                .name("image-clipboard".into())
                .spawn(move || {
                    worker_loop(clipboard, baseline, writer, worker_mailbox);
                })?;
            (Some(probe), Some(worker))
        } else {
            (None, None)
        };
        eprintln!(
            "image clipboard: {}",
            if enabled {
                "on (PNG/JPG files and image content)"
            } else {
                "off (disabled by one of the peers)"
            }
        );
        Ok(Self {
            enabled,
            incoming: Incoming::default(),
            revision: 0,
            probe,
            mailbox,
            worker,
        })
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn handles(kind: FrameKind) -> bool {
        matches!(
            kind,
            FrameKind::ImageStart | FrameKind::ImageChunk | FrameKind::ImageEnd
        )
    }
    pub fn receive(&mut self, frame: Frame) -> io::Result<()> {
        if !self.enabled {
            return Err(image_transfer::invalid(
                "image synchronization was not negotiated",
            ));
        }
        self.incoming.expire();
        if frame.kind == FrameKind::ImageStart {
            self.revision = self.probe.as_ref().unwrap().change_id()?;
        }
        if let Some(transfer) = self.incoming.receive(frame)? {
            // The newest complete transfer replaces one that has not been applied.
            *self.mailbox.pending.lock().unwrap() = Some(Delivery {
                transfer,
                revision: self.revision,
            });
            self.mailbox.changed.notify_one();
        }
        Ok(())
    }
}

impl Drop for ImageSession {
    fn drop(&mut self) {
        self.mailbox.stop.store(true, Ordering::Release);
        self.mailbox.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop<C: ClipboardApi>(
    mut clipboard: C,
    mut revision: u64,
    writer: SharedWriter,
    mailbox: Arc<Mailbox>,
) {
    let mut state = SyncState::default();
    let mut last_remote_revision = None;
    let cache = std::env::temp_dir().join("crossdesk-image-clipboard");
    // Read only to protect an existing file clipboard from cache cleanup; never send this baseline.
    let protected = match clipboard.read() {
        Ok(Some(ClipboardPayload::Files(files))) => files,
        _ => Vec::new(),
    };
    cleanup_cache(&cache, &protected);
    while !mailbox.stop.load(Ordering::Acquire) && !crate::shutdown::requested() {
        let incoming = mailbox.pending.lock().unwrap().take();
        if let Some(delivery) = incoming {
            match apply_delivery(
                &mut clipboard,
                delivery,
                &cache,
                last_remote_revision,
                &mut state,
            ) {
                Ok(Some(next_revision)) => {
                    revision = next_revision;
                    last_remote_revision = Some(next_revision);
                }
                Ok(None) => {}
                Err(error) => eprintln!("image clipboard receive skipped: {error}"),
            }
        }
        match clipboard.change_id() {
            Ok(current) if current != revision => {
                match clipboard.read() {
                    Ok(Some(payload)) => {
                        // Recheck after a native read, which may span several system calls.
                        if clipboard.change_id().ok() != Some(current) {
                            continue;
                        }
                        revision = current;
                        if matches!(payload, ClipboardPayload::Text(_)) {
                            state = SyncState::default();
                            continue;
                        }
                        match Transfer::from_clipboard(&payload).and_then(|transfer| {
                            let fingerprint = transfer.fingerprint()?;
                            if !state.should_send(fingerprint) {
                                return Ok(());
                            }
                            let count = transfer.images.len();
                            let bytes: usize = transfer.images.iter().map(|v| v.data.len()).sum();
                            // Do not send an older copy that changed while image conversion ran.
                            if clipboard.change_id()? != current {
                                return Ok(());
                            }
                            transfer.visit_frames(|frame| {
                                if mailbox.stop.load(Ordering::Acquire) {
                                    return Err(io::Error::new(
                                        io::ErrorKind::Interrupted,
                                        "image session stopped",
                                    ));
                                }
                                writer.write(frame)
                            })?;
                            state.note_sent(fingerprint);
                            eprintln!("image clipboard sent: {count} image(s), {bytes} bytes");
                            Ok(())
                        }) {
                            Ok(()) => {}
                            Err(error) => {
                                eprintln!("image clipboard send skipped: {error}");
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::BrokenPipe
                                        | io::ErrorKind::ConnectionReset
                                        | io::ErrorKind::TimedOut
                                        | io::ErrorKind::WouldBlock
                                        | io::ErrorKind::Interrupted
                                ) {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        revision = current;
                        state = SyncState::default();
                    }
                    Err(error) => {
                        eprintln!("image clipboard read failed (will retry): {error}");
                    }
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("image clipboard revision read failed: {error}"),
        }
        let guard = mailbox.pending.lock().unwrap();
        if guard.is_none() && !mailbox.stop.load(Ordering::Acquire) {
            let _ = mailbox
                .changed
                .wait_timeout(guard, Duration::from_millis(200));
        }
    }
    eprintln!("image clipboard worker stopped");
}

fn apply_delivery<C: ClipboardApi>(
    clipboard: &mut C,
    delivery: Delivery,
    root: &Path,
    last_remote: Option<u64>,
    state: &mut SyncState,
) -> io::Result<Option<u64>> {
    let current = clipboard.change_id()?;
    if !may_publish_remote(delivery.revision, current, last_remote) {
        eprintln!("image clipboard receive skipped: a newer local copy exists");
        return Ok(None);
    }
    let fingerprint = delivery.transfer.fingerprint()?;
    let count = delivery.transfer.images.len();
    let payload = delivery.transfer.into_payload(root)?;
    // Decoding and writing file caches may take time; check again before native publication.
    if !may_publish_remote(delivery.revision, clipboard.change_id()?, last_remote) {
        remove_unpublished(&payload, root);
        eprintln!("image clipboard receive skipped: local clipboard changed during decoding");
        return Ok(None);
    }
    let mut result = None;
    for _ in 0..3 {
        let current = clipboard.change_id()?;
        if !may_publish_remote(delivery.revision, current, last_remote) {
            remove_unpublished(&payload, root);
            return Ok(None);
        }
        match clipboard.write_if_current(&payload, current) {
            Ok(true) => {
                result = Some(Ok(()));
                break;
            }
            Ok(false) => {
                remove_unpublished(&payload, root);
                return Ok(None);
            }
            Err(error) => {
                result = Some(Err(error));
                thread::sleep(Duration::from_millis(50));
            }
        }
        if !may_publish_remote(delivery.revision, clipboard.change_id()?, last_remote) {
            break;
        }
    }
    if let Err(error) = result.unwrap() {
        remove_unpublished(&payload, root);
        return Err(error);
    }
    let published_revision = clipboard.change_id()?;
    state.note_remote(fingerprint);
    if let Ok(Some(readback)) = clipboard.read() {
        if clipboard.change_id()? == published_revision {
            if let Ok(transfer) = Transfer::from_clipboard(&readback) {
                if let Ok(fingerprint) = transfer.fingerprint() {
                    state.remote.push(fingerprint);
                }
            }
        }
    }
    eprintln!("image clipboard received: {count} image(s); ready to paste");
    Ok(Some(published_revision))
}

fn remove_unpublished(payload: &ClipboardPayload, root: &Path) {
    if let ClipboardPayload::Files(files) = payload {
        if let Some(parent) = files.first().and_then(|p| p.parent()) {
            if parent.parent() == Some(root) {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }
}

fn cleanup_cache(root: &Path, protected: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() != 32 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let directory = entry.path().canonicalize().unwrap_or_else(|_| entry.path());
        if protected.iter().any(|p| {
            p.canonicalize()
                .unwrap_or_else(|_| p.clone())
                .starts_with(&directory)
        }) {
            continue;
        }
        let old_directory = entry.file_type().is_ok_and(|t| t.is_dir())
            && entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > Duration::from_secs(86400));
        if old_directory {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_echo_is_suppressed_without_swallowing_the_next_local_image() {
        let mut state = SyncState::default();
        state.note_remote([1; 32]);
        assert!(!state.should_send([1; 32]));
        assert!(state.should_send([2; 32]));
        state.note_sent([2; 32]);
        assert!(!state.should_send([2; 32]));
        state.note_remote([3; 32]);
        assert!(state.should_send([2; 32]));
    }

    #[test]
    fn a_newer_local_copy_is_not_overwritten_by_an_older_remote_completion() {
        assert!(may_publish_remote(10, 10, None));
        assert!(!may_publish_remote(10, 11, None));
        assert!(!may_publish_remote(10, 12, Some(11)));
        assert!(may_publish_remote(10, 11, Some(11)));
    }

    #[derive(Clone)]
    struct MemoryClipboard(Arc<Mutex<(u64, ClipboardPayload, usize)>>);
    impl ClipboardApi for MemoryClipboard {
        fn change_id(&self) -> io::Result<u64> {
            Ok(self.0.lock().unwrap().0)
        }
        fn read(&mut self) -> io::Result<Option<ClipboardPayload>> {
            Ok(Some(self.0.lock().unwrap().1.clone()))
        }
        fn write(&mut self, payload: &ClipboardPayload) -> io::Result<()> {
            let mut state = self.0.lock().unwrap();
            state.0 += 1;
            state.1 = payload.clone();
            state.2 += 1;
            Ok(())
        }
    }
    fn png(red: u8) -> Vec<u8> {
        let pixels = image::RgbaImage::from_raw(1, 1, vec![red, 2, 3, 127]).unwrap();
        let mut bytes = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn stale_or_invalid_received_images_leave_the_real_worker_clipboard_unchanged() {
        let memory = Arc::new(Mutex::new((11, ClipboardPayload::Bitmap(png(9)), 0)));
        let mut clipboard = MemoryClipboard(memory.clone());
        let mut state = SyncState::default();
        let transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(png(5))).unwrap();
        let root = std::env::temp_dir().join("crossdesk-unused-stale-test");
        assert!(
            apply_delivery(
                &mut clipboard,
                Delivery {
                    transfer,
                    revision: 10
                },
                &root,
                None,
                &mut state
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(memory.lock().unwrap().2, 0);
        let mut transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(png(5))).unwrap();
        transfer.images[0].data = vec![1, 2, 3];
        assert!(
            apply_delivery(
                &mut clipboard,
                Delivery {
                    transfer,
                    revision: 11
                },
                &root,
                None,
                &mut state
            )
            .is_err()
        );
        let content = memory.lock().unwrap();
        assert_eq!(content.2, 0);
        let ClipboardPayload::Bitmap(bytes) = &content.1 else {
            panic!("local image was replaced");
        };
        assert_eq!(
            image::load_from_memory(bytes)
                .unwrap()
                .into_rgba8()
                .into_raw(),
            vec![9, 2, 3, 127]
        );
    }

    #[test]
    fn worker_skips_startup_content_suppresses_echo_and_stops_without_leaking() {
        use crate::protocol;
        use crate::secure::SecureStream;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let accept = thread::spawn(move || {
            SecureStream::accept(listener.accept().unwrap().0, &[44; 32]).unwrap()
        });
        let client = SecureStream::connect(&address, &[44; 32]).unwrap();
        let mut peer = accept.join().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let memory = Arc::new(Mutex::new((0, ClipboardPayload::Bitmap(png(99)), 0)));
        let mailbox = Arc::new(Mailbox::default());
        let worker_mailbox = mailbox.clone();
        let fake = MemoryClipboard(memory.clone());
        let writer = SharedWriter::new(client.try_clone().unwrap());
        let worker = thread::spawn(move || worker_loop(fake, 0, writer, worker_mailbox));
        assert!(
            protocol::read_frame(&mut peer).is_err(),
            "startup clipboard was transmitted"
        );
        {
            let mut state = memory.lock().unwrap();
            state.0 = 1;
            state.1 = ClipboardPayload::Bitmap(png(7));
        }
        mailbox.changed.notify_all();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        fn receive(peer: &mut SecureStream) -> Transfer {
            let mut incoming = Incoming::default();
            loop {
                if let Some(done) = incoming
                    .receive(protocol::read_frame(peer).unwrap())
                    .unwrap()
                {
                    return done;
                }
            }
        }
        assert_eq!(
            image::load_from_memory(&receive(&mut peer).images[0].data)
                .unwrap()
                .into_rgba8()
                .into_raw(),
            vec![7, 2, 3, 127]
        );
        *mailbox.pending.lock().unwrap() = Some(Delivery {
            transfer: Transfer::from_clipboard(&ClipboardPayload::Bitmap(png(8))).unwrap(),
            revision: 1,
        });
        mailbox.changed.notify_all();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while memory.lock().unwrap().2 == 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(memory.lock().unwrap().2, 1);
        peer.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        assert!(
            protocol::read_frame(&mut peer).is_err(),
            "received image was echoed"
        );
        {
            let mut state = memory.lock().unwrap();
            state.0 += 1;
            state.1 = ClipboardPayload::Bitmap(png(9));
        }
        mailbox.changed.notify_all();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(
            image::load_from_memory(&receive(&mut peer).images[0].data)
                .unwrap()
                .into_rgba8()
                .into_raw(),
            vec![9, 2, 3, 127]
        );
        mailbox.stop.store(true, Ordering::Release);
        mailbox.changed.notify_all();
        worker.join().unwrap();
    }
}
