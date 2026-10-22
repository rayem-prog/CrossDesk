use super::*;
fn root() -> PathBuf {
    let mut id = [0; 8];
    getrandom::getrandom(&mut id).unwrap();
    std::env::temp_dir().join(format!("crossdesk-tree-{:x}", u64::from_le_bytes(id)))
}
#[test]
fn scan_keeps_empty_directories_and_rejects_changes_and_cancellation() {
    let root = root();
    fs::create_dir_all(root.join("a/empty")).unwrap();
    fs::write(root.join("a/.hidden"), b"old").unwrap();
    let tree = SourceTree::scan(&[root.join("a")], &mut || false).unwrap();
    assert_eq!(tree.entries.len(), 3);
    assert_eq!(tree.entries[0].parent, None);
    assert_eq!(tree.entries[0].directory, true);
    tree.verify(&mut || false).unwrap();
    fs::write(root.join("a/.hidden"), b"changed").unwrap();
    assert!(tree.verify(&mut || false).is_err());
    let mut calls = 0;
    let error = SourceTree::scan(&[root.join("a")], &mut || {
        calls += 1;
        calls > 2
    })
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn scanner_rejects_oversize_file_deep_directory_and_excess_top_level_selection() {
    let root = root();
    fs::create_dir(&root).unwrap();
    let file = root.join("large");
    File::create(&file)
        .unwrap()
        .set_len(super::super::MAX_FILE_BYTES + 1)
        .unwrap();
    assert!(SourceTree::scan(&[file.clone()], &mut || false).is_err());
    assert!(SourceTree::scan(&vec![file; 65], &mut || false).is_err());
    let mut directory = root.join("deep");
    for _ in 0..33 {
        directory = directory.join("d");
    }
    fs::create_dir_all(&directory).unwrap();
    assert!(SourceTree::scan(&[root.join("deep")], &mut || false).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn symlink_anywhere_rejects_the_whole_tree_instead_of_following_it() {
    let root = root();
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("outside"), b"secret").unwrap();
    std::os::unix::fs::symlink(root.join("outside"), root.join("dir/link")).unwrap();
    assert!(SourceTree::scan(&[root.join("dir")], &mut || false).is_err());
    fs::remove_dir_all(root).unwrap();
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = root();
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scan_accepts_64_top_level_roots_and_rejects_65_or_zero() {
    let fixture = Fixture::new();
    let paths: Vec<_> = (0..65)
        .map(|i| fixture.0.join(format!("file-{i}")))
        .collect();
    for path in &paths {
        fs::write(path, b"").unwrap();
    }
    let tree = SourceTree::scan(&paths[..64], &mut || false).unwrap();
    assert_eq!(tree.entries.len(), 64);
    assert!(tree.entries.iter().all(|entry| entry.parent.is_none()));
    assert!(SourceTree::scan(&paths, &mut || false).is_err());
    assert!(SourceTree::scan(&[], &mut || false).is_err());
}

#[test]
fn scan_accepts_10000_total_entries_and_rejects_one_more() {
    let fixture = Fixture::new();
    for i in 0..9999 {
        fs::write(fixture.0.join(format!("file-{i:05}")), b"").unwrap();
    }
    let tree = SourceTree::scan(&[fixture.0.clone()], &mut || false).unwrap();
    assert_eq!(tree.entries.len(), 10_000);
    tree.verify(&mut || false).unwrap();
    fs::write(fixture.0.join("one-more"), b"").unwrap();
    assert!(SourceTree::scan(&[fixture.0.clone()], &mut || false).is_err());
}

#[test]
fn scan_accepts_32_levels_and_rejects_level_33() {
    let fixture = Fixture::new();
    let selected = fixture.0.join("level-1");
    let mut deepest = selected.clone();
    for _ in 1..32 {
        deepest.push("d");
    }
    fs::create_dir_all(&deepest).unwrap();
    let tree = SourceTree::scan(&[selected.clone()], &mut || false).unwrap();
    assert_eq!(tree.entries.len(), 32);
    fs::create_dir(deepest.join("too-deep")).unwrap();
    assert!(SourceTree::scan(&[selected], &mut || false).is_err());
}

#[test]
fn scan_accepts_two_gib_sparse_batch_and_rejects_one_extra_byte() {
    let fixture = Fixture::new();
    let paths: Vec<_> = (0..4)
        .map(|i| fixture.0.join(format!("part-{i}")))
        .collect();
    for path in &paths {
        File::create(path)
            .unwrap()
            .set_len(512 * 1024 * 1024)
            .unwrap();
    }
    let tree = SourceTree::scan(&paths, &mut || false).unwrap();
    assert_eq!(
        tree.entries.iter().map(|entry| entry.len).sum::<u64>(),
        2 * 1024 * 1024 * 1024
    );
    let extra = fixture.0.join("extra");
    fs::write(&extra, b"x").unwrap();
    let mut over = paths;
    over.push(extra);
    assert!(SourceTree::scan(&over, &mut || false).is_err());
}

#[test]
fn cancelled_scan_and_verify_return_interrupted() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("file"), b"content").unwrap();
    let tree = SourceTree::scan(&[fixture.0.clone()], &mut || false).unwrap();
    assert_eq!(
        SourceTree::scan(&[fixture.0.clone()], &mut || true)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(
        tree.verify(&mut || true).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    // Root visitation succeeds; cancellation during directory enumeration aborts.
    let mut polls = 0;
    let error = SourceTree::scan(&[fixture.0.clone()], &mut || {
        polls += 1;
        polls > 1
    })
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

#[test]
fn verify_rejects_added_and_removed_directory_children() {
    let fixture = Fixture::new();
    let child = fixture.0.join("child");
    fs::write(&child, b"original").unwrap();
    let tree = SourceTree::scan(&[fixture.0.clone()], &mut || false).unwrap();
    fs::write(fixture.0.join("added"), b"added").unwrap();
    assert!(tree.verify(&mut || false).is_err());
    let tree = SourceTree::scan(&[fixture.0.clone()], &mut || false).unwrap();
    fs::remove_file(child).unwrap();
    assert!(tree.verify(&mut || false).is_err());
}

#[test]
fn verify_and_open_reject_same_length_file_replacement() {
    let fixture = Fixture::new();
    let path = fixture.0.join("document");
    fs::write(&path, b"before").unwrap();
    let tree = SourceTree::scan(&[path.clone()], &mut || false).unwrap();
    let opened = tree.open(0).unwrap();
    let replacement = fixture.0.join("replacement");
    fs::write(&replacement, b"after!").unwrap();
    fs::remove_file(&path).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert!(tree.verify(&mut || false).is_err());
    assert!(tree.open(0).is_err());
    assert!(tree.verify_open(0, &opened).is_err());
}

#[cfg(unix)]
#[test]
fn scan_rejects_selected_directory_symlink_and_dangling_nested_link() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("directory");
    fs::create_dir(&directory).unwrap();
    let link = fixture.0.join("selected-link");
    std::os::unix::fs::symlink(&directory, &link).unwrap();
    assert!(SourceTree::scan(&[link], &mut || false).is_err());
    std::os::unix::fs::symlink(fixture.0.join("missing"), directory.join("dangling")).unwrap();
    assert!(SourceTree::scan(&[directory], &mut || false).is_err());
}

#[cfg(windows)]
#[test]
fn scan_rejects_real_offline_file_attribute() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE, SetFileAttributesW,
    };
    let fixture = Fixture::new();
    let path = fixture.0.join("offline");
    fs::write(&path, b"local fixture marked offline").unwrap();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    assert_ne!(
        unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_OFFLINE) },
        0
    );
    let result = SourceTree::scan(&[path], &mut || false);
    // Restore the fixture even if the rejection assertion fails.
    assert_ne!(
        unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_NORMAL) },
        0
    );
    assert!(result.is_err());
}

#[cfg(windows)]
#[test]
fn scan_rejects_real_directory_junction_at_root_and_inside_tree() {
    let fixture = Fixture::new();
    let target = fixture.0.join("target");
    let selected = fixture.0.join("selected");
    let junction = selected.join("junction");
    fs::create_dir(&target).unwrap();
    fs::create_dir(&selected).unwrap();
    fs::write(target.join("secret"), b"must not traverse").unwrap();
    // Junctions exercise FILE_ATTRIBUTE_REPARSE_POINT without requiring the
    // administrator or Developer Mode privilege that Windows symlinks need.
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root_result = SourceTree::scan(&[junction.clone()], &mut || false);
    let nested_result = SourceTree::scan(&[selected], &mut || false);
    fs::remove_dir(junction).unwrap();
    assert!(root_result.is_err());
    assert!(nested_result.is_err());
    assert_eq!(
        fs::read(target.join("secret")).unwrap(),
        b"must not traverse"
    );
}
