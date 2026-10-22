// CrossDesk simple connection controller, 2026-10-21. GPL-3.0-only.
use crate::{
    config::{AppConfig, Role},
    control::{ControlBackend, ProcessBackend},
    discovery::{Candidate, ConnectPolicy, Discovery},
    pairing::PairingService,
    service_event::ServiceState,
    trust_store::{DeviceId, TrustedStore, hex},
};
use std::{
    collections::HashSet,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const VERSION_MISMATCH: &str = "设备版本不兼容，请将两端 CrossDesk 都升级到 0.5.0 后重试";
mod preferences;
pub use preferences::Preferences;
mod pair_job;
use pair_job::PairJob;
mod actor;
pub use actor::Controller;
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub paired: bool,
    pub compatible: bool,
}
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectView {
    pub platform: String,
    pub state: String,
    pub devices: Vec<DeviceView>,
    pub selected_id: Option<String>,
    pub pairing_code: Option<String>,
    pub seconds_left: u16,
    pub auto_connect: bool,
    pub image_clipboard: bool,
    pub file_clipboard: bool,
    pub error: Option<String>,
    pub busy: bool,
    pub logs: Vec<String>,
    pub app_version: String,
    pub transfer: Option<crate::service_event::TransferEvent>,
}
impl ConnectView {
    pub fn loading() -> Self {
        Self {
            platform: crate::platform::Platform::local().as_str().into(),
            state: "starting".into(),
            devices: Vec::new(),
            selected_id: None,
            pairing_code: None,
            seconds_left: 0,
            auto_connect: true,
            image_clipboard: true,
            file_clipboard: true,
            error: None,
            busy: false,
            logs: Vec::new(),
            transfer: None,
            app_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}
pub trait ServiceControl: Send {
    fn start(&mut self, config: &AppConfig, key: &Path, image: bool, files: bool)
    -> io::Result<()>;
    fn stop(&mut self) -> io::Result<()>;
    fn poll(&mut self) -> (ServiceState, Option<String>);
    fn logs(&self) -> Vec<String>;
    fn transfer(&self) -> Option<crate::service_event::TransferEvent> {
        None
    }
}
impl ServiceControl for ProcessBackend {
    fn transfer(&self) -> Option<crate::service_event::TransferEvent> {
        self.transfer_status()
    }
    fn start(&mut self, c: &AppConfig, k: &Path, i: bool, f: bool) -> io::Result<()> {
        self.start_link(c, k, i, f)
    }
    fn stop(&mut self) -> io::Result<()> {
        ControlBackend::stop(self)
    }
    fn poll(&mut self) -> (ServiceState, Option<String>) {
        self.collect_logs();
        self.is_running();
        (self.state(), self.error())
    }
    fn logs(&self) -> Vec<String> {
        ControlBackend::logs(self).iter().cloned().collect()
    }
}
pub enum Command {
    Select(DeviceId),
    Pair(String),
    Connect,
    Disconnect,
    Refresh,
    Forget,
    Options {
        auto: bool,
        image: bool,
        files: bool,
    },
    Manual {
        address: SocketAddr,
        key: PathBuf,
    },
    Shutdown,
}
pub trait DiscoverySource: Send {
    fn snapshot(&self) -> Vec<Candidate>;
    fn announce(&mut self, id: DeviceId, name: &str, port: Option<u16>) -> io::Result<()>;
}
impl DiscoverySource for Discovery {
    fn snapshot(&self) -> Vec<Candidate> {
        Discovery::snapshot(self)
    }
    fn announce(&mut self, id: DeviceId, name: &str, port: Option<u16>) -> io::Result<()> {
        Discovery::announce(self, id, name, port)
    }
}
pub struct Core {
    platform: String,
    store: Arc<TrustedStore>,
    backend: Box<dyn ServiceControl>,
    pub candidates: Vec<Candidate>,
    discovery: Option<Box<dyn DiscoverySource>>,
    incompatible: HashSet<DeviceId>,
    pairing: Option<PairingService>,
    job: Option<PairJob>,
    selected: Option<DeviceId>,
    trusted: Option<DeviceId>,
    policy: ConnectPolicy,
    paused_by_user: bool,
    prefs: Preferences,
    phase: ServiceState,
    error: Option<String>,
    discovery_error: Option<String>,
    opened: Instant,
    current_address: Option<SocketAddr>,
    endpoint_index: usize,
    announced_pairing_port: Option<u16>,
    name: String,
}
impl Core {
    pub fn from_parts(
        platform: &str,
        store: Arc<TrustedStore>,
        backend: Box<dyn ServiceControl>,
    ) -> io::Result<Self> {
        let prefs = Preferences::load(&store)?;
        let remembered = match store.paired()? {
            Some(p) => Some(p),
            None => store.pending()?,
        };
        let trusted = remembered.map(|p| p.id);
        let policy = ConnectPolicy::new(trusted, prefs.auto_connect);
        Ok(Self {
            platform: platform.into(),
            store,
            backend,
            candidates: Vec::new(),
            discovery: None,
            incompatible: HashSet::new(),
            pairing: None,
            job: None,
            selected: None,
            trusted,
            policy,
            paused_by_user: false,
            prefs,
            phase: ServiceState::Stopped,
            error: None,
            discovery_error: None,
            opened: Instant::now(),
            current_address: None,
            endpoint_index: 0,
            announced_pairing_port: None,
            name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
        })
    }
    fn discover(&mut self) {
        match Discovery::browse() {
            Ok(d) => {
                self.discovery = Some(Box::new(d));
                self.discovery_error = None;
            }
            Err(_) => {
                self.discovery_error =
                    Some("暂时无法发现设备，请允许本地网络访问，或使用高级设置".into())
            }
        }
        self.announce();
    }
    fn announce(&mut self) {
        let port = self
            .pairing
            .as_ref()
            .and_then(|p| p.view().code.map(|_| p.local_addr().port()));
        self.announced_pairing_port = port;
        if self.platform == "windows" {
            if let Some(d) = &mut self.discovery {
                if let Ok(id) = self.store.local_id() {
                    if d.announce(id, &self.name, port).is_err() {
                        self.discovery_error = Some("设备公告失败，请检查本地网络访问".into());
                    }
                }
            }
        }
    }
    fn start_pairing(&mut self) -> io::Result<()> {
        if self.platform != "windows" {
            return Ok(());
        }
        if self.store.paired()?.is_some() {
            return Ok(());
        }
        self.pairing.take();
        self.store.host_key()?;
        self.pairing = Some(PairingService::start(
            "0.0.0.0:24921".parse().unwrap(),
            self.store.clone(),
        )?);
        self.paused_by_user = false;
        self.policy.manual_connect();
        self.announce();
        Ok(())
    }
    pub fn tick(&mut self) -> io::Result<()> {
        self.tick_at(Instant::now())
    }
    pub fn tick_at(&mut self, now: Instant) -> io::Result<()> {
        if let Some(d) = &self.discovery {
            self.candidates = d.snapshot();
        }
        self.incompatible.retain(|id| {
            self.trusted == Some(*id)
                || self.selected == Some(*id)
                || self.candidates.iter().any(|p| p.id == *id)
        });
        for candidate in &self.candidates {
            if candidate.protocol != crate::protocol::VERSION {
                self.incompatible.insert(candidate.id);
            } else if !self
                .candidates
                .iter()
                .any(|p| p.id == candidate.id && p.protocol != crate::protocol::VERSION)
            {
                self.incompatible.remove(&candidate.id);
            }
        }
        if let Some(job) = &self.job {
            if let Some(result) = job.result() {
                self.job.take();
                match result {
                    Ok(()) => {
                        if let Some(id) = self.selected {
                            if let Some(p) = self.candidates.iter().find(|p| p.id == id) {
                                self.prefs.peer_name = p.name.clone();
                            }
                        }
                        self.prefs.save(&self.store)?;
                        self.error = None;
                    }
                    Err(e) => {
                        self.error = Some(if e.kind() == io::ErrorKind::PermissionDenied {
                            "配对码不正确或已失效，请重试".into()
                        } else {
                            format!("配对未完成：{e}")
                        })
                    }
                }
            }
        }
        if self.pairing.as_ref().is_some_and(|p| p.view().completed) {
            self.pairing.take();
            self.announce();
        }
        let pairing_port = self
            .pairing
            .as_ref()
            .and_then(|p| p.view().code.map(|_| p.local_addr().port()));
        if pairing_port != self.announced_pairing_port {
            self.announce();
        }
        let paired = match self.store.paired()? {
            Some(p) => Some(p),
            None => self.store.pending()?,
        };
        let id = paired.as_ref().map(|p| p.id);
        if id != self.trusted {
            let first_pair = self.trusted.is_none() && id.is_some();
            self.trusted = id;
            self.policy = ConnectPolicy::new(id, self.prefs.auto_connect || first_pair);
            if self.paused_by_user {
                self.policy.manual_stop();
            }
            self.error = None;
            self.announce();
        }
        let (phase, message) = self.backend.poll();
        self.phase = phase;
        if phase == ServiceState::Connected {
            if self.store.paired()?.is_none() {
                if let Some(id) = self.trusted {
                    self.store.commit(id)?;
                    self.pairing.take();
                    self.announce();
                }
            }
            self.policy.connected();
            self.error = None;
            if self.platform == "macos" && self.current_address != self.prefs.last_address {
                self.prefs.last_address = self.current_address;
                self.prefs.save(&self.store)?;
            }
        }
        if phase == ServiceState::Error {
            self.error = Some(message.unwrap_or_else(|| "连接服务已退出，请重新连接".into()));
            self.policy.manual_stop();
            self.backend.stop()?;
            self.phase = ServiceState::Error;
        }
        if phase == ServiceState::Retrying {
            self.backend.stop()?;
            self.phase = ServiceState::Retrying;
            self.endpoint_index = self.endpoint_index.wrapping_add(1);
            self.policy.retry(now);
        }
        if let Some(peer) = paired {
            if self.incompatible.contains(&peer.id) {
                self.error = Some(VERSION_MISMATCH.into());
                return Ok(());
            }
            if self.error.as_deref() == Some(VERSION_MISMATCH) {
                self.error = None;
            }
            if self.policy.should_connect(peer.id, now) {
                let address = if self.platform == "windows" {
                    Some("0.0.0.0:24920".parse().unwrap())
                } else {
                    self.candidates
                        .iter()
                        .find(|p| p.id == peer.id)
                        .and_then(|p| {
                            if p.endpoints.is_empty() {
                                None
                            } else {
                                p.endpoints
                                    .get(self.endpoint_index % p.endpoints.len())
                                    .copied()
                            }
                        })
                        .or_else(|| {
                            if self.opened.elapsed() >= Duration::from_secs(8) {
                                self.prefs.last_address
                            } else {
                                None
                            }
                        })
                };
                if let Some(address) = address {
                    let mut config = AppConfig::default();
                    config.role = if self.platform == "windows" {
                        Role::Server
                    } else {
                        Role::Client
                    };
                    config.server = address.to_string();
                    self.backend.start(
                        &config,
                        &peer.key_path,
                        self.prefs.image_clipboard,
                        self.prefs.file_clipboard,
                    )?;
                    self.current_address = Some(address);
                    self.phase = ServiceState::Starting;
                    self.policy.started();
                }
            }
        }
        Ok(())
    }
    pub fn view(&self) -> ConnectView {
        let paired = if self.platform == "windows" && self.pairing.is_some() {
            None
        } else {
            self.trusted
        };
        let mut devices: Vec<_> = self
            .candidates
            .iter()
            .map(|p| DeviceView {
                id: hex(&p.id),
                name: if Some(p.id) == paired && !self.prefs.peer_name.is_empty() {
                    self.prefs.peer_name.clone()
                } else {
                    p.name.clone()
                },
                paired: Some(p.id) == paired,
                compatible: p.protocol == crate::protocol::VERSION,
            })
            .collect();
        if let Some(id) = paired {
            if !devices.iter().any(|p| p.id == hex(&id)) {
                devices.push(DeviceView {
                    id: hex(&id),
                    name: if self.prefs.peer_name.is_empty() {
                        if self.platform == "windows" {
                            "已配对的 Mac".into()
                        } else {
                            "已配对的 Windows".into()
                        }
                    } else {
                        self.prefs.peer_name.clone()
                    },
                    paired: true,
                    compatible: !self.incompatible.contains(&id),
                });
            }
        }
        let mut view = ConnectView::loading();
        view.platform = self.platform.clone();
        view.devices = devices;
        view.selected_id = self.selected.map(|id| hex(&id));
        view.auto_connect = self.prefs.auto_connect;
        view.image_clipboard = self.prefs.image_clipboard;
        view.file_clipboard = self.prefs.file_clipboard;
        view.error = self.error.clone().or_else(|| self.discovery_error.clone());
        view.busy = self.job.is_some();
        view.logs = self.backend.logs();
        view.transfer = self.backend.transfer();
        let pair_view = self.pairing.as_ref().map(PairingService::view);
        if let Some(p) = pair_view {
            view.pairing_code = p.code;
            view.seconds_left = p.seconds_left;
            if view.error.is_none() {
                view.error = p.error;
            }
        }
        view.state = if self.phase == ServiceState::Connected {
            "connected"
        } else if self.policy.paused() {
            if self.error.is_some() {
                "error"
            } else {
                "paused"
            }
        } else if matches!(
            self.phase,
            ServiceState::Starting | ServiceState::Connecting
        ) {
            "connecting"
        } else if self.phase == ServiceState::Listening {
            "listening"
        } else if self.phase == ServiceState::Retrying {
            "retrying"
        } else if self.trusted.is_some() {
            if !self.prefs.auto_connect {
                "paused"
            } else {
                "searching"
            }
        } else if self.platform == "windows" {
            "hosting"
        } else if self.selected.is_some() {
            "pairing"
        } else {
            "discovering"
        }
        .into();
        view
    }
    pub fn disconnect(&mut self) -> io::Result<()> {
        self.paused_by_user = true;
        self.policy.manual_stop();
        self.job.take();
        self.pairing.take();
        self.announce();
        self.backend.stop()?;
        self.phase = ServiceState::Stopped;
        self.error = None;
        Ok(())
    }
    pub fn connect(&mut self) -> io::Result<()> {
        if self.trusted.is_some_and(|id| {
            self.incompatible.contains(&id)
                || self
                    .candidates
                    .iter()
                    .any(|p| p.id == id && p.protocol != crate::protocol::VERSION)
        }) {
            return Err(io::Error::other(VERSION_MISMATCH));
        }
        self.paused_by_user = false;
        self.policy.manual_connect();
        self.error = None;
        if self.trusted.is_none() && self.platform == "windows" {
            self.start_pairing()?;
        }
        Ok(())
    }
    pub fn shutdown(&mut self) {
        let _ = self.disconnect();
        self.discovery.take();
    }
    pub fn command(&mut self, command: Command) -> io::Result<bool> {
        match command {
            Command::Select(id) => {
                if self
                    .candidates
                    .iter()
                    .any(|p| p.id == id && p.protocol != crate::protocol::VERSION)
                {
                    return Err(io::Error::other(VERSION_MISMATCH));
                }
                self.paused_by_user = false;
                self.policy.manual_connect();
                if self.store.paired()?.is_some() {
                    return Err(io::Error::other("已有配对，请先在高级设置忘记设备"));
                }
                if self.candidates.iter().any(|p| p.id == id) {
                    self.selected = Some(id);
                    self.error = None;
                } else {
                    return Err(io::Error::other("设备已离线，请刷新"));
                }
            }
            Command::Pair(code) => {
                if self.job.is_some() {
                    return Ok(true);
                }
                let code: [u8; 6] = code
                    .as_bytes()
                    .try_into()
                    .map_err(|_| io::Error::other("请输入六位配对码"))?;
                if !code.iter().all(u8::is_ascii_digit) {
                    return Err(io::Error::other("请输入六位数字"));
                }
                let candidate = self
                    .selected
                    .and_then(|id| self.candidates.iter().find(|p| p.id == id))
                    .cloned()
                    .ok_or_else(|| io::Error::other("请先选择 Windows 设备"))?;
                self.error = None;
                self.job = Some(PairJob::start(candidate, code, self.store.clone())?);
                self.paused_by_user = false;
                self.policy.manual_connect();
            }
            Command::Connect => self.connect()?,
            Command::Disconnect => self.disconnect()?,
            Command::Refresh => {
                self.error = None;
                self.discovery.take();
                self.discover();
                if self.platform == "windows" {
                    self.start_pairing()?;
                }
            }
            Command::Forget => {
                self.disconnect()?;
                self.store.forget()?;
                self.trusted = None;
                self.selected = None;
                self.prefs.last_address = None;
                self.prefs.peer_name.clear();
                self.prefs.save(&self.store)?;
                self.paused_by_user = false;
                self.policy = ConnectPolicy::new(None, true);
                if self.platform == "windows" {
                    self.start_pairing()?;
                }
            }
            Command::Options { auto, image, files } => {
                if matches!(
                    self.phase,
                    ServiceState::Connected
                        | ServiceState::Connecting
                        | ServiceState::Starting
                        | ServiceState::Listening
                ) {
                    return Err(io::Error::other("请先断开，再修改同步设置"));
                }
                let mut p = self.prefs.clone();
                p.auto_connect = auto;
                p.image_clipboard = image;
                p.file_clipboard = files;
                p.save(&self.store)?;
                self.prefs = p;
                self.policy = ConnectPolicy::new(self.trusted, auto);
                if self.paused_by_user {
                    self.policy.manual_stop();
                }
            }
            Command::Manual { address, key } => {
                self.disconnect()?;
                let key = crate::secure::load_key_from(&key)?;
                let mut digest = sha2::Sha256::new();
                use sha2::Digest;
                digest.update(address.to_string().as_bytes());
                let id: DeviceId = digest.finalize()[..16].try_into().unwrap();
                self.store.stage(id, &key)?;
                self.store.commit(id)?;
                self.prefs.last_address = Some(address);
                self.prefs.peer_name = "手动配置的 Windows".into();
                self.prefs.save(&self.store)?;
                self.trusted = Some(id);
                self.paused_by_user = false;
                self.policy = ConnectPolicy::new(Some(id), true);
                self.opened = Instant::now() - Duration::from_secs(9);
            }
            Command::Shutdown => {
                self.shutdown();
                return Ok(false);
            }
        }
        Ok(true)
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        self.shutdown();
    }
}
#[cfg(test)]
mod tests;
