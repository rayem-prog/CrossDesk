use crate::service_event::{TransferDirection, TransferState, transfer as report_transfer};
// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use super::{Clipboard, ClipboardApi};
use crate::file_transfer::{
    session::{CompletedBatch, FileSender, FileSession},
    wire,
};
use crate::image_transfer::{self, Incoming, Transfer};
use crate::protocol::{ClipboardPayload, Frame, FrameKind};
use crate::text_transfer;
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

// Native revisions connected only by our own publications form one lineage.
// A new local copy starts a new lineage, so a late older delivery cannot use
// the last-remote exception to cross that local-copy boundary.
#[derive(Default)]
struct PublicationGuard {
    revisions: std::collections::VecDeque<u64>,
}
impl PublicationGuard {
    fn permits(&mut self, start: u64, current: u64, last_remote: Option<u64>) -> bool {
        if last_remote != Some(current) {
            self.revisions.clear();
            self.revisions.push_back(current);
        }
        self.revisions.contains(&start)
    }
    fn published(&mut self, revision: u64) {
        self.revisions.push_back(revision);
        if self.revisions.len() > 64 {
            self.revisions.pop_front();
        }
    }
}

struct Delivery {
    transfer: Transfer,
    revision: u64,
}
enum RemoteDelivery {
    Image(Delivery),
    Files(CompletedBatch),
    Text(TextDelivery),
}
struct TextDelivery {
    transfer: text_transfer::Transfer,
    revision: u64,
}
#[derive(Default)]
struct TextReceiver {
    incoming: text_transfer::Incoming,
    revision: u64,
}
impl RemoteDelivery {
    fn id(&self) -> [u8; 16] {
        match self {
            Self::Image(v) => v.transfer.id,
            Self::Files(v) => v.id,
            Self::Text(v) => v.transfer.id,
        }
    }
}
fn enqueue(mailbox: &Mailbox, delivery: RemoteDelivery, writer: &SharedWriter) {
    let old = mailbox.pending.lock().unwrap().replace(delivery);
    if let Some(old) = old {
        let _ = writer.write(wire::result(old.id(), 1));
        if matches!(&old, RemoteDelivery::Text(_)) {
            text_status(
                old.id(),
                TransferDirection::Receive,
                TransferState::Skipped,
                "已由更新的远端复制内容替代",
            );
        }
    }
    mailbox.changed.notify_all();
}
struct Mailbox {
    pending: Mutex<Option<RemoteDelivery>>,
    file_sender: Mutex<Option<FileSender>>,
    images_enabled: AtomicBool,
    texts_enabled: AtomicBool,
    text_receiver: Mutex<TextReceiver>,
    sent_text: Mutex<std::collections::VecDeque<[u8; 16]>>,
    changed: Condvar,
    stop: AtomicBool,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            pending: Mutex::new(None),
            file_sender: Mutex::new(None),
            images_enabled: AtomicBool::new(true),
            texts_enabled: AtomicBool::new(false),
            text_receiver: Mutex::new(TextReceiver::default()),
            sent_text: Mutex::new(std::collections::VecDeque::new()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        }
    }
}

pub struct ClipboardSession {
    enabled: bool,
    image_enabled: bool,
    file_session: Option<FileSession>,
    writer: SharedWriter,
    incoming: Incoming,
    revision: u64,
    probe: Option<Clipboard>,
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
    expiry_worker: Option<JoinHandle<()>>,
}

impl ClipboardSession {
    pub fn start(
        writer: SharedWriter,
        image_enabled: bool,
        file_enabled: bool,
        text_enabled: bool,
    ) -> io::Result<Self> {
        let enabled = image_enabled || file_enabled || text_enabled;
        let mailbox = Arc::new(Mailbox::default());
        mailbox
            .images_enabled
            .store(image_enabled, Ordering::Release);
        mailbox.texts_enabled.store(text_enabled, Ordering::Release);
        let file_session = if file_enabled {
            let target = mailbox.clone();
            let replies = writer.clone();
            let session = FileSession::start(
                writer.clone(),
                std::env::temp_dir().join("crossdesk-file-clipboard"),
                move |batch| {
                    enqueue(&target, RemoteDelivery::Files(batch), &replies);
                },
            )?;
            *mailbox.file_sender.lock().unwrap() = Some(session.sender.clone());
            Some(session)
        } else {
            None
        };
        let worker_writer = writer.clone();
        let (probe, worker) = if enabled {
            let mut clipboard = Clipboard::new()?;
            let probe = Clipboard::new()?;
            let baseline = clipboard.change_id()?;
            // A busy clipboard must not prevent keyboard/mouse startup. Skip cleanup
            // when its current file references cannot be read safely.
            if let Ok(snapshot) = clipboard.read() {
                let protected = match snapshot {
                    Some(ClipboardPayload::Files(files)) => files,
                    _ => Vec::new(),
                };
                cleanup_cache(
                    &std::env::temp_dir().join("crossdesk-image-clipboard"),
                    &protected,
                );
                cleanup_cache(
                    &std::env::temp_dir().join("crossdesk-file-clipboard"),
                    &protected,
                );
            }
            let worker_mailbox = Arc::clone(&mailbox);
            let worker = thread::Builder::new()
                .name("clipboard-sync".into())
                .spawn(move || {
                    worker_loop(clipboard, baseline, worker_writer, worker_mailbox);
                })?;
            (Some(probe), Some(worker))
        } else {
            (None, None)
        };
        eprintln!(
            "image clipboard: {}",
            if image_enabled {
                "on (PNG/JPG files and image content)"
            } else {
                "off (disabled by one of the peers)"
            }
        );
        eprintln!(
            "file clipboard: {}",
            if file_enabled {
                "on (ordinary files; 512 MiB/file, 2 GiB/batch, 64 files)"
            } else {
                "off (disabled by one of the peers)"
            }
        );
        eprintln!(
            "text clipboard: {}",
            if text_enabled {
                "on (plain text, up to 1 MiB)"
            } else {
                "off (peer disabled or unsupported)"
            }
        );
        let mut session = Self {
            enabled,
            image_enabled,
            file_session,
            writer,
            incoming: Incoming::default(),
            revision: 0,
            probe,
            mailbox,
            worker,
            expiry_worker: None,
        };
        if text_enabled {
            let target = session.mailbox.clone();
            let replies = session.writer.clone();
            session.expiry_worker = Some(
                thread::Builder::new()
                    .name("text-clipboard-expiry".into())
                    .spawn(move || {
                        while !target.stop.load(Ordering::Acquire) {
                            expire_text(&target, &replies);
                            thread::sleep(Duration::from_millis(200));
                        }
                    })?,
            );
        }
        Ok(session)
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn handles(kind: FrameKind) -> bool {
        wire::handles(kind)
            || text_transfer::handles(kind)
            || kind == FrameKind::Clipboard
            || matches!(
                kind,
                FrameKind::ImageStart | FrameKind::ImageChunk | FrameKind::ImageEnd
            )
    }
    pub fn receive(&mut self, frame: Frame) -> io::Result<()> {
        // The legacy single-frame path never bypasses negotiated options or revision protection.
        if frame.kind == FrameKind::Clipboard {
            return Err(image_transfer::invalid(
                "legacy clipboard publication is disabled",
            ));
        }
        if text_transfer::handles(frame.kind) {
            let revision = if frame.kind == FrameKind::TextStart {
                self.probe
                    .as_ref()
                    .map(ClipboardApi::change_id)
                    .transpose()?
                    .unwrap_or(0)
            } else {
                0
            };
            return receive_text(frame, revision, &self.mailbox, &self.writer);
        }
        if frame.kind == FrameKind::TransferResult {
            let (id, status) = wire::parse_result(&frame.payload)?;
            let is_text = self.mailbox.sent_text.lock().unwrap().contains(&id);
            let report = if is_text {
                crate::service_event::text_transfer
            } else {
                report_transfer
            };
            report(
                id,
                TransferDirection::Send,
                match status {
                    0 => TransferState::Ready,
                    1 => TransferState::Skipped,
                    _ => TransferState::Failed,
                },
                Some(
                    match status {
                        0 => "对方已经可以粘贴",
                        1 => "对方有更新的复制内容，请重新复制后重试",
                        _ => "对方发布失败，请查看对方日志并重试",
                    }
                    .into(),
                ),
            );
            eprintln!(
                "destination clipboard result: {}",
                match status {
                    0 => "published; ready to paste",
                    1 => "skipped; a newer local copy exists (copy again to retry)",
                    _ => "rejected or failed; see destination log",
                }
            );
            return Ok(());
        }
        if wire::handles(frame.kind) {
            if let Some(files) = &self.file_session {
                let revision = if frame.kind == FrameKind::FileOffer {
                    self.probe.as_ref().unwrap().change_id()?
                } else {
                    0
                };
                return files.receive(frame, revision);
            }
            if let Ok((id, serial)) = wire::identity(&frame) {
                let _ = self.writer.write(wire::ack(id, serial, 2));
            }
            return Err(image_transfer::invalid(
                "file synchronization was not negotiated",
            ));
        }
        if !self.image_enabled {
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
            enqueue(
                &self.mailbox,
                RemoteDelivery::Image(Delivery {
                    transfer,
                    revision: self.revision,
                }),
                &self.writer,
            );
        }
        Ok(())
    }
}

impl Drop for ClipboardSession {
    fn drop(&mut self) {
        self.mailbox.stop.store(true, Ordering::Release);
        self.mailbox.changed.notify_all();
        if let Some(files) = &self.file_session {
            files.stop();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.expiry_worker.take() {
            let _ = worker.join();
        }
    }
}

fn text_status(id: [u8; 16], direction: TransferDirection, state: TransferState, message: &str) {
    crate::service_event::text_transfer(id, direction, state, Some(message.into()));
}

fn receive_text(
    frame: Frame,
    revision: u64,
    mailbox: &Mailbox,
    writer: &SharedWriter,
) -> io::Result<()> {
    let id = text_transfer::identity(&frame);
    if !mailbox.texts_enabled.load(Ordering::Acquire) {
        if let Some(id) = id {
            let _ = writer.write(wire::result(id, 2));
        }
        return Err(image_transfer::invalid(
            "text synchronization was not negotiated",
        ));
    }
    let mut receiver = mailbox.text_receiver.lock().unwrap();
    let active = receiver.incoming.active_id();
    // Invalid metadata also clears the assembly. Attribute its terminal result
    // to that discarded batch, rather than a corrupt or absent incoming ID.
    let failed_id = if frame.kind == FrameKind::TextStart {
        id.or(active)
    } else {
        active.or(id)
    };
    let cancelled =
        frame.kind == FrameKind::TextCancel && id.is_some() && receiver.incoming.active_id() == id;
    if frame.kind == FrameKind::TextStart {
        if let Some(old) = receiver.incoming.active_id() {
            let _ = writer.write(wire::result(old, 1));
        }
        receiver.revision = revision;
        if let Some(id) = id {
            text_status(
                id,
                TransferDirection::Receive,
                TransferState::Transferring,
                "正在接收文字",
            );
        }
    }
    match receiver.incoming.receive(frame) {
        Ok(Some(transfer)) => {
            enqueue(
                mailbox,
                RemoteDelivery::Text(TextDelivery {
                    transfer,
                    revision: receiver.revision,
                }),
                writer,
            );
            Ok(())
        }
        Ok(None) => {
            if cancelled {
                let id = id.unwrap();
                let _ = writer.write(wire::result(id, 1));
                text_status(
                    id,
                    TransferDirection::Receive,
                    TransferState::Skipped,
                    "发送端已取消或复制了新内容",
                );
            }
            Ok(())
        }
        Err(error) => {
            if let Some(id) = failed_id {
                let _ = writer.write(wire::result(id, 2));
                text_status(
                    id,
                    TransferDirection::Receive,
                    TransferState::Failed,
                    &error.to_string(),
                );
            }
            Err(error)
        }
    }
}

fn expire_text(mailbox: &Mailbox, writer: &SharedWriter) {
    let expired = mailbox.text_receiver.lock().unwrap().incoming.expire();
    if let Some(id) = expired {
        let _ = writer.write(wire::result(id, 2));
        text_status(
            id,
            TransferDirection::Receive,
            TransferState::Failed,
            "文字接收超时，请重新复制",
        );
    }
}

fn send_text<C: ClipboardApi>(
    text: &str,
    revision: u64,
    clipboard: &C,
    writer: &SharedWriter,
    mailbox: &Mailbox,
) -> io::Result<()> {
    let transfer = match text_transfer::Transfer::from_text(text) {
        Ok(transfer) => transfer,
        Err(error) => {
            let mut id = [0; 16];
            getrandom::getrandom(&mut id)
                .map_err(|_| io::Error::other("text transfer ID unavailable"))?;
            text_status(
                id,
                TransferDirection::Send,
                TransferState::Preparing,
                "正在检查文字",
            );
            text_status(
                id,
                TransferDirection::Send,
                TransferState::Skipped,
                &error.to_string(),
            );
            return Ok(());
        }
    };
    let id = transfer.id;
    {
        let mut ids = mailbox.sent_text.lock().unwrap();
        ids.push_back(id);
        if ids.len() > 64 {
            ids.pop_front();
        }
    }
    text_status(
        id,
        TransferDirection::Send,
        TransferState::Preparing,
        "正在准备文字",
    );
    text_status(
        id,
        TransferDirection::Send,
        TransferState::Transferring,
        "正在同步文字",
    );
    let result = transfer.visit_frames(|frame| {
        if mailbox.stop.load(Ordering::Acquire) || clipboard.change_id()? != revision {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "text copy superseded or session stopped",
            ));
        }
        writer.write(frame)
    });
    match &result {
        Ok(()) => text_status(
            id,
            TransferDirection::Send,
            TransferState::Waiting,
            "等待对方发布文字",
        ),
        Err(error) => {
            let _ = writer.write(text_transfer::cancel(id));
            text_status(
                id,
                TransferDirection::Send,
                if error.kind() == io::ErrorKind::Interrupted {
                    TransferState::Skipped
                } else {
                    TransferState::Failed
                },
                &error.to_string(),
            );
        }
    }
    result
}

fn apply_text_delivery<C: ClipboardApi>(
    clipboard: &mut C,
    delivery: TextDelivery,
    last_remote: Option<u64>,
) -> io::Result<Option<u64>> {
    let text = text_transfer::normalize(&delivery.transfer.text)?;
    let payload = ClipboardPayload::Text(text.clone());
    for attempt in 0..3 {
        let current = clipboard.change_id()?;
        if !may_publish_remote(delivery.revision, current, last_remote) {
            return Ok(None);
        }
        match clipboard.publish_revision_if_current(&payload, current) {
            Ok(Some(published)) => {
                if clipboard.change_id()? != published {
                    return Ok(None);
                }
                return match clipboard.read()? {
                    Some(ClipboardPayload::Text(readback))
                        if text_transfer::normalize(&readback).ok().as_deref()
                            == Some(text.as_str())
                            && clipboard.change_id()? == published =>
                    {
                        Ok(Some(published))
                    }
                    _ => Ok(None),
                };
            }
            Ok(None) => return Ok(None),
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    unreachable!()
}

fn worker_loop<C: ClipboardApi>(
    mut clipboard: C,
    mut revision: u64,
    writer: SharedWriter,
    mailbox: Arc<Mailbox>,
) {
    let mut state = SyncState::default();
    let mut last_remote_revision = None;
    let mut publication_guard = PublicationGuard::default();
    let cache = std::env::temp_dir().join("crossdesk-image-clipboard");
    // Initialize the platform boundary without ever transmitting startup contents.
    let _ = clipboard.read();
    let file_sender = mailbox.file_sender.lock().unwrap().clone();
    while !mailbox.stop.load(Ordering::Acquire) && !crate::shutdown::requested() {
        let incoming = mailbox.pending.lock().unwrap().take();
        if let Some(delivery) = incoming {
            let id = delivery.id();
            let started = match &delivery {
                RemoteDelivery::Image(d) => d.revision,
                RemoteDelivery::Files(b) => b.revision,
                RemoteDelivery::Text(d) => d.revision,
            };
            let can_publish = clipboard
                .change_id()
                .map(|current| publication_guard.permits(started, current, last_remote_revision));
            let is_text = matches!(&delivery, RemoteDelivery::Text(_));
            let result = match can_publish {
                Err(e) => Err(e),
                Ok(false) => Ok(None),
                Ok(true) => match delivery {
                    RemoteDelivery::Image(delivery) => apply_delivery(
                        &mut clipboard,
                        delivery,
                        &cache,
                        last_remote_revision,
                        &mut state,
                    ),
                    RemoteDelivery::Files(mut batch) => {
                        apply_file_delivery(&mut clipboard, &mut batch, last_remote_revision)
                    }
                    RemoteDelivery::Text(delivery) => {
                        apply_text_delivery(&mut clipboard, delivery, last_remote_revision)
                    }
                },
            };
            let status = match &result {
                Ok(Some(_)) => 0,
                Ok(None) => 1,
                Err(_) => 2,
            };
            let report = if is_text {
                crate::service_event::text_transfer
            } else {
                report_transfer
            };
            report(
                id,
                TransferDirection::Receive,
                match status {
                    0 => TransferState::Ready,
                    1 => TransferState::Skipped,
                    _ => TransferState::Failed,
                },
                Some(match &result {
                    Err(e) => e.to_string(),
                    Ok(None) => "已有更新的本地复制内容；请在发送端重新复制".into(),
                    _ if is_text => "文字已同步，可以粘贴".into(),
                    _ => "可在 Finder 或资源管理器中粘贴".into(),
                }),
            );
            let _ = writer.write(wire::result(id, status));
            match result {
                Ok(Some(next)) => {
                    revision = next;
                    last_remote_revision = Some(next);
                    publication_guard.published(next);
                }
                Err(e) => eprintln!("clipboard publication failed: {e}"),
                _ => {}
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
                        if let ClipboardPayload::Text(text) = &payload {
                            state = SyncState::default();
                            if mailbox.texts_enabled.load(Ordering::Acquire) {
                                if let Err(error) =
                                    send_text(text, current, &clipboard, &writer, &mailbox)
                                {
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::BrokenPipe
                                            | io::ErrorKind::ConnectionReset
                                            | io::ErrorKind::TimedOut
                                    ) {
                                        break;
                                    }
                                }
                            }
                            continue;
                        }
                        if let (Some(sender), ClipboardPayload::Files(files)) =
                            (&file_sender, &payload)
                        {
                            let result = sender.send(files, || {
                                mailbox.stop.load(Ordering::Acquire)
                                    || clipboard.change_id().ok() != Some(current)
                            });
                            if let Err(error) = result {
                                eprintln!("file clipboard send skipped: {error}");
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                                ) {
                                    break;
                                }
                            }
                            continue;
                        }
                        if !mailbox.images_enabled.load(Ordering::Acquire) {
                            continue;
                        }
                        match Transfer::from_clipboard(&payload).and_then(|transfer| {
                            let fingerprint = transfer.fingerprint()?;
                            // A changed native revision is a new copy, even if its content is identical.
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
                        if error.kind() == io::ErrorKind::InvalidData {
                            // A rejected copy is terminal for this native revision; a new
                            // explicit copy is required to retry, even with identical text.
                            revision = current;
                            if text_transfer::is_rejection(&error)
                                && mailbox.texts_enabled.load(Ordering::Acquire)
                            {
                                let mut id = [0; 16];
                                if getrandom::getrandom(&mut id).is_ok() {
                                    text_status(
                                        id,
                                        TransferDirection::Send,
                                        TransferState::Preparing,
                                        "正在检查复制内容",
                                    );
                                    text_status(
                                        id,
                                        TransferDirection::Send,
                                        TransferState::Skipped,
                                        &error.to_string(),
                                    );
                                }
                            }
                            eprintln!("clipboard copy skipped: {error}");
                        } else {
                            eprintln!("clipboard read failed (will retry): {error}");
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(error) => eprintln!("image clipboard revision read failed: {error}"),
        }
        expire_text(&mailbox, &writer);
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
        match clipboard.publish_revision_if_current(&payload, current) {
            Ok(Some(published)) => {
                result = Some(Ok(published));
                break;
            }
            Ok(None) => {
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
    let published_revision = match result.unwrap() {
        Ok(revision) => revision,
        Err(error) => {
            remove_unpublished(&payload, root);
            return Err(error);
        }
    };
    // Never advance observation to a newer local copy or whitelist arbitrary readback.
    // Published file caches are retained even if another app immediately copies again.
    if clipboard.change_id()? != published_revision {
        return Ok(None);
    }
    match clipboard.read() {
        Ok(Some(readback))
            if super::same_payload(&readback, &payload)
                && clipboard.change_id()? == published_revision => {}
        _ => return Ok(None),
    }
    state.note_remote(fingerprint);
    eprintln!("image clipboard received: {count} image(s); ready to paste");
    Ok(Some(published_revision))
}

fn apply_file_delivery<C: ClipboardApi>(
    clipboard: &mut C,
    batch: &mut CompletedBatch,
    last_remote: Option<u64>,
) -> io::Result<Option<u64>> {
    let payload = ClipboardPayload::Files(batch.files.clone());
    for attempt in 0..3 {
        let current = clipboard.change_id()?;
        if !may_publish_remote(batch.revision, current, last_remote) {
            return Ok(None);
        }
        match clipboard.publish_revision_if_current(&payload, current) {
            Ok(Some(published)) => {
                // Once native publication succeeded, a file manager may be reading these paths.
                batch.retain();
                if clipboard.change_id()? != published {
                    return Ok(None);
                }
                match clipboard.read() {
                    Ok(Some(readback))
                        if super::same_payload(&readback, &payload)
                            && clipboard.change_id()? == published =>
                    {
                        eprintln!(
                            "file clipboard received: {} file(s); ready to paste",
                            batch.files.len()
                        );
                        return Ok(Some(published));
                    }
                    _ => return Ok(None),
                }
            }
            Ok(None) => return Ok(None),
            Err(e) if attempt == 2 => return Err(e),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    unreachable!()
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
        *mailbox.pending.lock().unwrap() = Some(RemoteDelivery::Image(Delivery {
            transfer: Transfer::from_clipboard(&ClipboardPayload::Bitmap(png(8))).unwrap(),
            revision: 1,
        }));
        mailbox.changed.notify_all();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while memory.lock().unwrap().2 == 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(memory.lock().unwrap().2, 1);
        let receipt = protocol::read_frame(&mut peer).unwrap();
        assert_eq!(receipt.kind, FrameKind::TransferResult);
        assert_eq!(wire::parse_result(&receipt.payload).unwrap().1, 0);
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

    #[test]
    fn worker_exits_when_a_changed_image_cannot_be_sent_to_a_closed_connection() {
        use crate::secure::SecureStream;
        use std::net::{Shutdown, TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let accept = thread::spawn(move || {
            SecureStream::accept(listener.accept().unwrap().0, &[45; 32]).unwrap()
        });
        let client = SecureStream::connect(&address, &[45; 32]).unwrap();
        let _peer = accept.join().unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        let fake = MemoryClipboard(Arc::new(Mutex::new((
            1,
            ClipboardPayload::Bitmap(png(10)),
            0,
        ))));
        let mailbox = Arc::new(Mailbox::default());
        let worker_mailbox = mailbox.clone();
        let (done, completion) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            worker_loop(fake, 0, SharedWriter::new(client), worker_mailbox);
            done.send(()).unwrap();
        });
        let stopped = completion.recv_timeout(Duration::from_secs(2));
        mailbox.stop.store(true, Ordering::Release);
        mailbox.changed.notify_all();
        worker.join().unwrap();
        assert!(
            stopped.is_ok(),
            "clipboard worker leaked after a transport failure"
        );
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    use crate::secure::SecureStream;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    fn png(red: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_raw(1, 1, vec![red, 2, 3, 127]).unwrap();
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }
    struct Memory {
        value: Arc<Mutex<(u64, ClipboardPayload)>>,
        reads: Arc<AtomicUsize>,
    }
    impl ClipboardApi for Memory {
        fn change_id(&self) -> io::Result<u64> {
            Ok(self.value.lock().unwrap().0)
        }
        fn read(&mut self) -> io::Result<Option<ClipboardPayload>> {
            self.reads.fetch_add(1, Ordering::AcqRel);
            Ok(Some(self.value.lock().unwrap().1.clone()))
        }
        fn write(&mut self, payload: &ClipboardPayload) -> io::Result<()> {
            let mut state = self.value.lock().unwrap();
            state.0 += 1;
            state.1 = payload.clone();
            Ok(())
        }
    }
    #[test]
    fn review_explicit_recopy_of_same_image_must_send_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let accept = thread::spawn(move || {
            SecureStream::accept(listener.accept().unwrap().0, &[89; 32]).unwrap()
        });
        let client = SecureStream::connect(&address, &[89; 32]).unwrap();
        let mut peer = accept.join().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let value = Arc::new(Mutex::new((0, ClipboardPayload::Bitmap(png(1)))));
        let reads = Arc::new(AtomicUsize::new(0));
        let mailbox = Arc::new(Mailbox::default());
        let fake = Memory {
            value: value.clone(),
            reads: reads.clone(),
        };
        let worker_mailbox = mailbox.clone();
        let worker =
            thread::spawn(move || worker_loop(fake, 0, SharedWriter::new(client), worker_mailbox));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while reads.load(Ordering::Acquire) == 0 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        {
            let mut s = value.lock().unwrap();
            s.0 = 1;
            s.1 = ClipboardPayload::Bitmap(png(7));
        }
        mailbox.changed.notify_all();
        let mut incoming = Incoming::default();
        loop {
            if incoming
                .receive(crate::protocol::read_frame(&mut peer).unwrap())
                .unwrap()
                .is_some()
            {
                break;
            }
        }
        // The peer may reject publishing because a newer local copy exists. No outcome ACK is sent.
        {
            value.lock().unwrap().0 = 2;
        }
        mailbox.changed.notify_all();
        peer.set_read_timeout(Some(Duration::from_millis(650)))
            .unwrap();
        let second = crate::protocol::read_frame(&mut peer);
        mailbox.stop.store(true, Ordering::Release);
        mailbox.changed.notify_all();
        worker.join().unwrap();
        assert!(
            second.is_ok(),
            "explicit same-content recopy was suppressed: {second:?}"
        );
    }
    struct CopyDuringPublication {
        revision: u64,
        local: ClipboardPayload,
    }
    impl ClipboardApi for CopyDuringPublication {
        fn change_id(&self) -> io::Result<u64> {
            Ok(self.revision)
        }
        fn read(&mut self) -> io::Result<Option<ClipboardPayload>> {
            Ok(Some(self.local.clone()))
        }
        fn write(&mut self, _: &ClipboardPayload) -> io::Result<()> {
            unreachable!()
        }
        fn write_if_current(&mut self, _: &ClipboardPayload, expected: u64) -> io::Result<bool> {
            assert_eq!(self.revision, expected);
            // Remote write releases native clipboard ownership at revision 1.
            self.revision += 1;
            // Another app immediately copies a DIFFERENT local image before this call returns.
            self.revision += 1;
            Ok(true)
        }
    }
    #[test]
    fn review_local_copy_after_publication_must_not_be_marked_as_remote_echo() {
        let local = ClipboardPayload::Bitmap(png(9));
        let local_fingerprint = Transfer::from_clipboard(&local)
            .unwrap()
            .fingerprint()
            .unwrap();
        let mut clipboard = CopyDuringPublication { revision: 0, local };
        let delivery = Delivery {
            revision: 0,
            transfer: Transfer::from_clipboard(&ClipboardPayload::Bitmap(png(8))).unwrap(),
        };
        let mut state = SyncState::default();
        let recorded_revision = apply_delivery(
            &mut clipboard,
            delivery,
            &std::env::temp_dir(),
            None,
            &mut state,
        )
        .unwrap();
        assert!(
            state.should_send(local_fingerprint),
            "a distinct new local copy is registered as remote echo; revision={recorded_revision:?}"
        );
    }
}

#[cfg(test)]
mod file_tests;

#[cfg(test)]
mod text_tests;

#[cfg(test)]
mod text_loopback_tests;
