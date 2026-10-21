use super::PairingWindow;
use crate::trust_store::TrustedStore;
use std::{
    io,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct PairingView {
    pub code: Option<String>,
    pub seconds_left: u16,
    pub completed: bool,
    pub error: Option<String>,
}
struct Active {
    socket: TcpStream,
    deadline: Instant,
}
pub struct PairingService {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    active: Arc<Mutex<Option<Active>>>,
    view: Arc<Mutex<PairingView>>,
    deadline: Instant,
    worker: Option<JoinHandle<()>>,
    watch: Option<JoinHandle<()>>,
}
impl PairingService {
    pub fn start(bind: SocketAddr, store: Arc<TrustedStore>) -> io::Result<Self> {
        if store.paired()?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a device is already paired",
            ));
        }
        let mut window = PairingWindow::random()?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let view = Arc::new(Mutex::new(PairingView {
            code: Some(String::from_utf8(window.code().to_vec()).unwrap()),
            seconds_left: 120,
            completed: false,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let active: Arc<Mutex<Option<Active>>> = Arc::new(Mutex::new(None));
        let (s, a, v) = (stop.clone(), active.clone(), view.clone());
        let worker = thread::Builder::new()
            .name("pair-host".into())
            .spawn(move || {
                while !s.load(Ordering::Acquire) && window.open(Instant::now()) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            match socket.try_clone() {
                                Ok(clone) => {
                                    let mut active = a.lock().unwrap();
                                    // stop() uses this same lock: a socket accepted during
                                    // cancellation must never enter a blocking handshake.
                                    if s.load(Ordering::Acquire) {
                                        break;
                                    }
                                    *active = Some(Active {
                                        socket: clone,
                                        deadline: Instant::now() + Duration::from_secs(10),
                                    })
                                }
                                Err(_) => continue,
                            }
                            let result = super::transport::run_host_cancellable(
                                socket,
                                &mut window,
                                &store,
                                s.clone(),
                            );
                            a.lock().unwrap().take();
                            let mut view = v.lock().unwrap();
                            if result.is_ok() {
                                view.completed = true;
                                view.error = None;
                            } else {
                                view.error = Some("配对未完成，请检查配对码后重试".into());
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(30))
                        }
                        Err(_) => break,
                    }
                }
                window.cancel();
                let mut view = v.lock().unwrap();
                view.code = None;
                view.seconds_left = 0;
                if !view.completed && !s.load(Ordering::Acquire) {
                    view.error = Some("配对码已失效，请重新生成".into());
                }
                s.store(true, Ordering::Release);
            })?;
        let (s, a) = (stop.clone(), active.clone());
        let watch_result = thread::Builder::new()
            .name("pair-deadline".into())
            .spawn(move || {
                while !s.load(Ordering::Acquire) {
                    let now = Instant::now();
                    if now >= deadline {
                        s.store(true, Ordering::Release);
                    }
                    if let Some(active) = a.lock().unwrap().as_ref() {
                        if now >= active.deadline || now >= deadline {
                            let _ = active.socket.shutdown(Shutdown::Both);
                        }
                    }
                    thread::sleep(Duration::from_millis(30));
                }
            });
        let watch = match watch_result {
            Ok(watch) => watch,
            Err(error) => {
                stop.store(true, Ordering::Release);
                if let Some(active) = active.lock().unwrap().as_ref() {
                    let _ = active.socket.shutdown(Shutdown::Both);
                }
                let _ = worker.join();
                return Err(error);
            }
        };
        Ok(Self {
            address,
            stop,
            active,
            view,
            deadline,
            worker: Some(worker),
            watch: Some(watch),
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub fn view(&self) -> PairingView {
        let mut v = self.view.lock().unwrap().clone();
        v.seconds_left = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs() as u16;
        if v.seconds_left == 0 || self.stop.load(Ordering::Acquire) {
            v.code = None;
        }
        v
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            let _ = a.socket.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.worker.take() {
            t.join()
                .map_err(|_| io::Error::other("pairing worker failed"))?;
        }
        if let Some(t) = self.watch.take() {
            t.join()
                .map_err(|_| io::Error::other("pairing timer failed"))?;
        }
        Ok(())
    }
}
impl Drop for PairingService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
