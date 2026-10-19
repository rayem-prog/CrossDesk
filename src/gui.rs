use std::process::Command;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::{self, AppConfig, KeyTarget, Language, ModifierTarget, Role};
use crate::control::{ControlBackend, ProcessBackend};
use crate::server::Edge;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASES_URL: &str = "https://github.com/paulhe666/deskbridge/releases";
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/paulhe666/deskbridge/releases/latest";

pub fn run() -> std::io::Result<()> {
    tauri::Builder::default()
        .manage(AppState {
            config: Mutex::new(AppConfig::load()),
            backend: Mutex::new(ProcessBackend::default()),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            save_config,
            start_service,
            stop_service,
            clear_logs,
            check_for_updates,
            open_release_page
        ])
        .run(tauri::generate_context!())
        .map_err(|error| std::io::Error::other(error.to_string()))
}

struct AppState {
    config: Mutex<AppConfig>,
    backend: Mutex<ProcessBackend>,
}

#[tauri::command]
fn get_state(state: tauri::State<'_, AppState>) -> Result<ApiState, String> {
    Ok(build_state(state.inner()))
}

#[tauri::command]
fn save_config(config: ConfigView, state: tauri::State<'_, AppState>) -> Result<ApiState, String> {
    {
        let mut current = state
            .config
            .lock()
            .map_err(|_| "config mutex poisoned".to_string())?;
        let path = config::set_config_path_override(&config.config_path)
            .map_err(|error| format!("failed to set config path: {error}"))?;
        config.apply_to(&mut current);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create config directory: {error}"))?;
        }
        current
            .save()
            .map_err(|error| format!("failed to save config: {error}"))?;
    }

    Ok(build_state(state.inner()))
}

#[tauri::command]
fn start_service(state: tauri::State<'_, AppState>) -> Result<ApiState, String> {
    let config = {
        let config = state
            .config
            .lock()
            .map_err(|_| "config mutex poisoned".to_string())?
            .clone();
        config
            .save()
            .map_err(|error| format!("failed to save config: {error}"))?;
        config
    };

    {
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "backend mutex poisoned".to_string())?;
        if let Err(error) = backend.start(&config) {
            backend.push_log(format!("Failed to start service: {error}"));
            return Err(format!("failed to start service: {error}"));
        }
    }

    Ok(build_state(state.inner()))
}

#[tauri::command]
fn stop_service(state: tauri::State<'_, AppState>) -> Result<ApiState, String> {
    {
        let mut backend = state
            .backend
            .lock()
            .map_err(|_| "backend mutex poisoned".to_string())?;
        if let Err(error) = backend.stop() {
            backend.push_log(format!("Failed to stop service: {error}"));
            return Err(format!("failed to stop service: {error}"));
        }
    }

    Ok(build_state(state.inner()))
}

#[tauri::command]
fn clear_logs(state: tauri::State<'_, AppState>) -> Result<ApiState, String> {
    state
        .backend
        .lock()
        .map_err(|_| "backend mutex poisoned".to_string())?
        .clear_logs();
    Ok(build_state(state.inner()))
}

#[tauri::command]
fn check_for_updates() -> Result<UpdateInfo, String> {
    fetch_update_info().map_err(|error| error.to_string())
}

#[tauri::command]
fn open_release_page() -> Result<(), String> {
    open_url(RELEASES_URL)
}

fn build_state(state: &AppState) -> ApiState {
    let config = state.config.lock().expect("config mutex poisoned").clone();
    let mut backend = state.backend.lock().expect("backend mutex poisoned");
    backend.collect_logs();
    let running = backend.is_running();
    let command_preview = backend.command_preview(&config);
    let logs = backend.logs().iter().cloned().collect();

    ApiState {
        config: ConfigView::from_config(&config),
        running,
        command_preview,
        logs,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    current_version: String,
    latest_version: String,
    has_update: bool,
    release_url: String,
    release_name: String,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiState {
    config: ConfigView,
    running: bool,
    command_preview: String,
    logs: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigView {
    role: String,
    language: String,
    bind: String,
    server: String,
    edge: String,
    scroll_scale: f64,
    scroll_response: f64,
    scroll_max_step: f64,
    scroll_frame_ms: u64,
    mac_command_mapping: String,
    mac_control_mapping: String,
    mac_option_mapping: String,
    mac_shift_mapping: String,
    mac_caps_lock_mapping: String,
    mac_escape_mapping: String,
    mac_backspace_mapping: String,
    mac_delete_mapping: String,
    mac_arrow_left_mapping: String,
    mac_arrow_right_mapping: String,
    mac_arrow_up_mapping: String,
    mac_arrow_down_mapping: String,
    auto_update_check: bool,
    config_path: String,
}

fn fetch_update_info() -> Result<UpdateInfo, Box<dyn std::error::Error>> {
    let release: GithubRelease = reqwest::blocking::Client::builder()
        .user_agent("Deskbridge-Updater")
        .build()?
        .get(LATEST_RELEASE_API)
        .send()?
        .error_for_status()?
        .json()?;
    let latest_version = normalize_version(&release.tag_name);
    let current_version = APP_VERSION.to_string();
    let has_update = compare_versions(&latest_version, APP_VERSION).is_gt();
    Ok(UpdateInfo {
        current_version,
        latest_version,
        has_update,
        release_url: release.html_url,
        release_name: release.name.unwrap_or(release.tag_name),
        error: None,
    })
}

fn open_url(url: &str) -> Result<(), String> {
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    }
    .map_err(|error| format!("failed to open browser: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("browser command exited with status: {status}"))
    }
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let left_parts = version_parts(left);
    let right_parts = version_parts(right);
    for index in 0..3 {
        let ordering = left_parts[index].cmp(&right_parts[index]);
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

fn version_parts(version: &str) -> [u64; 3] {
    let mut parts = [0, 0, 0];
    for (index, part) in normalize_version(version).split('.').take(3).enumerate() {
        let digits: String = part.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        parts[index] = digits.parse().unwrap_or(0);
    }
    parts
}

impl ConfigView {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            role: match config.role {
                Role::Server => "server",
                Role::Client => "client",
            }
            .to_string(),
            language: config.language.as_str().to_string(),
            bind: config.bind.clone(),
            server: config.server.clone(),
            edge: match config.edge {
                Edge::Left => "left",
                Edge::Right => "right",
                Edge::Both => "both",
            }
            .to_string(),
            scroll_scale: config.scroll_scale,
            scroll_response: config.scroll_response,
            scroll_max_step: config.scroll_max_step,
            scroll_frame_ms: config.scroll_frame_ms,
            mac_command_mapping: config.mac_command_mapping.as_str().to_string(),
            mac_control_mapping: config.mac_control_mapping.as_str().to_string(),
            mac_option_mapping: config.mac_option_mapping.as_str().to_string(),
            mac_shift_mapping: config.mac_shift_mapping.as_str().to_string(),
            mac_caps_lock_mapping: config.mac_caps_lock_mapping.as_str().to_string(),
            mac_escape_mapping: config.mac_escape_mapping.as_str().to_string(),
            mac_backspace_mapping: config.mac_backspace_mapping.as_str().to_string(),
            mac_delete_mapping: config.mac_delete_mapping.as_str().to_string(),
            mac_arrow_left_mapping: config.mac_arrow_left_mapping.as_str().to_string(),
            mac_arrow_right_mapping: config.mac_arrow_right_mapping.as_str().to_string(),
            mac_arrow_up_mapping: config.mac_arrow_up_mapping.as_str().to_string(),
            mac_arrow_down_mapping: config.mac_arrow_down_mapping.as_str().to_string(),
            auto_update_check: config.auto_update_check,
            config_path: config::config_path().to_string_lossy().to_string(),
        }
    }

    fn apply_to(self, config: &mut AppConfig) {
        config.role = if self.role == "server" {
            Role::Server
        } else {
            Role::Client
        };
        config.language = Language::parse(&self.language).unwrap_or(config.language);
        config.bind = self.bind;
        config.server = self.server;
        config.edge = Edge::parse(&self.edge).unwrap_or(config.edge);
        config.scroll_scale = self.scroll_scale;
        config.scroll_response = self.scroll_response;
        config.scroll_max_step = self.scroll_max_step;
        config.scroll_frame_ms = self.scroll_frame_ms;
        config.mac_command_mapping =
            ModifierTarget::parse(&self.mac_command_mapping).unwrap_or(config.mac_command_mapping);
        config.mac_control_mapping =
            ModifierTarget::parse(&self.mac_control_mapping).unwrap_or(config.mac_control_mapping);
        config.mac_option_mapping =
            ModifierTarget::parse(&self.mac_option_mapping).unwrap_or(config.mac_option_mapping);
        config.mac_shift_mapping =
            ModifierTarget::parse(&self.mac_shift_mapping).unwrap_or(config.mac_shift_mapping);
        config.mac_caps_lock_mapping =
            KeyTarget::parse(&self.mac_caps_lock_mapping).unwrap_or(config.mac_caps_lock_mapping);
        config.mac_escape_mapping =
            KeyTarget::parse(&self.mac_escape_mapping).unwrap_or(config.mac_escape_mapping);
        config.mac_backspace_mapping =
            KeyTarget::parse(&self.mac_backspace_mapping).unwrap_or(config.mac_backspace_mapping);
        config.mac_delete_mapping =
            KeyTarget::parse(&self.mac_delete_mapping).unwrap_or(config.mac_delete_mapping);
        config.mac_arrow_left_mapping =
            KeyTarget::parse(&self.mac_arrow_left_mapping).unwrap_or(config.mac_arrow_left_mapping);
        config.mac_arrow_right_mapping = KeyTarget::parse(&self.mac_arrow_right_mapping)
            .unwrap_or(config.mac_arrow_right_mapping);
        config.mac_arrow_up_mapping =
            KeyTarget::parse(&self.mac_arrow_up_mapping).unwrap_or(config.mac_arrow_up_mapping);
        config.mac_arrow_down_mapping =
            KeyTarget::parse(&self.mac_arrow_down_mapping).unwrap_or(config.mac_arrow_down_mapping);
        config.auto_update_check = self.auto_update_check;
    }
}
