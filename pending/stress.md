# A stress run for b33p

Run b33p in ways the six phases never do (volume, interruptions, awkward content, concurrency,
long runtimes) and give every failure a root cause, whether it lies in b33p, presage,
libsignal-service, Slint, Qt or rusqlite. It reuses what the box has: the agent harness,
presage-cli, Desktop and Android as the other ends, the crash and lost-message checks, and the
record each phase leaves.

## Before it starts (once everything is on main)

- `cargo testbox run --rounds 2` on main, all green: what the stress run finds is then new, not
  inherited.
- Note main's revision and the four fork pins in the record, so a finding can be traced back.
- New phases in `xtask/src/checks/stress.rs`, one per group below, each on a fresh box like the
  others: `cargo testbox run stress-*`, and one finding doesn't hide the next.

## What counts as a finding

The crash and lost-message checks stay, and these join them:

- **Panics and errors in b33p's log:** any `panic`, `ERROR`, or warning from presage or
  libsignal-service, collected per check.
- **Database integrity:** `PRAGMA integrity_check` and the row counts of b33p's database after
  every forced stop.
- **State that agrees:** the harness snapshot after each step against what was sent: messages per
  chat, their order, edits, reactions. The snapshot goes into the record as JSON
  ([testbox-review.md](testbox-review.md)).
- **Resources:** b33p's RSS and open file descriptors, sampled every few seconds; steady growth
  fails.
- **Latency:** from send to shown, per message; recorded, not failed on.

## The groups

### A. Volume

- The peer sends 500 to 1000 short messages in a burst while b33p is up: order, nothing lost,
  and a harness snapshot still answering within a few seconds.
- The same burst while b33p is stopped, then b33p starts: the server's queue drained in one go.
- 50 conversations and 20 groups; scroll, search and open each through the harness.

### B. Interruptions

- `SIGKILL` b33p at chosen points: in a burst, in an attachment's download and upload, while
  linking, during a storage-service sync. Then start it again and run the integrity, state and
  lost-message checks.
- `podman pause` and `unpause` the server's container: a server that hangs rather than drops,
  also for longer than libsignal-service's keepalive.
- Restart the server's container during a send, and retry.
- A full disk: b33p's data on a small tmpfs. How sending and receiving fail, and that they
  recover once there is room again.

### C. Content b33p hasn't seen

- Texts: over 2 KB (a long-text attachment), very long single lines, right-to-left and mixed
  direction, combining marks and zero-width characters, long ZWJ emoji sequences, only
  whitespace, formatting ranges that overlap or reach the end.
- Attachments: zero bytes, close to the box's 100 MiB cap, odd filenames (Unicode, very long,
  path-like), a truncated or corrupt JPEG, PNG or GIF, an image of huge dimensions, an animated
  GIF with many frames, a wrong MIME type. These reach Qt's and Slint's image decoders, and part
  of the point is to find where they break.
- Edits: chains of 20 or more, a reaction to and a reply to the latest edit, a delete while an
  edit is on its way.

### D. Concurrency

- Desktop, Android and presage-cli sending to b33p at once while b33p sends back.
- Races on one message: a reaction, an edit and a delete close together, in different orders.
- Group changes (title, members, timer) from two clients at once while messages flow.

### E. Long runtime

- Hours of mixed, random traffic, from a seed the record keeps so a run can be repeated: memory
  and file descriptors that grow, reconnects that leak, a database that slows down.

### F. Environment

- A clock off in b33p's container, ahead and behind: disappearing timers, order, receipts.
- A memory limit on b33p's container.
- An ASan or valgrind build of b33p's image for groups B and C (Qt 6.4's `QRawFont` overrun is
  why).
- A CJK font, a non-English locale, scale factor 2.

## Order

1. C and A: cheap with presage-cli and the harness, and likely to find something.
2. B: needs stopping at a chosen point in the xtask, and the integrity check.
3. D and F.
4. E last, as a nightly run.

Each group is a phase of about ten minutes, except E.

## Triage

- Every failure keeps its record, its logs (before any wipe) and its seed: no intermittent
  failure without a cause.
- Narrow each finding to the smallest check that shows it, then place it: b33p, presage,
  libsignal-service, Slint or Qt, rusqlite.
- The fix goes upstream or onto the fork's `langis/integration`; the narrowed check stays in its
  phase.
- Findings are listed here, below, as [branches.md](branches.md) lists branches.

## Found out first

- Rootless `podman pause` works here (`core` used it already).
- presage-cli sends about ten texts a second, all in one `podman exec` (`Presage::send_all`), and
  attaches files from `/files`, which its container now mounts.
- The harness reports each message's delivery; a conversation's full history needs `LoadOlder`
  until nothing more comes (`all_texts`).

## Where it stands (26 October 2026, main 8ff6797)

Groups A to D are written: `cargo testbox run stress-volume stress-interrupt stress-content
stress-concurrency` (`xtask/src/checks/stress.rs`). They aren't among `run`'s default phases.
D has no group changes from two clients yet (it needs Android); E and F are still to do. `--release` (on `up` and `run`) builds b33p optimized, as it ships.

`stress-volume` passes: 200 messages in a burst arrive in order and once each, with b33p running
(the last shown 0.1 s after the peer's last send) and with it stopped (caught up within a
second of starting). Memory stays at 56 to 71 MB.

## Findings

### 1. A message acknowledged before it is stored is lost if b33p dies (libsignal-service)

`stress-interrupt`, "b33p killed while a burst arrives loses none of it", fails in every round:
b33p is killed three times during a 200-message burst, and afterwards one to three messages are
missing for good, each the one after a kill (042 after the kill at 40, 102 after the kill at
100). Nothing logs it: the lost-message check passes.

Cause: `MessagePipe::process_request` (libsignal-service `src/messagepipe.rs`) answers the
server's request with 200, which deletes the envelope from the server's queue, as it reads it off
the websocket, before presage has decrypted or stored it ("Envelopes yielded are acknowledged").
With the stream's channel of one, one or two envelopes are acknowledged and not yet stored at
any moment of a burst. Signal's apps acknowledge after processing.

Upstream: whisperfish/libsignal-service-rs#442 "feat: envelope ack handle" (rubdos, open since
June 2026) lets the consumer acknowledge after processing. Taken onto our libsignal-service
`langis/integration` (cf94b22), and presage's receive loop now holds the acknowledgement until
the envelope is handled and its message stored (our presage `langis/integration`, ef45a54).

With both, the check still loses one message in about one kill of three: one decrypted, and not
stored when b33p died. Its redelivery fails as a duplicate, the session having moved past its
counter (`DuplicatedMessage`), so it's gone (finding 1b). Those stored and not yet acknowledged
come again and are dropped as delivered again, as they should be. b33p took those for lost
messages too, since it remembers what it decrypted only in memory; it now asks the store
whether one is there first (`receive.rs`), so only a real loss is logged as one.

#### 1b. Decrypting and storing aren't one step (presage)

libsignal commits the session's new state as it decrypts (presage-store-sqlite's session store,
in a transaction of its own), and presage stores the message after, in another. A process that
dies in between has used up the message's key without keeping the message. The same happens
whenever the receiving task is dropped mid-envelope, as b33p's shutdown does
(`receiving.abort()`), so not only on a crash. Signal-Android decrypts and inserts the message
in one database transaction. presage would need a store transaction that the protocol store's
writes and `save_message` share, which its store traits don't have: a change to propose
upstream, not one to make on the fork alone. Until then the check keeps failing at this rate.

### 2. A message that arrives at the millisecond of one of our receipts is dropped (presage)

Same check, round 2: message 032 went missing before any kill, with presage's warning "not
saving a message in the place of someone else's".

presage keeps a chat's messages by `(ts, thread_id)` (`thread_messages`' primary key), and its
`send_message` stores whatever it sent there, our delivery receipts included, under the receipt's
own timestamp. b33p's store showed the row at 032's timestamp to be b33p's own delivery receipt
(sender b33p's account, device 2; `Content` field 5). When an incoming message carries the
millisecond one of our receipts went out at, presage's guard against overwriting someone else's
message drops the incoming one, and nothing says so. The other way round, an incoming receipt
can keep our own sent message from being stored.

In the box the peer and b33p share one clock and a receipt goes out every 75 ms or so, which
makes it likely; between real devices it is rarer but possible.

presage stores receipts on purpose (whisperfish/presage#409, so that apps can show who read
what), and the guard that drops the message is ours (#453, "only a message's sender deletes or
edits it"). Before it, the incoming message overwrote the receipt, but an incoming receipt
overwrote our own message just as well. Fixed on our presage `langis/integration` (7151260): a
receipt gives way. It isn't stored where a message is, and a message is stored where a receipt
is. It belongs with #453.

### 3. `down --wipe` didn't wipe a box another worktree had started (testbox, fixed)

`kube down` needs the YAML the pod was played from, which `up` writes in its own worktree's
`target/`. From another worktree `down` did nothing, and the next phase found the last one's b33p
store ("b33p already has a store"). `Pod::down` now also removes the pod and the
`b33p-testbox-*` volumes by name.

### 4. Pictures arriving together took b33p past 2 GiB (b33p, fixed)

`stress-content`: three PNGs of 11000×11000 px, 2.3 MB each as files, in one message. b33p's
memory peaked at 2149 MiB (the cgroup's `memory.peak`), from 470 MiB. Each passes
`decode_limited`'s bounds (12000 px a side, 512 MiB) and decodes to 484 MB, and the resize to
the bubble needs a buffer of its own; up to four download at once, and each was decoded as soon
as it arrived. Pictures are now decoded one at a time (`Downloads::decoding`), and a download's
slot is free again before its picture waits to be decoded.

### 5. A file with a long name saved once, not twice (b33p, fixed)

A name of 254 bytes saves, but the second copy, "name (1).txt", is 258 bytes, past what Linux
takes, and saving it failed ("failed to write"). The stem is now cut short, between
characters, so that the name with its number fits 255 bytes. Control characters in a sender's
file name (a line break saved as one) are now `_`.

### What Group C and D found right

- Every awkward text from the peer showed as sent: right to left and mixed, a direction
  override, 200 combining marks, zero-width characters, long emoji sequences, control
  characters, 20000 characters on one line, 500 lines, 30 KB, and whitespace alone. b33p sent
  each back as typed, less what ends it in whitespace (as Signal's apps), and `*bold*` and the
  like taken as formatting.
- Broken pictures (empty, truncated, a header claiming 100000 px a side, text or noise as a
  picture) showed as "This image can't be shown"; a PNG named .jpg and a 300-frame GIF showed.
  A snapshot took under 20 ms throughout.
- Odd names saved inside the downloads folder: `..\..\evil.txt` as `signal-attachment.bin`
  (a dot first), Unicode, a dash first, dots at the end dropped.
- presage-cli, Desktop and b33p sending into one chat at once: every message in order and once
  on b33p, and all of b33p's at the peer. A message edited, reacted to and deleted in quick
  succession ended deleted with no edit showing, with b33p running and with b33p stopped
  until all of it was queued.
- Two failures were the checks': b33p trims trailing whitespace and reads `*bold*` as
  formatting, and presage-cli's `send` receives what's queued without printing it, so the
  peer's messages are read from its store (`list-messages`) instead.

### Smaller notes

- Saving a 90 MiB file takes four to five minutes in the box: b33p runs as a debug build there.
  Worth a look in a release build before calling it slow.
- After a restart, an opened chat first shows only what arrived since, then its stored history:
  a check must wait for the list to settle (`settled`), which the first version of the
  killed-while-sending check didn't, and it wrongly reported sent messages as lost.
- presage logs `ERROR failed to register pre-keys, this is problematic and should never happen!`
  when the connection drops during its refresh at the start of a receive stream. The wording
  dates from when this ran once, right after registering (whisperfish/presage#222): failing
  then left the account without pre-keys, and a failure meant a bug, not the network. Since
  #343 and #366 it runs at the start of every receive stream, where a dropped connection is
  ordinary. Nothing is lost: the same drop ends the stream, b33p's reconnect starts the next,
  and that one refreshes the keys, while others can start sessions with the last-resort key
  meanwhile. Not worth a fix of its own; at most a warning for a transport error, should it
  bother a check.
- Messages b33p hadn't sent when it was killed vanished without a trace; Signal's apps show
  them as failed after a restart. b33p now does too: it keeps each send in its tables
  (`b33p_unsent`) from when it takes it until it went out, and at startup shows those still
  there as failed (`Event::Interrupted`), unless presage stored them as ours (sent, then killed
  before it was noted). Deleting one drops it (`Command::DropUnsent`). One that went out and
  wasn't stored yet shows as failed too: sent again, it goes at the same time, which the other
  side takes for the same message.
