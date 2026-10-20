# CrossDesk 0.2.0 — Windows / Mac 键鼠与图片共享

Windows 11 控制 Apple Silicon Mac，图片剪贴板支持双向复制粘贴。基于 [Deskbridge 1.6.9](https://github.com/paulhe666/deskbridge)，使用 GPL v3。

## 安装与连接

**两台电脑都需要更新到 0.2.0（协议 v5）**。旧版协议 v4 无法连接，新版会提示同时更新两端。已有的 `crossdesk.key` 可以继续使用。

1. Windows 解压 Windows x64 包，双击 `Start Windows.cmd`。首次启动在包目录生成配对文件 `crossdesk.key`，窗口列出 Windows 局域网地址。
2. Mac 解压 Mac ARM64 包，将 Windows 的同一份 `crossdesk.key` 复制到 Mac 包目录，放在 `Start Mac.command` 旁。不要公开或上传这个文件。
3. Mac 双击 `Start Mac.command`，输入 Windows 局域网 IPv4 地址。预编译包不需要安装 Rust。
4. 在 Mac“系统设置 → 隐私与安全性 → 辅助功能”中允许启动它的 Terminal / CrossDesk 条目，按提示重新运行。
5. 两台电脑连接同一局域网。Windows 防火墙弹出提示时允许该专用网络；服务使用 TCP 24920。

如果 Mac 提示未验证开发者，在系统“隐私与安全性”中按系统提供的方式允许打开。包为命令行程序，双击裸 `crossdesk` 只会显示帮助；请用启动脚本。

解压工具未保留执行权限时，在 Mac 包目录运行：

```sh
chmod +x crossdesk "Start Mac.command" scripts/start-mac-binary.sh
./"Start Mac.command"
```

也可以手动运行：

```sh
# Windows（PowerShell）
.\crossdesk.exe server --edge both --key-file crossdesk.key
# Mac
./crossdesk client --server 192.168.1.114:24920 --key-file crossdesk.key
```

将示例 IP 换成 Windows 当前局域网地址。Mac 客户端断线后自动重试；连接建立后终端会显示 `client ready (protocol v5)` 和图片同步状态。

## 图片互传

连接成功后，**重新复制**要传的内容。连接前或断线期间的剪贴板不会自动补发。图片同步与鼠标当前在哪台电脑无关。

| 复制内容 | 另一台电脑如何粘贴 |
|---|---|
| Windows 截图工具中复制截图，或浏览器/图片应用中的“复制图片” | Mac 的聊天、文档或图片应用中按 Command+V |
| Mac 按 Control+Command+Shift+4 截图到剪贴板，或应用中的“复制图片” | Windows 的聊天、文档或图片应用中按 Ctrl+V |
| Finder 选中 PNG/JPG/JPEG 文件后 Command+C | Windows 资源管理器打开目标文件夹，按 Ctrl+V |
| Windows 资源管理器选中 PNG/JPG/JPEG 文件后 Ctrl+C | Finder 打开目标文件夹，按 Command+V |

截图必须进入剪贴板；仅保存截图文件不触发传输。位图粘贴需要目标应用支持图片，文件粘贴需要目标文件夹可写。单张图片文件同时提供图片表示，可以粘贴到支持图片剪贴板的应用中；具体应用兼容性需要在对应应用中验证。程序只复制，不执行跨设备剪切。

- 单张图片或 PNG 传输内容最多 32 MiB；一组最多 32 张、合计 128 MiB；解码最多 32 Mi 像素，宽高各不超过 16384。
- 图片文件保留原始字节和文件名；同组重名会加序号。混选非图片文件、目录或符号链接时整组跳过，终端显示原因。
- 位图以 PNG 传输，支持透明度。使用既有加密连接，16 KiB 分块，键鼠事件可在块间发送。
- 接收完整并通过验证后才更新剪贴板。传输期间新复制的本地内容会保留；被跳过时可以在源端重新复制。
- 收到的图片文件保存在系统临时目录的 `crossdesk-image-clipboard` 下，正常粘贴后由文件管理器复制到目标文件夹。下次连接时清理超过 24 小时且未被当前剪贴板引用的缓存。
- 默认只同步图片。普通文本、其他文件和跨屏拖拽不在此功能范围内。

任一端关闭图片同步后，双方都停止图片同步。手动命令追加：

```sh
--image-clipboard off
```

恢复时使用 `--image-clipboard on` 或省略该选项。不要用旧的实验环境变量 `CROSSDESK_ENABLE_CLIPBOARD` 开启本功能。

## 键鼠共享与退出

| 操作 | 结果 |
|---|---|
| Windows 左外边界向外划 | 进入 Mac 主屏右侧 |
| Windows 右外边界向外划 | 进入 Mac 主屏左侧 |
| Mac 主屏左边界向外划 | 回到 Windows 右侧 |
| Mac 主屏右边界向外划 | 回到 Windows 左侧 |

键盘跟随鼠标，支持点击、滚轮和 Mac 内窗口拖动。跨屏按高度比例映射，Windows 显示器内部接缝不触发切换；Mac 以主显示器为控制范围。按住按钮或 Windows 键盘按键时先保留当前控制权，松开后再向外推动跨屏。

- Windows 按 **Ctrl+Alt+Esc** 或 **Scroll Lock** 立即返回 Windows。
- 两端终端中按 **Ctrl+C** 停止程序。心跳每秒一次，连接读超时 5 秒；断线释放远端输入状态并停止本次图片观察线程。
- Windows Ctrl 对应 Mac Command，Alt 对应 Option。

配对使用随机 256 位密钥，通信通过 Noise NNpsk0 / X25519 / ChaCha20-Poly1305 / SHA-256 加密认证。交付包不含配对密钥。

## 构建、验证与来源

每个预编译包内 `source/` 提供对应的完整项目源码。Mac 源码目录中的 `Build and Start Mac.command` 仍可用于本机编译，需要 Apple Command Line Tools 和 stable Rust。

```sh
cargo fmt --check
cargo test --locked --no-default-features
cargo build --release --locked --no-default-features
```

Mac 原生构建不要设置 `CROSSDESK_CHECK_ONLY`。Windows 与 Mac 原生 CI、制品见 [GitHub Actions](https://github.com/rayem-prog/CrossDesk/actions)。准确的验证范围与双机验收步骤见 `VALIDATION.md`。

保留的上游许可证、版权和修改说明见 `LICENSE`、`THIRD_PARTY_NOTICES.md`、`UPSTREAM_README.md`。
