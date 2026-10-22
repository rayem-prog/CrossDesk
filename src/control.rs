use crate::service_event::{ServiceEvent, ServiceState};
use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::clipboard::{Clipboard, ClipboardApi};
use crate::config::{AppConfig, Role};
use crate::protocol::ClipboardPayload;
use crate::server::Edge;

const MAX_LOG_LINES: usize = 500;

/// Backend contract used by the GUI. The UI owns presentation only; process
/// lifecycle, command construction, logging, and clipboard publishing live here.
pub trait ControlBackend {
    fn start(&mut self, config: &AppConfig) -> std::io::Result<()>;
    fn stop(&mut self) -> std::io::Result<()>;
    fn is_running(&mut self) -> bool;
    fn collect_logs(&mut self);
    fn logs(&self) -> &VecDeque<String>;
    fn clear_logs(&mut self);
    fn push_log(&mut self, line: String);
    fn command_preview(&self, config: &AppConfig) -> String;
    fn publish_files(&mut self, files: &[PathBuf]) -> std::io::Result<()>;
}

pub struct ProcessBackend {
    child: Option<Child>,
    child_stdin: Option<ChildStdin>,
    receiver: Option<Receiver<String>>,
    logs: VecDeque<String>,
    status: ServiceEvent,
    latest: Arc<Mutex<Option<ServiceEvent>>>,
    readers: Vec<std::thread::JoinHandle<()>>,
    transfers: Arc<Mutex<crate::service_event::TransferTracker>>,
    capabilities: Arc<Mutex<Option<crate::service_event::ClipboardCapabilities>>>,
}

impl ControlBackend for ProcessBackend {
    fn is_running(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.push_log(format!("Service exited: {status}"));
                    self.child = None;
                    self.child_stdin = None;
                    *self.transfers.lock().unwrap() = Default::default();
                    self.capabilities.lock().unwrap().take();
                    self.status = ServiceEvent {
                        state: ServiceState::Error,
                        message: Some(format!("服务已退出: {status}")),
                    };
                    false
                }
                Ok(None) => true,
                Err(e) => {
                    self.push_log(format!("Service status failed: {e}"));
                    false
                }
            }
        } else {
            false
        }
    }

    fn start(&mut self, config: &AppConfig) -> std::io::Result<()> {
        if self.is_running() {
            return Ok(());
        }

        let launch = ServiceLaunch::from_config(config);
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(&launch.args)
            .envs(launch.env.iter().map(|(key, value)| (key, value)))
            .env("DESKBRIDGE_GUI_CHILD", "1")
            .env_remove("CROSSDESK_ENABLE_CLIPBOARD");
        self.spawn(command)
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let Some(mut child) = self.child.take() else {
            self.finish_stop();
            return Ok(());
        };
        self.status = ServiceEvent {
            state: ServiceState::Stopping,
            message: None,
        };
        if let Some(mut stdin) = self.child_stdin.take() {
            let _ = stdin.write_all(b"stop\n");
            let _ = stdin.flush();
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                self.finish_stop();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        child.kill()?;
        let _ = child.wait();
        self.push_log("服务未及时退出，已终止旧进程".into());
        self.finish_stop();
        Ok(())
    }

    fn command_preview(&self, config: &AppConfig) -> String {
        ServiceLaunch::from_config(config).preview()
    }

    fn collect_logs(&mut self) {
        let event = self.latest.lock().unwrap().take();
        if let Some(event) = event.filter(|_| self.child.is_some()) {
            self.status = event;
        }
        if let Some(receiver) = &self.receiver {
            for line in receiver.try_iter().collect::<Vec<_>>() {
                self.push_log(line);
            }
        }
    }

    fn logs(&self) -> &VecDeque<String> {
        &self.logs
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }

    fn clear_logs(&mut self) {
        self.logs.clear();
    }

    fn publish_files(&mut self, files: &[PathBuf]) -> std::io::Result<()> {
        let mut clipboard = Clipboard::new()?;
        clipboard.write(&ClipboardPayload::Files(files.to_vec()))
    }
}

impl Drop for ProcessBackend {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ServiceLaunch {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl ServiceLaunch {
    fn from_config(config: &AppConfig) -> Self {
        match config.role {
            Role::Server => Self {
                args: vec![
                    "server".into(),
                    "--bind".into(),
                    config.bind.clone(),
                    "--edge".into(),
                    edge_name(config.edge).into(),
                ],
                env: vec![
                    (
                        "DESKBRIDGE_MAC_COMMAND_MAPPING".into(),
                        config.mac_command_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_CONTROL_MAPPING".into(),
                        config.mac_control_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_OPTION_MAPPING".into(),
                        config.mac_option_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_SHIFT_MAPPING".into(),
                        config.mac_shift_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_CAPS_LOCK_MAPPING".into(),
                        config.mac_caps_lock_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_ESCAPE_MAPPING".into(),
                        config.mac_escape_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_BACKSPACE_MAPPING".into(),
                        config.mac_backspace_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_DELETE_MAPPING".into(),
                        config.mac_delete_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_ARROW_LEFT_MAPPING".into(),
                        config.mac_arrow_left_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_ARROW_RIGHT_MAPPING".into(),
                        config.mac_arrow_right_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_ARROW_UP_MAPPING".into(),
                        config.mac_arrow_up_mapping.as_str().into(),
                    ),
                    (
                        "DESKBRIDGE_MAC_ARROW_DOWN_MAPPING".into(),
                        config.mac_arrow_down_mapping.as_str().into(),
                    ),
                ],
            },
            Role::Client => Self {
                args: vec!["client".into(), "--server".into(), config.server.clone()],
                env: vec![
                    (
                        "DESKBRIDGE_SCROLL_SCALE".into(),
                        format!("{:.3}", config.scroll_scale),
                    ),
                    (
                        "DESKBRIDGE_SCROLL_RESPONSE".into(),
                        format!("{:.3}", config.scroll_response),
                    ),
                    (
                        "DESKBRIDGE_SCROLL_MAX_STEP".into(),
                        format!("{:.1}", config.scroll_max_step),
                    ),
                    (
                        "DESKBRIDGE_SCROLL_FRAME_MS".into(),
                        config.scroll_frame_ms.to_string(),
                    ),
                ],
            },
        }
    }

    fn preview(&self) -> String {
        format!("deskbridge {}", self.args.join(" "))
    }
}

fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "left",
        Edge::Right => "right",
        Edge::Both => "both",
    }
}

fn spawn_log_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    sender: SyncSender<String>,
    events: Option<Arc<Mutex<Option<ServiceEvent>>>>,
    transfers: Option<Arc<Mutex<crate::service_event::TransferTracker>>>,
    capabilities: Option<Arc<Mutex<Option<crate::service_event::ClipboardCapabilities>>>>,
) -> std::thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut bytes = Vec::new();
            let n = std::io::BufRead::read_until(
                &mut std::io::Read::take(&mut reader, 16385),
                b'\n',
                &mut bytes,
            );
            match n {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            let truncated = bytes.last() != Some(&b'\n');
            if truncated {
                loop {
                    let mut discard = Vec::new();
                    match std::io::BufRead::read_until(
                        &mut std::io::Read::take(&mut reader, 16385),
                        b'\n',
                        &mut discard,
                    ) {
                        Ok(0) | Err(_) => break,
                        _ => {
                            if discard.last() == Some(&b'\n') {
                                break;
                            }
                        }
                    }
                }
            }
            let line = String::from_utf8_lossy(&bytes).trim_end().to_string();
            if !truncated {
                if let (Some(slot), Some(event)) = (
                    &capabilities,
                    crate::service_event::parse_capabilities(&line),
                ) {
                    *slot.lock().unwrap() = Some(event);
                    continue;
                }
                if let (Some(slot), Some(event)) =
                    (&transfers, crate::service_event::parse_transfer(&line))
                {
                    slot.lock().unwrap().apply(event);
                    continue;
                }
                if let (Some(slot), Some(event)) = (&events, crate::service_event::parse(&line)) {
                    if matches!(
                        event.state,
                        ServiceState::Retrying
                            | ServiceState::Stopped
                            | ServiceState::Error
                            | ServiceState::Listening
                            | ServiceState::Connecting
                    ) {
                        if let Some(capabilities) = &capabilities {
                            capabilities.lock().unwrap().take();
                        }
                    }
                    *slot.lock().unwrap() = Some(event);
                    continue;
                }
            }
            let _ = sender.try_send(line);
        }
    })
}
impl Default for ProcessBackend {
    fn default() -> Self {
        Self {
            child: None,
            child_stdin: None,
            receiver: None,
            logs: VecDeque::new(),
            status: ServiceEvent {
                state: ServiceState::Stopped,
                message: None,
            },
            latest: Arc::new(Mutex::new(None)),
            readers: Vec::new(),
            transfers: Arc::new(Mutex::new(Default::default())),
            capabilities: Arc::new(Mutex::new(None)),
        }
    }
}
impl ProcessBackend {
    pub fn clipboard_capabilities(&self) -> Option<crate::service_event::ClipboardCapabilities> {
        if self.child.is_some() && self.status.state == ServiceState::Connected {
            *self.capabilities.lock().unwrap()
        } else {
            None
        }
    }
    pub fn transfer_status(&self) -> Option<crate::service_event::TransferEvent> {
        if self.child.is_some() {
            self.transfers.lock().unwrap().latest.clone()
        } else {
            None
        }
    }
    pub fn state(&self) -> ServiceState {
        self.status.state
    }
    pub fn error(&self) -> Option<String> {
        self.status.message.clone()
    }
    fn finish_stop(&mut self) {
        for t in self.readers.drain(..) {
            let _ = t.join();
        }
        self.latest.lock().unwrap().take();
        *self.transfers.lock().unwrap() = Default::default();
        self.capabilities.lock().unwrap().take();
        self.child_stdin = None;
        self.receiver = None;
        self.status = ServiceEvent {
            state: ServiceState::Stopped,
            message: None,
        };
    }
    fn spawn(&mut self, mut command: Command) -> std::io::Result<()> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let (tx, rx) = mpsc::sync_channel(512);
        self.latest.lock().unwrap().take();
        *self.transfers.lock().unwrap() = Default::default();
        self.capabilities.lock().unwrap().take();
        if let Some(out) = child.stdout.take() {
            self.readers.push(spawn_log_reader(
                out,
                tx.clone(),
                Some(self.latest.clone()),
                Some(self.transfers.clone()),
                Some(self.capabilities.clone()),
            ));
        }
        if let Some(err) = child.stderr.take() {
            self.readers
                .push(spawn_log_reader(err, tx, None, None, None));
        }
        self.child_stdin = child.stdin.take();
        self.receiver = Some(rx);
        self.child = Some(child);
        self.status = ServiceEvent {
            state: ServiceState::Starting,
            message: None,
        };
        Ok(())
    }
    pub fn start_link(
        &mut self,
        config: &AppConfig,
        key: &std::path::Path,
        image: bool,
        files: bool,
        text: bool,
    ) -> std::io::Result<()> {
        if self.is_running() {
            return Ok(());
        }
        self.finish_stop();
        crate::secure::load_key_from(key)?;
        let launch = ServiceLaunch::from_config(config);
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args(&launch.args)
            .envs(launch.env)
            .env("DESKBRIDGE_GUI_CHILD", "1")
            .env_remove("CROSSDESK_ENABLE_CLIPBOARD");
        cmd.arg("--key-file").arg(key).args([
            "--image-clipboard",
            if image { "on" } else { "off" },
            "--file-clipboard",
            if files { "on" } else { "off" },
            "--text-clipboard",
            if text { "on" } else { "off" },
        ]);
        self.spawn(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_launch_keeps_modifier_mapping_out_of_the_ui() {
        let mut config = AppConfig::default();
        config.role = Role::Server;
        config.edge = Edge::Left;
        let launch = ServiceLaunch::from_config(&config);

        assert_eq!(
            launch.args,
            ["server", "--bind", "0.0.0.0:24920", "--edge", "left"]
        );
        assert!(
            launch
                .env
                .iter()
                .any(|(key, _)| key == "DESKBRIDGE_MAC_COMMAND_MAPPING")
        );
    }

    #[test]
    fn client_launch_is_a_language_neutral_process_contract() {
        let config = AppConfig::default();
        let launch = ServiceLaunch::from_config(&config);

        assert_eq!(
            launch.preview(),
            "deskbridge client --server 192.168.1.10:24920"
        );
        assert!(
            launch
                .env
                .iter()
                .any(|(key, _)| key == "DESKBRIDGE_SCROLL_FRAME_MS")
        );
    }
}
#[cfg(all(test, feature = "gui"))]
#[path = "control_lifecycle_tests.rs"]
mod lifecycle_tests;
