<img src="assets/icon.svg" width="64" height="64" alt="">

# feritto

A native X11 clipboard manager inspired by Ditto, written in Rust with iced.
Capture text, PNG screenshots, HTML, RTF, and file lists; search history and paste
into the previous application. The desktop service keeps running when its popup
is hidden. This is the first runnable implementation, not Ditto feature parity.

## Build and run

On Debian 13 / Ubuntu 24.04, install the build dependencies:

```sh
sudo apt-get install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libxkbcommon-x11-0 libfontconfig1-dev libxcb-shape0-dev libxcb-xfixes0-dev
cargo build --locked -p feritto
./target/debug/feritto
```

Install [rustup](https://rustup.rs/) first if needed. `rust-toolchain.toml` pins
Rust 1.90.0, including rustfmt and clippy. `Cargo.lock` pins the dependencies.
The default renderer is tiny-skia; the optional GPU configuration builds with
`cargo build --locked -p feritto --features wgpu`.

Run from a **native X11 desktop session**. The X11 build explicitly rejects
Wayland sessions; a Wayland backend and shell runner are pending. Core types,
storage, and UI messages are separate from the native adapter to support that port.

Press **Ctrl+grave** (Ctrl+\`) to toggle the popup. Type to search, use arrow keys
and PageUp/PageDown to select, then Enter or double-click to paste. Shift+Enter
copies without automatic paste. Escape hides the popup. Ctrl+L returns to search;
Ctrl+, opens settings. Home/End edit the query while search has focus and navigate
the history otherwise. Star clips to protect them from automatic cleanup.

Settings select capture families, pause capture, retention, theme, history-only
coexistence, and the paste shortcut. The initial shortcut is Ctrl+V; terminals
may need Ctrl+Shift+V or **Shift+Insert (publishes PRIMARY as well as CLIPBOARD)**.
If focus or input injection cannot be verified, the clipboard stays ready and a
status message asks you to paste manually. Non-Latin shortcut layouts still need
qualification; unsupported active layouts use this manual fallback.

A StatusNotifierItem tray appears when the desktop supplies a watcher. If the
tray is unavailable or the hotkey conflicts, bind `feritto --toggle` in the desktop.

```sh
feritto --daemon                 # start hidden
feritto --toggle                 # forward to the running instance
feritto --settings
feritto --status                 # capabilities and history count as JSON
feritto --list --search 'café'    # literal Unicode substring search
feritto --copy 42
feritto --paste 42
feritto --add 'Keep this text'
feritto --pause                  # toggle capture
feritto --clear-history          # keep the current clipboard
feritto --clear-clipboard        # keep history
feritto --quit
```

Use `./target/debug/feritto` in place of `feritto` before installing. `--help`
lists all commands. `--list` returns at most 1,000 summaries; the UI supports a
100,000-ID result snapshot and only builds widgets for the visible rows.

## Install

`make install` builds a release binary and installs it, the desktop launcher,
and the icon under `~/.local`. Override `PREFIX` and `DESTDIR` for packaging.
Ensure `~/.local/bin` is on your desktop's PATH. `make uninstall` removes those
installed files and keeps your data.

For optional autostart, copy
`assets/desktop/io.github.gh2302.Feritto-autostart.desktop` into
`~/.config/autostart/`. Remove that file to disable autostart.

## Data and clipboard behavior

History lives in `$XDG_DATA_HOME/feritto` (default `~/.local/share/feritto`),
settings in `$XDG_CONFIG_HOME/feritto/settings.json`, and the instance socket in
`$XDG_RUNTIME_DIR/feritto`. Without a runtime directory, the socket uses a private
per-user directory under `/tmp`. Directories are 0700 and data files are 0600.
Storage is **unencrypted**. Logs do not include clipboard contents.

Original representations remain intact, including configured opaque targets.
Payloads over 1 MiB use checksummed files; SQLite uses WAL. Search covers a bounded
64 KiB normalized text prefix; the preview marks truncation. Repeated copies of
the same configured format set update recency. Starred and other protected clips
can exceed retention limits. `settings.json` additionally supports explicit
`capture.extra_targets`, `capture.excluded_applications`, transfer budgets,
`max_history_bytes`, and `clear_clipboard_on_exit` (restart after manual edits).
PRIMARY is not monitored in the background.

Secret and ignore hints are checked before content is captured. Recovery after a
source exits applies only to a completely captured eligible generation; an
intentional clear or excluded generation invalidates older recovery data.
Another clipboard manager disables automatic recovery, and ownership contention
switches to history-only mode. A source that disappears before finishing a transfer
cannot be recovered. The last serving process exiting loses its selection unless
another manager accepts the shutdown handoff. Clearing history is logical deletion,
not secure erasure of backups or filesystem snapshots.

## Validation and scope

```sh
make check
make test
# Extra test dependencies: xvfb xauth xclip xdotool i3-wm xfwm4 xterm dbus-x11 x11-utils fonts-dejavu-core
make test-x11
cargo test --release -p feritto-store search_100k -- --ignored --nocapture
# Five-minute hidden-service measurement, after a release build:
xvfb-run -a dbus-run-session -- python3 scripts/measure-idle.py
```

The X11 checks run in isolated Xvfb/D-Bus sessions and exercise real i3/Xfwm
window managers and xterm. They cover large INCR transfers, multiple formats,
secret hints, source exit, explicit clears, SAVE_TARGETS, MULTIPLE, search,
PNG previews, paste, lock-key hotkeys, settings, privacy, and restart durability.
See [implementation evidence and remaining gates](docs/x11-implementation.md).
The GTK/Qt/browser/LibreOffice/password-manager matrix, IME, physical multi-monitor
and DPI behavior, reconnects, accessibility, and GPU runtime qualification remain.

Ditto database import, groups UI, advanced actions, scripting, Friends, and Wayland
are later milestones. This build does not open or modify a Windows Ditto database.
New code is an independent implementation under GPL-3.0-or-later; see LICENSE and NOTICE.

## Design documentation

- [Rust + iced porting plan](docs/ditto-rust-iced-porting-plan.md)
- [Critical review and resolutions](docs/ditto-rust-iced-review.md)
- [Ditto port analysis](docs/ditto-port-analysis.md)
- [Rust GUI options](docs/rust-gui-options.md)
