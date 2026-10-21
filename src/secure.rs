//! Noise NNpsk0 protects every protocol byte. The 256-bit pairing key is
//! generated locally and copied to the second machine; it is never logged.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_RECORD: usize = 65535;
const MAX_PLAINTEXT: usize = MAX_RECORD - 16;
const PROLOGUE: &[u8] = b"CrossDesk input prototype v1";
pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(5);

pub fn key_path() -> io::Result<PathBuf> {
    Ok(std::env::var_os("CROSSDESK_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("crossdesk.key")))
}

pub fn generate_key(path: &Path) -> io::Result<()> {
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key)
        .map_err(|e| io::Error::other(format!("random key generation failed: {e}")))?;
    let text: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    writeln!(file, "{text}")
}

pub fn load_key() -> io::Result<[u8; 32]> {
    load_key_from(&key_path()?)
}

pub fn load_key_from(path: &Path) -> io::Result<[u8; 32]> {
    let text = fs::read_to_string(path).map_err(|e| io::Error::new(e.kind(), format!("Cannot read pairing key {}. Run crossdesk keygen and copy the same key file to the Mac. ({e})", path.display())))?;
    decode_key(text.trim())
}

fn decode_key(text: &str) -> io::Result<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pairing key must contain exactly 64 hexadecimal characters",
        ));
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid pairing key"))?;
    }
    Ok(key)
}

pub struct SecureStream {
    reader: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    cipher: Arc<Mutex<snow::TransportState>>,
    buffered: Vec<u8>,
    position: usize,
}

pub fn from_tcp(stream: TcpStream, key: &[u8; 32], initiator: bool) -> io::Result<SecureStream> {
    SecureStream::handshake(stream, key, initiator)
}

impl SecureStream {
    pub fn connect(address: &str, key: &[u8; 32]) -> io::Result<Self> {
        let mut last = io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "address has no resolved endpoints",
        );
        for endpoint in address.to_socket_addrs()? {
            match TcpStream::connect_timeout(&endpoint, Duration::from_secs(4)) {
                Ok(stream) => return Self::handshake(stream, key, true),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    pub fn accept(stream: TcpStream, key: &[u8; 32]) -> io::Result<Self> {
        Self::handshake(stream, key, false)
    }

    fn handshake(mut stream: TcpStream, key: &[u8; 32], initiator: bool) -> io::Result<Self> {
        // Accepted sockets inherit a nonblocking Windows listener's mode.
        // Record framing uses read_exact/write_all with bounded timeouts.
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(LIVENESS_TIMEOUT))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let parameters = "Noise_NNpsk0_25519_ChaChaPoly_SHA256"
            .parse()
            .map_err(noise_error)?;
        let builder = snow::Builder::new(parameters)
            .prologue(PROLOGUE)
            .map_err(noise_error)?
            .psk(0, key)
            .map_err(noise_error)?;
        let mut handshake = if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(noise_error)?;
        let mut buffer = vec![0u8; MAX_RECORD];
        if initiator {
            let count = handshake
                .write_message(&[], &mut buffer)
                .map_err(noise_error)?;
            write_packet(&mut stream, &buffer[..count])?;
            let packet = read_packet(&mut stream)?;
            handshake
                .read_message(&packet, &mut buffer)
                .map_err(noise_error)?;
        } else {
            let packet = read_packet(&mut stream)?;
            handshake
                .read_message(&packet, &mut buffer)
                .map_err(noise_error)?;
            let count = handshake
                .write_message(&[], &mut buffer)
                .map_err(noise_error)?;
            write_packet(&mut stream, &buffer[..count])?;
        }
        let cipher = handshake.into_transport_mode().map_err(noise_error)?;
        Ok(Self {
            writer: Arc::new(Mutex::new(stream.try_clone()?)),
            reader: stream,
            cipher: Arc::new(Mutex::new(cipher)),
            buffered: Vec::new(),
            position: 0,
        })
    }

    // Clones may write concurrently, but exactly one handle owns reads. The
    // shared writer lock preserves both record order and Noise nonce order.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            reader: self.reader.try_clone()?,
            writer: Arc::clone(&self.writer),
            cipher: Arc::clone(&self.cipher),
            buffered: Vec::new(),
            position: 0,
        })
    }
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.reader.shutdown(how)
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.reader.set_read_timeout(timeout)
    }
    pub fn set_nodelay(&self, value: bool) -> io::Result<()> {
        self.reader.set_nodelay(value)
    }
}

impl Read for SecureStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.position == self.buffered.len() {
            let packet = read_packet(&mut self.reader)?;
            self.buffered.resize(MAX_RECORD, 0);
            let length = self
                .cipher
                .lock()
                .map_err(poisoned)?
                .read_message(&packet, &mut self.buffered)
                .map_err(noise_error)?;
            if length == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "empty encrypted record",
                ));
            }
            self.buffered.truncate(length);
            self.position = 0;
        }
        let count = output.len().min(self.buffered.len() - self.position);
        output[..count].copy_from_slice(&self.buffered[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl Write for SecureStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        let count = input.len().min(MAX_PLAINTEXT);
        let mut stream = self.writer.lock().map_err(poisoned)?;
        let mut packet = vec![0u8; count + 16];
        let size = self
            .cipher
            .lock()
            .map_err(poisoned)?
            .write_message(&input[..count], &mut packet)
            .map_err(noise_error)?;
        write_packet(&mut *stream, &packet[..size])?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.lock().map_err(poisoned)?.flush()
    }
}

fn write_packet(stream: &mut impl Write, packet: &[u8]) -> io::Result<()> {
    let length = u16::try_from(packet.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "encrypted record too large"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(packet)
}
fn read_packet(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header)?;
    let length = u16::from_be_bytes(header) as usize;
    if length < 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short encrypted record",
        ));
    }
    let mut packet = vec![0u8; length];
    stream.read_exact(&mut packet)?;
    Ok(packet)
}
fn noise_error(error: impl std::fmt::Debug) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "encrypted session rejected (check that both machines use the same pairing key): {error:?}"
        ),
    )
}
fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("encrypted session lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn accepted_nonblocking_socket_waits_for_handshake_and_delayed_hello() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let peer = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            // Windows accepts sockets with the listener's nonblocking mode.
            // Force it here too so this regression is covered on every OS.
            socket.set_nonblocking(true).unwrap();
            ready_tx.send(()).unwrap();
            let mut server = SecureStream::accept(socket, &[42; 32]).unwrap();
            server.write_all(b"server hello").unwrap();
            let mut hello = [0; 12];
            server.read_exact(&mut hello).unwrap();
            assert_eq!(&hello, b"client hello");
        });
        let socket = TcpStream::connect(address).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        std::thread::sleep(Duration::from_millis(75));
        let mut client = SecureStream::handshake(socket, &[42; 32], true).unwrap();
        let mut hello = [0; 12];
        client.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, b"server hello");
        std::thread::sleep(Duration::from_millis(75));
        client.write_all(b"client hello").unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn encrypted_bidirectional_stream_handles_partial_reads_and_large_payloads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let key = [42u8; 32];
        let payload: Vec<u8> = (0..200_123).map(|n| (n % 251) as u8).collect();
        let expected = payload.clone();
        let peer = std::thread::spawn(move || {
            let mut server = SecureStream::accept(listener.accept().unwrap().0, &key).unwrap();
            let mut data = vec![0; expected.len()];
            server.read_exact(&mut data).unwrap();
            assert_eq!(data, expected);
            server.try_clone().unwrap().write_all(b"ack").unwrap();
            server.write_all(b"next").unwrap();
        });
        let mut client = SecureStream::connect(&address, &key).unwrap();
        client.write_all(&payload).unwrap();
        let mut response = [0; 7];
        client.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"acknext");
        peer.join().unwrap();
    }

    #[test]
    fn wrong_pairing_key_is_rejected_before_input_can_be_sent() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let peer = std::thread::spawn(move || {
            assert!(SecureStream::accept(listener.accept().unwrap().0, &[1; 32]).is_err());
        });
        assert!(SecureStream::connect(&address, &[2; 32]).is_err());
        peer.join().unwrap();
    }

    #[test]
    fn tampered_transport_record_is_never_delivered_as_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let peer = std::thread::spawn(move || {
            let mut server = SecureStream::accept(listener.accept().unwrap().0, &[9; 32]).unwrap();
            let mut packet = vec![0; 64];
            let count = server
                .cipher
                .lock()
                .unwrap()
                .write_message(b"mouse", &mut packet)
                .unwrap();
            packet[count - 1] ^= 1;
            write_packet(&mut server.reader, &packet[..count]).unwrap();
        });
        let mut client = SecureStream::connect(&address, &[9; 32]).unwrap();
        assert_eq!(
            client.read(&mut [0u8; 5]).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        peer.join().unwrap();
    }

    #[test]
    fn malformed_key_and_short_record_are_rejected() {
        assert!(decode_key(&"0".repeat(63)).is_err());
        assert!(decode_key(&"z".repeat(64)).is_err());
        assert!(decode_key(&"界".repeat(22)).is_err());
        assert_eq!(decode_key(&"ab".repeat(32)).unwrap(), [171; 32]);
        assert!(read_packet(&mut &b"\0\0"[..]).is_err());
    }

    #[test]
    fn key_generation_never_overwrites_an_existing_pairing() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("crossdesk-key-{}-{stamp}.key", std::process::id()));
        generate_key(&path).unwrap();
        let original = fs::read_to_string(&path).unwrap();
        assert!(decode_key(original.trim()).is_ok());
        assert_eq!(
            generate_key(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
    }
}
