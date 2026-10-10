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
  PNI receive path too (a security fix; reproducer in `~/libsignal-pni-sync-repro`).
- presage: #442 history-transfer key on `RegistrationData`, #447 wait for the history archive
  (stacked on #442), #443 paged thread reads, #444 storage service from a linked device,
  #445 drop sync messages from other accounts, #446 a linked device unlinks itself, #448 create
  and change groups (draft until #473 lands; it pins libsignal-service to #473 meanwhile),
  #449 report only real identity changes, and note them.

Left: when upstream moves or a pull request changes, rebuild the integration branches (cherry-pick
the PR branches onto the new main) and bump the revisions in `Cargo.toml`; drop each entry as its
PR lands. A CI job (build, test, clippy) later.

## 3. M4: close the gaps M1–M3 left

By how much they matter, most first:

- **Safety numbers:** a notice when a contact's safety number changes is done (presage#449
  makes the store report real changes and note them; langis shows the note as a centred line,
  without notification, unread count or receipt). Left: marking it verified (and sending that
  to our other devices; reading what the phone set), and resetting it on a change. Known limits
  of the notice: it is timed when the change was noticed, so after a long time offline it can
  sort below the first message with the new key; it shows in the 1:1 chat only, not in shared
  groups. This is about trust, so it comes first.
- **Groups:** accepting an invitation here (now done on the phone); adding people known only
  by phone number (invite them by PNI); retrying a change that clashed with someone else's;
  the group picture; invite links and join requests.
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
- **Tests:** a sticker picker test that sees the stickers (the testing backend doesn't lay
  out popups).

## 4. Ready for others

- Packaging (`packaging/linux`) checked on a clean system, with the build from step 2.
- Large accounts: start-up time, memory and scrolling with years of history.
- A screen-reader pass with Orca through the main tasks.
- A way to collect logs for bug reports without message content in them.
