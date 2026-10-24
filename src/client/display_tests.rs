//! Real Noise transport and production queue/driver; native input is only recorded.
use super::*;
use crate::display_sync::{AlignmentDriver, ScreenUpdate};
use crate::pointer::{MotionAction, PointerRouter};
use crate::protocol::KeyState;
use crate::server::Edge;
use std::net::{Shutdown, TcpListener};
use std::sync::atomic::AtomicU64;

fn dimensions(w: u32, h: u32) -> u64 {
    ((w as u64) << 32) | h as u64
}
fn key(scancode: u16) -> InputEvent {
    InputEvent::Key {
        scancode,
        state: KeyState::Down,
    }
}

struct Fixture {
    stream: SecureStream,
    writer: SharedWriter,
    size: Arc<AtomicU64>,
    applied: mpsc::Receiver<InputEvent>,
    client: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(supported: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let size = Arc::new(AtomicU64::new(dimensions(1920, 1080)));
        let native = size.clone();
        let (tx, applied) = mpsc::channel();
        let client = thread::spawn(move || {
            let mut stream = SecureStream::connect(&address, &[81; 32]).unwrap();
            let hello = protocol::decode_hello(&protocol::read_frame(&mut stream).unwrap().payload)
                .unwrap();
            let writer = SharedWriter::new(stream.try_clone().unwrap());
            writer
                .write(Frame::new(
                    FrameKind::Hello,
                    protocol::hello_payload_with_screen(1920, 1080),
                ))
                .unwrap();
            let queue = Arc::new(EventQueue::new(256));
            let target = queue.clone();
            let worker = thread::spawn(move || {
                let mut driver =
                    AlignmentDriver::new((1920, 1080), hello.screen_updates, Instant::now());
                drive_input_queue(&target, |event| {
                    driver.step(
                        event,
                        Instant::now(),
                        true,
                        || {
                            assert!(
                                hello.screen_updates,
                                "queried native geometry for legacy peer"
                            );
                            let size = native.load(Ordering::Acquire);
                            ((size >> 32) as u32, size as u32)
                        },
                        |event| tx.send(event).map_err(std::io::Error::other),
                        |u| writer.write(u.frame()),
                    )
                });
            });
            while let Ok(frame) = protocol::read_frame(&mut stream) {
                assert_eq!(frame.kind, FrameKind::Input);
                if queue
                    .push(protocol::decode_input(&frame.payload).unwrap())
                    .is_err()
                {
                    break;
                }
            }
            queue.close();
            let _ = stream.shutdown(Shutdown::Both);
            worker.join().unwrap();
        });
        let mut stream = SecureStream::accept(listener.accept().unwrap().0, &[81; 32]).unwrap();
        let writer = SharedWriter::new(stream.try_clone().unwrap());
        writer
            .write(Frame::new(
                FrameKind::Hello,
                vec![0, 7, 1, if supported { 16 } else { 0 }],
            ))
            .unwrap();
        let frame = protocol::read_frame(&mut stream).unwrap();
        assert_eq!(frame.kind, FrameKind::Hello);
        let hello = protocol::decode_hello(&frame.payload).unwrap();
        assert_eq!(
            (hello.screen_width, hello.screen_height),
            (Some(1920), Some(1080))
        );
        Self {
            stream,
            writer,
            size,
            applied,
            client: Some(client),
        }
    }
    fn send(&self, event: InputEvent) {
        self.writer
            .write_input(Frame::new(FrameKind::Input, protocol::encode_input(&event)))
            .unwrap();
    }
    fn expect(&self, event: InputEvent) {
        assert_eq!(
            self.applied.recv_timeout(Duration::from_secs(3)).unwrap(),
            event
        );
    }
    fn update(&mut self) -> ScreenUpdate {
        let frame = protocol::read_frame(&mut self.stream).unwrap();
        assert_eq!(frame.kind, FrameKind::ScreenUpdate);
        ScreenUpdate::decode(&frame.payload).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(client) = self.client.take() {
            client.join().unwrap();
        }
    }
}

fn resized_connection(rapid: bool) {
    let mut f = Fixture::new(true);
    let mut route = PointerRouter::new(Edge::Both, (1920, 1080), (1920, 1080));
    let MotionAction::EnterRemote { x, y } = route.observe_local_motion(1919, 540) else {
        panic!("no entry")
    };
    f.send(InputEvent::MouseEnter { x, y });
    f.send(key(30));
    f.expect(InputEvent::MouseEnter { x: 8, y: 540 });
    f.expect(key(30));
    f.size.store(dimensions(1512, 982), Ordering::Release);
    let first = f.update();
    assert_eq!(
        first,
        ScreenUpdate {
            revision: 1,
            width: 1512,
            height: 982
        }
    );
    f.expect(InputEvent::MouseLeave);
    f.send(key(31));
    f.send(InputEvent::MouseDelta {
        dx: 5000,
        dy: -5000,
    });
    let (latest, want_y) = if rapid {
        f.size.store(dimensions(1280, 720), Ordering::Release);
        let second = f.update();
        assert_eq!(
            second,
            ScreenUpdate {
                revision: 2,
                width: 1280,
                height: 720
            }
        );
        f.expect(InputEvent::MouseLeave);
        f.send(InputEvent::ScreenSync { revision: 1 });
        f.send(key(33));
        (second, 360)
    } else {
        (first, 491)
    };
    route.force_local();
    route.update_remote_size((latest.width as i32, latest.height as i32));
    // The production queue keeps this fence behind earlier key/motion/leave work.
    let ordered = EventQueue::new(16);
    ordered.push(InputEvent::MouseLeave).unwrap();
    ordered
        .push(InputEvent::ScreenSync {
            revision: latest.revision,
        })
        .unwrap();
    while let Some(item) = ordered.try_pop() {
        f.send(item.event);
    }
    let MotionAction::EnterRemote { x, y } = route.observe_local_motion(1919, 540) else {
        panic!("no resized entry")
    };
    assert_eq!((x, y), (8, want_y));
    f.send(InputEvent::MouseEnter { x, y });
    f.send(key(32));
    // Any stale key or motion would precede these expected messages and fail.
    f.expect(InputEvent::MouseEnter { x: 8, y: want_y });
    f.expect(key(32));
}

#[test]
fn encrypted_resize_releases_input_and_reenters_with_new_coordinates() {
    resized_connection(false);
}

#[test]
fn encrypted_rapid_resize_discards_an_old_fence_and_its_following_keys() {
    resized_connection(true);
}

#[test]
fn encrypted_legacy_peer_never_receives_display_frames() {
    let mut f = Fixture::new(false);
    f.send(InputEvent::MouseEnter { x: 8, y: 540 });
    f.expect(InputEvent::MouseEnter { x: 8, y: 540 });
    f.size.store(dimensions(1512, 982), Ordering::Release);
    f.send(key(30));
    f.expect(key(30));
    f.stream
        .set_read_timeout(Some(Duration::from_millis(600)))
        .unwrap();
    let error = protocol::read_frame(&mut f.stream).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
}
