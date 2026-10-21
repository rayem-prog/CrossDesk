// Initial pairing is separate from the unchanged v6 data channel.
use super::PairingWindow;
use crate::trust_store::{DeviceId, TrustedStore};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
const MAGIC: &[u8; 8] = b"CDPAIR01";
fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "pairing failed: check the code and selected device",
    )
}
pub fn read_message(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut h = [0; 2];
    reader.read_exact(&mut h)?;
    let n = u16::from_be_bytes(h) as usize;
    if n == 0 || n > 4096 {
        return Err(bad());
    }
    let mut b = vec![0; n];
    reader.read_exact(&mut b)?;
    Ok(b)
}
fn write_message(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > 4096 {
        return Err(bad());
    }
    writer.write_all(&(bytes.len() as u16).to_be_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()
}
fn configure(s: &TcpStream) -> io::Result<()> {
    s.set_nonblocking(false)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))
}
fn identities(host: &DeviceId, client: &DeviceId, nonce: &[u8]) -> (Identity, Identity) {
    let mut a = b"CrossDesk/pair/1/client".to_vec();
    a.extend_from_slice(client);
    a.extend_from_slice(host);
    a.extend_from_slice(nonce);
    let mut b = b"CrossDesk/pair/1/host".to_vec();
    b.extend_from_slice(host);
    b.extend_from_slice(client);
    b.extend_from_slice(nonce);
    (Identity::new(&a), Identity::new(&b))
}
fn bootstrap(
    shared: &[u8],
    hello: &[u8],
    client: &DeviceId,
    a: &[u8],
    b: &[u8],
) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(MAGIC);
    hash.update(hello);
    hash.update(client);
    hash.update(a);
    hash.update(b);
    let salt = hash.finalize();
    let mut key = [0; 32];
    hkdf::Hkdf::<Sha256>::new(Some(&salt), shared)
        .expand(b"CrossDesk/pair/1/Noise bootstrap", &mut key)
        .map_err(|_| bad())?;
    Ok(key)
}
pub fn run_host(
    mut socket: TcpStream,
    window: &mut PairingWindow,
    store: &TrustedStore,
) -> io::Result<()> {
    if !window.admit(Instant::now()) {
        return Err(bad());
    }
    let result = (|| {
        configure(&socket)?;
        let start = Instant::now();
        let mut magic = [0; 8];
        socket.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(bad());
        }
        let host = store.local_id()?;
        let mut nonce = [0; 16];
        getrandom::getrandom(&mut nonce).map_err(|_| bad())?;
        let mut hello = vec![1];
        hello.extend_from_slice(&host);
        hello.extend_from_slice(&nonce);
        write_message(&mut socket, &hello)?;
        let client: DeviceId = read_message(&mut socket)?.try_into().map_err(|_| bad())?;
        let a = read_message(&mut socket)?;
        if a.len() != 33 {
            return Err(bad());
        }
        let (ia, ib) = identities(&host, &client, &nonce);
        let (state, b) = Spake2::<Ed25519Group>::start_b(&Password::new(&window.code()), &ia, &ib);
        let mut shared = state.finish(&a).map_err(|_| bad())?;
        write_message(&mut socket, &b)?;
        let mut key = bootstrap(&shared, &hello, &client, &a, &b)?;
        shared.fill(0);
        let channel = crate::secure::from_tcp(socket, &key, false);
        key.fill(0);
        let mut channel = channel.map_err(|_| bad())?;
        if !window.open(Instant::now()) || start.elapsed() > Duration::from_secs(10) {
            return Err(bad());
        }
        let mut long_term = store.host_key()?;
        let mut grant = vec![1];
        grant.extend_from_slice(&host);
        grant.extend_from_slice(&client);
        grant.extend_from_slice(&long_term);
        write_message(&mut channel, &grant)?;
        grant.fill(0);
        let mut wanted = vec![2];
        wanted.extend_from_slice(&client);
        if read_message(&mut channel)? != wanted {
            long_term.fill(0);
            return Err(bad());
        }
        if !window.open(Instant::now()) {
            long_term.fill(0);
            return Err(bad());
        }
        store.stage(client, &long_term)?;
        long_term.fill(0);
        store.commit(client)?;
        let mut complete = vec![3];
        complete.extend_from_slice(&host);
        write_message(&mut channel, &complete)
    })();
    window.finish(result.is_ok());
    result
}
pub fn run_client(
    mut socket: TcpStream,
    expected_host: DeviceId,
    code: &[u8; 6],
    store: &TrustedStore,
) -> io::Result<()> {
    if !code.iter().all(u8::is_ascii_digit) {
        return Err(bad());
    }
    configure(&socket)?;
    socket.write_all(MAGIC)?;
    let hello = read_message(&mut socket)?;
    if hello.len() != 33 || hello[0] != 1 || hello[1..17] != expected_host {
        return Err(bad());
    }
    let client = store.local_id()?;
    write_message(&mut socket, &client)?;
    let (ia, ib) = identities(&expected_host, &client, &hello[17..]);
    let (state, a) = Spake2::<Ed25519Group>::start_a(&Password::new(code), &ia, &ib);
    write_message(&mut socket, &a)?;
    let b = read_message(&mut socket)?;
    if b.len() != 33 {
        return Err(bad());
    }
    let mut shared = state.finish(&b).map_err(|_| bad())?;
    let mut key = bootstrap(&shared, &hello, &client, &a, &b)?;
    shared.fill(0);
    let channel = crate::secure::from_tcp(socket, &key, true);
    key.fill(0);
    let mut channel = channel.map_err(|_| bad())?;
    let mut grant = read_message(&mut channel)?;
    if grant.len() != 65
        || grant[0] != 1
        || grant[1..17] != expected_host
        || grant[17..33] != client
    {
        return Err(bad());
    }
    let mut long_term: [u8; 32] = grant[33..].try_into().map_err(|_| bad())?;
    grant.fill(0);
    store.stage(expected_host, &long_term)?;
    long_term.fill(0);
    let mut ack = vec![2];
    ack.extend_from_slice(&client);
    write_message(&mut channel, &ack)?;
    let mut wanted = vec![3];
    wanted.extend_from_slice(&expected_host);
    if read_message(&mut channel)? != wanted {
        return Err(bad());
    }
    store.commit(expected_host)
}
#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
