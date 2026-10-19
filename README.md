# CrossDesk 0.1.0 — Windows 操控 Mac 原型

针对 Windows 11 主控、M1 Pro Mac 客户端。当前交付鼠标和键盘共享。

| 操作 | 结果 |
|---|---|
| Windows 左外边界向外划 | 进入 Mac 主屏右侧 |
| Windows 右外边界向外划 | 进入 Mac 主屏左侧 |
| Mac 主屏左边界向外划 | 回到 Windows 右侧 |
| Mac 主屏右边界向外划 | 回到 Windows 左侧 |

键盘跟随鼠标；支持点击、滚轮和在 Mac 内拖动窗口。跨屏按高度比例映射，进入时向内偏移 8 个坐标单位，触发不需要停留。Windows 显示器内部接缝不会触发切换，支持负坐标和不同高度排列。当前 Mac 端以主显示器为控制范围。

第一版在按住鼠标按钮或 Windows 上的键盘按键时保留当前机器的控制权；松开后继续向外推动即可跨屏。文件跨机拖拽属于后续阶段。

## Windows 使用

1. 解压 Windows 包，双击 `Start Windows.cmd`。
2. 首次启动在包目录生成 `crossdesk.key`，窗口列出 Windows 局域网地址。
3. 将同一份 `crossdesk.key` 复制到 Mac 源码包根目录，放在 `Build and Start Mac.command` 旁。
4. Windows 防火墙若弹出提示，允许两台电脑所用的专用网络。

服务默认监听 TCP 24920，两台电脑需要能通过局域网互访。Windows 不需要安装 Rust 或 Visual C++ 运行库。

## Mac 使用

1. 解压 `CrossDesk-Mac-source.zip`。
2. 安装 Apple Command Line Tools（`xcode-select --install`）和 stable Rust（https://rustup.rs）。
3. 复制上述密钥文件，双击 `Build and Start Mac.command`，输入 Windows 的局域网 IPv4 地址。
4. 脚本在本机编译 ARM64 客户端。按提示在“系统设置 → 隐私与安全性 → 辅助功能”中允许启动它的 Terminal / CrossDesk 条目，然后重新运行。

如果解压工具未保留可执行权限，在终端运行：

```sh
chmod +x "Build and Start Mac.command"
./"Build and Start Mac.command"
```

此版本不要求开启 SSH，实际输入通过 macOS CGEvent 注入。Mac 操控范围是主显示器；更换主屏或分辨率后重新连接。

## 退出和恢复

- Windows 按 Ctrl+Alt+Esc 或 Scroll Lock：立即释放远端输入并返回 Windows。
- Windows 启动窗口中按 Ctrl+C：停止服务。Mac 终端中按 Ctrl+C：停止客户端。
- 心跳每秒一次，连接读超时 5 秒。断线时释放 Windows 鼠标限制，Mac 客户端释放远端按键和鼠标状态；两端随后等待或重试连接。
- 控制按键映射继承上游：Windows Ctrl 对应 Mac Command，Alt 对应 Option。

配对采用随机 256 位密钥，协议记录通过 Noise NNpsk0 / X25519 / ChaCha20-Poly1305 / SHA-256 加密认证。密钥只保存在本地文件中，包内不附带通用密钥。此原型默认关闭剪贴板与文件共享。

## 构建和验证

```sh
cargo test --locked --no-default-features
cargo build --release --locked --no-default-features
```

本工作区另有 `scripts/dev.ps1`，用于使用旁边的便携 Windows 编译工具链；该工具链不包含在交付包中。Mac 构建不依赖此脚本。

本地 Apple Silicon 检查使用 `CROSSDESK_CHECK_ONLY=1` 跳过原生 C 编译，仅证明 Mac Rust 代码通过类型检查。实际 Mac 构建应不设置此变量，以编译原生 C 并链接 Apple 框架。CI 工作流包含 Windows 和 Mac 原生构建，尚未上传或执行远端 CI。

当前本地验证记录见 `VALIDATION.md`。尚未在用户的 M1 Pro / macOS 27.0.1 上完成原生构建和双机操控验证。

## 来源

基于 Deskbridge 1.6.9 二次开发，保留 GPL v3 和上游版权信息，详见 `THIRD_PARTY_NOTICES.md`、`LICENSE` 和 `UPSTREAM_README.md`。
