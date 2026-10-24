use std::{
    collections::HashMap,
    io::BufRead,
    net::Shutdown,
    sync::{Arc, LazyLock},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};
static REQUESTED: AtomicBool = AtomicBool::new(false);
enum Connection {
    Secure(crate::secure::SecureStream),
    Plain(std::net::TcpStream),
}
static ACTIVE: LazyLock<Mutex<HashMap<u64, Arc<Connection>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT: AtomicU64 = AtomicU64::new(1);
pub fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}
pub fn request_shutdown() {
    REQUESTED.store(true, Ordering::Release);
    let connections: Vec<_> = ACTIVE
        .lock()
        .map(|active| active.values().cloned().collect())
        .unwrap_or_default();
    for connection in connections {
        match connection.as_ref() {
            Connection::Secure(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            Connection::Plain(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
    #[cfg(windows)]
    crate::server::request_stop();
}
pub struct TransportStopGuard(u64);
pub fn register_transport(
    stream: &crate::secure::SecureStream,
) -> std::io::Result<TransportStopGuard> {
    register(Connection::Secure(stream.try_clone()?))
}
pub fn register_socket(stream: &std::net::TcpStream) -> std::io::Result<TransportStopGuard> {
    register(Connection::Plain(stream.try_clone()?))
}
fn register(connection: Connection) -> std::io::Result<TransportStopGuard> {
    let id = NEXT.fetch_add(1, Ordering::AcqRel);
    ACTIVE
        .lock()
        .map_err(|_| std::io::Error::other("shutdown lock poisoned"))?
        .insert(id, Arc::new(connection));
    if requested() {
        request_shutdown();
    }
    Ok(TransportStopGuard(id))
}
impl Drop for TransportStopGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            active.remove(&self.0);
        }
    }
}

pub fn install_signal_handler() -> std::io::Result<()> {
    ctrlc::set_handler(request_shutdown).map_err(std::io::Error::other)
}
#[derive(Debug, PartialEq, Eq)]
enum GuiCommand {
    Stop,
    DiagnosticsStart(u64),
    DiagnosticsStop,
    Ignore,
}
fn parse_gui_command(line: &str) -> GuiCommand {
    if line.len() > 128 {
        return GuiCommand::Ignore;
    }
    if line.trim().eq_ignore_ascii_case("stop") {
        return GuiCommand::Stop;
    }
    let fields: Vec<_> = line.split_whitespace().collect();
    match fields.as_slice() {
        ["diagnostics", "stop"] => GuiCommand::DiagnosticsStop,
        ["diagnostics", "start", id] if id.bytes().all(|c| c.is_ascii_digit()) => id
            .parse::<u64>()
            .ok()
            .filter(|&n| n != 0)
            .map(GuiCommand::DiagnosticsStart)
            .unwrap_or(GuiCommand::Ignore),
        _ => GuiCommand::Ignore,
    }
}
#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn commands_keep_shutdown_distinct_and_accept_only_valid_diagnostic_ids() {
        assert_eq!(parse_gui_command(" STOP "), GuiCommand::Stop);
        assert_eq!(
            parse_gui_command("diagnostics start 42"),
            GuiCommand::DiagnosticsStart(42)
        );
        assert_eq!(
            parse_gui_command("diagnostics stop"),
            GuiCommand::DiagnosticsStop
        );
        for s in [
            "diagnostics start 0",
            "diagnostics start -1",
            "diagnostics start 18446744073709551616",
            "diagnostics stop extra",
            "diagnostics start 1 extra",
            "unrecognized",
        ] {
            assert_eq!(parse_gui_command(s), GuiCommand::Ignore, "{s}");
        }
    }
}
pub fn spawn_gui_stop_watcher() {
    if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_none() {
        return;
    }
    thread::spawn(|| {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        loop {
            let line = match read_command_line(&mut input) {
                Ok(Some(line)) => line,
                _ => break,
            };
            #[cfg(feature = "gui")]
            if line.starts_with("crossdesk-drop-command\t") {
                if let Ok(Some(command)) =
                    crate::drop_ipc::parse_command(line.trim_end_matches(['\r', '\n']))
                {
                    if crate::edge_drop::command(command).is_err() {
                        eprintln!("edge drop command rejected");
                    }
                }
                continue;
            }
            match parse_gui_command(&line) {
                GuiCommand::Stop => break,
                GuiCommand::DiagnosticsStart(id) => {
                    if let Err(error) = crate::diagnostics::start(id) {
                        eprintln!("diagnostics: {error}");
                    }
                }
                GuiCommand::DiagnosticsStop => crate::diagnostics::stop(),
                GuiCommand::Ignore => {}
            }
        }
        request_shutdown();
    });
}

// Drain overlong records without allocating their contents or interpreting a suffix as a command.
fn read_command_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    const MAX: usize = 4 * 1024 * 1024 + 64;
    let mut output = Vec::new();
    let mut oversized = false;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(output)
                    .map(Some)
                    .map_err(|_| std::io::Error::other("invalid GUI command"))
            };
        }
        let count = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(bytes.len());
        let newline = bytes[count - 1] == b'\n';
        if output.len() + count <= MAX && !oversized {
            output.extend_from_slice(&bytes[..count]);
        } else {
            oversized = true;
            output.clear();
        }
        reader.consume(count);
        if newline {
            return if oversized {
                Ok(Some(String::new()))
            } else {
                String::from_utf8(output)
                    .map(Some)
                    .map_err(|_| std::io::Error::other("invalid GUI command"))
            };
        }
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn registering_a_second_socket_does_not_replace_the_first_and_guard_removes_only_its_own() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut first = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let first_server = listener.accept().unwrap().0;
        let mut second = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let second_server = listener.accept().unwrap().0;
        let one = register_socket(&first_server).unwrap();
        let two = register_socket(&second_server).unwrap();
        assert!(ACTIVE.lock().unwrap().contains_key(&one.0));
        drop(one);
        let retained = ACTIVE.lock().unwrap().get(&two.0).cloned().unwrap();
        if let Connection::Plain(stream) = retained.as_ref() {
            stream.shutdown(Shutdown::Both).unwrap();
        }
        second
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        assert_eq!(second.read(&mut [0]).unwrap(), 0);
        first.write_all(b"live").unwrap();
        let mut server = first_server;
        let mut bytes = [0; 4];
        server.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"live");
    }
    #[test]
    fn oversized_command_is_drained_and_cannot_smuggle_a_stop_suffix() {
        let mut bytes = vec![b'x'; 4 * 1024 * 1024 + 128];
        bytes.extend_from_slice(b"stop\nstop\n");
        let mut input = std::io::Cursor::new(bytes);
        assert_eq!(read_command_line(&mut input).unwrap(), Some(String::new()));
        assert_eq!(
            read_command_line(&mut input).unwrap(),
            Some("stop\n".into())
        );
        assert_eq!(read_command_line(&mut input).unwrap(), None);
    }
}
