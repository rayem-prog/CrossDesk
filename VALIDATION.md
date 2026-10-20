# CrossDesk 0.2.0 validation — 2026-10-20

## Automated validation

Local environment: Apple Silicon, macOS 27.0.1, Rust 1.98.1.

- Native macOS build compiles the C input backend and links Apple frameworks; `CROSSDESK_CHECK_ONLY` is not set.
- The common transfer tests use real PNG/JPEG bytes, encrypted TCP connections, images larger than 64 KiB, simultaneous transfers in both directions, and interleaved input/heartbeat frames.
- Tests cover alpha/pixels, Chinese names, exact file bytes, duplicate names, count/size limits, unsafe paths, mixed selections, interrupted transfers, offsets, digests and invalid images.
- The real clipboard worker is exercised with an in-memory OS boundary over encrypted TCP: startup content is not sent, a remote write is not echoed, the next local image is sent, and the worker stops and joins.
- macOS adapter tests use named NSPasteboards, leaving the user's general clipboard untouched. They check PNG alpha/pixels, multiple real file URLs, a single file's image representation, and stale-write rejection.
- Windows Rust code and tests are compile-checked for `x86_64-pc-windows-gnu`. Native Windows format conversion, hidden owner-window lifecycle and clipboard publication are tested by the MSVC CI job. The clipboard test is ignored in ordinary local test runs and runs explicitly only on the disposable GitHub runner.
- Existing encrypted pairing, version/CLI validation, input routing, socket blocking-mode and input cleanup regressions remain in the suite.

Exact CI status and downloadable paired builds are available in [GitHub Actions](https://github.com/rayem-prog/CrossDesk/actions). A successful Mac test or Windows cross-check alone does not establish Windows runtime or physical two-device behavior.

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
