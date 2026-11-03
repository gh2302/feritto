# Rust GUI options for feritto

**Goal:** the fastest, cleanest, most minimalist UI for a Ditto-style clipboard popup.

Starting point: the article
[Top GUI libraries and frameworks for Rust](https://simplifycpp.org/articles/a0507/top-gui-libraries-and-frameworks-for-rust-a-comprehensive-guide/).
It covers Tauri, Druid, Iced, egui, GTK-rs, Azul and Conrod. It lists no
downsides for any of them and is out of date: three of its seven picks are no
longer maintained or have stalled, and it leaves out Slint, which is a strong
fit here. The review below re-evaluates its list and adds the missing candidates.

---

## What the UI must do (from the Ditto analysis)

1. **Appear instantly on a hotkey.** In practice the window is created once and
   shown/hidden, so show latency and idle memory matter more than cold start. The
   app lives in the tray all day.
2. **Place itself at the caret or cursor on X11.** It needs an absolute position
   and a popup/utility window type, without taking a taskbar entry.
3. **Wayland later.** The only way to position a popup there is `wlr-layer-shell`
   (KDE, wlroots compositors, COSMIC; not GNOME).
4. **Scroll a long history smoothly** (thousands of rows, live search filter), so
   it needs a virtualized list or one we can build easily.
5. Keyboard-first: type-to-search, IME input, arrow and number keys.
6. Previews: text, images, light rich text. Plus a small settings window.
7. Tray, hotkeys and the clipboard are **not** the GUI toolkit's job. They're
   handled by separate crates (see the end of this document).

---

## Candidates from the article

| Toolkit | Verdict | Why |
|---|---|---|
| **Tauri** | ❌ | It's a webview app (WebKitGTK on Linux): high RAM for something always running, a slower first paint, plus an HTML/JS frontend. Too heavy for a popup. |
| **Druid** | ❌ | No longer maintained. Its authors replaced it with Xilem, which is still experimental. |
| **Iced** | ✅ **Top pick** | See below. |
| **egui** | ✅ Runner-up | See below. |
| **GTK-rs (gtk4-rs / Relm4)** | ❌ for this project | Mature, and the most native look on GNOME. But GTK4 can't position windows, which kills the X11 "popup at caret" feature, and GTK deprecated its X11 backend in 4.17 (planned for removal in GTK 5). Only reasonable for a Wayland-only build using gtk4-layer-shell. |
| **Azul** | ❌ | Development has stalled, and the ecosystem is small. |
| **Conrod** | ❌ | Unmaintained; superseded by newer immediate-mode crates. |

## Candidates the article missed

| Toolkit | Verdict | Why |
|---|---|---|
| **Slint** | ✅ Strong alternative | See below. |
| **fltk-rs** | ⚠️ | The smallest binary and fastest start of the group, and window positioning is trivial. But it looks dated, and getting a "clean modern" look takes theming effort. |
| **GPUI** (Zed's toolkit) | ⚠️ | Very fast, with excellent text rendering. But it requires a GPU with Vulkan, its API changes often, its docs are thin, and it's shaped around Zed's needs. |
| **Dioxus desktop / Freya** | ❌ | Dioxus desktop is a webview, like Tauri. Freya uses Skia, which brings a heavy native build. |
| **Xilem / Floem / Vizia / Makepad** | ❌ for now | Interesting, but not stable or proven enough for a product. |
| **cxx-qt** (Qt from Rust) | ⚠️ | Keeps all of Qt's advantages, but the build mixes C++, Rust and QML. If we want Qt, we might as well write C++. |

---

## The three real contenders

### 1. iced — recommended

- **Clean code.** It uses the Elm architecture (`State` → `Message` → `update` →
  `view`), which keeps the UI predictable and easy to test. That's the cleanest
  model of the three for a keyboard-driven list app.
- **Clean look.** The default theme is flat and minimal, and it's easy to restyle.
- **Speed.** It redraws only when something changes. It renders with wgpu by
  default and can fall back to tiny-skia (CPU), which avoids GPU startup cost and
  memory on weak machines.
- **X11.** It runs on winit, which exposes absolute positioning, popup/utility
  window types and override-redirect.
- **Wayland, the deciding factor.** It's the only one of the three with a working
  layer-shell path: [`iced_layershell`](https://docs.rs/iced_layershell/latest/iced_layershell/),
  or System76's `libcosmic` built on iced. The COSMIC desktop is built on iced,
  and [its clipboard manager applet](https://github.com/cosmic-utils/clipboard-manager)
  (iced + libcosmic + SQLite + data-control) shows the whole stack works for this
  exact kind of app.
- **Downsides:**
  - Still pre-1.0, with breaking changes in every release (0.14 is current).
    `iced_layershell` releases lag behind iced, so we'd pin versions.
  - No built-in virtualized list. With fixed-height rows it's easy to build:
    query only the visible window of rows from SQLite and pad the rest with spacers.
  - Accessibility support is still immature.

### 2. Slint — choose this if the lowest footprint matters more than Wayland positioning

- **The lightest option.** It was designed for embedded devices and has a pure
  software renderer, so there's no GPU startup and idle memory is very low.
- **A clean split between UI and logic.** The UI is written in a declarative
  `.slint` language with a live preview. It has a built-in **virtualized `ListView`**,
  a stable 1.x API, and AccessKit accessibility support.
- **X11:** the winit backend handles positioning.
- **Downsides:**
  - **No official layer-shell support.** The third-party
    [layer-shika](https://docs.rs/crate/layer-shika/latest) is early development
    and not production-ready, so Wayland popup placement is a real gap.
  - Licensing: GPLv3, a royalty-free license that requires attribution, or a
    commercial license. Fine for open source, but worth deciding up front.
  - `.slint` is one more language to learn.

### 3. egui — fastest to build, least polished look

- Immediate mode means the least code and fastest iteration of the three.
  `ScrollArea::show_rows` gives virtualization for free.
- Good fit for the settings window or a quick prototype.
- **Downsides:**
  - The default look is "developer tool", so reaching a polished minimalist style
    takes real styling work.
  - It re-lays out the whole UI on every frame. That's cheap, but less tidy than
    retained or Elm-style code as the app grows.
  - IME and text editing are weaker.
  - No maintained layer-shell integration.

---

## Image previews

All three can show image clips: a thumbnail in each list row and a large preview.

| | iced | Slint | egui |
|---|---|---|---|
| How | `image` widget (`image` feature): `image::Handle::from_bytes(png)` or `from_rgba(w, h, px)`; also an `svg` widget | `Image` element; `slint::Image::from_rgba8(SharedPixelBuffer)` (decode in-memory bytes with the `image` crate yourself) | `egui_extras::install_image_loaders` + `egui::Image::from_bytes(uri, bytes)`, or `ctx.load_texture(ColorImage)` |
| Formats | PNG, JPEG, BMP, GIF (first frame), WebP, SVG | PNG, JPEG, SVG built in; others via the `image` crate | PNG, JPEG, GIF, WebP, SVG via loader features |
| Animated GIF | Third-party crate (`iced_gif`) | Not built in | Supported by the loaders in recent versions |

Same plan whichever toolkit we pick:

- **Make the thumbnail once, when the clip is copied.** Store a small PNG in the
  database; never decode a 4K screenshot just to draw a list row.
- **Decode on a background thread** and keep a small cache of image handles for
  the rows on screen.
- **Downscale large images before display**, since GPU renderers cap texture size
  (wgpu typically at 8192 px).
- **Convert BMP/DIB clips to PNG at capture**, e.g. ones that arrive via Xwayland or
  are imported from Ditto.

## Recommendation

**iced** (tiny-skia renderer by default, wgpu optional) for both the popup and
the settings window.

- **Why:** it gives the cleanest code and look of the three, it's fast enough,
  and it's the only one with a credible Wayland popup-placement path.
- **When to switch to Slint:** if Wayland positioning ends up out of scope, or
  idle memory turns out to matter most.
- **How to keep switching cheap:** keep the GUI a thin layer over a core crate
  that owns the database, search and actions.

## If we go with Rust: crates for the non-GUI parts

| Concern | Crate |
|---|---|
| X11 clipboard (XFixes, INCR), XTest, XGrabKey, EWMH | `x11rb` |
| Wayland data-control | `wayland-client` + `wayland-protocols` (ext/wlr data-control), or `wl-clipboard-rs` |
| Portals (GlobalShortcuts, RemoteDesktop) | `ashpd` |
| Caret position | `atspi` |
| Tray (StatusNotifierItem over D-Bus, no GTK) | `ksni` |
| Storage | `rusqlite` (bundled SQLite; SQLCipher feature if we want encryption) |
| Scripting (replaces ChaiScript) | `rhai` |
| QR codes | `qrcode` |
| Friends sync | `tokio` + `rustls`, or a Noise transport, plus an optional legacy Ditto protocol mode |

**What changes from the C++/Qt plan in `ditto-port-analysis.md`:**

- **ChaiScript compatibility is lost.** Users' existing Ditto scripts would need
  rewriting in Rhai, which has a similar syntax.
- **HTML preview is limited.** None of the three toolkits renders HTML, so HTML
  clips preview as text, or as simplified rich text in iced.
- **What stays the same:** every X11 and Wayland conclusion in
  `ditto-port-analysis.md`, because those come from the platforms, not the toolkit.
