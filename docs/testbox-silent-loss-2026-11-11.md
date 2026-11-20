# What would let a lost message pass the testbox? (investigation of finding 3, 2026-11-11)

Finding 3 of [testbox-sceptical-run-2026-11-11.md](testbox-sceptical-run-2026-11-11.md): "no
client lost a message" passed on a b33p that dropped half of what it received. This asks how
big that gap is: which losses the end-of-phase checks see, which only the direct checks see, and
which nothing sees.

## Short answer

- "No client lost a message" sees **protocol-level** losses only: decryption errors, duplicates,
  counter reuse (`xtask/src/checks/lost.rs`, the lines b33p, Desktop, Android and presage-cli
  log for those). It is blind to anything b33p does after it has decrypted and stored a message.
- What stands behind it are the phases' **direct checks**, which look for specific messages they
  sent. They catch a drop of the kinds they send (below), and the end-of-phase "no unexpected
  error" check catches a drop that logs an ERROR line. A drop that is silent **and** in a path no
  direct check reads goes through. Experiments found three such paths (read on the phone,
  blocked on the phone, a request answered on the phone) and one other hole: a backend that
  stops without a panic left no line in the log (fixed here).
- Closing the gap in general needs a ground truth that is not b33p. The cheapest sound one is
  a ledger of what the drivers send to b33p's account, compared with what b33p logged taking in
  (design below). I did not build it; it touches every send helper of every phase.

## Experiments

Both on 806c673 in a throwaway worktree (`../beep-sceptic-mut`, patches `m1-drop-and-log.patch` and
`m2-silent-handlers.patch` in `docs/testbox-sceptical-run-2026-11-11-mutations/` of the main tree), instance `sc3`.

### M1, a loud drop (earlier report)

`handle_content` drops every data message from another account with an even timestamp and logs
an ERROR. `test desktop core`: 12 of 33 `desktop` and 4 of 8 `core` direct checks failed (each
after its 120 s patience), "b33p logged no unexpected error" failed in both, **"no client lost a
message" passed**.

### M2, silent no-ops in four handlers (`test desktop android groups storage`)

`on_read_elsewhere` (a message read on the phone), `on_blocked_elsewhere` (the phone's block
list), `on_answered_elsewhere` (a request accepted or deleted on the phone), each made to return
at once, with no log line, and `on_typing` as a positive control.

| handler made a no-op | did the content reach b33p in this run? | did any check fail? |
|---|---|---|
| `on_typing` (control) | yes (Desktop's typing) | **yes**: 7 of 33 `desktop` checks, but see below: the mutation returned `false`, which stops b33p's session, not only the handler |
| `on_read_elsewhere` | yes: 3 `syncMessage.read` from Android | **no**: `android` 18/0, `groups` 30/0, `storage` 14/0 |
| `on_blocked_elsewhere` | yes: 1 `syncMessage.blocked` from Android | **no**, same runs |
| `on_answered_elsewhere` | **no**: no `syncMessage.messageRequestResponse` in the content log | not exercised at all |

So what a phone's reading and blocking do to b33p is **not checked by any phase**, even though
the box delivers those messages to b33p; and accepting a request on the phone is not even sent.

The control also showed something else. With `on_typing` returning `false`, b33p's backend thread
ended ("b33p stopped working unexpectedly", screen `Failed { Stopped }`) and 6 more `desktop`
checks failed on a dead session, but **"no client crashed" and "b33p logged no unexpected error"
passed**: the process was alive, and the window's message is not a log line. Nothing at the end of
a phase looks at that state. That is fixed here (the ERROR line below).

## What the phases send to b33p (measured), against what b33p handles

From the decoded contents of the capture run E (`compare.md` "Contents", both boxes, every field
b33p took in):

- Taken in: `dataMessage` (text, body ranges, attachments, quote, reaction, delete, pin and
  unpin, group context and its changes, expire timer and its flag, view-once flag, story
  context), `editMessage`, `receiptMessage` (delivery, read), `typingMessage`, `storyMessage`;
  sync: `sent` (17), `fetchLatest` (44), `read` (5 and its batches), `blocked`, `keys`,
  `stickerPackOperation`.
- **Never taken in** by any default phase, although `handle_content`
  (`b33p-core/src/signal/receive.rs`) has a path for most: `syncMessage.messageRequestResponse`,
  `viewed` and `viewOnceOpen`, `deleteForMe`, `contacts`/`configuration` as syncs, `verified`,
  `callMessage` and the call syncs, an incoming link `preview`, `sticker`, `contact`, `payment`,
  gift badge and poll in a data message, `groupCallUpdate`, number changes. (The tour sends b33p's
  own features between b33ps, which covers several of these in one direction: tour.rs.)
- Taken in, but with no check of its effect: `syncMessage.read`, `syncMessage.blocked` (M2),
  `syncMessage.keys` beyond the storage phase working, the view-once flag, `expireTimer` of
  an incoming message beyond Desktop's timer checks.

## What I changed

- The window's "b33p stopped working unexpectedly" (the events channel closing without a
  report, both windows: `b33p-slint/src/main.rs`, `b33p-gpui/src/shared.rs` `backend_gone`) now
  logs an ERROR first, so the existing "b33p logged no unexpected error" check sees a stopped
  backend. Before, that state left no line in the log and no end-of-phase check could tell.
  (Verification runs: see the end.)
- Nothing else in the checks yet.

## What I would do, in order of value for cost

1. **A ledger check** (the one that answers finding 3). Every helper that makes a client send
   to b33p's account (presage-cli `send`, Desktop's and Android's drivers, the other b33ps in
   the mesh) records `(sender, timestamp, kind, expected effect)` in the part's `Checks`. At
   the end of every part: each ledger entry that is a data message with a body must be in
   b33p's content log (`B33P_CONTENT_LOG`, already written by `test-servers` builds), and each
   of those in b33p's window. The first half is cheap and has no side effects. The second
   needs `Action::Select` per conversation and `LoadOlder` for long ones, and selecting marks a
   conversation read and sends read receipts, which later checks depend on: so run it at the
   end of the box (after the last phase), or read the store through a new harness request
   instead of the window.
2. **Checks for the three unexercised effects** that need little: a message read on Android shows
   read on b33p (Android driver: open the chat), the phone's block list blocks the contact in b33p
   (Android driver: block), accepting a request on Android clears it on b33p. Each is one
   check beside the `storage` phase's.
3. **Mutation testing as a routine**, not a one-off: a list of silent no-ops, one per handler in
   `handle_content` and its callees, run nightly against the phases that should see them, with
   "survivors" (the no-ops nothing fails on) reported. M2 found three survivors from four handlers
   in one run; the list is about 30 handlers. A survivor is either a missing check or a dead
   handler.
4. **Content-kind coverage report at the end of a run** (a table of what each client's contents
   were, which `compare.md` already has for `--capture`), with "handled by b33p but never seen" as
   its last column, so that a new handler can't ship with no phase reaching it.
5. Keep the loud checks: M1 shows the "no unexpected error" check is the one that catches a drop
   that logs. Silent drops should not exist in b33p-core; a `debug_assert!`-style "every content has
   a disposition" at the end of `handle_content` in `test-servers` builds would turn them into
   the loud kind, but `handle_content` has 60-odd early `return true` paths and that is a large edit
   for the gain next to the ledger.

## Not done

- No ledger, window audit or new check, only the log line.
- M2's three survivors were found with Slint on a debug build; GPUI shares the receive path
  (b33p-core), so it applies to both.
- The list of unexercised kinds is from one capture run (E) and the M2 run; a kind that appears
  only in the stress phases or the tour would be missing from it.
