// CrossDesk file clipboard sessions: 2026-10-20 (GPL-3.0-only).
#[cfg(test)]
use super::wire::Entry;
use super::wire::{self, Ack, Id, MAX_MANIFEST, Offer, Reader};
use super::{
    MAX_CACHE_BYTES, invalid,
    tree::SourceTree,
    tree_cache::{TreeCache, cache_size},
};
use crate::protocol::{CHUNK_SIZE, Frame, FrameKind};
use crate::service_event::{
    TransferDirection::{Receive, Send},
    TransferState as TS, transfer,
};
use crate::transport::SharedWriter;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    path::PathBuf,
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
    root: PathBuf,
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
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
struct Active {
    offer: Offer,
    serial: u64,
    index: usize,
    offset: u64,
    hash: Sha256,
    bundle: TreeCache,
    revision: u64,
    progress: Instant,
}
struct Pending {
    id: Id,
    len: usize,
    data: Vec<u8>,
    serial: u64,
    revision: u64,
    progress: Instant,
}
pub struct Receiver {
    root: PathBuf,
    cache_budget: u64,
    active: Option<Active>,
    pending: Option<Pending>,
}
impl Receiver {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: None,
            pending: None,
            cache_budget: MAX_CACHE_BYTES,
        }
    }
    pub fn expire(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.progress.elapsed() > TIMEOUT)
        {
            transfer(
                self.active.as_ref().unwrap().offer.id,
                Receive,
                TS::Failed,
                Some("接收超时，请重新复制".into()),
            );
            self.active = None;
            eprintln!("file clipboard receive timed out; partial files removed");
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|a| a.progress.elapsed() > TIMEOUT)
        {
            transfer(
                self.pending.as_ref().unwrap().id,
                Receive,
                TS::Failed,
                Some("目录清单接收超时，请重新复制".into()),
            );
            self.pending = None;
        }
    }
    pub fn handle(&mut self, frame: Frame, revision: u64) -> io::Result<Option<CompletedBatch>> {
        let (id, _) = wire::identity(&frame)?;
        let cancelled = frame.kind == FrameKind::FileCancel;
        if cancelled && frame.payload.len() != 16 {
            return Err(invalid("invalid file cancellation"));
        }
        let result = if cancelled {
            Ok(None)
        } else {
            self.handle_inner(frame, revision)
        };
        if cancelled || result.is_err() {
            if self.active.as_ref().is_some_and(|a| a.offer.id == id) {
                self.active = None;
            }
            if self.pending.as_ref().is_some_and(|a| a.id == id) {
                self.pending = None;
            }
        }
        result
    }
    fn finish_metadata(&mut self) -> io::Result<()> {
        if !self.pending.as_ref().is_some_and(|p| p.data.len() == p.len) {
            return Ok(());
        }
        let p = self.pending.take().unwrap();
        let offer = Offer::parse_metadata(p.id, &p.data)?;
        if cache_size(&self.root)?
            .checked_add(offer.total())
            .is_none_or(|n| n > self.cache_budget)
        {
            return Err(invalid(
                "file cache budget reached; clear unreferenced caches before retrying",
            ));
        }
        let mut bundle = TreeCache::new(&self.root, &offer)?;
        let index = offer
            .entries
            .iter()
            .position(|e| !e.directory)
            .unwrap_or(offer.entries.len());
        if let Some(e) = offer.entries.get(index) {
            bundle.start(index, e.len)?;
        }
        self.active = Some(Active {
            offer,
            serial: p.serial,
            index,
            offset: 0,
            hash: Sha256::new(),
            bundle,
            revision: p.revision,
            progress: Instant::now(),
        });
        Ok(())
    }
    fn handle_inner(&mut self, frame: Frame, revision: u64) -> io::Result<Option<CompletedBatch>> {
        if frame.kind == FrameKind::FileOffer {
            if self.active.is_some() || self.pending.is_some() {
                return Err(invalid("file batch already in progress"));
            }
            let mut r = Reader::new(&frame.payload);
            let id = r.id()?;
            let len = r.u32()? as usize;
            if len < 2 || len > MAX_MANIFEST || r.rest().is_empty() || r.rest().len() > len {
                return Err(invalid("invalid manifest size; maximum is 4 MiB"));
            }
            self.pending = Some(Pending {
                id,
                len,
                data: r.rest().to_vec(),
                serial: 1,
                revision,
                progress: Instant::now(),
            });
            self.finish_metadata()?;
            return Ok(None);
        }
        if frame.kind == FrameKind::FileManifest {
            let p = self
                .pending
                .as_mut()
                .ok_or_else(|| invalid("manifest continuation without offer"))?;
            let mut r = Reader::new(&frame.payload);
            if r.id()? != p.id
                || r.u64()? != p.serial
                || r.rest().is_empty()
                || p.data.len() + r.rest().len() > p.len
            {
                return Err(invalid("invalid manifest page ID, serial or length"));
            }
            p.data.extend_from_slice(r.rest());
            p.serial += 1;
            p.progress = Instant::now();
            self.finish_metadata()?;
            return Ok(None);
        }
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| invalid("file data without complete manifest"))?;
        let mut r = Reader::new(&frame.payload);
        if r.id()? != active.offer.id || r.u64()? != active.serial {
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
                    || index >= active.offer.entries.len()
                {
                    return Err(invalid("file index, offset or chunk size mismatch"));
                }
                active.bundle.write(data)?;
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
                active.bundle.seal()?;
                active.index += 1;
                while active
                    .offer
                    .entries
                    .get(active.index)
                    .is_some_and(|e| e.directory)
                {
                    active.index += 1;
                }
                active.offset = 0;
                active.hash = Sha256::new();
                if let Some(next) = active.offer.entries.get(active.index) {
                    active.bundle.start(active.index, next.len)?;
                }
            }
            FrameKind::FileCommit => {
                r.end()?;
                if active.index != active.offer.entries.len() {
                    return Err(invalid("file batch is incomplete"));
                }
                let completed = self.active.take().unwrap();
                let (root, files) = completed.bundle.finish()?;
                return Ok(Some(CompletedBatch {
                    id: completed.offer.id,
                    revision: completed.revision,
                    files,
                    root,
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
        let mut id = [0; 16];
        getrandom::getrandom(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        transfer(id, Send, TS::Preparing, None);
        let result = (|| {
            let mut cancelled = || {
                self.shared.stop.load(Ordering::Acquire) || crate::shutdown::requested() || cancel()
            };
            let sources = SourceTree::scan(files, &mut cancelled)?;
            let offer = sources.offer(id);
            offer.validate()?;
            transfer(
                id,
                Send,
                TS::Transferring,
                Some(format!(
                    "{} 项 · {} 字节",
                    offer.entries.len(),
                    offer.total()
                )),
            );
            let frames = offer.frames();
            let mut serial = frames.len() as u64;
            for frame in frames {
                self.exchange(frame, &mut cancelled)?;
            }
            sources.verify(&mut cancelled)?;
            let mut buffer = vec![0; CHUNK_SIZE];
            for (index, entry) in sources.entries.iter().enumerate() {
                if entry.directory {
                    continue;
                }
                let mut file = sources.open(index)?;
                let mut offset = 0;
                let mut hash = Sha256::new();
                while offset < entry.len {
                    super::tree::cancelled(&mut cancelled)?;
                    let length = (entry.len - offset).min(CHUNK_SIZE as u64) as usize;
                    let n = file.read(&mut buffer[..length])?;
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
                        &mut cancelled,
                    )?;
                    serial += 1;
                    offset += n as u64;
                }
                sources.verify_open(index, &file)?;
                drop(file);
                self.exchange(
                    wire::request(
                        FrameKind::FileSeal,
                        id,
                        serial,
                        index as u16,
                        0,
                        &hash.finalize(),
                    ),
                    &mut cancelled,
                )?;
                serial += 1;
            }
            sources.verify(&mut cancelled)?;
            self.exchange(
                wire::request(FrameKind::FileCommit, id, serial, 0, 0, &[]),
                &mut cancelled,
            )?;
            eprintln!(
                "file clipboard transmitted: {} items, {} bytes; waiting for destination publication",
                offer.entries.len(),
                offer.total()
            );
            transfer(id, Send, TS::Waiting, Some("等待对方发布到剪贴板".into()));
            Ok(id)
        })();
        if let Err(e) = &result {
            transfer(
                id,
                Send,
                if e.kind() == io::ErrorKind::Interrupted {
                    TS::Skipped
                } else {
                    TS::Failed
                },
                Some(e.to_string()),
            );
        }
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
        mut completed: impl FnMut(CompletedBatch) + std::marker::Send + 'static,
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
                    if command.frame.kind == FrameKind::FileOffer {
                        transfer(
                            identity.0,
                            Receive,
                            TS::Transferring,
                            Some("正在接收文件和文件夹".into()),
                        );
                    }
                    let result = receiver.handle(command.frame, command.revision);
                    if cancelled {
                        transfer(
                            identity.0,
                            Receive,
                            TS::Skipped,
                            Some("发送端取消了本次复制".into()),
                        );
                        continue;
                    }
                    let status = match &result {
                        Ok(Some(_)) => 1,
                        Ok(None) => 0,
                        Err(_) => 2,
                    };
                    match result {
                        Ok(Some(batch)) => {
                            transfer(
                                identity.0,
                                Receive,
                                TS::Waiting,
                                Some("校验完成，等待写入剪贴板".into()),
                            );
                            completed(batch)
                        }
                        Err(e) => {
                            transfer(identity.0, Receive, TS::Failed, Some(e.to_string()));
                            eprintln!("file clipboard rejected: {e}")
                        }
                        _ => {}
                    }
                    // Queue publication before granting commit credit. A sender
                    // may start its next image/file immediately after this ACK.
                    if writer
                        .write(wire::ack(identity.0, identity.1, status))
                        .is_err()
                    {
                        break;
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
                | FrameKind::FileManifest
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
