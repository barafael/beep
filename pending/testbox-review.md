# The testbox, looked back on (October 2026)

What stood in the way of a green testbox, what the box would do better, and the user flows it
doesn't check yet. The branches themselves are in [branches.md](branches.md).

## What stood in our way

1. **Losses that passed.** Every check could pass while a message was lost on the way: a sync
   copy, an answer to a request. Only the lost-message check (#67) made them visible, and with
   it every phase with Desktop failed. The cause, a session race in presage and
   libsignal-service, had been fixed already; the fix hadn't reached the box.
2. **Two pins for one thing.** The box builds its presage-cli and sticker tool on the presage
   and libsignal-service b33p pins, but names those revisions in four files of its own. #69
   moved b33p and not them, so the box's presage-cli kept the race. A test now fails on that
   drift (`xtask/src/pins.rs`).
3. **The box itself.** `with-display`, PID 1 in each client container, ignored SIGTERM:
   `podman restart b33p` killed b33p ten seconds later with its database open, and what that
   lost looked like a protocol fault.
4. **Evidence that went away.** The logs were gone with the next `down --wipe`, and Desktop's
   own logs and Android's logcat were never in `podman logs`. Each investigation started with a
   script to collect them. Now every phase keeps them, and screenshots of each check
   (`target/testbox-record/`).
5. **Checks that assumed.** The failed-send check first stopped the server, which b33p answers
   by not letting anyone write; then it waited less than libsignal-service's keepalive takes to
   give up. Both were the check's fault, and only the screenshots and logs said so quickly.
6. **A box unlike a desktop.** b33p's image had no emoji font, so emoji drew as boxes and a crash
   every Debian 12 user with Noto Color Emoji would meet stayed hidden: Qt 6.4's `QRawFont`
   overruns the heap on colour bitmap glyphs. Adding the font for nicer screenshots found it.
7. **Test data unlike conversations.** One-line ASCII messages, never edited before being
   reacted to. The first check that reacted to an edited message found that b33p (and presage)
   look an edited message up only by its first timestamp, while Signal's apps name it by its
   latest edit's.
8. **Slow rounds.** Each phase needs a fresh box: about ten minutes, an hour for all six. A
   fix is confirmed hours after it is written, and two runs can't share the machine (one pod
   name).
9. **Work spread out.** The fix for the race was written and proven in other sessions, on
   branches named `integration-next`, `pr/session-lock`, `debug/session-trace-fixed`, with #69
   open; finding what was where took longer than the fix. `branches.md` is the remedy here.

## How the box could be better

- **Start phases from a linked box.** Snapshot the volumes after `up` and linking (one template
  per client set) and start each phase from a copy: minutes saved per phase.
- **Phases at once.** The pod name and network as parameters, so that `run` can play two or
  three pods side by side; Android's emulator is the limit.
- **CI for the box's own code.** None of `testbox/tools`, `vnc`, `viewer`, `web`, `shot` or
  `stickers` is built in CI, nor `driver.ts` type-checked outside its image build. A job that
  builds and lints them, and runs `core` in a container, would catch most breakage early.
- **Memory errors on purpose.** A `--valgrind` (or ASan) build of b33p's image for a nightly
  run; and core dumps of the box's processes copied into the record (`coredumpctl dump`), as
  this one had to be by hand.
- **A report per run.** `run` writes the record; an `index.html` beside it with each phase's
  checks, their outcome and screenshots side by side would make a run reviewable at a glance.
- **More of a real desktop.** Emoji font (done), a CJK font, a right-to-left text, scale factor
  2 on b33p's display, a non-English locale.
- **Snapshots of the harness with the screenshots.** b33p's JSON snapshot after each check, so
  that what the check saw is in the record as data, not only as a picture.

## User flows the phases don't check yet

Added in this round: reactions, replies and emoji each way and a delete for everyone from
Desktop (`desktop`); a send to a server that doesn't answer, and its retry (`core`); that no
client crashed (every phase).

Still missing, roughly by how often people do them:

- A long text (over 2 KB: sent as a long-text attachment) and right-to-left text, each way
  with Desktop.
- Typing indicators each way.
- Read receipts from b33p (window focus) to Desktop; b33p marking read what Android read.
- Disappearing messages in a one-to-one chat: a timer set on Desktop, the messages gone on
  b33p when it runs out.
- Forwarding a message with its attachment.
- Link previews each way.
- Blocking and unblocking a contact on b33p, and the phone taking it from the storage service.
- b33p joining a group by its invite link, and accepting an invitation.
- A safety number change (the peer re-registers) and confirming it.
- Being unlinked from the phone's list, and linking again with the history transfer
  (TESTPLAN.md §5 item 8).
- b33p killed while receiving (SIGKILL), and nothing lost after it starts again.
- A network that drops during an attachment's upload (CDN paused).

## Seen on the way, since changed

- After a send failed and Retry sent it, the error banner stayed up, reading as the library's
  error. It now says it in words and goes with the retry (core checks it).
- Receipts and read syncs for edited messages named the first time only; Signal's apps name
  the latest edit. b33p now takes and sends them as Signal-Desktop does (storage checks both
  ways).
