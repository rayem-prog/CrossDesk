# File Clipboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Fix six reviewed defects and deliver bidirectional ordinary-file clipboard transfer.

**Architecture:** One clipboard observer per session dispatches image or ordinary-file sends. A bounded independent file receiver performs streaming disk I/O, acknowledges each request to apply backpressure, and queues only verified completed batches for clipboard publication. Native publication receipts and readback checks distinguish remote writes from new local copies.

**Tech Stack:** Rust, existing Noise encrypted TCP, SHA-256, Windows CF_HDROP, macOS NSPasteboard.

## Global Constraints

- Single file <=512 MiB; batch <=2 GiB, <=64 regular files; zero-byte files supported.
- Files stream in 16 KiB chunks; per-direction file buffers <8 MiB.
- Both peers use protocol v6 and version 0.3.0; same key and TCP24920.
- Files and images have independent on/off capabilities; text stays opt-in legacy only.
- Never publish partial files, overwrite collisions, or delete referenced completed caches.
- Keep newer local copies; explicit recopy retries; no offline replay, directories or resume.
- Reject symlinks/reparse points; byte counts and hashes enforced on receiver independently.
- Do not overwrite the user's general clipboard during automated Mac tests or start a live service.

---

### Task 1: Clipboard publication and recopy regressions

Files: `src/clipboard/{mod,session,windows,macos}.rs`.
- [x] Import the two review regressions; run `cargo test review_ -- --test-threads=1` and observe failure.
- [x] Introduce publication revision receipts and verify readback; an explicit changed revision bypasses content-only suppression.
- [x] Pass new regressions plus existing native named-pasteboard tests, then commit.

```rust
assert!(state.should_send(local_fingerprint)); // new local B must not become remote echo
assert!(second_transfer.is_ok()); // explicit recopy A must send again
```

### Task 2: Safe legacy receiver and common file primitives

Files: `src/file_transfer.rs`, `src/file_transfer/tests.rs`, existing legacy consumers.
- [x] Import four file regressions and observe failure.
- [x] Make completion fallible; enforce count/size, isolate batches, use exclusive filename allocation and remove partial files on abort.
- [x] Add streaming source handles and file identity checks; reject symlinks/reparse and unsafe names.
- [x] Verify exact PDF/ZIP/binary/empty bytes, duplicate names, interruption and bounds, then commit.

```rust
assert!(incoming.finish().is_err()); // 3/10 bytes is not a completed file
assert!(previously_published.exists()); // next batch cannot erase it
```

### Task 3: File wire protocol and bounded transport worker

Files: `src/file_transfer/{wire,session}.rs`, `src/protocol.rs`, `src/transport.rs`.
Interfaces: `FileSender::send(&[PathBuf], cancel)`; `FileSession::receive(Frame, revision)`; `CompletedBatch` guards cache lifetime until publication.
- [x] Add failing tests for offer/data/seal/commit, exact offsets/digests, sender ACK timeout and cancellation.
- [x] Add v6 file capability and typed request/ACK/result/cancel frames; ACK after disk processing, never queue unlimited bytes.
- [x] Test both directions concurrently over encrypted loopback TCP, input/heartbeat interleaving and closed transport shutdown.
- [x] Run regressions and commit.

```rust
assert_eq!(sha256(received_file), sha256(original_file));
assert!(receiver.commit().is_err()); // wrong hash cannot publish
```

### Task 4: Unified clipboard integration

Files: `src/clipboard/session.rs`, `src/client.rs`, `src/server/windows.rs`, `src/main.rs`.
- [x] Add failing capability/CLI/worker tests, including file-only mode and mixed file selection.
- [x] Route ordinary files via file sender and verified completions via observer; image behavior remains available independently.
- [x] Return published/skipped/failed results, cancel superseded sends, join workers, protect local revisions.
- [x] Verify off switches, repeated copy, new local copy races, session exit; commit.

```rust
assert!(parse_args(&args("server --file-clipboard off")).is_ok());
```

### Task 5: Native adapters and delivery

Files: platform clipboard adapters, `Cargo.*`, launchers, package script, README, VALIDATION, CI.
- [x] Add named Mac ordinary-file/mixed-file adapter tests and compile Windows native tests.
- [x] Preserve single-image representation only for supported small image files; never read arbitrary huge files solely to create an image representation.
- [x] Run format/native tests/Windows target check/release builds; prepare Mac ARM64 and Windows x64 archives with corresponding source and dependency licenses.
- [ ] Verify package contents exclude keys and match commit/binaries; record exact physical validation limits and commit.

Commands: `cargo fmt --check`; `cargo test --locked --no-default-features`; `cargo check --locked --no-default-features --target x86_64-pc-windows-gnu --tests`; native and cross release build; package integrity checks.

## Execution evidence — 2026-10-20

Implemented inline after explicit user authorization. 78 native Mac tests pass; 129 MiB encrypted streaming acceptance passes separately in release mode; Windows code/tests compile and release links on Mac. Mac named-pasteboard tests include origin receipts and Unicode file identity. Remaining physical Windows/Mac acceptance is documented in VALIDATION.md rather than marked tested.

Private unique staging directories replace the planned `.part` suffix: their paths are never published until full batch verification. Final platform package verification and artifact paths are recorded with delivery. No new dependencies were required.
