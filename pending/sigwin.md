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

**Merged (2026-11-07):** 0.4 mentions while composing (#135), 1.1 quote jump (#136), 2.1 pinned
messages (#137), 1.4 Read more (#138), and the core of 1.3 readers and 1.6 message info (#139). The
rest is planned under **What's next** below.

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

## Phase 4: Windows integration

See **What's next**, step D.

## How each change is checked

- A unit test in b33p-core, a `ui_tests.rs` case, and a harness action wherever a person could do it.
- `shoot.py` screenshots in light and dark, Qt and Fusion, for every visible change, reviewed by
  eye. Add the viewer and the pin bar to its screens.
- A test box check where the wire is involved (1.1, 2.1, 2.4), and the default phases with the tour
  before any merge.
- One branch and PR per numbered item.

## What's next (planned 2026-11-07, after #134–#139)

Five steps, A to E. A and B need nothing from anyone. C waits for the gpui branch. D and E need a
yes on the decisions listed at the end.

### A. Windows in CI (first: everything in D and E builds on it)

Today Windows is built only by `release.yml`. A broken `cfg(windows)` path (toasts, autostart,
single instance, shutdown, capture exclusion, keyring) shows up only on release day, and no test
has ever run on Windows.

1. **A `windows` job in `ci.yml`** on `windows-2025`:
   - `cargo build -p b33p-slint`, plus `cargo test -p b33p-core -p b33p-slint`. The UI tests use
     Slint's testing backend, which needs no display.
   - Keep the BoringSSL and protoc steps from `release.yml`, and cache `target/`.
   - Run on pushes to `main`, and on PRs that touch `cfg(windows)` code, `b33p-notify` or
     `Cargo.lock` (`paths:`). x86_64 only; arm64 stays in the release.
2. **A smoke run of the release build with the mock backend** (`Backend::mock_requested()`):
   - Start it, wait for the window, take a harness snapshot and a `screenshot`, then quit.
     Upload the screenshot as a CI artifact.
   - This is the first proof that the Windows window draws at all, with the fonts Windows has.
     It also covers 0.3 (CJK on femtovg): the mock gets a chat with 駅 鉄 国 and Hangul, and the
     screenshot shows them, or blanks.
3. **Memory on that runner** (phase 3.1, which no longer needs a Windows machine):
   - Add a `mem:` log line: private bytes from `GetProcessMemoryInfo` on Windows and
     `/proc/self/status` on Linux.
   - Optional: a counting allocator, as sigwin's `alloc.rs`, only behind a feature.
   - Run the mock idle with one chat open and record the figure on femtovg. Then do one run with
     `renderer-software` (a feature switch, not a release change) to see whether sigwin's
     212 MB → 8 MB finding holds for b33p.
   - Phase 3.2 (the software renderer patches) is decided by that number.

### B. Features that need no one else

1. **2.4 Animated stickers**:
   - In `attachments.rs`, decode APNG (`image` with `png`'s APNG) and animated WebP
     (`image-webp`) into frames with their delays. Cap the frames and pixels, as for any image,
     against decompression bombs.
   - In the slint crate, one 16 ms-granular `Timer` drives every sticker on screen. Each sticker's
     frame is chosen from `now - started` and its delays, so a slow frame never shifts the timing.
     Stickers off screen hold no frames: an LRU of decoded frames, about 32 MB.
   - Reduced motion and an unfocused window both show the first frame.
   - Tests: unit tests on two fixture files (frame times, the loop, the cap), and a ui test that
     the frame changes with the clock.
   - Check in the testbox whether Desktop at `f3040f3e` can send an animated sticker (it needs a
     pack installed on the device). If not, the check is the mock's sticker plus the screenshot.
2. **2.5 Notification settings per chat**:
   - Settings: mentions and replies only, whether the text shows, and sound on or off.
   - Kept locally, in a table next to `ChatSettings`. The storage service has no field for these,
     so they don't sync.
   - The decision lives in b33p-core, where the notification is built: an `Event` carries the
     setting, and the toast is either left out or shown without the text.
   - Tests in `state/tests.rs` for each rule. The UI goes in the chat's details panel, next to
     mute.
3. **2.2 An image viewer of our own**, as an overlay in the main window first:
   - The overlay works the same on Wayland, X11, Windows and macOS, and needs no window placement.
     A separate window on Windows and X11 can come later, if anyone misses it.
   - Decode into a `SharedPixelBuffer` with a pixel cap, and scale with `fast_image_resize`
     (Lanczos3 down, Catmull-Rom up), keeping sigwin's crop test (Cykooz/fast_image_resize#63).
   - Zoom with the wheel, catching at 100%; pan by dragging; ←/→ for the chat's other pictures;
     Esc closes. Bare pixels while dragging, smoothed 100 ms after.
   - The viewer has **Save** and **Open with…** (today's plain copy, now only when asked for).
     Clicking a picture no longer writes a copy for another app.
   - Harness actions `OpenViewer`, `ViewerNext` and `CloseViewer`, ui tests, and `shoot.py`
     screens.
4. **Small follow-ups from phase 0:**
   - 0.4 with a real input method: fcitx5 with Hangul in the dev container, and pick a name in the
     middle of a syllable.

### C. Waiting for the gpui branch's Slint pass to reach `main`

That branch reworks `items.rs` and `types.slint`, and it adds `sent-minute`. Whoever merges it
tells this plan; then, each on its own branch:

- **1.2 Elapsed times**: formatting in b33p-core with the injected clock, and one 30 s timer.
- **1.3 Readers in the UI**: up to three avatars and "+N" under the row from `readers_of`, with
  the label "Read by A, B and 2 others".
- **1.6 Message info**: a popup from the row's menu, built on `open_message_info`. It has a
  harness snapshot already.
- **1.5 Time at the last line**: decided by `shoot.py` screenshots, not ported blindly.
- **2.3 Albums as a grid**: a click opens 2.2's viewer on that picture, so after B.3.

### D. Windows integration (wants a yes per item)

What b33p already has on Windows: toasts with an app id (`b33p-notify`), autostart, a single
instance over a named pipe, clean shutdown on log-off, exclusion from screen capture, and the
database key in Credential Manager (sigwin keeps it with DPAPI; ours is equivalent, so nothing to
take). What it lacks, from sigwin, in order of value:

1. **A Windows test machine in the testbox** (needs a yes: it downloads a Windows 11 evaluation
   image, about 6 GB, which expires after 90 days and is then rebuilt):
   - Run `dockur/windows` in a container on KVM (`/dev/kvm` is here), in the testbox pod, with
     b33p linked to the testbox server like the Linux instance. The harness talks to it over the
     pod network.
   - This is where the rest of D gets checked by hand and by the driver. It is also what CI
     can't do: toast clicks, the taskbar, IME.
2. **Installing per user**, without admin rights or an installer (sigwin `install.rs`):
   - Run from the zip, b33p copies itself to `%LOCALAPPDATA%\b33p\b33p.exe`, adds a Start menu
     shortcut and an entry in "Installed apps" (HKCU), then starts the installed copy.
   - Uninstalling from "Installed apps" removes it all, and the account data only if asked.
   - It never overwrites a newer installed version, whose database may be migrated already.
   - One fixed path keeps autostart and the updater pointing at the same file. Today the `Run`
     key (`autostart.rs`) holds wherever the zip was unpacked, so moving or re-unpacking the
     folder silently breaks start-with-Windows.
3. **The unread count on the taskbar button and on the tray icon** (`61394d1`, `7561454`,
   `57a59bd`):
   - An `ITaskbarList3::SetOverlayIcon` badge, up to "99+", drawn for the window's DPI.
   - It is re-applied on `TaskbarButtonCreated`, which needs a window procedure subclass like
     sigwin's `winhook.rs` (`SetWindowSubclass` on the main window; `shutdown.rs` runs its own
     hidden window, which doesn't get taskbar messages).
   - The tray count can serve every platform: our tray shows a dot today.
   - The count's words go to the tooltip, for screen readers.
4. **Ctrl shortcuts during an IME composition** (from 0.2):
   - winit reports Ctrl+letter during a Korean IME composition as `Named(Process)`, and Slint
     drops it.
   - Find out first whether winit or Slint should map it, and offer the fix there as a draft PR.
     Use sigwin's event filter (`e3055df`) only as a stopgap in our Slint fork.
   - Checked on the test machine from D.1.
5. **Self-update from signed releases** (sigwin `update.rs`, `380bb67`):
   - `release.yml` signs each archive with minisign (the secret key in an Actions secret).
   - The app checks GitHub's latest release after the first sync, and only if a setting allows
     it, off by default: it is new outgoing traffic.
   - The download is streamed to disk and checked against the signature, whose trusted comment
     names that exact version, so an old signed build can't be replayed. The app then swaps in
     the installed copy from D.2.
   - Windows and the AppImage only: distribution packages update themselves.
6. **Code signing**, so SmartScreen stops warning:
   - Not code, but a decision. SignPath's free plan for open source signs from CI, with
     conditions on the project. A paid certificate is the other way.
   - Without either, D.2 still helps: SmartScreen asks once, for the zip.
7. **Only if A.3 measures the cost: the file dialog in a child process** (sigwin `picker.rs`).
   The shell, thumbnail and cloud-sync DLLs add 28 MB while open, and over 10 MB stay after.
8. **To check on the test machine, not port:**
   - Pasting a picture from the Windows clipboard (our `clipboard.rs`).
   - A toast click opening the chat (sigwin `4e3bbee`).
   - The window after restoring from minimized. sigwin's repaint fix is for the software
     renderer only; it matters to us only if A.3 moves us to it.

### E. Order and decisions

Order: A.1 → A.2 → A.3 and B, in parallel (B.1, B.2, then B.3) → C, when the gpui branch lands →
D in its numbered order.

Decisions needed:

| Decision | Blocks |
|---|---|
| A Windows job in CI (free: the repo is public; it adds roughly 15–25 min of wall time to those runs) | A |
| When the gpui branch's Slint pass merges, and beep-6a's two Slint fork patches | C |
| A Windows 11 evaluation VM in the testbox | D.1, and checking D.3–D.8 |
| Installing per user to `%LOCALAPPDATA%` | D.2, D.5 |
| Self-update (and a minisign key in the repo's secrets) | D.5 |
| Code signing, and with what | D.6 |

### F. Implementation (started 2026-11-07, on the owner's "implement them")

Found while planning:
- Windows code that links C (BoringSSL, aws-lc, ring) can't be checked from Linux without
  Microsoft's SDK. `cargo check --target x86_64-pc-windows-msvc` works only for `b33p-notify`.
  The Windows CI job (A.1) is therefore the check for every item in D, and the D branches
  stack on it until it merges.
- **B.1 animated stickers moves to C**: a sticker's picture is converted in `items.rs`
  (`:345`–`:413`), which the gpui branch reworks.

Taken as decided by "implement them": A, B.2–B.4, D.2, D.3, D.4 and D.5. D.5 is off by
default, and its release signing step skips when no key is set, so the code can merge before
the owner creates the key. Still the owner's call: D.1 (the Windows evaluation image's licence
and download), D.6 (a signing account), the key for D.5, and C.

| Item | Branch | Files | Proof |
|---|---|---|---|
| A.1 Windows CI job | `windows-ci` | `.github/workflows/ci.yml` | the job green on the PR |
| A.2 Smoke run with the mock, CJK in the mock | `windows-ci` | `ci.yml`, a `--smoke` exit in `main.rs` after the first frame and a screenshot, the mock's chat text | the screenshot artifact, looked at |
| A.3 `mem:` line | `windows-ci` | a `memory` module in b33p-slint (Windows `GetProcessMemoryInfo`, Linux `/proc/self/status`), logged once a minute and at the smoke run's end | the number in the job log, written into phase 3 |
| B.2 Notification settings per chat | `notify-per-chat` | `local.rs` table, `ChatSettings`, the notification decision in b33p-core, details panel | core tests per rule, ui test, harness action |
| B.3 Image viewer overlay | `image-viewer` | a new `viewer.slint` with its own global, `src/ui/viewer.rs`, the picture click in `callbacks/chat.rs`; no `items.rs` | ui tests, harness `OpenViewer`/`ViewerNext`/`CloseViewer`, a screenshot |
| B.4 fcitx5 Hangul check of #135 | done, no branch | a throwaway image on the testbox's b33p image: fcitx5 + fcitx5-hangul + the Qt 6 frontend on a private D-Bus, keys by `wtype` into the headless `cage`, harness screenshots | see below |

**B.4 result (2026-11-07):**
- With a real fcitx5 Hangul input method, `@` followed by composing 가 closes the @mention list: b33p sees the composition while its committed draft is still `@`. Before #135 the list showed every name. #135 holds.
- **New finding:** Enter in the middle of a syllable commits it *and sends* the message in one press (`@가` went out). Chromium, and so Signal-Desktop, spends that first Enter on finishing the syllable. Changing it needs to know that the commit and the Enter came from one key press: Qt's fcitx plugin commits the preedit before it delivers the Return, so `Composer.composing` is already empty when the composer sees Enter. Owner's call whether to match Desktop.
| D.2 Per-user install | `windows-install`, on `windows-ci` | `install.rs` (HKCU Uninstall key, Start menu `.lnk` via `IShellLinkW`), `main.rs` | Windows CI build, unit tests on the decisions (version compare, paths) |
| D.3 Unread counts | `unread-badge`, on `windows-ci` | `badge.rs` (`ITaskbarList3`), a window subclass, the tray icon with a count on every platform | Windows CI build, a ui or unit test of the count's words and "99+" |
| D.4 IME Ctrl shortcuts | upstream draft | winit or Slint | a draft PR, credited to sigwin |
| D.5 Self-update | `self-update`, on `windows-install` | `update.rs` (`minisign-verify`), the setting, `release.yml` signing | unit tests on signature and version checks with a test key |
