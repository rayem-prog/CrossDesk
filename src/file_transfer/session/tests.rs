use super::*;

#[test]
fn encrypted_folder_copy_preserves_empty_nested_hidden_and_mixed_items_in_both_directions() {
    let root = root("folder-duplex");
    let folder = root.join("source/资料");
    std::fs::create_dir_all(folder.join("多层/空目录")).unwrap();
    std::fs::write(folder.join("多层/报告.pdf"), b"%PDF-folder\0\xff").unwrap();
    std::fs::write(folder.join(".hidden"), b"hidden").unwrap();
    std::fs::write(folder.join("zero"), []).unwrap();
    let loose = root.join("loose.txt");
    std::fs::write(&loose, b"mixed").unwrap();
    let (left, right) = pair(root.join("receive-left"), root.join("receive-right"));
    for (sender, receiver) in [(&left, &right), (&right, &left)] {
        sender
            .sender
            .send(&[folder.clone(), loose.clone()], || false)
            .unwrap();
        let batch = receiver.done.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            batch.files.len(),
            2,
            "only selected roots belong on the clipboard"
        );
        assert!(batch.files[0].join("多层/空目录").is_dir());
        assert_eq!(
            std::fs::read(batch.files[0].join("多层/报告.pdf")).unwrap(),
            b"%PDF-folder\0\xff"
        );
        assert_eq!(
            std::fs::read(batch.files[0].join(".hidden")).unwrap(),
            b"hidden"
        );
        assert_eq!(
            std::fs::metadata(batch.files[0].join("zero"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(std::fs::read(&batch.files[1]).unwrap(), b"mixed");
        drop(batch);
    }
    drop(left);
    drop(right);
    assert_eq!(
        std::fs::read_dir(root.join("receive-left"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(root.join("receive-right"))
            .unwrap()
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn encrypted_paged_manifest_transfers_the_file_after_hundreds_of_directories() {
    let root = root("encrypted-pages");
    let source = root.join("source");
    std::fs::create_dir_all(&source).unwrap();
    for i in 0..800 {
        std::fs::create_dir(source.join(format!("{i:04}-{}", "a".repeat(85)))).unwrap();
    }
    std::fs::write(
        source.join("zz-final.txt"),
        b"last file after metadata pages",
    )
    .unwrap();
    let (left, right) = pair(root.join("left"), root.join("right"));
    left.sender.send(&[source], || false).unwrap();
    let batch = right.done.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(batch.files.len(), 1);
    assert_eq!(std::fs::read_dir(&batch.files[0]).unwrap().count(), 801);
    assert_eq!(
        std::fs::read(batch.files[0].join("zz-final.txt")).unwrap(),
        b"last file after metadata pages"
    );
    drop(batch);
    drop(left);
    drop(right);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nested_cache_contents_count_toward_the_budget() {
    let root = root("recursive-budget");
    std::fs::create_dir_all(root.join("old/a/b/c")).unwrap();
    std::fs::write(root.join("old/a/b/c/keep.bin"), [0; 10]).unwrap();
    let mut receiver = Receiver::new(root.clone());
    receiver.cache_budget = 10;
    assert!(receiver.handle(offer([9; 16], 1), 0).is_err());
    assert_eq!(
        std::fs::read(root.join("old/a/b/c/keep.bin")).unwrap(),
        [0; 10]
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_pages_are_bounded_and_not_applied_until_complete() {
    let mut entries = vec![Entry {
        parent: None,
        directory: true,
        name: "root".into(),
        len: 0,
    }];
    for i in 0..1500 {
        entries.push(Entry {
            parent: Some(0),
            directory: true,
            name: format!("{i:04}-{}", "a".repeat(70)),
            len: 0,
        });
    }
    let frames = Offer {
        id: [21; 16],
        entries,
    }
    .frames();
    assert!(frames.len() > 1);
    let count = frames.len() as u64;
    let root = root("pages");
    let mut receiver = Receiver::new(root.clone());
    for (n, frame) in frames.into_iter().enumerate() {
        assert!(frame.payload.len() <= crate::protocol::MAX_FRAME_PAYLOAD_SIZE);
        let mut encoded = Vec::new();
        crate::protocol::write_frame(&mut encoded, &frame).unwrap();
        receiver
            .handle(
                crate::protocol::read_frame(&mut encoded.as_slice()).unwrap(),
                45,
            )
            .unwrap();
        if n == 0 {
            assert!(!root.exists(), "partial metadata created a staging tree");
        }
    }
    let batch = receiver
        .handle(
            wire::request(FrameKind::FileCommit, [21; 16], count, 0, 0, &[]),
            99,
        )
        .unwrap()
        .unwrap();
    assert_eq!(batch.revision, 45);
    assert_eq!(std::fs::read_dir(&batch.files[0]).unwrap().count(), 1500);
    let staging = batch.root.clone();
    drop(batch);
    assert!(!staging.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn duplicate_directory_names_keep_children_in_the_allocated_subtree() {
    let root = root("directory-names");
    let entries = vec![
        Entry {
            parent: None,
            directory: true,
            name: "é".into(),
            len: 0,
        },
        Entry {
            parent: Some(0),
            directory: true,
            name: "first".into(),
            len: 0,
        },
        Entry {
            parent: None,
            directory: true,
            name: "e\u{301}".into(),
            len: 0,
        },
        Entry {
            parent: Some(2),
            directory: true,
            name: "second".into(),
            len: 0,
        },
    ];
    let mut receiver = Receiver::new(root.clone());
    receiver
        .handle(
            Offer {
                id: [22; 16],
                entries,
            }
            .frame(),
            0,
        )
        .unwrap();
    let mut batch = receiver
        .handle(
            wire::request(FrameKind::FileCommit, [22; 16], 1, 0, 0, &[]),
            0,
        )
        .unwrap()
        .unwrap();
    assert_eq!(batch.files.len(), 2);
    assert!(batch.files[0].join("first").is_dir());
    assert!(batch.files[1].join("second").is_dir());
    assert!(
        batch.files[1]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("(2)")
    );
    batch.retain();
    let old = batch.files.clone();
    drop(batch);
    receiver.handle(offer([23; 16], 1), 0).unwrap();
    drop(receiver);
    assert!(old.iter().all(|p| p.exists()));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn directory_add_remove_and_file_rewrite_during_send_cancel_before_publication() {
    for operation in 0..3 {
        let root = root("changed-source");
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("data.bin"), vec![7; 100_000]).unwrap();
        let (left, right) = pair(root.join("left"), root.join("right"));
        let mut changed = false;
        let error = left
            .sender
            .send(&[source.clone()], || {
                if !changed
                    && std::fs::read_dir(root.join("right")).is_ok_and(|mut d| d.next().is_some())
                {
                    match operation {
                        0 => std::fs::write(source.join("new"), b"new").unwrap(),
                        1 => std::fs::remove_file(source.join("data.bin")).unwrap(),
                        _ => std::fs::write(source.join("data.bin"), b"rewrite").unwrap(),
                    }
                    changed = true;
                }
                false
            })
            .unwrap_err();
        assert!(changed);
        assert_ne!(error.kind(), io::ErrorKind::TimedOut);
        drop(left);
        drop(right);
        assert_eq!(std::fs::read_dir(root.join("right")).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn partial_manifest_cancel_and_excess_declared_bytes_never_allocate_disk() {
    let root = root("partial-manifest");
    let mut receiver = Receiver::new(root.clone());
    let mut b = [24; 16].to_vec();
    b.extend_from_slice(&100u32.to_be_bytes());
    b.push(0);
    receiver
        .handle(Frame::new(FrameKind::FileOffer, b), 0)
        .unwrap();
    receiver
        .handle(Frame::new(FrameKind::FileCancel, vec![24; 16]), 0)
        .unwrap();
    assert!(receiver.pending.is_none());
    let mut b = [25; 16].to_vec();
    b.extend_from_slice(&(4u32 * 1024 * 1024 + 1).to_be_bytes());
    b.push(0);
    assert!(
        receiver
            .handle(Frame::new(FrameKind::FileOffer, b), 0)
            .is_err()
    );
    assert!(!root.exists());
}

#[test]
fn commit_credit_waits_until_the_completed_batch_is_handed_to_publication() {
    let root = root("commit-credit");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let accept = thread::spawn(move || {
        crate::secure::SecureStream::accept(listener.accept().unwrap().0, &[32; 32]).unwrap()
    });
    let client = crate::secure::SecureStream::connect(&address, &[32; 32]).unwrap();
    let mut peer = accept.join().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    let (entered, arrived) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let session = FileSession::start(SharedWriter::new(client), root.clone(), move |_| {
        entered.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(3)).unwrap();
    })
    .unwrap();
    let id = [31; 16];
    session.receive(offer(id, 0), 0).unwrap();
    crate::protocol::read_frame(&mut peer).unwrap();
    session
        .receive(
            wire::request(FrameKind::FileSeal, id, 1, 0, 0, &Sha256::digest([])),
            0,
        )
        .unwrap();
    crate::protocol::read_frame(&mut peer).unwrap();
    session
        .receive(wire::request(FrameKind::FileCommit, id, 2, 0, 0, &[]), 0)
        .unwrap();
    arrived.recv_timeout(Duration::from_secs(2)).unwrap();
    let early_credit = crate::protocol::read_frame(&mut peer).is_ok();
    release.send(()).unwrap();
    if !early_credit {
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(
            wire::parse_ack(&crate::protocol::read_frame(&mut peer).unwrap().payload)
                .unwrap()
                .status,
            1
        );
    }
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        !early_credit,
        "sender could start its next copy before the previous one was queued"
    );
}
fn root(tag: &str) -> PathBuf {
    let mut id = [0; 8];
    getrandom::getrandom(&mut id).unwrap();
    std::env::temp_dir().join(format!("crossdesk-file-{tag}-{:x}", u64::from_le_bytes(id)))
}
fn offer(id: Id, len: u64) -> Frame {
    Offer {
        id,
        entries: vec![Entry {
            parent: None,
            directory: false,
            name: "报告.pdf".into(),
            len,
        }],
    }
    .frame()
}
#[test]
fn corrupted_or_incomplete_batches_never_return_files_and_drop_removes_partials() {
    let root = root("corruption");
    let mut receiver = Receiver::new(root.clone());
    let id = [1; 16];
    receiver.handle(offer(id, 4), 7).unwrap();
    receiver
        .handle(wire::request(FrameKind::FileData, id, 1, 0, 0, b"abcd"), 7)
        .unwrap();
    assert!(
        receiver
            .handle(wire::request(FrameKind::FileSeal, id, 2, 0, 0, &[0; 32]), 7)
            .is_err()
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    receiver.handle(offer(id, 4), 8).unwrap();
    assert!(
        receiver
            .handle(wire::request(FrameKind::FileCommit, id, 1, 0, 0, &[]), 8)
            .is_err()
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    receiver.handle(offer(id, 4), 9).unwrap();
    drop(receiver);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn verified_batch_preserves_bytes_revision_and_file_lifetime() {
    let root = root("verified");
    let id = [2; 16];
    let mut r = Receiver::new(root.clone());
    r.handle(offer(id, 5), 42).unwrap();
    r.handle(
        wire::request(FrameKind::FileData, id, 1, 0, 0, b"%PDF\0"),
        999,
    )
    .unwrap();
    r.handle(
        wire::request(FrameKind::FileSeal, id, 2, 0, 0, &Sha256::digest(b"%PDF\0")),
        999,
    )
    .unwrap();
    let mut batch = r
        .handle(wire::request(FrameKind::FileCommit, id, 3, 0, 0, &[]), 999)
        .unwrap()
        .unwrap();
    assert_eq!(batch.revision, 42);
    assert_eq!(std::fs::read(&batch.files[0]).unwrap(), b"%PDF\0");
    batch.retain();
    let path = batch.files[0].clone();
    drop(batch);
    r.handle(offer([3; 16], 0), 100).unwrap();
    drop(r);
    assert!(path.exists());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn offsets_serials_and_extra_data_are_rejected_before_publication() {
    for (serial, offset, data) in [(2, 0, b"a".as_slice()), (1, 1, b"a"), (1, 0, b"too long")] {
        let root = root("offset");
        let mut r = Receiver::new(root.clone());
        r.handle(offer([3; 16], 1), 0).unwrap();
        assert!(
            r.handle(
                wire::request(FrameKind::FileData, [3; 16], serial, 0, offset, data),
                0
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}

struct Endpoint {
    sender: FileSender,
    done: mpsc::Receiver<CompletedBatch>,
    socket: crate::secure::SecureStream,
    reader: Option<JoinHandle<()>>,
    input: Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(t) = self.reader.take() {
            t.join().unwrap();
        }
    }
}
fn endpoint(mut stream: crate::secure::SecureStream, root: PathBuf) -> Endpoint {
    let socket = stream.try_clone().unwrap();
    let writer = SharedWriter::new(stream.try_clone().unwrap());
    let (tx, done) = mpsc::channel();
    let session = FileSession::start(writer, root, move |b| {
        tx.send(b).unwrap();
    })
    .unwrap();
    let sender = session.sender.clone();
    let input = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = input.clone();
    let reader = thread::spawn(move || {
        while let Ok(frame) = crate::protocol::read_frame(&mut stream) {
            if matches!(
                frame.kind,
                FrameKind::Input | FrameKind::Ping | FrameKind::Pong
            ) {
                counter.fetch_add(1, Ordering::AcqRel);
            } else {
                session.receive(frame, 77).unwrap();
            }
        }
    });
    Endpoint {
        sender,
        done,
        socket,
        reader: Some(reader),
        input,
    }
}
fn pair(a: PathBuf, b: PathBuf) -> (Endpoint, Endpoint) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = thread::spawn(move || {
        crate::secure::SecureStream::accept(listener.accept().unwrap().0, &[56; 32]).unwrap()
    });
    let client = crate::secure::SecureStream::connect(&addr, &[56; 32]).unwrap();
    (endpoint(client, a), endpoint(server.join().unwrap(), b))
}
#[test]
fn encrypted_streams_transfer_files_both_directions_with_input_and_heartbeat() {
    let root = root("duplex");
    std::fs::create_dir_all(root.join("left")).unwrap();
    std::fs::create_dir_all(root.join("right")).unwrap();
    let a = root.join("left/报告.pdf");
    let b = root.join("right/报告.pdf");
    let empty = root.join("empty");
    let original: Vec<u8> = (0..262_150).map(|n| (n % 251) as u8).collect();
    std::fs::write(&a, &original).unwrap();
    std::fs::write(&b, b"%PDF-second\0\xff").unwrap();
    std::fs::write(&empty, b"").unwrap();
    let (left, right) = pair(root.join("receive-left"), root.join("receive-right"));
    let l = left.sender.clone();
    let r = right.sender.clone();
    let sources = vec![a, b, empty];
    let sources2 = sources.clone();
    let send_left = thread::spawn(move || l.send(&sources, || false));
    let send_right = thread::spawn(move || r.send(&sources2, || false));
    for _ in 0..30 {
        left.sender
            .writer
            .write(Frame::new(FrameKind::Input, vec![6]))
            .unwrap();
        right
            .sender
            .writer
            .write(Frame::new(FrameKind::Ping, vec![]))
            .unwrap();
    }
    send_left.join().unwrap().unwrap();
    send_right.join().unwrap().unwrap();
    let l = left.done.recv_timeout(Duration::from_secs(3)).unwrap();
    let r = right.done.recv_timeout(Duration::from_secs(3)).unwrap();
    for batch in [&l, &r] {
        assert_eq!(batch.files.len(), 3);
        assert_eq!(std::fs::read(&batch.files[0]).unwrap(), original);
        assert_eq!(
            std::fs::read(&batch.files[1]).unwrap(),
            b"%PDF-second\0\xff"
        );
        assert_eq!(std::fs::metadata(&batch.files[2]).unwrap().len(), 0);
        assert_ne!(batch.files[0], batch.files[1]);
    }
    assert_eq!(left.input.load(Ordering::Acquire), 30);
    assert_eq!(right.input.load(Ordering::Acquire), 30);
    drop(l);
    drop(r);
    drop(left);
    drop(right);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn cancellation_removes_partial_batch_and_next_copy_can_succeed() {
    let root = root("cancel");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("binary.zip");
    std::fs::write(&path, vec![77; 100_000]).unwrap();
    let (left, right) = pair(root.join("left"), root.join("right"));
    let destination = root.join("right");
    let result = left.sender.send(&[path.clone()], || {
        std::fs::read_dir(&destination).ok().is_some_and(|dirs| {
            dirs.flatten().any(|dir| {
                std::fs::read_dir(dir.path()).ok().is_some_and(|files| {
                    // Windows directory enumeration can report the old file size
                    // until the writer closes. Query a live handle so cancellation
                    // happens during data transfer, before FileCommit publishes it.
                    files.flatten().any(|f| {
                        std::fs::File::open(f.path())
                            .and_then(|file| file.metadata())
                            .is_ok_and(|m| m.len() > 0)
                    })
                })
            })
        })
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    // A new copy is accepted after the ordered cancel on the same connection.
    left.sender.send(&[path], || false).unwrap();
    let batch = right.done.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(std::fs::read(&batch.files[0]).unwrap(), vec![77; 100_000]);
    assert_eq!(std::fs::read_dir(root.join("right")).unwrap().count(), 1);
    assert!(matches!(
        right.done.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    drop(batch);
    drop(left);
    drop(right);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stalled_transfer_expiry_and_explicit_cancel_remove_only_their_partial_files() {
    let root = root("expiry");
    let mut r = Receiver::new(root.clone());
    let id = [8; 16];
    r.handle(offer(id, 1), 0).unwrap();
    r.active.as_mut().unwrap().progress = Instant::now() - TIMEOUT - Duration::from_secs(1);
    r.expire();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    r.handle(offer(id, 1), 0).unwrap();
    r.handle(Frame::new(FrameKind::FileCancel, id.to_vec()), 0)
        .unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn cache_budget_is_enforced_before_accepting_a_new_batch() {
    let root = root("budget");
    std::fs::create_dir_all(root.join("completed")).unwrap();
    let file = std::fs::File::create(root.join("completed/large.bin")).unwrap();
    file.set_len(4).unwrap();
    let mut r = Receiver::new(root.clone());
    r.cache_budget = 4;
    assert!(r.handle(offer([9; 16], 1), 0).is_err());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    drop(file);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn session_stop_unblocks_a_sender_waiting_for_peer_credit() {
    let root = root("stop");
    std::fs::create_dir(&root).unwrap();
    let file = root.join("report.pdf");
    std::fs::write(&file, b"%PDF").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let accept = thread::spawn(move || {
        crate::secure::SecureStream::accept(listener.accept().unwrap().0, &[60; 32]).unwrap()
    });
    let client = crate::secure::SecureStream::connect(&addr, &[60; 32]).unwrap();
    let mut peer = accept.join().unwrap();
    let session =
        FileSession::start(SharedWriter::new(client), root.join("receive"), |_| {}).unwrap();
    let sender = session.sender.clone();
    let (tx, done) = mpsc::channel();
    let task = thread::spawn(move || tx.send(sender.send(&[file], || false)).unwrap());
    assert_eq!(
        crate::protocol::read_frame(&mut peer).unwrap().kind,
        FrameKind::FileOffer
    );
    session.stop();
    assert_eq!(
        done.recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    task.join().unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "Large streaming acceptance; run explicitly for release validation"]
fn large_file_stream_exceeds_image_batch_limit_without_buffering_the_file() {
    let root = root("large");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("large.bin");
    let bytes = 129u64 * 1024 * 1024;
    std::fs::File::create(&path)
        .unwrap()
        .set_len(bytes)
        .unwrap();
    let (left, right) = pair(root.join("left"), root.join("right"));
    left.sender.send(&[path], || false).unwrap();
    let batch = right.done.recv_timeout(Duration::from_secs(3)).unwrap();
    let mut file = std::fs::File::open(&batch.files[0]).unwrap();
    assert_eq!(file.metadata().unwrap().len(), bytes);
    let mut buffer = [1u8; 32 * 1024];
    let mut read = 0u64;
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        assert!(buffer[..n].iter().all(|b| *b == 0));
        read += n as u64;
    }
    assert_eq!(read, bytes);
    drop(file);
    drop(batch);
    drop(left);
    drop(right);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_stopped_session_cannot_write_a_new_offer_through_a_retained_sender() {
    let root = root("stopped-offer");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("report.pdf");
    std::fs::write(&path, b"%PDF").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let accepted = thread::spawn(move || {
        crate::secure::SecureStream::accept(listener.accept().unwrap().0, &[63; 32]).unwrap()
    });
    let client = crate::secure::SecureStream::connect(&address, &[63; 32]).unwrap();
    let mut peer = accepted.join().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    let session =
        FileSession::start(SharedWriter::new(client), root.join("receive"), |_| {}).unwrap();
    let sender = session.sender.clone();
    session.stop();
    assert_eq!(
        sender.send(&[path], || false).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    assert!(
        crate::protocol::read_frame(&mut peer).is_err(),
        "stopped session still wrote to its old connection"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}
