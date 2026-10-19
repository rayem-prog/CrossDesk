use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

static REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}

pub fn install_signal_handler() -> std::io::Result<()> {
    ctrlc::set_handler(|| {
        REQUESTED.store(true, Ordering::Release);
        #[cfg(windows)]
        crate::server::request_stop();
    })
    .map_err(std::io::Error::other)
}

const GUI_CHILD_ENV: &str = "DESKBRIDGE_GUI_CHILD";

pub fn spawn_gui_stop_watcher(stop: impl FnOnce() + Send + 'static) {
    if std::env::var_os(GUI_CHILD_ENV).is_none() {
        return;
    }
    thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();
        if lines
            .next()
            .and_then(Result::ok)
            .is_some_and(|line| line.trim().eq_ignore_ascii_case("stop"))
        {
            stop();
        }
    });
}
