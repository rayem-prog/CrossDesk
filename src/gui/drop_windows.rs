use super::*;
use crate::edge_drop::{DropCommand, DropView};
use std::time::{Duration, Instant};
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DropSnapshot {
    pub edge: String,
    pub drop: DropView,
    pub peer_name: String,
    pub error: Option<String>,
}
#[derive(Default)]
pub(super) struct DropWindows {
    expanded: bool,
    emitted_expanded: Option<bool>,
    resized: Option<Instant>,
    dismissed_incoming: Option<String>,
    dismissed_outgoing: Option<String>,
    error: Option<String>,
    dismissed_error: Option<String>,
}
pub(super) type DropWindowsState = Mutex<DropWindows>;
fn snapshot(app: &tauri::AppHandle) -> DropSnapshot {
    let desktop = app.state::<DesktopState>();
    let (edge, mut drop, peer_name) = desktop
        .controller
        .as_ref()
        .map(|c| c.drop_status())
        .unwrap_or_else(|| ("off".into(), Default::default(), "已配对设备".into()));
    let state = app.state::<DropWindowsState>();
    let state = state.lock().unwrap();
    if drop
        .outgoing
        .as_ref()
        .is_some_and(|t| state.dismissed_outgoing.as_deref() == Some(&t.id))
    {
        drop.outgoing = None;
    }
    if drop
        .incoming
        .as_ref()
        .is_some_and(|t| state.dismissed_incoming.as_deref() == Some(&t.id))
    {
        drop.incoming = None;
    }
    let error = state
        .error
        .clone()
        .or_else(|| desktop.controller.as_ref().and_then(|c| c.drop_error()));
    let error = error.filter(|error| state.dismissed_error.as_ref() != Some(error));
    DropSnapshot {
        edge,
        drop,
        peer_name,
        error,
    }
}
#[tauri::command]
pub(super) fn get_drop_state(app: tauri::AppHandle) -> DropSnapshot {
    snapshot(&app)
}
#[tauri::command]
pub(super) fn set_drop_edge(
    edge: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<(), String> {
    state.send(Command::DropEdge(edge))
}
fn send(app: &tauri::AppHandle, command: DropCommand) -> Result<(), String> {
    let result = app.state::<DesktopState>().send(Command::Drop(command));
    let state = app.state::<DropWindowsState>();
    let mut state = state.lock().unwrap();
    state.error = result.as_ref().err().cloned();
    state.dismissed_error = None;
    result
}
#[tauri::command]
pub(super) async fn accept_drop(app: tauri::AppHandle, id: String) -> Result<bool, String> {
    let current = snapshot(&app);
    if current.edge == "off"
        || !current.drop.enabled
        || !current
            .drop
            .incoming
            .as_ref()
            .is_some_and(|t| t.id == id && t.state == "waiting")
    {
        return Err("此投送已失效或已关闭".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let Some(directory) = app
            .dialog()
            .file()
            .set_title("选择 CrossDesk 文件保存位置")
            .blocking_pick_folder()
        else {
            return Ok(false);
        };
        let directory = directory.into_path().map_err(|e| e.to_string())?;
        let latest = snapshot(&app);
        if latest.edge == "off"
            || !latest
                .drop
                .incoming
                .as_ref()
                .is_some_and(|t| t.id == id && t.state == "waiting")
        {
            return Err("此投送已失效或已关闭".into());
        }
        send(&app, DropCommand::Accept { id, directory })?;
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub(super) fn reject_drop(app: tauri::AppHandle, id: String) -> Result<(), String> {
    send(&app, DropCommand::Reject { id })
}
#[tauri::command]
pub(super) fn cancel_drop(app: tauri::AppHandle, id: String) -> Result<(), String> {
    send(&app, DropCommand::Cancel { id })
}
fn dismiss_window(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    let current = snapshot(app);
    app.state::<DropWindowsState>()
        .lock()
        .unwrap()
        .dismissed_error = current.error.clone();
    let transfer = if label == "drop-receive" {
        current.drop.incoming
    } else {
        current.drop.outgoing
    };
    if let Some(t) = transfer {
        match super::drop_policy::close_action(&t.state) {
            "reject" => send(app, DropCommand::Reject { id: t.id.clone() })?,
            "cancel" => send(app, DropCommand::Cancel { id: t.id.clone() })?,
            _ => {}
        }
        let state = app.state::<DropWindowsState>();
        let mut state = state.lock().unwrap();
        if label == "drop-receive" {
            state.dismissed_incoming = Some(t.id);
        } else {
            state.dismissed_outgoing = Some(t.id);
            state.expanded = false;
        }
        state.error = None;
    } else {
        let state = app.state::<DropWindowsState>();
        let mut state = state.lock().unwrap();
        state.expanded = false;
        state.error = None;
    }
    if let Some(window) = app.get_webview_window(label) {
        hide_surface(&window).map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
pub(super) fn dismiss_drop(window: tauri::WebviewWindow) -> Result<(), String> {
    dismiss_window(window.app_handle(), window.label())
}
#[tauri::command]
pub(super) async fn open_drop_result(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let current = snapshot(&app);
    let transfer = current
        .drop
        .incoming
        .filter(|t| t.id == id && t.state == "saved")
        .ok_or("没有可打开的已保存文件夹")?;
    let path = transfer.saved_path.ok_or("保存位置不可用")?;
    tauri::async_runtime::spawn_blocking(move || {
        if !path.is_dir() {
            return Err("保存文件夹已移走或不可用".into());
        }
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("open").arg(&path).status();
        #[cfg(windows)]
        let result = std::process::Command::new("explorer.exe")
            .arg(&path)
            .status();
        #[cfg(not(any(target_os = "macos", windows)))]
        let result = std::process::Command::new("xdg-open").arg(&path).status();
        if result.map_err(|e| e.to_string())?.success() {
            Ok(())
        } else {
            Err("无法打开保存文件夹".into())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
fn create_window(app: &tauri::AppHandle, label: &str, surface: &str) -> tauri::Result<()> {
    let window = tauri::WebviewWindowBuilder::new(
        app,
        label,
        tauri::WebviewUrl::App(format!("index.html?surface={surface}").into()),
    )
    .title("CrossDesk 文件投送")
    .inner_size(340.0, 330.0)
    .min_inner_size(24.0, 100.0)
    .decorations(false)
    .resizable(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .focused(false)
    .build()?;
    if label == "drop-edge" {
        let app = app.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::DragDrop(event) = event {
                match event {
                    tauri::DragDropEvent::Enter { .. } => {
                        let state = app.state::<DropWindowsState>();
                        let mut state = state.lock().unwrap();
                        state.expanded = true;
                        state.resized = Some(Instant::now());
                        state.error = None;
                        state.dismissed_error = None;
                        drop(state);
                        layout(&app);
                    }
                    tauri::DragDropEvent::Leave => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(450));
                            if let Some(w) = app.get_webview_window("drop-edge") {
                                let inside = w
                                    .cursor_position()
                                    .ok()
                                    .zip(w.outer_position().ok().zip(w.outer_size().ok()))
                                    .is_some_and(|(cursor, (pos, size))| {
                                        cursor.x >= pos.x as f64
                                            && cursor.y >= pos.y as f64
                                            && cursor.x < (pos.x as f64 + size.width as f64)
                                            && cursor.y < (pos.y as f64 + size.height as f64)
                                    });
                                let state = app.state::<DropWindowsState>();
                                let mut state = state.lock().unwrap();
                                let stable = state.resized.is_none_or(|t| {
                                    super::drop_policy::should_collapse(
                                        t.elapsed().as_millis() as u64,
                                        0,
                                    )
                                });
                                if !inside && stable {
                                    state.expanded = false;
                                    drop(state);
                                    layout(&app);
                                }
                            }
                        });
                    }
                    tauri::DragDropEvent::Drop { paths, .. } => {
                        let current = snapshot(&app);
                        let result = if current.edge == "off" || !current.drop.enabled {
                            Err("边缘投送已关闭".into())
                        } else if current.drop.peer_supported == Some(false) {
                            Err("请升级两端 CrossDesk 以使用投送".into())
                        } else if current.drop.peer_enabled == Some(false) {
                            Err("对端已关闭投送".into())
                        } else if current.drop.channel != "ready" {
                            Err("文件通道尚未就绪，请先连接设备".into())
                        } else {
                            send(
                                &app,
                                DropCommand::Send {
                                    paths: paths.clone(),
                                },
                            )
                        };
                        let state = app.state::<DropWindowsState>();
                        let mut state = state.lock().unwrap();
                        state.error = result.err();
                        state.expanded = state.error.is_some();
                        drop(state);
                        layout(&app);
                    }
                    _ => {}
                }
            }
        });
    }
    Ok(())
}
// Tauri's ordinary show() makes macOS windows key and Windows windows active.
// These surfaces must appear without moving focus away from the user's work.
fn show_without_focus(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let window = window.clone();
        let dispatch = window.clone();
        let _ = dispatch.run_on_main_thread(move || {
            #[link(name = "objc")]
            unsafe extern "C" {
                fn sel_registerName(name: *const std::ffi::c_char) -> *mut std::ffi::c_void;
                fn objc_msgSend();
            }
            if let Ok(native) = window.ns_window() {
                // orderFrontRegardless changes visibility and stacking, never key status.
                unsafe {
                    let selector = sel_registerName(c"orderFrontRegardless".as_ptr());
                    let call: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) =
                        std::mem::transmute(objc_msgSend as *const ());
                    call(native, selector);
                }
            }
        });
    }
    #[cfg(windows)]
    {
        if let Ok(hwnd) = window.hwnd() {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd.0 as _,
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
                );
            }
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = window.show();
    }
}
fn hide_surface(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    #[cfg(windows)]
    {
        let hwnd = window.hwnd()?;
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                hwnd.0 as _,
                windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        window.hide()
    }
}
fn layout(app: &tauri::AppHandle) {
    let current = snapshot(app);
    let Some(monitor) = app.primary_monitor().ok().flatten() else {
        return;
    };
    let state = app.state::<DropWindowsState>();
    let mut state = state.lock().unwrap();
    let pos = monitor.position();
    let size = monitor.size();
    let scale = monitor.scale_factor();
    let show_out = current
        .drop
        .outgoing
        .as_ref()
        .is_some_and(|t| state.dismissed_outgoing.as_deref() != Some(&t.id));
    let expanded = state.expanded || show_out || current.error.is_some();
    if let Some(window) = app.get_webview_window("drop-edge") {
        if current.edge == "off" {
            let _ = hide_surface(&window);
        } else {
            let (x, y, w, h) = super::drop_policy::edge_bounds(
                (pos.x, pos.y, size.width, size.height),
                scale,
                &current.edge,
                expanded,
            );
            let h = if show_out { (330.0 * scale) as u32 } else { h };
            if window.outer_size().ok() != Some(tauri::PhysicalSize::new(w, h)) {
                let _ = window.set_size(tauri::PhysicalSize::new(w, h));
            }
            if window.outer_position().ok() != Some(tauri::PhysicalPosition::new(x, y)) {
                let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
            }
            if !window.is_visible().unwrap_or(false) {
                show_without_focus(&window);
            }
            if state.emitted_expanded != Some(expanded) {
                let _ = window.emit("crossdesk-drop-expanded", expanded);
                state.emitted_expanded = Some(expanded);
            }
        }
    }
    if let Some(window) = app.get_webview_window("drop-receive") {
        let show = current.edge != "off"
            && current.drop.enabled
            && current
                .drop
                .incoming
                .as_ref()
                .is_some_and(|t| state.dismissed_incoming.as_deref() != Some(&t.id));
        if show {
            let w = (340.0 * scale) as u32;
            let h = (330.0 * scale) as u32;
            let _ = window.set_size(tauri::PhysicalSize::new(w, h));
            let _ = window.set_position(tauri::PhysicalPosition::new(
                pos.x + size.width as i32 - w as i32 - (24.0 * scale) as i32,
                pos.y + (size.height as i32 - h as i32) / 2,
            ));
            if !window.is_visible().unwrap_or(false) {
                show_without_focus(&window);
            }
        } else {
            let _ = hide_surface(&window);
        }
    }
}
pub(super) fn on_close(window: &tauri::Window) {
    let _ = dismiss_window(window.app_handle(), window.label());
}
pub(super) fn start(app: &tauri::AppHandle) -> tauri::Result<()> {
    app.manage(DropWindowsState::default());
    create_window(app, "drop-edge", "edge")?;
    create_window(app, "drop-receive", "receive")?;
    let app = app.clone();
    std::thread::spawn(move || {
        let mut previous = String::new();
        loop {
            let value = snapshot(&app);
            let serialized = serde_json::to_string(&value).unwrap_or_default();
            if serialized != previous {
                for label in ["drop-edge", "drop-receive"] {
                    if let Some(window) = app.get_webview_window(label) {
                        let _ = window.emit("crossdesk-drop", &value);
                    }
                }
                previous = serialized;
            }
            // Primary display geometry is queried without touching the connection or the hidden main view.
            layout(&app);
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    Ok(())
}
