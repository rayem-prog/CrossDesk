use super::*;
use std::net::{Shutdown, TcpStream};
pub struct PairJob {
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    rx: std::sync::mpsc::Receiver<io::Result<()>>,
    worker: Option<JoinHandle<()>>,
}
impl PairJob {
    pub fn start(
        candidate: Candidate,
        code: [u8; 6],
        store: Arc<TrustedStore>,
    ) -> io::Result<Self> {
        let port = candidate
            .pairing_port
            .ok_or_else(|| io::Error::other("请在 Windows 上重新生成配对码"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();
        let (s, a) = (stop.clone(), socket.clone());
        let worker = std::thread::Builder::new()
            .name("pair-client".into())
            .spawn(move || {
                let mut code = code;
                let result = (|| {
                    let mut last = io::Error::new(
                        io::ErrorKind::TimedOut,
                        "找不到 Windows，请确认两端在同一网络",
                    );
                    for mut address in candidate.endpoints {
                        if s.load(Ordering::Acquire) {
                            return Err(io::Error::new(io::ErrorKind::Interrupted, "已取消配对"));
                        }
                        address.set_port(port);
                        match TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
                            Ok(stream) => {
                                *a.lock().unwrap() = Some(stream.try_clone()?);
                                if s.load(Ordering::Acquire) {
                                    return Err(io::Error::new(
                                        io::ErrorKind::Interrupted,
                                        "已取消配对",
                                    ));
                                }
                                return crate::pairing::transport::run_client_cancellable(
                                    stream,
                                    candidate.id,
                                    &code,
                                    &store,
                                    s.clone(),
                                );
                            }
                            Err(e) => last = e,
                        }
                    }
                    Err(last)
                })();
                code.fill(0);
                a.lock().unwrap().take();
                let _ = tx.send(result);
            })?;
        Ok(Self {
            stop,
            socket,
            rx,
            worker: Some(worker),
        })
    }
    pub fn result(&self) -> Option<io::Result<()>> {
        self.rx.try_recv().ok()
    }
    pub fn cancel(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(socket) = self.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.worker.take() {
            let _ = t.join();
        }
    }
}
impl Drop for PairJob {
    fn drop(&mut self) {
        self.cancel();
    }
}
