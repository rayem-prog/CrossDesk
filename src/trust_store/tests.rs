use super::*;
fn root() -> PathBuf {
    let mut x = [0u8; 8];
    getrandom::getrandom(&mut x).unwrap();
    std::env::temp_dir().join(format!("crossdesk-trust-{x:x?}"))
}
#[test]
fn pending_is_not_trusted_and_committed_identity_survives_restart() {
    let root = root();
    let s = TrustedStore::new(root.clone()).unwrap();
    let key = s.host_key().unwrap();
    let id = s.local_id().unwrap();
    s.stage([7; 16], &key).unwrap();
    assert!(s.paired().unwrap().is_none());
    s.commit([7; 16]).unwrap();
    let again = TrustedStore::new(root.clone()).unwrap();
    assert_eq!(again.local_id().unwrap(), id);
    assert!(again.host_key().unwrap() == key);
    assert_eq!(again.paired().unwrap().unwrap().id, [7; 16]);
    assert!(s.stage([8; 16], &[9; 32]).is_err());
    assert!(s.stage([7; 16], &[9; 32]).is_err());
    assert!(crate::secure::load_key_from(&s.paired().unwrap().unwrap().key_path).unwrap() == key);
    std::fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn symlink_store_is_rejected_and_keys_are_private() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let root = root();
    std::fs::create_dir(&root).unwrap();
    symlink(root.join("target"), root.join("link")).unwrap();
    assert!(TrustedStore::new(root.join("link")).is_err());
    let s = TrustedStore::new(root.join("private")).unwrap();
    s.host_key().unwrap();
    assert_eq!(
        std::fs::metadata(root.join("private/host.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::remove_dir_all(root).unwrap();
}
