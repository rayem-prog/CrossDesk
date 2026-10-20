# CrossDesk 0.3.0 validation — 2026-10-20

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
