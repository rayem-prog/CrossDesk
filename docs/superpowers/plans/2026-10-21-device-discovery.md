# Device Discovery and Reconnect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 自动找到 Windows，并在应用重新打开或 IP 改变后连接已经配对的设备。

**Architecture:** mDNS 层只维护候选设备；独立状态机决定是否连接、退避或暂停。信任和长期密钥始终来自 TrustedStore，连接成功由认证后的服务事件确认。

**Tech Stack:** Rust、mdns-sd、已有 ProcessBackend/Noise、确定性时钟状态机测试、假子进程。

## Global Constraints

- 产品目标版本 0.4.0；传输继续使用协议 v6、TCP24920 和现有 Noise 加密。
- 发现只提供地址线索，信任必须来自已保存密钥或当次正确的配对码。
- 主动断开抑制本次运行的自动重连；网络掉线可重试，但不重放离线剪贴板内容。
- 关闭应用必须停止发现、配对监听和服务子进程；不开机自启、不后台常驻。
- 自动化只使用临时数据、假子进程与加密回环连接，不控制用户设备或覆盖日常剪贴板。

---

先完成 [首次配对](2026-10-21-easy-pairing.md)。沿用其 DeviceId、TrustedStore::paired、StoredPeer.key_path 和 PairingService::local_addr；本计划不修改配对算法。

## 文件职责

| 文件 | 职责 |
|---|---|
| src/discovery/{mod,catalog,tests}.rs（新） | 有界候选列表、公告校验、移除过期地址 |
| src/discovery/mdns.rs（新） | 公告、browse、停止与接口变化 |
| src/easy_connect/{mod,policy,tests}.rs（新） | 自动连接/暂停/重试策略 |
| src/service_event.rs（新）、src/control.rs | 结构化服务状态和唯一子进程 |
| src/client.rs、src/server/windows.rs、src/shutdown.rs、src/main.rs | 真实认证边界与退出 |

### Task 1: 设备目录和 mDNS 生命周期

**Files:** Create src/discovery/{mod,catalog,mdns,tests}.rs；Modify Cargo.*、src/main.rs。

**Interfaces:**
- Candidate { id: DeviceId, name: String, endpoints: Vec<SocketAddr>, pairing_port: Option<u16> }。
- Catalog::default()；upsert(candidate: Candidate, deadline: Instant) -> io::Result<()>；expire(now: Instant)；get(id: DeviceId) -> Option<&Candidate>；list() -> Vec<Candidate>。
- Discovery::browse() -> io::Result<Self>；snapshot() -> Vec<Candidate>；announce(id: DeviceId, name: &str, pairing_port: Option<u16>) -> io::Result<Self>；stop(&mut self) -> io::Result<()>。公告端口固定 24920；测试允许注入目录事件，不要求真实组播可用。

- [ ] **Step 1: 添加地址变化与目录上限失败测试。**

```rust
#[test]
fn same_device_replaces_old_address_and_expires() {
    let now = Instant::now();
    let mut c = Catalog::default();
    let mut peer = Candidate { id: [3;16], name: "Windows".into(),
        endpoints: vec!["192.168.1.10:24920".parse().unwrap()], pairing_port: None };
    c.upsert(peer.clone(), now + Duration::from_secs(30)).unwrap();
    peer.endpoints = vec!["192.168.1.114:24920".parse().unwrap()];
    c.upsert(peer, now + Duration::from_secs(30)).unwrap();
    assert_eq!(c.list().len(), 1);
    assert_eq!(c.get([3;16]).unwrap().endpoints[0].ip().to_string(), "192.168.1.114");
    c.expire(now + Duration::from_secs(30));
    assert!(c.list().is_empty());
}
```

- [ ] **Step 2: 运行红灯测试，再实现目录。** 最多 32 设备、每设备 8 地址、名称 128 UTF-8 字节；拒绝控制字符、非法 ID、端口 0、不支持协议和非 Windows 角色。地址更新替换旧地址、ServiceRemoved 删除对应实例；多个实例相同 ID 时合并有界地址集合，不增加信任。
- [ ] **Step 3: 用 mdns-sd 建立适配器。** 创建 Discovery 时启动，stop/Drop 必须 stop_browse 或 unregister 并 shutdown，避免每次轮询开新线程。加入独立 discovery feature，并锁定实际解析版本；GUI 启用该 feature。

```rust
use mdns_sd::{ServiceDaemon, ServiceInfo};
let daemon = ServiceDaemon::new().map_err(std::io::Error::other)?;
let service = ServiceInfo::new(
    "_crossdesk._tcp.local.", "CrossDesk-Windows", "crossdesk-device.local.",
    "", 24920,
    &[("id", device_id_hex.as_str()), ("role", "windows"), ("protocol", "6")][..],
).map_err(std::io::Error::other)?.enable_addr_auto();
daemon.register(service).map_err(std::io::Error::other)?;
```

生产 instance/hostname 加稳定 device_id 后缀避免重名；pairing_port 只在窗口开启时公告。ResolvedService 转为 Candidate 时保留 IPv6 scope，不把 link-local 地址丢成无 scope 的 IpAddr。所有 TXT 先校验长度/版本/角色；广播属性白名单禁止码和 key 字段。mDNS 不可用时返回可读错误并保留手动连接后备入口。

- [ ] **Step 4: 验证重复公告、相同名称不同 ID、伪造 ID、过期/移除、接口地址更新、停止重复调用。** 目录用合成事件测试；本机网络适配测试只在独立/一次性测试环境显式运行，不以公共 CI 是否允许组播决定目录逻辑通过与否。运行 `cargo test --locked --features pairing,discovery`，提交 `feat: discover CrossDesk devices on the local network`。

### Task 2: 唯一连接、真实状态与主动断开

**Files:** Create src/easy_connect/{mod,policy,tests}.rs、src/service_event.rs；Modify src/control.rs、src/shutdown.rs、src/client.rs、src/server/windows.rs、src/main.rs。

**Interfaces:**
- ConnectPolicy::new(trusted: Option<DeviceId>, auto_connect: bool) -> Self；opened()；discovered(id: DeviceId, endpoint: SocketAddr) -> Action；disconnected()；manual_stop()；manual_connect()；tick(now: Instant) -> Action。
- Action = None | Start(SocketAddr) | Stop | RetryAfter(Duration)。网络掉线按 1/2/4/8 秒封顶退避；认证失败设 Error，等待用户重新配对。
- ServiceState = Stopped | Starting | Listening | Connecting | Connected | Retrying | Error；带 peer 地址与错误原因的 ServiceEvent 经 stdout 前缀 crossdesk-event\t + 有界 JSON 发送；serde 仅在启用相关 feature 时加入。
- ControlBackend::connection_state(&self) -> ServiceState；ProcessBackend 保持最多一个 Child；stop/restart 回收后才能再启动。

- [ ] **Step 1: 添加主动断开与未知设备不自动连接的失败测试。**

```rust
#[test]
fn discovery_cannot_undo_a_manual_disconnect() {
    let id = [7;16];
    let address = "192.168.1.114:24920".parse().unwrap();
    let mut p = ConnectPolicy::new(Some(id), true);
    p.opened();
    assert_eq!(p.discovered([8;16], address), Action::None);
    p.manual_stop();
    assert_eq!(p.discovered(id, address), Action::None);
    p.manual_connect();
    assert_eq!(p.discovered(id, address), Action::Start(address));
    assert_eq!(p.discovered(id, address), Action::None);
}
```

- [ ] **Step 2: 实现策略，连接参数仅从 TrustedStore 取得 key_path。** discovered 只对相同可信 ID 调度一次 Start；IP/名称不决定信任。Start 进入 Starting，重复发现不能派生子进程。上次地址最多作为备选；发现八秒仍为空时展示后备入口。认证失败不自动重置密钥。

```rust
pub fn manual_stop(&mut self) {
    self.paused = true;
    self.in_flight = false;
}
pub fn manual_connect(&mut self) { self.paused = false; }
// 调用者收到 manual_stop 时必须同时执行 backend.stop() 并取消待调度重试。
```

- [ ] **Step 3: 添加假子进程生命周期测试，再接 stdout 状态。** 假子进程仅输出结构化状态并读 stdin，不启动输入钩子。测试中进程活着但只发 Listening 不得变 Connected；发 Connected 才转在线。stderr 中含同样前缀不应被解析成事件；坏 JSON/超过 4096 字节事件不崩溃。普通日志最多 500 行，管道读取不得因 UI 慢而无限缓存。
- [ ] **Step 4: 把事件接到真实认证边界。** Windows bind 后 Listening；Mac 发起前 Connecting；双方 Noise + v6 hello 和本地输入/剪贴板初始化完成后 Connected。仅网络异常走重试，配对错误走 Error。GUI 子进程 argv 自动加 --key-file、--image-clipboard on、--file-clipboard on；不在 UI 显示命令预览作为必经步骤。
- [ ] **Step 5: 统一停止。** 一个服务进程只创建一个 stdin 控制读取器；stop/EOF 设置全局停止、关闭当前 socket、Windows 释放控制并 join workers。父进程等两秒仍未退才 kill/wait 并记录强制终止。mDNS tick、重试回调和窗口关闭均检查同一停止状态；十次假进程重启后无旧 PID/reader 残留。
- [ ] **Step 6: 运行策略、假进程、旧 v6 与大文件回归。** `cargo test --locked --features pairing,discovery`；`cargo test --locked --no-default-features`；保留原生 Windows 剪贴板测试与 129 MiB 显式验收。提交 `feat: reconnect remembered devices and respect manual disconnect`。


## Execution record — 2026-10-21

Implementation is in `codex/easy-connect`. Pairing/store/discovery/policy/controller/process and UI regression tests are implemented and run locally. Production orchestration is `src/easy_connect/mod.rs` plus its actor, not the initially proposed controller.rs; test doubles replace network/OS/process boundaries only. Pairing uses real loopback transport and service tests rather than adding pair-host/pair-client CLI commands. Authenticated pending grants can recover a lost final acknowledgment through the existing v6 data channel. Manual disconnect remains authoritative.

Local GUI bundle, native CI and final package verification are recorded in `VALIDATION.md`. Physical Windows/Mac discovery, OS prompts and actual Finder/Explorer paste remain acceptance work on the two computers; browser preview and loopback tests do not substitute for those checks. Checkboxes above retain the original task breakdown instead of marking unrun hardware acceptance complete.
