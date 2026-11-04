# Wayland implementation (M7)

Implemented 2026-11-03. The X11 notes in [x11-implementation.md](x11-implementation.md)
remain the reference for shared behavior. This document records how the Wayland adapter
maps each function, which decisions differ from the porting plan, and what was verified.

## Architecture

`feritto_platform::detect_session()` selects the backend at runtime. `WAYLAND_DISPLAY`
always selects Wayland, even when Xwayland also sets `DISPLAY`. `Platform` wraps
`X11Service` and `WaylandService` behind the same six core contracts, so the executable,
IPC, tray, and UI are backend-agnostic. One binary contains both adapters; libwayland is
loaded with dlopen, so builds need no Wayland development packages.

- `wayland.rs`: a dedicated worker thread with its own Wayland connection. It polls the
  display socket, a command wake-up socket, and non-blocking transfer pipes, without
  idle polling. Protocol objects are bound once at startup and reported as capabilities.
- `portal.rs`: a portal thread with a current-thread Tokio runtime and `ashpd`. It
  registers the host app ID `io.github.gh2302.Feritto`, binds the GlobalShortcuts
  portal, and manages RemoteDesktop keyboard access. Every call that can show a dialog
  (shortcut binding, grant restore, grant request) runs as its own task. A dialog left
  open never blocks clipboard work, shortcut activations, injections, or another dialog.
  Its progress is reported as a `Pending` capability. `FERITTO_NO_PORTALS=1` disables
  the portal thread for tests and minimal sessions.
- UI: on Wayland the popup is created on show and closed on hide or focus loss. winit
  cannot hide or focus an existing Wayland window (`set_visible`/`focus_window` are
  no-ops). An xdg-activation token travels with the request that brought it: the CLI
  (`XDG_ACTIVATION_TOKEN`, forwarded over IPC) for show, toggle, or settings, or a shortcut
  portal activation. It is applied to that request's window (popup or Settings, new or
  already open) through the toolkit's display on a private event queue.

## Function mapping

| Contract | Wayland implementation | Verified |
|---|---|---|
| ClipboardMonitor | `ext-data-control-v1`, else `zwlr-data-control` v2. Eager sequential capture through non-blocking pipes, with the X11 policy, aliases, limits, and timeouts. | sway, KWin |
| ClipboardOwner | Data-control source offering original types, every text alias, Latin-1 `STRING`, and a private per-instance owner marker. Clear sets an empty selection. PRIMARY is published for Shift+Insert. | sway, KWin |
| Persistence | An empty selection after a completely captured current generation republishes it, unless feritto cleared it, history-only mode is on, or capture is paused. | sway, KWin |
| HotkeyService | GlobalShortcuts portal, preferred trigger `CTRL+grave`; otherwise a reasoned `Unavailable` pointing to `feritto --toggle`. | Fallback only |
| ActiveWindowTracker | `wlr-foreign-toplevel-management`: activated toplevel, excluding feritto's own windows; restore uses `activate(seat)`. | sway |
| PasteInjector | `zwp-virtual-keyboard-v1` with a private two-key keymap, or RemoteDesktop portal keysyms. | sway (virtual keyboard) |
| PopupPlacer | The compositor places the window; pointer and caret placement are `Unavailable`. | sway, KWin |

### Safety rules carried over from X11

- Own offers are recognized by a per-process marker type and never recaptured. The core
  treats the marker as a protocol type, so it is never stored or served from history.
- An offer announcing more than 512 types is not captured. A hint could hide in the part
  that is not retained, as with X11's oversized TARGETS lists.
- `x-kde-passwordManagerHint` is read first, capped at 64 bytes. A `secret` value, an
  oversized hint, or a failed or timed-out hint read drops the generation. The ignore
  markers skip it. Every new generation invalidates older recovery data, so a secret,
  paused, excluded, or incomplete copy cannot resurrect an older clip.
- Paste keys are sent only after foreign-toplevel reports the chosen target active again.
  This waits at most 1 s; feritto first asks the compositor to activate the target.
  If the target closed, the selection changed, or no injector exists, the clipboard stays
  ready and the user is told to paste manually.
- Each paste is bound to the publication it was requested for. Replacing (including a
  copy-only feritto publication), clearing, or losing the CLIPBOARD contents cancels it,
  as does losing PRIMARY for Shift+Insert. A request queued to the portal thread rechecks
  this, and a 500 ms deadline, before its first key. Portal injection releases every key
  whose press was attempted, even after an error, and closes a failed session.
- Repeated takeovers of feritto's selection (three within 2 s) switch to history-only
  mode, as on X11. Replaced own sources are destroyed rather than cancelled, so they never
  count as contention.

## Decisions that differ from the plan

- **Regular window instead of layer-shell.** One winit runner keeps the popup, settings,
  IME (text-input-v3) and renderer identical to X11 on every compositor. Measured
  focus and floating behavior on sway and KWin met the interaction loop requirements.
  `iced_layershell` (a second runner with its own event loop) remains an option for
  overlay above fullscreen windows and guaranteed keyboard exclusivity.
- **No GUI clipboard on Wayland.** iced's `wayland` feature attaches iced's own
  clipboard (smithay-clipboard) to the first window and reconnects it whenever that window
  closes. With a popup that closes on every hide, a selection change during teardown
  desynchronized libwayland's server object IDs. The GUI connection then died with
  `not a valid new object id … data_offer`, reproduced on sway. `feritto-ui` therefore
  enables winit's and softbuffer's Wayland features directly. iced's `x11` feature
  still sets the window name, which winit uses as the Wayland `app_id`. Instead, each
  window's content is wrapped in a widget that gives the text fields inside it a clipboard
  backed by the platform service. The fields keep iced's own editing: paste inserts at the
  cursor, and copy and cut publish the selected text. A Ctrl+V that arrives before a
  just-opened popup has focused its search field is appended to the query instead of
  being lost. Reads receive text from the current
  selection owner like any paste, bounded to 1 s and 1 MiB. They do not depend on history
  capture, pause, or format settings.
- **Virtual keyboard created at startup.** Clients then already know the device and its
  keymap before the first paste. Creating it on demand let keys race ahead of client
  keyboard setup on a seat without other keyboards.
- **Unverified paste is opt-in.** The plan defaults Wayland to copy-and-ask. Where the
  target cannot be confirmed, `unverified_paste` allows sending keys after the popup
  closes and a 250 ms settle period. It is shown only on Wayland and defaults to off.

## Reproducible evidence

Reference environment as in the X11 notes: Debian 13 VM, Rust 1.90.0, tiny-skia.
Compositors: sway 1.10.1 (wlroots 0.18, headless backend, pixman renderer) and
KWin 6.3.6 (`--virtual`). Clients: wl-clipboard 2.2.1, wev 1.0.0, wtype 0.4, foot 1.21.0.

`make test-wayland` (sway) runs `wayland_protocol` and `scripts/wayland-smoke.py`:

- Capture of Unicode text with canonical MIME. feritto's own offer is served as
  `text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`, `TEXT`, and Latin-1 `STRING`,
  and is not recaptured.
- Source-exit recovery. An explicit feritto clear stays empty. A 20 MiB opaque transfer is
  captured completely and served byte-for-byte after its source exits.
- Pause: no capture, and no resurrection of older data after a paused source exits.
- A raw data-control source offering text, HTML, RTF, URI list, a configured opaque type
  and an unconfigured private type yields exactly the five configured formats, all served
  unchanged after source exit.
- Secret, non-secret, and ignore hints behave as on X11. PRIMARY is published only for
  Shift+Insert and stays independent of later CLIPBOARD changes.
- Verified Ctrl+V reaches a focused `wev`. A paste aimed at a closed window sends no keys.
  A copy-only publication made while a paste waits cancels it, and no keys are sent.
- An offer with a secret hint at type 514 (past the limit) is neither captured nor
  recovered. Text-field reads return the live selection while capture is paused or
  text capture is off.
- End to end with the real binary: `--status` capabilities. The portal fallback degrades
  to a reason with no portal installed. `--add`/`--list --search`. `--toggle` opens a
  floating, focused popup inside the output. Typing searches; Enter pastes into foot with
  Ctrl+Shift+V; focus returns. Search-field Ctrl+V pastes through the service, including
  at the start of an existing query while capture is paused; Ctrl+C and Ctrl+X in the
  search field set the clipboard.
  20 open/Escape cycles, close on focus loss, external capture with source-exit
  recovery, PNG round trip, settings without quitting, pause/resume, clear clipboard
  without resurrection, clear history, 0600/0700 private files, and restart durability.
  CLI request → focused popup p95 was 81–83 ms in development builds. Each open creates a
  window, unlike the warm X11 popup.

`make test-wayland-kwin` runs the two clipboard protocol tests on KWin. That KWin offers
`zwlr_data_control_manager_v1` v2, but no ext-data-control, foreign-toplevel, or
virtual-keyboard globals to ordinary clients. feritto reports automatic paste, source
identity and (without a portal) the hotkey as unavailable, with reasons. A wire
log showed the popup receiving keyboard focus and the activated state on each re-creation.

The X11 suite (`make test-x11`) still passes after these changes, including 100 popup
cycles on i3 and Xfwm.

## Not yet qualified

- GlobalShortcuts and RemoteDesktop portals are implemented against the xdg-desktop-portal
  API but were not exercised at runtime. Their dialogs need a full Plasma or GNOME session.
  Qualify them on Plasma 6 (shortcut binding, `activation_token` delivery, keyboard grant,
  restore-token reuse) before claiming automatic paste or the hotkey on KDE.
- GNOME Shell (mutter) does not offer data-control as of the checked sources, so history
  cannot be captured or owned there. feritto says so in the popup, Settings, and
  `--status`. A Shell extension would be separate, explicitly scoped work.
- Real-app matrix, IME composition, fractional scaling and multi-output placement,
  compositor restart (the service stops, as on X11), Hyprland/river/labwc, and physical
  desktops remain qualification work.
- Wayland cannot distinguish an application exiting from an explicit clear by another
  client. feritto restores the last complete capture in both cases.
- Source identity is the focused application at copy time. Background copies, for example
  `wl-copy` from a terminal, are attributed to the focused window.
