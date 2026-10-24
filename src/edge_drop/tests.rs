use super::*;
use crate::file_transfer::wire::{Entry, Offer};
use std::fs;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "crossdesk-drop-test-{}",
            hex(&random_id().unwrap())
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn offer() -> Offer {
    Offer {
        id: [7; 16],
        entries: vec![Entry {
            parent: None,
            directory: false,
            name: "hello.txt".into(),
            len: 5,
        }],
    }
}
// Removing the explicit acceptance gate would allocate files before the user chooses a destination.
#[test]
fn validated_offer_does_not_touch_destination_until_accept() {
    let t = Temp::new();
    let mut r = Incoming::default();
    for f in offer_frames(&offer()) {
        r.metadata(f).unwrap();
    }
    assert!(r.offer.is_some());
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 0);
    assert!(
        r.data(wire::request(
            FrameKind::FileData,
            [7; 16],
            0,
            0,
            0,
            b"hello"
        ))
        .is_err()
    );
}
// Publishing unverified content or replacing the destination would lose data.
#[test]
fn digest_failure_cleans_owned_staging_and_preserves_neighbors() {
    let t = Temp::new();
    fs::write(t.0.join("keep"), b"safe").unwrap();
    let mut r = Incoming::default();
    for f in offer_frames(&offer()) {
        r.metadata(f).unwrap();
    }
    r.accept(&t.0).unwrap();
    r.data(wire::request(
        FrameKind::FileData,
        [7; 16],
        0,
        0,
        0,
        b"hello",
    ))
    .unwrap();
    assert!(
        r.data(wire::request(
            FrameKind::FileSeal,
            [7; 16],
            1,
            0,
            0,
            &[0; 32]
        ))
        .is_err()
    );
    drop(r);
    assert_eq!(fs::read(t.0.join("keep")).unwrap(), b"safe");
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 1);
}
#[test]
fn verified_batch_publishes_exclusively_and_survives_receiver_drop() {
    let t = Temp::new();
    let mut r = Incoming::default();
    for f in offer_frames(&offer()) {
        r.metadata(f).unwrap();
    }
    r.accept(&t.0).unwrap();
    r.data(wire::request(
        FrameKind::FileData,
        [7; 16],
        0,
        0,
        0,
        b"hello",
    ))
    .unwrap();
    r.data(wire::request(
        FrameKind::FileSeal,
        [7; 16],
        1,
        0,
        0,
        &Sha256::digest(b"hello"),
    ))
    .unwrap();
    let saved = r
        .data(wire::request(FrameKind::FileCommit, [7; 16], 2, 0, 0, &[]))
        .unwrap()
        .unwrap();
    drop(r);
    assert_eq!(fs::read(saved.join("hello.txt")).unwrap(), b"hello");
    let a = t.0.join("a");
    let b = t.0.join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    assert!(rename_exclusive(&a, &b).is_err());
    assert!(a.exists() && b.exists());
}
#[test]
fn invalid_manifest_never_becomes_visible_offer() {
    let mut r = Incoming::default();
    let mut bad = offer();
    bad.entries[0].name = "../escape".into();
    assert!(r.metadata(offer_frames(&bad).remove(0)).is_err());
    assert!(r.offer.is_none());
}
// A relative destination would produce an ambiguous savedPath outside the chooser contract.
#[test]
fn receive_requires_absolute_destination() {
    let mut r = Incoming::default();
    for f in offer_frames(&offer()) {
        r.metadata(f).unwrap();
    }
    assert!(r.accept(Path::new(".")).is_err());
}
fn encrypted_pair() -> (SecureStream, SecureStream) {
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        SecureStream::accept(listener.accept().unwrap().0, &[31; 32]).unwrap()
    });
    let client =
        crate::secure::from_tcp(TcpStream::connect(addr).unwrap(), &[31; 32], true).unwrap();
    (client, peer.join().unwrap())
}
// A shared bulk writer or missing independent receive path would stall simultaneous sends.
#[test]
fn encrypted_simultaneous_transfers_preserve_both_trees() {
    let a = Temp::new();
    let b = Temp::new();
    let out_a = Temp::new();
    let out_b = Temp::new();
    fs::create_dir(a.0.join("folder")).unwrap();
    fs::create_dir(a.0.join("folder/empty")).unwrap();
    let payload = vec![53u8; protocol::CHUNK_SIZE * 7 + 13];
    fs::write(a.0.join("folder/data.bin"), &payload).unwrap();
    fs::write(b.0.join("reply.txt"), b"reply").unwrap();
    let ta = SourceTree::scan(&[a.0.join("folder")], &mut || false).unwrap();
    let tb = SourceTree::scan(&[b.0.join("reply.txt")], &mut || false).unwrap();
    let (sa, sb) = encrypted_pair();
    fn endpoint(mut stream: SecureStream, tree: SourceTree, dest: PathBuf, id: Id) -> PathBuf {
        let writer = SharedWriter::new(stream.try_clone().unwrap());
        for f in offer_frames(&tree.offer(id)) {
            writer.write(f).unwrap();
        }
        let mut incoming = Incoming::default();
        while incoming.offer.is_none() {
            incoming
                .metadata(protocol::read_frame(&mut stream).unwrap())
                .unwrap();
        }
        let peer_id = incoming.offer.as_ref().unwrap().id;
        incoming.accept(&dest).unwrap();
        writer
            .write(control(FrameKind::DropAccept, peer_id))
            .unwrap();
        assert_eq!(
            control_id(&protocol::read_frame(&mut stream).unwrap()).unwrap(),
            id
        );
        let (tx, _rx) = mpsc::sync_channel(32);
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let send_writer = writer.clone();
        let sending = thread::spawn(move || {
            send_tree(
                &send_writer,
                &tree,
                id,
                &AtomicBool::new(false),
                &tx,
                &ack_rx,
            )
            .unwrap()
        });
        let mut saved = None;
        let mut confirmed = false;
        while saved.is_none() || !confirmed {
            let f = protocol::read_frame(&mut stream).unwrap();
            if f.kind == FrameKind::DropResult {
                confirmed = true;
                continue;
            }
            if f.kind == FrameKind::FileAck {
                ack_tx.send(wire::parse_ack(&f.payload).unwrap()).unwrap();
                continue;
            }
            let identity = wire::identity(&f).unwrap();
            let result = incoming.data(f).unwrap();
            writer.write(wire::ack(identity.0, identity.1, 0)).unwrap();
            if let Some(path) = result {
                saved = Some(path);
                let mut p = peer_id.to_vec();
                p.push(0);
                writer.write(Frame::new(FrameKind::DropResult, p)).unwrap();
            }
        }
        sending.join().unwrap();
        saved.unwrap()
    }
    let da = out_a.0.clone();
    let db = out_b.0.clone();
    let left = thread::spawn(move || endpoint(sa, ta, da, [1; 16]));
    let right = thread::spawn(move || endpoint(sb, tb, db, [2; 16]));
    let saved_a = left.join().unwrap();
    let saved_b = right.join().unwrap();
    assert_eq!(fs::read(saved_a.join("reply.txt")).unwrap(), b"reply");
    assert_eq!(fs::read(saved_b.join("folder/data.bin")).unwrap(), payload);
    assert!(saved_b.join("folder/empty").is_dir());
}
#[test]
fn changed_source_fails_original_snapshot_without_sending_content() {
    let src = Temp::new();
    let path = src.0.join("source");
    fs::write(&path, b"before").unwrap();
    let tree = SourceTree::scan(&[path.clone()], &mut || false).unwrap();
    fs::write(path, b"changed").unwrap();
    let (a, _b) = encrypted_pair();
    let (tx, _rx) = mpsc::sync_channel(32);
    let (_at, ar) = mpsc::sync_channel(1);
    assert!(
        send_tree(
            &SharedWriter::new(a),
            &tree,
            [8; 16],
            &AtomicBool::new(false),
            &tx,
            &ar
        )
        .is_err()
    );
}
#[test]
fn dropping_partial_receive_removes_only_its_hidden_staging() {
    let t = Temp::new();
    fs::write(t.0.join("existing"), b"safe").unwrap();
    let mut r = Incoming::default();
    for f in offer_frames(&offer()) {
        r.metadata(f).unwrap();
    }
    r.accept(&t.0).unwrap();
    r.data(wire::request(FrameKind::FileData, [7; 16], 0, 0, 0, b"he"))
        .unwrap();
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 2);
    drop(r);
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 1);
    assert_eq!(fs::read(t.0.join("existing")).unwrap(), b"safe");
}
fn test_lane(stream: SecureStream) -> Lane {
    let (tx, _rx) = mpsc::sync_channel(32);
    Lane {
        writer: SharedWriter::new(stream),
        tx,
        incoming: Incoming::default(),
        outgoing: None,
        incoming_progress: Instant::now(),
        peer_enabled: true,
        ignored: None,
    }
}
#[test]
fn rejection_sends_only_rejection_and_creates_no_files() {
    let _guard = global_guard();
    let (a, mut b) = encrypted_pair();
    let t = Temp::new();
    let mut lane = test_lane(a);
    for f in offer_frames(&offer()) {
        lane.incoming.metadata(f).unwrap();
    }
    lane.handle_command(DropCommand::Reject { id: hex(&[7; 16]) })
        .unwrap();
    assert!(lane.incoming.offer.is_none());
    let f = protocol::read_frame(&mut b).unwrap();
    assert_eq!(f.kind, FrameKind::DropReject);
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 0);
}
#[test]
fn idle_receive_timeout_cancels_and_removes_staging() {
    let _guard = global_guard();
    let (a, mut b) = encrypted_pair();
    let t = Temp::new();
    let mut lane = test_lane(a);
    for f in offer_frames(&offer()) {
        lane.incoming.metadata(f).unwrap();
    }
    lane.incoming.accept(&t.0).unwrap();
    lane.incoming_progress = Instant::now() - Duration::from_secs(31);
    lane.tick();
    assert!(lane.incoming.offer.is_none());
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 0);
    assert_eq!(
        protocol::read_frame(&mut b).unwrap().kind,
        FrameKind::DropCancel
    );
}
#[test]
fn waiting_offer_gets_full_120_seconds_before_cancellation() {
    let _guard = global_guard();
    let (a, mut b) = encrypted_pair();
    let mut lane = test_lane(a);
    for f in offer_frames(&offer()) {
        lane.incoming.metadata(f).unwrap();
    }
    lane.incoming_progress = Instant::now() - Duration::from_secs(119);
    lane.tick();
    assert!(lane.incoming.offer.is_some());
    lane.incoming_progress = Instant::now() - Duration::from_secs(121);
    lane.tick();
    assert!(lane.incoming.offer.is_none());
    assert_eq!(
        protocol::read_frame(&mut b).unwrap().kind,
        FrameKind::DropCancel
    );
}
// Without bounded credit a sender can finish before the receiver has checked even its first chunk.
#[test]
fn sender_requires_receiver_credit_and_cancellation_wakes_waiter() {
    let t = Temp::new();
    let path = t.0.join("x");
    fs::write(&path, b"hello").unwrap();
    let tree = SourceTree::scan(&[path], &mut || false).unwrap();
    let (a, mut b) = encrypted_pair();
    let (tx, _rx) = mpsc::sync_channel(32);
    let (done, completion) = mpsc::sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let cc = cancel.clone();
    let sender = thread::spawn(move || {
        let (_at, ar) = mpsc::sync_channel(1);
        let result = send_tree(&SharedWriter::new(a), &tree, [8; 16], &cc, &tx, &ar);
        done.send(result).unwrap();
    });
    assert_eq!(
        protocol::read_frame(&mut b).unwrap().kind,
        FrameKind::FileData
    );
    thread::sleep(Duration::from_millis(120));
    assert!(
        completion.try_recv().is_err(),
        "sender completed without receiver acknowledgement"
    );
    cancel.store(true, Ordering::Release);
    assert_eq!(
        completion
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    sender.join().unwrap();
}
#[test]
fn second_offer_is_rejected_without_destroying_waiting_first_offer() {
    let _guard = global_guard();
    let (a, mut b) = encrypted_pair();
    let mut lane = test_lane(a);
    for f in offer_frames(&offer()) {
        lane.incoming.metadata(f).unwrap();
    }
    let mut another = offer();
    another.id = [9; 16];
    assert!(lane.handle_frame(offer_frames(&another).remove(0)).is_ok());
    assert_eq!(lane.incoming.offer.as_ref().unwrap().id, [7; 16]);
    let f = protocol::read_frame(&mut b).unwrap();
    assert_eq!(f.kind, FrameKind::DropReject);
    assert_eq!(control_id(&f).unwrap(), [9; 16]);
}
fn global_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
// A peer can vanish after commit; the already-published local folder must remain a visible success.
#[test]
fn saved_local_folder_remains_reported_when_commit_ack_cannot_be_written() {
    let _guard = global_guard();
    let (a, _b) = encrypted_pair();
    let kill = a.try_clone().unwrap();
    let t = Temp::new();
    let mut lane = test_lane(a);
    let o = offer();
    for f in offer_frames(&o) {
        lane.incoming.metadata(f).unwrap();
    }
    lane.incoming.accept(&t.0).unwrap();
    update(|v| v.incoming = Some(transfer(&o, "receiving")));
    lane.incoming
        .data(wire::request(FrameKind::FileData, o.id, 0, 0, 0, b"hello"))
        .unwrap();
    lane.incoming
        .data(wire::request(
            FrameKind::FileSeal,
            o.id,
            1,
            0,
            0,
            &Sha256::digest(b"hello"),
        ))
        .unwrap();
    kill.shutdown(Shutdown::Both).unwrap();
    assert!(
        lane.handle_frame(wire::request(FrameKind::FileCommit, o.id, 2, 0, 0, &[]))
            .is_err()
    );
    let received = view().incoming.unwrap();
    assert_eq!(received.state, "saved");
    assert_eq!(
        fs::read(received.saved_path.unwrap().join("hello.txt")).unwrap(),
        b"hello"
    );
    assert!(lane.incoming.offer.is_none());
}
#[test]
fn lane_disconnect_removes_partial_destination_staging() {
    let _guard = global_guard();
    let t = Temp::new();
    let (local, mut remote) = encrypted_pair();
    command(DropCommand::SetEnabled { enabled: true }).unwrap();
    primary_connected(true);
    let stop = Arc::new(AtomicBool::new(false));
    let lane_stop = stop.clone();
    let worker = thread::spawn(move || run_lane(local, lane_stop));
    protocol::write_frame(&mut remote, &Frame::new(FrameKind::DropStatus, vec![1])).unwrap();
    for f in offer_frames(&offer()) {
        protocol::write_frame(&mut remote, &f).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !view()
        .incoming
        .is_some_and(|i| i.id == hex(&[7; 16]) && i.state == "waiting")
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    command(DropCommand::Accept {
        id: hex(&[7; 16]),
        directory: t.0.clone(),
    })
    .unwrap();
    loop {
        let f = protocol::read_frame(&mut remote).unwrap();
        if f.kind == FrameKind::DropAccept {
            break;
        }
    }
    protocol::write_frame(
        &mut remote,
        &wire::request(FrameKind::FileData, [7; 16], 0, 0, 0, b"he"),
    )
    .unwrap();
    loop {
        if protocol::read_frame(&mut remote).unwrap().kind == FrameKind::FileAck {
            break;
        }
    }
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 1);
    remote.shutdown(Shutdown::Both).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 0);
    assert_eq!(view().incoming.unwrap().state, "cancelled");
    primary_disconnected();
    command(DropCommand::SetEnabled { enabled: false }).unwrap();
}
// A saved path that cannot fit in the bounded UI snapshot must be rejected before staging.
#[test]
fn oversized_destination_is_rejected_before_filesystem_access() {
    let mut receiver = Incoming::default();
    for frame in offer_frames(&offer()) {
        receiver.metadata(frame).unwrap();
    }
    let path = std::env::temp_dir().join("a".repeat(4097));
    let error = receiver.accept(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("路径过长"));
    assert!(receiver.receive.is_none());
}
#[test]
fn escaped_destination_must_fit_snapshot_budget() {
    let mut receiver = Incoming::default();
    for frame in offer_frames(&offer()) {
        receiver.metadata(frame).unwrap();
    }
    let path = std::env::temp_dir().join("\u{0001}".repeat(1500));
    let error = receiver.accept(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("路径过长"));
}
// Raw unprefixed Win32 calls reject staging paths after std has successfully created them.
// Run natively on the Windows CLI CI job; it needs no GUI, clipboard or registry opt-in.
#[cfg(windows)]
#[test]
fn windows_long_parent_supports_hidden_staging_publication_collision_and_cancel() {
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN;
    let t = Temp::new();
    let mut destination = t.0.clone();
    while destination.as_os_str().encode_wide().count() < 235 {
        destination.push("long-parent-segment");
    }
    fs::create_dir_all(&destination).unwrap();
    let mut incoming = Incoming::default();
    let manifest = offer();
    for f in offer_frames(&manifest) {
        incoming.metadata(f).unwrap();
    }
    incoming
        .accept(&destination)
        .expect("std-created long staging must be accepted by native attribute APIs");
    let staging = fs::read_dir(&destination)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(staging.as_os_str().encode_wide().count() > 260);
    assert_ne!(
        fs::metadata(&staging).unwrap().file_attributes() & FILE_ATTRIBUTE_HIDDEN,
        0
    );
    incoming
        .data(wire::request(
            FrameKind::FileData,
            manifest.id,
            0,
            0,
            0,
            b"hello",
        ))
        .unwrap();
    incoming
        .data(wire::request(
            FrameKind::FileSeal,
            manifest.id,
            1,
            0,
            0,
            &Sha256::digest(b"hello"),
        ))
        .unwrap();
    let saved = incoming
        .data(wire::request(
            FrameKind::FileCommit,
            manifest.id,
            2,
            0,
            0,
            &[],
        ))
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(saved.join("hello.txt")).unwrap(), b"hello");
    assert_eq!(
        fs::metadata(&saved).unwrap().file_attributes() & FILE_ATTRIBUTE_HIDDEN,
        0
    );
    assert!(!staging.exists());
    let source = destination.join("existing-source-directory");
    let target = destination.join("existing-target-directory");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("sentinel"), b"untouched").unwrap();
    assert!(rename_exclusive(&source, &target).is_err());
    assert!(source.is_dir());
    assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"untouched");
    let count = fs::read_dir(&destination).unwrap().count();
    let mut partial = Incoming::default();
    for f in offer_frames(&manifest) {
        partial.metadata(f).unwrap();
    }
    partial.accept(&destination).unwrap();
    partial
        .data(wire::request(
            FrameKind::FileData,
            manifest.id,
            0,
            0,
            0,
            b"he",
        ))
        .unwrap();
    assert_eq!(fs::read_dir(&destination).unwrap().count(), count + 1);
    drop(partial);
    assert_eq!(fs::read_dir(&destination).unwrap().count(), count);
    assert_eq!(fs::read(saved.join("hello.txt")).unwrap(), b"hello");
}
#[cfg(windows)]
#[test]
fn windows_native_path_rejects_embedded_nul_before_filesystem_access() {
    use std::os::windows::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_wide(&[67, 58, 92, 120, 0, 121]));
    let error = crate::file_transfer::windows_native_path(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
// A local filesystem failure is distinct from the user explicitly declining the offer.
#[test]
fn destination_failure_reports_failed_result_to_sender_without_staging() {
    let _guard = global_guard();
    let (a, mut b) = encrypted_pair();
    let t = Temp::new();
    let mut lane = test_lane(a);
    let manifest = offer();
    for frame in offer_frames(&manifest) {
        lane.incoming.metadata(frame).unwrap();
    }
    let was_enabled = enabled();
    update(|view| {
        view.enabled = true;
        view.incoming = Some(transfer(&manifest, "waiting"));
    });
    let result = lane.handle_command(DropCommand::Accept {
        id: hex(&manifest.id),
        directory: t.0.join("x".repeat(4097)),
    });
    update(|view| view.enabled = was_enabled);
    assert!(result.is_err());
    let reply = protocol::read_frame(&mut b).unwrap();
    assert_eq!(reply.kind, FrameKind::DropResult);
    assert_eq!(reply.payload.len(), 17);
    assert_eq!(&reply.payload[..16], &manifest.id);
    assert_eq!(reply.payload[16], 1);
    let local = view().incoming.unwrap();
    assert_eq!(local.state, "failed");
    assert!(local.message.unwrap().contains("路径过长"));
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), 0);
    assert!(lane.incoming.offer.is_none());
}
