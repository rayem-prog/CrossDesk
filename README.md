# CrossDesk 0.3.0 — Windows / Mac 键鼠、图片与文件共享

Windows 11 控制 Apple Silicon Mac，图片和普通文件支持双向复制粘贴。基于 [Deskbridge 1.6.9](https://github.com/paulhe666/deskbridge)，使用 GPL v3。

## 安装与连接

**两台电脑都更新到 0.3.0 / 协议 v6**。0.2.0 / v5 和更早版本无法连接；已有 `crossdesk.key` 可以继续使用。

1. Windows 解压完整 Windows x64 包，双击 `Start Windows.cmd`。首次启动生成配对文件 `crossdesk.key`，窗口列出 Windows 局域网地址。
2. Mac 解压完整 Mac ARM64 包，将 Windows 的同一份 `crossdesk.key` 放在 `Start Mac.command` 旁。不要上传或公开此文件。
3. Mac 双击 `Start Mac.command`，输入 Windows 局域网 IP。预编译包不需要安装 Rust。
4. Mac 在“系统设置 → 隐私与安全性 → 辅助功能”允许启动程序的 Terminal / CrossDesk，按提示重启程序。
5. 两台电脑连接同一局域网；Windows 防火墙提示时允许使用的专用网络，端口 TCP 24920。

Mac 提示未验证开发者时，使用系统“隐私与安全性”提供的允许打开方式。双击裸 `crossdesk` 只显示帮助，请使用启动脚本。解压未保留权限时，在 Mac 包目录运行：

```sh
chmod +x crossdesk "Start Mac.command" scripts/start-mac-binary.sh
./"Start Mac.command"
```

手动启动：

```sh
# Windows PowerShell
.\crossdesk.exe server --edge both --key-file crossdesk.key
# Mac：将示例 IP 替换为 Windows 当前局域网地址
./crossdesk client --server 192.168.1.114:24920 --key-file crossdesk.key
```

连接后应显示 `protocol v6`、`image clipboard: on`、`file clipboard: on`。

## 普通文件双向互传

连接后**重新复制**要传的文件，等待目标端显示 `file clipboard received: ... ready to paste`，或源端显示 `destination clipboard result: published; ready to paste`。

| 复制位置与操作 | 另一端如何粘贴 |
|---|---|
| Windows 资源管理器选中 PDF、DOCX、XLSX、ZIP 等，Ctrl+C | Mac Finder 打开目标文件夹，Command+V |
| Mac Finder 选中普通文件，Command+C | Windows 资源管理器打开目标文件夹，Ctrl+V |
| 选中 PNG/JPG 与 PDF 等多个普通文件一起复制 | 另一端文件夹粘贴整组文件 |

- 支持任意扩展名的普通文件、零字节文件、中文名和多选，保留原始内容。跨设备剪切按复制处理，不删除源文件；接收不自动打开或执行文件。
- 单文件最多 **512 MiB**，每批最多 **2 GiB / 64 个文件**，两端都执行限制。
- 不支持目录、符号链接、Windows reparse points、尚未落地的云盘占位文件。混选这些内容时整组跳过；先将文件下载到本地，再重新复制。
- 文件名需可同时用于 Windows 和 Mac：不能含控制字符、路径分隔符、Windows 保留名称或尾部点/空格；单名不超过 255 UTF-8 字节。组内同名和文件系统等价名称会添加序号，不静默覆盖。
- 内容分块读取、加密传输、分块落盘；每个文件验证长度与 SHA-256，整组完成后才更新剪贴板。无需将整个文件加载到内存。
- `transmitted ... waiting for destination publication` 表示已传完，**不等于已更新目标剪贴板**。目标端有更新的本地复制时会跳过发布，源端收到 `skipped ... copy again to retry`。重新复制相同文件也会建立新任务。
- 在源端复制新内容会取消尚未完成的旧文件发送；断线清理本次未发布的暂存文件。重连后重新复制，从头发送，不自动补发断线期间的内容。
- 文件暂存于系统临时目录 `crossdesk-file-clipboard` 的独立批次目录。新批次不会删除旧批次；当前剪贴板引用的缓存受到保护。会话启动时清理超过 24 小时且未被当前剪贴板引用的缓存。
- 文件缓存预算为 4 GiB；达到预算会拒绝新批次并给出原因。已经粘贴到目标文件夹的内容由文件管理器保存；清理缓存前应完成粘贴并清除剪贴板里的旧文件引用，避免在文件管理器仍复制时清理。

任一端手动命令加 `--file-clipboard off`，双方停止普通文件同步；使用 `--file-clipboard on` 或省略该选项恢复。普通文本不在此功能范围内，不要用旧实验环境变量 `CROSSDESK_ENABLE_CLIPBOARD` 开启文件互传。

## 图片双向互传

| 复制内容 | 另一端如何粘贴 |
|---|---|
| Windows 截图工具/浏览器的“复制图片” | Mac 聊天、文档、图片应用，Command+V |
| Mac Control+Command+Shift+4 截图到剪贴板，或“复制图片” | Windows 支持图片的应用，Ctrl+V |
| 单张 PNG/JPG/JPEG 文件 | 另一端文件夹，或支持图片剪贴板的应用 |

截图必须复制到剪贴板；只保存到磁盘不触发同步。位图以 PNG 传输，保留透明度；单图表示限制为 32 MiB、32 Mi 像素，宽高不超过 16384。单张图片文件超过图片表示限制时仍可作为普通文件粘贴到文件夹。

图片可用 `--image-clipboard off` 单独关闭。关闭普通文件同步后，图片模式仍可处理符合图片限制的 PNG/JPG 文件（最多 32 张、128 MiB）；关闭两项则停止本次剪贴板同步。目标聊天/文档应用是否接受图片，仍需对相应应用验证。

## 键鼠与退出

| 操作 | 结果 |
|---|---|
| Windows 左外边界向外划 | 进入 Mac 主屏右侧 |
| Windows 右外边界向外划 | 进入 Mac 主屏左侧 |
| Mac 主屏左/右边界向外划 | 回到 Windows 对应另一侧 |
| Windows Ctrl+Alt+Esc 或 Scroll Lock | 紧急返回 Windows |
| 两端终端 Ctrl+C | 停止程序 |

键盘跟随鼠标，Windows Ctrl 对应 Mac Command，Alt 对应 Option。支持点击、滚轮、Mac 内拖动；按住键或按钮时保留控制权，松开后再跨屏。Mac 以主显示器为控制范围，Windows 内部显示器接缝不切换。

复用 Noise NNpsk0 / X25519 / ChaCha20-Poly1305 / SHA-256 加密配对连接；文件块为 16 KiB，确认后再发下一块，键鼠、心跳及控制帧优先。不增加端口。文件复制与鼠标当前在哪台电脑无关。

## 验证、构建与来源

```sh
cargo fmt --check
cargo test --locked --no-default-features
cargo test --release --locked --no-default-features large_file_stream_exceeds_image_batch_limit_without_buffering_the_file -- --ignored
cargo build --release --locked --no-default-features
```

验证范围与双机验收清单见 `VALIDATION.md`。交付包提供 `source/` 对应项目源码及 `third-party-source/` 依赖源码和许可证。Mac 原生构建不要设置 `CROSSDESK_CHECK_ONLY`。

来源与修改说明：`LICENSE`、`THIRD_PARTY_NOTICES.md`、`UPSTREAM_README.md`。代码托管与原生 CI：[rayem-prog/CrossDesk](https://github.com/rayem-prog/CrossDesk)。
