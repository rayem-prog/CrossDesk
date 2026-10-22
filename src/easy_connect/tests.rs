use super::*;
use std::sync::{Arc, Mutex};
struct MemoryService(Arc<Mutex<ServiceState>>);
impl ServiceControl for MemoryService {
    fn start(&mut self, _: &AppConfig, _: &std::path::Path, _: bool, _: bool) -> io::Result<()> {
        *self.0.lock().unwrap() = ServiceState::Starting;
        Ok(())
    }
    fn stop(&mut self) -> io::Result<()> {
        *self.0.lock().unwrap() = ServiceState::Stopped;
        Ok(())
    }
    fn poll(&mut self) -> (ServiceState, Option<String>) {
        (*self.0.lock().unwrap(), None)
    }
    fn logs(&self) -> Vec<String> {
        Vec::new()
    }
}
#[test]
fn remembered_start_waits_for_authentication_and_disconnect_stays_paused() {
    let mut nonce = [0; 8];
    getrandom::getrandom(&mut nonce).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-core-{nonce:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let state = Arc::new(Mutex::new(ServiceState::Stopped));
    let peer = Candidate {
        id: [3; 16],
        name: "我的 Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: Some(24921),
        protocol: crate::protocol::VERSION,
    };
    let mut core = Core::from_parts(
        "macos",
        store.clone(),
        Box::new(MemoryService(state.clone())),
    )
    .unwrap();
    core.candidates = vec![peer.clone()];
    core.tick().unwrap();
    assert_eq!(core.view().state, "discovering");
    store.stage(peer.id, &[8; 32]).unwrap();
    store.commit(peer.id).unwrap();
    core.tick().unwrap();
    assert_eq!(core.view().state, "connecting");
    *state.lock().unwrap() = ServiceState::Connected;
    core.tick().unwrap();
    assert_eq!(core.view().state, "connected");
    core.disconnect().unwrap();
    core.candidates = vec![peer];
    core.tick().unwrap();
    assert_eq!(core.view().state, "paused");
    assert_eq!({ *state.lock().unwrap() }, ServiceState::Stopped);
    core.connect().unwrap();
    core.tick().unwrap();
    assert_eq!(core.view().state, "connecting");
    core.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn disabled_auto_connect_and_sync_choices_survive_restart() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-pref-{n:x?}"));
    let store = TrustedStore::new(root.clone()).unwrap();
    let mut pref = Preferences::default();
    pref.auto_connect = false;
    pref.image_clipboard = false;
    pref.save(&store).unwrap();
    let restored = Preferences::load(&store).unwrap();
    assert!(!restored.auto_connect);
    assert!(!restored.image_clipboard);
    assert!(restored.file_clipboard);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn disconnect_wins_even_if_pairing_finishes_before_the_next_tick() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-pause-{n:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let status = Arc::new(Mutex::new(ServiceState::Stopped));
    let mut c = Core::from_parts(
        "macos",
        store.clone(),
        Box::new(MemoryService(status.clone())),
    )
    .unwrap();
    c.candidates = vec![Candidate {
        id: [3; 16],
        name: "Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: None,
        protocol: crate::protocol::VERSION,
    }];
    c.disconnect().unwrap();
    store.stage([3; 16], &[8; 32]).unwrap();
    store.commit([3; 16]).unwrap();
    c.tick().unwrap();
    assert_eq!(c.view().state, "paused");
    assert_eq!(*status.lock().unwrap(), ServiceState::Stopped);
    c.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn first_pairing_starts_both_services_and_the_next_launch_needs_no_code() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-first-use-{n:x?}"));
    let host_store = Arc::new(TrustedStore::new(root.join("host")).unwrap());
    let client_store = Arc::new(TrustedStore::new(root.join("client")).unwrap());
    let host_state = Arc::new(Mutex::new(ServiceState::Stopped));
    let client_state = Arc::new(Mutex::new(ServiceState::Stopped));
    let mut host = Core::from_parts(
        "windows",
        host_store.clone(),
        Box::new(MemoryService(host_state.clone())),
    )
    .unwrap();
    host.pairing =
        Some(PairingService::start("127.0.0.1:0".parse().unwrap(), host_store.clone()).unwrap());
    let pin = host.pairing.as_ref().unwrap().view().code.unwrap();
    let candidate = Candidate {
        id: host_store.local_id().unwrap(),
        name: "Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: Some(host.pairing.as_ref().unwrap().local_addr().port()),
        protocol: crate::protocol::VERSION,
    };
    let mut client = Core::from_parts(
        "macos",
        client_store.clone(),
        Box::new(MemoryService(client_state.clone())),
    )
    .unwrap();
    client.candidates = vec![candidate.clone()];
    client.command(Command::Select(candidate.id)).unwrap();
    client.disconnect().unwrap();
    // A new explicit Pair after cancellation is a fresh request to connect.
    client.command(Command::Pair(pin)).unwrap();
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        host.tick().unwrap();
        client.tick().unwrap();
        if host_store.paired().unwrap().is_some()
            && client_store.paired().unwrap().is_some()
            && client.phase == ServiceState::Starting
        {
            break;
        }
        assert!(Instant::now() < until, "first-use flow did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(host.view().pairing_code.is_none());
    assert_eq!(client.view().state, "connecting");
    *host_state.lock().unwrap() = ServiceState::Connected;
    *client_state.lock().unwrap() = ServiceState::Connected;
    host.tick().unwrap();
    client.tick().unwrap();
    assert_eq!(client.view().state, "connected");
    client.shutdown();
    host.shutdown();
    let next_status = Arc::new(Mutex::new(ServiceState::Stopped));
    let mut next =
        Core::from_parts("macos", client_store, Box::new(MemoryService(next_status))).unwrap();
    next.candidates = vec![candidate];
    next.tick().unwrap();
    assert_eq!(next.view().state, "connecting");
    assert!(next.view().pairing_code.is_none());
    next.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
struct AddressService {
    state: ServiceState,
}
impl ServiceControl for AddressService {
    fn start(&mut self, c: &AppConfig, _: &std::path::Path, _: bool, _: bool) -> io::Result<()> {
        self.state = if c.server == "192.168.1.114:24920" {
            ServiceState::Connected
        } else {
            ServiceState::Retrying
        };
        Ok(())
    }
    fn stop(&mut self) -> io::Result<()> {
        self.state = ServiceState::Stopped;
        Ok(())
    }
    fn poll(&mut self) -> (ServiceState, Option<String>) {
        (self.state, None)
    }
    fn logs(&self) -> Vec<String> {
        Vec::new()
    }
}
#[test]
fn unreachable_virtual_adapter_does_not_hide_a_working_lan_address() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-address-{n:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    store.stage([3; 16], &[8; 32]).unwrap();
    store.commit([3; 16]).unwrap();
    let mut c = Core::from_parts(
        "macos",
        store,
        Box::new(AddressService {
            state: ServiceState::Stopped,
        }),
    )
    .unwrap();
    c.candidates = vec![Candidate {
        id: [3; 16],
        name: "Windows".into(),
        endpoints: vec![
            "172.17.0.1:24920".parse().unwrap(),
            "192.168.1.114:24920".parse().unwrap(),
        ],
        pairing_port: None,
        protocol: crate::protocol::VERSION,
    }];
    let now = Instant::now();
    c.tick_at(now).unwrap();
    c.tick_at(now + Duration::from_millis(1)).unwrap();
    c.tick_at(now + Duration::from_secs(2)).unwrap();
    c.tick_at(now + Duration::from_secs(3)).unwrap();
    assert_eq!(c.view().state, "connected");
    c.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn a_confirmed_pairing_grant_recovers_a_lost_final_ack_without_another_code() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-pending-{n:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    // stage is only called after PAKE and the bootstrap Noise channel authenticated.
    store.stage([3; 16], &[8; 32]).unwrap();
    assert!(store.paired().unwrap().is_none());
    let status = Arc::new(Mutex::new(ServiceState::Stopped));
    let mut c = Core::from_parts(
        "macos",
        store.clone(),
        Box::new(MemoryService(status.clone())),
    )
    .unwrap();
    c.candidates = vec![Candidate {
        id: [3; 16],
        name: "Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: None,
        protocol: crate::protocol::VERSION,
    }];
    c.tick().unwrap();
    assert_eq!(c.view().state, "connecting");
    assert!(store.paired().unwrap().is_none());
    *status.lock().unwrap() = ServiceState::Connected;
    c.tick().unwrap();
    assert_eq!(store.paired().unwrap().unwrap().id, [3; 16]);
    assert_eq!(c.view().state, "connected");
    c.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
struct RecordedDiscovery(Arc<Mutex<Option<u16>>>);
impl DiscoverySource for RecordedDiscovery {
    fn snapshot(&self) -> Vec<Candidate> {
        Vec::new()
    }
    fn announce(&mut self, _: DeviceId, _: &str, port: Option<u16>) -> io::Result<()> {
        *self.0.lock().unwrap() = port;
        Ok(())
    }
}
#[test]
fn a_closed_pairing_window_is_removed_from_the_discovery_announcement() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-announce-{n:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let status = Arc::new(Mutex::new(ServiceState::Stopped));
    let port = Arc::new(Mutex::new(None));
    let mut c =
        Core::from_parts("windows", store.clone(), Box::new(MemoryService(status))).unwrap();
    c.discovery = Some(Box::new(RecordedDiscovery(port.clone())));
    c.pairing = Some(PairingService::start("127.0.0.1:0".parse().unwrap(), store).unwrap());
    c.announce();
    assert!(port.lock().unwrap().is_some());
    c.pairing.as_mut().unwrap().stop().unwrap();
    c.tick().unwrap();
    assert!(port.lock().unwrap().is_none());
    c.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_first_pair_connects_even_when_next_launch_auto_connect_is_off() {
    let mut n = [0; 8];
    getrandom::getrandom(&mut n).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-explicit-{n:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let mut pref = Preferences::default();
    pref.auto_connect = false;
    pref.save(&store).unwrap();
    let state = Arc::new(Mutex::new(ServiceState::Stopped));
    let mut c = Core::from_parts("macos", store.clone(), Box::new(MemoryService(state))).unwrap();
    c.candidates = vec![Candidate {
        id: [3; 16],
        name: "Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: None,
        protocol: crate::protocol::VERSION,
    }];
    c.command(Command::Select([3; 16])).unwrap();
    store.stage([3; 16], &[8; 32]).unwrap();
    store.commit([3; 16]).unwrap();
    c.tick().unwrap();
    assert_eq!(c.view().state, "connecting");
    c.shutdown();
    let mut next = Core::from_parts(
        "macos",
        store,
        Box::new(MemoryService(Arc::new(Mutex::new(ServiceState::Stopped)))),
    )
    .unwrap();
    next.tick().unwrap();
    assert_eq!(next.view().state, "paused");
    next.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn discovered_devices_expose_compatibility_to_the_connection_ui() {
    let mut nonce = [0; 8];
    getrandom::getrandom(&mut nonce).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-version-view-{nonce:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let mut core = Core::from_parts(
        "macos",
        store,
        Box::new(MemoryService(Arc::new(Mutex::new(ServiceState::Stopped)))),
    )
    .unwrap();
    core.candidates = vec![Candidate {
        id: [3; 16],
        name: "Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: Some(24921),
        protocol: crate::protocol::VERSION,
    }];
    let view = serde_json::to_value(core.view()).unwrap();
    assert_eq!(view["devices"][0]["compatible"], true);
    core.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn version_test_core() -> (Core, PathBuf, Arc<Mutex<ServiceState>>) {
    let mut nonce = [0; 8];
    getrandom::getrandom(&mut nonce).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-incompatible-{nonce:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let state = Arc::new(Mutex::new(ServiceState::Stopped));
    let mut core =
        Core::from_parts("macos", store, Box::new(MemoryService(state.clone()))).unwrap();
    core.candidates = vec![Candidate {
        id: [3; 16],
        name: "Legacy Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: Some(24921),
        protocol: 6,
    }];
    (core, root, state)
}

#[test]
fn legacy_device_is_visible_but_cannot_be_selected_or_paired() {
    let (mut core, root, _) = version_test_core();
    let view = serde_json::to_value(core.view()).unwrap();
    assert_eq!(view["devices"][0]["compatible"], false);
    let error = core
        .command(Command::Select([3; 16]))
        .err()
        .expect("legacy selection must fail");
    assert!(error.to_string().contains("0.5.0"));
    assert!(core.selected.is_none());
    // Discovery may change between selection and submission; check again at PairJob boundary.
    core.selected = Some([3; 16]);
    let error = core
        .command(Command::Pair("123456".into()))
        .err()
        .expect("legacy pairing must fail synchronously");
    assert!(error.to_string().contains("两端"));
    assert!(core.job.is_none());
    core.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_discovery_blocks_auto_connect_even_with_a_saved_address() {
    let (mut core, root, state) = version_test_core();
    core.store.stage([3; 16], &[8; 32]).unwrap();
    core.store.commit([3; 16]).unwrap();
    core.prefs.last_address = Some("127.0.0.1:24920".parse().unwrap());
    core.opened = Instant::now() - Duration::from_secs(9);
    core.tick().unwrap();
    assert_eq!({ *state.lock().unwrap() }, ServiceState::Stopped);
    assert!(core.view().error.as_deref().unwrap().contains("0.5.0"));
    let error = core
        .command(Command::Connect)
        .err()
        .expect("explicit reconnect must also reject legacy devices");
    assert!(error.to_string().contains("两端"));
    core.candidates.clear();
    core.tick().unwrap();
    assert_eq!(
        { *state.lock().unwrap() },
        ServiceState::Stopped,
        "discovery expiry must not bypass a known protocol mismatch"
    );
    assert!(!core.view().devices[0].compatible);
    core.candidates = vec![Candidate {
        id: [3; 16],
        name: "Updated Windows".into(),
        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
        pairing_port: None,
        protocol: 7,
    }];
    core.tick().unwrap();
    assert_eq!({ *state.lock().unwrap() }, ServiceState::Starting);
    assert!(
        core.view().error.is_none(),
        "upgrade must clear the compatibility error"
    );
    core.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn incompatible_history_keeps_the_trusted_peer_without_accumulating_broadcast_ids() {
    let (mut core, root, _) = version_test_core();
    core.store.stage([3; 16], &[8; 32]).unwrap();
    core.store.commit([3; 16]).unwrap();
    core.tick().unwrap();
    for n in 10..50 {
        core.candidates[0].id = [n; 16];
        core.tick().unwrap();
    }
    assert!(core.incompatible.contains(&[3; 16]));
    assert!(core.incompatible.len() <= 2);
    core.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
