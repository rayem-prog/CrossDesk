# Mouse Latency Implementation Plan

> **For agentic workers:** Execute the approved design task by task with test-first changes. GPT-6.1 Sol, medium reasoning; shared checkout with disjoint file ownership.

**Goal:** Reduce ordinary mouse movement scheduling jitter and event backlog without changing v7 or input order.

**Architecture:** A bounded coalescing input queue feeds both endpoints. Windows uses immediate-first pacing and an interruptible high-resolution timer; the Mac consumer applies only the queued batches. Fixed-size opt-in duration histograms report timing without input content.

**Tech Stack:** Rust std synchronization, windows-sys waitable timers, existing encrypted SharedWriter and native input sinks.

## Global Constraints

- Protocol v7, existing trust/clipboard behavior, no new transport channel.
- 256 queue records; merge only adjacent axis-monotonic moves with checked arithmetic. Preserve every control boundary.
- Default 2 ms pacing, no initial motion delay, no busy waiting; Windows high-resolution timer with ordinary timer fallback.
- Stop discards pending work and releases input; overflow ends the session instead of blocking the hook or losing controls.
- Fixed-memory opt-in metrics; no input content; deliver 0.6.1 test builds and a draft PR, without real-device acceptance or automatic app restart.

## Task 1: Ordered bounded input queue and Mac consumer (root)

Files: new `src/input/queue.rs`; modify `src/input/mod.rs`, `src/client.rs`; tests within queue and client.

Interface: `EventQueue::new(usize)`, `push(InputEvent)->io::Result<()>`, `try_pop()->Option<TimedInput>`, `recv()->Option<TimedInput>`, `recv_timeout(Duration)->Option<TimedInput>`, `close()`, `is_closed()`, `len()`; `TimedInput {event,enqueued:Instant,samples:u64}`; `merge_motion(&mut InputEvent,&InputEvent)->bool`.

- [x] Write and run failing tests for ordered coalescing, reversals, edge-clamp equivalence, checked overflow, control barriers, bounded flood, overflow and close wakeup.
- [x] Implement queue with Mutex/Condvar and conservative merging, retain original timestamp/sample count.
- [x] Replace client blocking sync_channel with shared queue; propagate overflow to session shutdown and retain InputSink drop cleanup.
- [x] Inject a fake input boundary in worker tests; verify no startup work, exact event sequence and stop cleanup without native posting.

## Task 2: Windows pacing and timer (Windows agent)

Files: new `src/input/pacing.rs`, `src/input/wait_windows.rs`; source sender section of `src/server/windows.rs`; Windows features in Cargo.toml.

- [x] First write deterministic deadline tests showing first motion is immediate, continuous motion bounded by deadlines, nonmergeable motion/control boundaries flush in order, and close does not replay pending events.
- [x] Replace unbounded sender with EventQueue and an interruptible event/timer wait; native hook only pushes and signals.
- [x] Use CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, fall back to a normal timer, close every handle; add native wait/notification regressions for disposable Windows CI (execution pending delivery).
- [x] Cross-compile with tests and retain all pointer/input regressions.

## Task 3: Fixed-memory timing statistics (metrics agent)

File: new `src/input/latency.rs` only; root registers module. API and call sites coordinated with root/Windows agent.

- [x] Write failing literal-bucket tests for percentile rank, overflow, disabled path and bounded periodic reports.
- [x] Implement aggregate recorder for SendQueue, SendWrite, ReceiveQueue, Inject, PacingLate; bounded histogram and content-free output.
- [x] Root and Windows agent record stage durations and queue depth/sample counts; report at most once every 5 seconds when enabled.

## Task 4: Regression, packaging and delivery (root)

- [x] Run Rust GUI/CLI, receiver/queue/pacing/metrics tests, web regression, formatting and Windows test cross-check.
- [x] Bump own versions to 0.6.1 (keep spake2 0.4.0 and protocol 7), document metrics and timing limits.
- [ ] Push branch based on c7f1f3b, create draft PR based on codex/text-clipboard, attach PR and run native CI.
- [ ] Build Mac app and download Windows installer with matching source packages; record checks actually passed and deliver without physical acceptance.
