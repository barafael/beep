# Plan

What is left to make langis a full Signal client on the desktop. Payments and calls are out
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

The group changes in libsignal-service-rs live only in a local clone (`../libsignal-service-rs`,
branch `langis/groups`), so langis builds on this machine only.

1. Push that branch to a fork (as with presage: `barafael/libsignal-service-rs`) and point the
   `[patch]` at it by revision, so a fresh checkout builds.
2. Offer the changes upstream (whisperfish/libsignal-service-rs) as a pull request — your
   call; until they land, the fork stays.
3. A CI job (build, test, clippy) on every push, so a broken build shows at once.

## 3. M4: close the gaps M1–M3 left

By how much they matter, most first:

- **Safety numbers:** a notice when a contact's safety number changes, and marking it
  verified. Needs the presage fork to report identity changes. This is about trust, so it
  comes first.
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
