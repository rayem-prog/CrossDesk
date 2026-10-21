// Unauthenticated discovery supplies addresses, never trust.
use crate::trust_store::{DeviceId, hex, parse_id};
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, SocketAddrV6},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const TYPE: &str = "_crossdesk._tcp.local.";
#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: DeviceId,
    pub name: String,
    pub endpoints: Vec<SocketAddr>,
    pub pairing_port: Option<u16>,
}
#[derive(Default)]
pub struct Catalog {
    entries: HashMap<String, (Candidate, Option<Instant>)>,
}
impl Catalog {
    pub fn upsert(
        &mut self,
        instance: String,
        mut peer: Candidate,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        if peer.name.is_empty()
            || peer.name.len() > 128
            || peer.name.chars().any(char::is_control)
            || peer.endpoints.is_empty()
            || peer.endpoints.len() > 8
            || peer.endpoints.iter().any(|e| e.port() == 0)
            || peer.pairing_port == Some(0)
            || (!self.entries.contains_key(&instance) && self.entries.len() >= 32)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid or excessive discovery record",
            ));
        }
        peer.endpoints
            .sort_by_key(|a| (!a.is_ipv4(), a.to_string()));
        peer.endpoints.dedup();
        self.entries.insert(instance, (peer, deadline));
        Ok(())
    }
    pub fn remove(&mut self, instance: &str) {
        self.entries.remove(instance);
    }
    pub fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|_, (_, until)| until.is_none_or(|until| until > now));
    }
    pub fn list(&self) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        for (peer, _) in self.entries.values() {
            if let Some(old) = out.iter_mut().find(|p| p.id == peer.id) {
                for addr in &peer.endpoints {
                    if old.endpoints.len() < 8 && !old.endpoints.contains(addr) {
                        old.endpoints.push(*addr);
                    }
                }
            } else {
                out.push(peer.clone());
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        out
    }
}
pub struct ConnectPolicy {
    trusted: Option<DeviceId>,
    enabled: bool,
    paused: bool,
    in_flight: bool,
    retry_at: Option<Instant>,
    failures: u8,
}
impl ConnectPolicy {
    pub fn new(trusted: Option<DeviceId>, auto: bool) -> Self {
        Self {
            trusted,
            enabled: auto,
            paused: false,
            in_flight: false,
            retry_at: None,
            failures: 0,
        }
    }
    pub fn should_connect(&self, id: DeviceId, now: Instant) -> bool {
        self.enabled
            && !self.paused
            && !self.in_flight
            && self.trusted == Some(id)
            && self.retry_at.is_none_or(|t| now >= t)
    }
    pub fn started(&mut self) {
        self.in_flight = true;
    }
    pub fn connected(&mut self) {
        self.failures = 0;
        self.retry_at = None;
    }
    pub fn retry(&mut self, now: Instant) {
        self.in_flight = false;
        self.failures = self.failures.saturating_add(1);
        self.retry_at =
            Some(now + Duration::from_secs(1u64 << self.failures.saturating_sub(1).min(3)));
    }
    pub fn manual_stop(&mut self) {
        self.paused = true;
        self.in_flight = false;
        self.retry_at = None;
    }
    pub fn manual_connect(&mut self) {
        self.enabled = true;
        self.paused = false;
        self.retry_at = None;
        self.failures = 0;
    }
    pub fn paused(&self) -> bool {
        self.paused
    }
}
pub struct Discovery {
    daemon: ServiceDaemon,
    catalog: Arc<Mutex<Catalog>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    announced: Option<String>,
}
impl Discovery {
    pub fn browse() -> io::Result<Self> {
        let daemon = ServiceDaemon::new().map_err(io::Error::other)?;
        let receiver = daemon.browse(TYPE).map_err(io::Error::other)?;
        let catalog = Arc::new(Mutex::new(Catalog::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (catalog.clone(), stop.clone());
        let worker = thread::Builder::new()
            .name("device-discovery".into())
            .spawn(move || {
                while !s.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            if info.get_property_val_str("role") != Some("windows")
                                || info.get_property_val_str("protocol") != Some("6")
                            {
                                continue;
                            }
                            let Some(id) = info
                                .get_property_val_str("id")
                                .and_then(|s| parse_id(s).ok())
                            else {
                                continue;
                            };
                            let mut endpoints = Vec::new();
                            for ip in info.get_addresses() {
                                let addr = match ip {
                                    ScopedIp::V4(a) => {
                                        SocketAddr::new((*a.addr()).into(), info.get_port())
                                    }
                                    ScopedIp::V6(a) => SocketAddr::V6(SocketAddrV6::new(
                                        *a.addr(),
                                        info.get_port(),
                                        0,
                                        a.scope_id().index,
                                    )),
                                    _ => continue,
                                };
                                if !addr.ip().is_unspecified()
                                    && !addr.ip().is_multicast()
                                    && endpoints.len() < 8
                                {
                                    endpoints.push(addr);
                                }
                            }
                            let peer = Candidate {
                                id,
                                name: info
                                    .get_property_val_str("name")
                                    .unwrap_or("Windows")
                                    .to_string(),
                                endpoints,
                                pairing_port: info
                                    .get_property_val_str("pair_port")
                                    .and_then(|s| s.parse().ok()),
                            };
                            let _ = c.lock().unwrap().upsert(
                                info.get_fullname().into(),
                                peer,
                                None, // mdns-sd owns record TTL and emits ServiceRemoved.
                            );
                        }
                        Ok(ServiceEvent::ServiceRemoved(_, name)) => {
                            c.lock().unwrap().remove(&name)
                        }
                        _ => {}
                    }
                    c.lock().unwrap().expire(Instant::now());
                }
            })?;
        Ok(Self {
            daemon,
            catalog,
            stop,
            worker: Some(worker),
            announced: None,
        })
    }
    pub fn snapshot(&self) -> Vec<Candidate> {
        self.catalog.lock().unwrap().list()
    }
    pub fn announce(&mut self, id: DeviceId, name: &str, pair_port: Option<u16>) -> io::Result<()> {
        let id = hex(&id);
        let mut properties = HashMap::<String, String>::from([
            ("id".into(), id.clone()),
            ("role".into(), "windows".into()),
            ("protocol".into(), "6".into()),
            (
                "name".into(),
                name.chars().filter(|c| !c.is_control()).take(32).collect(),
            ),
        ]);
        if let Some(p) = pair_port {
            properties.insert("pair_port".into(), p.to_string());
        }
        let info = ServiceInfo::new(
            TYPE,
            &format!("CrossDesk-{id}"),
            &format!("crossdesk-{id}.local."),
            "",
            24920,
            properties,
        )
        .map_err(io::Error::other)?
        .enable_addr_auto();
        self.announced = Some(info.get_fullname().into());
        self.daemon.register(info).map_err(io::Error::other)
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.worker.take() {
            let _ = t.join();
        }
        if let Some(name) = self.announced.take() {
            let _ = self.daemon.unregister(&name);
        }
        let _ = self.daemon.stop_browse(TYPE);
        if let Ok(done) = self.daemon.shutdown() {
            let _ = done.recv_timeout(Duration::from_secs(1));
        }
    }
}
impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
    }
}
#[cfg(test)]
mod tests;
