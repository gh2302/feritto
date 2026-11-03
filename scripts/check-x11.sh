#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --locked -p feritto
xvfb-run -a cargo test --locked -p feritto-platform --test x11_protocol -- --ignored
for wm in i3 xfwm4; do
    xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- python3 scripts/x11-smoke.py "$wm"
done
