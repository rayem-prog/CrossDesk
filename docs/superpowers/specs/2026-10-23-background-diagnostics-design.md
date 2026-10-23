# CrossDesk 0.6.2: background operation and local diagnostics

Approved scope: explicit background button, macOS menu bar/Windows tray, login startup disabled by default, local 60-second diagnostics, source-inclusive test packages and draft PR. Window close still fully exits. Protocol v7, pairing and clipboard behavior remain unchanged. Separate input transport is deferred to 0.7.0.

The Rust controller remains the owner of connection and shutdown; hiding only changes window visibility. Tray pause/resume uses existing disconnect/connect commands. Login startup uses the official Tauri plugin and `gui --background`; OS registration is the source of truth. Manual relaunch shows the existing instance. Failed tray setup must not hide the only reachable window.

Diagnostics use existing stage histograms, opt in at runtime through child stdin, and emit bounded structured stdout events. Each run has a parent-generated ID and a child-side monotonic 60-second deadline. A new connection, disconnect or child exit clears metrics; stale run events are ignored. The reporter is independent of the WebView. Input hot paths only touch collection while enabled. Reports contain counts/durations/queue depth, never input values, clipboard contents, network addresses or keys.

The frontend stops polling when hidden and immediately refreshes when shown. Native visibility and document visibility both participate. Export is an explicit save-dialog action. No native input or general clipboard is posted by tests; no currently running app is replaced or restarted.
