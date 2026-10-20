// CrossDesk file clipboard sessions: 2026-10-20 (GPL-3.0-only).
use super::wire::{self, Ack, Entry, Id, Offer, Reader};
use super::{IncomingBundle, MAX_CACHE_BYTES, invalid, sources};
use crate::protocol::{CHUNK_SIZE, Frame, FrameKind};
use crate::transport::SharedWriter;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct CompletedBatch {
    pub id: Id,
    pub revision: u64,
    pub files: Vec<PathBuf>,
    retained: bool,
}
impl CompletedBatch {
    pub fn retain(&mut self) {
        self.retained = true;
    }
}
impl Drop for CompletedBatch {
    fn drop(&mut self) {
        if !self.retained {
            if let Some(parent) = self.files.first().and_then(|p| p.parent()) {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }
}
struct Active {
    offer: Offer,
    serial: u64,
    index: usize,
    offset: u64,
    hash: Sha256,
    bundle: IncomingBundle,
    revision: u64,
    progress: Instant,
}
pub struct Receiver {
    root: PathBuf,
    cache_budget: u64,
    active: Option<Active>,
}
impl Receiver {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: None,
            cache_budget: MAX_CACHE_BYTES,
        }
    }
    pub fn expire(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.progress.elapsed() > TIMEOUT)
        {
            self.active = None;
            eprintln!("file clipboard receive timed out; partial files removed");
        }
    }
    pub fn handle(&mut self, frame: Frame, revision: u64) -> io::Result<Option<CompletedBatch>> {
        let identity = wire::identity(&frame)?;
        if frame.kind == FrameKind::FileCancel {
            if frame.payload.len() != 16 {
                return Err(invalid("invalid file cancellation"));
            }
            if self
                .active
                .as_ref()
                .is_some_and(|a| a.offer.id == identity.0)
            {
                self.active = None;
            }
            return Ok(None);
        }
        let result = self.handle_inner(frame, revision);
        if result.is_err()
            && self
                .active
                .as_ref()
                .is_some_and(|a| a.offer.id == identity.0)
        {
            self.active = None;
        }
        result
    }
    fn handle_inner(&mut self, frame: Frame, revision: u64) -> io::Result<Option<CompletedBatch>> {
        if frame.kind == FrameKind::FileOffer {
            if self.active.is_some() {
                return Err(invalid("file batch already in progress"));
            }
            let offer = Offer::parse(&frame.payload)?;
            if cache_size(&self.root)?.saturating_add(offer.total()) > self.cache_budget {
                return Err(invalid(
                    "file cache budget reached; clear unreferenced caches before retrying",
                ));
            }
            let mut bundle = IncomingBundle::new(self.root.clone());
            bundle.start_file(&offer.entries[0].name, offer.entries[0].len)?;
            self.active = Some(Active {
                offer,
                serial: 1,
                index: 0,
                offset: 0,
                hash: Sha256::new(),
                bundle,
                revision,
                progress: Instant::now(),
            });
            return Ok(None);
        }
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| invalid("file data without offer"))?;
        let mut r = Reader::new(&frame.payload);
        let id = r.id()?;
        let serial = r.u64()?;
        if id != active.offer.id || serial != active.serial {
            return Err(invalid("file batch ID or serial mismatch"));
        }
        match frame.kind {
            FrameKind::FileData => {
                let index = r.u16()? as usize;
                let offset = r.u64()?;
                let data = r.rest();
                if index != active.index
                    || offset != active.offset
                    || data.is_empty()
                    || data.len() > CHUNK_SIZE
                    || active.index >= active.offer.entries.len()
                {
                    return Err(invalid("file index, offset or chunk size mismatch"));
                }
                active.bundle.write_chunk(data)?;
                active.hash.update(data);
                active.offset += data.len() as u64;
            }
            FrameKind::FileSeal => {
                let index = r.u16()? as usize;
                let digest = r.take(32)?;
                r.end()?;
                if index != active.index
                    || index >= active.offer.entries.len()
                    || active.offset != active.offer.entries[index].len
                    || active.hash.clone().finalize().as_slice() != digest
                {
                    return Err(invalid("file length or SHA-256 mismatch"));
                }
                active.index += 1;
                active.offset = 0;
                active.hash = Sha256::new();
                if let Some(next) = active.offer.entries.get(active.index) {
                    active.bundle.start_file(&next.name, next.len)?;
                }
            }
            FrameKind::FileCommit => {
                r.end()?;
                if active.index != active.offer.entries.len() {
                    return Err(invalid("file batch is incomplete"));
                }
                let mut completed = self.active.take().unwrap();
                let files = completed.bundle.finish()?;
                return Ok(Some(CompletedBatch {
                    id: completed.offer.id,
                    revision: completed.revision,
                    files,
                    retained: false,
                }));
            }
            _ => return Err(invalid("unexpected file request")),
        }
        active.serial += 1;
        active.progress = Instant::now();
        Ok(None)
    }
}
fn cache_size(root: &Path) -> io::Result<u64> {
    let dirs = match std::fs::read_dir(root) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut total = 0u64;
    for entry in dirs {
        let entry = entry?;
        let m = entry.file_type()?;
        if m.is_symlink() {
            return Err(invalid("unexpected link in private file cache"));
        }
        if m.is_dir() {
            for f in std::fs::read_dir(entry.path())? {
                let f = f?;
                let m = f.metadata()?;
                total = total.saturating_add(m.len());
            }
        } else {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}
#[derive(Default)]
struct ReplyState {
    expected: Option<(Id, u64)>,
    ack: Option<Ack>,
}
struct Shared {
    replies: Mutex<ReplyState>,
    changed: Condvar,
    stop: AtomicBool,
    send_lock: Mutex<()>,
}
#[derive(Clone)]
pub struct FileSender {
    shared: Arc<Shared>,
    writer: SharedWriter,
}
impl FileSender {
    fn note_ack(&self, ack: Ack) {
        let mut r = self.shared.replies.lock().unwrap();
        if r.expected == Some((ack.id, ack.serial)) {
            r.ack = Some(ack);
            self.shared.changed.notify_all();
        }
    }
    fn exchange(&self, frame: Frame, cancel: &mut impl FnMut() -> bool) -> io::Result<()> {
        if self.shared.stop.load(Ordering::Acquire) || crate::shutdown::requested() || cancel() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "file session stopped or copy superseded",
            ));
        }
        let (id, serial) = wire::identity(&frame)?;
        {
            let mut r = self.shared.replies.lock().unwrap();
            r.expected = Some((id, serial));
            r.ack = None;
        }
        self.writer.write(frame)?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if self.shared.stop.load(Ordering::Acquire) || crate::shutdown::requested() || cancel()
            {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "file copy superseded or session stopped",
                ));
            }
            let mut r = self.shared.replies.lock().unwrap();
            if let Some(ack) = r.ack.take() {
                r.expected = None;
                if ack.status == 2 {
                    return Err(invalid(
                        "destination rejected this file batch; see destination log",
                    ));
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                r.expected = None;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "file acknowledgment timed out",
                ));
            }
            let _guard = self
                .shared
                .changed
                .wait_timeout(r, Duration::from_millis(100))
                .unwrap();
        }
    }
    pub fn send(&self, files: &[PathBuf], mut cancel: impl FnMut() -> bool) -> io::Result<Id> {
        let _single = self.shared.send_lock.lock().unwrap();
        if self.shared.stop.load(Ordering::Acquire) || crate::shutdown::requested() || cancel() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "file session stopped or copy superseded",
            ));
        }
        let mut sources = sources(files)?;
        let mut id = [0; 16];
        getrandom::getrandom(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        let offer = Offer {
            id,
            entries: sources
                .iter()
                .map(|s| Entry {
                    name: s.name.clone(),
                    len: s.len,
                })
                .collect(),
        };
        let result = (|| {
            self.exchange(offer.frame(), &mut cancel)?;
            let mut serial = 1;
            let mut buffer = vec![0; CHUNK_SIZE];
            for (index, source) in sources.iter_mut().enumerate() {
                let mut offset = 0;
                let mut hash = Sha256::new();
                while offset < source.len {
                    let length = (source.len - offset).min(CHUNK_SIZE as u64) as usize;
                    let n = source.file.read(&mut buffer[..length])?;
                    if n == 0 {
                        return Err(invalid("source file truncated; copy again"));
                    }
                    hash.update(&buffer[..n]);
                    self.exchange(
                        wire::request(
                            FrameKind::FileData,
                            id,
                            serial,
                            index as u16,
                            offset,
                            &buffer[..n],
                        ),
                        &mut cancel,
                    )?;
                    serial += 1;
                    offset += n as u64;
                }
                source.unchanged()?;
                self.exchange(
                    wire::request(
                        FrameKind::FileSeal,
                        id,
                        serial,
                        index as u16,
                        0,
                        &hash.finalize(),
                    ),
                    &mut cancel,
                )?;
                serial += 1;
            }
            for source in &sources {
                source.unchanged()?;
            }
            self.exchange(
                wire::request(FrameKind::FileCommit, id, serial, 0, 0, &[]),
                &mut cancel,
            )?;
            eprintln!(
                "file clipboard transmitted: {} file(s), {} bytes; waiting for destination publication",
                offer.entries.len(),
                offer.total()
            );
            Ok(id)
        })();
        if result.is_err()
            && !self.shared.stop.load(Ordering::Acquire)
            && !crate::shutdown::requested()
        {
            let _ = self
                .writer
                .write(Frame::new(FrameKind::FileCancel, id.to_vec()));
        }
        {
            let mut r = self.shared.replies.lock().unwrap();
            r.expected = None;
            r.ack = None;
        }
        result
    }
}
struct Command {
    frame: Frame,
    revision: u64,
}
pub struct FileSession {
    pub sender: FileSender,
    inbound: Option<mpsc::SyncSender<Command>>,
    worker: Option<JoinHandle<()>>,
}
impl FileSession {
    pub fn start(
        writer: SharedWriter,
        root: PathBuf,
        mut completed: impl FnMut(CompletedBatch) + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            replies: Mutex::new(ReplyState::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            send_lock: Mutex::new(()),
        });
        let sender = FileSender {
            shared: shared.clone(),
            writer: writer.clone(),
        };
        let (tx, rx) = mpsc::sync_channel::<Command>(8);
        let worker = thread::Builder::new()
            .name("file-receiver".into())
            .spawn(move || {
                let mut receiver = Receiver::new(root);
                while !shared.stop.load(Ordering::Acquire) && !crate::shutdown::requested() {
                    receiver.expire();
                    let command = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(v) => v,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    let identity = match wire::identity(&command.frame) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("file frame rejected: {e}");
                            continue;
                        }
                    };
                    let cancelled = command.frame.kind == FrameKind::FileCancel;
                    let result = receiver.handle(command.frame, command.revision);
                    if cancelled {
                        continue;
                    }
                    let status = match &result {
                        Ok(Some(_)) => 1,
                        Ok(None) => 0,
                        Err(_) => 2,
                    };
                    if writer
                        .write(wire::ack(identity.0, identity.1, status))
                        .is_err()
                    {
                        break;
                    }
                    match result {
                        Ok(Some(batch)) => completed(batch),
                        Err(e) => eprintln!("file clipboard rejected: {e}"),
                        _ => {}
                    }
                }
            })?;
        Ok(Self {
            sender,
            inbound: Some(tx),
            worker: Some(worker),
        })
    }
    pub fn receive(&self, frame: Frame, revision: u64) -> io::Result<()> {
        if frame.kind == FrameKind::FileAck {
            self.sender.note_ack(wire::parse_ack(&frame.payload)?);
            return Ok(());
        }
        if !matches!(
            frame.kind,
            FrameKind::FileOffer
                | FrameKind::FileData
                | FrameKind::FileSeal
                | FrameKind::FileCommit
                | FrameKind::FileCancel
        ) {
            return Err(invalid("unexpected file frame"));
        }
        self.inbound
            .as_ref()
            .unwrap()
            .try_send(Command { frame, revision })
            .map_err(|_| invalid("file receive queue is full or stopped"))
    }
    pub fn stop(&self) {
        self.sender.shared.stop.store(true, Ordering::Release);
        self.sender.shared.changed.notify_all();
    }
}
impl Drop for FileSession {
    fn drop(&mut self) {
        self.stop();
        self.inbound.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests;
