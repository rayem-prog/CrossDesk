# Easy Start Experience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 首次打开选设备输一次码，以后两端打开就连接，让用户直接复制粘贴图片和文件。

**Architecture:** Tauri / React 首页只呈现设备、配对和连接，Rust 负责发现/信任/服务生命周期。高级设置承接手动后备入口，正常流程无需 IP、密钥路径或终端。

**Tech Stack:** 现有 Tauri 2 / React、Rust pairing/discovery、Vitest/Testing Library、原生 Windows/MSVC 与 Mac/ARM64 CI。

## Global Constraints

- 正常连接流程不要求用户输入 IP、端口、密钥路径或终端命令。
- 产品目标版本 0.4.0；传输继续使用协议 v6、TCP24920 和现有 Noise 加密。
- Windows 为键鼠服务端，Apple Silicon Mac 为客户端；图片与普通文件双向同步。
- 关闭应用必须停止发现、配对监听和服务子进程；不开机自启、不后台常驻。
- 自动化只使用临时数据、假子进程与加密回环连接，不控制用户设备或覆盖日常剪贴板。

---

## 执行路线

先依次完成 [首次配对计划](2026-10-21-easy-pairing.md) 和 [设备发现/重连计划](2026-10-21-device-discovery.md)，各自独立测试、提交；再实施本计划。设计：[便捷开启传输](../specs/2026-10-21-easy-connect-design.md)。旧 2026-10-20 GUI 文档仅保留背景，不能继续把 IP 和文件路径表单作为首页前置条件。

## 文件职责

| 文件 | 职责 |
|---|---|
| src/gui.rs、src/easy_connect/controller.rs（新） | 串接发现、配对、可信记录和服务 |
| src/config.rs、src/control.rs | 默认开启能力、持久化自动连接偏好、自动服务参数 |
| src/main.rs、tauri.conf.json、Cargo.*、web/package* | 双击启动、产品与版本、GUI feature |
| web/src/ConnectHome.jsx、ConnectHome.test.jsx（新） | 设备列表、一次输入配对码、连接状态和断开 |
| web/src/AdvancedSettings.jsx（新）、main.jsx、style.css | 次要设置、系统权限引导、错误反馈 |
| scripts/package_gui.py、.github/workflows/crossdesk-gui.yml（新） | GUI 包、对应源码许可证与原生测试 |
| README.md、VALIDATION.md | 三步开始流程及真实验收结果 |

### Task 1: 便捷连接控制器与 GUI 合约

**Files:** Create src/easy_connect/controller.rs；Modify src/gui.rs、src/config.rs、src/control.rs；Test src/easy_connect/tests.rs。

**Interfaces:**
- Controller::open() -> io::Result<Self> 加载私有用户数据，开始发现；Windows 没有配对时开启短时配对窗口，已有配对时自动启动服务。
- Controller::from_parts(platform: &str, store: Arc<TrustedStore>, candidates: Vec<Candidate>, backend: Box<dyn ServiceControl>) -> io::Result<Self> 为 open 的生产组装入口；update_devices(&mut self, candidates: Vec<Candidate>) -> io::Result<()> 接收发现更新。
- ServiceControl: Send 提供 start(&mut self, config: &AppConfig) -> io::Result<()>、stop(&mut self) -> io::Result<()>、state(&self) -> ServiceState；在 src/control.rs 用适配器连接 ProcessBackend。策略不依赖 Tauri 或某个具体进程实现。
- Controller::view(&mut self) -> ConnectView；select_device(id: DeviceId)；pair(code: String) -> io::Result<()>；connect()；disconnect()；shutdown()。
- ConnectView { platform: String, state: String, devices: Vec<DeviceView>, selected_id: Option<String>, pairing_code: Option<String>, seconds_left: u16, auto_connect: bool, error: Option<String> }。
- DeviceView { id: String, name: String, paired: bool }，不带 IP/密钥；Windows pairing_code 只在配对界面使用，不转入通用日志。
- Tauri 命令 get_connect_state / select_device / pair_device / connect_device / disconnect_device 返回 ConnectView；长操作放工作线程，pair code 不进入 argv。

- [ ] **Step 1: 先写冷启动/已配对启动的控制器测试。** 测试依赖 Discovery 与 ProcessBackend 的内存适配器，真实存储用临时目录；对网络和进程边界进行替换，不 mock 策略本身。

```rust
struct SilentBackend;
impl ServiceControl for SilentBackend {
    fn start(&mut self, _: &AppConfig) -> io::Result<()> { Ok(()) }
    fn stop(&mut self) -> io::Result<()> { Ok(()) }
    fn state(&self) -> ServiceState { ServiceState::Starting }
}
#[test]
fn remembered_device_starts_connecting_but_manual_stop_stays_paused() {
    let mut random = [0u8;16]; getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-controller-{random:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let peer = Candidate { id:[9;16], name:"Windows".into(),
        endpoints:vec!["127.0.0.1:24920".parse().unwrap()], pairing_port:Some(24921) };
    let mut first = Controller::from_parts("macos", store.clone(), vec![peer.clone()], Box::new(SilentBackend)).unwrap();
    assert_eq!(first.view().state, "discovering");
    assert!(first.view().selected_id.is_none());
    first.shutdown();
    store.stage(peer.id, &[8;32]).unwrap(); store.commit(peer.id).unwrap();
    let mut remembered = Controller::from_parts("macos", store, vec![peer.clone()], Box::new(SilentBackend)).unwrap();
    assert_eq!(remembered.view().state, "connecting");
    assert!(remembered.view().devices[0].paired);
    remembered.disconnect();
    remembered.update_devices(vec![peer]).unwrap();
    assert_eq!(remembered.view().state, "paused");
    remembered.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
```

生产 open 创建真实 discovery 与 ProcessBackend，再调用同一个 from_parts；测试不增加生产中的测试专用构造器。额外的已连接测试使用可注入 ServiceEvent 的测试后端，必须等 Connected 事件才显示 connected。

- [ ] **Step 2: 观察失败后接入前两个子项目。** 读取配对状态、默认自动连接 true；只有存储中 Paired 记录可以自动启动。首次 pair 完成触发真实服务启动，密钥路径取内部存储；Windows role 固定 Server、Mac 固定 Client，地址由发现更新。图片/文件开关默认 true，已有显式用户 off 偏好保留。
- [ ] **Step 3: 将阻塞任务和停止统一。** Tauri 所有网络/配对/子进程操作在工作线程；视图请求读取快照，不锁住十秒握手。点击断开中止配对/重试但保留已配对信息；关闭窗口等待 Controller::shutdown 回收所有资源。并发 pair/connect 请求返回已有任务状态，不再开启任务。
- [ ] **Step 4: 加入假时钟验收。** IP 更新、两台候选、错误码、存储失败、配对超时、主动断开、已有开关关闭、无网络均有明确状态。run `cargo test --locked --features gui`，提交 `feat: orchestrate the simple connect flow`。

### Task 2: 三步首页与双击启动

**Files:** Create web/src/ConnectHome.jsx、ConnectHome.test.jsx、AdvancedSettings.jsx；Modify web/src/main.jsx、style.css、package*、src/main.rs、src/gui.rs、tauri.conf.json、Cargo.*。

**Interfaces:**
- ConnectHome({ view, busy, onSelect, onPair, onConnect, onDisconnect, onRefresh })。onSelect(id: String)、onPair(code: String)、其余无参数；异步失败显示 view.error。
- 首次 Windows：“在 Mac 上输入此配对码”；首次 Mac：“选择你的 Windows”→六位数字输入→“配对并连接”；已配对显示设备名、状态与断开/连接。
- AdvancedSettings 承接手动地址、旧密钥导入、两个同步开关和日志；进入它不是正常连接的必要步骤。

- [ ] **Step 1: 加入前端失败测试与依赖锁。** 使用 Vitest/jsdom/Testing Library；mock 仅限 Tauri IPC，组件和输入验证真实执行。

```jsx
import { render, screen } from '@testing-library/react';
import { test, expect, vi } from 'vitest';
import userEvent from '@testing-library/user-event';
import { ConnectHome } from './ConnectHome.jsx';
test('首次连接不要求 IP 或密钥文件', async () => {
  const select = vi.fn();
  render(<ConnectHome view={{ platform:'macos', state:'discovering',
    devices:[{id:'a'.repeat(32),name:'我的 Windows',paired:false}] }}
    busy={false} onSelect={select} onPair={vi.fn()} onConnect={vi.fn()}
    onDisconnect={vi.fn()} onRefresh={vi.fn()} />);
  expect(screen.queryByLabelText(/IP|端口|密钥路径/)).toBeNull();
  await userEvent.click(screen.getByRole('button',{name:'我的 Windows'}));
  expect(select).toHaveBeenCalledWith('a'.repeat(32));
});
```

- [ ] **Step 2: 运行 `npm --prefix web test` 观察组件缺失，再实现首页。** 六位数字外禁止提交；错误码保留已选设备，允许改码；过期展示刷新配对码入口；后台发现更新不能清掉用户正在输入的码。已连接时显示“可以开始复制粘贴”，不要求再点击开启文件传输。

```jsx
const validCode = /^\d{6}$/.test(code);
<button disabled={busy || !validCode} onClick={() => onPair(code)}>配对并连接</button>
// 用户点击断开后等待后台状态回到 paused/stopped；按钮显示“连接”。
// 连接过程中禁用重复提交，但保留取消/断开入口。
```

- [ ] **Step 3: 添加已配对自动连接和权限反馈测试。** 已配对视图没有码/IP 表单；get_connect_state 轮询串行且卸载后停止；Mac 权限拒绝时显示系统设置引导，Windows 发现失败时提示同网络/专用网络允许，超过八秒显示刷新和高级入口。所有错误经文本节点呈现。最小窗口下主按钮、状态和码完整可见，支持键盘与高对比焦点。
- [ ] **Step 4: 接好真正的应用入口。** GUI feature 无参数直接 gui::run；server/client/keygen/--help/--version 保留；headless 构建无参数仍帮助。GUI feature 自动启用 pairing、discovery；三处产品版本统一 0.4.0，产品名 CrossDesk、identifier local.crossdesk.app。Windows GUI 构建使用 windows_subsystem 避免额外控制台；子进程管道照常工作。旧更新链接指向 rayem-prog/CrossDesk，保留来源声明。
- [ ] **Step 5: 运行前后端测试与 build，提交 `feat: open CrossDesk and connect without manual setup`。**

### Task 3: 运行包与“无需教程”验收

**Files:** Create scripts/package_gui.py、.github/workflows/crossdesk-gui.yml；Modify scripts/package_macos.sh、scripts/package_windows.ps1、README.md、VALIDATION.md。

**Interfaces:** package_gui.py --artifact PATH --platform Mac-ARM64|Windows-x64 --output DIR；打包完整 .app 或 Windows 安装程序，加对应源码、GUI/配对/发现依赖源码和许可证；排除密钥与用户数据。

- [ ] **Step 1: 加产品与打包失败断言，证明旧 Deskbridge 配置/无 gui 构建不能通过。**

```python
import json, pathlib, tomllib
root=pathlib.Path('.')
tauri=json.loads((root/'tauri.conf.json').read_text())
cargo=tomllib.loads((root/'Cargo.toml').read_text())
assert tauri['productName']=='CrossDesk'
assert tauri['version']==cargo['package']['version']=='0.4.0'
assert {'pairing','discovery'}.issubset(set(cargo['features']['gui']))
```

- [ ] **Step 2: 更新原生打包。** npm ci；Mac Tauri build --features gui --bundles app，Windows --features gui --bundles nsis。去掉旧 deskbridge.exe 路径和会删除上游数据的卸载逻辑；交付应用双击即首页，不让用户选择 .command 或裸命令。

```sh
npm --prefix web ci
npm --prefix web test
npm --prefix web run build
./web/node_modules/.bin/tauri build --features gui --bundles app
# Windows：& .\web\node_modules\.bin\tauri.cmd build --features gui --bundles nsis
```

- [ ] **Step 3: 原生 CI 同时覆盖新旧功能。** 两端跑 gui feature Rust 测试、前端测试/build、原生 bundle 和打包；保留无 GUI 测试、Windows 原生剪贴板、129 MiB 流传输。对应源码收集 cargo metadata 必须启用 gui/pairing/discovery；ZIP 验证 CRC、执行权限、COMMIT、SHA256 和无 .key。Mac 未公证、Windows 无可信签名时如实记录。
- [ ] **Step 4: 对照设计的六条验收逐条执行并记录。** 特别检查首次不查 IP、不搬 key、不用终端；再次打开自动连接；地址变更仍连接；手动断开不回弹；配对错码不入库；真实图片/PDF 双向粘贴。未运行的物理双机项目标明待验收，不用 mock 测试结果代替。
- [ ] **Step 5: README 首屏只写两条流程。** “第一次：打开两端，选 Windows，输入一次码。以后：打开两端，直接复制粘贴。”系统授权和异常排查紧随其后，技术参数放高级章节。提交 `build: deliver easy start CrossDesk packages`。

## 完成标准与交接

主路径中没有 IP/端口/文件路径表单，没有脚本和手动服务步骤；配对只做一次，后续连接与传输开关自动恢复。旧 GUI 计划中的手动配置不得作为本次完成标准。

执行可选子代理逐项实施并审查，或当前会话顺序实施。当前尚未启动实现；开始时先建立失败测试，再按三个子项目依次提交、验证。
