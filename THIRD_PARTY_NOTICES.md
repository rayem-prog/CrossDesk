# Source and dependencies

CrossDesk 0.1.0 is a modified version of [Deskbridge](https://github.com/paulhe666/deskbridge),
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
