// CrossDesk simple desktop flow, 2026-10-21 (GPL-3.0-only).
use crate::easy_connect::{Command, ConnectView, Controller};
use crate::trust_store::parse_id;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tauri::Manager;
struct DesktopState {
    controller: Option<Arc<Controller>>,
    error: Option<String>,
}
impl DesktopState {
    fn view(&self) -> ConnectView {
        if let Some(c) = &self.controller {
            c.view()
        } else {
            let mut v = ConnectView::loading();
            v.state = "error".into();
            v.error = self.error.clone();
            v
        }
    }
    fn send(&self, c: Command) -> Result<(), String> {
        self.controller
            .as_ref()
            .ok_or_else(|| {
                self.error
                    .clone()
                    .unwrap_or_else(|| "应用尚未准备好".into())
            })?
            .submit(c)
            .map_err(|e| e.to_string())
    }
}
pub fn run() -> std::io::Result<()> {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let result = app
                .path()
                .app_data_dir()
                .map_err(|e| e.to_string())
                .and_then(|root| Controller::open(root).map_err(|e| e.to_string()));
            let state = match result {
                Ok(c) => DesktopState {
                    controller: Some(Arc::new(c)),
                    error: None,
                },
                Err(e) => DesktopState {
                    controller: None,
                    error: Some(format!("无法读取应用数据：{e}")),
                },
            };
            app.manage(state);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_connect_state,
            select_device,
            pair_device,
            connect_device,
            disconnect_device,
            refresh_devices,
            forget_device,
            set_options,
            manual_connect,
            open_permissions,
            open_project_page
        ])
        .build(tauri::generate_context!())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let exiting = Arc::new(AtomicU8::new(0));
    app.run(move |app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if exiting.load(Ordering::Acquire) < 2 {
                api.prevent_exit();
                if exiting
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let controller = app.state::<DesktopState>().controller.clone();
                    let app = app.clone();
                    let exiting = exiting.clone();
                    std::thread::spawn(move || {
                        if let Some(c) = controller {
                            c.shutdown();
                        }
                        exiting.store(2, Ordering::Release);
                        app.exit(0);
                    });
                }
            }
        }
    });
    Ok(())
}
#[tauri::command]
fn get_connect_state(state: tauri::State<'_, DesktopState>) -> ConnectView {
    state.view()
}
#[tauri::command]
fn select_device(id: String, state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Select(parse_id(&id).map_err(|e| e.to_string())?))
}
#[tauri::command]
fn pair_device(code: String, state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Pair(code))
}
#[tauri::command]
fn connect_device(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Connect)
}
#[tauri::command]
fn disconnect_device(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Disconnect)
}
#[tauri::command]
fn refresh_devices(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Refresh)
}
#[tauri::command]
fn forget_device(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Forget)
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Options {
    auto_connect: bool,
    image_clipboard: bool,
    file_clipboard: bool,
}
#[tauri::command]
fn set_options(options: Options, state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Options {
        auto: options.auto_connect,
        image: options.image_clipboard,
        files: options.file_clipboard,
    })
}
#[tauri::command]
fn manual_connect(
    address: String,
    key_path: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<(), String> {
    let address = if let Ok(ip) = address.parse::<std::net::IpAddr>() {
        std::net::SocketAddr::new(ip, 24920)
    } else {
        address
            .parse()
            .map_err(|_| "请输入有效 IP 地址".to_string())?
    };
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err("请输入设备的有效 IP 地址".into());
    }
    state.send(Command::Manual {
        address,
        key: PathBuf::from(key_path),
    })
}
#[tauri::command]
async fn open_permissions() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("请打开系统设置中的辅助功能权限".into());
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn open_project_page() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let url = "https://github.com/rayem-prog/CrossDesk";
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("open").arg(url).status();
        #[cfg(windows)]
        let result = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status();
        #[cfg(not(any(target_os = "macos", windows)))]
        let result = std::process::Command::new("xdg-open").arg(url).status();
        result.map_err(|e| e.to_string()).and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err("请在浏览器打开项目主页".into())
            }
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
