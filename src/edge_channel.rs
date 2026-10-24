//! Independent authenticated edge-drop sockets; primary input never carries file bytes.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_is_one_use_and_an_old_session_cannot_attach() {
        let mut tickets = Tickets::default();
        tickets.begin(1);
        let first = tickets.issue(1).unwrap();
        let mut wrong = first;
        wrong[17] ^= 1;
        assert!(!tickets.consume(&wrong));
        assert!(tickets.consume(&first));
        assert!(!tickets.consume(&first));
        let stale = tickets.issue(1).unwrap();
        tickets.begin(2);
        assert!(!tickets.consume(&stale));
        let fresh = tickets.issue(2).unwrap();
        assert!(tickets.consume(&fresh));
        assert!(tickets.issue(1).is_err());
    }
}

use crate::{
    protocol::{self, Frame, FrameKind},
    secure::SecureStream,
    transport::SharedWriter,
};
use std::{
    io,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Tickets {
    generation: u64,
    token: Option<[u8; 32]>,
}
impl Tickets {
    fn begin(&mut self, generation: u64) {
        self.generation = generation;
        self.token = None;
    }
    fn issue(&mut self, generation: u64) -> io::Result<[u8; 32]> {
        if self.generation != generation {
            return Err(rejected());
        }
        let mut token = [0; 32];
        getrandom::getrandom(&mut token)
            .map_err(|_| io::Error::other("ticket entropy unavailable"))?;
        self.token = Some(token);
        Ok(token)
    }
    fn consume(&mut self, value: &[u8]) -> bool {
        let Some(token) = self.token else {
            return false;
        };
        if value.len() != 32 {
            return false;
        }
        use subtle::ConstantTimeEq;
        if !bool::from(token.ct_eq(value)) {
            return false;
        }
        self.token = None;
        true
    }
}
fn rejected() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "edge lane attachment rejected",
    )
}

pub fn supported() -> bool {
    cfg!(feature = "gui") && std::env::var_os("DESKBRIDGE_GUI_CHILD").is_some()
}

fn take_finished_workers(workers: &mut Vec<thread::JoinHandle<()>>) -> Vec<thread::JoinHandle<()>> {
    let mut finished = Vec::new();
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            finished.push(workers.swap_remove(index));
        } else {
            index += 1;
        }
    }
    finished
}

struct Session {
    id: u64,
    writer: SharedWriter,
    supported: bool,
    stop: Arc<AtomicBool>,
    lane: Option<SecureStream>,
    workers: Vec<thread::JoinHandle<()>>,
}
#[derive(Default)]
struct HubState {
    session: Option<Session>,
    tickets: Tickets,
}
#[derive(Clone, Default)]
pub struct ServerHub {
    state: Arc<Mutex<HubState>>,
    next: Arc<AtomicU64>,
}
pub struct PrimaryGuard {
    hub: ServerHub,
    id: u64,
}
impl ServerHub {
    pub fn begin(&self, writer: SharedWriter, supported: bool) -> io::Result<PrimaryGuard> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("edge session lock"))?;
        if state.session.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "primary already connected",
            ));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        state.tickets.begin(id);
        state.session = Some(Session {
            id,
            writer,
            supported: supported && self::supported(),
            stop: Arc::new(AtomicBool::new(false)),
            lane: None,
            workers: Vec::new(),
        });
        Ok(PrimaryGuard {
            hub: self.clone(),
            id,
        })
    }
    fn busy(&self) -> bool {
        self.state
            .lock()
            .map(|s| s.session.is_some())
            .unwrap_or(true)
    }
    /// Signal primary loss immediately; its owner joins all workers after capture exits.
    pub fn invalidate(&self) {
        let lane = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.tickets.token = None;
            state.session.as_mut().and_then(|session| {
                session.supported = false;
                session.stop.store(true, Ordering::Release);
                session.lane.take()
            })
        };
        if let Some(stream) = lane {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    pub fn ticket(&self) -> io::Result<()> {
        let (writer, token) = {
            let mut state = self.state.lock().map_err(|_| rejected())?;
            let session = state.session.as_ref().ok_or_else(rejected)?;
            if !session.supported || session.lane.is_some() {
                return Ok(());
            }
            let (id, writer) = (session.id, session.writer.clone());
            let token = state.tickets.issue(id)?;
            (writer, token)
        };
        writer.write(Frame::new(FrameKind::DropTicket, token.to_vec()))
    }
    fn attach(&self, mut stream: SecureStream, payload: &[u8]) -> io::Result<()> {
        let mut state = self.state.lock().map_err(|_| rejected())?;
        let session = state.session.as_ref().ok_or_else(rejected)?;
        if !session.supported || session.lane.is_some() {
            return Err(rejected());
        }
        if !state.tickets.consume(payload) {
            return Err(rejected());
        }
        let session = state.session.as_mut().unwrap();
        stream.finish_deadline()?;
        protocol::write_frame(&mut stream, &Frame::new(FrameKind::DropAttachAck, vec![]))?;
        let shutdown = stream.try_clone()?;
        session.lane = Some(shutdown);
        let stop = session.stop.clone();
        let hub = self.clone();
        let id = session.id;
        let finished = take_finished_workers(&mut session.workers);
        session.workers.push(thread::spawn(move || {
            let _registered = crate::shutdown::register_transport(&stream);
            if let Err(error) = crate::edge_drop::run_lane(stream, stop) {
                eprintln!("edge lane ended: {error}");
            }
            // A failed batch stays failed; only a new idle lane is negotiated.
            if let Ok(mut state) = hub.state.lock() {
                if state.session.as_ref().is_some_and(|s| s.id == id) {
                    state.session.as_mut().unwrap().lane = None;
                    state.tickets.token = None;
                }
            }
        }));
        drop(state);
        for worker in finished {
            let _ = worker.join();
        }
        Ok(())
    }
}
impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        let session = {
            let Ok(mut state) = self.hub.state.lock() else {
                return;
            };
            if !state.session.as_ref().is_some_and(|s| s.id == self.id) {
                return;
            }
            state.tickets.begin(0);
            state.session.take()
        };
        if let Some(mut session) = session {
            session.stop.store(true, Ordering::Release);
            if let Some(stream) = session.lane.take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
            for worker in session.workers {
                let _ = worker.join();
            }
        }
        crate::edge_drop::primary_disconnected();
    }
}

struct Reservation(Arc<AtomicBool>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
pub struct AcceptedPrimary {
    pub stream: SecureStream,
    pub address: SocketAddr,
    pub hello: protocol::Hello,
    _reservation: Reservation,
}
/// Owns only handshake workers; Windows capture stays on the calling thread.
pub struct Acceptor {
    receiver: mpsc::Receiver<AcceptedPrimary>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Acceptor {
    pub fn start(listener: TcpListener, key: [u8; 32], hub: ServerHub) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = stop.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let reserved = Arc::new(AtomicBool::new(false));
            let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();
            while !cancel.load(Ordering::Acquire) && !crate::shutdown::requested() {
                let mut index = 0;
                while index < workers.len() {
                    if workers[index].is_finished() {
                        let _ = workers.swap_remove(index).join();
                    } else {
                        index += 1;
                    }
                }
                match listener.accept() {
                    Ok((socket, address)) if workers.len() < 4 => {
                        let (tx, stop, hub, reserved) =
                            (tx.clone(), cancel.clone(), hub.clone(), reserved.clone());
                        workers.push(thread::spawn(move || {
                            let result = (|| -> io::Result<()> {
                                let _pending = crate::shutdown::register_socket(&socket)?;
                                let deadline = Instant::now() + Duration::from_secs(5);
                                let socket =
                                    crate::socket::Socket::deadline(socket, stop, deadline)?;
                                let mut stream =
                                    SecureStream::handshake_socket(socket, &key, false)?;
                                let _registered = crate::shutdown::register_transport(&stream)?;
                                protocol::write_frame(
                                    &mut stream,
                                    &Frame::new(FrameKind::Hello, protocol::hello_payload()),
                                )?;
                                let first = protocol::read_frame(&mut stream)?;
                                match first.kind {
                                    FrameKind::DropAttach => hub.attach(stream, &first.payload),
                                    FrameKind::Hello => {
                                        if hub.busy()
                                            || reserved
                                                .compare_exchange(
                                                    false,
                                                    true,
                                                    Ordering::AcqRel,
                                                    Ordering::Acquire,
                                                )
                                                .is_err()
                                        {
                                            return Err(io::Error::new(
                                                io::ErrorKind::AlreadyExists,
                                                "primary busy",
                                            ));
                                        }
                                        let reservation = Reservation(reserved);
                                        let hello = protocol::decode_hello(&first.payload)?;
                                        protocol::validate_version(hello)?;
                                        stream.finish_deadline()?;
                                        tx.try_send(AcceptedPrimary {
                                            stream,
                                            address,
                                            hello,
                                            _reservation: reservation,
                                        })
                                        .map_err(|_| io::Error::other("primary busy"))
                                    }
                                    _ => Err(rejected()),
                                }
                            })();
                            if let Err(error) = result {
                                eprintln!("Rejected connection from {address}: {error}");
                            }
                        }));
                    }
                    Ok((socket, _)) => {
                        let _ = socket.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
            cancel.store(true, Ordering::Release);
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            receiver,
            stop,
            worker: Some(worker),
        })
    }
    pub fn next(&self) -> io::Result<Option<AcceptedPrimary>> {
        match self.receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(peer) => Ok(Some(peer)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err(io::Error::other("listener stopped")),
        }
    }
}
impl Drop for Acceptor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Called only while idle: handshake and active batches never schedule ticket requests.
fn wait_for_ticket(
    receiver: &mpsc::Receiver<Vec<u8>>,
    writer: &SharedWriter,
    stop: &AtomicBool,
    initial_delay: Duration,
    maximum_delay: Duration,
) -> Option<Vec<u8>> {
    let mut delay = initial_delay;
    let mut next_request = Instant::now() + delay;
    while !stop.load(Ordering::Acquire) {
        let wait = next_request
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        match receiver.recv_timeout(wait) {
            Ok(token) => return (!stop.load(Ordering::Acquire)).then_some(token),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Acquire) {
                    return None;
                }
                if Instant::now() >= next_request {
                    if writer
                        .write(Frame::new(FrameKind::DropTicket, vec![]))
                        .is_err()
                    {
                        return None;
                    }
                    delay = delay.saturating_mul(2).min(maximum_delay);
                    next_request = Instant::now() + delay;
                }
            }
            Err(_) => return None,
        }
    }
    None
}

pub struct ClientLane {
    tickets: mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    active: Arc<Mutex<Option<SecureStream>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl ClientLane {
    pub fn start(endpoint: SocketAddr, key: [u8; 32], writer: SharedWriter) -> Self {
        let (tickets, receiver) = mpsc::sync_channel::<Vec<u8>>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(None));
        let (cancel, socket): (_, Arc<Mutex<Option<SecureStream>>>) =
            (stop.clone(), active.clone());
        let worker = thread::spawn(move || {
            let mut backoff = 200u64;
            while !cancel.load(Ordering::Acquire) {
                let Some(token) = wait_for_ticket(
                    &receiver,
                    &writer,
                    &cancel,
                    Duration::from_secs(1),
                    Duration::from_secs(2),
                ) else {
                    break;
                };
                let result = (|| -> io::Result<()> {
                    let tcp = TcpStream::connect_timeout(&endpoint, Duration::from_secs(2))?;
                    let _pending = crate::shutdown::register_socket(&tcp)?;
                    let raw = crate::socket::Socket::deadline(
                        tcp,
                        cancel.clone(),
                        Instant::now() + Duration::from_secs(5),
                    )?;
                    let mut stream = SecureStream::handshake_socket(raw, &key, true)?;
                    let _registered = crate::shutdown::register_transport(&stream)?;
                    *socket.lock().map_err(|_| rejected())? = Some(stream.try_clone()?);
                    let hello = protocol::read_frame(&mut stream)?;
                    if hello.kind != FrameKind::Hello {
                        return Err(rejected());
                    }
                    let hello = protocol::decode_hello(&hello.payload)?;
                    protocol::validate_version(hello)?;
                    if !hello.edge_drop {
                        return Err(rejected());
                    }
                    protocol::write_frame(&mut stream, &Frame::new(FrameKind::DropAttach, token))?;
                    let ack = protocol::read_frame(&mut stream)?;
                    if ack.kind != FrameKind::DropAttachAck || !ack.payload.is_empty() {
                        return Err(rejected());
                    }
                    stream.finish_deadline()?;
                    backoff = 200;
                    crate::edge_drop::run_lane(stream, cancel.clone())
                })();
                if let Ok(mut active) = socket.lock() {
                    if let Some(stream) = active.take() {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                }
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = result {
                    eprintln!("edge lane unavailable: {error}");
                }
                for _ in 0..backoff / 20 {
                    if cancel.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                backoff = (backoff * 2).min(2000);
                // Empty ticket is a request, never an offer or batch replay.
                if writer
                    .write(Frame::new(FrameKind::DropTicket, vec![]))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            tickets,
            stop,
            active,
            worker: Some(worker),
        }
    }
    pub fn receive(&self, payload: Vec<u8>) {
        if payload.len() == 32 {
            let _ = self.tickets.try_send(payload);
        }
    }
}
impl Drop for ClientLane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(active) = self.active.lock() {
            if let Some(stream) = active.as_ref() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use std::io::{Read, Write};
    fn pair() -> (SecureStream, SecureStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            SecureStream::accept(listener.accept().unwrap().0, &[71; 32]).unwrap()
        });
        let client = SecureStream::connect(&address.to_string(), &[71; 32]).unwrap();
        (client, worker.join().unwrap())
    }
    #[test]
    fn idle_ticket_wait_retries_when_primary_ignores_first_request() {
        let (mut client, mut server) = pair();
        let writer = SharedWriter::new(client.try_clone().unwrap());
        let (tickets, receiver) = mpsc::sync_channel(1);
        let dispatch = thread::spawn(move || {
            if let Ok(frame) = protocol::read_frame(&mut client) {
                assert_eq!(frame.kind, FrameKind::DropTicket);
                tickets.send(frame.payload).unwrap();
            }
        });
        let primary = thread::spawn(move || {
            server
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let first = protocol::read_frame(&mut server).unwrap();
            assert_eq!(first.kind, FrameKind::DropTicket);
            assert!(first.payload.is_empty());
            // Simulate old lane cleanup: first negotiation request has no response.
            let Ok(second) = protocol::read_frame(&mut server) else {
                return false;
            };
            assert_eq!(second.kind, FrameKind::DropTicket);
            assert!(second.payload.is_empty());
            protocol::write_frame(
                &mut server,
                &Frame::new(FrameKind::DropTicket, vec![19; 32]),
            )
            .unwrap();
            true
        });
        writer
            .write(Frame::new(FrameKind::DropTicket, vec![]))
            .unwrap();
        let token = wait_for_ticket(
            &receiver,
            &writer,
            &AtomicBool::new(false),
            Duration::from_millis(20),
            Duration::from_millis(40),
        );
        dispatch.join().unwrap();
        let repeated = primary.join().unwrap();
        assert_eq!(
            token,
            Some(vec![19; 32]),
            "ignored ticket request must not strand the idle lane"
        );
        assert!(repeated);
    }
    #[test]
    fn invalidating_primary_revokes_its_ticket_and_closes_lane_immediately() {
        let (primary, _peer) = pair();
        let (lane, mut lane_peer) = pair();
        let hub = ServerHub::default();
        let guard = hub.begin(SharedWriter::new(primary), false).unwrap();
        let token = {
            let mut state = hub.state.lock().unwrap();
            state.session.as_mut().unwrap().lane = Some(lane);
            state.tickets.issue(guard.id).unwrap()
        };
        hub.invalidate();
        let mut state = hub.state.lock().unwrap();
        assert!(!state.tickets.consume(&token));
        assert!(state.session.as_ref().unwrap().stop.load(Ordering::Acquire));
        drop(state);
        assert!(protocol::read_frame(&mut lane_peer).is_err());
        drop(guard);
    }
    #[test]
    fn repeated_reconnects_reap_completed_workers_but_preserve_the_live_worker() {
        let (stop, receiver) = mpsc::sync_channel(0);
        let live = thread::spawn(move || receiver.recv().unwrap());
        let mut workers = vec![live];
        for _ in 0..64 {
            let finished = thread::spawn(|| {});
            let deadline = Instant::now() + Duration::from_secs(2);
            while !finished.is_finished() {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            workers.push(finished);
            let retired = take_finished_workers(&mut workers);
            assert_eq!(
                retired.len(),
                1,
                "finished reconnect worker must be retired"
            );
            assert_eq!(workers.len(), 1, "only the live worker remains owned");
            for worker in retired {
                worker.join().unwrap();
            }
        }
        assert!(!workers[0].is_finished());
        stop.send(()).unwrap();
        workers.pop().unwrap().join().unwrap();
    }
    #[test]
    fn independent_lane_failure_leaves_primary_input_usable() {
        let (mut primary, mut peer) = pair();
        let (lane, lane_peer) = pair();
        lane.shutdown(Shutdown::Both).unwrap();
        drop(lane_peer);
        protocol::write_frame(&mut primary, &Frame::new(FrameKind::Input, vec![6])).unwrap();
        let input = protocol::read_frame(&mut peer).unwrap();
        assert_eq!(input.kind, FrameKind::Input);
        assert_eq!(input.payload, [6]);
    }
    #[test]
    fn blocked_drop_socket_does_not_stall_primary_input_writer() {
        let (mut primary, mut peer) = pair();
        let (mut lane, lane_peer) = pair();
        let shutdown = lane.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let bytes = vec![7; 65536];
            tx.send(()).unwrap();
            for _ in 0..2048 {
                if lane.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        rx.recv().unwrap();
        thread::sleep(Duration::from_millis(50));
        assert!(
            !worker.is_finished(),
            "lane must be blocked against a nonreading peer"
        );
        let started = Instant::now();
        primary.write_all(b"input").unwrap();
        let mut bytes = [0; 5];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"input");
        assert!(started.elapsed() < Duration::from_secs(1));
        shutdown.shutdown(Shutdown::Both).unwrap();
        drop(lane_peer);
        worker.join().unwrap();
    }
    #[test]
    fn four_partial_handshakes_are_bounded_and_cancel_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let acceptor = Acceptor::start(listener, [1; 32], ServerHub::default()).unwrap();
        let mut idle = Vec::new();
        for _ in 0..4 {
            let mut socket = TcpStream::connect(endpoint).unwrap();
            socket.write_all(&[0]).unwrap();
            idle.push(socket);
        }
        thread::sleep(Duration::from_millis(100));
        let mut excess = TcpStream::connect(endpoint).unwrap();
        excess
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        match excess.read(&mut [0]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) => {}
            result => panic!("fifth partial handshake was not rejected: {result:?}"),
        }
        let started = Instant::now();
        drop(acceptor);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn slow_partial_noise_handshake_has_an_absolute_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let stream = listener.accept().unwrap().0;
        let socket = crate::socket::Socket::deadline(
            stream,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + Duration::from_millis(80),
        )
        .unwrap();
        let worker = thread::spawn(move || SecureStream::handshake_socket(socket, &[1; 32], false));
        peer.write_all(&[0]).unwrap();
        thread::sleep(Duration::from_millis(40));
        peer.write_all(&[64, 7]).unwrap();
        let result = worker.join().unwrap();
        assert!(matches!(result, Err(ref e) if e.kind() == io::ErrorKind::TimedOut));
    }
    #[test]
    fn an_additional_primary_is_rejected_until_the_original_session_exits() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let acceptor = Acceptor::start(listener, [1; 32], ServerHub::default()).unwrap();
        let mut first = SecureStream::connect(&endpoint, &[1; 32]).unwrap();
        assert_eq!(
            protocol::read_frame(&mut first).unwrap().kind,
            FrameKind::Hello
        );
        protocol::write_frame(
            &mut first,
            &Frame::new(
                FrameKind::Hello,
                protocol::hello_payload_with_screen(100, 100),
            ),
        )
        .unwrap();
        let primary = loop {
            if let Some(primary) = acceptor.next().unwrap() {
                break primary;
            }
        };
        let mut second = SecureStream::connect(&endpoint, &[1; 32]).unwrap();
        assert_eq!(
            protocol::read_frame(&mut second).unwrap().kind,
            FrameKind::Hello
        );
        protocol::write_frame(
            &mut second,
            &Frame::new(
                FrameKind::Hello,
                protocol::hello_payload_with_screen(100, 100),
            ),
        )
        .unwrap();
        assert!(protocol::read_frame(&mut second).is_err());
        assert!(acceptor.next().unwrap().is_none());
        let mut primary_stream = primary.stream.try_clone().unwrap();
        protocol::write_frame(&mut first, &Frame::new(FrameKind::Input, vec![6])).unwrap();
        assert_eq!(
            protocol::read_frame(&mut primary_stream).unwrap().payload,
            [6]
        );
        drop(primary);
        drop(acceptor);
    }
}

#[cfg(test)]
mod comparison_tests;
