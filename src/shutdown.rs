use std::{
    io::BufRead,
    net::Shutdown,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};
static REQUESTED: AtomicBool = AtomicBool::new(false);
static ACTIVE: Mutex<Option<(u64, crate::secure::SecureStream)>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);
pub fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}
pub fn request_shutdown() {
    REQUESTED.store(true, Ordering::Release);
    if let Ok(active) = ACTIVE.lock() {
        if let Some((_, stream)) = active.as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    #[cfg(windows)]
    crate::server::request_stop();
}
pub struct TransportStopGuard(u64);
pub fn register_transport(
    stream: &crate::secure::SecureStream,
) -> std::io::Result<TransportStopGuard> {
    let id = NEXT.fetch_add(1, Ordering::AcqRel);
    let cloned = stream.try_clone()?;
    *ACTIVE
        .lock()
        .map_err(|_| std::io::Error::other("shutdown lock poisoned"))? = Some((id, cloned));
    if requested() {
        request_shutdown();
    }
    Ok(TransportStopGuard(id))
}
impl Drop for TransportStopGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            if active.as_ref().is_some_and(|(id, _)| *id == self.0) {
                active.take();
            }
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
        for line in stdin.lock().lines() {
            match line.map(|s| parse_gui_command(&s)) {
                Ok(GuiCommand::Stop) | Err(_) => break,
                Ok(GuiCommand::DiagnosticsStart(id)) => {
                    if let Err(error) = crate::diagnostics::start(id) {
                        eprintln!("diagnostics: {error}");
                    }
                }
                Ok(GuiCommand::DiagnosticsStop) => crate::diagnostics::stop(),
                Ok(GuiCommand::Ignore) => {}
            }
        }
        request_shutdown();
    });
}
