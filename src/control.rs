use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
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

#[derive(Default)]
pub struct ProcessBackend {
    child: Option<Child>,
    child_stdin: Option<ChildStdin>,
    receiver: Option<Receiver<String>>,
    logs: VecDeque<String>,
}

impl ControlBackend for ProcessBackend {
    fn is_running(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.push_log(format!("Service exited: {status}"));
                    self.child = None;
                    self.child_stdin = None;
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
            .env("DESKBRIDGE_GUI_CHILD", "1");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let (sender, receiver) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(stdout, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(stderr, sender);
        }
        self.child_stdin = child.stdin.take();
        self.receiver = Some(receiver);
        self.child = Some(child);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        if let Some(mut stdin) = self.child_stdin.take() {
            let _ = stdin.write_all(b"stop\n");
            let _ = stdin.flush();
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        child.kill()?;
        let _ = child.wait();
        Ok(())
    }

    fn command_preview(&self, config: &AppConfig) -> String {
        ServiceLaunch::from_config(config).preview()
    }

    fn collect_logs(&mut self) {
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
    }
}

fn spawn_log_reader<R>(reader: R, sender: mpsc::Sender<String>)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_launch_keeps_modifier_mapping_out_of_the_ui() {
        let config = AppConfig {
            role: Role::Server,
            edge: Edge::Left,
            ..AppConfig::default()
        };
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
