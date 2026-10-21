use super::*;
#[test]
fn stopping_pairing_closes_listener_and_cancels_idle_socket() {
    let mut x = [0; 8];
    getrandom::getrandom(&mut x).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-service-{x:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let mut s = PairingService::start("127.0.0.1:0".parse().unwrap(), store).unwrap();
    let address = s.local_addr();
    let idle = TcpStream::connect(address).unwrap();
    let accepted_by = Instant::now() + Duration::from_secs(5);
    while s.active.lock().unwrap().is_none() {
        assert!(
            Instant::now() < accepted_by,
            "pairing worker never accepted socket"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    s.stop().unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "idle cancellation took {:?}",
        start.elapsed()
    );
    assert!(TcpListener::bind(address).is_ok());
    drop(idle);
    assert!(s.view().code.is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stopping_at_acceptance_does_not_start_an_untracked_handshake() {
    let mut x = [0; 8];
    getrandom::getrandom(&mut x).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-accept-stop-{x:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    for _ in 0..16 {
        let mut service =
            PairingService::start("127.0.0.1:0".parse().unwrap(), store.clone()).unwrap();
        let address = service.local_addr();
        let idle = TcpStream::connect(address).unwrap();
        let start = Instant::now();
        service.stop().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "accept cancellation took {:?}",
            start.elapsed()
        );
        assert!(TcpListener::bind(address).is_ok());
        drop(idle);
    }
    std::fs::remove_dir_all(root).unwrap();
}
