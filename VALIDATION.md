# CrossDesk 0.1.0 validation — 2026-10-19

Environment: Windows 11 x64; Rust 1.99.1; x86_64-pc-windows-gnu.

- Windows release executable built locally.
- 27 tests passed: both-edge routing, all four transitions, fast edge crossing,
  vertical mapping, small dimensions, negative and staggered desktop coordinates,
  internal monitor joins, held-button behavior, emergency release state,
  protocol validation, malformed keys, pairing mismatch, ciphertext tampering,
  concurrent encrypted writers, large encrypted stream framing, key-file
  overwrite protection, and Windows native hook/window registration and cleanup.
- The native hook test installs no capture state and does not move or constrain
  the user's cursor.
- Apple Silicon Rust target passed type checking with native C compilation
  skipped explicitly using CROSSDESK_CHECK_ONLY=1.
- The Windows executable imports only Windows system DLLs; no extra runtime DLLs
  are needed. Its help command ran outside the developer toolchain environment.
- Clippy completed successfully with style suggestions in retained upstream code.
- Reproduced Windows error 10035 with an accepted nonblocking socket. The added
  regression passes after restoring blocking mode before the encrypted handshake,
  including delayed handshake data and a delayed client hello. Read/write timeouts
  remain enabled.

Pending real-device checks:

- Native macOS C compilation/linking and Accessibility permission behavior.
- Actual pointer, button, scroll, text/IME and window dragging on M1 Pro/macOS 27.0.1.
- Physical unplug, Wi-Fi loss, sleep/wake, and reconnect behavior across both machines.

There is no claim of completed execution on the user's Mac or dual-machine validation.
Remote Windows and macOS builds run through [GitHub Actions](https://github.com/rayem-prog/CrossDesk/actions);
consult the individual run for its status. The checks above describe local validation.
Cross-machine file dragging is outside this input prototype.
