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

## Soaks

(Filled in as they finish.)
