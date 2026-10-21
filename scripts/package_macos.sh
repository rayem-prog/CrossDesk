#!/usr/bin/env bash
set -euo pipefail
export COPYFILE_DISABLE=1
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
npm --prefix web ci
web/node_modules/.bin/tauri build --features gui --bundles app
APP="$ROOT/target/release/bundle/macos/CrossDesk.app"
codesign --force --deep --sign "${MACOS_APPLICATION_IDENTITY:--}" "$APP"
codesign --verify --deep --strict "$APP"
python3 scripts/package_gui.py --artifact "$APP" --platform Mac-ARM64 --output dist/gui
