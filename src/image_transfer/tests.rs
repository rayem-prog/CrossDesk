// CrossDesk image clipboard changes: 2026-10-20 (GPL-3.0-only).
use super::*;
use crate::protocol::{ClipboardPayload, Frame, FrameKind};
use std::io::Cursor;

fn png(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut seed = 123456789u32;
    let pixels: Vec<u8> = (0..width * height * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let image = image::RgbaImage::from_raw(width, height, pixels.clone()).unwrap();
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    (encoded.into_inner(), pixels)
}

fn frames(transfer: &Transfer) -> Vec<Frame> {
    let mut out = Vec::new();
    transfer
        .visit_frames(|frame| {
            out.push(frame);
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn image_larger_than_one_frame_round_trips_with_every_alpha_pixel() {
    let (bytes, pixels) = png(512, 256);
    assert!(bytes.len() > 64 * 1024);
    let transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(bytes)).unwrap();
    let mut receiver = Incoming::default();
    let mut completed = None;
    for frame in frames(&transfer) {
        assert!(frame.payload.len() <= 64 * 1024);
        completed = receiver.receive(frame).unwrap().or(completed);
    }
    let received = completed.unwrap();
    let image = image::load_from_memory(&received.images[0].data)
        .unwrap()
        .into_rgba8();
    assert_eq!(image.dimensions(), (512, 256));
    assert_eq!(image.into_raw(), pixels);
}

#[test]
fn interrupted_transfer_never_returns_a_clipboard_and_bad_offset_resets_it() {
    let (bytes, _) = png(32, 32);
    let transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(bytes)).unwrap();
    let mut packets = frames(&transfer);
    let mut receiver = Incoming::default();
    assert!(receiver.receive(packets.remove(0)).unwrap().is_none());
    let mut chunk = packets.remove(0);
    chunk.payload[23] = 1; // first chunk offset must be zero
    assert!(receiver.receive(chunk).is_err());
    assert!(receiver.receive(packets.pop().unwrap()).is_err());
    let mut complete = None;
    for frame in frames(&transfer) {
        complete = receiver.receive(frame).unwrap().or(complete);
    }
    assert!(complete.is_some());
}

#[test]
fn content_hash_rejects_corrupted_chunks_before_publishing() {
    let (bytes, _) = png(32, 32);
    let transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(bytes)).unwrap();
    let mut receiver = Incoming::default();
    let mut rejected = false;
    for mut frame in frames(&transfer) {
        if frame.kind == FrameKind::ImageChunk {
            *frame.payload.last_mut().unwrap() ^= 1;
        }
        if receiver.receive(frame).is_err() {
            rejected = true;
        }
    }
    assert!(rejected);
}

#[test]
fn files_preserve_bytes_and_chinese_names_but_mixed_selections_are_rejected() {
    let directory = temp_directory();
    let (bytes, _) = png(2, 2);
    let first = directory.join("截图.png");
    let second = directory.join("第二张.JPG");
    let mut jpeg = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(2, 2, image::Rgb([30, 40, 50]))
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let jpeg = jpeg.into_inner();
    std::fs::write(&first, &bytes).unwrap();
    std::fs::write(&second, &jpeg).unwrap();
    let transfer =
        Transfer::from_clipboard(&ClipboardPayload::Files(vec![first.clone(), second])).unwrap();
    let mut receiver = Incoming::default();
    let mut result = None;
    for frame in frames(&transfer) {
        result = receiver.receive(frame).unwrap().or(result);
    }
    let result = result.unwrap();
    assert_eq!(result.images[0].name, "截图.png");
    assert_eq!(result.images[1].name, "第二张.JPG");
    assert_eq!(result.images[1].data, jpeg);
    assert_eq!(result.images[0].data, bytes);
    let text = directory.join("private.txt");
    std::fs::write(&text, b"not an image").unwrap();
    assert!(Transfer::from_clipboard(&ClipboardPayload::Files(vec![first, text])).is_err());
    assert!(Transfer::from_clipboard(&ClipboardPayload::Files(vec![directory.clone()])).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn malformed_manifests_and_unsafe_filenames_are_rejected() {
    for name in [
        "../image.png",
        "C:\\image.png",
        "/image.png",
        "a/b.png",
        "a\\b.png",
        "NUL.png",
        "a.png:",
        "a.png.",
    ] {
        assert!(!safe_image_name(name), "accepted {name}");
    }
    assert!(safe_image_name("你好.PNG"));
    let mut receiver = Incoming::default();
    assert!(
        receiver
            .receive(Frame::new(FrameKind::ImageStart, vec![0; 19]))
            .is_err()
    );
    assert!(
        receiver
            .receive(Frame::new(FrameKind::ImageStart, vec![0; 64 * 1024]))
            .is_err()
    );
}

#[test]
fn identical_pixels_in_different_image_encodings_have_the_same_fingerprint() {
    let pixels = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
    let mut bmp = Cursor::new(Vec::new());
    pixels.write_to(&mut bmp, image::ImageFormat::Bmp).unwrap();
    let mut png = Cursor::new(Vec::new());
    pixels.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let a = Transfer::from_clipboard(&ClipboardPayload::Bitmap(bmp.into_inner())).unwrap();
    let b = Transfer::from_clipboard(&ClipboardPayload::Bitmap(png.into_inner())).unwrap();
    assert_eq!(a.fingerprint().unwrap(), b.fingerprint().unwrap());
}

fn temp_directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "crossdesk-image-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn completed_file_batches_preserve_previous_clipboard_files_and_duplicate_names() {
    let directory = temp_directory();
    let a = directory.join("a");
    let b = directory.join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let (bytes, _) = png(3, 3);
    let first = a.join("same.png");
    let second = b.join("same.png");
    std::fs::write(&first, &bytes).unwrap();
    std::fs::write(&second, &bytes).unwrap();
    let transfer =
        Transfer::from_clipboard(&ClipboardPayload::Files(vec![first.clone(), second])).unwrap();
    let cache = directory.join("cache");
    let ClipboardPayload::Files(old) = transfer.into_payload(&cache).unwrap() else {
        panic!("not file clipboard");
    };
    assert_eq!(old[0].file_name().unwrap(), "same.png");
    assert_eq!(old[1].file_name().unwrap(), "same (2).png");
    let transfer = Transfer::from_clipboard(&ClipboardPayload::Files(vec![first])).unwrap();
    let ClipboardPayload::Files(new) = transfer.into_payload(&cache).unwrap() else {
        panic!("not file clipboard");
    };
    assert_ne!(new[0], old[0]);
    assert_eq!(std::fs::read(&old[0]).unwrap(), bytes);
    assert_eq!(std::fs::read(&new[0]).unwrap(), bytes);
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn filesystem_equivalent_names_cannot_silently_overwrite_a_received_image() {
    let directory = temp_directory();
    let transfer = Transfer {
        id: [99; 16],
        kind: Kind::Files,
        images: vec![
            ImageData {
                name: "é.png".into(),
                data: png(2, 2).0,
            },
            ImageData {
                name: "e\u{301}.png".into(),
                data: png(3, 3).0,
            },
        ],
    };
    let cache = directory.join("cache");
    assert!(
        transfer.into_payload(&cache).is_err(),
        "colliding file names overwrote an image"
    );
    assert_eq!(
        std::fs::read_dir(&cache).unwrap().count(),
        0,
        "incomplete cache was retained"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn declared_image_and_batch_limits_are_checked_before_allocating_contents() {
    fn manifest(count: usize, size: u64) -> Frame {
        let mut bytes = vec![0; 16];
        bytes.push(2);
        bytes.extend_from_slice(&(count as u16).to_be_bytes());
        for index in 0..count {
            let name = format!("image{index}.png");
            bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&[0; 32]);
        }
        Frame::new(FrameKind::ImageStart, bytes)
    }
    let mut receiver = Incoming::default();
    assert!(receiver.receive(manifest(33, 1)).is_err());
    assert!(receiver.receive(manifest(1, 32 * 1024 * 1024 + 1)).is_err());
    assert!(receiver.receive(manifest(5, 32 * 1024 * 1024)).is_err());
    assert!(!receiver.active());
    assert!(receiver.receive(manifest(1, 0)).is_err());
}

#[test]
fn encrypted_tcp_transfers_both_directions_with_input_and_heartbeat_between_chunks() {
    use crate::protocol::{self, InputEvent};
    use crate::secure::SecureStream;
    use crate::transport::SharedWriter;
    use std::net::TcpListener;
    let (server_png, server_pixels) = png(256, 128);
    let (client_png, client_pixels) = png(512, 256);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    fn exchange(mut stream: SecureStream, bytes: Vec<u8>, expected: Vec<u8>) {
        let writer = SharedWriter::new(stream.try_clone().unwrap());
        let sender = std::thread::spawn(move || {
            let transfer = Transfer::from_clipboard(&ClipboardPayload::Bitmap(bytes)).unwrap();
            let mut injected = false;
            transfer
                .visit_frames(|frame| {
                    let chunk = frame.kind == FrameKind::ImageChunk;
                    writer.write(frame)?;
                    if chunk && !injected {
                        writer.write(Frame::new(
                            FrameKind::Input,
                            protocol::encode_input(&InputEvent::MouseDelta { dx: 17, dy: -9 }),
                        ))?;
                        writer.write(Frame::new(FrameKind::Ping, Vec::new()))?;
                        injected = true;
                    }
                    Ok(())
                })
                .unwrap();
        });
        let mut receiver = Incoming::default();
        let (mut input, mut ping) = (false, false);
        loop {
            let frame = protocol::read_frame(&mut stream).unwrap();
            match frame.kind {
                FrameKind::Input => {
                    assert_eq!(
                        protocol::decode_input(&frame.payload).unwrap(),
                        InputEvent::MouseDelta { dx: 17, dy: -9 }
                    );
                    input = true;
                }
                FrameKind::Ping => ping = true,
                _ => {
                    if let Some(transfer) = receiver.receive(frame).unwrap() {
                        assert!(
                            input && ping,
                            "input and heartbeat were blocked behind the image"
                        );
                        assert_eq!(
                            image::load_from_memory(&transfer.images[0].data)
                                .unwrap()
                                .into_rgba8()
                                .into_raw(),
                            expected
                        );
                        break;
                    }
                }
            }
        }
        sender.join().unwrap();
    }
    let server = std::thread::spawn(move || {
        let stream = SecureStream::accept(listener.accept().unwrap().0, &[77; 32]).unwrap();
        exchange(stream, server_png, client_pixels);
    });
    exchange(
        SecureStream::connect(&address, &[77; 32]).unwrap(),
        client_png,
        server_pixels,
    );
    server.join().unwrap();
}
