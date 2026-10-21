# Easy Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用户只输入一次六位码，程序自动安全交换并保存长期连接信息。

**Architecture:** 私有数据存储与短时配对监听独立于现有传输服务。SPAKE2 引导出仅当次使用的 Noise 密钥，长期随机密钥在完成确认的加密连接中交付。配对失败不替换旧信任记录。

**Tech Stack:** Rust、spake2 0.4.0、HKDF/SHA-256、现有 snow/Noise、用户目录私有文件、TCP 回环测试。

## Global Constraints

- 配对码六位数字、120 秒有效、最多五次尝试、同一窗口最多一个握手；成功、取消、过期后立即关闭配对窗口。
- 六位码仅用于 PAKE 引导，不能明文发送，不能直接哈希后充当长期配对密钥。
- 长期密钥为 32 字节随机数，保存在用户数据目录；配对码、长期密钥不得进入配置日志、发现广播、UI 日志或交付包。
- 第一版只管理一组 Windows/Mac 配对；新增设备必须经首次配对流程，已有配对不被静默替换。
- 自动化只使用临时数据、假子进程与加密回环连接，不控制用户设备或覆盖日常剪贴板。

---

## 文件与接口边界

| 文件 | 职责 |
|---|---|
| src/pairing/mod.rs、window.rs、window_tests.rs（新） | 短码窗口、尝试次数和取消 |
| src/trust_store.rs、src/trust_store/tests.rs（新） | 稳定 ID、长期密钥、待确认/已配对记录 |
| src/pairing/wire.rs、transport.rs、tests.rs（新） | 有界消息、PAKE、Noise 确认、加密密钥交付 |
| src/secure.rs | 暴露现有 TcpStream 握手和按路径读密钥；保留旧连接 |
| src/main.rs、Cargo.* | pairing feature 与独立测试入口 |

具体消息、流程及验收依据：[便捷开启设计](../specs/2026-10-21-easy-connect-design.md)。本计划仅生产配对能力；设备发现和 GUI 分别有后续计划。

### Task 1: 配对窗口与用户私有存储

**Files:** Create src/pairing/{mod,window,window_tests}.rs、src/trust_store.rs、src/trust_store/tests.rs；Modify src/main.rs、Cargo.*。

**Interfaces:**
- DeviceId = [u8; 16]，本机生成一次并持久化；UI 显示名称不是身份。
- PairingWindow::new(now: Instant, code: [u8; 6]) -> io::Result<Self>；admit(now: Instant) -> bool；finish(success: bool)；cancel()。admit 先检查时间/取消/并发，再占用一次尝试；finish 释放并发槽，成功消费窗口。
- TrustedStore::new(root: PathBuf) -> io::Result<Self>；local_id() -> io::Result<DeviceId>；host_key() -> io::Result<[u8; 32]> 自动创建且不覆盖；stage(peer: DeviceId, key: &[u8;32]) -> io::Result<()>；commit(peer: DeviceId) -> io::Result<()>；paired() -> io::Result<Option<StoredPeer>>。
- StoredPeer { id: DeviceId, key_path: PathBuf } 只在内部使用；UI DTO 不携带该路径/密钥。

- [ ] **Step 1: 写窗口失败测试。**

```rust
#[test]
fn expired_or_exhausted_pairing_window_cannot_accept() {
    let now = Instant::now();
    let mut w = PairingWindow::new(now, *b"123456").unwrap();
    for _ in 0..5 { assert!(w.admit(now)); w.finish(false); }
    assert!(!w.admit(now));
    let mut w = PairingWindow::new(now, *b"123456").unwrap();
    assert!(!w.admit(now + Duration::from_secs(120)));
    let mut w = PairingWindow::new(now, *b"123456").unwrap();
    assert!(w.admit(now));
    assert!(!w.admit(now));
    w.finish(true);
    assert!(!w.admit(now));
}
```

- [ ] **Step 2: 注册测试模块并运行 `cargo test --locked --features pairing expired_or_exhausted_pairing_window_cannot_accept`，预期窗口类型尚不存在而失败。** 新 feature pairing 仅启用 spake2、hkdf、serde/serde_json；保持 --no-default-features 的旧构建。
- [ ] **Step 3: 加入窗口状态实现和无偏六位码生成。** 新码从 getrandom 的 u32 拒绝采样后模 1_000_000，格式化到六位；不得使用时间或进程 ID。码和窗口不 derive Debug/Serialize；只允许专用方法向配对视图返回码。

```rust
pub fn admit(&mut self, now: Instant) -> bool {
    if self.closed || self.busy || now >= self.expires || self.remaining == 0 { return false; }
    self.remaining -= 1;
    self.busy = true;
    true
}
pub fn finish(&mut self, success: bool) {
    self.busy = false;
    self.closed |= success;
}
pub fn cancel(&mut self) { self.closed = true; }
```

- [ ] **Step 4: 写持久化失败测试，再实现存储。** 测试模块使用随机命名临时目录，不调用真实用户路径。

```rust
#[test]
fn stage_is_not_trust_and_host_key_survives_restart() {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-trust-{random:x?}"));
    let store = TrustedStore::new(root.clone()).unwrap();
    let original = store.host_key().unwrap();
    store.stage([7;16], &original).unwrap();
    assert!(store.paired().unwrap().is_none());
    store.commit([7;16]).unwrap();
    assert_eq!(store.paired().unwrap().unwrap().id, [7;16]);
    let reopened = TrustedStore::new(root.clone()).unwrap();
    assert_eq!(reopened.host_key().unwrap(), original);
    assert_eq!(reopened.local_id().unwrap(), store.local_id().unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
```

实现固定相对文件名和 create_new；禁止把远端名称拼进路径。元数据写到同目录临时文件，sync_all 后原子替换；Windows 使用能替换目标的原生文件替换操作，首次创建与替换分开处理。stage 不能替换不同的已配对设备；写失败保持旧记录。Unix 应用目录 0700、密钥 0600；Windows 用当前用户 AppData。添加损坏元数据、权限不足、重复 stage/commit 和 symlink 路径拒绝测试。

- [ ] **Step 5: 运行配对窗口/存储测试及旧测试，提交 `feat: add expiring pairing window and private trust store`。**

### Task 2: 短码引导与加密交付

**Files:** Create src/pairing/{wire,transport,tests}.rs；Modify src/secure.rs。

**Interfaces:**
- secure::from_tcp(stream: TcpStream, psk: &[u8;32], initiator: bool) -> io::Result<SecureStream> 包装现有 handshake，保持其超时和模式恢复。
- secure::load_key_from(path: &Path) -> io::Result<[u8;32]> 复用原 decode_key；旧 load_key 委托此函数。
- PairContext { client: DeviceId, host: DeviceId, nonce: [u8;16] } 实现 Clone；其两个身份串按固定字段顺序编码，并带 CrossDesk/pair/1 域名。
- run_host(stream: TcpStream, context: PairContext, window: &mut PairingWindow, store: &TrustedStore) -> io::Result<()>；run_client(stream: TcpStream, context: PairContext, code: &[u8;6], store: &TrustedStore) -> io::Result<()>。
- wire::read_message(reader: &mut impl Read) -> io::Result<Vec<u8>> 和 write_message(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()>；u16 长度，1..=4096 字节。

- [ ] **Step 1: 写纯 PAKE/Noise 测试与失败的真实回环测试。** 首先固定身份并检查同码生成相同材料；错误码即使 finish 返回材料，也必须在 Noise 确认阶段失败，且不能 commit。

```rust
#[test]
fn same_code_derives_the_same_bootstrap_secret() {
    use spake2::{Ed25519Group, Identity, Password, Spake2};
    let a = Identity::new(b"CrossDesk/pair/1/client/test");
    let b = Identity::new(b"CrossDesk/pair/1/host/test");
    let (left, lm) = Spake2::<Ed25519Group>::start_a(&Password::new(b"123456"), &a, &b);
    let (right, rm) = Spake2::<Ed25519Group>::start_b(&Password::new(b"123456"), &a, &b);
    assert_eq!(left.finish(&rm).unwrap(), right.finish(&lm).unwrap());
}
```

在 tests.rs 用 TcpListener::bind("127.0.0.1:0") 配成两端，临时 store 执行 run_host/run_client。成功应同 key/对端 ID；错误码、nonce/目标 ID 被修改、截断消息、接收超长记录均返回 Err，paired() 仍为空；这些断言在实现前必须失败。

```rust
#[test]
fn wrong_code_never_saves_a_paired_device() {
    let mut random = [0u8;16]; getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-pair-{random:x?}"));
    let host = Arc::new(TrustedStore::new(root.join("host")).unwrap());
    let client = TrustedStore::new(root.join("client")).unwrap();
    let context = PairContext { host:host.local_id().unwrap(),
        client:client.local_id().unwrap(), nonce:[4;16] };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let host_store = host.clone(); let host_context = context.clone();
    let worker = std::thread::spawn(move || {
        let mut window = PairingWindow::new(Instant::now(), *b"123456").unwrap();
        run_host(listener.accept().unwrap().0, host_context, &mut window, &host_store)
    });
    let result = run_client(std::net::TcpStream::connect(address).unwrap(), context, b"654321", &client);
    assert!(result.is_err()); assert!(worker.join().unwrap().is_err());
    assert!(client.paired().unwrap().is_none());
    assert!(host.paired().unwrap().is_none());
    std::fs::remove_dir_all(root).unwrap();
}
```

- [ ] **Step 2: 明确线协议后加入 bounded codec。** 连接先发八字节 CDPAIR01；清文只包含版本 1、双方 ID、随机窗口 nonce、SPAKE2 的 A/B 消息和受限设备名。每条消息长度不超过 4096，设备名不超过 128 UTF-8 字节，未知版本立即拒绝。每个 socket 的读/写上限两秒，总握手不超过十秒且不能超过窗口到期时间。分配缓冲区前校验长度。
- [ ] **Step 3: 使用既有库完成引导。** A 固定为 Mac、B 固定为 Windows；双方 transcript 为按线序拼接的 hello/A/B 原始字节。将 SHA256(transcript) 用作 HKDF salt；双方使用完全相同的域串。

```rust
let mut bootstrap = [0u8; 32];
hkdf::Hkdf::<sha2::Sha256>::new(Some(&transcript_hash), &shared)
    .expand(b"CrossDesk/pair/1/Noise bootstrap", &mut bootstrap)
    .map_err(|_| std::io::Error::other("pairing key derivation failed"))?;
let mut channel = crate::secure::from_tcp(stream, &bootstrap, initiator)?;
```

只有 Noise 握手成功且窗口仍有效时，才允许发送包含随机长期密钥的 Grant。加密通道内消息顺序固定 Grant(host_id,client_id,key32) → Stored(client_id) → Complete(host_id)。Mac 先 stage，Windows 收到 Stored 后 commit 并发 Complete，Mac 收到 Complete 后 commit；任一步失败不把 Pending 显示为已配对。已有配对保留，重试可复用 host_key。所有秘密类型禁用 Debug，错误只含阶段/原因。

- [ ] **Step 4: 增加重放、反射和落盘失败的负向测试。** 旧 nonce、B 发给 B、成功后的同码第二次连接、磁盘只读、Stored 前断开、Complete 前断开不得误报成功。将所有明文发送字节录到测试缓冲，断言其中没有六位码和 key32；不要在失败断言中打印秘密。
- [ ] **Step 5: 运行 `cargo test --locked --features pairing` 和 `cargo test --locked --no-default-features`，提交 `feat: pair devices through PAKE and confirmed Noise channel`。** 将 spake2 未独立审计这一库限制记入 VALIDATION，不使用“已审计”措辞。

### Task 3: 有生命周期的配对服务与独立验证入口

**Files:** Create src/pairing/service.rs；Modify src/pairing/mod.rs、src/main.rs；Test src/pairing/tests.rs。

**Interfaces:**
- PairingService::start(bind: SocketAddr, store: Arc<TrustedStore>) -> io::Result<Self>；view() -> PairingView；stop(&mut self) -> io::Result<()>；Drop 调用 stop。
- PairingView { code: Option<String>, seconds_left: u16, completed: bool, error: Option<String> } 仅返回给配对 UI，禁止写入通用日志。
- CLI 测试入口 pair-host / pair-client；pair-host 向交互终端展示短码，pair-client 从 stdin 读取，不把码放进 argv。GUI 直接调用 Rust 服务，不启动这些交互入口。

- [ ] **Step 1: 写取消服务测试。** 绑定 127.0.0.1:0，记录实际端口，stop 后该端口能立即重新绑定；Drop/120 秒到期/五次失败也退出所有 worker。已有窗口重复 start 返回 AlreadyExists，不叠加监听。

```rust
#[test]
fn stopping_pairing_service_closes_its_listener() {
    // store 使用 Task 1 同样的随机临时目录构造并在结尾删除。
    let mut random = [0u8;16]; getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!("crossdesk-service-{random:x?}"));
    let store = Arc::new(TrustedStore::new(root.clone()).unwrap());
    let mut service = PairingService::start("127.0.0.1:0".parse().unwrap(), store).unwrap();
    let address = service.local_addr();
    service.stop().unwrap();
    assert!(std::net::TcpListener::bind(address).is_ok());
    std::fs::remove_dir_all(root).unwrap();
}
```

另定义 local_addr(&self) -> SocketAddr 作为生产状态/广播所需端口接口；不是测试专用后门。

- [ ] **Step 2: 实现一个监听器、一个受限握手任务与退出句柄。** 非阻塞 accept 每 50 ms 检查 stop/expiry；不能为每个未认证 socket 无限开线程。停止时关闭握手 socket、join worker、清空码。默认真实配对端口 24921；测试端口由调用者指定。
- [ ] **Step 3: 接入 CLI 入口并做回环验收。** 现有 keygen/server/client 不变，新增命令受 pairing feature 控制。任何 CLI 输出码仅用于交互模式，GUI 不读取或记录此 stdout。现有旧 .key 由 load_key_from 导入私有 host_key，源文件不删除。
- [ ] **Step 4: 运行测试与格式检查，提交 `feat: manage short lived pairing service`。** 本子项目独立验收结束，随后实施设备发现计划。


## Execution record — 2026-10-21

Implementation is in `codex/easy-connect`. Pairing/store/discovery/policy/controller/process and UI regression tests are implemented and run locally. Production orchestration is `src/easy_connect/mod.rs` plus its actor, not the initially proposed controller.rs; test doubles replace network/OS/process boundaries only. Pairing uses real loopback transport and service tests rather than adding pair-host/pair-client CLI commands. Authenticated pending grants can recover a lost final acknowledgment through the existing v6 data channel. Manual disconnect remains authoritative.

Local GUI bundle, native CI and final package verification are recorded in `VALIDATION.md`. Physical Windows/Mac discovery, OS prompts and actual Finder/Explorer paste remain acceptance work on the two computers; browser preview and loopback tests do not substitute for those checks. Checkboxes above retain the original task breakdown instead of marking unrun hardware acceptance complete.
