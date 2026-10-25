# Review plan: snappiness, memory, usability, architecture, look

What the review of 2026-10-24 found, and how to address all of it. Phases go in order; within a
phase, items are independent unless noted. Each item is meant as one pull request. Sizes are rough
guesses: **S** half a day or less, **M** one to two days, **L** three to five days.

Decisions are marked **(Q1)** … **(Q6)**; the answers are at the end.

## Status (2026-10-25)

Nothing is committed yet; everything below is in the working tree of the `review` branch and
passes what CI runs, plus the harness tests.

**Done:** 0.1–0.5 · 1.1–1.9 (1.4 by way of 6.6) · 2.1–2.5 · 3.2–3.8 · 4.1–4.4, 4.6 ·
5.1 (as answered in Q3), 5.2–5.6 · 5.7 prepared (branches on the fork, not pushed) · 6.1–6.9 ·
7.1–7.6.

How some of them turned out:
- **2.2:** the window's state is in seven globals (`ui/state.slint`: `Account`, `Prefs`,
  `ChatList`, `OpenChat`, `Composer`, `Groups`, `Panels`); `AppWindow` keeps only its own
  layout state (`narrow`, `sidebar-width`, `escaped`, …). The sidebar (`ui/chat-list.slint`) and
  the overlays (`ui/overlays.slint`) are components of their own; the chat pane is still inline.
- **2.3 / 2.4:** `ui/` is `mod.rs`, `render.rs`, `items.rs` and `callbacks/{app,chat,groups,
  panels}.rs`; `state/` is `mod.rs` (the types and `ChatState`'s fields) and `events`, `reading`,
  `session`, `list`, `composer`, `history`, `selection`, `panels`, `attachments`, `clock`,
  `views`, `text`, with each test module in its own file. Both were pure moves, checked line by
  line. **2.5's `Session`** wasn't made: `Ctx` already holds what is shared, and the manager is
  passed apart because some calls need it `&mut`.
- **3.1:** rather than one change record replacing `Update`, the state names what changed where
  it matters: `take_changed_rows()` (messages) and `take_changed_chats()` (chat list rows).
  `Update` stays as the coarse "what kind".
- **3.7:** events that change only their own chat's row (messages, typing, reactions, pictures,
  …) name it, and the list redraws that row and the rows that moved (14 µs instead of 400 µs with
  1,050 chats); anything else redraws the list as before.
- **3.8:** older pages go in with one `row_added` (`rows.rs`, a model that takes several rows).
- **5.2:** sends, edits, reactions, deletions, request answers and timer changes go through one
  outbox task, in order; everything else no longer waits behind an upload. One queue for all
  chats rather than one per chat: simpler, and a send rarely waits behind another chat's.
- **5.8:** `ChatState` takes its time from a `Clock`, which tests set by hand. Errors are
  typed (`b33p_core::Error`, kind unreachable, refused or storage): the banner offers **Try
  again** for failed commands worth repeating, refused sends say "Not sent" without Retry, and
  failed downloads are clicked again only when that may work.
- **6.5:** no separate React button: the reaction bar already comes after the pointer rests on a
  message for 350 ms, and the menu has "React…".
- **7.4:** previews and notices say "Photo", "Voice message", … with an icon from the window;
  notifications keep the emoji, having no icons.

**Not done:**
- **4.5** (bounded state), on purpose: reactions, edits and deletions that came for a trimmed page
  can't be restored when the page loads again, so trimming would lose them.
- **7.7** (regenerating `docs/screenshots` and the demo GIF). `docs/screenshots/shoot.py` makes the
  shots, but offscreen Qt draws italic text garbled, so the README's images should be taken on a
  real desktop.

## Ground rules

- Every PR passes what CI runs (`cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`); anything the window
  shows also `cargo test -p b33p-slint --features harness`.
- Performance PRs show before/after numbers from the Phase 0 benchmarks.
- PRs that change what the window looks like attach before/after screenshots (light, dark,
  Fusion dark) from the Phase 0 screenshot script.
- Behaviour changes get a line in CHANGELOG.md (an "Unreleased" section) and, where only a person
  can check them, a check in TESTPLAN.md.
- The large file splits (Phase 2) land while no big PR against the same file is open.

## Phase 0: baseline (S–M)

| # | What | Where |
|---|---|---|
| 0.1 | A core benchmark (`#[ignore]`d test): 1,050 chats, an open chat with 5,000 messages. Measures what `render` asks of `ChatState` per keystroke (nothing selected, everything selected), `Appended(1)`, one reaction, 100 prepended pages, 3,000 catch-up events with the chat focused. | `b33p-core/src/state.rs` tests |
| 0.2 | A `phase_render_*` in the callgrind bench that runs `ui::render` for one reaction and one keystroke with 1,000 rows. | `b33p-slint/src/ui_tests.rs:189` |
| 0.3 | Failing tests for the bugs first: add `first`, `last`, `day` to the render simulation's `Row` so the stale-run bug shows; a UI test that an emoji lands at the caret. | `state.rs:6304`, `ui_tests.rs` |
| 0.4 | The screenshot script in the repo: the README's ten shots, plus settings, details, media, calendar, the forward and wipe dialogs and stories, each in light, dark and Fusion dark. | `docs/screenshots/` |
| 0.5 | A query-plan test: `EXPLAIN QUERY PLAN` for presage's per-thread queries on a fresh store, asserting an index is used (fails until 1.1). | `b33p-core/src/signal.rs` tests |

## Phase 1: quick fixes (each S)

| # | What | Where |
|---|---|---|
| 1.1 | **Index** `thread_messages (thread_id, ts)`, `CREATE INDEX IF NOT EXISTS`, made in `open_store` once both stores are open. It can't go in `SCHEMA`: `LocalStore` is opened alone in tests, without presage's tables. Same file and key as presage's store. Older builds ignore the index, so `SCHEMA_VERSION` stays. Built once, during "Opening your messages…". Serves unread counts, latest message, history pages and the identity-note scan on every incoming message. | `signal.rs:631-673` |
| 1.2 | **Stale previous bubble**: on `Appended(n)`, also rebuild the row before the first appended one, as `Prepended` does for the row after. | `ui.rs:1312` |
| 1.3 | **Emoji at the caret**: the picker passes the composer's selection offsets to Rust, which replaces the selection and puts the caret after the emoji. | `app.slint:4584`, `ui.rs` |
| 1.4 | **"Really leave?"** goes back to "Leave group" after a few seconds, and when details close. Becomes a proper confirmation in 6.6. | `app.slint:3733` |
| 1.5 | **No spinner while offline**: a `wifi-off` icon beside "Not connected · trying again"; the spinner stays for "Catching up…" only. Needs a `connection-offline` flag next to the text. | `app.slint:2897`, `ui.rs:1172` |
| 1.6 | **Flash only highlighted rows**: start the 900 ms `flash` animation only when `item.highlighted`. | `app.slint:917-928` |
| 1.7 | **Tray host off the UI thread**: `host_present()` reads a cached flag. A thread keeps it current by watching the watcher's registered/unregistered signals. | `tray.rs:139-169` |
| 1.8 | **Bounded event batches**: at most a few hundred events or ~8 ms per turn, then yield to the event loop. | `main.rs:623` |
| 1.9 | Cleanups: unused `ProgressIndicator` and `TextEdit` imports; the misplaced comment above the wipe dialog. | `app.slint:1, 5217` |

## Phase 2: structure, no behaviour change (each M)

Mostly moves, reviewed with `git diff --color-moved`.

- **2.1 Split `app.slint`** (5,482 lines). The main file re-exports what Rust uses:

  ```
  ui/app.slint            AppWindow: screens and overlays wired together
  ui/types.slint          exported structs and enums (ConversationItem, MessageItem, …)
  ui/globals.slint        Motion, Playback, TextSelection, MuteChoices, TimerChoices
  ui/widgets/             Icon, IconButton, SendButton, Avatar, Dialog, SettingRow, …
  ui/chat/                ConversationRow + menu, Bubble + ReactionPicker, AttachmentView,
                          ActivityStrip, ActivityCalendar
  ui/screens/             Password, Failed, Link, Syncing, Settings
  ui/overlays/            Stories, GroupDraft, Join, Forward, Wipe, Details, Media, ChatSearch
  ```

- **2.2 Areas as components, state in globals.** Extract `AppWindow`'s inline blocks (sidebar,
  chat header, message pane, composer, the overlays) into components. Move their properties and
  callbacks into globals: `ChatList`, `OpenChat`, `Composer`, `Panels`, `Account`.
  - **Why:** `AppWindow` has about 237 properties and callbacks today.
  - **Watch for:** cross-references such as `focus-composer-timer`, which bubbles and the mention
    list use, become `Composer` functions.
  - **Rust side:** calls become `window.global::<OpenChat>()`; the harness's snapshot follows.
- **2.3 Split `ui.rs`** (2,142 lines) the same way:
  - `ui/mod.rs`
  - `ui/callbacks/{chat, groups, stories, media, settings}.rs`
  - `ui/render/{screen, list, messages, panels}.rs`
  - `ui/items.rs` (`conversation_item`, `message_item`, `attachment_item`)
- **2.4 Split `state.rs`**: `state/{mod, list, history, composer, selection, panels, notify,
  attachments, clock, views}.rs`. `apply`'s 600-line match dispatches into these modules, and the
  tests move with their code.
- **2.5 Split `signal.rs`**: `signal/{lifecycle, conversations, groups, stories, history, receive,
  send, trust, storage_sync, avatars, commands}.rs`. A `Session { manager, ctx }` replaces the
  repeated `(manager, ctx, names, …)` parameters.

## Phase 3: change tracking and the render pipeline

Target, checked with 0.1 and 0.2:
- A keystroke costs under 1 ms with 5,000 messages loaded, whatever is selected.
- A reaction, receipt or download rebuilds one to three rows.

| # | What | Size |
|---|---|---|
| 3.1 | **`ChatState` records what changed**. It keeps a `Changes { screen, list: Dirty<ConversationId>, messages: MessagesChange, rows: Dirty<MessageRef>, panels: Panels }`, which `Shared::flush` drains with `take_changes()`, as it does `take_commands()`. `Dirty<T>` is nothing, a few, or all. Actions stop returning `Update`; `act`/`edit`/`command`/`redraw` become one. **Migration:** (a) record changes alongside the returned `Update`, with a debug assertion that the record covers it; (b) `flush` reads the record; (c) `Update` goes. This drops `Update::reset()` from the chat-menu actions (`ui.rs:1113`, `agent.rs:559-597`) and from Alt+↑/↓ (`ui.rs:1013`). | L |
| 3.2 | **Messages by key, not row**: `MessageItem.key` (sender + timestamp). Reply, react, edit, delete, reveal, retry, select and forward pass the key, so rows shifting between a redraw and a click can't hit the wrong message. The harness's message actions change the same way (`agent.rs:262-448`, `tests/harness.rs`). | M |
| 3.3 | **Dirty rows**: `render` rebuilds only `changes.rows`, plus each one's neighbours (run and day flags), found by binary search in the sorted history. Needs `message_view(i)` (random access from i±1) in the core. `last_messages(n)` walks only the last n. | M |
| 3.4 | **Panels when they change**: each `render_*` runs only when its panel's bit is set. Panel data is computed when its event comes and kept: media items, group candidates for the filter, story rings, folder unread counts, search snippets. Typing in the composer touches only `can-send` and the mention candidates. | M |
| 3.5 | **Cheaper core calls** (several PRs). **Selection:** a `HashSet<MessageRef>` with its count, text and permissions kept up to date; `select_through` linear; `delete_selection` one `retain`. **Lookups:** a persistent `HashMap<ConversationId, usize>` refreshed after sorting; `Event::Conversations` without rebuilding an index per event; folder membership as sets; binary search in the sorted history. **Catch-up:** `mark_seen` once per batch, starting after `last_read`, without allocating. **Per row:** each message's local day computed on insert (again if the time zone changes); `now` taken once per render; no per-row `format!` or `reference()`. | M–L |
| 3.6 | **Styled-text cache**: `StyledText` by (key, edited at, revealed, full text fetched), bounded LRU. Resets and re-renders stop re-parsing Markdown. | S |
| 3.7 | **Chat list rows when they change**. The preview and list time are cached per chat, and the time labels are refreshed by a timer at minute and day boundaries. The typing timer touches only typing chats. `Stories` events don't touch the list. An `Avatar` event marks that sender's group rows dirty. That last one fixes stale sender avatars in group chats; this bug is inferred from the code, so check it first. | M |
| 3.8 | **Older pages in one go**. **Core:** `splice` or a linear merge instead of `Vec::insert` per message (`state.rs:611`). **UI:** a small custom `MessageModel` (a `Vec` plus `ModelNotify`) with `prepend(items)`, which is one `row_added(0, n)`. | M |

## Phase 4: rendering and memory

| # | What | Size |
|---|---|---|
| 4.1 | **Hide the list under overlays**: once search, details, media or the calendar has faded in, the message list below is `visible: false` (it keeps its scroll position). | S |
| 4.2 | **Calendar**: years in a `ListView`, so only visible years exist, and rebuilt only while open and on change. If still heavy, each year becomes one image drawn in Rust, with one `TouchArea` that hit-tests columns and rows. | S–M |
| 4.3 | **Media budget (Q4)**: the core drops a picture's pixels once the UI has converted them. The UI's cache becomes an LRU bounded in bytes (e.g. 200 MB). An evicted row falls back to its blurhash, with `fetch: true`, and is fetched again when it comes into view. Audio bytes go when playback ends or the chat closes. | M |
| 4.4 | **Pictures sized by kind**: stickers 256 px, link-preview images 300 px, photos 640 px. Decode JPEG at a reduced scale where the decoder supports it. | S |
| 4.5 | **Bounded state**: chats no longer open keep only their newest page (the last few opened stay whole). The `read` set is pruned. Failed sends drop their payload once given up. | M |
| 4.6 | **One HTTP client per session** for attachment downloads (`attachments.rs:115`). | S |

## Phase 5: backend throughput and start-up

| # | What | Size |
|---|---|---|
| 5.1 | **Chats before catch-up (Q3)**: switch to the chat list once conversations and unread counts are in. The sidebar's "Catching up…" line covers the rest, and `announce_caught_up` still keeps notifications to a summary. TESTPLAN §1.0 and CHANGELOG change with it. | S–M |
| 5.2 | **Long commands as tasks**: sending (upload and link preview), searching a chat, gathering media and sticker packs run as local tasks. Sends stay in order per chat (a queue each). A new search or media request aborts the previous one. | L |
| 5.3 | **CPU work off the runtime**: sticker decoding (lazily, per pack), the group picture's resize and encode, search's lowercasing and attachment writes go to `spawn_blocking`. | M |
| 5.4 | **Fewer round trips per message**: blocked state, chat settings, group membership and verifications kept in `Ctx`. A group id → chat map built in `register()`, instead of deriving `GroupSecretParams` for every group on each typing message. | M |
| 5.5 | **Lost update in `LocalStore::update`**: read-modify-write inside `BEGIN IMMEDIATE`, or upserts that set only the changed column (`local.rs:353-376`). | S |
| 5.6 | **Smaller queries**: a page's deliveries, edits, expiries and deletions only for its time range; `conversations_of` as one query; `sweep_expired` in one transaction; stories and contact syncs sent as changes, not the whole list. | M |
| 5.7 | **Upstream, presage fork (Q2)**: `messages()` bounds an index can use (`ts >= ? AND ts < ?` with sentinels, not `coalesce`); the `(thread_id, ts)` index as a presage migration, after which b33p's own goes (migrations only grow at the end, PLAN §2); a batched `save_messages` for the history import, one transaction per ~1,000 messages. | M each |
| 5.8 | **Typed backend errors** (retryable or fatal) instead of `Result<_, String>`; an injected `Clock` for the state and format tests. | M each |

## Phase 6: usability

New controls are built as reusable components (`Tooltip` use in `IconButton`, `DangerButton`,
`FloatingButton`), so Phase 7 only has to put them on the shared values.

| # | What | Size |
|---|---|---|
| 6.1 | **Open at the first unread**: opening a chat with unread messages jumps to "New messages" (the jump path, `state.rs:2033-2060`), otherwise to the end. | S |
| 6.2 | **Jump to latest**: a round `arrow-down` button over the list's bottom right while not following, counting messages that came since. A click scrolls to the end. | S–M |
| 6.3 | **Tooltips**: the pinned Slint fork has a built-in `Tooltip` element. `IconButton` gets one from its `label`; message times inside a run show on hover; elided chat titles too. | S |
| 6.4 | **Chat list by keyboard**: Enter in the search field opens the first match; ↑/↓ move through the results. | S–M |
| 6.5 | **Message actions on hover**: a "⋯" button beside Reply opens the right-click menu; a "React" button (`face-slightly-smiling-plus`) opens the reaction bar. | S |
| 6.6 | **Destructive actions, one pattern (Q5)**: a confirmation `Dialog` whose destructive button is marked as such and isn't the default. Covers unlink, start over, leave group, delete and block in the request bar, and delete for everyone. | M |
| 6.7 | **Seeking audio**: clicking or dragging the progress bar seeks (`Playback.seek` → rodio `try_seek`). | S–M |
| 6.8 | **Emoji (Q6)**: a full picker with categories, search and recently used; the reaction bar gets a "+" that opens it for reacting. | M |
| 6.9 | **Screen readers**: a message's label gives sender, time, delivery state, "edited" and reactions; bubbles are list items. | S |

## Phase 7: one look (last)

Goal: every colour, size, radius, spacing and duration comes from one place. Twelve or so shared
components replace the copies, and a CI check keeps it that way.

**7.1 A `Theme` global** in `ui/theme.slint`, derived from `Palette` so the desktop's colours
still apply:

| Group | Tokens | Replaces |
|---|---|---|
| Text | `text`, `text-secondary` (0.7), `text-tertiary` (0.55), `text-faint` (0.4) | 16 foreground alphas: 0.8/0.75/0.7/0.65 → secondary, 0.6/0.55/0.5 → tertiary, 0.45/0.4/0.35 → faint |
| Fills | `hover` (0.06), `pressed` (0.1), `selected` (accent 0.18), `subtle` (0.025) | 0.06/0.07/0.08, 0.1/0.12 |
| Status | `danger` (one shade for light, one for dark), `on-accent` (white) | `#c4314b` ×3 |
| Bubbles | `bubble-outgoing`, `bubble-incoming`, `bubble-highlight` | the expressions inline in `Bubble` |
| Type | `caption` 0.75, `small` 0.8, `body-small` 0.85, `body` 1, `title` 1.1, `heading` 1.3, `display` 1.6 rem | 15 sizes; 0.65, 0.7, 0.72, 0.864, 0.9, 1.05, 1.2, 1.4 folded in |
| Radii | `radius-s` 6, `radius-m` 8, `radius-l` 12, `bubble` 16 / `bubble-tight` 5, pills `height / 2` | 10 radii |
| Spacing | 4, 8, 12, 16, 24 (2 and 6 for tight spots) | ad hoc paddings |
| Motion | `fast` 120, `normal` 160, `slow` 240 ms, each × `Motion.scale` | 100–250 ms in 60-odd places |
| Elevation | `popover`, `dialog` shadows | three variants |

To check first: whether `rem` in a global follows the window's text size. If it doesn't, the type
tokens live in the text components of 7.2 instead.

**7.2 Text components**: `Caption`, `Small`, `Body`, `Title`, `Heading`, `Display` and the existing
`SectionTitle`, each a size and a colour role.

**7.3 Shared components** instead of copies:

| Component | Replaces |
|---|---|
| `FadeIn` | 19 copies of `opacity: 0; init => { self.opacity = 1; }` |
| `Pressable` / `ListRow` | 18 hover backgrounds, with press state and accessibility built in |
| `TabStrip` | folder tabs, media tabs and sticker-pack tabs, which differ in height (28/32) and radius (14/6) today |
| `Banner` | the six bars above the composer (request, invited, requested, left, safety number, blocked) |
| `Popover` | the reaction bar, the emoji and sticker pickers, both activity tooltips |
| `Chip` | reactions, staged files, the day label |
| `PanelHeader` | details ("Close"), media ("Close"), search ("Close"), calendar ("Back to chat"), settings ("Back"): one title-and-back header |
| `EmptyState`, `SmallSpinner`, `DangerButton` | the empty texts, spinners of 14/20/24 px, 6.6's button |

**7.4 Icons instead of text glyphs**:
- **Swaps:** ▶/⏸ → `play`/`pause`, ⏱ → `timer`, ✓ → `check`, 📎 → `paperclip`/`file`, 🎤 → `mic`. The "GIF" badge stays text.
- **Where the glyphs come from:** core strings (`state.rs:268, 272, 1519`, `lib.rs:434`, `ui.rs:2088`). Chat rows, attachments and notices get a kind or icon field instead, and notifications keep their emoji.
- **Hit targets:** at least 24 px (removing a staged file is 20 px today).

**7.5 Layout**: one header height for the sidebar and the chat; the same paddings in panels and
dialogs; the same button order in every dialog (Cancel, then the primary or destructive action).

**7.6 Keeping it**: a CI step that fails on literal `with-alpha(`, hex colours, `font-size:`,
`border-radius:` or `duration:` values outside `theme.slint` and the shared components.

**7.7 Checking it**: the 0.4 screenshots before and after (light, dark, Fusion dark); Small and
Large text; reduced motion. Then regenerate `docs/screenshots` and the demo GIF.

## Every finding, and where it is addressed

| Finding | Items |
|---|---|
| One changed message rebuilds every row; `Update::reset()` for chat-menu actions | 3.1, 3.3, 3.6 |
| `render` on every keystroke recomputes panels; O(n²) selection | 3.4, 3.5 |
| `last_messages(1)` walks the whole history | 3.3 |
| Endless spinner while offline | 1.5 |
| List drawn under opaque overlays | 4.1 |
| Older pages inserted row by row | 3.8 |
| Every row starts a 900 ms animation | 1.6 |
| Unbounded event batches | 1.8 |
| Calendar's element count | 4.2 |
| Blocking D-Bus call on the UI thread | 1.7 |
| Decoded media never freed, kept twice; stickers at 640 px | 4.3, 4.4, 4.5 |
| Missing `(thread_id, ts)` index; `coalesce` bounds | 1.1, 5.7 |
| Chat list waits for the offline queue | 5.1 |
| Commands one at a time; CPU work on the runtime | 5.2, 5.3 |
| History import fsyncs per message | 5.7 |
| Stale previous bubble on append | 1.2 |
| Emoji appended at the end | 1.3 |
| "Really leave?" never resets | 1.4, 6.6 |
| Callbacks by row index | 3.2 |
| Unused imports, misplaced comment | 1.9 |
| Opens at the bottom, past the unread line | 6.1 |
| No jump to latest | 6.2 |
| No tooltips; hidden message times | 6.3 |
| Enter in the chat search does nothing | 6.4 |
| Message actions hard to find | 6.5 |
| Destructive actions styled as primary | 6.6 |
| No seeking; 32 emojis; no "+" for reactions | 6.7, 6.8 |
| Screen-reader labels for messages | 6.9 |
| Glyph icons, 16 alphas, 15 font sizes, 10 radii, hex colours, copied patterns | 7.1–7.6 |
| `app.slint` / `AppWindow` size; state in globals | 2.1, 2.2 |
| Changes recorded by `ChatState` | 3.1 |
| `state.rs` and `signal.rs` size; `Session` | 2.4, 2.5 |
| Typed errors; injected clock | 5.8 |
| Also from the core and backend review: stale sender avatars, group id per typing message, lost update in `LocalStore`, whole-chat side data per page, HTTP client per download, many small queries, full-snapshot events | 3.7, 5.4, 5.5, 5.6, 4.6, 5.6, 5.6 |

## Decisions

- **Q1** Start with the look now. Done right after 2.1.
- **Q2** Prepare the presage changes as branches on the fork, to push and propose. They are
  `pr/thread-messages-index`, `pr/batch-save-messages` and `langis/integration-next` in a clone
  at `.presage-work/` (untracked; don't commit it), with PR texts in `.presage-work/PR-*.md`.
  Once they're merged, bump presage's rev, append to `b33p-core/tests/presage-migrations.txt` and
  drop b33p's own index.
- **Q3** Keep the start-up screen. While it waits for the messages that came meanwhile, a "Start"
  button appears after 1.5 s and goes to the chats at once; catch-up goes on, and the sidebar
  shows "Catching up…".
- **Q4** Download evicted pictures again. The core keeps up to 160 MB of downloaded pictures; past
  that the oldest ones are forgotten, and fetched again when shown.
- **Q5** Native buttons. In a confirmation, Cancel is the default and the destructive button names
  the action.
- **Q6** A dependency: the `emojis` crate.

`SCHEMA_VERSION` didn't need to change: the index is made in `open_store`, and older builds
ignore it.
