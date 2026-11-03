#!/usr/bin/env bash
# Runs the Wayland checks against a private headless compositor, never the user's desktop.
#   scripts/check-wayland.sh [sway|kwin]   (default: sway)
# sway (wlroots) runs everything. Requires: sway, wl-clipboard, wev, wtype, foot, coreutils
# stdbuf, dbus-run-session, fonts-dejavu-core.
# kwin (KDE) exposes data-control but neither foreign-toplevel nor virtual-keyboard to
# ordinary clients; it runs the clipboard protocol tests. Requires: kwin-wayland.
set -euo pipefail
cd "$(dirname "$0")/.."
compositor_name=${1:-sway}
cargo build --locked -p feritto

base=$(mktemp -d /tmp/feritto-wayland-XXXXXX)
chmod 700 "$base"
mkdir -m 700 "$base/runtime"
compositor=
cleanup() {
    [ -n "$compositor" ] && kill "$compositor" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$base"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$base/runtime" XDG_SESSION_TYPE=wayland
unset DISPLAY WAYLAND_DISPLAY SWAYSOCK
case "$compositor_name" in
sway)
    cat >"$base/sway.conf" <<'CONF'
output HEADLESS-1 resolution 1280x1024
default_border none
xwayland disable
CONF
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
        sway -c "$base/sway.conf" >"$base/compositor.log" 2>&1 &
    ;;
kwin)
    # KWin needs a session bus; the tests share it.
    eval "$(dbus-launch --sh-syntax)"
    kwin_wayland --virtual --socket wayland-feritto --width 1280 --height 1024 \
        >"$base/compositor.log" 2>&1 &
    ;;
*)
    echo "unknown compositor: $compositor_name" >&2
    exit 2
    ;;
esac
compositor=$!
for _ in $(seq 200); do
    socket=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit)
    [ -n "$socket" ] && break
    sleep 0.05
done
[ -n "${socket:-}" ] || { cat "$base/compositor.log"; echo "$compositor_name did not start" >&2; exit 1; }
export WAYLAND_DISPLAY=$(basename "$socket")

if [ "$compositor_name" = sway ]; then
    export SWAYSOCK=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -name 'sway-ipc.*' -print -quit)
    cargo test --locked -p feritto-platform --test wayland_protocol -- --ignored --test-threads=1
    dbus-run-session -- python3 scripts/wayland-smoke.py
else
    cargo test --locked -p feritto-platform --test wayland_protocol -- --ignored --test-threads=1 \
        --skip verified_paste_reaches_the_target_window
    kill "$DBUS_SESSION_BUS_PID" 2>/dev/null || true
fi
