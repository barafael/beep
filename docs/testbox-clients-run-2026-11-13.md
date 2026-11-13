# Testbox with every Signal client: run notes, 2026-11-13

Working notes of the run, kept as it goes. Each anomaly has a root cause or is listed as open.

## The clients on the box

| client | stack | independent of libsignal-service? |
|---|---|---|
| b33p | presage fork + libsignal-service-rs fork | – |
| presage-cli | the same forks | no |
| gurk-rs | presage 94b3978, lss fb92b13 | no |
| Flare (flare-box on flare-backend) | presage 290effe, lss 7a75267 | no |
| sigwin, purple-presage | presage | no (not on the box: Windows / libpurple) |
| signal-cli | signal-service-java fork + libsignal-net | yes |
| mautrix-signal (signalmeow) | Go, libsignal FFI | yes |
| Signal-Desktop, Signal-Android | Signal's own | yes |
| Molly | Signal-Android fork at 8.19.2 | yes (older release of Signal's own) |

Not onboarded, with the reason: Whisperfish (Qt, no headless binary), siggy (its default backend is
signal-cli), iOS, abandoned projects.

## The mixed phase (`cargo testbox test mixed`)

b33p plus presage-cli, signal-cli, signalmeow and Flare's backend on one box; each client sends a
numbered burst to every other at the same time, and every receiver must hold each burst once, in order.

### Run 1 (4 clients): 21 passed, 4 failed

All four were the check's own mistake: presage-cli's `list-messages` lists newest first. Nothing was
lost or doubled. Fixed by reversing that listing.

### Run 2 (5 clients): 25 passed, 3 failed

All three were Flare's `flare-box send` exiting 134. The server and the wire showed nothing wrong: no
failed connection, no non-2xx but the expected first-send 409s.

Root cause: `flare-box` ended with `std::process::exit` from a task of its multi-thread runtime,
beside the threads still using the heap. glibc aborted one run in ~100 after the work was done
(`double free or corruption (fasttop)`, `malloc_consolidate(): invalid chunk size`). A burst stops at
its first failed send, so one abort at message 14 left b33p holding 14 of 20 after its restart.
Ending the process after the runtime (`shutdown_timeout`, then `exit`): 0 of 1,200 sends aborted (the
original: 3 of 360; at that rate about 10 would have). It is the driver's, not Flare's or b33p's.

### Run 3 (flare's driver fixed): 22 passed, 6 failed

A real loss: b33p logged `invalid Whisper message: decryption failed` for flare's first message of
round 2: `post-quantum ratchet error: Key already requested: 2` (SPQR 1.5.3 in libsignal 0.99, the
same on both sides). One session reset by b33p, and bursts from other peers missing one message each
afterwards. Not reproduced between presage-cli and Flare alone (8 rounds of crossing sends). Followed up
below with runs of one peer at a time (`TESTBOX_MIXED_ONLY`).

### Single-peer runs, 12 rounds each

**presage-cli only: 47 passed, 5 failed.** Four rounds, b33p's burst to presage-cli lacked one message at
the peer (r4 007, r8 004, r9 011, r11 005). Root cause, found: the store keys a message by
`(thread, timestamp)` (`presage-store-sqlite` migration `thread_messages`: `PRIMARY KEY (ts, thread_id)`),
and two messages of one conversation, one from each side, sent in the same millisecond, take one key.
b33p's own log has exactly four `not saving a message in the place of someone else's`
(presage `manager/keep.rs`), at the millisecond of each missing message (e.g. ts 1794538240615: the
peer's "b33p-too" message and b33p's 007), and the peer's store, with the same key, lacks b33p's.
b33p itself is covered: it keeps the content presage's store has no place for in its own
`b33p_collided` table (`b33p-core/src/local.rs`), and the warning is that guard working. The loss
is in the *peer's* store: presage-cli (and any presage client: gurk, Flare) keeps one of the two. Signal's own
definition of a message's identity is (author, sent time); Desktop and Android key by both. So the
check's "the peer holds every message b33p sent" fails through the peer's store, once in about 60
messages here because the bursts are tight; not flaky: a limit of the test client. Open: whether
the check should read b33p's side of the exchange for the peer's loss (b33p's own record of what it
sent is complete), and whether presage's store should take the author into its key
(`PRIMARY KEY (ts, thread_id, sender_service_id)`, a migration, lookups by author) for the other
clients' sake.

(The last check there, "b33p killed, started again", failed with "b33p shows 0 of 20" for round 1:
the check's own: `all_texts` stopped loading older pages once the count of *matching* messages stopped
growing, which twelve rounds of other bursts in front of round 1 do. It now counts every message
loaded.)

**flare only, four runs of 3 to 12 rounds; two causes, both in Flare's old libsignal-service
(7a75267), neither in b33p:**

1. *A session race inside one process.* b33p kept failing on flare's messages:
   `post-quantum ratchet error: Key already requested: N`, then `MAC verification failed` for 242
   messages after b33p's one reset (the reset is at most hourly per sender). The flare process that
   sent the first failing message received a live envelope from b33p in the same millisecond as its
   own send (capture: server push at +10 ms, PUT at +10 ms on one connection). That lss has no session
   lock (b33p's fork has one, #488): the decrypt and the encrypt each start from the stored session and
   the later write takes the earlier step back, so the sender's next message reuses a counter. With a
   global lock added to its `cipher.rs`/`sender.rs` (`testbox/family/lss-session-lock.patch`): 0
   decrypt failures in 12 rounds (before: 2 in 3).
2. *A message acknowledged and lost at exit.* With the lock, 7 of 12 rounds lacked one message of b33p's at
   Flare. Not the timestamp collision (b33p's send 1794541466642, the neighbour at 649). The wire: that
   envelope was pushed to a flare process 1 ms before the process closed, and acknowledged (200). Flare's
   lss acknowledges when it reads, before decrypting and storing; a one-shot process that ends then has
   lost it and the server has it as delivered. This is what b33p's fork's ack handle (#442) is for.
   Mitigation in the driver (not in Flare): the process stays until 400 ms after the last message the
   manager handed out, and a burst is sent by one process (`send-many`). Result: 52 of 52 checks, 12
   rounds, flare only.

### Run 4 (all five, 8 rounds, Flare mitigated): 41 passed, 5 failed

- b33p: no decrypt failure, no loss, no unexpected ERROR line. The two presage-cli losses (r5, r8) are
  the store collision again: one `not saving a message in the place of someone else's` in b33p's log at
  each (ts 1794544276514, 1794545198431).
- signalmeow, at first contact in round 1: 8 messages of signal-cli's burst failed with
  `failed to decrypt prekey message: 70: invalid Kyber prekey identifier`, and one of presage-cli's
  burst was written down twice. Probable cause (the mechanism is inferred, not seen; run 5 with `serve` tests it): each signalmeow command was a
  process of its own, ending a few hundred milliseconds after its work, and the first messages of a new
  session (pre-key messages) arrived live in those processes. One handled as the process ended spent its
  one-time Kyber key (the store deleted it) without saving the session; the sender, which has no
  acknowledgement of its session, goes on sending pre-key messages that name that key, and each fails;
  or, if the envelope was not acknowledged, it is delivered again and the driver's record, which is only
  appended to, has it twice. The same family as Flare's. The driver's `serve` mode (one process, connected
  all the time) is now what the mixed phase uses for signalmeow.

### Run 5 (all five, 8 rounds; signalmeow on `serve`, Flare mitigated): 47 passed, 0 failed

2,294 s. b33p: no decrypt failure, no loss, no unexpected ERROR. Each round, 2 x 20 messages from each of 4
clients to b33p, 20 from b33p to each, and 20 from each client to each of the other three (160 to b33p,
80 from it and 240 among the clients a round; 1,280, 640 and 1,920 over the eight), every one once and
in order. Three presage-cli store collisions told (r3 013, r3 020, r5 009), each matching one
`not saving a message in the place of someone else's` in b33p's log (3 of 3). No round slower than the
one before (the three bursts: 58.0, 66.2 and 144.2 s in every round from the second on; the first took
51.5, 59.6 and 126.6 s), so no slowdown with history within 8 rounds. The times are the slowest sender's:
signal-cli's one-shot `send` is a JVM start, about 1.5 s a message; b33p's own typing is not the limit.
Not measured here: b33p's memory (the earlier runs' finding 2, 2026-11-11, is untouched by this).

### The 24-round soak: 93 passed, 3 failed, 7,260 s (2 hours)

All five clients (signalmeow on `serve`, Flare's driver mitigated, no warm-up, no groups: the binary
before those), 24 rounds of the three bursts: 480 texts a round (80 to b33p, 80 more from the peers while b33p writes, 80 from b33p, 240 among the peers), 11,520 in all.
The three failures are explained: round 1's signalmeow first contact (below), which also fails "no
client lost a message" by its 8 decryption errors; and round 14, b33p's 004 to presage-cli lost to
the same-millisecond store collision (one guard warning in b33p's log; the binary did not yet tell
collisions). b33p: no decrypt failure, no unexpected ERROR line, "b33p killed, started again: it still
shows every burst, each in order, once" passed over all 24 rounds (450 s), and the growth check
passed. After each round (`RssAnon`): 81 MiB after round 1; 85 after 3; 91 after 5; 90 to 91 through
round 17; 113 to 115 from round 18 to 24 (steps, no slope: +10 MiB at rounds 3 to 5, +23 at 18);
46 open files all the way; the harness's snapshot answered in 2.6 ms after round 1 and 32 ms after
round 24, about 1.2 ms for each round's 480 texts stored (a harness snapshot, which reads the open chat's
loaded messages and the conversation list; whether the app's own list loads slow down the same way is not
measured). The earlier soak's finding 2 (memory growth, 0.7 MiB a minute) is not seen at this rate of
messages: about 1.6 texts a second here, flat apart from two steps.

### The first contact of signal-cli and signalmeow (not b33p; not root-caused)

When signalmeow and signal-cli make first contact with each other with both bursts at once (round 1
of the "every client writes to every other" scenario), signalmeow cannot decrypt signal-cli's
messages from the 13th on: `failed to decrypt prekey message: 70: invalid Kyber prekey identifier`,
8 messages (13 to 20) in each box that showed it, 2.1 s apart (signal-cli's pace). Seen in 4 of 8 fresh boxes
without a warm-up (run 4, the 24-round soak, the third group run, one of three signalmeow-only boxes;
run 5, the second group run and two of the three signalmeow-only boxes passed); signal-cli's resends (retry receipts, `content_hint=Resendable`)
sometimes bring them later and out of order. With each pair writing once, in turn, before the bursts
(`TESTBOX_MIXED_WARM`, on by default): 0 of 3 signalmeow-and-signal-cli boxes, and every full run since.
What was tried and did not hold: a lock on signalmeow's decryption, as its sends have
(`encryptionLock`; sending.go takes it, receiving.go's `decryptEnvelope` does not): 1 of 3 boxes
still failed, so the patch is not kept. Not looked at: signal-cli's own log of what it sent
(its store is the JVM's), and why the key is the same id (70) in every box. The count 8 is
constant, which a race would not give.

### Groups

The group scenarios (b33p makes a group of all four clients; each writes a burst to it at once; b33p
writes a burst) passed in the full run: 41 of 41 over 3 rounds, with the clients holding each other's
bursts once and in order (each client's own account of what it received; a sender process that
also received counts: presage-cli seemed to lack signal-cli's first group message of each
burst, which its store held, because a `send-to-group` process printed it and the check dropped that
output). A group's id is a master key for presage-cli and signalmeow, a base64 id for
signal-cli and a hex id for Flare.

## Files, reactions and edits in the mix

Each round now also sends **files** (every client to b33p and to every other client at once, and b33p
to each: 3 KB to 150 KB, incompressible, off the padding's edges), and **reactions and edits** (b33p
reacts to and edits a message of each client; the clients that can react to b33p's message and edit
their own; all of it under a burst of texts from every client). `mixed/amend.rs`;
`TESTBOX_MIXED_FILES=0` and `TESTBOX_MIXED_AMEND=0` leave them out.

What each client can do (the check holds each to what it can):

| | sends files | tells what it received of a file | sends reactions | sends edits | receives reactions and edits |
|---|---|---|---|---|---|
| b33p | yes | saved file's SHA-256 (read in its container) | yes | yes | yes |
| signal-cli | yes | the saved attachment's hash | yes (`sendReaction`) | yes (`--edit-timestamp`) | yes (JSON) |
| signalmeow | yes | hash printed by the driver | yes (driver `react`, new) | yes (driver `edit`, new) | yes |
| Flare | yes (`send-file`, new) | hash printed (new) | yes (`react`, new) | **no**: see below | yes |
| presage-cli | yes (`--attach`) | no: its attachment directory is deleted at exit | no | no | yes (prints the text) |

**Flare's backend cannot send an edit.** `flare-types/src/convert/to_presage.rs:32` converts only a text,
a reaction and a deletion to presage's content; any other message is `_ => todo!()`, and the process
panicked there ("not yet implemented", then "no echo of the send in 60 s"). Flare receives edits. Not
b33p's; the driver has no `edit` command for it.

### What the runs found

- **My own harness, twice.** The `serve` daemon forwarded only text events to the check, so a file's
  hash, an edit and a reaction signalmeow had received were never seen (the first file check failed
  with "never received": the message was in the driver's store with `files:1`). Fixed in the driver
  (the message event carries the reaction) and in the reader. And the daemon's lines are now typed
  (xtask's clippy forbids `serde_json::Value`; CI would have failed the branch).
- **signal-cli sends a reaction's emoji as U+FFFD.** The image's locale is POSIX, so the JVM decodes its
  arguments as ASCII (`sun.jnu.encoding = ANSI_X3.4-1968`) and `sendReaction -e 👍` goes out as four
  replacement characters. b33p showed exactly that, correctly. Any text with a non-ASCII character given
  to `signal-cli send` on that image had the same fate; no earlier check sent one. `LC_ALL=C.utf8` in the
  image.
- **A loaded machine.** Run 4 (4 rounds, burst 20, `--capture`) had a window, 11:13 to 11:25, in which
  b33p logged `database is locked` (SQLite code 5; `failed to unarchive a conversation` four times,
  `command failed … failed to keep the chat's change` once, a delivery receipt not sent) and Flare's own
  store failed a send with `database error: failed to send the message to a contact`. At that time
  another build on this machine (a `cargo` of an unrelated project) wrote about 260 MB/s: host load
  average above 100 on 32 cores, tasks stalled on IO 47% of the five minutes (`/proc/pressure/io`,
  btrfs endio workers busy). b33p opens its database with `synchronous=FULL` and a 10 s busy timeout
  (`b33p-core/src/signal/lifecycle.rs`), presage's store 30 s; an fsync stalled longer than that fails
  whoever waits. The failures: the first check of round 3 (message 020 of presage-cli's burst not shown
  within 120 s: it was shown later, the restart check found it), a Flare `send-many` that died at its
  twelfth text (the restart check lists exactly 012 to 020 as missing: never sent), and the
  unexpected-error check. Nothing sent to b33p was lost; its screen was minutes late. The phase now
  prints the host's load and IO stall after each round (`host after round N`).
  - What it shows of b33p: a write that fails on a busy database is not retried. The
    unarchive-on-incoming-message is logged and dropped (the conversation stays archived), and a user's
    command fails with an error. Only reachable with a disk that stalls for ten seconds.

### Run 5 (4 rounds, burst 20, `--capture`): 51 passed, 4 failed

Three failures in the group check of rounds 1, 3 and 4, and "no client lost a message". Host load was
7.8 after round 1 (IO stalled 7.8% of the minute), 2.4 after round 2, 21 after round 3 (14.7%).

- **Round 1: presage-cli "lacked" Flare's group message 003.** It did not: presage-cli's store lists all
  twenty (`list-messages -k <master key>`, 003 at 1794571456960). A one-shot presage-cli process can store a
  message and end before printing it, and the group check read only what was printed. The 1:1 checks
  read the store, which is why they never showed it. The harness's: the group checks now read the store
  for presage-cli and Flare (`held_in_group`).
- **Round 3: `presage-cli send-to-group` exited 101,** `Store(Db(Database(SqliteError { code: 5, message:
  "database is locked" })))`, after trivial statements (`INSERT INTO sender_keys`, `DELETE FROM
  decrypted_contents`) had taken 1.5 to 2.6 s each (sqlx's slow-statement warnings in its output). The
  disk was stalled (above), the same window as the ones in run 4.
- **Round 4: Flare lacked presage-cli's group message 019**: not printed and not in its store
  (`flare-box messages <group>`: 19 of 20; round 1 has 19 of presage-cli's and 19 of signal-cli's, too).
  And the end check had one lost message: `flare-box send-group … error opening envelope, message will
  be skipped! error=Sealed sender decryption error: … database is locked`. Flare's libsignal-service
  acknowledges an envelope when it reads it, before it is decrypted and stored (see above), so one it
  could not store, or had read as the process ended, is the server's as delivered and Flare's never. With
  the disk stalling, the 400 ms the driver stays after its last message is not long enough. Flare's, and
  already known; b33p's fork acknowledges after storing.
- **Reporting.** `together` reported only the first failing client, so round 1's other failures
  (Flare's) were hidden behind presage-cli's. It now reports all.

### Run 6 (same settings, quiet host): 54 passed, 1 failed, 2,219 s

Host load 3 to 7 and IO stalled under 0.4% of every minute. Everything passed (4 rounds of bursts, the
group, files, reactions and edits) but one check, after b33p was killed and started again:

> b33p's "testbox mixed: r3 target of b33p to flare, edited" to flare carries the reactions [], not [🎉]

**A real gap in b33p, root-caused.** Asked directly (the harness socket, after the run), b33p showed
Flare's 🎉 on that message in rounds 1, 2 and 4, and none in round 3, though the round's own check, minutes
before, had seen it. b33p's content log has all four reactions as received, each naming its message by
author and time correctly (the round 3 one: `targetSentTimestamp` 1794576231472, which is that message).
It also has this:

    received dc5ecd31… 1794576239017  a DataMessage with a reaction (🎉)
    received dc5ecd31… 1794576239017  a DataMessage "testbox mixed: r3 flare to b33p-amid 006"

Two contents of one sender, in one thread, at one millisecond. Flare's driver runs a process a command,
and the reaction (`flare-box react`) and the burst (`flare-box send-many`) ran at once, each stamping with
its own clock. presage keeps one content per thread and millisecond (`thread_messages`' key) and refuses
a message only at a time **someone else's** holds (`presage/src/manager/keep.rs`: `existing.metadata.sender
!= stored.metadata.sender`); the same sender's second content is saved in the first's place (that is
how a redelivery is idempotent). b33p logged both as `Stored`, showed both (the reaction from memory), and
the store kept the later one: the reaction was lost to the next load. b33p's own guard for a collision
(`keep_if_dropped`, the `b33p_collided` table) runs after presage has saved and only asks whether the
holder is someone else's, so it never sees a displaced content of the same sender.

How likely: it needs one account to send two contents to one chat in the same millisecond: two
devices of one person (a phone and a desktop), or a script like this. The reaction and the message take
different roles in Signal's own clients (a reaction is not a message there, and has no slot of its own
to lose); here a reaction is a stored content in the message's key space. A text displaced by a text is the
same loss but an app can't tell two messages of one author and one millisecond apart anyway (Signal names
a message by author and time).

Two ways to close it, neither done here (they change the presage fork's pin or add state):
1. In presage's `keep`, refuse to replace a content of the same sender that is a different content (compare
   the bodies; a redelivery is equal) as it already refuses someone else's, and let `keep_if_dropped` keep
   the refused one beside the store when the holder differs in content, not only in sender.
2. In b33p alone: a small bounded record of the contents just received (thread, time, sender, content),
   and the earlier one kept beside the store when a different content of the same key arrives.

The testbox's check stays as it is: it fails when this happens, with the message and the reactions
b33p shows, and the content log has the two contents.

### The wire, for the files (run 4's capture, 620,216 frames, 4,216 connections, none undecoded)

Every client uploaded 16 files and downloaded 16 (4 rounds, 4 files each), through the CDN on 8446.

- **Four clients upload with three requests; signalmeow with one.** b33p, Flare, presage-cli and
  signal-cli: `POST /upload/attachments` (tus, `Upload-Defer-Length`, empty body, 201), then
  `HEAD /upload/attachments/{id}` (200), then `PATCH` with the body (204). signalmeow: one
  `POST /upload/attachments` with `Upload-Length` and the body in it (tus "creation-with-upload"), 201.
  The server accepts both. Median request times in the capture: b33p's POST, HEAD and PATCH took 15, 2 and
  54 ms on a quiet box. Over a real link the two extra round trips per attachment are what b33p could save
  by uploading in the one request. The `HEAD` is lss's (the `libsignal-service-java` client, signal-cli,
  does the same), so it is the reference client's shape, and not removed here; worth testing against
  the real CDN before changing it.
- signal-cli asks for the upload form over gRPC (`Attachments/GetUploadForm`, 16); the others over
  `GET /v4/attachments/form/upload?uploadLength=…` (16 each).

## What the other clients say about b33p's behaviours

Each held up in this run against a client without it, so it is shown needed, or not shown unneeded:

- **The session lock (lss #488)** and **acknowledging after storing (#442)**: Flare's libsignal-service
  has neither, and the two causes above are their absence: a counter reused, and a message the
  server has as delivered that the client never stored. Both reproduced in about one message in sixty
  and went away with the lock, and with the process outliving its last message.
- **`b33p_collided`**: nine messages in three runs (4, 2 and 3) were lost by presage-cli's store to a
  same-millisecond collision; b33p lost none (its log shows the guard at each).
- **Shown by the mix, in b33p**: the store's slot for a reaction (above, run 6): it is the one gap in b33p
  found that a client without b33p's guards could not show.
- **Not shown either way**: the 30 s keepalive (the other clients' connections last under 8 s, so
  nothing here says whether it is needed; the Desktop and Android captures can), the picture
  metadata stripping (the mix sends files byte for byte; b33p strips only pictures'), the tus upload shape (now compared: see "The wire, for the files").
- **Removable or simplifiable**: nothing found in this run. Nothing was removed to see whether the
  testbox noticed; the earlier mutation run (`docs/testbox-sceptical-run-2026-11-11-mutations`) is the
  method for that.

## Wire observations (run 2, `--capture`)

- **The 409 on `PUT /v1/messages/{aci}`** is each sender's first send to an account that has a linked
  device: the sender knew device 1 only, the answer lists `missingDevices`, the sender retries. Once per
  sender and recipient; presage-cli, flare, signal-cli all do it. b33p never sees one in the
  sends of this run (its device list is current).
- **signal-cli pulls the whole backlog on every command.** 27,945 envelopes pushed to signal-cli over
  250 connections, 446 acknowledged. A one-shot `signal-cli send` opens the receive websocket, the server
  pushes the first 256 queued envelopes (237 KB; mostly delivery receipts, `type=5`), and the command
  closes 0.3 s later without acknowledging any. The backlog drains only in `receive`. This is signal-cli's
  one-shot mode, not the server; the daemon (JSON-RPC) keeps one connection. Load runs should use the daemon.
- **Connections that carried no byte** (214 of 1,695). 208 are signal-cli's: a connection to
  `[::1]:8445` ahead of each working one to `127.0.0.1:8445`, 300 ms before it. The omnibus (HTTP/2 and
  gRPC) port listens on IPv4 only (`/dev/tcp/::1/8445` is refused inside the server container; 8443 and
  8444 answer on both), and libsignal-net tries `localhost`'s IPv6 address first, so every signal-cli
  command pays 300 ms. Test-box listener, not a client bug; b33p does not use 8445. The other 6 are on
  8444, hellos that never finished their handshake; they sit next to signalmeow and presage-cli
  connections (Go's dialer races both address families and drops the loser; not confirmed).
- **Keepalive**: b33p alone sends `GET /v1/keepalive` (every 30.0 s, median). No other client sends a
  keepalive or a WebSocket ping; their connections last under 8 s. Whether b33p needs it is for the
  review below.
- **Message body**: b33p and the presage clients send `destination`; signalmeow leaves it out and sets
  `urgent`; both are accepted.
- **b33p alone** asks `GET /v1/storage/*` (storage service, 8448) and the provisioning websocket; the
  presage clients do neither.
- **Not decoded**: one signalmeow websocket frame (90,112 bytes of a frame cut short) and one signal-cli
  record (`Received` did not open). Cause not yet looked at (open).

## Open

- The 214 byte-less connections.
- The unexplained one-time EC pre-key absence in b33p's `GET /v2/keys/{aci}/*` answer (b33p got a
  response shape without it where gurk and presage-cli got one).
- signal-cli's NPE after a link.
- Molly: reaction label, the "Deleting is now synced" dialog, the timer check (`docs/onboarding-molly.md`).
- A reaction displaced from presage's store by another content of the same sender and millisecond (run 6):
  two ways to close it, neither done.
- Not in the mix yet: Desktop and Android as peers, gurk, a seeded random mix, a signal-cli daemon, groups'
  reactions and edits, file sizes beyond 150 KB, latency and throughput kept as numbers.
- Flare sends no edits (`todo!()`), and acknowledges before it stores; the group and 1:1 checks tolerate
  neither beyond what the store shows. A pins test for `testbox/family/*.lock` (flare-box gained two
  dependencies in the lock this session).
- The upload in one request (signalmeow's) against the three of the others, tried against the real CDN.

## Soaks

(Filled in as they finish.)
