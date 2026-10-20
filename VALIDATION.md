# CrossDesk 0.2.0 validation — 2026-10-20

## Automated validation

Local environment: Apple Silicon, macOS 27.0.1, Rust 1.98.1.

- 48 native Mac tests pass. `cargo fmt --check`, Windows target type checking (including tests), and the native Mac release build pass.
- Native macOS build compiles the C input backend and links Apple frameworks; `CROSSDESK_CHECK_ONLY` is not set.
- The common transfer tests use real PNG/JPEG bytes, encrypted TCP connections, images larger than 64 KiB, simultaneous transfers in both directions, and interleaved input/heartbeat frames.
- Tests cover alpha/pixels, Chinese names, exact file bytes, duplicate names, count/size limits, unsafe paths, mixed selections, interrupted transfers, offsets, digests and invalid images.
- The real clipboard worker is exercised with an in-memory OS boundary over encrypted TCP: startup content is not sent, a remote write is not echoed, the next local image is sent, and the worker stops and joins.
- macOS adapter tests use named NSPasteboards, leaving the user's general clipboard untouched. They check PNG alpha/pixels, multiple real file URLs, a single file's image representation, stale-write rejection, and safe rejection of file names that collide under macOS Unicode normalization.
- Windows Rust code and tests are compile-checked for `x86_64-pc-windows-gnu`. Native Windows format conversion, hidden owner-window lifecycle and clipboard publication have tests configured in the MSVC CI job. Native Windows execution has not been run in this session. The clipboard test is ignored in ordinary local test runs and runs explicitly only on the disposable GitHub runner.
- Windows x64 GNU release executable also links successfully on this Mac using Rust LLD, Homebrew MinGW-w64 14.0.0 (GCC 16.2.0 target libraries), and Zig 0.15.2 for import-library generation. Its PE32+ / AMD64 format and import table were inspected: only Windows system DLLs are imported, with no separate GCC/pthread DLL requirement. This is a cross-build, not a Windows execution test.
- Existing encrypted pairing, version/CLI validation, input routing, socket blocking-mode and input cleanup regressions remain in the suite.

GitHub publishing is currently unavailable in this session: CLI Git has no push credentials and the connected GitHub integration returned HTTP 403 for writes. No new CI run has been triggered. Once the branch is pushed, status and paired CI builds will appear in [GitHub Actions](https://github.com/rayem-prog/CrossDesk/actions). A successful Mac test or Windows cross-check alone does not establish Windows runtime or physical two-device behavior.

## Physical two-device acceptance checklist

Use both 0.2.0 builds with the same private key. Confirm `protocol v5` and `image clipboard: on` in the terminal.

- Copy a screenshot to the Windows clipboard; paste into a Mac image-aware app. Repeat from Mac to Windows. Check size, colors and transparency with a transparent PNG.
- Copy a PNG and a JPEG with Chinese names in Explorer; paste both into a Finder folder. Repeat in the other direction and compare file bytes/names.
- Copy a single image file; paste into both a destination folder and an image-aware app. Check the specific chat/editor apps you use.
- While a large image transfers, move/type on the Mac through CrossDesk. Confirm input remains usable and both terminals stay connected.
- During an incoming transfer, copy a different local image. Confirm the new local copy is preserved. Copy again at the source if the remote completion was skipped.
- Copy two different images rapidly after receiving an image; confirm the final local copy sends and there is no repeating echo traffic.
- Interrupt Wi-Fi during transfer. Confirm no partial image is published, remote input releases, the image worker stops, and reconnect creates one worker. After reconnect, copy again to send.
- Start either end with `--image-clipboard off`; confirm image sync is off in both directions while keyboard/mouse sharing works.
- Try a mixed PNG/text-file selection and an oversized image; confirm the selection is skipped without disturbing input or the existing destination clipboard.
- Stop with Ctrl+C and test emergency input release with Ctrl+Alt+Esc or Scroll Lock on Windows.

Actual hardware/app compatibility, physical Wi-Fi loss, sleep/wake, Accessibility permission prompts and pointer/IME behavior require these device checks. Automated tests do not claim they have been completed.

## Prior baseline

Version 0.1.0 was originally built and tested on Windows 11 x64. The baseline includes the fix restoring accepted sockets to blocking mode before the encrypted handshake, with delayed-handshake regression coverage. Cross-device file dragging remains outside the implemented feature.

## Commands recorded for this delivery

```sh
cargo fmt --check
cargo test --locked --no-default-features
cargo build --release --locked --no-default-features
cargo check --locked --no-default-features --target x86_64-pc-windows-gnu --tests
```

The local Windows link used `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER` pointing to Rust's `rust-lld`, and target rustflags `-C linker-flavor=ld.lld -C link-self-contained=yes -C dlltool=<Zig dlltool wrapper>`, plus `-L native=<MinGW x64 CRT directory>` and `-L native=<GCC x64 runtime directory>`. The ordinary Windows native build needs none of these cross-build overrides. MinGW target libraries came from the official Homebrew bottle and its SHA-256 was verified before extraction.
