use super::*;
pub struct Controller {
    tx: SyncSender<Command>,
    view: Arc<Mutex<ConnectView>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Controller {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        let store = Arc::new(TrustedStore::new(root)?);
        let core = Core::from_parts(
            crate::platform::Platform::local().as_str(),
            store,
            Box::new(ProcessBackend::default()),
        )?;
        let (tx, rx) = mpsc::sync_channel(16);
        let view = Arc::new(Mutex::new(core.view()));
        let snapshot = view.clone();
        let worker = thread::Builder::new()
            .name("easy-connect".into())
            .spawn(move || {
                let mut core = core;
                core.discover();
                if core.platform == "windows" {
                    if let Err(e) = core.start_pairing() {
                        core.error = Some(e.to_string());
                    }
                }
                loop {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(command) => match core.command(command) {
                            Ok(false) => break,
                            Err(e) => core.error = Some(e.to_string()),
                            _ => {}
                        },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if let Err(e) = core.tick() {
                        core.error = Some(e.to_string());
                        core.policy.manual_stop();
                    }
                    *snapshot.lock().unwrap() = core.view();
                }
                core.shutdown();
                let mut last = core.view();
                last.state = "paused".into();
                *snapshot.lock().unwrap() = last;
            })?;
        Ok(Self {
            tx,
            view,
            worker: Mutex::new(Some(worker)),
        })
    }
    pub fn view(&self) -> ConnectView {
        self.view.lock().unwrap().clone()
    }
    pub fn submit(&self, command: Command) -> io::Result<()> {
        self.tx
            .try_send(command)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "操作正在处理中，请稍候"))
    }
    pub fn shutdown(&self) {
        let mut worker = self.worker.lock().unwrap();
        if let Some(t) = worker.take() {
            let _ = self.tx.send(Command::Shutdown);
            let _ = t.join();
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.shutdown();
    }
}
