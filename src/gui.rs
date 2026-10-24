// CrossDesk simple desktop flow, 2026-10-21 (GPL-3.0-only).
use crate::easy_connect::{Command, ConnectView, Controller};
use crate::trust_store::parse_id;
mod drop_policy;
mod drop_windows;
use drop_windows::*;
use std::sync::Mutex;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::DialogExt as _;
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
    fn connection_status(&self) -> (String, bool, Option<String>) {
        if let Some(controller) = &self.controller {
            controller.connection_status()
        } else {
            ("error".into(), false, self.error.clone())
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
    let background = std::env::args().any(|arg| arg == "--background");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if !args.iter().any(|arg| arg == "--background") {
                show_main(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["gui", "--background"])
                .build(),
        )
        .setup(move |app| {
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
            let resident = match create_tray(app.handle()) {
                Ok(tray) => ResidentState {
                    tray: Some(tray),
                    error: Mutex::new(None),
                },
                Err(error) => ResidentState {
                    tray: None,
                    error: Mutex::new(Some(format!("状态栏不可用：{error}"))),
                },
            };
            let available = resident.tray.is_some();
            app.manage(resident);
            if !background || !available {
                show_main(app.handle());
            } else if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
            if let Err(error) = drop_windows::start(app.handle()) {
                for label in ["drop-edge", "drop-receive"] {
                    if let Some(window) = app.get_webview_window(label) {
                        let _ = window.destroy();
                    }
                }
                *app.state::<ResidentState>().error.lock().unwrap() =
                    Some(format!("无法创建投送窗口：{error}"));
                show_main(app.handle());
            }
            monitor_desktop(app.handle().clone(), background);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "main" {
                    window.app_handle().exit(0);
                } else {
                    drop_windows::on_close(window);
                }
            } else if window.label() == "main"
                && matches!(
                    event,
                    tauri::WindowEvent::Focused(_) | tauri::WindowEvent::Resized(_)
                )
            {
                let visible =
                    window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false);
                let _ = window.emit("crossdesk-visibility", visible);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_drop_state,
            set_drop_edge,
            accept_drop,
            reject_drop,
            cancel_drop,
            open_drop_result,
            dismiss_drop,
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
            open_project_page,
            hide_to_background,
            get_desktop_state,
            get_window_visibility,
            set_start_at_login,
            start_input_diagnostics,
            stop_input_diagnostics,
            export_input_diagnostics
        ])
        .build(tauri::generate_context!())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let exiting = Arc::new(AtomicU8::new(0));
    app.run(move |app, event| {
        #[cfg(target_os = "macos")]
        if matches!(event, tauri::RunEvent::Reopen { .. }) {
            show_main(app);
        }
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
    text_clipboard: bool,
    image_clipboard: bool,
    file_clipboard: bool,
}
#[tauri::command]
fn set_options(options: Options, state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::Options {
        auto: options.auto_connect,
        text: options.text_clipboard,
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopView {
    background_available: bool,
    start_at_login: bool,
    error: Option<String>,
}
struct TrayState {
    _icon: TrayIcon,
    status: MenuItem<tauri::Wry>,
    pause: MenuItem<tauri::Wry>,
    resume: MenuItem<tauri::Wry>,
    login: CheckMenuItem<tauri::Wry>,
}
struct ResidentState {
    tray: Option<TrayState>,
    error: Mutex<Option<String>>,
}
fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = window.emit("crossdesk-visibility", true);
    }
}
fn create_tray(app: &tauri::AppHandle) -> tauri::Result<TrayState> {
    let status = MenuItem::with_id(app, "status", "CrossDesk · 正在启动", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "打开 CrossDesk", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "暂停连接", true, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resume", "恢复连接", true, None::<&str>)?;
    let login = CheckMenuItem::with_id(app, "login", "登录时启动", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 CrossDesk", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status, &open, &pause, &resume, &login, &quit])?;
    // Use an explicit icon: packaged defaults are not guaranteed during development.
    let image = image::load_from_memory(include_bytes!("../assets/deskbridge.png"))
        .map_err(|e| tauri::Error::Io(std::io::Error::other(e.to_string())))?
        .to_rgba8();
    let icon =
        tauri::image::Image::new_owned(image.as_raw().clone(), image.width(), image.height());
    let tray = TrayIconBuilder::with_id("crossdesk-resident")
        .icon(icon)
        .tooltip("CrossDesk")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let result = match event.id.as_ref() {
                "open" => {
                    show_main(app);
                    Ok(())
                }
                "pause" => app.state::<DesktopState>().send(Command::Disconnect),
                "resume" => app.state::<DesktopState>().send(Command::Connect),
                "login" => {
                    let app = app.clone();
                    std::thread::spawn(move || {
                        match app.autolaunch().is_enabled() {
                            Ok(enabled) => {
                                update_start_at_login(&app, !enabled);
                            }
                            Err(error) => {
                                *app.state::<ResidentState>().error.lock().unwrap() =
                                    Some(format!("无法读取登录启动状态：{error}"));
                            }
                        }
                        if desktop_view(&app).error.is_some() {
                            show_main(&app);
                        }
                    });
                    Ok(())
                }
                "quit" => {
                    app.exit(0);
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                *app.state::<ResidentState>().error.lock().unwrap() = Some(error);
                show_main(app);
            }
        })
        .build(app)?;
    Ok(TrayState {
        _icon: tray,
        status,
        pause,
        resume,
        login,
    })
}
fn desktop_view(app: &tauri::AppHandle) -> DesktopView {
    let resident = app.state::<ResidentState>();
    let (start_at_login, query_error) = match app.autolaunch().is_enabled() {
        Ok(enabled) => (enabled, None),
        Err(error) => (false, Some(format!("无法读取登录启动状态：{error}"))),
    };
    if let Some(tray) = &resident.tray {
        let _ = tray.login.set_checked(start_at_login);
    }
    DesktopView {
        background_available: resident.tray.is_some(),
        start_at_login,
        error: query_error.or_else(|| resident.error.lock().unwrap().clone()),
    }
}
fn update_start_at_login(app: &tauri::AppHandle, enabled: bool) -> DesktopView {
    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    let resident = app.state::<ResidentState>();
    *resident.error.lock().unwrap() = match result {
        Ok(()) => None,
        Err(error) => Some(format!("无法更改登录启动设置：{error}")),
    };
    let mut view = desktop_view(app);
    if view.error.is_none() && view.start_at_login != enabled {
        let error = "系统未应用登录启动设置，请检查系统设置".to_string();
        *resident.error.lock().unwrap() = Some(error.clone());
        view.error = Some(error);
    }
    view
}
#[tauri::command]
fn hide_to_background(app: tauri::AppHandle) -> Result<(), String> {
    if app.state::<ResidentState>().tray.is_none() {
        return Err("状态栏不可用，无法后台运行".into());
    }
    let window = app.get_webview_window("main").ok_or("找不到主窗口")?;
    window.hide().map_err(|e| e.to_string())?;
    let _ = window.emit("crossdesk-visibility", false);
    Ok(())
}
#[tauri::command]
fn get_window_visibility(app: tauri::AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false)
    })
}
#[tauri::command]
async fn get_desktop_state(app: tauri::AppHandle) -> Result<DesktopView, String> {
    tauri::async_runtime::spawn_blocking(move || desktop_view(&app))
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn set_start_at_login(app: tauri::AppHandle, enabled: bool) -> Result<DesktopView, String> {
    tauri::async_runtime::spawn_blocking(move || update_start_at_login(&app, enabled))
        .await
        .map_err(|e| e.to_string())
}
fn monitor_desktop(app: tauri::AppHandle, background: bool) {
    std::thread::spawn(move || {
        let mut startup = crate::resident::StartupPolicy::new(background);
        let mut previous = String::new();
        let mut previous_visibility = None;
        loop {
            if let Some(window) = app.get_webview_window("main") {
                let visible =
                    window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false);
                if previous_visibility != Some(visible) {
                    let _ = window.emit("crossdesk-visibility", visible);
                    previous_visibility = Some(visible);
                }
            }
            let (state, has_paired, error) = app.state::<DesktopState>().connection_status();
            let resident = app.state::<ResidentState>();
            let permission_required = error.as_deref().is_some_and(|error| {
                let error = error.to_lowercase();
                error.contains("权限")
                    || error.contains("permission")
                    || error.contains("accessibility")
                    || error.contains("允许")
                    || error.contains("辅助功能")
            });
            if startup.reveal(
                state != "starting",
                has_paired,
                permission_required,
                state == "connected",
                resident.tray.is_some(),
            ) {
                if app.get_webview_window("main").is_some_and(|w| {
                    !w.is_visible().unwrap_or(true) || w.is_minimized().unwrap_or(false)
                }) {
                    show_main(&app);
                }
            }
            if previous != state {
                if let Some(tray) = &resident.tray {
                    let label = match state.as_str() {
                        "connected" => "已连接",
                        "paused" => "已暂停",
                        "error" => "需要处理",
                        "connecting" | "retrying" => "正在连接",
                        "listening" => "等待连接",
                        "hosting" | "pairing" => "等待配对",
                        _ => "正在查找设备",
                    };
                    let text = format!("CrossDesk · {label}");
                    let _ = tray.status.set_text(&text);
                    let _ = tray._icon.set_tooltip(Some(&text));
                    let paused = state == "paused" || state == "error";
                    let _ = tray.pause.set_enabled(!paused);
                    let _ = tray.resume.set_enabled(paused);
                }
                previous = state;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}
#[tauri::command]
fn start_input_diagnostics(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::DiagnosticsStart)
}
#[tauri::command]
fn stop_input_diagnostics(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    state.send(Command::DiagnosticsStop)
}
#[tauri::command]
async fn export_input_diagnostics(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let view = app.state::<DesktopState>().view();
    let report = view.diagnostics.report.ok_or("还没有可导出的诊断报告")?;
    let payload = serde_json::json!({ "version": view.app_version, "platform": view.platform, "report": report });
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = app
            .dialog()
            .file()
            .add_filter("JSON", &["json"])
            .set_file_name("CrossDesk-diagnostics.json")
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = path.into_path().map_err(|e| e.to_string())?;
        let data = serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?;
        std::fs::write(&path, data).map_err(|e| e.to_string())?;
        Ok(Some(path.display().to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}
