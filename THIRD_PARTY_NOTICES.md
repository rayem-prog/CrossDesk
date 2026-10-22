# Source and dependencies

CrossDesk 0.2.0 is a modified version of [Deskbridge](https://github.com/paulhe666/deskbridge),
based on commit `1b881f552b9780c05e222a7cf57733f1cf556123` (version 1.6.9).
The original GPL v3 license is preserved in `LICENSE` and the upstream README is
preserved in `UPSTREAM_README.md`. CrossDesk modifications use the same GPL v3 license.

The prototype retains Deskbridge's Windows capture, macOS input injection,
scancode mapping, and protocol foundation. It adds two-sided routing,
desktop coordinate mapping, encrypted pairing, connection liveness and cleanup,
ordered client input, command-line validation, launch scripts, and regression tests.

Noise encryption uses [snow](https://github.com/mcginty/snow), licensed MIT OR
Apache-2.0. Dependency versions are recorded in `Cargo.lock`. Deskflow, Lan Mouse,
and ShareCursor were inspected during planning; their code is not incorporated
into this prototype.

## Modifications — 2026-10-20

Added bidirectional image clipboard synchronization to the existing modified code: protocol v5 capabilities, bounded encrypted image chunks, PNG normalization and alpha support, PNG/JPEG file copies, content fingerprints, revision guards, a session-owned worker, cache lifecycle and native clipboard adapters/tests. CrossDesk modifications remain GPL-3.0-only; this release does not relicense Deskbridge.

Image decoding/encoding uses [image-rs/image](https://github.com/image-rs/image), licensed MIT OR Apache-2.0. SHA-256 uses [RustCrypto/hashes](https://github.com/RustCrypto/hashes), licensed MIT OR Apache-2.0. Exact dependency versions and transitive dependencies are recorded in `Cargo.lock`. The complete corresponding project source is included in the `source/` directory of each binary package. Native dependency sources and their original license files are included in `third-party-source/`, with an index in `THIRD_PARTY_SOURCES.txt`. Cargo resolves dependencies from the lockfile when rebuilding.

The locally cross-built Windows GNU executable uses MinGW-w64 14.0.0 target libraries (ZPL-2.1 and file-specific licenses), winpthreads (MIT/BSD), and GCC 16.2.0 runtime support (GPL v3 with GCC Runtime Library Exception 3.1). Relevant notices are in `licenses/toolchain/`; the GPL v3 text is `LICENSE`. Official source trees are [mingw-w64 v14.0.0](https://github.com/mingw-w64/mingw-w64/tree/v14.0.0) and [GCC 16.2.0](https://github.com/gcc-mirror/gcc/tree/releases/gcc-16.2.0). Native MSVC CI packages use the Microsoft runtime instead of this GNU cross-build toolchain.

## CrossDesk 0.3.0 modifications — 2026-10-20

Adds bounded, acknowledged ordinary-file clipboard transfer, protocol v6 capabilities and publication results; fixes publication/recopy races and legacy receiver completeness, cache lifetime, collisions and receive limits. Uses existing SHA-256 and encrypted transport dependencies. Retains GPL-3.0-only and upstream notices. No pairing keys are part of the source or packages.

## CrossDesk 0.4.0 modifications — 2026-10-21

Replaces the manual setup GUI with one-time code pairing, LAN device discovery, durable device preferences, automatic reconnection, structured process state and complete child/service shutdown. Preserves protocol v6 image/file transfer and GPL-3.0-only. No upstream data is removed by the new packaging scripts.

Pairing uses [RustCrypto/PAKEs spake2](https://github.com/RustCrypto/PAKEs) 0.4.0 (MIT OR Apache-2.0), [RustCrypto/KDFs hkdf](https://github.com/RustCrypto/KDFs) (MIT OR Apache-2.0), and the existing snow transport. The SPAKE2 implementation is not independently audited. Discovery uses [mdns-sd](https://github.com/keepsimple1/mdns-sd) (Apache-2.0 OR MIT). Desktop uses [Tauri](https://github.com/tauri-apps/tauri) and official single-instance/dialog plugins (MIT OR Apache-2.0). Web uses React (MIT), Lucide (ISC) and their locked dependencies; build/test tools retain their original licenses. Cargo.lock and web/package-lock.json specify exact resolved versions. GUI packages include corresponding project and Rust dependency source, and installed JavaScript license/notice files with a dependency index. Existing Deskbridge artwork retains its upstream license.

## CrossDesk 0.5.0 modifications — 2026-10-22

Adds bounded, paged protocol v7 directory manifests, folder scanning, lazy source access, independent receive validation, recursive staging/cache ownership, native folder clipboard tests and separate transfer status. Keeps existing pairing identities and GPL-3.0-only. Uses [unicode-normalization](https://github.com/unicode-rs/unicode-normalization) (MIT OR Apache-2.0) for deterministic canonical-equivalent filename collision handling; full corresponding dependency source and notices are included in build packages.
