// A plain data socket or deadline-bound, cancellable pairing socket.
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone)]
struct Cancellation {
    stop: Arc<AtomicBool>,
    deadline: Instant,
}
pub struct Socket {
    stream: TcpStream,
    cancellation: Option<Cancellation>,
}
impl Socket {
    pub fn plain(stream: TcpStream) -> io::Result<Self> {
        // Windows accept inherits the listener's nonblocking mode.
        stream.set_nonblocking(false)?;
        Ok(Self {
            stream,
            cancellation: None,
        })
    }
    pub fn pairing(stream: TcpStream, stop: Arc<AtomicBool>) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            cancellation: Some(Cancellation {
                stop,
                deadline: Instant::now() + Duration::from_secs(10),
            }),
        })
    }
    pub fn deadline(
        stream: TcpStream,
        stop: Arc<AtomicBool>,
        deadline: Instant,
    ) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            cancellation: Some(Cancellation { stop, deadline }),
        })
    }
    pub fn finish_deadline(&mut self) -> io::Result<()> {
        self.check()?;
        self.stream.set_nonblocking(false)?;
        self.cancellation = None;
        Ok(())
    }
    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.stream.peer_addr()
    }
    fn check(&self) -> io::Result<()> {
        if let Some(c) = &self.cancellation {
            if c.stop.load(Ordering::Acquire) {
                // read_exact/write_all retry Interrupted, so cancellation must be terminal.
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "pairing cancelled",
                ));
            }
            if Instant::now() >= c.deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "pairing timed out"));
            }
        }
        Ok(())
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            cancellation: self.cancellation.clone(),
        })
    }
    pub fn set_nodelay(&self, value: bool) -> io::Result<()> {
        self.stream.set_nodelay(value)
    }
    pub fn set_read_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(value)
    }
    pub fn set_write_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(value)
    }
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.stream.shutdown(how)
    }
}
impl Read for Socket {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.stream.read(output) {
                Err(e) if self.cancellation.is_some() && e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }
}
impl Write for Socket {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            match self.stream.write(input) {
                Err(e) if self.cancellation.is_some() && e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.stream.flush()
    }
}

#[cfg(all(test, feature = "pairing"))]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn cancellation_interrupts_a_noise_handshake_without_socket_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Socket::pairing(stream, stop.clone()).unwrap();
        let worker = thread::spawn(move || {
            crate::secure::SecureStream::handshake_socket(socket, &[8; 32], true)
        });
        // Observe the first Noise packet on the wire before cancelling its pending response.
        let mut header = [0; 2];
        peer.read_exact(&mut header).unwrap();
        let mut hello = vec![0; u16::from_be_bytes(header) as usize];
        peer.read_exact(&mut hello).unwrap();
        assert!(!hello.is_empty());
        stop.store(true, Ordering::Release);
        let start = Instant::now();
        match worker.join().unwrap() {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::ConnectionAborted),
            Ok(_) => panic!("idle unauthenticated socket completed a handshake"),
        }
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn partial_reads_preserve_bytes_and_an_idle_deadline_is_terminal() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut socket = Socket::pairing(stream, Arc::new(AtomicBool::new(false))).unwrap();
        peer.write_all(b"a").unwrap();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            peer.write_all(b"bc").unwrap();
            peer
        });
        let mut bytes = [0; 3];
        socket.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"abc");
        let _idle = sender.join().unwrap();
        socket.cancellation.as_mut().unwrap().deadline = Instant::now() + Duration::from_millis(30);
        assert_eq!(
            socket.read_exact(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
