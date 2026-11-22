# Plan

What is left to make b33p a full Signal client on the desktop. Payments and calls are out of
scope. Section 3 is in order of how much each gap matters, most first; the other sections run
beside it.

**How a feature counts as checked.** The [test box](testbox/README.md) runs Signal-Server in its
test mode with Signal-Android and Signal-Desktop built from their sources, on accounts of their
own: a phase that passes there counts as checked on a real account. b33p also runs every day on
a real account, informally. TESTPLAN.md marks each check the box covers with the full run that
last passed it; the unmarked rest (packages, a screen reader, what only a long-lived account
shows) is the pass by hand before b33p goes to others (section 6).

## Where it stands

- **Base:** linking with the message history transfer (attachments as long as Signal's CDN
  keeps them, about 45 days), relinking when the device is removed, an encrypted store (keyring
  or password), tray, single instance, notifications, the desktop's own widget style, light and
  dark.
- **M1 (done):** read state and receipts, delivery ticks, typing indicators, edits and
  deletions, disappearing messages, formatting, mentions, pinned / muted / archived chats,
  history a page at a time, Note to Self, chat settings read from the storage service.
- **M2 (done):** message requests, blocking, chat folders (read), details and safety numbers,
  forwarding, search in a chat, media overview, stickers, link previews, paste and drop, blurred
  previews, keyboard shortcuts, screen-reader labels; chat settings written back to the storage
  service (opt-in).
- **M3 (done):** group management (create, rename, description, timer, members, admins,
  leave; others' changes as notices), audio and voice message playback, stories
  (viewed with receipts for 24 hours; text and photo stories posted to groups, to My Story and
  to the account's own story lists).
- **0.2.0 (done):** a screen for start-up failures with ways out (try again, start over, use a
  password instead of a locked keyring, forgot password, unlink); the store refuses data from a
  newer build instead of calling it a wrong password, and linking again keeps the messages;
  a lock on the data directory; device names; the connection's state; About; a scrubbed log
  file; builds on Debian 12; the Windows icon and `--unregister`; the AGPL text in every
  package; release notes from CHANGELOG.md. Also in it: selecting text and picking messages, a
  group's picture and invite links.
- **Since 0.2.0 (CHANGELOG.md, Unreleased):** pinned messages; notifications per chat; the
  unread count on the tray and taskbar; b33p's own picture viewer; seeking in audio; screen
  security on Windows; on Windows, installing itself from the zip and a signed update check;
  faster starts and reading on large accounts; My Story, story rings and a full-window viewer.

## 1. TESTPLAN.md

43 of its checks are marked as passed by the box (the 2026-11-21 full run). Left, by hand:

1. What the box can't do: the packages on clean systems (§6), the screen reader (§2.14 and
   §4b's item 12), the phone's own screens where the box only drives Desktop, the review fixes
   and the UI review (§4, the rest of §4b) and what goes wrong on Signal's own servers (§5).
2. §2.13 last, one change at a time: it writes to the account's shared settings.
3. Note each result there. Fix what fails, most harmful first (anything that loses messages,
   misleads about who reads what, or changes the phone's settings unasked).

## 2. The forks

b33p builds from our forks of presage, libsignal-service-rs, rusqlite and Slint, each pinned by
revision in Cargo.toml (and, for presage and libsignal-service, in testbox/presage/Containerfile
and testbox/stickers/Cargo.toml; the `pins` test checks they agree). **What upstream would take
is offered there**, in their style (short pull requests, commits and comments; `Assisted-by:`,
not `Co-Authored-By:`, per their AGENTS.md). What only b33p needs stays a documented delta.
Pull request states as of 2026-11-21:

- **libsignal-service-rs**, branch `langis/integration`, built by its `integration.sh`: upstream
  main with each topic branch merged in, in order (a merge done by hand kept as
  `merge/<topic>`).
  - Open: #476 transcripts once (and its follow-up branch `pr/sync-message-send`), #481 a healthy
    socket kept after a stall, #482 websocket errors that say what failed, #488 a session lock,
    #490 a GET not failing on a closing connection, #491 a replenishment's keys saved together,
    #442 (rubdos') the envelope ack handle, with our #489 merged into it; drafts: #493 stories
    sent as stories, #495 Accept-Language, #496 a signed group change, #497 the group change log.
  - Merged upstream, gone from the branch: #473 create and change groups, #474 sync messages
    from other accounts dropped on the PNI path, #492, #494, #503, #504.
  - No pull request yet, to offer: `pr/sender-key-send` (a group message in one request),
    `pr/story-list-send` (the same without a group id, for story lists), `fix/sealed-plaintext`.
  - b33p's own: `pr/custom-servers` (servers of one's own, which the test box needs).
- **presage**, branch `langis/integration`: upstream main with our changes as one commit per
  theme, rebased by hand (not cherry-picked pull requests); Cargo.toml's comment lists each.
  - Open: #447 wait for the history archive, #449 only real identity changes, #450 a pinned
    identity, #461 why a message failed to decrypt, #473 session steps one at a time, #476 one
    transaction per envelope, #477, #478 link previews off, #483, #499 (work owed after a message
    kept as jobs; it carries what #444, #453, #472, #475, #480 and #484–#487 were, now closed),
    #503, #504; drafts: #448 create and change groups (its pin of libsignal-service to #473 is
    obsolete now that #473 landed: rebase it and take it out of draft), #464, #489, #490, #491,
    #492, #493.
  - Merged or settled upstream: #442, #446, #454, #460 merged; #443 replaced by upstream's #502;
    #445 closed, fixed by libsignal-service-rs #474.
  - No pull request yet, to offer: `send_story_to_list` (on `pr/story-list-send`), a thread's
    messages found by `ts BETWEEN` through b33p's index.
  - b33p's own unless upstream wants it: `Manager::server_alerts`, and building on our
    libsignal-service fork.
- **rusqlite** (whisperfish's SQLCipher fork), branch `langis/integration` of ours:
  whisperfish/rusqlite#2 (not ours: builds on Linux arm64, ends a crash at exit) and our #3 (the
  Rust crypto provider on Windows without OpenSSL), both open.
- **Slint**, branch `slint/integration-branch` of ours, built by its `integration.sh`: upstream
  master with #14823 (no animation from a value to itself), #14841 (scrollbar thumb), #14855
  (cursor blink timeout), #14721 (inline code padding), #14953 (shaping only changed paragraphs),
  all open, drafts #15091 (an open popup moving with its parent) and #15097 (a `show` handler on
  a `ContextMenuArea` in place of the built-in menu, for our own menus in `menu.slint`), and
  edualc's text work (no pull request; merged with #14721 by hand as `merge/edualc-text`). The
  branch's `integration.sh` lists them in order, and `push` tags each old tip
  `integration/<date>-<commit>`. Owed: #14953 overlaps ogoffart's #14742 (plain text only; ours
  covers StyledText and content widths), so rebase it onto that as a follow-up; #14721 asks on
  the PR whether a U+FFFC under each in-flow inline box (which parley 0.13 needs) is sane. #13985 (selecting text) landed. Back to a
  release once one carries these.

Contact discovery brings BoringSSL, so SQLCipher uses a crypto provider in Rust and nothing links
OpenSSL (CI checks both). On Windows arm64, BoringSSL is built with Ninja and clang-cl.

Left: rebuild the integration branches when upstream moves or a pull request changes, and bump
the revisions; drop each entry as it lands; open pull requests for the "to offer" items. When
rebuilding presage's branch, its store migrations may only grow at the end: users' databases
record every migration they ran, and sqlx refuses one whose migrations changed or went missing.
`presage_migrations_only_ever_grow` checks them against `b33p-core/tests/presage-migrations.txt`
(9 at 0.2.0, 15 now). b33p's own tables carry a version (`SCHEMA_VERSION` in `local.rs`), raised
only for a change older builds would misread.

## 3. M4: close the gaps

By how much they matter, most first.

- **What others expect b33p to honour:**
  - The account's default disappearing timer (`universalExpireTimer` in the account record) is
    not read: chats and groups started in b33p don't get it. Read it with the other account
    settings (storage.rs), apply it in commands.rs when a chat or group is made, show it in
    Settings (read-only: setting it would write the account record).
  - A story its author deleted stays shown here: deletes only touch thread messages
    (receive.rs), `b33p_story` goes by age alone.
  - b33p's own stories carry no `profile_key` (the `StoryMessage` in commands.rs): someone
    without b33p's key can't tell who posted it.
- **Parity with the phone** (what only a long-lived account shows; design and evidence in
  docs/account-parity.md):
  - Contacts' profile keys from the storage service (`ContactRecord.profileKey`), so that
    contacts who never wrote here get their pictures and sealed sends; first log how many lack one.
  - "Delete for me" deletes on the account's other devices too: send `SyncMessage::DeleteForMe`
    (and `ConversationDelete` when a chat is deleted). Incoming message deletes work; incoming
    conversation, local-only conversation and attachment deletes don't yet.
  - `dontNotifyForMentionsIfMuted`, with the per-chat notification settings; view-once opened
    elsewhere, hidden and unregistered contacts, preferred reaction emoji, "prefer contact photos".
  - A test of every storage record and sync message field, each marked taken, ignored or to do,
    failing on anything unmarked: a fork bump then shows what is new.
  - An aged account in the box: address-book contacts, a default timer, a contact who never
    wrote, a deleted message, a muted mention-only group.
- **Messaging:**
  - Retry receipts: b33p never sends a `DecryptionErrorMessage` when it can't decrypt (it only
    resets the session), so a first message from a new contact can be lost for good. It already
    answers others'.
  - A rate limit's `retry_after` shown to the user (errors.rs builds it with none).
  - Group sends with the group's send endorsements instead of the combined access key; a group
    log read that answers 403 read from `joined_at_version` (presage); group changes in one
    request; only changed pre-keys uploaded.
  - Enter while an input method composes a syllable (fcitx5 Hangul) commits and sends at once;
    Desktop only finishes the syllable. Decide whether to match it.
- **Safety numbers:** done: a notice when a contact's safety number changes (presage#449);
  marking it verified or clearing that from the details page, told to our other devices and
  taken over from the phone, with notes in the chat. Verified is per key: when a verified
  contact's key changes, nothing is sent to them, in their chat or in a group they are in,
  until it is confirmed ("Continue", in their chat) or verified again; the store refuses their new
  key for sending (presage#450). Left: a group asks there, as Signal does, instead of naming who
  to confirm in their chat; the change notice is timed when it was noticed (after a long time
  offline it can sort below the first message with the new key), shows only in the 1:1 chat, and
  appears live only within five minutes of the change; a test of a key change and the held send
  (the box's tour checks verifying and clearing only).
- **Groups:** done: accepting or declining an invitation (to our PNI too), a change that clashed
  with someone else's built again on the latest revision, people found by phone number (contact
  discovery), the group's picture, invite links (on, with approval, off, reset; requests let in
  or turned down), joining by link and taking a request back.
- **Storage service:** writing records for chats the phone has none for (only existing records
  are changed now); writing folders (create, edit); a write that failed as unreachable tried
  again later, not at the next change. (The read-receipt and typing settings are taken over from
  the phone already; link previews too, when the phone turns them off.)
- **Stories:** done: posting to My Story and the account's own lists (read from the storage
  service, to the connections they say, in one request with a sender key for the list, synced
  to the phone naming the list; [the 2026-11-21 runs](docs/testbox-stories-run-2026-11-21.md)).
  Replies and reactions others send arrive, as messages in the group. Left: showing them with
  the story they answer, sending them; editing who is on a list from b33p (needs the storage
  write proven first); Signal's own setting for story view receipts (they follow read receipts
  now); a look at Android's own story transcripts in the wire record.
- **Media:** recording voice messages (audio input across desktops); animated GIFs and stickers
  (APNG, WebP frames; a frame budget; reduced motion), video thumbnails (a video decoder);
  several pictures in one message as a grid opening the viewer.
- **Reading the chat** (from sigwin, docs/sigwin.md): who read a group message, as avatars under
  it (the core has `readers_of`); a message's info from its menu (the core has
  `open_message_info`); relative times within the hour; the time at the last line's end, dropped
  within a minute's run (decide by screenshots); a floating day over the timeline.
- **Known limits from M1:** the earlier text of someone else's edit is lost after a restart
  (b33p keeps the old text of its own edits only); a group message counts as read once any member
  read it (who read it is known per member); pinned chats move by menu, not by dragging.
- **Selecting text in messages:** done, through Slint since #13985 (a `selectable` `StyledText`;
  a click that doesn't drag still follows a link). Left: Slint back to a release (section 2).
- **Device name:** renaming a device that is already linked. presage has the call
  (`Manager::update_device_name`); b33p only keeps the name for the next link (session.rs).
- **macOS:** notifications (`UNUserNotificationCenter`, which wants a signed bundle);
  notarizing the builds; a menu-bar icon that follows the menu bar's colours (Slint's tray has
  no template images); opening the window from the Dock icon; starting with the system.
- **Windows:** done: installing itself from the zip for one user (Start menu, Installed apps,
  uninstall) and a signed update check. Left: signing (SmartScreen; SignPath or paid), an
  installer if one is wanted beside that, a Windows 11 machine in the box (a Korean-locale check,
  toast clicks, clipboard paste, restoring from minimized), memory on real Windows hardware with a
  GPU driver (and sigwin's software-renderer patches if it warrants), the file dialog in a
  process of its own if memory warrants.

## 4. The test box

What the phases don't check yet, and the box itself (findings and how the tools work:
docs/testbox-review.md, docs/testbox-stress.md, docs/testbox-tour.md, docs/testbox-wire.md):

- **Flows:** right-to-left text with Desktop; blocking in b33p, the phone taking it over;
  accepting a group invitation (also a tour step, which needs a b33p without the invitee's
  profile key); a safety number change after re-registering, confirmed; unlinked by the phone and
  linked again with the history transfer; the disappearing timer set from Desktop; the default
  timer and the deletes against Android.
- **Stress:** 50 chats and 20 groups scrolled, searched and opened; b33p killed during an upload,
  linking and a storage sync; the server restarted mid-send; a full disk and recovery; a
  zero-byte attachment; an edit chain of 20 with a reaction and a reply to the latest; group
  changes from two clients at once; b33p's clock ahead and behind; an ASan or valgrind build; a
  non-English locale and right-to-left UI; `PRAGMA integrity_check` and row counts after every
  forced stop; saving a 90 MiB file measured in a release build.
- **The box:** phases started from a snapshot of a linked box; CI for its own code (tools, vnc,
  viewer, web, shot, stickers, driver.ts); core dumps and a harness snapshot per failed check
  kept in the record (the tour keeps snapshots already); a record when a run is stopped; a report
  page per run; quieter logs; the harness snapshot with in-chat search and trust state.
- **Open findings:** presage-cli holding the write lock for 30 s about once in 40 tour rounds
  (tour finding 16); an emulator's first boot where `adb shell stop` dropped its transport (the
  stories runs; the driver now keeps the state around such a failure).

## 5. The core's structure and speed

- **Actors** (docs/actors.md: the rules, and steps 0–8, mostly done): an `Expiry` actor for the
  `expiring` and `story_kept` notices; a `Storage` actor for `storage_changed`, following and
  writing in one; a shutdown that drops the context and awaits the actors in order instead of
  aborting them; a trait over `Sending`'s presage calls, to test its ordering with a fake.
- **Speed:** batched saves for the history import: `ContentsStore::save_messages` (branch
  `pr/batch-save-messages` of our presage, not in `langis/integration`; an import far faster
  than one save a message): rebase it onto the integration branch, append its migration, import with
  `save_messages` about 1,000 at a time. (The thread index went another way: b33p's own
  `b33p_thread_messages_thread_ts_sender`, which our presage finds a thread's messages through;
  upstream #502 replaced our paged reads.) Store calls while catching up: reading
  `handle_content` suggests 6 to 8 small store calls per received message, measured only with the
  mock; count them with a counting layer around the store while an account receives a backlog of
  a few thousand messages in the testbox, to see whether one transaction per batch pays.
  `ChatMessage.quote` and `.preview` boxed, as `LinkPreview.image` is (that made a message about
  25% smaller): keep it if the mock's catch-up of a 200,000-message backlog
  (`B33P_MOCK_SCALE`) saves more than a few MB. A picked message counted as it is picked (a keystroke's cost grows with
  the number picked); an upload holding its file once, not twice; what the open chat holds in memory;
  presage keeping a working HTTP client. The chat pane a component of its own, as the chat list.

## 6. Ready for others

- Each package checked on a clean system (TESTPLAN.md §6): the AppImage on GNOME, the .deb on
  Debian 12, the zip on Windows, the dmg on macOS.
- Large accounts: start-up time, memory and scrolling with years of history (a mock of that
  scale exists: `B33P_MOCK_SCALE`). The soak's memory growth is undecided, leak or unbounded cache
  (docs/testbox-soak-memory-2026-11-11.md).
- A screen-reader pass with Orca through the main tasks.
- The README's screenshots and its animation taken again on a real desktop.
- Done: logs for bug reports without message content in them (scrubbed, in a file; Settings →
  About opens the folder).

## 7. Android

`b33p-dioxus` is b33p for Android: Dioxus in the system's WebView over `b33p-core`. It runs on the
emulator against the mock and against Signal as far as the code to link with (b33p-dioxus/README.md
has the details and what is different on Android). In order:

1. **A phone.** Run the arm64 build on a real device, link it to an account made for testing, and
   go through TESTPLAN.md §1 as far as the screens go. Until then nothing past the link code has
   been seen on Android.
2. **What the platform needs.** The database key in the Android Keystore, not a password;
   notifications and the app being stopped and started by Android (the backend dies with the
   process now); a release build that is signed. `dx`'s APK is debug-signed and `debuggable`
   (anyone with `adb` can read the app's files), so a build to hand out needs its own signing
   and a non-debuggable variant.
3. **The screens the desktop client has:** attachments and the file picker, groups and their
   details, search, forwarding, stories, stickers, settings; text styling, links, mentions; older
   messages as one scrolls.
4. **Shared code.** The desktop's log (`b33p-platform`, with its desktop-only crates) and
   notifications: `b33p-notify` has freedesktop and Windows backends only, and its `unix`
   condition would pull D-Bus into Android. A CI job, which needs the NDK.
