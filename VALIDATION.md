# CrossDesk 0.6.0 validation — 2026-10-22

## Current implementation and verification

Plain-text clipboard transfer is memory-only and defaults on independently of images/files. Protocol remains v7, with bit 0x04 negotiated in Hello and TextStart/Chunk/End/Cancel frames 24–27. No text frame is sent to a 0.5.0 peer. Both local and remote options gate the common session; the primary endpoints reject the legacy raw Clipboard message and no longer start its experimental watcher.

Text is normalized to LF without trimming; Windows publishes CRLF using CF_UNICODETEXT. Normalized UTF-8 is limited to 1 MiB, chunks to 16 KiB, the existing frame cap to 64 KiB. Empty, NUL, malformed encoding, noncanonical wire newlines, bad offsets/identities/lengths/hashes and oversize input are rejected as whole transfers. SHA-256 verification precedes native publication. Each direction holds one text assembly; a separate expiry worker discards assemblies after 30 seconds without progress even while the clipboard worker is sending files. Disconnect releases unfinished memory. No text history/cache or body logging is added.

The existing common publication queue and native revision lineage protect newer local copies across text/images/files. Explicit identical text recopy creates a new ID; confirmed remote publication advances observation without echoing. Native publication validates before clearing, checks the current revision and obtains an origin/revision receipt. macOS tests use named pasteboards; Windows clipboard runtime tests execute only on disposable GitHub runners.

Automated tests cover normalization and whitespace, Chinese/emoji, 1 MiB and multiframe UTF-8 boundaries, invalid native/wire text, idle timeout, cancellation, bidirectional encrypted publication, no startup or reconnect replay, same-content retry, newer local copy protection, cross-format lineage, legacy capability downgrade while files continue, text-only mode, preference migration, CLI validation, structured capability/transfer IPC lifecycle and accessible UI states. Local final run: GUI Rust 167 passed / 3 ignored (subprocess fixtures and the explicit large-file test); CLI Rust 129 passed / 2 ignored; React 25 passed; source-packaging regression 1 passed. Native Mac tests (14 named-pasteboard cases), formatting and Windows target checking including tests pass. The existing Windows-only unused_mut warning is unchanged. Native CI and package results will be recorded in the delivery build record.

## Physical two-device text acceptance — pending

1. Install both 0.6.0 test packages using existing pairing. Copy Chinese/emoji, multiline code, indentation, leading/trailing spaces and blank lines in both directions into Notepad, TextEdit (plain text) and a code editor.
2. Verify LF/CRLF behavior, repeated identical copies, empty copy and oversize rejection without clearing the target. Copy locally during incoming text and confirm the newer target copy survives.
3. Alternate screenshots, folders and text; interrupt Wi-Fi and reconnect. Old contents must not replay; a fresh copy must work.
4. Disconnect, turn text off on either end, reconnect, and verify neither direction sends text while images/files still work. Enable only text and verify both directions. Repeat with a 0.5.0 peer and verify the truthful unavailable hint with existing file/folder transfer.

These hardware and target-application checks have not been claimed as passed. No merge or formal release occurs before actual two-device acceptance.

---

## Prior 0.5.0 baseline

# CrossDesk 0.5.0 validation — 2026-10-22

## Current implementation

Protocol v7 carries a parent-first tree (type, parent index, portable basename, length), in ordered bounded manifest pages. Both peers independently enforce 64 roots, 10,000 entries, 32 levels, 512 MiB/file, 2 GiB/batch, 4 MiB/manifest, 64 KiB/frame and 4 GiB recursive file-cache budget. Files are streamed and SHA-256 checked; empty directories survive without synthetic file data. All content is staged before publishing top-level native clipboard paths. Unpublished batches own and remove the entire staging root; published earlier batches survive subsequent failures.

Source scanning opens/checks/closes files individually; sending holds one source handle. Directory/file metadata and identities are rechecked before/after transfer. Links, reparse points, unmaterialized cloud placeholders, inaccessible entries and unsafe names reject the batch. This is best-effort change detection, not a filesystem snapshot; no permissions, xattrs or link relations are migrated.

Separate bounded transfer IPC supplies preparing, transferring, waiting, ready, skipped and failed notices; these cannot establish or end a connection. Terminal publication notices cannot regress to waiting if a fast peer publishes before the sender finishes handling its commit ACK. Commit credit is granted only after the completed batch is queued for publication. A bounded native-revision lineage guard additionally prevents a late older batch crossing an intervening local-copy boundary after a newer remote image has published.

Discovery advertises v7 and displays incompatible peers with an upgrade hint. Selection, pairing and reconnect reject them, including a saved-address fallback after a known incompatible peer disappears. Existing trust store, keys, identities and preferences are unchanged.

## Automated verification

- Real encrypted bidirectional folder + mixed file transfer checks nested/empty directories, hidden files, Chinese names, zero-byte files and exact contents. A separate encrypted multi-page manifest transfers a file after hundreds of directories.
- Adversarial manifests exercise bad parents, cycles/forward references, files used as parents, traversal, invalid lengths, count/depth/size limits, partial metadata cancellation and a declared manifest above 4 MiB.
- Native scanning covers exact limits and one-over-limit, source additions/deletions/replacement, cancellation and Unix links. Windows-only tests use a real offline attribute and directory junction.
- Cache/receiver tests cover recursive quota, canonical-equivalent names and renamed subtrees, retained older batches, corruption, serial/offset errors, cancellation, timeouts and cleanup on drop.
- Clipboard observer tests cover folders, no echo, identical recopy, newer local content and the cross-content late-completion regression. macOS native tests use named NSPasteboards. Native file paths use NUL delimiters so a control-character filename is preserved as one invalid item for whole-batch rejection, rather than being split into different source paths. Windows system-clipboard tests are ignored except on disposable GitHub CI runners.
- UI tests cover each transfer state, direction-specific waiting, failure while connected, disconnect without a paired fixture, incompatible devices and unchanged options flow. Controller tests cover actual mDNS discovery plus incompatible-peer recovery and bounded remembered versions.

Local verification: `cargo test --locked --features gui` **135 passing / 2 ignored**; `cargo test --locked --no-default-features` **103 passing / 1 ignored**; React **18 passing**; source-packaging regression **1 passing**; release-mode **129 MiB encrypted transfer passes**. `cargo fmt --check`, `git diff --check` and Vite production build pass. Native CI/package identifiers are recorded with the final build results. No live CrossDesk GUI or connection to the user's devices is started by validation. No test intentionally modifies this Mac's general clipboard.

## Windows native source-identity regression

The initial Windows native run caught same-length source replacement that preserved both timestamps during a fast rename. Metadata lengths/times alone did not distinguish the two files. Source nodes now also capture the Windows volume serial and file index from an open handle, and compare both the path's current handle and the actual sending handle during verification. Directory handles use backup semantics and source opens use OPEN_REPARSE_POINT. The original native regression remains unchanged; Windows CI reruns validate the fix. The Windows GUI run also exposed cached directory write times after a child was added. Directory verification now re-enumerates and compares exact bounded child-name lists, in addition to metadata and identity checks, so added/removed entries cannot rely on delayed directory timestamps. Unix continues to compare device/inode and ctime. Detection remains best-effort rather than a filesystem snapshot.

## Physical Finder / Explorer acceptance — pending

1. Install both 0.5.0 test packages. Retain the existing pairing. Confirm both show connected and file/folder sync enabled.
2. Copy a folder containing nested and empty directories, hidden files, zero-byte files and Chinese/accented names. Paste in the other file manager. Repeat both ways and compare each file's length and SHA-256.
3. Copy several folders mixed with PDFs/images. Try same-basename roots and Unicode-equivalent names; verify each subtree remains associated with its allocated name. Confirm the destination file manager handles final paste-name conflicts.
4. During a large transfer, add/remove/change a source file, make a new local copy at either end, cancel or interrupt Wi-Fi. Confirm no half batch is published, newer clipboard content survives, an explicit identical recopy retries, and an earlier completed copy remains pasteable.
5. Repeat screenshots, single-image paste in intended applications, keyboard/mouse, emergency release, pairing, reconnect and closing both windows. Check actual OS permission/firewall and unsigned installer prompts.

These hardware/file-manager checks have not been performed. Test packages are not a formal GitHub Release; physical acceptance is required before release. Mac builds use ad-hoc signatures and are not notarized.

---

## Previous 0.4.0 validation record (historical)

# CrossDesk 0.4.0 validation — 2026-10-21

## New easy-start evidence

Local host: Apple Silicon macOS, Rust 1.98.1, Node.js 24.21.0.

- GUI Rust suite: **102 passing / 2 intentionally ignored** after the explicit-pair preference regression. The ignored subprocess fixture is run by its parent lifecycle tests; the large acceptance is run explicitly in release mode.
- CLI-only suite: **79 passing / 1 large acceptance ignored**. Existing v6 data protocol and command-line options remain usable.
- Seven React / Testing Library tests cover a normal flow without IP/key forms, leading-zero codes, busy cancellation, manual reconnect, polling that preserves typed codes and persistent action errors. Production Vite build passes; npm audit reports zero vulnerabilities in the locked dependency set at validation time.
- A real encrypted TCP pairing test gives host/client the same durable long-term key. Wrong-code and malformed-frame tests reject unauthenticated pairing. Window tests cover expiration, attempt exhaustion and admission concurrency; service shutdown closes its real loopback listener and an idle accepted socket.
- Controller integration runs both first-use roles with actual PAKE/Noise pairing and real temporary storage. Only the input-service boundary is replaced; subsequent startup requires no code. Other tests cover manual disconnect winning a completion race, pending authenticated-grant recovery after a lost final acknowledgment, multi-address retry, persistent preferences and removal of expired pairing advertisements.
- Backend tests spawn the actual test executable as a child, consume structured events, stop/reap it repeatedly, and prove that human log text and late events from an exited process cannot produce a connected state.
- Native mDNS browsing and firewall/permission prompts require the physical two-device checks below. Catalog validation, address replacement/expiry, untrusted identities and bounded candidate counts are tested without announcing on the user's LAN.
- UI was inspected in a local browser with an explicitly labeled, development-only mock preview: first pairing, connected state and advanced settings. Mock preview is excluded from the production build. This is visual verification, not evidence of a hardware connection.

## Release build record

Native Mac ARM64 release `.app` build passes. The full source-inclusive ZIP is approximately 41 MB. Ad-hoc signing and `codesign --verify --deep --strict` pass, including after ZIP extraction with restored executable permissions. Extracted `crossdesk --version` returns 0.4.0 without launching GUI or touching devices. ZIP CRC, matching source commit, dependency notices, executable mode, pairing-data exclusion and external SHA-256 manifest checks pass. The explicit release-mode 129 MiB encrypted streaming test passes. Native Windows/MSVC GUI tests, the explicit disposable-runner clipboard test, 129 MiB transfer and NSIS installer generation pass in [this run](https://github.com/rayem-prog/CrossDesk/actions/runs/39562193771); its final source-archive guard exposed build-generated file rewrites, documented below. Current end-to-end native package checks are visible on [PR #2](https://github.com/rayem-prog/CrossDesk/pull/2/checks). Build and test logs are retained in the task workspace. The app is not launched against the user's devices during validation. No test modifies the user's general clipboard; macOS clipboard tests use named pasteboards and Windows clipboard tests run only on disposable CI runners.

## Implementation decisions and limits

- No-argument GUI builds open the app; explicit CLI commands retain their prior behavior. A single-instance plugin focuses the existing window. Shutdown stops pairing, discovery, timers and the data-service child; no tray/autostart/background mode is added.
- The discovery record is an untrusted address hint with a stable device ID, role, protocol, display name and optional pairing port. It contains no code/key. Reconnection authenticates with the stored random key, including when an advertisement spoofs a trusted ID.
- Codes use rejection-sampled randomness, expire after 120 seconds and admit at most five attempts per window; one handshake is accepted at a time. SPAKE2 0.4.0 + transcript-bound HKDF-SHA256 derive a bootstrap Noise PSK. The code is not a reusable data key. RustCrypto documents that its SPAKE2 implementation has not been independently audited; no audit claim is made.
- Trust is staged atomically. A final pairing-ack loss can recover only by authenticating the existing v6 data channel with the previously delivered key; merely discovering a device cannot commit a trust record. Cancellation and stopped-process state cannot be overridden by stale events.
- Pairing is tested directly through real loopback transport/service APIs instead of adding interactive pair-host/pair-client CLI commands. Production orchestration is the same Core used by integration tests, wrapped in an actor so the WebView stays responsive.
- Runtime app data uses the per-user OS application-data directory for `local.crossdesk.app`; Unix directory/file permissions are 0700/0600. Preferences and identities are private local files. Packages exclude user data and application pairing `.key` files. Public dependency test-key fixtures remain in the corresponding third-party source, where required.
- Data transfer stays protocol v6 on TCP 24920. Pairing TCP 24921 is temporary; discovery uses mDNS UDP 5353. Image/file size limits and original transfer tests remain as documented below.
- Closing or disabling auto-connect does not turn on OS login startup. Manual disconnect pauses the current run. Explicit first pairing still starts a session when automatic connection on future launches is disabled.

## Physical two-device acceptance — pending

1. Install both GUI packages. Open Windows and Mac, allow the actual OS prompts, select Windows on Mac and enter its current code. No terminal, IP or key copying should be needed; both windows should reach connected only after authentication and local input setup.
2. Quit/reopen both apps, change Windows' LAN IP and repeat. Verify automatic discovery/reconnection. Repeat with a virtual adapter and several nearby Windows advertisements.
3. Enter a wrong/expired code and cancel during pairing. Verify no unauthorized trust, listener expiry, and an explicit refresh opens a new code window.
4. Disconnect manually, wait through multiple discovery updates, and confirm it stays paused; reconnect explicitly. Close/reopen repeatedly and confirm no duplicate children/listeners remain.
5. Allow/deny Mac Accessibility/local-network permissions and Windows firewall access. Verify actionable errors and recovery. Test Wi-Fi interruption and sleep/wake.
6. Perform real screenshot/image and PDF/DOCX/ZIP copying in both directions, including Finder/Explorer paste and target chat/editor applications. Compare SHA-256 for file contents. Repeat the ordinary-file physical acceptance below.

No result above claims these hardware, Wi-Fi, real paste, permission or specific target-application checks have already passed.

---

## Prior 0.3.0 baseline — 2026-10-20

## Recorded automated evidence

Local environment: Apple Silicon, macOS 27.0.1, Rust 1.98.1. Windows remains the keyboard/mouse server and Mac the client; clipboard transfer works in both directions.

- **78 native Mac tests pass**, zero failures. One large acceptance test is ignored in ordinary runs and was run explicitly in release mode.
- The release acceptance test transfers and verifies **129 MiB / 135,266,304 bytes** through the real encrypted TCP file protocol. The file exceeds the former image batch limit; test verification reads bounded chunks instead of loading it all into memory.
- All six review regressions now pass: a new local image during remote publication is preserved; identical-content recopy sends again; incomplete file completion is rejected; the next batch preserves previously published files; duplicate basenames do not overwrite; receiving independently rejects oversized manifests.
- File tests exercise simultaneous bidirectional encrypted transfer, binary PDF/ZIP content, zero-byte files, Chinese names, duplicate names, input and heartbeat frames between chunks, offsets/serials, digest failures, cancellation after disk writes followed by retry, source truncation, symlink rejection, batch/count/size limits, cache quota, timeout cleanup, session-stop wakeup and a retained sender after stop.
- One common clipboard observer is exercised over encrypted loopback TCP with an in-memory OS boundary: mixed ordinary files publish once, no remote echo, an explicit same-content recopy is sent and published again, incoming files preserve a newer local copy, and newer remote content replaces a queued older batch across image/file kinds.
- Native Mac tests use **named NSPasteboards**, leaving the user's general clipboard untouched. They cover PNG alpha/pixels, real multiple file URLs, PDF/ZIP/zero-byte files, single-image file representations, stale publication rejection, publication revision receipts, and an identical normal local copy injected before the receipt boundary. A per-publication origin marker distinguishes that copy from the remote write.
- macOS Unicode normalization is checked both at filesystem collision allocation and native file-URL readback. File identities, rather than path spelling, prevent normalized URLs from triggering clipboard echo; separate files with identical bytes remain distinct.
- `cargo fmt --check`, `git diff --check`, and Windows target checking including tests pass.
- Mac ARM64 release build and Windows GNU x64 release link pass. The Windows binary is cross-built on this Mac using Rust LLD, Homebrew MinGW-w64 14.0.0 / GCC 16.2.0 target libraries, and Zig 0.15.2 dlltool. It is a PE32+ AMD64 executable; import-table inspection checks system DLLs only. This is **not Windows runtime validation**.
- The Windows CI test remains explicitly ignored in ordinary local runs to avoid replacing an interactive user's clipboard. The disposable Windows GitHub runner test now covers ordinary PDF/empty-file CF_HDROP, image formats, owner lifecycle, publication receipts and stale-write guards. It was compile-checked locally; native Windows and Mac runner results for the uploaded branch are recorded in [PR #1 checks](https://github.com/rayem-prog/CrossDesk/pull/1/checks). These automated runners do not replace physical two-device acceptance.
- Prior encrypted pairing, version checks, input routing, emergency release, accepted nonblocking socket recovery and input cleanup regressions remain in the suite.

## Implementation boundaries

- Version 0.3.0 / protocol v6 is required on both computers; existing pairing key and TCP port 24920 remain usable.
- Ordinary file limits: 512 MiB per file, 2 GiB / 64 files per batch; cache budget 4 GiB. These are enforced independently by the receiver.
- A sender waits for a disk-processing acknowledgment per 16 KiB request. The receiver has a bounded queue of eight frames and incremental SHA-256. File buffering remains bounded below the 8 MiB design budget; optional native image representations have their separate existing image decode budget. No total-process RSS limit is claimed.
- Partial data resides under a private unique batch directory. The clipboard receives no paths until every file is sealed and the batch commits. Completed batches retain those directories once published; failed/unpublished batches are removed by their lifetime guards. A `.part` suffix is not needed for visibility because staging directories are private and unpublished.
- Clipboard publication uses native revisions and verifies readback identity; native Windows sample is taken under the clipboard lock. Mac publishes fully prepared NSPasteboardItems with a per-write origin marker and captures its receipt inside the same script.
- Content validation and interruption failures do not publish partial batches. Operating-system publication failures and target-application behavior still require platform/device acceptance.
- Ordinary files are streamed unchanged. No arbitrary file execution, directories, symbolic links/reparse points, cloud placeholders, cross-device file dragging, text-default sync, or resume is provided. A new explicit copy can retry the same content. No replay occurs on reconnect.
- Stop-and-wait flow control favors bounded resource use and input responsiveness; throughput depends on round-trip latency. Loopback timing is not a Wi-Fi throughput guarantee.
- No live CrossDesk service or hardware connection was started during these tests.

## Physical two-device acceptance still required

Use both 0.3.0 packages with the same private key. Confirm `protocol v6`, `image clipboard: on`, `file clipboard: on`.

1. Copy PDFs, DOCX/XLSX, ZIP, an empty file and Chinese/accented filenames from Explorer to Finder; repeat in reverse. Compare byte counts and SHA-256 after actual paste.
2. Copy mixed PNG/PDF selections, duplicate names from different folders, and single image files; check folder paste and intended chat/editor applications.
3. Copy a large ordinary file in each direction simultaneously while using the remote mouse/keyboard. Record latency compared with idle operation and confirm heartbeat/input remain usable.
4. During a large send, copy a different local file, bitmap or text. Verify cancellation of unfinished sends and correct final contents. Repeat an identical copy after a skipped publication and confirm retry.
5. Create a newer local copy while receiving and immediately after publication. Confirm it is preserved and can still send; no repeating echo traffic.
6. Interrupt Wi-Fi, stop a process, sleep/wake and reconnect repeatedly. Confirm no partial file can be pasted, old senders stop, workers do not accumulate, and input releases.
7. Leave a completed file copy pending; start a new transfer that fails. Paste the earlier copy. Also test an active Finder/Explorer paste while new batches arrive and before cache cleanup.
8. Run one side with `--file-clipboard off` and separately `--image-clipboard off`. Confirm independently negotiated behavior, including PNG file transfer while ordinary files are enabled.
9. Test inaccessible files, excessive size/count, invalid names, disk-full and clipboard-busy conditions. Confirm errors and that keyboard/mouse remain available.
10. Test Windows emergency release and Ctrl+C on both ends.

Hardware Wi-Fi loss, sleep/wake, real Explorer/Finder paste, clipboard contention, Accessibility prompts and specific application compatibility have not been run in this session. Native Mac adapters and encrypted protocol tests do not establish those results.

## Reproduction commands

```sh
cargo fmt --check
cargo test --locked --no-default-features
cargo test --release --locked --no-default-features large_file_stream_exceeds_image_batch_limit_without_buffering_the_file -- --ignored --nocapture
cargo check --locked --no-default-features --target x86_64-pc-windows-gnu --tests
cargo build --release --locked --no-default-features
```

Windows cross link uses `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=<Rust rust-lld>` and target flags `-C linker-flavor=ld.lld -C link-self-contained=yes -C dlltool=<Zig wrapper>`, with `-L native=` for MinGW x64 CRT and GCC x64 runtime libraries. Native MSVC CI needs none of these overrides. Sources/license notices for the extracted runtime libraries are included under `licenses/toolchain/`.

The implementation is uploaded to `codex/file-clipboard` with [draft PR #1](https://github.com/rayem-prog/CrossDesk/pull/1); it has not been merged or published as a GitHub Release. Push and pull-request workflows run native Windows/MSVC and Mac/ARM64 tests, the explicit disposable Windows clipboard test, release builds, the 129 MiB transfer acceptance test, and source-inclusive packaging.

The first Windows run exposed an invalid cancellation-test assumption: `DirEntry::metadata` uses cached directory enumeration data on Windows, so its file size could remain zero until the receive handle closed at commit. The test now reads metadata from an open file handle to cancel during transfer, and also checks that only the retry reaches completed delivery. The unchanged receiver cleanup remains covered by the same cache-directory assertion. See [Rust DirEntry platform behavior](https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata) and [Microsoft NTFS enumeration caveat](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirstfilea).


### 0.4.0 native CI investigation

The initial GUI Windows run compiled successfully but idle pairing cancellation exceeded its one-second regression bound. Synchronizing acceptance fixed a separate stop/admission race; the next native run still measured 2.010 seconds for an already accepted idle socket, matching the two-second blocking read timeout. The shutdown path therefore did not satisfy cancellation on this runner. Pairing sockets now use nonblocking I/O with a shared stop flag and a ten-second total deadline, including the bootstrap Noise exchange. Data sockets retain blocking behavior and the unchanged v6 framing. New tests cancel a Noise handshake without relying on socket shutdown and verify fragmented reads plus terminal deadlines. Existing idle-stop, immediate-accept-stop, port-release and one-second bounds remain. Native CI reruns verify the result; no relaxed timeout is used to hide it.

Explicit pairing after cancellation is also covered by the real encrypted controller integration: the new Pair action resumes connection; cancellation without a new action remains paused. The CLI-native workflow passed both Windows/MSVC and Mac/ARM64 on the initial implementation commit ([run](https://github.com/rayem-prog/CrossDesk/actions/runs/39559395748)).


The Windows NSIS installer itself was generated successfully; the source packager initially rejected the post-build working tree because Tauri/Vite had rewritten tracked build inputs/outputs. The package now includes the exact working source and an explicit `BUILD_CHANGES.patch` when only Cargo.toml or generated web/dist files differ from the recorded base commit. Edits to application source still fail packaging. A real temporary-Git regression verifies clean packaging, recording and reversing native build rewrites, and rejection of uncommitted application changes. This preserves correspondence instead of labeling a modified tree as an exact commit. Rust-source line endings and the application's v6 protocol are unchanged.
