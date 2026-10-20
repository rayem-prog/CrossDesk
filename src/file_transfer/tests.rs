use super::*;
#[test]
fn portable_names_accept_short_unicode_and_extensionless_files_without_panicking() {
    for name in ["a", "文", "README", "文档.pdf", "archive.tar.gz"] {
        assert!(safe_name(name));
    }
    for name in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "NUL.txt",
        "CONIN$",
        "COM¹.pdf",
        "x.",
        "x ",
    ] {
        assert!(!safe_name(name));
    }
}
#[test]
fn receive_aborts_partial_cache_and_preserves_completed_zero_byte_and_binary_files() {
    let root = std::env::temp_dir().join(format!("crossdesk-stream-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut r = IncomingBundle::new(root.clone());
    r.start_file("empty", 0).unwrap();
    r.start_file("文档.pdf", 4).unwrap();
    r.write_chunk(&[0, 255, 17, 0]).unwrap();
    let files = r.finish().unwrap();
    assert_eq!(fs::read(&files[0]).unwrap(), Vec::<u8>::new());
    assert_eq!(fs::read(&files[1]).unwrap(), vec![0, 255, 17, 0]);
    r.start_file("partial", 7).unwrap();
    r.write_chunk(b"bad").unwrap();
    drop(r);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert!(files.iter().all(|p| p.exists()));
    fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn source_rejects_links_and_detects_truncation_on_the_open_file() {
    let root = std::env::temp_dir().join(format!("crossdesk-source-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    let path = root.join("report.pdf");
    fs::write(&path, b"original").unwrap();
    let link = root.join("link.pdf");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(sources(&[link]).is_err());
    let source = sources(&[path.clone()]).unwrap().remove(0);
    fs::write(path, b"short").unwrap();
    assert!(source.unchanged().is_err());
    fs::remove_dir_all(root).unwrap();
}
