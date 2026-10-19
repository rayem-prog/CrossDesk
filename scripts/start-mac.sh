#!/bin/bash
set -euo pipefail
task_root="$(cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$task_root"

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
  echo 'This build script targets Apple Silicon Macs (including M1 Pro).'
  exit 1
fi
if ! xcrun --find clang >/dev/null 2>&1; then
  echo 'Install Apple Command Line Tools first: xcode-select --install'
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1 && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo 'Install stable Rust from https://rustup.rs, then open this script again.'
  exit 1
fi
if [[ ! -f "$task_root/crossdesk.key" ]]; then
  echo 'Start CrossDesk on Windows once, then copy its crossdesk.key beside this script.'
  exit 1
fi

task_address="${1:-}"
if [[ -z "$task_address" && -f "$task_root/mac-server.txt" ]]; then
  task_address="$(cat "$task_root/mac-server.txt")"
fi
if [[ -z "$task_address" ]]; then
  read -r -p 'Windows LAN IP (for example 192.168.1.10): ' task_address
fi
if [[ -z "$task_address" || "$task_address" == -* || "$task_address" == *[[:space:]]* ]]; then
  echo 'Enter a Windows LAN IPv4 address or hostname.'
  exit 1
fi
if [[ "$task_address" != *:* ]]; then task_address="${task_address}:24920"; fi
printf '%s\n' "$task_address" > "$task_root/mac-server.txt"
chmod 600 "$task_root/crossdesk.key"

echo 'Building the Apple Silicon client (first build downloads Rust dependencies)...'
export MACOSX_DEPLOYMENT_TARGET=13.0
unset CROSSDESK_CHECK_ONLY
cargo build --release --locked --no-default-features --target aarch64-apple-darwin
echo
echo 'Enable the launching Terminal/CrossDesk entry under Privacy & Security > Accessibility.'
echo 'If a permission prompt stops the first run, grant permission and open this script again.'
echo 'Ctrl+C stops the client. Network failures automatically retry.'
exec "$task_root/target/aarch64-apple-darwin/release/crossdesk" client \
  --server "$task_address" --key-file "$task_root/crossdesk.key"
