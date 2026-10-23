# CrossDesk 0.6.2 implementation plan

Goal: background operation and one-click local diagnostics, preserving close-to-exit and protocol v7.

## Interfaces

- Native: `hide_to_background`, `get_desktop_state`, `set_start_at_login(enabled)`, `start_input_diagnostics`, `stop_input_diagnostics`, `export_input_diagnostics`.
- Desktop state: camelCase `backgroundAvailable`, `startAtLogin`, `error`.
- Child commands: `diagnostics start RUN_ID`, `diagnostics stop`; `stop` and EOF retain shutdown semantics.
- Diagnostic events: `crossdesk-diagnostics\t` followed by bounded JSON. Run IDs correlate events to requests. Stage names: send_queue, send_write, receive_queue, inject, pacing_late. Snake-case diagnostic fields are shared by UI and export.
- Existing ConnectView gains `diagnostics`; ServiceControl supplies default no-op/unsupported methods for existing test backends.

## Tasks

- [x] Test lifecycle rules before implementing tray/autostart. Keep native actions behind a boundary and never register login items during tests.
- [x] Test capture deadline, opt-in, reset and report validation before implementing the diagnostic core and input integration.
- [x] Test real stdin/stdout correlation and disconnect cleanup before integrating ProcessBackend/Core.
- [x] Test visible UI behavior, login errors, empty metrics and visibility polling before implementing frontend.
- [x] Add GUI `--background` parsing and bump own versions to 0.6.2; keep protocol and dependency versions except required plugin.
- [ ] Run CLI/GUI/web/native regressions, packaging checks, and opt-in overhead/idle comparisons without launching the user's live app.
- [ ] Commit, push, create/attach a draft PR based on codex/mouse-latency; build separate-path Mac app and native Windows installer with corresponding sources.

No physical two-device acceptance gate, no formal release, no automatic live-app restart. Defer independent input/clipboard connections to a separate 0.7.0 plan informed by diagnostic evidence.
