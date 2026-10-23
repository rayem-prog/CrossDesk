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
- [x] Run CLI/GUI/web/native regressions, packaging checks, and opt-in overhead/idle comparisons without launching the user's live app.
- [x] Commit, push, create/attach a draft PR based on codex/mouse-latency; build separate-path Mac app and native Windows installer with corresponding sources.

No physical two-device acceptance gate, no formal release, no automatic live-app restart. Defer independent input/clipboard connections to a separate 0.7.0 plan informed by diagnostic evidence.

## Delivery evidence

Code commit71c85b15f23e68bbdc7ed9724da570f6d56d4728. Draft PR https://github.com/rayem-prog/CrossDesk/pull/6. GUI native run39946901755 passed Mac208/4ignored and Windows194/5ignored plus native clipboard and explicit large-file tests; PR CLI run39946901973 and duplicate push run39946786076 passed both platforms. Source-inclusive Mac ZIP and Windows NSIS/source ZIP were verified against version0.6.2, commit identities and SHA-256. The Windows source base is CI synthetic merge8c8147f14516a1481501a95c98e144eae0b69c73, whose source bytes match the implementation.

Local synthetic one-million-operation loop comparison, five runs/mode: baseline disabled median40.118ns, new disabled40.985ns, new collecting166.688ns per iteration. This measures queue/metrics operations, not application idle CPU, OS input or end-to-end screen latency. Hidden frontend recurring polls are zero; full native idle CPU and actual login item/tray usability are not claimed from the local automated checks. No physical acceptance gate or formal release was added; current live app stayed untouched.
