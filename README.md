# CrossDesk 0.4.0 — 打开两端，复制粘贴

Windows 与 Apple Silicon Mac 共享键鼠，双向复制图片和普通文件。基于 [Deskbridge](https://github.com/paulhe666/deskbridge)，保留 GPL v3。

## 开始使用

**第一次：** Windows 安装并打开 CrossDesk，Mac 将 `CrossDesk.app` 拖入“应用程序”后打开。在 Mac 上选择发现的 Windows，输入 Windows 窗口中的六位配对码，点击“配对并连接”。

**以后：** 两台电脑打开 CrossDesk，自动连接；看到“可以开始复制粘贴”，就能直接复制截图、图片或文件，在另一台粘贴。默认开启图片与文件同步。不需要终端、IP 地址或搬运密钥文件。

- 两端使用 0.4.0 GUI 应用，并连接同一局域网。Windows 为键鼠服务端，Mac 为客户端；文件/图片传输双向。
- Mac 首次使用需要允许“本地网络”和“系统设置 → 隐私与安全性 → 辅助功能”中的 CrossDesk。授权后点击“连接设备”，必要时重新打开应用。
- Windows 首次防火墙提示时允许当前使用的专用网络。访客 Wi-Fi / AP 隔离可能阻止设备互相发现。
- 配对码 120 秒有效；失效后 Windows 点击“重新生成配对码”。错误码不会建立信任。首次配对成功后不用再次输入。
- 点击“断开连接”后，本次运行保持断开；点击“连接设备”恢复。关闭应用会停止服务和发现，不在后台常驻。
- 设置按钮中可关闭自动连接、图片或文件同步，查看日志，或忘记设备后重新配对。修改同步选项前先断开。

这些构建尚无可信发布签名 / Apple 公证。Mac 若提示未验证开发者，请按系统“隐私与安全性”的允许打开流程处理；不要关闭系统安全防护。Windows 安装程序可能显示 SmartScreen 提示。开发者本地构建与 CI 安装包不等于正式 GitHub Release。

## 发现失败与旧版迁移

先检查两端已打开、网络相同、本地网络/防火墙权限已允许，然后刷新。仍失败时可在“高级设置 → 手动连接与旧版迁移”中指定 Windows IP 和旧版 `crossdesk.key`。已有其他配对时需先忘记设备；新 GUI 配对材料自动存入每用户应用数据目录，不放在安装目录。

局域网发现使用 mDNS UDP 5353，临时配对使用 TCP 24921，数据使用原有 TCP 24920 / 协议 v6。公告只提供设备地址，不构成信任；六位码通过 SPAKE2、HKDF 和 Noise 确认后交付随机长期密钥。采用的 RustCrypto SPAKE2 库说明其实现未经独立安全审计，本项目也不声称已审计。只支持一组 Windows/Mac 配对。

命令行与 0.3.0 v6 数据通道继续保留；正常 GUI 使用无需以下命令：

```sh
crossdesk keygen --out crossdesk.key
crossdesk server --edge both --key-file crossdesk.key
crossdesk client --server WINDOWS_IP:24920 --key-file crossdesk.key
```

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
| 窗口中的“断开连接” / 关闭应用 | 停止连接 / 完全退出 |

键盘跟随鼠标，Windows Ctrl 对应 Mac Command，Alt 对应 Option。支持点击、滚轮、Mac 内拖动；按住键或按钮时保留控制权，松开后再跨屏。Mac 以主显示器为控制范围，Windows 内部显示器接缝不切换。

复用 Noise NNpsk0 / X25519 / ChaCha20-Poly1305 / SHA-256 加密配对连接；文件块为 16 KiB，确认后再发下一块，键鼠、心跳及控制帧优先。数据传输继续使用 TCP 24920。文件复制与鼠标当前在哪台电脑无关。

## 验证与构建

Node.js 24、Rust stable，分别在原生 Windows / Apple Silicon Mac 上构建；Windows 需要 MSVC 构建工具，GUI 使用 WebView2。

```sh
npm --prefix web ci
npm --prefix web test
cargo fmt --check
cargo test --locked --no-default-features
cargo test --locked --features gui
# Mac 应用与含源码压缩包
bash scripts/package_macos.sh
# Windows PowerShell 安装程序与含源码压缩包
# powershell -File scripts/package_windows.ps1
```

原生 [GUI CI](https://github.com/rayem-prog/CrossDesk/actions/workflows/crossdesk-gui.yml) 同时构建 Mac `.app` 与 Windows NSIS 安装程序，并执行配对、发现策略、界面、剪贴板和 129 MiB 文件测试。自动测试的范围和待做双机验收见 [VALIDATION.md](VALIDATION.md)。交付包包含 `source/`、Rust 依赖源码、Web 依赖许可、提交编号和 SHA-256 校验文件。

来源与修改说明：[LICENSE](LICENSE)、[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)、[UPSTREAM_README.md](UPSTREAM_README.md)。[GitHub 项目](https://github.com/rayem-prog/CrossDesk)。
