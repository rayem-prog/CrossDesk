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
    std::thread::sleep(Duration::from_millis(100));
    let start = Instant::now();
    s.stop().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(TcpListener::bind(address).is_ok());
    drop(idle);
    assert!(s.view().code.is_none());
    std::fs::remove_dir_all(root).unwrap();
}
