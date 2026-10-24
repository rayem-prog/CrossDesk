#!/bin/bash
set -euo pipefail
task_root="$(cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$task_root"
if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
  echo 'This package requires an Apple Silicon Mac.'
  exit 1
fi
if [[ ! -x "$task_root/crossdesk" ]]; then
  echo 'Extract the full Mac package, then run: chmod +x crossdesk'
  exit 1
fi
if [[ ! -f "$task_root/crossdesk.key" ]]; then
  echo 'Copy crossdesk.key from the Windows package folder beside Start Mac.command.'
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
echo 'CrossDesk 0.7.0 is recommended on both computers. Protocol v7 is required; text needs 0.6.0 or later.'
echo 'Enable Terminal/CrossDesk under Privacy & Security > Accessibility.'
echo 'Copy an image or file after connecting; clipboard sync is on by default.'
echo 'Ctrl+C stops the client. Network failures automatically retry.'
exec "$task_root/crossdesk" client --server "$task_address" --key-file "$task_root/crossdesk.key"
