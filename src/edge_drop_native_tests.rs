//! Native clipboard sentinels around a real disk receive. Mac uses a named board;
//! Windows runs only when explicitly selected on the disposable CI runner.
use super::*;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "crossdesk-native-drop-{}",
            hex(&random_id().unwrap())
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn receive_file(root: &Path) {
    let offer = Offer {
        id: [81; 16],
        entries: vec![wire::Entry {
            parent: None,
            directory: false,
            name: "hello.txt".into(),
            len: 5,
        }],
    };
    let mut receiver = Incoming::default();
    for frame in offer_frames(&offer) {
        receiver.metadata(frame).unwrap();
    }
    receiver.accept(root).unwrap();
    receiver
        .data(wire::request(
            FrameKind::FileData,
            offer.id,
            0,
            0,
            0,
            b"hello",
        ))
        .unwrap();
    receiver
        .data(wire::request(
            FrameKind::FileSeal,
            offer.id,
            1,
            0,
            0,
            &Sha256::digest(b"hello"),
        ))
        .unwrap();
    let saved = receiver
        .data(wire::request(FrameKind::FileCommit, offer.id, 2, 0, 0, &[]))
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(saved.join("hello.txt")).unwrap(), b"hello");
}

#[cfg(target_os = "macos")]
#[test]
fn native_edge_drop_preserves_named_macos_clipboard_contents_and_revision() {
    let root = Temp::new();
    let name = format!("org.crossdesk.drop-test-{}", hex(&random_id().unwrap()));
    let script = |initialize: bool| {
        let setup = if initialize {
            "pb.clearContents; pb.setStringForType('保留文字 🔒', 'public.utf8-plain-text');"
        } else {
            ""
        };
        let source = format!(
            "ObjC.import('AppKit'); const pb=$.NSPasteboard.pasteboardWithName('{name}'); {setup} String(Number(pb.changeCount))+'|'+ObjC.unwrap(pb.stringForType('public.utf8-plain-text')); "
        );
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", &source])
            .output()
            .unwrap();
        assert!(output.status.success(), "named pasteboard script failed");
        String::from_utf8(output.stdout).unwrap()
    };
    let before = script(true);
    assert!(before.contains("保留文字 🔒"));
    receive_file(&root.0);
    assert_eq!(script(false), before);
}

#[cfg(windows)]
#[test]
#[ignore = "changes Windows clipboard; disposable CI runner only"]
fn native_edge_drop_preserves_windows_clipboard() {
    use crate::clipboard::{Clipboard, ClipboardApi};
    use crate::protocol::ClipboardPayload;
    let root = Temp::new();
    let mut clipboard = Clipboard::new().unwrap();
    clipboard
        .write(&ClipboardPayload::Text(
            "CrossDesk drop sentinel 保留 🔒".into(),
        ))
        .unwrap();
    let revision = clipboard.change_id().unwrap();
    receive_file(&root.0);
    assert_eq!(clipboard.change_id().unwrap(), revision);
    assert!(
        matches!(clipboard.read().unwrap(), Some(ClipboardPayload::Text(ref value)) if value == "CrossDesk drop sentinel 保留 🔒")
    );
}
