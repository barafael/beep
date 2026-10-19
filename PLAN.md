# Plan

What is left to make b33p a full Signal client on the desktop. Payments and calls are out
of scope. Steps go in order: each builds on the one before.

## Where it stands

- **Base:** linking with message history transfer (45 days of attachments), relinking when the
  device is removed, encrypted store (keyring or password), tray, single instance,
  notifications, several widget styles and themes.
- **M1 (done):** read state and receipts, delivery ticks, typing indicators, edits and
  deletions, disappearing messages, formatting, mentions, pinned / muted / archived chats,
  history a page at a time, Note to Self, chat settings read from the storage service.
- **M2 (done):** message requests, blocking, chat folders (read), details and safety numbers,
  forwarding, search in a chat, media overview, stickers, link previews, paste and drop, blurred
  previews, keyboard shortcuts, screen-reader labels; chat settings written back to the storage
  service (opt-in).
- **M3 (done):** group management (create, rename, description, timer, members, admins,
  leave; others' changes as notices), audio and voice message playback, stories (viewed with
  receipts for 24 hours; text and photo stories posted to groups). Reviewed twice; the fixes
  are in.

- **0.2.0 (done):** a screen for start-up failures with ways out (try again, start over, use a
  password instead of a locked keyring, forgot password, unlink); the store refuses data from a
  newer build instead of calling it a wrong password, and linking again keeps the messages;
  a lock on the data directory; device names; the connection's state; About; a scrubbed log
  file; builds on Debian 12; the Windows icon and `--unregister`; the AGPL text in every
  package; release notes from CHANGELOG.md.

None of M1–M3 has been checked against a real account yet. Everything below waits for that.

## 1. Verify on a real account

Go through TESTPLAN.md, in this order, and note each result there:

1. §1 (M1) and §2 except 2.13: mostly reading and sending; little can go wrong.
2. §3 (M3) in a group made for testing, with people who know it is a test.
3. §2.13 last, one change at a time: it writes to the account's shared settings.

Then fix what fails, most harmful first (anything that loses messages, misleads about who
reads what, or changes the phone's settings unasked). Re-run the affected checks. This step is
done when every check is ✅ or has an issue noted here.

## 2. Make the build stand on its own

Done: every change we need in presage and libsignal-service is offered upstream, and b33p
builds from branch `langis/integration` of each fork: upstream main plus our open pull requests,
so what b33p runs is exactly what is proposed. Nothing waits on a merge.

- libsignal-service-rs: #473 group calls, #474 drop sync messages from other accounts on the
  PNI receive path too (a security fix; reproducer in `~/libsignal-pni-sync-repro`), #476 send a
  sync message to ourselves once, not twice.
- presage: #442 history-transfer key on `RegistrationData`, #447 wait for the history archive
  (stacked on #442), #443 paged thread reads, #444 storage service from a linked device,
  #445 drop sync messages from other accounts, #446 a linked device unlinks itself, #448 create
  and change groups (draft until #473 lands; it pins libsignal-service to #473 meanwhile),
  #449 report only real identity changes, and note them, #450 pin an identity required for
  sending, #453 only a message's sender deletes or edits it, #454 write transactions take the
  lock up front, #460 the provisioning link logged at debug, #461 why a message failed to
  decrypt, and retry receipts as their own event.
- rusqlite (whisperfish's SQLCipher fork), through branch `langis/integration` of our fork:
  whisperfish/rusqlite#2 (not ours) builds on Linux arm64 and ends a crash at exit; ours, #3, lets the
  Rust crypto provider build on Windows without OpenSSL.

Contact discovery brings BoringSSL, so SQLCipher uses a crypto provider in Rust and nothing links
OpenSSL. On Windows arm64, BoringSSL is built with Ninja and clang-cl (MSVC can't assemble it).

Upstream asks for short pull requests, commits and comments, and `Assisted-by:` rather than
`Co-Authored-By:` (their AGENTS.md). #444 and #449 are one commit each now; #448 is the feature
plus the temporary #473 pin.

Left: when upstream moves or a pull request changes, rebuild the integration branches (cherry-pick
the PR branches onto the new main) and bump the revisions in `Cargo.toml`; drop each entry as its
PR lands.

When rebuilding presage's branch, its store migrations may only grow at the end: users'
databases record every migration they ran, and sqlx refuses one whose migrations changed or
went missing. `presage_migrations_only_ever_grow` checks them against
`b33p-core/tests/presage-migrations.txt` (identical to 0.1.0's). b33p's own tables carry a
version (`SCHEMA_VERSION` in `local.rs`), raised only for a change older builds would misread.

## 3. M4: close the gaps M1–M3 left

By how much they matter, most first:

- **Safety numbers:** done: a notice when a contact's safety number changes (presage#449), and
  marking it verified or clearing that from the details page, told to our other devices as
  Signal's apps do and taken over from the phone (its sync messages and the storage service),
  with notes in the chat. Verified is per key, so a new key isn't; when a verified contact's
  key changes, nothing is sent to them until that is confirmed ("Continue") or verified again.
  Known limits: only 1:1 chats wait for that, not groups the contact is in (Signal asks there
  too); the change notice is timed when the change was noticed (after a long time
  offline it can sort below the first message with the new key); notices show in the 1:1 chat
  only, not in shared groups; a change notice appears live only within five minutes of the
  change; the parts that need a linked account (`announce_identity_change`, `set_verified`,
  `remember_verification`, `trust_of`, `deliver`) have no test, the rest does. The first message
  after a verified contact's key changed is held too: the store refuses their new key for
  sending (presage#450 pins the verified one) and the chat waits for "Continue".
- **Groups:** accepting or declining an invitation is done here (presage#448;
  invitations to our PNI too), and a change that clashed with someone else's is built again on
  the latest revision. People known only by phone number
  can be found (Signal's contact discovery), then messaged or added. The group's picture can
  be set and removed here (libsignal-service#473 and presage#448 carry the upload), and
  another member's change of it shows. Invite links: a member sees and copies the group's
  link; an admin turns it on (for anyone, or with their approval) or off, resets it, and lets
  people in who asked or turns them down. A link pasted into "Join a group by link" (or
  clicked in a chat) shows the group and joins it, or asks to; the request can be taken back
  (libsignal-service#473 and presage#448 carry the join). Done.
- **Storage service:** records for chats the phone has none for (only existing records are
  changed now); writing folders (create, edit); taking over the read-receipt and typing
  settings.
- **Stories:** posting to My Story and custom lists (their audiences are distribution lists
  in the storage service, so this needs the storage write to be proven first); replies and
  reactions; removing stories their author deleted; Signal's own setting for story receipts
  (they follow read receipts now).
- **Media:** recording voice messages (audio input across desktops); a seek bar; animated
  GIFs and video thumbnails (need a video decoder).
- **Known limits from M1:** the earlier text of someone else's edit is lost after a restart
  (presage overwrites the stored message first); a group counts as read once any member read a
  message; pinned chats move by menu, not by dragging.
- **Selecting text in messages:** done through our Slint fork (barafael/slint, branch
  `styled-text-selection`: master (1.19) plus a `text-selectable` `StyledText` with selection
  colours, `has-selection`, `set-selection-offsets`, `select-all`, `clear-selection` and
  `copy()`, the read-only subset of what `TextEdit` exposes; a click that doesn't drag still
  follows a link), pinned as a git dependency in the workspace, since the change is in the
  `slint` facade too. An input laid over the styled text to fake it was tried first and
  dropped: a second copy of the text, drifting along bold and code, in the way of links. Sent
  upstream as draft slint-ui/slint#13985. Left: see the review through and drop the fork once
  a release carries it.
- **Tests:** a sticker picker test that sees the stickers (the testing backend doesn't lay
  out popups).
- **Device name:** renaming a device that is already linked (presage has no call for it yet;
  libsignal-service's `update_device_name` would do). Until then a new name waits for the next
  link.
- **macOS:** notifications (`UNUserNotificationCenter`, which wants a signed bundle);
  notarizing the builds; a menu-bar icon that follows the menu bar's colours (Slint's tray has
  no template images); opening the window from the Dock icon, unchecked; starting with the
  system.
- **Windows:** an installer, and signing (SmartScreen).

## 4. Ready for others

- Each package checked on a clean system (TESTPLAN.md §6): the AppImage on GNOME, the .deb on
  Debian 12, the zip on Windows, the dmg on macOS.
- Large accounts: start-up time, memory and scrolling with years of history.
- A screen-reader pass with Orca through the main tasks.
- Done: logs for bug reports without message content in them (scrubbed, in a file; Settings →
  About opens the folder).
