# Messages at one millisecond: what is still open

As of 4 November 2026, on the uncommitted work that adds `b33p_collided`, with presage at
`53fb55a` (now `3de451c`: its save guard and the store's `(ts, thread_id)` key are
unchanged). The history and the part that is fixed are in `pending/stress.md`, finding 8.

**None of this has been reproduced.** Every finding comes from reading the code, so re-derive one
before acting on it. The hints name symbols and log strings rather than line numbers, as the lines
move.

Set this first in bash or zsh; the commands below use it:

```sh
P=$(ls -d ~/.cargo/git/checkouts/presage-*/3de451c)   # the rev in Cargo.toml
```

## Why any of this exists

presage keeps one content per thread and millisecond, and no author is in the key. b33p's list goes
by timestamp and sender. The two disagree when two messages share a millisecond.

```sh
git -C $P grep -n "PRIMARY KEY (ts, thread_id)"                 # the key
git -C $P grep -n "in the place of someone else"                # the guard that drops the second
git -C $P grep -n "INSERT OR REPLACE INTO thread_messages" -- '*.rs'   # same author: last wins
grep -n "fn message_key" -A3 b33p-core/src/state/history.rs     # the list's key
gh pr view 67 -R whisperfish/presage --comments                 # why: "an assumption we can make"
```

Don't just put the author in the key: deleting for everyone and edits rely on a save replacing what
is at that time (`registered.rs`, the `existing_msg.metadata.sender` and `edit.metadata.client_timestamp`
assignments).

## In the product

### 1. One account, two devices: the message read is replaced after a restart

Two messages from one account at one millisecond, A then B. The list keeps A and never shows B; the
store keeps only B. On the next restart B replaces A, a message the user read for one they never
saw. Edits, reactions and deletes aimed at that time then apply to B.

Signal addresses a message by author and time, so these two can't both be addressed, and keeping one
is defensible (Signal-Android does). What is wrong is keeping a different one in each place.

b33p can't mend this alone: by the time it hears of B, A is gone from the store. It takes presage
keeping the first, without stopping edits, which reuse the original's time on purpose.

Re-derive:

- The list: `insert` in `b33p-core/src/state/history.rs` returns `None` for a key it has.
- The store: in `save_message` in `$P/presage/src/manager/registered.rs`, the guard only stops a
  different sender, so the same sender falls through to `store.save_message`, which replaces.
- The guard lives in the manager, not the store. A test that calls `SqliteStore::save_message`
  shows the replacing but goes past the guard.
- `same_moment` in the stress check allows this on purpose: it wants exactly one of each set shown,
  either one, so it passes through the swap.

### 2. A delete-for-me from another device leaves a kept message in place

The user deletes a message on another device, for themselves only. If that message is in
`b33p_collided`, b33p keeps it, on disk and shown after a restart. It should go, as deleted content
is not meant to stay on disk.

The other person's message is safe. An earlier note here said the delete would remove it; it can't,
because presage finds the thread by sender and time, and finds nothing for a kept message. Read
alone, the `delete_message(&thread, timestamp)` call looks as if it would.

Re-derive:

```sh
grep -n "SyncContent::DeleteForMe" -A36 $P/presage/src/manager/registered.rs     # presage's handling
grep -n "async fn thread_for_sender_and_timestamp" -A10 \
    $P/presage-store-sqlite/src/content.rs                  # WHERE sender_service_id = ? AND ts = ?
grep -rn "DeleteForMe\|message_deletes" b33p-core/src       # nothing: b33p doesn't handle these
```

The fix goes in `handle_content` in `b33p-core/src/signal/receive.rs`: for each message a
`DeleteForMe` names, call `forget_collided`. Note that b33p has no handling of these at all, so the
list doesn't update live for any deleted-elsewhere message; that is worth settling at the same time.

### 3. `on_missed_changes` saves past the guard

A missed group change is saved with `manager.store().save_message` directly, at a time made from
now (`now - (count - n)`). If a content already holds that millisecond in that group, it is
replaced, whoever wrote it, with nothing to say so.

Rare: it takes a message at exactly that made-up millisecond.

Re-derive: `grep -n "fn on_missed_changes" -A45 b33p-core/src/signal/groups.rs`. For other writes
past the guard, `grep -rn "\.save_message" b33p-core/src --include='*.rs'` and
`git -C $P grep -n "store.save_message\|\.save_message(&Thread" -- presage/src`, leaving out the
tests. `import.rs` is one such write, and it checks first.

The fix: `store.message` first, as `store` in `b33p-core/src/import.rs` does, and if someone else
holds the time, move on a millisecond (the time is ours to choose here).

## In the concurrency check

All three are in `xtask/src/checks/stress.rs`.

### 4. A collision on a sender's last text fails as a timeout

The check waits for each sender's last text (`for last in [last(&from_cli)?, last(&from_desktop)?]`)
before it looks for sets (`let together = same_moment`). If `...from Desktop 015` shares a
millisecond with another message of that account and arrives second, it never shows (finding 1), so
the wait runs out and the check fails with no word of why. This is the false alarm `same_moment` was
written to remove.

Re-derive: read those two lines in `concurrency`, then `insert` as in finding 1.

The fix: wait on `settled`, then find the sets, and want the last text only if it isn't in one.

### 5. "Reach the peer as sent" proves only a long text's first 2000 bytes

`received.contains(b33p_core::format::inline_head(text))`, because presage-cli prints the body and
not the attachment. A broken `LongText` attachment still passes.

The attachment path is covered by `long_text` in the desktop and android phases, but that text is
plain numbered ASCII. So the awkward ones here (`awkward_texts`: 20000 of `x`, 30 KB, long emoji
sequences) aren't shown to arrive whole anywhere. That is a gap in coverage, not a known bug.

Re-derive:

```sh
grep -n "fn awkward_texts" -A45 xtask/src/checks/stress.rs
grep -n "long_text" xtask/src/checks/*.rs
```

The fix: send one or two of these to Desktop as well, or rename the check to say "inline part".

### 6. `moments_in` fails on a line b33p is still writing

`note` in `b33p-core/src/content_log.rs` does `writeln!(file, "{line}")` on an unbuffered `File`
with a `serde_json::Value`, which writes in pieces. b33p logs every content it takes in, late
receipts and typing too. A read between two pieces gets half a line, and `serde_json::from_str(line)?`
in `moments_in` fails the whole check.

A narrow window, not seen so far.

Re-derive: read `note`, then `moments_in`. To confirm the pieces, run `strace -e write` on a small
program doing the same `writeln!`.

The fix, either: build the line in a `String` and write it at once, or have `moments_in` ignore a
last line with no newline.

## Not yet shown

### 7. The fix has not met a real collision

The `stress-concurrency` run of 4 November passed 17 of 17 with the restart step, but nothing
collided in it: the guard's warning isn't in the record. The two unit tests
(`a_message_the_store_has_no_place_for_is_kept_beside_it`,
`importing_a_message_at_someone_elses_time_keeps_both` in `signal/lifecycle.rs`) call the keeping
directly. So `keep_if_dropped` from `handle_content`, and from `keep_sent_if_dropped` in `send.rs`,
hasn't run on the real thing.

Re-derive, and exercise:

```sh
cargo testbox run --rounds 5 --optimized stress-concurrency
grep -rl "in the place of someone else" target/testbox-record/
```

Each hit is a round worth reading. Search that round's `b33p-testbox-b33p.log` for the warning, then
check the check passed after the restart. A set of one account's prints
`reached b33p from the peer's account at one millisecond` in the phase's output.

Collisions were seen on 2 November under `--optimized --b33p-memory 768`, so try that first. In the
box every client shares one clock and sends every 75 ms or so.
