# Ditto → Rust + iced porting plan

Prepared: 2026-11-03. Status: proposed implementation plan; no application code
exists in this repository yet.

Critically reviewed in an independent read-only pass; the corrections and remaining feasibility
decisions are recorded in [the review resolutions](ditto-rust-iced-review.md).

This plan builds on [the Ditto port analysis](ditto-port-analysis.md) and
[the Rust GUI review](rust-gui-options.md). It adopts Rust and iced, with an
X11 release first and capability-based Wayland support afterward. It is a
rewrite of Ditto's behavior and data model, not a translation of its MFC UI.

## 1. Decisions and release boundaries

- Use **Rust and iced for both the popup and settings**. Keep the application
  core independent of iced, X11, Wayland, and database connections.
- Start with **iced 0.14.0**, a pinned, tested stable Rust toolchain, and a
  committed `Cargo.lock`. iced requires Rust 1.88 or newer; the complete
  dependency graph may require a newer compiler. Freeze the working combination
  after milestone M0 rather than tracking upstream development branches.
- Make **tiny-skia the initial renderer**, with wgpu an optional build feature.
  Disable iced's default features and select the renderer explicitly; simply
  adding tiny-skia would leave wgpu enabled by default. Measure both in M0.
  The initial feature set is `tiny-skia`, `x11`, `tokio`,
  `image-without-codecs`, and `advanced`, with selected image codecs enabled on
  the separate `image` dependency. Add shaping/theme features after checking the
  prototype; enable debug tooling only in development. Leave iced's `wayland`
  feature out of the X11 build and check transitive features as well.
- Own clipboard monitoring, selection serving, hotkeys, focus, and paste through
  a native platform adapter. iced's clipboard helpers alone cannot implement a
  clipboard manager.
- Use a **native feritto database**, with a read-only Ditto importer. Retain
  original payloads and format names; derived Linux representations are separate.
- Build the portable DittoUtil transformations into the action system; defer
  its two Windows file-attribute actions explicitly, as described in M5. Use
  **Rhai for new scripts later**; existing ChaiScript programs and Windows DLLs
  do not run.
- Treat **Windows Friends interoperability as a separate compatibility project**.
  A new TLS transport does not implement Ditto's existing wire protocol.

| Release boundary | Included | Deferred |
|---|---|---|
| X11 MVP, after M3 | Capture and retain configured clipboard formats; text/image previews; literal search; keyboard navigation; one global popup hotkey; tray and CLI control; cursor placement; clipboard ownership and automatic paste where supported; basic settings and cleanup | Ditto import, advanced organization/actions, scripting, networking, Wayland |
| X11 beta, after M4 | Ditto import; groups, sticky/starred clips and ordering; native export/restore; XDG integration; recovery and performance qualification | Remaining Ditto actions, scripting, networking, Wayland |
| Extended X11, M5–M6 | Advanced search and actions; editing; copy buffers; Rhai; native Friends sync; legacy Friends only after its own interoperability gate | Windows-only features and an unrestricted native plugin ABI |
| Wayland, M7 | Shared core/UI with runtime protocol detection and explicit reduced-function modes | Universal caret positioning, universal input injection, or promised GNOME integration without an implementation |

The MVP must already respect secret clipboard hints, suppress its own capture
loops, handle large transfers, and preserve fully captured configured formats
when their source exits, subject to the coexistence policy below. Those are
correctness requirements, not later polish.

The first release targets native X11 sessions on **Xfce/Xfwm and i3**. Cinnamon,
MATE, and maintained KWin-X11 packages can be qualified additionally. Record
actual distro/desktop versions in M0; do not rely on future GNOME or Plasma
releases retaining Xorg sessions. The X11 build refuses to start the desktop
service in a Wayland session with a clear diagnostic; it does not silently
present Xwayland-only behavior as support for native applications.

## 2. Verify the hard assumptions first

The existing analyses identify the correct problem areas, but do not establish
that every native window operation is exposed through iced or that every Ditto
format can be imported unchanged.

Published metadata checked for this plan shows iced **0.14.0** and
iced_layershell **0.19.1**; the latter depends on iced **0.14** components.
That is a starting combination, not proof that rendering, settings windows,
and both event-loop runners work together.

M0 must answer these questions with small, runnable spikes:

1. **Can an iced popup behave like Ditto on X11?** Create it hidden, obtain its
   native X11 handle through iced's supported window API, apply the necessary
   EWMH hints, and repeatedly show/hide it without exiting the process. Verify
   keyboard focus, IME, no unwanted taskbar entry, multi-monitor placement, and
   focus restoration under Xfwm and i3, plus KWin-X11 where available. Prefer a
   managed utility window; test override-redirect separately because it changes
   focus/IME behavior.
2. **Can tiny-skia meet the UI budget?** Test text, thumbnails, mixed scripts,
   scrolling, transparency with a compositor, and an opaque fallback without
   one. Use a fixed-height list prototype with 100,000 synthetic clip IDs and
   benchmark bounded search text with SQLite scans and an FTS5 trigram index.
3. **Can the platform service capture and serve the configured formats?**
   Exercise the allow-list, alias/image preferences, and budgets below. Transfer
   text, PNG, HTML, URI lists, and opaque RTF, including incoming and outgoing
   INCR. Test eager capture without stealing ownership, recovery after source
   destruction, explicit clipboard clearing, and `SAVE_TARGETS` independently.
4. **What is the actual Ditto import contract?** Inspect `DatabaseUtilities.cpp`,
   `Clip.cpp`, and associated serialization at a pinned source revision. Record
   schema versions, blob compression/encoding, IDs, groups, ordering, timestamps,
   sticky/star flags, and format naming. Collect reproducible fixture databases.
5. **Does the Wayland path have a viable runner?** Compile a minimal
   iced_layershell surface with the chosen iced versions and identify renderer
   requirements, keyboard interactivity, text-input-v3/IME behavior, and how
   ordinary settings windows will coexist. Layer-shell is a different
   shell/event-loop integration, not a flag on a winit window. A limitation here
   can defer Wayland without blocking the X11 product.

Reference Ditto revision retrieved while preparing this plan:
[`a80fd3536bc407cac8cce43b5682022282ace983`](https://github.com/sabrogden/Ditto/commit/a80fd3536bc407cac8cce43b5682022282ace983).
The original analysis did not pin its clone; recheck its assumptions against
this revision or record the replacement revision. Settle the project license
and attribution approach before translating upstream code or copying resources;
Ditto includes GPLv3 licensing. A functionally compatible independent
implementation and a source-derived port need different reuse records.

**M0 exit:** a checked-in compatibility note, runnable spikes, measurements,
and a dependency lock. If an essential X11 operation needs an iced patch, scope
that patch and its maintenance cost before building the application around it.
Do not silently switch toolkits or declare Wayland supported on compile success.

| M0 spike | Pass criterion | Decision if it fails |
|---|---|---|
| Popup/window integration | At least 100 show/hide cycles on each primary WM, with correct focus/IME, taskbar behavior, placement, and focus-denial fallback; no input reaches the wrong target | Test override-redirect and document its focus/IME costs. If neither approach meets required keyboard/IME behavior, block M1 UI integration and scope an iced patch; any toolkit change needs an explicit revised project decision |
| Renderer/list/search | Meet the provisional warm-popup, scrolling, memory, and search targets in section 9 on a declared reference setup | Compare tiny-skia with wgpu and select the measured default; record any revised budget and reason before M1. Do not trade clipboard correctness for a benchmark |
| Clipboard service | Complete configured multi-format transfers including a 20 MiB INCR fixture; preserve eligible data after source exit; never overwrite a newer copy or resurrect an intentional clear; coexist without ownership loops | Block M2 until the state machine is corrected. A history-only mode may be documented but does not pass the full persistence gate |
| Import contract | Fixture inventory maps group IDs, all four order keys, protection, shortcuts, padding, and unsupported/internal formats to the proposed schema | Preserve unsupported representations opaquely; block the relevant M1 schema decision until metadata has a lossless mapping. Import UI remains deferred to M4 |
| Wayland runner | Pinned runners build, with a runtime proof of keyboard input and a recorded settings-window/IME strategy on one supporting compositor | Defer M7 or narrow its declared capabilities; continue X11. Revalidate versions and runtime behavior when M7 starts |

## 3. Workspace and ownership

Start with five crates. Add scripting/networking crates only when their
milestones begin.

```text
crates/
  feritto-core/       clip/action types, policies, transforms, service contracts
  feritto-store/      SQLite repository, migrations, search, Ditto import/export
  feritto-platform/   X11 adapter, tray, capabilities; later Wayland/portals
  feritto-ui/         iced state, messages, views, virtual list, settings
  feritto/            executable, composition, IPC, lifecycle, configuration
```

`feritto-core` defines repository and platform contracts in application terms.
It does not depend on iced, rusqlite, X11 window types, or Wayland objects.
`feritto-ui` receives summaries and capabilities and emits typed intents.
The executable connects these to storage and platform services.

Keep the six platform interfaces from the analysis:

| Interface | Responsibility and result |
|---|---|
| `ClipboardMonitor` | Observe generations, capture offers, report exclusions and incomplete transfers |
| `ClipboardOwner` | Publish immutable multi-format offers, serve requests, persist independently of popup visibility |
| `HotkeyService` | Register actions, report conflicts, unregister/reload bindings |
| `PasteInjector` | Execute layout-aware key sequences against an identified target, or report unavailable |
| `ActiveWindowTracker` | Snapshot and validate a target, restore focus where supported, report best-effort source identity |
| `PopupPlacer` | Place at caret/cursor/remembered position when possible; otherwise choose a supported presentation |

Capabilities distinguish **available**, **permission required**, and
**unavailable with a reason**. Source identification and caret placement also
report their confidence/fallback. Recompute capabilities after reconnects or
portal-session changes. UI actions use these results rather than assuming that
every service works because a session is labeled X11 or Wayland.

Use iced's Elm model: `State` → `Message` → `update` → `view`, with effects
returned as tasks and service events delivered by subscriptions. Keep the GUI
event loop on the main thread. Run X11 selection handling on a dedicated worker;
use a database worker for writes and bounded background work for queries and
image decoding. Start with iced's Tokio executor integration rather than
creating competing async runtimes.

Pass immutable clip references, summaries, request IDs, and bounded payload
handles between services. Never block the X11 event loop on SQL or image decoding.
Bound queues and transfer sizes; report incomplete capture when limits are hit.
Only expendable work such as obsolete searches/thumbnails may be coalesced
silently. A hidden popup must leave clipboard serving and monitoring running.

## 4. Native data model and Ditto compatibility

Use `rusqlite` with bundled SQLite, migrations, foreign keys, and WAL. A single
writer serializes changes; background reads return short-lived snapshots.
Keep files in XDG data/config/runtime locations. Take recoverable backups before
migration and refuse unsupported newer database versions.

Use private directories (0700) and data, sidecar, payload, export, and backup
files accessible only to the user (0600). Default storage is not encrypted;
SQLCipher and payload encryption are deferred until a complete key/lock lifecycle
is designed. Encrypting SQLite alone would not protect external payload files.

The initial schema should separate:

- **Clips:** stable IDs, capture/use times, bounded display/search text, content
  hash, source identity when known, one parent group, starred/auto-delete
  protection, and separate global/group and sticky-global/sticky-group positions.
- **Representations:** original format/target name, canonical MIME when known,
  raw bytes, and provenance. Preserve unknown formats; do not force every clip
  into a text-or-image enum.
- **Derived assets:** thumbnails, normalized images, and safe preview text,
  versioned so they can be rebuilt without losing the original payload.
- **Groups and settings:** hierarchy and membership semantics confirmed against
  Ditto, plus native preferences, capture-target policy, and action bindings.
- **Import bookkeeping:** source snapshot identity, source-to-native ID mapping,
  conversion diagnostics, and resumable batch state.

Keep small payloads in a separate SQLite BLOB table. Initially spill payloads
over 1 MiB to content-addressed files so large images do not repeatedly fill the
SQLite WAL/page cache; validate that threshold in M0/M1. Publish a flushed
temporary file by atomic rename before committing its reference. Recover orphan
files after crashes, lease files needed by active offers/exports, and back up
the database and referenced files as one snapshot. Do not eagerly map every blob
into memory to serve a selection.

Hash the canonical configured format set and logical bytes, excluding derived
aliases and allocation padding, in a deterministic order for deduplication.
Do not merge two rich clips solely because their plain-text representations
match. Keep volatile owner/timestamp markers out of content hashes. Apply count,
age, and byte-size retention policies transactionally, protecting starred,
sticky, grouped, and shortcut-bound clips by default, and retaining leases for
active clipboard offers. Provide clear-history and clear-clipboard actions with
distinct semantics, and an optional clear-on-exit setting. Logical deletion is
not secure erasure of backups or filesystem snapshots. Repeated copies update
recency according to a documented policy; PRIMARY uses a separate opt-in policy.

### Confirmed Ditto schema requirements

At the pinned revision, groups are `Main` rows with `bIsGroup=1` and hierarchy
through `lParentID`; each clip has at most one parent group. Preserve the four
`REAL` fields `clipOrder`, `clipGroupOrder`, `stickyClipOrder`, and
`stickyClipGroupOrder` independently. Normalize legacy null/zero and
`-2147483647` non-sticky values using schema-version fixtures, while retaining
the original ordering metadata for diagnostics.

Ditto's star/never-auto-delete state is `lDontAutoDelete > 0`; retain its timestamp
as the native protection value, rather than importing it as two independent
flags. Retain `lShortCut`, `globalShortCut`, `MoveToGroupShortCut`,
`GlobalMoveToGroupShortCut`, `QuickPasteText`, `lastPasteDate`, and `CRC`, even
before the corresponding action exists. Windows shortcut codes remain imported
metadata until explicitly translated to Linux bindings.

Join `Data` through live `Main` rows: deferred deletion through `MainDeletes`
leaves orphan payloads that must not be resurrected. Inspect available columns
for older schemas and supply documented defaults; never call Ditto's mutating
`ValidDB` upgrade/validation routine on the source. Payload lengths can include
`GlobalSize` allocation padding: validate format boundaries, stop Windows text
at a bounded NUL terminator, and compute image data extents from validated headers
while retaining the original blob separately.

### Representation mapping

| Ditto / Windows representation | Native treatment |
|---|---|
| `CF_UNICODETEXT` | Decode UTF-16LE and terminators correctly; offer UTF-8 text aliases while retaining the source payload |
| `CF_TEXT` and legacy text | Prefer `CF_UNICODETEXT`; use a known code page for a legacy-only clip, or request an explicit import choice. Ditto may not store `CF_LOCALE`; report ambiguity instead of silently corrupting bytes |
| `CF_DIB` / `CF_DIBV5` | Parse the DIB header, palette, masks, stride, orientation, and alpha; derive PNG. A DIB is not a BMP file with a complete file header |
| `PNG` | Validate and retain the original PNG; offer `image/png` and derive thumbnails without unnecessary re-encoding |
| `HTML Format` | Parse CF_HTML byte offsets and fragment boundaries, validate bounds, derive `text/html`; retain original bytes |
| RTF | Preserve and offer `text/rtf` aliases; preview fallback text. No RTF editor is promised |
| `CF_HDROP` | Decode the file list; derive `text/uri-list` and `x-special/gnome-copied-files` copy offers only for meaningful local paths. Preserve Windows paths and offer explicit path mapping; never imply those files exist locally or invent cut semantics |
| Custom formats | Preserve name and bytes; expose only through known-safe mappings or a selected original format |
| Windows metafiles | Preserve as unsupported imported data if present; no rendering or native paste conversion |

Classify `Ditto Delay Saving Data`, `Ditto File Data`, `Ditto Remote CF_HDROP`,
`Ditto Ping Format`, and `Clipboard Viewer Ignore` separately. Retain them for
import diagnostics where appropriate; do not advertise control markers as user
data, execute delayed operations, or extract bundled files implicitly.

For native X11 capture, distinguish MIME data targets from protocol targets such
as `TARGETS` and `TIMESTAMP`; handle `UTF8_STRING`, legacy text encodings, and
aliases deliberately. Preserve useful MIME parameters. Do not mistake selection
bookkeeping for clip content.

The initial importer requires **a quiescent source from closed Ditto or a
validated backup**. Copy the main database and any matching `-journal`/`-wal`
sidecars into private staging, preserving names. Open only that copy read-write
so SQLite can recover a hot rollback journal, then use SQLite backup facilities
to make the clean import snapshot. Never open the user's source with write
access or copy a changing file set. Any later live-import path must use SQLite's
locking/online-backup API and handle read-only recovery failures explicitly.

Identify content, not the filename extension: SQLite databases can have legacy
`.mdb` names; actual Access/Jet/ACE databases are unsupported. Report encrypted,
corrupt, or unrecognized databases distinctly where detection allows. Upstream
vendors SQLite3 Multiple Ciphers, which is not a promise that bundled rusqlite
can open every encrypted database.

Stage transactional batches, resolve group/ID references, and publish only
consistent imported records.
Cancellation/retry must not create duplicate batches. Report imported, converted,
unsupported, and failed items; export the report without clipboard contents.

Support native export/restore separately. Writing databases that Windows Ditto
can open is not part of the initial import promise. Keep fixture-based format
converters reusable by the later legacy Friends adapter.

## 5. X11 clipboard and paste lifecycle

Implement X11 with `x11rb`: XFixes for selection changes, ICCCM selection
protocols for transfer, XGrabKey for shortcuts, XTest for input, and EWMH for
focus/window behavior. Verify both the session and the actual GUI backend;
the presence of `DISPLAY` inside a Wayland session is not sufficient.

### Configured capture policy

Use a persisted allow-list by format family, comparable to Ditto's `Types`
settings. Initially enable text, one preferred lossless image representation,
HTML, RTF, and URI/file-copy formats. Resolve text aliases by preference and
encoding; fetch another alias only when necessary, not all synonymous targets.
Prefer PNG when available and derive alternative supported image offers rather
than asking the source to render every encoding. Never substitute a lossy image
representation for an original without recording it.

Skip protocol targets and private/transient application targets by default.
Users may add explicit extra targets; accepted opaque bytes are preserved, but
unknown advertised formats are not fetched automatically. Imported blobs remain
preserved regardless of the native capture allow-list. Capture-policy version
and accepted representations define the format set used for deduplication.

Start M0 with configurable budgets of 32 MiB per target, 64 MiB per generation,
2 seconds without transfer progress, and 10 seconds total per generation.
Calibrate and freeze the defaults against the real-app matrix. Exceeding a limit
produces an incomplete-capture result; it does not qualify that generation for
automatic persistence recovery. Bound concurrent transfers and decoded images
separately from wire-byte limits.

### Capture and persistence

1. On a selection-owner change, assign a generation and snapshot owner/source
   information. Ignore the app's own owner window and recognized internal
   transfers, without trusting a custom marker as the only loop guard. Capture
   eagerly while leaving ownership with the source; do not claim CLIPBOARD on
   every copy or replace the source's lazily rendered formats.
2. Discover targets and check secret/ignore hints before collecting content.
   Respect `x-kde-passwordManagerHint=secret`, feritto's ignore marker, configured
   application exclusions when identity is known, and a global pause control.
3. Fetch policy-selected targets with bounded time and memory, including INCR.
   If the owner changes or disappears, cancel stale work. Never combine formats
   from different clipboard generations. Record any omitted or failed targets.
4. Commit captured representations atomically; derive thumbnails asynchronously.
   Do not store payloads or previews in diagnostic logs.
5. On XFixes owner-window destruction/client-close, reclaim only the last fully
   captured eligible generation, provided the selection is still unowned and no
   newer generation has appeared. Use server timestamps and generation guards;
   M0 must prove a concurrent new copy cannot be overwritten. An explicit clear
   or secret/ignored generation must never resurrect older content. Observe
   owner-loss event reasons rather than treating every `None` owner as an exit.
6. Separately, implement `SAVE_TARGETS` for cooperative source shutdown when
   feritto owns `CLIPBOARD_MANAGER`. Honor the requested target set within
   supported policy/budgets and report the targets actually retained. Sources
   that do not request this protocol still rely on eager capture and step 5.

Detect existing managers through selection ownership, desktop configuration,
and observed ownership contention; `CLIPBOARD_MANAGER` alone is insufficient.
Offer a coexistence/history mode, and disable automatic reclamation if ownership
fights occur instead of entering a loop. Process-name heuristics alone are not
authoritative. State the reduced persistence guarantee in coexistence mode.
Data a source never finished delivering cannot be recovered after it exits.

Selection serving must support concurrent requests, correct timestamps, ordinary
transfers, INCR, and the protocol targets needed by real clients, including
`MULTIPLE` when requested. Load or retain a historical offer before claiming
ownership. Keep serving it while the popup is hidden and during history cleanup.
On orderly shutdown, attempt clipboard handoff to another manager if available;
document that ownership cannot survive the last serving process exiting.

PRIMARY monitoring is off by default and separate from CLIPBOARD. Debounce
selection churn and avoid silently copying every transient text selection.

### Show and paste

1. A hotkey or `feritto --toggle` records the target window **before** opening
   the popup, including the triggering X timestamp when available. Register
   CapsLock/NumLock variants and report key conflicts. Re-resolve bindings on
   keyboard mapping/group changes.
2. Default to pointer placement. Offer measured opt-in AT-SPI caret lookup with
   an initial 20 ms deadline inside the warm-popup budget; fall back without
   blocking on accessibility bridges or moving the window after interaction
   starts. Clamp to the monitor work area with correct logical/physical scaling.
3. Before each show, reapply necessary EWMH state/hints through x11rb and complete
   a round trip before asking iced to map the window. Obtain the XID from
   `window::run` and the typed Xlib/Xcb raw-window handle, not an assumed numeric
   `WindowId`. Use iced `window::set_mode` for hidden/windowed lifecycle so its
   runner stays in sync. Withdrawn windows may lose WM state.
4. Set `_NET_WM_USER_TIME` and request `_NET_ACTIVE_WINDOW` with the latest valid
   user timestamp, the requestor's active window, and source indication 1 for an
   application. Verify focus before accepting keyboard input; do not pretend to
   be a pager to bypass WM policy. Record the direct focus/grab strategy and IME
   consequences if M0 selects override-redirect.
5. On acceptance, finish the selected transforms, publish the resulting offer,
   and confirm ownership of the selection(s) required by the paste profile.
   Keep the service alive while hiding the popup.
6. Request focus for the saved target using the acceptance event's user timestamp
   and confirm the active window within a bounded deadline. Wait for shortcut
   modifiers to be released, then inject
   the configured sequence. Resolve shortcut keysyms across XKB groups, including
   Latin shortcuts under non-Latin active layouts; do not assume the current
   group contains `v`. Avoid temporary global keymap mutation as a normal path.
7. If the target vanished, focus was refused, or injection is unsupported, leave
   the clipboard ready and report “Copied; press your paste shortcut.” Never send
   keystrokes to an unverified window or report automatic paste as successful.

Escape hides without changing the clipboard. Losing popup focus must not inject
input. Support per-application paste sequences only when target identity is
available. Each profile specifies CLIPBOARD and/or PRIMARY: Shift+Insert in
terminals such as xterm may consume PRIMARY. Publishing to PRIMARY for an
explicit paste is independent of enabling background PRIMARY monitoring.
Copy buffers later extend the same state machine with capture and restore; they
must detect intervening user copies before restoring old data.

Use `ksni` for a GTK-free StatusNotifierItem tray. Provide CLI/desktop-launcher
access because some desktops, particularly GNOME without an extension, do not
display it. Use a per-user single-instance endpoint in the XDG runtime directory;
secondary invocations forward `--toggle`, `--show`, or `--quit` to the owner.

## 6. iced UI and search

Create a persistent application/daemon with a reusable popup and an independent
settings window. Closing a window is distinct from quitting the process. Keep
service startup independent of tray availability and popup visibility.

The popup starts with a search field and fixed-height rows containing a bounded
text preview or cached image thumbnail, format/group indicators, and optional
numeric shortcuts. Put expanded previews in a separate pane; variable-height
rows would complicate the first virtual list.

Build an explicit virtual-list controller: viewport offset → visible range plus
overscan → summaries and thumbnail handles. Represent off-screen rows with
spacers; instantiate widgets only for the visible range. Keep selection by clip
ID, handle updates during scrolling, and ensure arrow/PageUp/PageDown/Home/End
navigation scrolls the selected row into view. Use cursors or a compact ordered
ID snapshot to avoid repeated deep `OFFSET` scans. Do not load every clip blob
or create one widget per database row.

Route Home/End to editing while the search field has focus, and to list
navigation while the list has focus. Qualify spacer geometry through 100,000
rows; above that, use a row-indexed scrolling model rather than assuming f32
pixel offsets remain precise for arbitrarily long histories.

Debounce search initially at about 75 ms, attach a generation to every request,
and discard stale responses. Persist normalized/case-folded search text capped
initially at 64 KiB per clip, truncated on a UTF-8 boundary. The original text
remains intact; indicate truncation because matches beyond that prefix are not
included in ordinary search. An explicit slower full-clip search can be added
in M5, outside the interactive-search latency budget.

Define literal Unicode-aware substring semantics and normalize queries in Rust,
rather than relying on SQLite's ASCII-only default case folding. Benchmark
FTS5's **trigram** tokenizer as an accelerator for substrings of three or more
characters, with literal-query escaping and a fallback for shorter queries.
Verify indexed results against the defined normalized substring predicate;
ordinary FTS5 token search does not supply those semantics automatically.
Keep search data on disk and the query page cache bounded, rather than loading
the entire searchable corpus into RAM. Add regex and wildcard modes in M5 with
bounded pattern work and visible errors, without changing ordinary search.

Decode/downscale images in background jobs with pixel/dimension limits. Store
small PNG thumbnails once, decode them off-thread to RGBA, and create/cache
`image::Handle::from_rgba` handles in UI state by clip/asset revision. Do not use
encoded-byte handles that defer decode to first paint, or recreate handles in
`view`. Load full previews on demand, preserving originals independently of
renderer texture limits. Use plain text or simplified spans for HTML preview; do not execute
scripts, fetch remote images, or introduce a browser runtime. Preserve HTML/RTF
for paste even when the preview is simplified.

Gate number shortcuts and type-to-search around IME composition; test Unicode,
RTL text, and non-US keyboard layouts. Add theme tokens, scalable fonts, and
translation catalogs without embedding English action names in domain types.
Keyboard operation is required from the MVP; measure iced's actual assistive
technology support and document accessibility gaps before beta.

## 7. Implementation milestones and exit criteria

Proceed in dependency order. These are deliverables and gates, not calendar
commitments; estimate dates after M0 establishes integration costs.

| Milestone | Work | Exit criterion |
|---|---|---|
| **M0 — feasibility** | Native-window, renderer/search, selection-transfer, import-schema, and layer-shell spikes described above | Xfwm/i3 and each required pass criterion in section 2 satisfied, or an explicit scoped decision recorded; version pins, source revision, limits, and measurements checked in |
| **M1 — core and storage** | Workspace; typed actions and capabilities; capture policy; migrations; raw representations and payload leases; dedup/retention; paged/indexed queries; format converters; import fixture reader | Headless round-trips preserve bytes and metadata; migration backups and crash recovery work; opaque imported/configured formats survive; no toolkit dependency in core |
| **M2 — X11 service** | Capture/serve; INCR and manager persistence; exclusions; XGrabKey; tray; single-instance CLI; focus/paste state machine | External clients exchange large/multi-format selections; source-exit persistence and manager coexistence behave as specified; hotkey conflicts and failed focus are visible; no self-capture loop |
| **M3 — usable X11 MVP** | iced popup/settings; virtualization; literal search; thumbnails; text/image paste and copy-only fallback; pause and retention settings | Complete copy → history → search → paste loop in the real-app matrix; keyboard/IME checks and MVP performance targets pass; popup hide never stops serving selections |
| **M4 — X11 beta** | Full fixture-backed Ditto import; groups, sticky/starred state and four order keys; retained shortcut metadata; native backup/export/restore; expiry/statistics; XDG integration | Import counts, group relationships, ordering, and format results verified; recovery/retry and private-storage/clear-history checks pass; install/uninstall tested on declared distributions |
| **M5 — action parity** | Remaining local Ditto actions and desktop integrations listed below | Each action has an explicit mapping or documented limitation; transforms and multi-select actions preserve expected representations; unavailable actions reflect capabilities |
| **M6a — scripting** | Rhai on-copy/before-paste hooks and action API | Bounded scripts cannot block selection/UI workers; errors leave the clip intact; migration examples explain ChaiScript differences |
| **M6b — Friends** | Optional authenticated native sync; separately audited legacy Ditto adapter | Native peers interoperate with explicit pairing; legacy support is labeled compatible only after Windows Ditto fixture and live-peer tests |
| **M7 — Wayland** | Data-control backend, portals, layer-shell runner, fallback regular window | Clipboard, activation, shortcuts, and paste capabilities validated on actual compositors; absent/denied protocols produce useful reduced modes |

M1 follows M0. M2 follows M1; M3 joins the native service and UI. M4 follows M3.
M5, M6a, M6b, and M7 build on the X11 beta and can be scheduled independently
once the core interfaces are stable. Do not delay a qualified X11 release for
optional Friends, scripting, or Wayland work.

### M5 coverage checklist

- Search: wildcard and regex, group filters, quick-paste text, and custom bindings
  corresponding to Ditto's `ActionEnums`; direct-paste positions and per-clip
  hotkeys subject to platform availability.
- Organization: multi-select, move-to-group, group navigation, and explicit rules
  for starred/sticky clips during cleanup and edits. A read-only clip flag would
  be a new feature, not the meaning of DittoUtil's file read-only actions.
- Editing: create/edit plain-text clips, choose a paste format, paste any format
  as text where conversion exists, and a configurable external diff tool.
- Text transforms: upper/lower/title/sentence/camel/invert case, slugify, trim,
  add/remove line feeds, current time, GUID, ASCII-only, typoglycemia, and
  Windows-to-POSIX path conversion with explicit path rules.
- Combined actions: multi-clip paste with configured separators and ordering,
  horizontal/vertical image joins, image-to-HTML, and capture/restore copy buffers.
- Desktop features: QR codes, web search/translate via URLs, `mailto:` or
  `xdg-email`, XDND drag-out with temporary-file lifetime management, opacity and
  themes, translations, and optional snap/always-on-top behavior where supported.

The five DittoUtil exports are `PasteAnyAsText`, `ConvertPathToHtmlImageTag`,
`ClearReadOnlyFlag`, `SetReadOnlyFlag`, and `RemoveLineFeeds` (the last is present
but not advertised in the pinned add-in's menu). Port the three transformation
behaviors into native actions. Choosing a paste format is also supported, but
is not a separate one of these five exports. `SetReadOnlyFlag` and
`ClearReadOnlyFlag` modify **referenced files' Windows attributes**, not clip
state. Defer those two actions from this roadmap: a future Linux file-permission
action needs an explicit local-path and mode-restoration design rather than
silently treating `chmod` as an equivalent flag toggle.

### M6 constraints

Rhai replaces ChaiScript rather than emulating it. Expose clip transformations,
capability-aware source metadata, and explicit action hooks. Limit execution,
memory, and recursion; keep filesystem, shell, and networking out of the default
script API. Provide examples for common on-copy and before-paste scripts.

For Friends, inspect `Client.cpp` and the server code for framing, format names,
compression, authentication/encryption, versioning, and payload limits. Port
23443 and format-name translation alone do not prove compatibility. Keep the
native protocol versioned and authenticated using established transport
libraries such as Tokio + rustls. Keep networking disabled until configured,
require explicit peer pairing, and prevent sync/capture echo loops. Isolate any
legacy adapter and disclose its actual transport properties. Do not invent a
new crypto protocol to imitate undocumented behavior. The pinned source uses a
password-based Rijndael implementation for legacy traffic; authentication and
integrity guarantees need auditing. Keep legacy mode separate, explicitly opt-in
and restricted to trusted-LAN use, with its weaker guarantees stated before
configuration. It must not inherit the native TLS protocol's security claims.

Existing DLL add-ins remain unsupported. Defer a stable native plugin ABI;
built-in transformations and the Rhai API cover the initial extension needs
without loading arbitrary `.so` files.

## 8. Wayland delivery model

Use a separate platform adapter and, if needed, a separate iced shell runner,
while sharing UI state/messages/views and the entire core/store. Wrap UI messages
in runner-specific enums for layer-shell commands instead of adding shell-only
variants to domain messages. Recheck the iced/iced_layershell version pair when
M7 begins. Detect actual protocol globals and portal interfaces at runtime;
compositor names are hints, not capability guarantees.

- **Clipboard:** implement ext-data-control and/or wlr-data-control using
  `wayland-client` and the matching protocol bindings. Evaluate
  `wl-clipboard-rs` as an implementation option, not a second clipboard owner.
  Check background capture, ownership persistence, and PRIMARY independently.
- **Shortcuts:** use `ashpd` GlobalShortcuts where supported, with manual
  compositor bindings to `feritto --toggle` as the fallback. Dynamic per-clip
  shortcuts need their own support and may remain unavailable.
- **Presentation:** use `iced_layershell` only where the compositor advertises
  the needed protocol, with fixed anchors or configured output. Otherwise use a
  regular window. Do not promise cursor/caret placement or remembered absolute
  coordinates. Validate activation tokens, focus return, `on_demand`/`exclusive`
  keyboard interactivity, and text-input-v3/IME on layer surfaces separately.
- **Paste:** default to setting the clipboard and asking the user to paste.
  Add RemoteDesktop/libei only after an explicit permission-backed path is
  tested. Virtual-keyboard or uinput support is an optional specialized backend,
  never an assumed requirement or silent privilege workaround.
- **App identity:** mark identity-dependent exclusions and paste rules unavailable
  when source/target identity cannot be established. Continue respecting reliable
  secret MIME hints and make that exclusion limitation visible in settings.

First qualify one KDE environment and one wlroots environment, then test GNOME
as a separate reduced-capability target. If its installed compositor lacks
background data-control, document that monitoring is unavailable. A GNOME Shell
extension would be another explicitly scoped deliverable, not an implied part
of this port. Xwayland input injection does not provide native Wayland support.
Unrestricted host packages come before Flatpak because sandbox access to the
required protocols cannot be assumed.

## 9. Validation and release gates

Automate domain and storage behavior headlessly. Exercise selection protocols in
a nested X server for repeatability, and use real desktops for focus/placement
checks that a headless server cannot validate.

Required fixtures cover Unicode and binary data, alias-heavy offers, capture
limits, large/slow INCR, owner changes mid-capture, reclaim/new-copy races,
intentional clears, ignored generations, unsupported formats, malformed/oversized
images, allocation padding, empty clips, group trees, all four order keys,
star/protection mapping, retained shortcut metadata, timestamp conversion,
orphaned `Data` rows, duplicate imports, missing older-schema columns, and offline
hot-journal recovery. Keep only synthetic or redistributable data in fixtures.

The X11 real-app matrix includes a GTK editor, a Qt app, Firefox/Chromium,
LibreOffice, a terminal, and a password manager using secret hints. Test both
Xfwm and i3 (plus KWin-X11 where maintained), multiple monitors/scales, non-Latin
shortcut layouts, IME composition, tray present/absent, another clipboard manager
present, suspend/reconnect, and a
source exiting immediately after copy. Terminal paste uses its configured
sequence and selection rather than assuming Ctrl+V/CLIPBOARD. Test both
cooperative `SAVE_TARGETS` shutdown and a source that exits without it.

These are **provisional acceptance targets**, not measured claims. Record the
reference hardware, distro, compositor, renderer, corpus, and measurement method
in M0, then keep results with the release evidence:

- Warm hotkey → focused, painted popup: **p95 ≤ 100 ms**.
- Literal search over 100,000 representative clips → visible results:
  **p95 ≤ 150 ms**, including debounce, with stale work discarded. Report
  one-/two-character queries separately from indexed longer substrings, using
  the declared search-text cap.
- Visible popup scrolling: **p95 frame time ≤ 16.7 ms** on the reference display.
- Hidden resident process: **< 0.5% of one CPU core** averaged over five idle
  minutes and **≤ 100 MiB RSS** on the declared mixed text/thumbnail corpus,
  excluding transient large transfers. Diagnose native/renderer caches before
  adjusting the target.
- Repeated show/hide and long capture runs: bounded queues/caches and no
  sustained resource growth; no missing captures within declared transfer limits.

Measure small text, large images, cold preview loads, and search/import running
concurrently. Keep slow work away from input and selection serving even if a
performance target needs revision.

CI should run formatting, clippy, meaningful core/storage/converter tests, X11
protocol integration tests, and builds for the declared software and GPU
renderer configurations. Qualify native x86_64 Linux packages first; declare
supported distro/library versions and aarch64 support only after testing them.
An X11 beta release also needs a desktop entry, icons, optional autostart,
backup/restore instructions, and a capability/limitations page. Accessibility,
clipboard persistence limits, and import failures must be stated concretely.
Verify file/sidecar/backup permissions, private staging cleanup, clear-history
and clear-clipboard behavior, and that diagnostics contain no clip payloads.

## 10. Explicit exclusions and first implementation batch

Drop Windows UAC/admin paste, Store/U3 modes, Windows ICU/DPI setup, and injected
focus DLL behavior. Do not implement Windows metafile rendering, a full RTF
editor, ChaiScript binary compatibility, or existing add-in DLL loading.
Preserving an unsupported imported payload does not imply rendering it.
The two DittoUtil file-attribute actions and actual Access/Jet/ACE database import
are explicitly deferred/unsupported as detailed above.

The next implementation batch should contain only M0 and the M1 skeleton:

1. Pin the upstream revision and working Rust/iced dependency set; add workspace
   manifests, tooling configuration, and the license/attribution decision.
2. Demonstrate the persistent iced popup and native X11 handle/hints, focus,
   renderer selection, and settings-window lifecycle.
3. Demonstrate configured clipboard capture/ownership with multi-format INCR,
   source-exit recovery, explicit clears, and ownership-race tests.
4. Benchmark the fixed-height virtual list, thumbnails, and Unicode/IME path.
5. Save a Ditto schema/format fixture inventory and the Wayland runner result.
6. Define core service contracts and the first versioned native schema from those
   findings. Break M2–M4 into implementation issues only after the gates pass.

The result of that batch is an evidence-backed implementation foundation, not a
claim of feature parity. It resolves the integration choices most likely to
force a redesign while keeping the first usable X11 release small.

## References

- [Ditto port analysis](ditto-port-analysis.md)
- [Rust GUI review](rust-gui-options.md)
- [iced 0.14.0 package, features, and minimum Rust version](https://crates.io/crates/iced/0.14.0)
- [iced 0.14.0 window operations](https://github.com/iced-rs/iced/blob/0.14.0/runtime/src/window.rs)
- [iced 0.14.0 Linux window settings](https://github.com/iced-rs/iced/blob/0.14.0/core/src/window/settings/linux.rs)
- [iced_layershell 0.19.1 dependencies](https://crates.io/crates/iced_layershell/0.19.1/dependencies)
- [Ditto reference revision](https://github.com/sabrogden/Ditto/tree/a80fd3536bc407cac8cce43b5682022282ace983)
- [Ditto license at that revision](https://github.com/sabrogden/Ditto/blob/a80fd3536bc407cac8cce43b5682022282ace983/LICENSE)
- [Ditto schema and migrations](https://github.com/sabrogden/Ditto/blob/a80fd3536bc407cac8cce43b5682022282ace983/src/DatabaseUtilities.cpp)
- [Ditto capture-format defaults](https://github.com/sabrogden/Ditto/blob/a80fd3536bc407cac8cce43b5682022282ace983/src/OptionsTypes.cpp)
- [DittoUtil exports](https://github.com/sabrogden/Ditto/blob/a80fd3536bc407cac8cce43b5682022282ace983/Addins/DittoUtil/Exports.cpp)
- [EWMH active-window requests](https://specifications.freedesktop.org/wm-spec/latest/ar01s03.html)
- [SQLite online backup](https://www.sqlite.org/backup.html)
- [SQLite FTS5 trigram tokenizer](https://www.sqlite.org/fts5.html#the_trigram_tokenizer)
