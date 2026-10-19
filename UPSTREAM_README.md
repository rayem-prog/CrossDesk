# Deskbridge

Deskbridge is a Deskflow-like keyboard, mouse, clipboard, and file bridge for
Windows, macOS, and Linux systems.

Deskbridge was created mainly to work around the macOS and Linux support gaps
and compatibility issues encountered when using Deskflow-like workflows. The
code therefore puts extra attention on macOS input injection, Linux X11/Wayland
backends, keyboard mapping, clipboard formats, wheel behavior, packaging, and
permission handling.

The project is experimental but usable for daily testing. It was developed with
substantial assistance from OpenAI Codex for code generation, debugging,
packaging, documentation, and iterative user testing.

Because of macOS and Linux desktop-environment compatibility limits, the mouse
may still occasionally stutter or pause. Linux GUI startup also depends on the
local WebKitGTK, Mesa/NVIDIA, X11/Wayland, and EGL stack. These are known
compatibility areas and will be improved in later versions.

## Features

- One binary with a bilingual GUI control panel
- Windows, macOS, and Linux server/client modes
- Keyboard and mouse sharing between Windows, macOS, and Linux where platform permissions allow
- Shortcut-oriented modifier mapping for cross-platform control
- Caps Lock state emulation for remote macOS letter input
- macOS screenshot mapping through the Windows Print Screen key
- Tunable macOS wheel smoothing
- Bidirectional clipboard sync for text, bitmap images, and files
- GUI file drop zone on both systems
- Windows edge drop strip for Explorer file transfer
- macOS `.app`/`.pkg`, Windows Inno Setup, and Linux `.deb`/AppImage packaging support
- macOS `.icns` icon, Windows `.ico` icon, and Linux desktop bundle support

## Status

This repository supports these practical setups:

- Server: Windows, macOS, or Linux
- Client: Windows, macOS, or Linux
- Transport: TCP over LAN, hotspot LAN, or Tailscale

Linux support is beta. Linux client input injection uses Deskbridge's native `/dev/uinput` virtual input backend on both X11 and Wayland. Linux server mode uses readable `/dev/input/event*` devices for global keyboard/mouse capture and grabs those devices while the pointer is on the remote screen. Linux clipboard uses `wl-copy`/`wl-paste` on Wayland and `xclip` or `xsel` on X11. Images are normalized to PNG, and file clipboard data supports both `text/uri-list` and `x-special/gnome-copied-files` where the selected clipboard backend allows it. The Linux GUI initializes WebKitGTK with safer rendering defaults to avoid common EGL/DMABUF startup failures.

Direct drag-and-drop of a file across the screen edge into an arbitrary Finder or
Explorer window is not fully implemented. File transfer currently works through
copy/paste, the GUI drop zone, and the Windows edge drop strip. The macOS server
path remains a beta feature. Version 1.1.4 uses one shared pointer transition
state machine for both server platforms and explicit enter/leave protocol
messages, but the available real-device and multi-display test matrix is still
smaller than the Windows server path.

## Install

Download the latest release from:

https://github.com/paulhe666/deskbridge/releases

### Windows

1. Download `Deskbridge-Windows-Setup-<version>.exe` or the portable `.exe`.
2. Open the installer and follow the setup wizard.
3. Start `Deskbridge` from the Start menu.
4. If Windows Firewall asks for permission, allow Deskbridge on private
   networks.

### macOS

1. Download `Deskbridge-<version>.pkg` or `Deskbridge-macOS-app.zip`.
2. Open the package and install Deskbridge.
3. Open `System Settings -> Privacy & Security -> Accessibility`.
4. Add and enable `Deskbridge.app`.
5. Start `Deskbridge` from Applications.

### Linux

1. Download the Linux `.deb`, AppImage, or source package.
2. On Debian/Ubuntu, install the `.deb` with `sudo apt install ./Deskbridge_<version>_amd64.deb`.
3. For AppImage, run `chmod +x Deskbridge_<version>_amd64.AppImage` and start it directly.
4. If the AppImage or local GUI fails with an EGL/WebKitGTK error, try `sh scripts/run_linux_safe.sh` from a source build, or run with `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
5. Install Linux helper tools according to your session: `xclip` or `xsel` for X11 clipboard and `wl-clipboard` for Wayland clipboard. Native Linux input requires `/dev/uinput` write permission for client-side injection and `/dev/input/event*` read permission for server-side global capture. If automatic screen-size detection is wrong, set `DESKBRIDGE_SCREEN_SIZE=1920x1080` before starting Deskbridge.

## Quick Start

Launch `Deskbridge` directly.

Windows server, macOS client:

1. Set role to `Server`.
2. Keep bind as `0.0.0.0:24920`.
3. Set edge to `right` if the Mac is to the right of the Windows screen.
4. On macOS, set role to `Client` and server to `WINDOWS_IP:24920`.
5. Click `Start` on both systems.

macOS server, Windows client:

1. On macOS, set role to `Server`.
2. Keep bind as `0.0.0.0:24920`.
3. Set edge to `right` if the Windows screen is to the right of the Mac screen.
4. On Windows, set role to `Client` and server to `MAC_IP:24920`.
5. Click `Start` on both systems.

By default, the GUI saves config next to the running executable:

```text
<program-directory>/config.ini
```

Set `DESKBRIDGE_CONFIG_DIR` to override this location.

The GUI supports Chinese and English. If Chinese text renders as boxes, ensure
the operating system has a CJK UI font such as Hiragino Sans GB, STHeiti,
Microsoft YaHei, SimHei, or Noto Sans CJK.

## Modifier Mapping

When macOS runs as the server and controls Windows, the app can map the physical
macOS Command, Control, and Option keys to Windows Ctrl, Win, Alt, or disabled.
Open `Settings` next to the Start/Stop button in the GUI to adjust these values.
Restart the service after saving mapping changes.

The default keeps compatibility with earlier builds:

- Command -> Ctrl
- Control -> Ctrl
- Option -> Alt

Command can be changed to Win if your workflow treats it more like the Windows
logo key.

## Uninstall

On Windows, uninstall Deskbridge from Settings or Control Panel.

On macOS, remove:

```text
/Applications/Deskbridge.app
~/.deskbridge
```

The macOS release package also includes `Uninstall Deskbridge.command` in the
app zip artifact.

## Developer Build

End users should use the release installers above. Developers can still build
packages from source:

macOS:

```bash
./scripts/package_macos.sh
```

Windows:

```powershell
.\scripts\package_windows.ps1
```

Windows packaging requires Rust, Visual Studio Build Tools with the Windows SDK,
and Inno Setup 6.

Linux:

```bash
sudo apt install -y build-essential curl wget file pkg-config libssl-dev \
  libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libwebkit2gtk-4.1-dev
cd web && npm install && npm run build && cd ..
cargo build --release
cargo tauri build --bundles deb,appimage
```

For conservative Linux GUI startup during local testing:

```bash
sh scripts/run_linux_safe.sh
```

Optional Linux GUI environment overrides:

```bash
DESKBRIDGE_LINUX_GUI_BACKEND=x11 deskbridge gui
DESKBRIDGE_LINUX_GUI_BACKEND=wayland deskbridge gui
DESKBRIDGE_LINUX_SOFTWARE_RENDERING=1 deskbridge gui
```

## Architecture

The GUI and service backend use a process boundary:

- `src/gui.rs` contains presentation and user interaction only.
- `src/control.rs` owns service lifecycle, logs, command construction, and
  GUI clipboard publishing.
- `server` and `client` are standalone backend commands and never import the
  GUI or `eframe`.
- A GUI child receives a graceful `stop\n` command over stdin before any
  forced termination. This releases captured keys, mouse buttons, and the
  hidden macOS cursor.

This command/stdin contract is language neutral. A future frontend can be
implemented without Rust by launching the same `server` or `client` command,
setting `DESKBRIDGE_GUI_CHILD=1`, and sending `stop\n` on shutdown.

## macOS Permissions

The macOS app needs Accessibility permission to inject or capture keyboard and
mouse input. macOS server mode may also require Input Monitoring permission.

Open:

```text
System Settings -> Privacy & Security -> Accessibility
```

Add and enable `Deskbridge.app`, or the terminal app if running from terminal.

## Command Line

The GUI starts these same service commands internally.

Windows server:

```powershell
deskbridge.exe server --bind 0.0.0.0:24920 --edge right
```

macOS client:

```bash
deskbridge client --server WINDOWS_IP:24920
```

macOS server:

```bash
deskbridge server --bind 0.0.0.0:24920 --edge right
```

macOS server with Command mapped to Win:

```bash
DESKBRIDGE_MAC_COMMAND_MAPPING=win \
deskbridge server --bind 0.0.0.0:24920 --edge right
```

Windows client:

```powershell
deskbridge.exe client --server MAC_IP:24920
```

macOS client with scroll tuning:

```bash
DESKBRIDGE_SCROLL_SCALE=1.35 \
DESKBRIDGE_SCROLL_RESPONSE=0.38 \
DESKBRIDGE_SCROLL_MAX_STEP=120 \
DESKBRIDGE_SCROLL_FRAME_MS=8 \
deskbridge client --server WINDOWS_IP:24920
```

## Low Latency Notes

Use a direct LAN IP when possible. Relay paths can noticeably affect keyboard,
mouse, and wheel responsiveness.

Deskbridge coalesces high-frequency mouse and wheel deltas and keeps keyboard and
button events immediate. File transfer chunks are intentionally modest so large
clipboard/file transfers do not monopolize the input stream for too long.

On macOS, the input backend uses a small native C shim. It initializes
`IOHIDManager` for HID device discovery/diagnostics, then posts synthetic input
through the public CoreGraphics event APIs. The shim also disables the default
local event suppression interval on its `CGEventSource` to reduce short pauses
after synthetic events.

The input coalescing window defaults to 2 ms. It can be tuned on both sides:

```bash
DESKBRIDGE_INPUT_FLUSH_MS=2 deskbridge client --server WINDOWS_IP:24920
```

Allowed values are clamped to 1-16 ms. If pointer motion still occasionally
pauses under a high-polling-rate mouse or a relay network path, try 3-4 ms.

## License

Deskbridge is licensed under the GNU General Public License v3.0. See
[`LICENSE`](LICENSE) for details.

---

# Deskbridge 中文说明

Deskbridge 是一个类似 Deskflow 的跨设备工具，目标是在 Windows、macOS 和
Linux 之间实现键鼠共享、剪贴板同步和文件投递。

本项目主要是为了解决 Deskflow 类工作流在 macOS 和 Linux 支持、兼容性上的不足，
因此对 macOS 输入注入、Linux X11/Wayland 后端、键盘映射、剪贴板格式、滚轮行为、打包和权限处理做了
较多优化。

本项目仍然是实验性质，但已经可以用于日常测试。项目开发过程中大量使用了
OpenAI Codex 辅助完成代码生成、调试、打包、文档编写和迭代测试。

由于 macOS 和 Linux 桌面环境兼容性本身存在限制，鼠标仍然可能出现偶发卡顿或短暂停顿。
Linux GUI 还依赖本机 WebKitGTK、Mesa/NVIDIA、X11/Wayland 和 EGL 图形栈。
这些都是已知兼容性问题，将在后续版本继续优化。

## 功能

- 一个二进制文件，内置中英文 GUI 控制面板
- Windows、macOS 和 Linux 服务端/客户端模式
- 支持 Windows、macOS、Linux 之间的跨平台键鼠共享，具体能力取决于平台权限和桌面环境
- 跨平台控制时面向常用快捷键的修饰键映射
- 远程 Caps Lock 状态模拟，用于 macOS 字母大小写输入
- Windows Print Screen 映射到 macOS 截图
- 可调节的 macOS 滚轮平滑参数
- 文本、图片、文件三类剪贴板双向同步
- 两端 GUI 文件拖放投递区
- Windows 屏幕边缘文件投递条
- macOS `.app`/`.pkg`、Windows Inno Setup、Linux `.deb`/AppImage 打包
- macOS `.icns` 图标、Windows `.ico` 图标和 Linux 桌面包支持

## 当前状态

当前支持这些实际使用场景：

- 服务端：Windows、macOS 或 Linux
- 客户端：Windows、macOS 或 Linux
- 传输：局域网、热点局域网或 Tailscale TCP 连接

Linux 支持目前属于 Beta。Linux 客户端输入注入使用 Deskbridge 内置的 `/dev/uinput` 虚拟输入后端，X11 和 Wayland 都走同一套后端。Linux 服务端使用可读取的 `/dev/input/event*` 设备做全局键鼠捕获，并在鼠标位于远端屏幕时临时 grab 本机输入设备。Wayland 剪贴板依赖 `wl-copy`/`wl-paste`；X11 剪贴板依赖 `xclip` 或 `xsel`。图片剪贴板会尽量归一化为 PNG；文件剪贴板会兼容 `text/uri-list` 与 `x-special/gnome-copied-files`。Linux GUI 会在启动时设置更保守的 WebKitGTK 渲染环境，以减少 EGL/DMABUF 导致的白屏或崩溃。

目前还没有完整实现“把文件从一台电脑直接拖过屏幕边缘并放到另一台电脑的
任意 Finder/Explorer 窗口”。现在可用的是复制粘贴、GUI 拖放投递区，以及
Windows 边缘投递条。macOS 作为服务端目前仍属于 Beta 功能。v1.1.4 已让
Windows 和 macOS 服务端共用一套指针切换状态机，并在协议中明确发送进入和
离开事件，但真实设备、多显示器和不同鼠标型号的测试矩阵仍小于 Windows
服务端路径，后续还需要继续实机验证。

## 安装

从 GitHub Release 下载最新版：

https://github.com/paulhe666/deskbridge/releases

### Windows

1. 下载 `Deskbridge-Windows-Setup-<version>.exe` 或 portable `.exe`。
2. 双击安装包并按安装向导安装。
3. 从开始菜单启动 `Deskbridge`。
4. 如果 Windows 防火墙弹窗，请允许 Deskbridge 访问专用网络。

### macOS

1. 下载 `Deskbridge-<version>.pkg` 或 `Deskbridge-macOS-app.zip`。
2. 双击 pkg 安装。
3. 打开 `系统设置 -> 隐私与安全性 -> 辅助功能`。
4. 添加并启用 `Deskbridge.app`。
5. 从应用程序里启动 `Deskbridge`。

### Linux

1. 下载 Linux `.deb`、AppImage 或源码包。
2. Debian/Ubuntu 可以用 `sudo apt install ./Deskbridge_<version>_amd64.deb` 安装。
3. AppImage 需要先 `chmod +x Deskbridge_<version>_amd64.AppImage`，再直接启动。
4. 如果出现 EGL/WebKitGTK 白屏或崩溃，源码构建时可以用 `sh scripts/run_linux_safe.sh` 启动，或手动加 `WEBKIT_DISABLE_DMABUF_RENDERER=1`。
5. Linux 输入需要先配置权限：客户端注入需要 `/dev/uinput` 写权限，服务端全局捕获需要 `/dev/input/event*` 读取权限。运行 `sh scripts/setup_linux_uinput.sh` 查看推荐配置后退出并重新登录。剪贴板工具按桌面环境安装：Wayland 使用 `wl-clipboard`；X11 使用 `xclip` 或 `xsel`。如果自动检测屏幕大小不正确，可以在启动前设置 `DESKBRIDGE_SCREEN_SIZE=1920x1080`。

## 快速开始

直接启动 `Deskbridge`。

Windows 服务端、macOS 客户端：

1. Windows 端角色选择 `服务端`
2. 监听地址保持 `0.0.0.0:24920`
3. 如果 Mac 在 Windows 屏幕右侧，边缘选择 `右侧`
4. macOS 端角色选择 `客户端`，服务端地址填写 `WINDOWS_IP:24920`
5. 两端点击 `启动`

macOS 服务端、Windows 客户端：

1. macOS 端角色选择 `服务端`
2. 监听地址保持 `0.0.0.0:24920`
3. 如果 Windows 在 Mac 屏幕右侧，边缘选择 `右侧`
4. Windows 端角色选择 `客户端`，服务端地址填写 `MAC_IP:24920`
5. 两端点击 `启动`

默认配置文件位置：

```text
<程序所在目录>/config.ini
```

可以通过 `DESKBRIDGE_CONFIG_DIR` 覆盖配置目录。

如果中文显示成方框，请确认系统存在 CJK 字体，例如 Hiragino Sans GB、
STHeiti、微软雅黑、黑体或 Noto Sans CJK。

## 修饰键映射

当 macOS 作为服务端控制 Windows 时，可以把 macOS 物理键盘上的 Command、
Control、Option 分别映射成 Windows 的 Ctrl、Win、Alt 或禁用。GUI 中在
启动/停止按钮旁边点击 `设置` 即可调整。保存后需要重启服务才会生效。

默认值保持旧版本兼容：

- Command -> Ctrl
- Control -> Ctrl
- Option -> Alt

如果你的使用习惯里 Command 更像 Windows 徽标键，可以把 Command 改成 Win。

## 卸载

Windows 端可以从系统设置或控制面板卸载 Deskbridge。

macOS 端删除：

```text
/Applications/Deskbridge.app
~/.deskbridge
```

macOS 的 app zip 里也包含 `Uninstall Deskbridge.command`。

## 开发者构建

普通用户请直接使用上面的 Release 安装包。开发者仍然可以从源码打包：

macOS：

```bash
./scripts/package_macos.sh
```

Windows：

```powershell
.\scripts\package_windows.ps1
```

Windows 打包需要 Rust、Visual Studio Build Tools、Windows SDK 和 Inno Setup 6。

Linux：

```bash
sudo apt install -y build-essential curl wget file pkg-config libssl-dev \
  libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libwebkit2gtk-4.1-dev
cd web && npm install && npm run build && cd ..
cargo build --release
cargo tauri build --bundles deb,appimage
```

Linux 本地测试时如果遇到 EGL/WebKitGTK 白屏，可以使用保守启动脚本：

```bash
sh scripts/run_linux_safe.sh
```

也可以手动指定：

```bash
DESKBRIDGE_LINUX_GUI_BACKEND=x11 deskbridge gui
DESKBRIDGE_LINUX_GUI_BACKEND=wayland deskbridge gui
DESKBRIDGE_LINUX_SOFTWARE_RENDERING=1 deskbridge gui
```

## 架构

GUI 与服务后端通过进程边界解耦：

- `src/gui.rs` 只负责界面展示和用户交互。
- `src/control.rs` 负责服务生命周期、日志、命令构造和 GUI 文件投递。
- `server` 与 `client` 是独立后端命令，不依赖 GUI 或 `eframe`。
- GUI 停止服务时先通过 stdin 发送 `stop\n`，后端释放按键、鼠标按钮和
  macOS 隐藏光标后再退出，只有超时才会强制终止。

这套命令/stdin 协议与语言无关。未来可以不用 Rust 编写前端，只需启动同一
`server` 或 `client` 命令，设置 `DESKBRIDGE_GUI_CHILD=1`，并在退出时
写入 `stop\n`。

## macOS 权限

macOS 端需要辅助功能权限来注入或捕获键盘和鼠标事件。macOS 服务端模式可能
还需要输入监控权限。

打开：

```text
系统设置 -> 隐私与安全性 -> 辅助功能
```

添加并启用 `Deskbridge.app`。如果从终端运行，则需要给对应终端应用权限。

## 命令行

GUI 内部实际启动的也是这些命令。

Windows 服务端：

```powershell
deskbridge.exe server --bind 0.0.0.0:24920 --edge right
```

macOS 客户端：

```bash
deskbridge client --server WINDOWS_IP:24920
```

macOS 服务端：

```bash
deskbridge server --bind 0.0.0.0:24920 --edge right
```

macOS 服务端，并把 Command 映射为 Win：

```bash
DESKBRIDGE_MAC_COMMAND_MAPPING=win \
deskbridge server --bind 0.0.0.0:24920 --edge right
```

Windows 客户端：

```powershell
deskbridge.exe client --server MAC_IP:24920
```

带滚轮参数的 macOS 客户端：

```bash
DESKBRIDGE_SCROLL_SCALE=1.35 \
DESKBRIDGE_SCROLL_RESPONSE=0.38 \
DESKBRIDGE_SCROLL_MAX_STEP=120 \
DESKBRIDGE_SCROLL_FRAME_MS=8 \
deskbridge client --server WINDOWS_IP:24920
```

## 低延迟建议

尽量使用直连局域网 IP。中继路径会明显影响键盘、鼠标和滚轮响应。

Deskbridge 会合并高频鼠标和滚轮 delta，并让键盘、鼠标按键事件立即发送。
文件传输分块刻意保持较小，避免大文件剪贴板/文件传输长时间占用输入流。

macOS 输入后端使用了一层很小的原生 C shim。它会用 `IOHIDManager` 做 HID
设备发现和诊断，然后通过公开的 CoreGraphics 事件 API 发送合成输入。这个
shim 还会把 `CGEventSource` 的默认本地事件抑制窗口设为 0，以减少合成事件后
出现的短暂停顿。

输入合并窗口默认是 2 ms，两端都可以通过环境变量调节：

```bash
DESKBRIDGE_INPUT_FLUSH_MS=2 deskbridge client --server WINDOWS_IP:24920
```

允许值会被限制在 1-16 ms。如果高回报率鼠标或中继网络下仍然偶发暂停，
可以尝试 3-4 ms。

## 许可证

Deskbridge 使用 GNU General Public License v3.0 开源。详情见
[`LICENSE`](LICENSE)。
