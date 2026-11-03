# Rust + iced porting plan: critical review and resolutions

Date: 2026-11-03. Independent read-only review.
Document reviewed: [the porting plan](ditto-rust-iced-porting-plan.md), before the
corrections below. The reviewer inspected iced 0.14.0 and Ditto revision
`a80fd3536bc407cac8cce43b5682022282ace983`; no application prototypes were run.

The review judged the structure sound but identified four high-priority gaps
and required explicit feasibility pass/fail decisions before starting M0.
The plan has since been revised by its author. These revisions have received
local documentation checks, not a second independent review or runtime testing.
Finding IDs below refer to the review, not the implementation milestones.

## High-priority findings

| Finding | Resolution in the plan |
|---|---|
| H1 — Clipboard persistence conflated eager capture and `SAVE_TARGETS` | Capture without claiming ownership; reclaim only a fully captured eligible generation after source destruction, with timestamp/generation race guards. Keep cooperative `SAVE_TARGETS` separate. Do not resurrect explicit clears or excluded content; detect ownership contention and enter coexistence mode |
| H2 — No policy for choosing capture targets | Add format-family allow-lists, alias/image preferences, extra user targets, per-target/generation budgets, and incomplete-capture semantics. Preserve opaque imported data without promising to fetch every advertised native target |
| H3 — DittoUtil file actions mistaken for clip protection | Correct all five export names. Port the three transformation behaviors; explicitly defer the two Windows file-attribute actions. Remove read-only clips from the parity claim |
| H4 — Missing Ditto schema/import facts | Preserve four order keys, one-parent group relationships, the star/auto-delete timestamp, shortcuts/quick-paste metadata, and padded original blobs. Ignore orphaned payload rows, handle older optional columns, classify internal formats, and recover only a private offline source copy |

## Other actionable findings

| Finding | Resolution in the plan |
|---|---|
| M1/M2 — Native focus and two-connection window ordering | Use typed raw X11 handles, iced's own visibility lifecycle, per-show EWMH hints and an x11rb round trip. Use real user timestamps, verify focus, and retain clipboard-only fallback |
| M3 — Shortcut layouts and PRIMARY | Resolve shortcut keysyms across XKB groups, refresh mappings, and specify the selection consumed by each paste profile |
| M4 — Search budget and indexing | Persist a documented 64 KiB search-text prefix, keep corpus data off the UI heap, benchmark FTS5 trigram and short-query fallbacks, and qualify Unicode semantics |
| M5 — Decode/cache behavior | Decode off-thread to RGBA and retain stable `image::Handle::from_rgba` handles in state; do not recreate handles in `view` |
| M6 — Weak feasibility gates | Add a pass criterion and failure decision for every M0 spike; block correctness-dependent work and explicitly defer an unsuccessful Wayland runner |
| M7 — X11 target viability | Target Xfwm/i3 first, record actual supported versions, treat KWin-X11 as additional where maintained, and refuse the X11 desktop service under Wayland rather than implying native support |
| M8 — Incomplete iced feature selection | Name the initial renderer/backend/executor/image/widget feature set and check transitive backend features |
| M9 — Storage/privacy and large payloads | Require private files/directories, make unencrypted defaults explicit, defer complete encryption design, define clear actions, and spill large payloads to leased content-addressed files with snapshot/crash rules |
| M10 — Caret lookup versus latency | Make caret lookup opt-in with an initial 20 ms deadline; default to pointer placement without waiting for an accessibility bridge |
| L1–L3 — List precision, legacy text, file-copy MIME | Qualify 100,000-row spacer geometry and a row-indexed extension; define Home/End focus behavior; prefer Unicode over ambiguous `CF_TEXT`; retain GNOME file-copy offers |
| L4–L7 — Legacy sync, runner wrappers, IME, licensing | Separate legacy transport guarantees from native TLS; wrap shell messages; test layer keyboard/IME protocols; settle reuse/license choices before translating code |

## Clarifications after evaluating the recommendations

- **Database snapshots:** copying a live main file and its journals independently
  is not an atomic snapshot. The initial importer therefore requires a quiescent
  source/validated backup and performs any recovery only in private staging.
  A later live importer must use SQLite's locking/backup API.
- **Database extensions:** `.mdb` is not proof of an Access database. Detect
  SQLite by content and reject actual Jet/ACE input, regardless of extension.
- **Focus source indication:** EWMH specifies application source 1 and pager
  source 2. Use 1 with valid user timestamps; do not use the review's suggested
  pager indication merely to obtain more permissive focus behavior.
- **Manager detection:** process names are only heuristics. Observable ownership
  contention and an explicit coexistence policy are required; checking the
  `CLIPBOARD_MANAGER` selection alone is insufficient.
- **Intentional clipboard clears:** a transition to no owner does not necessarily
  mean the source crashed. Reclamation must distinguish destruction from a clear
  and must not restore content cleared by a password manager or the user.
- **File attributes:** Windows read-only attributes do not have an exact POSIX
  permission toggle. Deferral is preferable to implying identical semantics.

The corrected plan is ready to guide M0. Its window, protocol, performance,
import-fixture, and Wayland gates still require implementation evidence before
any corresponding support claim or release.
