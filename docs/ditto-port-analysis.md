# Ditto → Linux port analysis

Source analysed: <https://github.com/sabrogden/Ditto> (shallow clone, Nov 2026).
About 137k lines of C++ built on MFC/Win32: 178 `.cpp` files. The largest are
`QPasteWnd.cpp` (8.6k lines), `Options.cpp`, `QListCtrl.cpp`, `Clip.cpp` and `Path.cpp`.

Plan: **X11 first**, Wayland second.

---

## 1. Language

### Original recommendation: C++20 + Qt 6 (Widgets), with a small native X11 layer

- **None of Ditto's code can be reused directly.** It's built on MFC throughout:
  `CString`, `HGLOBAL`, `CLIPFORMAT` and GDI+ show up even in the add-in
  interface (`Shared/IClip.h`). So this is a rewrite either way. C++ still lets
  us keep a few portable parts:
  - the ChaiScript engine, so users' existing on-copy and paste scripts keep working
  - the SQLite schema (`Main`, `Data`, `Types` in `DatabaseUtilities.cpp`), so old
    Ditto databases can be imported
  - the Tokenizer, text transforms, MD5/CRC and the QR encoder (the system
    `libqrencode` can replace the bundled copy)
- **Qt covers most of the UI Ditto needs.** Ditto's main window draws its own large
  list. Qt's `QListView` plus a custom delegate is the direct equivalent. Qt also
  has `QSystemTrayIcon`, HTML preview, drag and drop, translations, `QTcpSocket`
  for Friends sync, and plugin loading to replace add-in DLLs.
- **Window placement.** On X11, Qt can still place a window anywhere, and Ditto's
  "popup at caret/cursor" depends on that. GTK4 dropped window positioning, so
  GTK4 (and gtk4-rs) is ruled out.
- **There's already a model to follow.** CopyQ, the most mature Linux clipboard
  manager, is built on Qt. On Wayland, KDE's `KGuiAddons` (`KSystemClipboard`)
  already implements the data-control protocol.

**Rust is now being evaluated as the alternative.** See
[rust-gui-options.md](rust-gui-options.md) for the GUI toolkit review and the
Rust crate mapping.

### Architecture (applies to either language)

Put everything platform-specific behind interfaces from day one:
`ClipboardMonitor`, `ClipboardOwner`, `HotkeyService`, `PasteInjector`,
`ActiveWindowTracker`, `PopupPlacer`. Each interface reports what it can and
can't do, and the app falls back when something is missing. The X11 version uses
xcb with XFixes, XTest, XGrabKey and EWMH. Wayland is then added later without
redesigning the app.

---

## 2. X11: what ports and what doesn't

### Ports cleanly (no Windows dependency)

History in SQLite, groups, sticky and starred clips, search (plain, regex,
wildcard), quick-paste text, all paste transforms (upper/lower/capitalize/sentence/
camel/invert case, slugify, trim, add/remove line feeds, current time, GUID,
ASCII-only, typoglycemia, posixify paths, multi-image join horizontal/vertical),
multi-select paste, editing and new clips (plain text), import/export, QR code,
web search and translate, clip compare with an external diff tool (meld or
kdiff3), expiry and max-entries cleanup, statistics, themes, transparency,
translations, custom key bindings (`ActionEnums`), Friends network sync
(TCP 23443).

### Ports, but works differently

| Ditto feature (source) | Linux/X11 replacement |
|---|---|
| Clipboard listener, `AddClipboardFormatListener` (`ClipboardViewer.cpp`) | XFixes `SelectionNotify`; must handle large transfers (INCR) |
| Windows formats: CF_UNICODETEXT, CF_DIB, "HTML Format", CF_HDROP, RTF | MIME types: `text/plain;charset=utf-8`, `image/png`, `text/html`, `text/uri-list` plus `x-special/gnome-copied-files`. Old Ditto databases and Friends need a translation table. Friends sends format *names* as strings (`Client.cpp`), so a name↔MIME mapping keeps it compatible with Windows Ditto. |
| Global hotkeys, `RegisterHotKey` (`HotKeys.cpp`) | `XGrabKey`, registered once per NumLock/CapsLock combination. Some desktops already take certain key combos. |
| Paste by sending Ctrl+V: `SendKeys`, per-app paste/copy/cut strings like `^{VKEY86}` (`ExternalWindowTracker.cpp`, `SendKeys.cpp`) | XTest. Windows key codes must be translated to X keysyms. |
| Copy Buffers: send Ctrl+C, capture, restore clipboard (`ClipboardSaveRestore`, `DittoCopyBuffer`) | XTest. The PRIMARY selection is often a better source here. |
| Focus tracking and returning focus to the target window (`GetForegroundWindow`, `SetForegroundWindow`) | `_NET_ACTIVE_WINDOW`. KWin's focus-stealing prevention may get in the way. |
| Show popup at caret: `GetGUIThreadInfo` → `rcCaret` | **Partly.** X11 has no caret. AT-SPI works in GTK, Qt and browser apps but often not in terminals or Electron apps. Fall back to the mouse cursor position. |
| Ignore copies from certain apps, `GetClipboardOwner` | **Partly.** Selection owner → `_NET_WM_PID` → `/proc/<pid>/exe`. The owner is sometimes a hidden helper window. |
| Password-manager exclusion: `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore` | Respect `x-kde-passwordManagerHint=secret` (set by KeePassXC and others) and keep our own ignore format. |
| HTML preview: `SimpleBrowser` (embedded Internet Explorer / MSHTML) | A rich-text widget that supports a subset of HTML, or a webview if full rendering is needed. |
| Email: MAPI `SendMail`, Gmail compose URL | `xdg-email` or `mailto:` |
| Tray icon (`SystemTray`, `NTray`) | StatusNotifierItem. **GNOME needs the AppIndicator extension**, even on X11. |
| Run at startup, show in taskbar, power-resume reconnect (`PowerManager`) | XDG autostart; `_NET_WM_STATE_SKIP_TASKBAR`; not needed |
| Add-ins (`Addins/DittoUtil` DLL) | Existing DLLs can't be loaded. Write a new `.so` plugin API and build the 5 DittoUtil functions into the app (paste any as text, image as HTML `<img>`, remove line feeds, select paste format, read-only flag). |
| Drag clips out (OLE `IDataObject`, CF_HDROP temp files) | XDND drag from our own window |

### Cannot be ported (Windows-only, drop them)

- **Paste as Admin / UAC** (`UAC_Helper`, `UAC_Thread`, action `ELEVATE_PRIVlEGES`).
  Linux has no equivalent integrity levels.
- **Windows metafile images** (CF_ENHMETAFILE, CF_METAFILEPICT).
- **RTF editing** (`RichEditCtrlEx`, `RulerRichEditCtrl`). No Linux toolkit ships
  an RTF editor. RTF clips can be stored and pasted back unchanged, but editing and
  preview would have to be plain text or HTML.
- **Existing Windows add-in DLLs**, and ChaiScript scripts that use Windows format
  names. Scripts can be kept working with a mapping layer, but only if ChaiScript is kept.
- **Windows Store app mode, U3 USB launcher (`U3Stop`), the Windows ICU loader
  (`ICU_Loader`) and the DPI manifest.** None are needed.
- **`focusdll` and `FocusHighlight`** (a hook DLL injected into other processes).
  Linux has no equivalent, but nothing in `src/` calls it any more.

### Needed on X11 that Ditto doesn't have

- **Keep the clipboard alive when the source app quits.** On X11 the clipboard
  contents vanish when the app that copied them closes. The manager must fetch all
  formats and take ownership itself (or support the `SAVE_TARGETS` protocol).
- **Optionally track the PRIMARY selection** (the text you've just highlighted).

---

## 3. Wayland: what ports and what doesn't

Wayland's core clipboard only works for the window that has focus, so a
background clipboard manager has to rely on extra protocols. This breaks Ditto's
main loop of *watch the clipboard → popup at caret → send Ctrl+V*.

| Feature | Wayland status |
|---|---|
| Watching the clipboard in the background | ✅ via `ext-data-control-v1` or `wlr-data-control` (KWin, Sway, Hyprland, labwc, niri, COSMIC). **❌ GNOME/Mutter doesn't support it** (as far as known), so GNOME would need a companion Shell extension talking to the app over D-Bus. Flatpak-sandboxed clients can't use data-control. |
| Keeping the clipboard alive, setting it from history, PRIMARY | ✅ same protocol. Reading PRIMARY can replace the Ctrl+C trick used by Copy Buffers. |
| Global hotkey to open the popup | ⚠️ GlobalShortcuts portal (KDE, GNOME 48+, Hyprland). Fallback: the user binds a compositor shortcut to `feritto --toggle`. |
| Per-clip global hotkeys, "paste position 1–10", move-to-group hotkeys | ⚠️ Each needs its own portal registration, or manual binding. The current dynamic setup gets much clumsier. |
| **Sending Ctrl+V / Ctrl+C / custom keystrokes** | ⚠️ No XTest. Options: RemoteDesktop portal with libei (shows a permission prompt), `zwp_virtual_keyboard_v1` (wlroots only), or uinput/ydotool (needs `/dev/uinput` access). Fallback mode: "copy to clipboard only, the user presses Ctrl+V". |
| **Active window, per-app paste strings, ignore-from-app, `GetActiveApp()` in scripts** | ⚠️/❌ `wlr-foreign-toplevel` on wlroots, Plasma's own protocol on KDE, nothing on GNOME. The clipboard protocol never says which app copied something, so only the password-manager hint MIME type is reliable. |
| **Popup at caret, at cursor, or at a remembered position** | ❌ Normal Wayland windows can't choose their position and can't read the global cursor position. `wlr-layer-shell` (not available on GNOME) allows a fixed-position overlay. Caret position from AT-SPI isn't in screen coordinates on Wayland. |
| Magnetic/snap windows, always-on-top, show/hide in taskbar | ❌ No standard protocol (layer-shell is a partial substitute). |
| Returning focus to the previous app after the popup | ✅ mostly. The compositor usually does it when our window closes. Raising the popup itself needs an `xdg-activation` token, and GNOME may show "is ready" instead of focusing it. |
| Transparency, scaling, drag and drop from the popup, tray (SNI) | ✅ |
| Database, search, groups, transforms, scripts, Friends, export, QR | ✅ No platform dependency. |
| Xwayland apps | Clipboard is bridged, but X11 features like XTest only reach other Xwayland windows, so they don't help here. |

**What this means for Wayland:** expect a tiered experience. KDE and wlroots
compositors can get close to full Ditto behaviour, minus caret positioning. GNOME
needs a Shell extension to be usable. The "paste into the previous window" step
depends on portals or uinput everywhere.
