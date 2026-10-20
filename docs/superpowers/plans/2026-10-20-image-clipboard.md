# Image Clipboard Implementation Plan

> **For agentic workers:** Execute this approved plan inline, task-by-task, with a failing-test / implementation / passing-test cycle and recorded validation. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Copy screenshots, image content, and PNG/JPG image files between a Windows server and Apple Silicon Mac client through their existing encrypted connection.

**Architecture:** A common image transfer codec bounds and chunks image data. A session-owned clipboard worker polls local changes, publishes only complete incoming transfers, suppresses native-format echoes, and exits with the connection. Platform adapters retain the existing clipboard interface and add revision tracking and native image-first representations.

**Tech Stack:** Rust, existing Noise encrypted stream, image PNG/JPEG/BMP codecs, SHA-256, macOS AppKit pasteboard, Windows clipboard APIs.

## Global Constraints

- Protocol v5; both ends update together; negotiate image capability in Hello.
- 16 KiB chunks; preserve 64 KiB per-frame bound and input priority.
- PNG for bitmap transfer; preserve PNG/JPG/JPEG file bytes and filenames.
- 32 MiB per image, 128 MiB / 32 images per batch; 32 Mi pixels / 128 MiB decoded allocation.
- Reject unsafe names, partial or corrupt transfers, non-image selections, and image decode failures.
- Default image sync on, explicit `--image-clipboard on|off`; do not default-enable text or arbitrary files.
- No stale clipboard send at connection start; no remote echo; no overwrite of a newer local copy.
- Stop all image workers at disconnect. Retain files currently referenced by the clipboard.
- Preserve GPLv3 and attribution. Never claim Windows device validation from a Mac build.

## Task 1: Wire protocol and CLI contracts

**Files:** Modify `src/protocol.rs`, `src/main.rs`, `src/transport.rs`; add `src/image_transfer.rs`; update `Cargo.toml` / lockfile.

**Interfaces:** `Hello.image_clipboard: bool`; `image_transfer::Transfer::from_clipboard(&ClipboardPayload) -> io::Result<Transfer>`; `Transfer::frames() -> io::Result<Vec<Frame>>` is a test adapter only (production iterates chunks); `Incoming::receive(Frame) -> io::Result<Option<Transfer>>`; `FrameKind::{ImageStart,ImageChunk,ImageEnd}`.

- [x] Add failing tests exercising frame rejection/acceptance, CLI invalid option values, and a real >64 KiB image transfer through encrypted TCP.
- [x] Implement v5 capability negotiation, validated on/off option, bounded manifest and offset-checked chunk decoding.
- [x] Normalize bitmap pixels with bounded image decoding and retain verified file bytes. SHA-256 authenticates whole-transfer assembly in addition to the existing encrypted records.
- [x] Assert corruption, missing chunks, repeated/out-of-order offsets, bad names, total limits, and PNG alpha preservation are rejected or preserved as appropriate.
- [x] Run `cargo test --no-default-features image_transfer` and `cargo test --no-default-features protocol`.

Behavioral test shape:

```rust
let source = ClipboardPayload::Bitmap(noisy_png_over_64_kib());
let transfer = Transfer::from_clipboard(&source)?;
let mut incoming = Incoming::default();
for frame in transfer.test_frames()? {
    assert!(frame.payload.len() <= 64 * 1024);
    if let Some(received) = incoming.receive(frame)? {
        assert_eq!(received.pixel_fingerprint()?, transfer.pixel_fingerprint()?);
    }
}
```

The test fixture encodes hand-constructed RGBA pixels, including transparent pixels, and checks decoded output pixels independently of the wire codec.

## Task 2: Session lifecycle and clipboard synchronization

**Files:** Add `src/clipboard/session.rs`; modify `src/clipboard/mod.rs`, `src/client.rs`, `src/server/windows.rs`.

**Interfaces:** `ImageSession::start(writer: SharedWriter, enabled: bool) -> io::Result<ImageSession>`; `ImageSession::receive(&mut self, frame: Frame) -> io::Result<()>`; `ImageSession::handles(FrameKind) -> bool`; dropping a session signals and joins its clipboard worker. `ClipboardApi::change_id(&self) -> io::Result<u64>` supports generation checks.

- [x] Test the common synchronization state with actual image fingerprints: remote A read back as another encoding is not emitted, immediately following local B is emitted.
- [x] Test stale remote completion versus newer local revision and failure-to-publish behavior.
- [x] Implement local baseline capture, bounded incoming queue, atomic full-batch publishing, content deduplication, and session shutdown.
- [x] Route only negotiated image frames through the new session; preserve legacy opt-in mode separately and prevent duplicate legacy/new observers.
- [x] Test stop during polling and transport failure; run the encrypted image-plus-input round trip to verify interleaving and simultaneous directions.

Behavioral state test shape:

```rust
let mut state = SyncState::default();
state.note_remote(fingerprint_a);
assert!(!state.should_send(fingerprint_a));
assert!(state.should_send(fingerprint_b));
assert!(!may_publish_remote(10, 11, None));
```

## Task 3: Platform image and file clipboard adapters

**Files:** Modify `src/clipboard/macos.rs`, `src/clipboard/windows.rs`; share pure image conversions in `src/image_transfer.rs` where useful.

- [x] Add native isolated-pasteboard Mac tests and pure Windows image-format conversion tests before changing adapters.
- [x] Prioritize actual image data over auxiliary text; handle Windows PNG plus DIB and Mac PNG/TIFF/JPEG representations.
- [x] Fix macOS file-list standard output; write image file URLs / CF_HDROP and the single-image bitmap representation together.
- [x] Add revision tracking and ensure malformed images fail before clearing the current clipboard.
- [x] Run native Mac tests; compile-check the Windows target and keep Windows-specific tests in existing CI.

Pixel fixture expectations use literal RGBA pixels, including alpha 0, 127, and 255. File-list tests use two real small images and Chinese names in an isolated temporary directory; no test overwrites the user's general clipboard.

## Task 4: Integration, delivery, and review

**Files:** Update `README.md`, `VALIDATION.md`, `THIRD_PARTY_NOTICES.md`, start scripts and `.github/workflows/crossdesk-input.yml` where needed.

- [x] Document Cmd/Ctrl+C and Cmd/Ctrl+V for image-aware apps and file managers, enabled/disabled behavior, limits, and simultaneous version upgrade.
- [x] Run `cargo fmt --check`, `cargo test --locked --no-default-features`, and `cargo build --release --locked --no-default-features`.
- [x] Compile-check `x86_64-pc-windows-gnu` if available; retain native MSVC tests in GitHub Actions. Record the exact validation boundary.
- [x] Inspect the final diff for accidental text/arbitrary-file synchronization, worker leaks, unchecked allocations, incomplete clipboard publication, and credential inclusion.
- [ ] Package Mac binary plus launch instructions and a two-device acceptance checklist. Record tested commands and any unavailable Windows runtime checks.

Execution uses this session inline. User approval of the design and implementation was received on 2026-10-20; no further implementation approval is required.

Validation so far: 46 native Mac tests passed; `cargo fmt --check`, native Mac release build and Windows GNU target check including tests passed. Native Windows runtime validation and final packages are being produced by CI.
