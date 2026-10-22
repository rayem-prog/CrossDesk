use super::file_tests::{Memory, streams};
use super::*;
use std::sync::atomic::AtomicUsize;

#[test]
#[ignore = "subprocess fixture; use cancelled_text_reports_a_terminal_receive_status"]
fn cancelled_text_event_fixture() {
    assert!(std::env::var_os("CROSSDESK_TEXT_CANCEL_FIXTURE").is_some());
    let (stream, _peer) = streams();
    let writer = SharedWriter::new(stream);
    let mailbox = Mailbox::default();
    mailbox.texts_enabled.store(true, Ordering::Release);
    let transfer = text_transfer::Transfer::from_text("cancel me").unwrap();
    let mut frames = Vec::new();
    transfer
        .visit_frames(|frame| {
            frames.push(frame);
            Ok(())
        })
        .unwrap();
    receive_text(frames.remove(0), 0, &mailbox, &writer).unwrap();
    match std::env::var("CROSSDESK_TEXT_CANCEL_FIXTURE")
        .unwrap()
        .as_str()
    {
        "replaced" => {
            for frame in frames {
                receive_text(frame, 0, &mailbox, &writer).unwrap();
            }
            let mut png = std::io::Cursor::new(Vec::new());
            image::RgbaImage::from_pixel(1, 1, image::Rgba([4, 2, 3, 255]))
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let transfer =
                Transfer::from_clipboard(&ClipboardPayload::Bitmap(png.into_inner())).unwrap();
            enqueue(
                &mailbox,
                RemoteDelivery::Image(Delivery {
                    transfer,
                    revision: 0,
                }),
                &writer,
            );
        }
        "short" => assert!(
            receive_text(Frame::new(FrameKind::TextEnd, vec![]), 0, &mailbox, &writer).is_err()
        ),
        "wrong" => {
            let mut payload = vec![99; 16];
            payload.extend_from_slice(&0u32.to_be_bytes());
            payload.push(b'x');
            assert!(
                receive_text(
                    Frame::new(FrameKind::TextChunk, payload),
                    0,
                    &mailbox,
                    &writer
                )
                .is_err()
            );
        }
        _ => receive_text(text_transfer::cancel(transfer.id), 0, &mailbox, &writer).unwrap(),
    }
}

#[test]
#[cfg(feature = "gui")]
fn malformed_text_reports_failure_for_the_discarded_active_batch() {
    for mode in ["short", "wrong"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "clipboard::session::text_tests::cancelled_text_event_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("CROSSDESK_TEXT_CANCEL_FIXTURE", mode)
            .env("DESKBRIDGE_GUI_CHILD", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        let events: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(crate::service_event::parse_transfer)
            .collect();
        assert_eq!(
            events.last().unwrap().state,
            TransferState::Failed,
            "discarded {mode} batch has no terminal status"
        );
        assert_eq!(events.first().unwrap().id, events.last().unwrap().id);
    }
}

#[test]
#[cfg(feature = "gui")]
fn replacing_queued_text_with_an_image_reports_the_text_as_skipped() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "clipboard::session::text_tests::cancelled_text_event_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("CROSSDESK_TEXT_CANCEL_FIXTURE", "replaced")
        .env("DESKBRIDGE_GUI_CHILD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let events: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(crate::service_event::parse_transfer)
        .collect();
    assert_eq!(events.last().unwrap().state, TransferState::Skipped);
    assert_eq!(events.first().unwrap().id, events.last().unwrap().id);
}

#[test]
#[cfg(feature = "gui")]
fn cancelled_text_reports_a_terminal_receive_status() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "clipboard::session::text_tests::cancelled_text_event_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("CROSSDESK_TEXT_CANCEL_FIXTURE", "1")
        .env("DESKBRIDGE_GUI_CHILD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let events: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(crate::service_event::parse_transfer)
        .collect();
    assert_eq!(events.last().unwrap().state, TransferState::Skipped);
    assert_eq!(events.last().unwrap().direction, TransferDirection::Receive);
}

struct ChangingCopy(AtomicUsize);
impl ClipboardApi for ChangingCopy {
    fn change_id(&self) -> io::Result<u64> {
        Ok(if self.0.fetch_add(1, Ordering::AcqRel) < 2 {
            1
        } else {
            2
        })
    }
    fn read(&mut self) -> io::Result<Option<ClipboardPayload>> {
        unreachable!()
    }
    fn write(&mut self, _: &ClipboardPayload) -> io::Result<()> {
        unreachable!()
    }
}

#[test]
fn invalid_native_text_is_consumed_once_instead_of_retried_forever() {
    struct InvalidNative(Arc<AtomicUsize>);
    impl ClipboardApi for InvalidNative {
        fn change_id(&self) -> io::Result<u64> {
            Ok(1)
        }
        fn read(&mut self) -> io::Result<Option<ClipboardPayload>> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Err(text_transfer::invalid("invalid native text"))
        }
        fn write(&mut self, _: &ClipboardPayload) -> io::Result<()> {
            unreachable!()
        }
    }
    let (stream, _peer) = streams();
    let mailbox = Arc::new(Mailbox::default());
    mailbox.texts_enabled.store(true, Ordering::Release);
    let reads = Arc::new(AtomicUsize::new(0));
    let fake = InvalidNative(reads.clone());
    let target = mailbox.clone();
    let worker = thread::spawn(move || worker_loop(fake, 0, SharedWriter::new(stream), target));
    thread::sleep(Duration::from_millis(650));
    mailbox.stop.store(true, Ordering::Release);
    mailbox.changed.notify_all();
    worker.join().unwrap();
    assert_eq!(
        reads.load(Ordering::Acquire),
        2,
        "invalid native contents were retried"
    );
}

#[test]
fn a_new_copy_cancels_partial_text_without_closing_the_connection() {
    let (stream, mut peer) = streams();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let writer = SharedWriter::new(stream);
    let mailbox = Mailbox::default();
    let error = send_text(
        &"x".repeat(2 * crate::protocol::CHUNK_SIZE),
        1,
        &ChangingCopy(AtomicUsize::new(0)),
        &writer,
        &mailbox,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let mut incoming = text_transfer::Incoming::default();
    for expected in [
        FrameKind::TextStart,
        FrameKind::TextChunk,
        FrameKind::TextCancel,
    ] {
        let frame = crate::protocol::read_frame(&mut peer).unwrap();
        assert_eq!(frame.kind, expected);
        assert!(incoming.receive(frame).unwrap().is_none());
    }
    assert!(incoming.active_id().is_none());
    writer.write(Frame::new(FrameKind::Ping, vec![])).unwrap();
    assert_eq!(
        crate::protocol::read_frame(&mut peer).unwrap().kind,
        FrameKind::Ping
    );
}

#[test]
fn invalid_or_empty_text_never_sends_partial_frames_or_clears_the_peer() {
    let (stream, mut peer) = streams();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let writer = SharedWriter::new(stream);
    let mailbox = Mailbox::default();
    let memory = Memory(Arc::new(Mutex::new((
        0,
        ClipboardPayload::Text("keep".into()),
        0,
    ))));
    for text in [
        "".to_string(),
        "bad\0content".into(),
        "x".repeat(text_transfer::MAX_TEXT_BYTES + 1),
    ] {
        send_text(&text, 0, &memory, &writer, &mailbox).unwrap();
    }
    writer.write(Frame::new(FrameKind::Ping, vec![])).unwrap();
    assert_eq!(
        crate::protocol::read_frame(&mut peer).unwrap().kind,
        FrameKind::Ping
    );
}

#[test]
fn text_publication_is_guarded_and_preserves_unicode_and_whitespace() {
    let mut memory = Memory(Arc::new(Mutex::new((
        4,
        ClipboardPayload::Text("previous".into()),
        0,
    ))));
    let text = " 中文🙂\n  code();\n\n ";
    let result = apply_text_delivery(
        &mut memory,
        TextDelivery {
            transfer: text_transfer::Transfer::from_text(text).unwrap(),
            revision: 4,
        },
        None,
    )
    .unwrap();
    assert_eq!(result, Some(5));
    assert_eq!(
        memory.0.lock().unwrap().1,
        ClipboardPayload::Text(text.into())
    );
    assert_eq!(memory.0.lock().unwrap().2, 1);
}

#[test]
fn a_new_text_copy_uses_the_encrypted_session_when_images_and_files_are_off() {
    let (stream, mut peer) = streams();
    peer.set_read_timeout(Some(Duration::from_millis(600)))
        .unwrap();
    let memory = Memory(Arc::new(Mutex::new((
        0,
        ClipboardPayload::Text("before connection".into()),
        0,
    ))));
    let mailbox = Arc::new(Mailbox::default());
    mailbox.images_enabled.store(false, Ordering::Release);
    mailbox.texts_enabled.store(true, Ordering::Release);
    let fake = memory.clone();
    let target = mailbox.clone();
    let worker = thread::spawn(move || worker_loop(fake, 0, SharedWriter::new(stream), target));
    assert!(
        crate::protocol::read_frame(&mut peer).is_err(),
        "startup text was sent"
    );
    {
        let mut state = memory.0.lock().unwrap();
        state.0 += 1;
        state.1 = ClipboardPayload::Text(" 中文🙂\r\n  code();\n\n ".into());
    }
    mailbox.changed.notify_all();
    let result = crate::protocol::read_frame(&mut peer);
    mailbox.stop.store(true, Ordering::Release);
    mailbox.changed.notify_all();
    worker.join().unwrap();
    assert_eq!(result.unwrap().kind, FrameKind::TextStart);
}
