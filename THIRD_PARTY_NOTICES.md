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
