# What b33p can take from sigwin

[Othereum/sigwin](https://github.com/Othereum/sigwin) (inspected at `88ea8a5`, 2026-11-07) is a
Windows Signal client on the same stack: presage, libsignal-service, Slint. It is AGPL-3.0-only like
b33p, so code may be ported, not only ideas. A ported commit says so in its body
(`Ported from Othereum/sigwin <commit>`), and the About screen's notices name the project.

b33p is far ahead on the protocol: groups, stories, sender keys, history transfer, storage service.
sigwin is ahead on the **conversation's feel**, **memory** and **Windows integration**. Its fixes
live as `// sigwin patch` hunks under `vendor/` (listed in its `vendor/PATCHES.md`); where we take
one, it goes to our Slint fork's `langis/integration` and upstream as a draft PR, not into a
vendor directory.

## Already in b33p (nothing to do)

| sigwin | b33p |
|---|---|
| Global `SESSION_LOCK` in libsignal-service (`a2dc65d`) | lss #488 / presage #473, in the integration branches |
| Placeholder previews to stop presage's link fetch (`42327d6`) | presage #478 `set_automatic_link_previews` |
| 16 group sends at once (vendored) | Sender keys (fork) |
| Read syncs with a binary sender (`e9ebf2e`) | `receive.rs` reads `*_aci_binary` |
| Delivery receipts batched by the app | Sent by b33p-core |
| Jump-to-latest button with a count | `app.slint`: "Newest messages, N new" |
| Drafts per chat, Esc cancels a reply, typing indicator, autostart, single instance, tray dot | Present |
| Quote's author above the quote | Present |

One detail to keep: **Signal iOS drops a whole message whose `Preview` has no `url` field**
(sigwin `.claude/rules/presage.md`). b33p only sends previews it built from a URL, but a unit test
should hold every outgoing `Preview` to a set `url`.

## Phase 0: checks and upstream fixes (small, do first)

Each is a test before it is a change: if b33p doesn't have the bug, record that and move on.

**Results (2026-11-07):**

| Item | Result |
|---|---|
| 0.1 ListView end of model | **Not reproduced.** Two Slint case tests (rows removed at the end, the view grown, a jump far past the end, a new model; uniform rows and tall-then-short rows) pass on Slint master `6a4ef80` *and* on v1.18.1. The `content_y_value += …` in that branch is dead (nothing in the loop reads it), but the layout still ends with the last row at the bottom; sigwin's patch seems to make it converge in one pass, which its first-frame `seek-end` needs. b33p's own "scroll to end" works. No upstream PR without a failing test |
| 0.2 Caps Lock in Ctrl shortcuts | **Fixed upstream**, in our pin: `InternalKeyEvent::shortcut` compares in lower case (`input.rs:1175`), and b33p's own shortcuts use `eq_ignore_ascii_case` or test both cases. Left: winit reports a Ctrl+letter pressed during an IME composition as `Named(Process)` (Windows, Korean IME), which Slint drops; sigwin rewrites it in a winit event filter (`e3055df`). Windows only; goes with phase 4 |
| 0.3 CJK fallback on winit | Open: needs a Windows or macOS machine with a CJK locale |
| 0.4 Mentions while composing | **Done**, #135: candidates follow the composition. Still to check with a real input method: a name picked mid-syllable may get the syllable committed after it |
| 0.5 Preview `url` | **Holds by construction**: the only place building a `Preview` sets `url` (`send.rs:608`); forwards and edits keep a preview only if its `url` is set and in the text (`commands.rs:922`, `:989`). No test added (it would need a manager) |
| 0.6 Attachments at rest | **Already so**: downloads stay in memory; only Save (to Downloads, asked for) and Open (a `0600` copy in a private scratch directory, the runtime directory on Linux, cleaned) write files |

1. **Slint ListView at the end of its model.** When the rows run out with room to spare,
   `update_visible_instances` raises `content_y` but not `new_offset_y`, so the next pass lays out the
   same rows and the view never moves up (sigwin `vendor/i-slint-core/model/repeater.rs:404`). It is
   still unfixed on Slint master, at our pin `f0c244a`, `internal/core/model/repeater.rs:510`. First
   find out whether b33p hits it: switch to a short chat after a long one, and shrink the window at
   the bottom of a chat. Either way, add a Slint test, merge the fix into `langis/integration`, and
   open an upstream draft PR crediting sigwin.
2. **Ctrl shortcuts with Caps Lock on, or during IME composition** (`e3055df`). Slint's key text is
   `"C"` with Caps Lock on, so a check against `"c"` misses. Go through b33p's `FocusScope` handlers
   and add a `ui_tests.rs` case per shortcut with Caps Lock on.
3. **CJK ideographs on the winit backends** (sigwin `shaping.rs`, `de6f512`): parley's script
   fallback picks one font per script, and on Korean Windows that is Malgun Gothic, which lacks
   Japanese and Chinese-only ideographs (駅 鉄 国 render blank). b33p on Windows and macOS uses
   winit and femtovg, so parley too. Check in the mock on Windows with a Korean or Japanese locale;
   if it shows, offer the fix upstream (a fallback list per platform), not as a hunk.
4. **Mentions while composing with an IME** (`0359739`, `f36d7b4`, Signal-Desktop#8081). b33p's
   candidate list reads `Composer.draft`, which doesn't include `preedit-text`. Check with a
   Hangul IME (fcitx5 in the dev container) that `@` plus a composing syllable filters, and that
   Enter or Tab mid-composition picks the name instead of sending.
5. **Preview `url` test** (above).
6. **Attachments and thumbnails at rest.** sigwin encrypts every cache file and keeps plain copies
   only in a folder emptied on start and exit (`70fcc38`, `7430a18`). Check what b33p writes in the
   clear under its data directory (the database is SQLCipher; the attachment files I didn't check),
   and where the copy for "open in the desktop's viewer" goes and when it's removed.

## Phase 1: the conversation's feel (each change on its own)

| # | Change | sigwin | b33p today | How |
|---|---|---|---|---|
| 1.1 | **Click a quote to jump to the quoted message**, loading older history if needed | `992dc98`, `a7e3e42` | Quote is not clickable (`bubble.slint:324`) | A `TouchArea` on the quote and a core action that reuses search's `pending_jump` / `continue_jump` (`state/panels.rs:446`). A harness action, a ui test, and a testbox check with a Desktop quote of an old message |
| 1.2 | **Recent times as elapsed** ("now", "3 min") within the last hour, in bubbles and the chat list, refreshed every 30 s | `d420ba4` | Clock time only | Formatting in b33p-core with an injected clock (tests); one timer in the slint crate. Respect reduced motion: no animation, only text |
| 1.3 | **Who read it**: in groups, readers' avatars under the newest message each has read (up to 3, +N), straddling the bubble's edge | `9b2e99b`, `0be7797`, `ee78b51` | One state per message (`b33p_delivery`, `local.rs:933`); only the ticks show | A table of receipts per recipient (sigwin `ls_receipts(target_ts, who, level)`, filled live and backfilled once; receipts arrive under the sender's 1:1 chat, never the group), then "newest read per member"; the row gets up to 3 avatars. Screen-reader label "Read by A, B and 2 others". 1:1 shows nothing more than ✓✓ |
| 1.4 | **Long messages collapse** with "Read more", expanding in place | `d1a1b80` | Shown in full (LongText attachments are fetched) | Row property `collapsed`, a line cap in `bubble.slint`; expanding keeps the row's top in place (as history loading does) |
| 1.5 | **Time at the last line's right edge**, dropped on a row followed by one from the same run and minute | `93dc607`, `9b2e99b` | Time on every row, below or beside | Visual change: compare with `shoot.py` in light and dark, Qt and Fusion, before deciding |
| 1.6 | **Message info in the context menu** (sent, delivered, read per recipient) | `6a538a2` | Details exist for chats, not per message | A popup from the row's menu; data is already in core |
| 1.7 | **A pin bar's text follows edits** | `e63dcf5` | Only with 2.1 | With 2.1 |

## Phase 2: features b33p lacks

1. **Pinned messages** (`384bfd3`, `e6b9701`, `3d46fb0`). Pin and unpin from the message menu
   (24 h, 7 d, 30 d, until unpinned), a pin bar above the timeline that jumps to the message, marks on
   pinned rows, and timeline notices.
   - Wire: the `PinMessage` / `UnpinMessage` data messages. presage stores them but keeps no pin
     state, so b33p-core keeps it: a table filled live and backfilled once (sigwin's `ls_pins`
     design, with expiry, deleted targets and a cap).
   - Test box: first check that Desktop at our revision (`f3040f3e`) and Android send and show pins.
     If so, add driver calls and a `desktop` check both ways, and have the tour pin and unpin.
   - The jump reuses 1.1.
2. **An image viewer of our own** (`a2af344` onward, about 25 commits). Today a click opens the
   desktop's viewer on a plain copy.
   - What to take: its own frameless window, wheel zoom that catches at 100%, starting at up to 80%
     of the screen, smoothing only the visible part (bare pixels while dragging, smooth 100 ms after),
     a thumbnail strip for albums, decoding straight into the UI's buffer, and a pixel cap against
     decompression bombs.
   - Resampling with `fast_image_resize`: Lanczos3 down, Catmull-Rom up. Their measurements are in
     their `docs/decisions.md`. Mind the crash they found (Cykooz/fast_image_resize#63) and their
     test that holds crops away from it.
   - Platforms: their tricks against snapping and minimizing are Win32; on Wayland a window can't
     place itself. Decide per platform: a separate window on Windows and X11, an overlay in the main
     window on Wayland?
   - It also removes a privacy cost: no plain copy for another app (keep "Open with…" as a choice).
3. **Albums as a grid** (`a6a3a29`, `03c6120`). Several pictures in one message as a grid; a click
   opens the viewer on that picture.
4. **Animated stickers** (`501a825`, `98f6a16`, `58f3c11`). b33p shows the first frame
   (`attachments.rs:723`). Play APNG and animated WebP at their exact frame times, only for stickers
   on screen, with an LRU frame budget. Reduced motion shows the first frame, as now.
5. **Per-chat notification settings beyond mute** (`fa65d34`): notify on mentions and replies only,
   whether the preview shows text, sound. These stay local, as in sigwin: the storage service has no
   field for them.

## Phase 3: memory and rendering (decide by measurement)

sigwin holds idle private memory under 30 MB on Windows. b33p peaked at 66 MiB in the test box
(Linux, Qt), and nobody has measured it on Windows. Steps:

1. **Measure b33p's idle private memory** on each platform (one chat open, after sync), and the
   Windows build on femtovg against `renderer-software`. sigwin measured femtovg at 212 MB, Skia at
   388 MB, and software at 8.4 MB (`docs/decisions.md`: the GPU driver loads into the process). Add a
   `mem:` log line like theirs (Rust heap and private memory), and an optimized nightly test box run
   with `--b33p-memory`.
2. **If the Windows build moves to the software renderer**, take sigwin's renderer patches into our
   Slint fork, each as an upstream draft PR crediting sigwin:
   - Color emoji from COLR v0 layers, drawn as premultiplied images (Segoe UI Emoji still has them).
   - TrueType hinting and an enhanced-contrast curve (k = 0.5) for small text. Slint declined a
     similar underline change (slint#14642), so ask first.
   - Synthetic italic skew.
   - Clipping `fill_rectangle` before its i16 cast for very tall text (slint#14747 fixed the cursor
     case only).
3. **What the open chat holds** (whatever the renderer):
   - A cap on rows kept in the timeline. b33p reads history a page at a time; find out whether it
     ever drops pages (I found no trimming in `state/`, but didn't confirm).
   - Images only for the open chat.
   - Dropping them when the window has been hidden for a while (sigwin `gui/window.rs`).
   - Thumbnails decoded at the size shown.
4. **The emoji findings from the 6 Nov sweep** (black ❤, blurry ZWJ sequences, on Qt and Linux) are a
   different path from sigwin's renderer. Their COLR approach doesn't apply to Qt; note it with
   those findings, don't merge them.

## Phase 4: Windows integration (ask before starting; it widens what b33p maintains)

- An unread count overlay on the taskbar button (`ITaskbarList3`, redrawn per DPI, re-applied when
  Explorer restarts) and a count on the tray icon instead of a dot (`61394d1`, `57a59bd`).
- Self-update from signed GitHub releases (minisign, `380bb67`) for the Windows zip and the
  AppImage. Packages from distributions update themselves. This is new outgoing traffic, so it
  needs a setting, off by default.
- File dialogs in a child process (heavy shell DLLs stay out of the process), only if phase 3's
  measurement shows the cost.

## How each change is checked

- A unit test in b33p-core, a `ui_tests.rs` case, and a harness action wherever a person could do it.
- `shoot.py` screenshots in light and dark, Qt and Fusion, for every visible change, reviewed by
  eye. Add the viewer and the pin bar to its screens.
- A test box check where the wire is involved (1.1, 2.1, 2.4), and the default phases with the tour
  before any merge.
- One branch and PR per numbered item. Order: phase 0, then 1.1 and 2.1 together (the jump is
  shared), then the rest of phase 1, then 2.2 and 2.3, then phase 3's measurement. Phase 4 waits
  for a yes.
