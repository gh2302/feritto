# First runnable X11 implementation

Implemented 2026-11-03. This supersedes the plan's original “no application code”
status. The roadmap remains the feature and qualification contract; completion
of this batch does **not** mean all M0–M4 release gates have passed.

## Delivered

The five-crate Rust workspace follows the planned boundaries:

- `feritto-core`: immutable multi-format offers, capture policy, Unicode search
  normalization, settings, capabilities, and six protocol-independent service contracts.
- `feritto-store`: serialized SQLite worker, native schema v1, WAL, original
  representations, checksummed payload files over 1 MiB, atomic publication and
  startup orphan recovery, deduplication, retention and protection, thumbnails,
  normalized literal substring search with FTS5 trigram acceleration.
- `feritto-platform`: dedicated x11rb worker with XFixes monitoring, native selection
  serving, incoming/outgoing INCR, MULTIPLE, TIMESTAMP, SAVE_TARGETS, source-exit
  recovery, explicit clear handling, secret/ignore hints, manager coexistence,
  lock-aware Ctrl+grave registration, cursor/monitor/work-area placement, EWMH focus
  requests, event-driven waits on the X connection and command wakeup socket,
  and focus-checked XTest paste with a clipboard-only fallback.
- `feritto-ui`: iced daemon, reusable hidden managed popup, independent settings,
  keyboard navigation and IME composition guards, debounced generation-tagged
  search, visible-row widgets with spacers, compact ID snapshots, bounded decoded
  thumbnail pages, previews, stars and deletion. A single click selects; double-click
  or Enter accepts. The native handle is acquired through typed `window::run`.
- `feritto`: service composition, private single-instance Unix socket and lock,
  persisted XDG settings, CLI controls and JSON inspection, GTK-free ksni tray,
  desktop launchers and install/uninstall targets.

No Ditto source or artwork was translated/copied. New code uses GPL-3.0-or-later;
NOTICE records the upstream reference revision and the independent implementation
choice. The native schema reserves the group/order/protection/import metadata
needed by later milestones, but it is **not** a Ditto database schema.

## Reproducible evidence

Reference environment: Debian 13 x86_64 VM, Linux 6.12, 8 exposed Intel i7-14700K
vCPUs, 15 GiB RAM. Rust 1.90.0, iced 0.14.0, software tiny-skia renderer;
Xvfb 21.1.16 (`2:21.1.16-1.3+deb13u4`) at 1280×1024×24, i3 4.24-1,
Xfwm 4.20.0-1 without a compositor. Tests use private XDG directories and D-Bus
sessions, synthetic clipboard data, actual window managers and xterm.

Run `make check`, `make test`, and `make test-x11`. The latter starts fresh X
servers rather than interacting with the user's desktop. CI runs these checks
on Ubuntu 24.04; that workflow has been added but has not run remotely yet.

Locally exercised behavior:

- A 20 MiB text selection transfers into the service via INCR. After the xclip
  source is killed, a separate client receives identical bytes through outgoing
  INCR. The source retains ownership throughout eager capture.
- An explicit clear leaves CLIPBOARD unowned. Own offers do not recapture. A
  secret-hinted generation is never fetched or stored and cannot resurrect an
  older clip on source destruction.
- A synthetic source offers Unicode text, HTML, RTF, URI data, and configured
  opaque binary bytes. Captured formats are served unchanged; an unconfigured
  private target is never requested. MULTIPLE marks an unsupported sub-request
  failed, and cooperative SAVE_TARGETS retains the requested supported targets.
- PRIMARY and CLIPBOARD offers remain independent, including after subsequent
  copies and CLIPBOARD clears. Background PRIMARY capture remains off.
- A held-modifier/focus-change fixture confirms that losing the intended target
  leaves the clipboard ready and reports manual paste, without sending paste
  keys to the unrelated newly focused window. A copy-only publication made while
  that paste waits cancels it: feritto still owns CLIPBOARD, so each paste is
  bound to its publication rather than to ownership alone.
- Failure paths keep the worker alive: a SAVE_TARGETS requestor that exits while
  its source stalls, and TARGETS lists naming unknown atoms, are settled (window
  released, requestor answered) instead of stopping the service. `TEXT` requests
  are answered with type `UTF8_STRING` (`x11_robustness` test).
- Both WMs complete copy → history → case-insensitive literal search → selection
  → paste into the saved xterm target. Shift+Insert explicitly publishes PRIMARY.
  The global hotkey works with CapsLock/NumLock. A PNG is captured, thumbnailed,
  previewed, and served byte-for-byte. Popup geometry stays within the test screen.
- Both WMs complete 100 show/hide cycles with popup focus and Escape focus
  restoration checked each cycle. Settings close without quitting. Pause/resume,
  clear-history-with-live-offer, private files, single-instance IPC, and restart
  durability pass.
- Core/storage tests cover rich-format hashes, Unicode normalization/case folding,
  UTF-8 truncation, literal punctuation, alias policy, protected retention, binary
  spill/reload, private permissions, thumbnails, orphan recovery, and refusal of
  a database with a newer schema.
- The X11-only feature graph of this batch contained no `wayland-client`. The Wayland
  adapter (see [Wayland implementation notes](wayland-implementation.md)) now adds it to
  the same binary, loaded with dlopen. `cargo check -p feritto --features wgpu --locked` passes;
  GPU rendering has not been runtime-qualified.

The final 100-cycle development-build timing run used event-driven native waits
and the frame-margin fix. CLI request → verified popup keyboard focus was p95
76.6 ms on i3 and 75.6 ms on Xfwm. This includes command process startup and
focus polling, and does **not** measure first paint or IME readiness. These are development-build observations,
not a claim that the complete warm-popup release gate is satisfied.

The explicit `feritto-store` `search_100k` test seeds 100,000 short synthetic rows
and checks exact result counts. Run it with `cargo test --release -p feritto-store
search_100k -- --ignored --nocapture`. Ten warm queries per pattern measured p95
0.22–0.28 ms for selective indexed queries, 16.25 ms for a two-character scan, and
18.57 ms for a one-character query returning 40,951 IDs. Queries matching every
row measured 38.68–47.06 ms. These are storage-only times, excluding the 75 ms
UI debounce, page loading and paint. They do not establish the complete 150 ms
interactive-search gate. The corpus contains short text, not 64 KiB per row.

The five-minute optimized hidden-service measurement (`scripts/measure-idle.py`)
ran for 300.7 s with 20 short Unicode text clips and 3 generated PNG clips with
thumbnails. CPU averaged 0.163% of one core; RSS and peak RSS were 20.99 MiB.
The popup stayed hidden under Xvfb with no WM or tray watcher. This small-corpus
check satisfies the provisional idle budgets on that setup; larger-corpus,
tray-present and physical-desktop measurements remain qualification work.
The native worker waits on the X socket and a command wakeup socket rather than
periodically polling a quiet connection. Both optimized binaries also completed
the end-to-end desktop suite with a short three-cycle smoke run per WM.

`cargo build --release --locked -p feritto` passes. Desktop launchers pass
`desktop-file-validate`. Install/uninstall was exercised with `PREFIX=/usr` and
a private `DESTDIR`; the installed binary runs and uninstall removes all staged
files. Host desktop files and startup settings were not modified.

## Current limits and remaining gates

This is a usable X11 foundation through the basic M3 interaction loop. It is not
yet an X11 beta release. In particular:

- The real GTK/Qt/browser/LibreOffice/password-manager matrix, WM policy denial and
  ownership-race stress, slow/malformed source transfers, suspend/reconnect,
  physical multiple monitors/DPI, non-Latin shortcuts, actual IME and accessibility
  tests still require qualification. Focus failure never permits unverified injection.
- Caret lookup is unavailable; pointer placement is reported as the fallback.
  X11 source identity is best effort through the selection owner's WM_CLASS;
  hidden helper windows may have no usable identity. Application exclusions apply
  only when identity is known. Secret hints do not depend on identity.
- Only the active keyboard group's supported paste keys are injected. The group is
  read over a separate XKB-initialized connection, so the main connection keeps
  core MappingNotify delivery for hotkey re-registration. A layout
  without a resolvable sequence leaves the clipboard ready for manual paste;
  cross-group Latin shortcut support and per-application profiles are pending.
- PRIMARY monitoring, configurable hotkey editing, a full-size preview, multi-select,
  advanced actions, and translations are pending. Raw HTML/RTF is retained and
  offered; previews are plain text and never execute HTML or fetch remote resources.
- The first schema creates a new database and refuses unknown/newer schemas;
  migration backup/restore machinery and fixture-backed Ditto import are M4 work.
  There is no import command. Native backup/export, groups UI, sticky ordering,
  shortcut migration, Rhai, and Friends remain on the roadmap.
- A broken X connection currently reports the stopped service; it does not reconnect
  automatically. The tray is optional. Another clipboard manager or history-only
  mode reduces source-exit persistence guarantees; no manager can recover bytes
  the original source never delivered.
- Limits default to 32 MiB/target, 64 MiB/generation, 2 s progress and 10 s total.
  Transfers exceeding limits are incomplete and ineligible for recovery. Queues,
  result snapshots, wire bytes, outgoing requests and decoded image allocation are
  bounded. Large active offers remain alive independently of history deletion.
- Wayland is implemented by a separate adapter selected at runtime, sharing the core,
  store and UI; see [Wayland implementation notes](wayland-implementation.md). The X11
  adapter still refuses Wayland sessions, including sessions with Xwayland DISPLAY set.

Before declaring the remaining M0 gates complete, also finish the pinned Ditto fixture
inventory. The layer-shell runner was not adopted; the Wayland notes record the regular-window
decision and its runtime results. Passing
these synthetic X11 tests does not substitute for those deliverables.
