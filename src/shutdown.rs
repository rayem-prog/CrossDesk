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
pub fn spawn_gui_stop_watcher() {
    if std::env::var_os("DESKBRIDGE_GUI_CHILD").is_none() {
        return;
    }
    thread::spawn(|| {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(s) if s.trim().eq_ignore_ascii_case("stop") => break,
                Err(_) => break,
                _ => continue,
            }
        }
        request_shutdown();
    });
}
