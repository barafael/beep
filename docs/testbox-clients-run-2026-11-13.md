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
Signal's own definition of a message's identity is (author, sent time); Desktop and Android key by
both. So with presage's store a message is silently lost when both sides send in one millisecond:
rare for people, once in about 60 messages here because the bursts are tight. Not flaky: a gap.
Fix needs the author in the key (`PRIMARY KEY (ts, thread_id, sender_service_id)`, a migration, and the
store's lookups by thread and time taking the author), in presage's store, for edits, deletes,
reactions and receipts, which all name an author and a time already.

(The last check there, "b33p killed, started again", failed with "b33p shows 0 of 20" for round 1:
b33p's chat shows its newest page after a restart, and twelve rounds are more than that; to be
checked against the check's own paging before it counts.)

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
