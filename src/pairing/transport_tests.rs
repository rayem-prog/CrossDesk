use super::*;
use crate::trust_store::TrustedStore;
use std::{
    net::{TcpListener, TcpStream},
    sync::Arc,
    time::Instant,
};
fn exchange(
    code: &[u8; 6],
) -> (
    bool,
    bool,
    Arc<TrustedStore>,
    TrustedStore,
    std::path::PathBuf,
) {
    let mut nonce = [0; 8];
    getrandom::getrandom(&mut nonce).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-pair-{nonce:x?}"));
    let host = Arc::new(TrustedStore::new(root.join("host")).unwrap());
    let client = TrustedStore::new(root.join("client")).unwrap();
    let id = host.local_id().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let h = host.clone();
    let worker = std::thread::spawn(move || {
        let mut w = PairingWindow::new(Instant::now(), *b"123456").unwrap();
        run_host(listener.accept().unwrap().0, &mut w, &h).is_ok()
    });
    let c = run_client(TcpStream::connect(address).unwrap(), id, code, &client).is_ok();
    let h = worker.join().unwrap();
    (h, c, host, client, root)
}
#[test]
fn encrypted_pairing_delivers_identical_long_term_key_without_file_copy() {
    let (h, c, host, client, root) = exchange(b"123456");
    assert!(h && c);
    assert_eq!(
        client.paired().unwrap().unwrap().id,
        host.local_id().unwrap()
    );
    assert_eq!(
        host.paired().unwrap().unwrap().id,
        client.local_id().unwrap()
    );
    assert!(
        crate::secure::load_key_from(&client.paired().unwrap().unwrap().key_path).unwrap()
            == host.host_key().unwrap()
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn incorrect_code_cannot_publish_either_trust_record() {
    let (h, c, host, client, root) = exchange(b"654321");
    assert!(!h && !c);
    assert!(host.paired().unwrap().is_none());
    assert!(client.paired().unwrap().is_none());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_pairing_lengths_are_rejected_before_allocation() {
    assert!(read_message(&mut &b"\xff\xff"[..]).is_err());
    assert!(read_message(&mut &b"\0\0"[..]).is_err());
    assert!(read_message(&mut &b"\0\x05a"[..]).is_err());
}
