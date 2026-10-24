# Automatic display alignment implementation plan

**Goal:** Keep cross-screen mouse coordinates correct when either computer changes resolution or scaling during a connection.

**Architecture:** A revisioned client geometry update is acknowledged by an ordered input fence. Native input is released and gated during resynchronization. Windows applies all capture/layout changes on its existing input thread.

**Tech stack:** Rust, protocol v7, existing Noise stream/EventQueue/PointerRouter, CoreGraphics and Win32.

## Global constraints

- Preserve Hello lengths 4/12, pairing, protocol v7, clipboard/edge-drop behavior and existing input pacing.
- Capability 0x10 gates frame 38 and input tag 7; legacy peers receive neither.
- Dimensions 1..=32768; revision nonzero and increasing; 250 ms maximum client poll frequency.
- Coordinate adaptation is automatic; OS display modes and mouse speed stay user-controlled.
- Windows capture/native calls stay on its input thread; native input tests use a recorded sink.

## Task 1: Geometry codec and ordered client gate

Files: `src/display_sync.rs`, `src/protocol.rs`, `src/main.rs`, `src/transport.rs`.

Interfaces:
```rust
pub struct ScreenUpdate { pub revision: u64, pub width: u32, pub height: u32 }
impl ScreenUpdate {
    pub fn frame(self) -> crate::protocol::Frame;
    pub fn decode(payload: &[u8]) -> std::io::Result<Self>;
}
pub struct ClientAlignment { /* private state */ }
impl ClientAlignment {
    pub fn new(size: (u32,u32), enabled: bool) -> Self;
    pub fn observe(&mut self, size: (u32,u32)) -> Option<ScreenUpdate>;
    pub fn filter(&mut self, event: crate::protocol::InputEvent) -> Option<crate::protocol::InputEvent>;
}
```

- [x] Add a failing test: after `(1920,1080)` changes to `(1512,982)`, revision 1 is sent, old MouseDelta/Key are suppressed, only `ScreenSync { revision: 1 }` reopens the gate. A second change before that fence requires revision 2.
- [x] Run `cargo test --locked --no-default-features display_sync` and observe failure before production implementation.
- [x] Implement bounded exact codec, capability decoding and gate; retain old Hello layouts and reject malformed input tags.
- [x] Run codec/gate/protocol and transport tests; add an encrypted queue/router regression for a stale-key boundary and legacy negotiation.

## Task 2: Client native observation

Files: `src/client.rs`, `src/input/mod.rs`, native input sinks.

- [x] First exercise the input driver with queued old input, idle polling and a recorded sink; verify it cannot emit feedback before Hello.
- [x] Extend the existing queue driver with an idle tick that wakes immediately on real input, retaining latency measurements and coalescing boundaries.
- [x] Start geometry observation after successful client Hello. On change, locally apply MouseLeave, send ScreenUpdate via SharedWriter and filter input until the exact ScreenSync. Refresh native bounds on the next MouseEnter.
- [x] Export native current screen-size queries; other native sinks ignore the protocol-only fence because the driver consumes it.
- [x] Run `cargo test --locked --features gui client` and the full display-sync suite without native injection.

## Task 3: Windows capture thread and router

Files: `src/server/windows.rs`, `src/routing.rs`, `src/desktop.rs`.

- [x] First test router entry after resize: local `(1000,800)`, remote changes from `(1200,900)` to `(1600,1000)`, local bottom maps to remote y999 and entry x8; return at top maps to y0.
- [x] Add `PointerRouter::update_remote_size((i32,i32))` with bounded internal positions. Detect actual DesktopLayout changes, not just dimensions.
- [x] Pass negotiated support to the reader. Validate monotonically increasing ScreenUpdate, store the latest in one bounded slot and post an owning-window message. The reader performs no cursor/native capture calls.
- [x] On the owning thread, deactivate old control, update remote bounds, enqueue `InputEvent::ScreenSync { revision }` in the existing FIFO input emitter. A fence is sent even when already local.
- [x] Handle WM_DISPLAYCHANGE/WM_SETTINGCHANGE and refresh layout at most once/second during motion, including remote motion. Release before changing geometry.
- [ ] Run portable routing/desktop tests, Windows cross-check and native Windows CI. Retain emergency release and drag boundaries.

## Task 4: Integration and delivery

Files: current version manifests/lockfiles, README.md, VALIDATION.md, package artifacts.

- [x] Complete real encrypted loopback tests for resize, rapid resize, old-peer fallback, input-only mode, and ordered suppression of stale input. No clipboard or OS input writes.
- [x] Run formatting, GUI/CLI/web regressions, explicit release large-file regression and package-source check.
- [x] Bump owned app versions to 0.7.1, document automatic geometry and safe return behavior; preserve historical evidence.
- [ ] Commit/push `codex/display-alignment`, create/attach draft PR stacked on `codex/edge-drop`, run native CI and verify both source-inclusive test packages and checksums. Do not formally release or restart the live app.
