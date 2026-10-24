# CrossDesk automatic display alignment

The user authorized implementation of the previously explained automatic coordinate adaptation. This extends the existing proportional edge mapping; it does not change OS display modes, mouse speed, pairing, or clipboard behavior.

## Design choice

Keep the existing proportional vertical entry/return mapping and add live geometry synchronization. Forcing matching OS resolutions would discard usable desktop space; retaining startup-only dimensions would leave stale bounds after display changes. Use the coordinate space of each native input API: DPI-aware Windows desktop coordinates and macOS CoreGraphics logical bounds, including Retina scaling.

Windows remains the input server and Mac the normal client. Main-display selection on Mac and complete-desktop outer edges on Windows retain their existing semantics. This does not add a monitor layout editor or per-monitor speed matching.

## Compatibility and ordering

Protocol v7 retains four-/twelve-byte Hello layouts. Capability bit 0x10 advertises live display updates only on endpoints that implement them. A new ScreenUpdate frame (38) contains a nonzero u64 revision and bounded u32 width/height (1..=32768 each). The client sends only after Hello and only to a capable server. Older peers retain the existing startup geometry and receive no new frames or input tags.

Poll the client's native logical bounds at most every 250 ms from its existing input worker, including when input is idle. On change, release locally injected keys/buttons and suppress stale queued input. Send a revisioned geometry update. Windows handles that update on its owning input/message thread, returns remote control safely, updates its router, and queues a ScreenSync input fence (new input tag 7) after all earlier input. The client resumes only at its exact latest revision fence. A stale fence cannot unlock a newer resize; queued events before the fence cannot act in the new coordinate space. The next edge entry uses the refreshed native bounds.

Windows display-change messages and bounded periodic checks refresh its desktop layout even while control is remote. A changed local layout also returns control and releases input before replacing routing bounds. No reconnect is required. Geometry feedback and its fence work with clipboard synchronization disabled.

## Failures and limits

Reject malformed geometry, zero revisions, nonmonotonic revisions and unnegotiated control frames. Keep queues bounded and existing shutdown/join and emergency-release behavior. Invalid transient native samples retain the last valid geometry. Do not log input values or clipboard contents. Display metadata contains dimensions/revision only.

## Verification and delivery

Write failing tests before implementation: changing either screen, proportion-preserving entry and return, stale-fence suppression, rapid consecutive resizes, bounded geometry and old-peer fallback. Exercise real encrypted frames and the real input queue/router with a recorded native sink; never inject input into the user's desktop. Retain clipboard, edge-drop, input and reconnect regressions. Run native Mac and Windows CI and produce 0.7.1 test packages and a draft PR, without launching/replacing the user's running app or creating a formal release.
