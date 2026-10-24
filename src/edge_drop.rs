//! Explicit, independent, clipboard-free file drop lane.
use crate::{
    file_transfer::{
        invalid,
        tree::SourceTree,
        tree_cache::TreeCache,
        wire::{self, Id, MAX_MANIFEST, Offer, Reader},
    },
    protocol::{self, Frame, FrameKind},
    secure::SecureStream,
    transport::SharedWriter,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    net::Shutdown,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
mod filesystem;
use filesystem::{available_space, rename_exclusive};
#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "gui",
    serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)
)]
pub enum DropCommand {
    Send { paths: Vec<PathBuf> },
    Accept { id: String, directory: PathBuf },
    Reject { id: String },
    Cancel { id: String },
    SetEnabled { enabled: bool },
}
#[derive(Clone, Debug)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "camelCase"))]
pub struct DropTransfer {
    pub id: String,
    pub state: String,
    pub names: Vec<String>,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub message: Option<String>,
    pub saved_path: Option<PathBuf>,
}
#[derive(Clone, Debug)]
#[cfg_attr(feature = "gui", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "gui", serde(rename_all = "camelCase"))]
pub struct DropView {
    pub enabled: bool,
    pub peer_supported: Option<bool>,
    pub peer_enabled: Option<bool>,
    pub channel: String,
    pub outgoing: Option<DropTransfer>,
    pub incoming: Option<DropTransfer>,
}
impl Default for DropView {
    fn default() -> Self {
        Self {
            enabled: cfg!(feature = "gui")
                && std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some()
                && std::env::var("CROSSDESK_EDGE_DROP").map_or(true, |v| v != "off" && v != "0"),
            peer_supported: None,
            peer_enabled: None,
            channel: "disconnected".into(),
            outgoing: None,
            incoming: None,
        }
    }
}
#[derive(Default)]
struct Service {
    view: DropView,
    tx: Option<mpsc::SyncSender<Event>>,
    generation: u64,
    last_progress: Option<Instant>,
}
static SERVICE: OnceLock<Mutex<Service>> = OnceLock::new();
fn service() -> &'static Mutex<Service> {
    SERVICE.get_or_init(|| Mutex::new(Service::default()))
}
pub fn view() -> DropView {
    service().lock().unwrap().view.clone()
}
pub fn enabled() -> bool {
    view().enabled
}
fn emit(v: &DropView) {
    #[cfg(feature = "gui")]
    if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some() {
        if let Ok(s) = serde_json::to_string(v) {
            if s.len() + "crossdesk-drop\t".len() <= 16 * 1024 {
                println!("crossdesk-drop\t{s}");
            }
        }
    }
    #[cfg(not(feature = "gui"))]
    let _ = v;
}
fn update(f: impl FnOnce(&mut DropView)) {
    let snapshot = {
        let mut s = service().lock().unwrap();
        f(&mut s.view);
        s.view.clone()
    };
    emit(&snapshot);
}
fn progress(send: bool, bytes: u64, absolute: bool) {
    let snapshot = {
        let mut s = service().lock().unwrap();
        if let Some(t) = if send {
            &mut s.view.outgoing
        } else {
            &mut s.view.incoming
        } {
            if absolute {
                t.transferred_bytes = bytes;
            } else {
                t.transferred_bytes += bytes;
            }
        }
        if s.last_progress
            .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
        {
            return;
        }
        s.last_progress = Some(Instant::now());
        s.view.clone()
    };
    emit(&snapshot);
}
fn friendly(e: &io::Error) -> &'static str {
    let text = e.to_string();
    if text.contains("路径过长") {
        "保存路径过长，请选择较浅的目录"
    } else if text.contains("busy") {
        "当前已有发送任务，请等待完成或取消后再试"
    } else if text.contains("disabled") {
        "本机或对方已关闭文件投送"
    } else if text.contains("stale")
        || text.contains("unknown drop")
        || text.contains("no incoming")
    {
        "这个投送请求已结束，请重新发送"
    } else if e.kind() == io::ErrorKind::PermissionDenied {
        "没有文件或目录的读写权限"
    } else if e.kind() == io::ErrorKind::StorageFull {
        "保存位置空间不足"
    } else if text.contains("changed") || text.contains("replaced") || text.contains("truncated") {
        "源文件或目录已变化，请重新投送"
    } else if text.contains("512 MiB") || text.contains("2 GiB") {
        "文件超过512 MiB或总量超过2 GiB"
    } else if text.contains("link") || text.contains("placeholder") {
        "不支持链接或云端占位文件，请先下载到本地"
    } else if text.contains("10,000") || text.contains("32 levels") {
        "目录超过10000项或32层限制"
    } else if text.contains("filename") || text.contains("name") {
        "文件名不兼容另一台电脑"
    } else {
        "无法读取文件或写入保存位置，请重新选择"
    }
}
pub fn primary_connected(peer_supported: bool) {
    update(|v| {
        v.peer_supported = Some(peer_supported);
        v.channel = if peer_supported {
            "connecting"
        } else {
            "unavailable"
        }
        .into();
    });
}
pub fn primary_disconnected() {
    let tx = {
        let mut s = service().lock().unwrap();
        s.tx.take()
    };
    if let Some(tx) = tx {
        let _ = tx.try_send(Event::Stop);
    }
    update(|v| {
        v.peer_supported = None;
        v.peer_enabled = None;
        v.channel = "disconnected".into();
        for t in [&mut v.outgoing, &mut v.incoming].into_iter().flatten() {
            if active(&t.state) {
                t.state = "cancelled".into();
                t.message = Some("连接已断开".into());
            }
        }
    });
}
pub fn command(command: DropCommand) -> io::Result<()> {
    if let DropCommand::SetEnabled { enabled } = command {
        update(|v| v.enabled = enabled);
        let tx = service().lock().unwrap().tx.clone();
        if let Some(tx) = tx {
            tx.try_send(Event::Command(DropCommand::SetEnabled { enabled }))
                .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "drop lane busy"))?;
        }
        return Ok(());
    }
    let tx = {
        let s = service().lock().unwrap();
        if !s.view.enabled {
            return Err(invalid("file drop is disabled"));
        }
        if s.view.channel != "ready" {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "file drop channel is not ready",
            ));
        }
        s.tx.clone()
            .ok_or_else(|| invalid("drop lane unavailable"))?
    };
    tx.try_send(Event::Command(command))
        .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "drop lane busy"))
}
fn active(s: &str) -> bool {
    matches!(s, "preparing" | "waiting" | "sending" | "receiving")
}
fn random_id() -> io::Result<Id> {
    let mut id = [0; 16];
    getrandom::getrandom(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(id)
}
fn hex(id: &Id) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
fn transfer(offer: &Offer, state: &str) -> DropTransfer {
    DropTransfer {
        id: hex(&offer.id),
        state: state.into(),
        names: offer
            .entries
            .iter()
            .filter(|e| e.parent.is_none())
            .take(4)
            .map(|e| e.name.clone())
            .collect(),
        total_bytes: offer.total(),
        transferred_bytes: 0,
        message: None,
        saved_path: None,
    }
}
fn offer_frames(offer: &Offer) -> Vec<Frame> {
    offer
        .frames()
        .into_iter()
        .map(|mut f| {
            f.kind = if f.kind == FrameKind::FileOffer {
                FrameKind::DropOffer
            } else {
                FrameKind::DropManifest
            };
            f
        })
        .collect()
}
fn control(kind: FrameKind, id: Id) -> Frame {
    Frame::new(kind, id.to_vec())
}
fn control_id(f: &Frame) -> io::Result<Id> {
    if f.payload.len() != 16 {
        return Err(invalid("invalid drop control ID"));
    }
    Ok(f.payload[..].try_into().unwrap())
}
struct Pending {
    id: Id,
    len: usize,
    data: Vec<u8>,
    serial: u64,
}
struct Receiving {
    bundle: Option<TreeCache>,
    destination: PathBuf,
    index: usize,
    offset: u64,
    serial: u64,
    hash: Sha256,
}
#[derive(Default)]
struct Incoming {
    pending: Option<Pending>,
    offer: Option<Offer>,
    receive: Option<Receiving>,
}
impl Incoming {
    fn metadata(&mut self, f: Frame) -> io::Result<bool> {
        let mut r = Reader::new(&f.payload);
        match f.kind {
            FrameKind::DropOffer => {
                if self.offer.is_some() || self.pending.is_some() {
                    return Err(invalid("incoming drop already active"));
                }
                let id = r.id()?;
                let len = r.u32()? as usize;
                if !(2..=MAX_MANIFEST).contains(&len) || r.rest().is_empty() || r.rest().len() > len
                {
                    return Err(invalid("invalid drop manifest length"));
                }
                self.pending = Some(Pending {
                    id,
                    len,
                    data: r.rest().to_vec(),
                    serial: 1,
                });
            }
            FrameKind::DropManifest => {
                let p = self
                    .pending
                    .as_mut()
                    .ok_or_else(|| invalid("unsolicited manifest page"))?;
                if r.id()? != p.id
                    || r.u64()? != p.serial
                    || r.rest().is_empty()
                    || p.data.len() + r.rest().len() > p.len
                {
                    return Err(invalid("invalid drop manifest page"));
                }
                p.data.extend_from_slice(r.rest());
                p.serial += 1;
            }
            _ => return Err(invalid("unexpected manifest frame")),
        }
        if self.pending.as_ref().is_some_and(|p| p.data.len() == p.len) {
            let p = self.pending.take().unwrap();
            self.offer = Some(Offer::parse_metadata(p.id, &p.data)?);
            return Ok(true);
        }
        Ok(false)
    }
    fn accept(&mut self, destination: &Path) -> io::Result<()> {
        // Reserve bounded snapshot room for the local saved path, including JSON escapes.
        let text = destination.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "保存路径必须使用有效的UTF-8字符",
            )
        })?;
        let escaped: usize = text
            .chars()
            .map(|c| {
                if c.is_control() {
                    6
                } else if matches!(c, '"' | '\\') {
                    2
                } else {
                    c.len_utf8()
                }
            })
            .sum();
        if text.len() > 4096 || escaped > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "保存路径过长，请选择较浅的目录",
            ));
        }
        if !destination.is_absolute() {
            return Err(invalid("destination must be absolute"));
        }
        if self.receive.is_some() {
            return Err(invalid("drop already accepted"));
        }
        let offer = self
            .offer
            .as_ref()
            .ok_or_else(|| invalid("no validated offer"))?;
        if !crate::file_transfer::tree::local(&fs::symlink_metadata(destination)?)
            || !destination.is_dir()
        {
            return Err(invalid("choose an existing local directory"));
        }
        if available_space(destination)? < offer.total().saturating_add(1024 * 1024) {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "insufficient destination space",
            ));
        }
        let mut bundle = TreeCache::new_drop(destination, offer)?;
        let index = offer
            .entries
            .iter()
            .position(|e| !e.directory)
            .unwrap_or(offer.entries.len());
        if let Some(e) = offer.entries.get(index) {
            bundle.start(index, e.len)?;
        }
        self.receive = Some(Receiving {
            bundle: Some(bundle),
            destination: destination.to_owned(),
            index,
            offset: 0,
            serial: 0,
            hash: Sha256::new(),
        });
        Ok(())
    }
    fn data(&mut self, f: Frame) -> io::Result<Option<PathBuf>> {
        let offer = self
            .offer
            .as_ref()
            .ok_or_else(|| invalid("data without offer"))?;
        let a = self
            .receive
            .as_mut()
            .ok_or_else(|| invalid("data before explicit acceptance"))?;
        let mut r = Reader::new(&f.payload);
        if r.id()? != offer.id || r.u64()? != a.serial {
            return Err(invalid("drop identity or serial mismatch"));
        }
        match f.kind {
            FrameKind::FileData => {
                let index = r.u16()? as usize;
                let offset = r.u64()?;
                let data = r.rest();
                if index != a.index
                    || offset != a.offset
                    || data.is_empty()
                    || data.len() > protocol::CHUNK_SIZE
                    || index >= offer.entries.len()
                {
                    return Err(invalid("invalid drop chunk"));
                }
                a.bundle.as_mut().unwrap().write(data)?;
                a.hash.update(data);
                a.offset += data.len() as u64;
            }
            FrameKind::FileSeal => {
                let index = r.u16()? as usize;
                let hash = r.take(32)?;
                r.end()?;
                if index != a.index
                    || index >= offer.entries.len()
                    || a.offset != offer.entries[index].len
                    || a.hash.clone().finalize().as_slice() != hash
                {
                    return Err(invalid("file length or SHA-256 mismatch"));
                }
                a.bundle.as_mut().unwrap().seal()?;
                a.index += 1;
                while offer.entries.get(a.index).is_some_and(|e| e.directory) {
                    a.index += 1;
                }
                a.offset = 0;
                a.hash = Sha256::new();
                if let Some(e) = offer.entries.get(a.index) {
                    a.bundle.as_mut().unwrap().start(a.index, e.len)?;
                }
            }
            FrameKind::FileCommit => {
                r.end()?;
                if a.index != offer.entries.len() {
                    return Err(invalid("incomplete batch"));
                }
                let (root, _) = a.bundle.take().unwrap().finish()?;
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                for n in 0..10000 {
                    let name = if n == 0 {
                        format!("CrossDesk-{stamp}")
                    } else {
                        format!("CrossDesk-{stamp}-{n}")
                    };
                    let path = a.destination.join(name);
                    match rename_exclusive(&root, &path) {
                        Ok(()) => {
                            // Publication already succeeded. Attribute failures must never orphan a saved batch.
                            let _ = crate::file_transfer::tree_cache::set_hidden(&path, false);
                            return Ok(Some(path));
                        }
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                        Err(e) => {
                            let _ = fs::remove_dir_all(&root);
                            return Err(e);
                        }
                    }
                }
                let _ = fs::remove_dir_all(&root);
                return Err(invalid("cannot allocate destination folder"));
            }
            _ => return Err(invalid("unexpected drop data")),
        }
        a.serial += 1;
        Ok(None)
    }
}
struct Outgoing {
    id: Id,
    cancel: Arc<AtomicBool>,
    tree: Option<SourceTree>,
    since: Instant,
    worker: Option<thread::JoinHandle<()>>,
    acks: Option<mpsc::SyncSender<wire::Ack>>,
}
enum Event {
    Command(DropCommand),
    Frame(Frame),
    Prepared(Id, io::Result<SourceTree>),
    Progress(Id, u64),
    Sent(Id, io::Result<()>),
    Stop,
}
struct Lane {
    writer: SharedWriter,
    tx: mpsc::SyncSender<Event>,
    incoming: Incoming,
    outgoing: Option<Outgoing>,
    incoming_progress: Instant,
    peer_enabled: bool,
    ignored: Option<Pending>,
}
impl Lane {
    fn publish(&self, send: bool, f: impl FnOnce(&mut DropTransfer)) {
        update(|v| {
            if let Some(t) = if send {
                &mut v.outgoing
            } else {
                &mut v.incoming
            } {
                f(t);
            }
        });
    }
    fn fail(&mut self, send: bool, state: &str, message: &str) {
        if send {
            if self.outgoing.is_none() {
                return;
            }
            if let Some(mut o) = self.outgoing.take() {
                o.cancel.store(true, Ordering::Release);
                if let Some(w) = o.worker.take() {
                    let _ = w.join();
                }
            }
        } else {
            if self.incoming.offer.is_none() && self.incoming.pending.is_none() {
                return;
            }
            self.incoming = Incoming::default();
        }
        self.publish(send, |t| {
            t.state = state.into();
            t.message = Some(message.into());
        });
    }
    fn cancel_all(&mut self) {
        if let Some(o) = self.outgoing.as_ref() {
            let _ = self.writer.write(control(FrameKind::DropCancel, o.id));
        }
        if let Some(o) = self.incoming.offer.as_ref() {
            let _ = self.writer.write(control(FrameKind::DropCancel, o.id));
        }
        self.fail(true, "cancelled", "投送已停止");
        self.fail(false, "cancelled", "投送已停止");
    }
    fn handle_command(&mut self, c: DropCommand) -> io::Result<()> {
        let reject = matches!(c, DropCommand::Reject { .. });
        match c {
            DropCommand::SetEnabled { enabled } => {
                if !enabled {
                    self.cancel_all();
                }
                self.writer
                    .write(Frame::new(FrameKind::DropStatus, vec![enabled as u8]))?;
            }
            DropCommand::Send { paths } => {
                if !enabled() || !self.peer_enabled || self.outgoing.is_some() {
                    return Err(invalid("drop disabled or outgoing transfer busy"));
                }
                let id = random_id()?;
                let cancel = Arc::new(AtomicBool::new(false));
                let cc = cancel.clone();
                let tx = self.tx.clone();
                update(|v| {
                    v.outgoing = Some(DropTransfer {
                        id: hex(&id),
                        state: "preparing".into(),
                        names: Vec::new(),
                        total_bytes: 0,
                        transferred_bytes: 0,
                        message: None,
                        saved_path: None,
                    })
                });
                let worker = thread::Builder::new()
                    .name("drop-scan".into())
                    .spawn(move || {
                        let result = SourceTree::scan(&paths, &mut || cc.load(Ordering::Acquire));
                        deliver(&tx, Event::Prepared(id, result), &cc);
                    })?;
                self.outgoing = Some(Outgoing {
                    id,
                    cancel,
                    tree: None,
                    since: Instant::now(),
                    worker: Some(worker),
                    acks: None,
                });
            }
            DropCommand::Accept { id, directory } => {
                let offer = self
                    .incoming
                    .offer
                    .as_ref()
                    .ok_or_else(|| invalid("no incoming drop"))?;
                if hex(&offer.id) != id || !enabled() {
                    return Err(invalid("stale acceptance"));
                }
                let batch = offer.id;
                if let Err(e) = self.incoming.accept(&directory) {
                    self.fail(false, "failed", friendly(&e));
                    self.writer.write(control(FrameKind::DropReject, batch))?;
                    return Err(e);
                }
                self.publish(false, |t| t.state = "receiving".into());
                self.incoming_progress = Instant::now();
                self.writer.write(control(FrameKind::DropAccept, batch))?;
            }
            DropCommand::Reject { id } | DropCommand::Cancel { id } => {
                let mut found = false;
                if self
                    .incoming
                    .offer
                    .as_ref()
                    .is_some_and(|o| hex(&o.id) == id)
                {
                    let batch = self.incoming.offer.as_ref().unwrap().id;
                    self.writer.write(control(
                        if reject {
                            FrameKind::DropReject
                        } else {
                            FrameKind::DropCancel
                        },
                        batch,
                    ))?;
                    self.fail(
                        false,
                        if reject { "rejected" } else { "cancelled" },
                        "已拒绝本次投送",
                    );
                    found = true;
                }
                if self.outgoing.as_ref().is_some_and(|o| hex(&o.id) == id) {
                    let batch = self.outgoing.as_ref().unwrap().id;
                    self.writer.write(control(FrameKind::DropCancel, batch))?;
                    self.fail(true, "cancelled", "投送已取消");
                    found = true;
                }
                if !found {
                    return Err(invalid("unknown drop transfer"));
                }
            }
        }
        Ok(())
    }
    fn handle_frame(&mut self, f: Frame) -> io::Result<()> {
        match f.kind {
            FrameKind::Ping => self.writer.write(Frame::new(FrameKind::Pong, Vec::new()))?,
            FrameKind::Pong => {}
            FrameKind::DropStatus => {
                if f.payload.len() != 1 || f.payload[0] > 1 {
                    return Err(invalid("invalid drop status"));
                }
                self.peer_enabled = f.payload[0] == 1;
                update(|v| {
                    v.peer_enabled = Some(self.peer_enabled);
                    v.channel = "ready".into();
                });
                if !self.peer_enabled {
                    self.cancel_all();
                }
            }
            FrameKind::DropOffer | FrameKind::DropManifest => {
                if f.kind == FrameKind::DropOffer
                    && (!enabled()
                        || self.incoming.offer.is_some()
                        || self.incoming.pending.is_some())
                {
                    let mut r = Reader::new(&f.payload);
                    let id = r.id()?;
                    let len = r.u32()? as usize;
                    if !(2..=MAX_MANIFEST).contains(&len)
                        || r.rest().is_empty()
                        || r.rest().len() > len
                    {
                        return Err(invalid("invalid declined offer length"));
                    }
                    self.writer.write(control(FrameKind::DropReject, id))?;
                    self.ignored = if r.rest().len() < len {
                        Some(Pending {
                            id,
                            len,
                            data: Vec::new(),
                            serial: 1,
                        })
                    } else {
                        None
                    };
                    // Only bounded counters are retained for rejected manifest continuations.
                    if let Some(p) = self.ignored.as_mut() {
                        p.len -= r.rest().len();
                    }
                    self.publish(false, |t| {
                        t.message = Some("另一个投送已拒绝：当前接收任务尚未结束".into())
                    });
                    return Ok(());
                }
                if f.kind == FrameKind::DropManifest {
                    let mut r = Reader::new(&f.payload);
                    let id = r.id()?;
                    if self.ignored.as_ref().is_some_and(|p| p.id == id) {
                        let p = self.ignored.as_mut().unwrap();
                        if r.u64()? != p.serial || r.rest().is_empty() || r.rest().len() > p.len {
                            return Err(invalid("invalid declined manifest page"));
                        }
                        p.serial += 1;
                        p.len -= r.rest().len();
                        if p.len == 0 {
                            self.ignored = None;
                        }
                        return Ok(());
                    }
                }
                if self.incoming.metadata(f)? {
                    let t = transfer(self.incoming.offer.as_ref().unwrap(), "waiting");
                    update(|v| v.incoming = Some(t));
                }
                self.incoming_progress = Instant::now();
            }
            FrameKind::DropAccept => {
                let id = control_id(&f)?;
                let o = self
                    .outgoing
                    .as_mut()
                    .ok_or_else(|| invalid("unsolicited accept"))?;
                if o.id != id {
                    return Err(invalid("accept ID mismatch"));
                }
                let tree = o
                    .tree
                    .take()
                    .ok_or_else(|| invalid("duplicate or premature accept"))?;
                if let Some(w) = o.worker.take() {
                    let _ = w.join();
                }
                let cancel = o.cancel.clone();
                let (ack_tx, ack_rx) = mpsc::sync_channel(1);
                o.acks = Some(ack_tx);
                let writer = self.writer.clone();
                let tx = self.tx.clone();
                self.publish(true, |t| t.state = "sending".into());
                self.outgoing.as_mut().unwrap().since = Instant::now();
                let worker = thread::Builder::new()
                    .name("drop-send".into())
                    .spawn(move || {
                        let result = send_tree(&writer, &tree, id, &cancel, &tx, &ack_rx);
                        deliver(&tx, Event::Sent(id, result), &cancel);
                    })?;
                self.outgoing.as_mut().unwrap().worker = Some(worker);
            }
            FrameKind::DropReject | FrameKind::DropCancel => {
                let id = control_id(&f)?;
                let state = if f.kind == FrameKind::DropReject {
                    "rejected"
                } else {
                    "cancelled"
                };
                if self.outgoing.as_ref().is_some_and(|o| o.id == id) {
                    self.fail(true, state, "对方已拒绝或取消投送");
                }
                if self.incoming.offer.as_ref().is_some_and(|o| o.id == id)
                    || self.incoming.pending.as_ref().is_some_and(|p| p.id == id)
                {
                    self.fail(false, state, "对方已取消投送");
                }
            }
            FrameKind::FileAck => {
                let ack = wire::parse_ack(&f.payload)?;
                if let Some(o) = self.outgoing.as_ref().filter(|o| o.id == ack.id) {
                    if let Some(tx) = &o.acks {
                        let _ = tx.try_send(ack);
                    }
                }
            }
            FrameKind::DropResult => {
                if f.payload.len() != 17 || f.payload[16] > 1 {
                    return Err(invalid("invalid drop result"));
                }
                let id: Id = f.payload[..16].try_into().unwrap();
                if self.outgoing.as_ref().is_some_and(|o| o.id == id) {
                    if f.payload[16] == 0 {
                        self.fail(true, "saved", "对方已保存");
                    } else {
                        self.fail(true, "failed", "对方未能保存本次投送");
                    }
                }
            }
            FrameKind::FileData | FrameKind::FileSeal | FrameKind::FileCommit => {
                let identity = wire::identity(&f)?;
                let id = self.incoming.offer.as_ref().map(|o| o.id);
                let chunk = if f.kind == FrameKind::FileData {
                    f.payload.len().saturating_sub(34) as u64
                } else {
                    0
                };
                match self.incoming.data(f) {
                    Ok(saved) => {
                        self.incoming_progress = Instant::now();
                        progress(false, chunk, false);
                        if let Some(path) = saved {
                            self.publish(false, |t| {
                                t.state = "saved".into();
                                t.saved_path = Some(path);
                            });
                            self.incoming = Incoming::default();
                            self.writer.write(wire::ack(identity.0, identity.1, 0))?;
                            let mut p = id.unwrap().to_vec();
                            p.push(0);
                            self.writer.write(Frame::new(FrameKind::DropResult, p))?;
                        } else {
                            self.writer.write(wire::ack(identity.0, identity.1, 0))?;
                        }
                    }
                    Err(_) => {
                        self.fail(false, "failed", "文件校验失败或保存目录不可写");
                        self.writer.write(wire::ack(identity.0, identity.1, 2))?;
                        if let Some(id) = id {
                            let mut p = id.to_vec();
                            p.push(1);
                            self.writer.write(Frame::new(FrameKind::DropResult, p))?;
                        }
                    }
                }
            }
            _ => return Err(invalid("unexpected frame on drop lane")),
        }
        Ok(())
    }
    fn tick(&mut self) {
        if let Some(o) = self.outgoing.as_ref() {
            let state = view().outgoing.map(|t| t.state).unwrap_or_default();
            let limit = if state == "waiting" {
                Duration::from_secs(120)
            } else {
                Duration::from_secs(30)
            };
            if o.since.elapsed() > limit {
                let id = o.id;
                let _ = self.writer.write(control(FrameKind::DropCancel, id));
                self.fail(true, "failed", "投送超时，请重新发送");
            }
        }
        if self.incoming.offer.is_some() || self.incoming.pending.is_some() {
            let limit = if self.incoming.receive.is_none() && self.incoming.offer.is_some() {
                Duration::from_secs(120)
            } else {
                Duration::from_secs(30)
            };
            if self.incoming_progress.elapsed() > limit {
                if let Some(o) = &self.incoming.offer {
                    let _ = self.writer.write(control(FrameKind::DropCancel, o.id));
                }
                self.fail(false, "failed", "投送超时，请重新发送");
            }
        }
    }
}
fn deliver(tx: &mpsc::SyncSender<Event>, mut event: Event, cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        match tx.try_send(event) {
            Ok(()) => return,
            Err(mpsc::TrySendError::Full(e)) => {
                event = e;
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}
fn send_tree(
    writer: &SharedWriter,
    tree: &SourceTree,
    id: Id,
    cancel: &AtomicBool,
    tx: &mpsc::SyncSender<Event>,
    ack_rx: &mpsc::Receiver<wire::Ack>,
) -> io::Result<()> {
    let mut cancelled = || cancel.load(Ordering::Acquire);
    let exchange = |f: Frame| -> io::Result<()> {
        crate::file_transfer::tree::cancelled(&mut || cancel.load(Ordering::Acquire))?;
        let (_, serial) = wire::identity(&f)?;
        writer.write(f)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            crate::file_transfer::tree::cancelled(&mut || cancel.load(Ordering::Acquire))?;
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "接收端确认超时"));
            }
            match ack_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ack) if ack.id == id && ack.serial == serial => {
                    return if ack.status == 2 {
                        Err(invalid("接收端拒绝了文件内容"))
                    } else {
                        Ok(())
                    };
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(io::Error::new(io::ErrorKind::BrokenPipe, "投送通道已断开")),
            }
        }
    };
    tree.verify(&mut cancelled)?;
    let mut serial = 0;
    let mut total = 0;
    let mut buffer = vec![0; protocol::CHUNK_SIZE];
    for (index, e) in tree.entries.iter().enumerate() {
        if e.directory {
            continue;
        }
        let mut file = tree.open(index)?;
        let mut offset = 0;
        let mut hash = Sha256::new();
        while offset < e.len {
            crate::file_transfer::tree::cancelled(&mut cancelled)?;
            let len = (e.len - offset).min(protocol::CHUNK_SIZE as u64) as usize;
            let n = file.read(&mut buffer[..len])?;
            if n == 0 {
                return Err(invalid("source truncated"));
            }
            hash.update(&buffer[..n]);
            exchange(wire::request(
                FrameKind::FileData,
                id,
                serial,
                index as u16,
                offset,
                &buffer[..n],
            ))?;
            serial += 1;
            offset += n as u64;
            total += n as u64;
            let _ = tx.try_send(Event::Progress(id, total));
        }
        tree.verify_open(index, &file)?;
        drop(file);
        exchange(wire::request(
            FrameKind::FileSeal,
            id,
            serial,
            index as u16,
            0,
            &hash.finalize(),
        ))?;
        serial += 1;
    }
    tree.verify(&mut cancelled)?;
    exchange(wire::request(FrameKind::FileCommit, id, serial, 0, 0, &[]))
}
/// Called only after a fresh authenticated attachment. One reader and one bounded actor;
/// at most one scanner/sender worker exists per lane.
pub fn run_lane(mut stream: SecureStream, stop: Arc<AtomicBool>) -> io::Result<()> {
    let shutdown = stream.try_clone()?;
    let writer = SharedWriter::new(stream.try_clone()?);
    let (tx, rx) = mpsc::sync_channel(32);
    let generation = {
        let mut s = service().lock().unwrap();
        if s.tx.is_some() {
            return Err(invalid("drop lane already attached"));
        }
        s.generation += 1;
        s.tx = Some(tx.clone());
        s.generation
    };
    update(|v| v.channel = "connecting".into());
    let local_stop = Arc::new(AtomicBool::new(false));
    let actor_stop = local_stop.clone();
    let external = stop.clone();
    let actor = thread::Builder::new()
        .name("drop-lane".into())
        .spawn(move || {
            let mut lane = Lane {
                writer,
                tx,
                incoming: Incoming::default(),
                outgoing: None,
                incoming_progress: Instant::now(),
                peer_enabled: false,
                ignored: None,
            };
            let mut heartbeat = Instant::now();
            let mut result = lane
                .writer
                .write(Frame::new(FrameKind::DropStatus, vec![enabled() as u8]));
            while result.is_ok()
                && !actor_stop.load(Ordering::Acquire)
                && !external.load(Ordering::Acquire)
            {
                lane.tick();
                if heartbeat.elapsed() >= Duration::from_secs(1) {
                    result = lane.writer.write(Frame::new(FrameKind::Ping, Vec::new()));
                    heartbeat = Instant::now();
                    if result.is_err() {
                        break;
                    }
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Event::Stop) => break,
                    Ok(Event::Command(c)) => {
                        let sending = matches!(c, DropCommand::Send { .. });
                        if let Err(e) = lane.handle_command(c) {
                            update(|v| {
                                let t = if sending {
                                    &mut v.outgoing
                                } else {
                                    &mut v.incoming
                                };
                                if let Some(t) = t {
                                    t.message = Some(friendly(&e).into());
                                }
                            });
                        }
                    }
                    Ok(Event::Frame(f)) => result = lane.handle_frame(f),
                    Ok(Event::Prepared(id, prepared)) => {
                        if !lane.outgoing.as_ref().is_some_and(|o| o.id == id) {
                            continue;
                        }
                        match prepared {
                            Ok(tree) => {
                                let offer = tree.offer(id);
                                result = offer.validate();
                                if result.is_ok() {
                                    update(|v| v.outgoing = Some(transfer(&offer, "waiting")));
                                    let o = lane.outgoing.as_mut().unwrap();
                                    if let Some(worker) = o.worker.take() {
                                        let _ = worker.join();
                                    }
                                    let cancel = o.cancel.clone();
                                    let writer = lane.writer.clone();
                                    let tx = lane.tx.clone();
                                    o.tree = Some(tree);
                                    o.since = Instant::now();
                                    // Keep the actor reading during duplex multi-page manifests.
                                    match thread::Builder::new().name("drop-offer".into()).spawn(
                                        move || {
                                            let sent = (|| {
                                                for f in offer_frames(&offer) {
                                                    crate::file_transfer::tree::cancelled(
                                                        &mut || cancel.load(Ordering::Acquire),
                                                    )?;
                                                    writer.write(f)?;
                                                }
                                                Ok(())
                                            })(
                                            );
                                            // Acceptance can immediately join this worker; never wait on the actor's queue here.
                                            let _ = tx.try_send(Event::Sent(id, sent));
                                        },
                                    ) {
                                        Ok(worker) => o.worker = Some(worker),
                                        Err(e) => result = Err(e),
                                    }
                                }
                            }
                            Err(e) => {
                                lane.fail(true, "failed", friendly(&e));
                            }
                        }
                    }
                    Ok(Event::Progress(id, n)) => {
                        if let Some(o) = lane.outgoing.as_mut().filter(|o| o.id == id) {
                            o.since = Instant::now();
                            progress(true, n, true);
                        }
                    }
                    Ok(Event::Sent(id, sent)) => {
                        if lane.outgoing.as_ref().is_some_and(|o| o.id == id) {
                            if let Err(e) = sent {
                                let _ = lane.writer.write(control(FrameKind::DropCancel, id));
                                lane.fail(true, "failed", friendly(&e));
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            actor_stop.store(true, Ordering::Release);
            let _ = shutdown.shutdown(Shutdown::Both);
            lane.cancel_all();
            result
        })?;
    let mut result = Ok(());
    while !stop.load(Ordering::Acquire) && !local_stop.load(Ordering::Acquire) {
        match protocol::read_frame(&mut stream) {
            Ok(f) => {
                let sender = service().lock().unwrap().tx.clone();
                if sender.is_none_or(|tx| tx.send(Event::Frame(f)).is_err()) {
                    break;
                }
            }
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    local_stop.store(true, Ordering::Release);
    let _ = stream.shutdown(Shutdown::Both);
    let tx = service().lock().unwrap().tx.clone();
    if let Some(tx) = tx {
        let _ = tx.try_send(Event::Stop);
    }
    let actor_result = actor
        .join()
        .unwrap_or_else(|_| Err(io::Error::other("drop actor panicked")));
    {
        let mut s = service().lock().unwrap();
        if s.generation == generation {
            s.tx = None;
        }
    }
    update(|v| {
        if v.peer_supported.is_some() {
            v.channel = "unavailable".into();
        }
    });
    result.and(actor_result)
}

#[cfg(all(test, any(target_os = "macos", windows)))]
#[path = "edge_drop_native_tests.rs"]
mod native_tests;
