# Wire: what Signal's own clients say to the server, next to what b33p says

`cargo testbox test --capture` relays every TLS connection in the box through the `tap`
(testbox/tools `tap`), and xtask/src/wire decodes it. The output goes in the box record's `wire/`:
`events.jsonl`, a transcript for each client, and `compare.md`. The work is on branch `wire-tap`
(worktree `~/code/beep-wire`).

This file is kept as the runs go. Newest entries go at the end of each section.

## Runs

- 2026-10-29, `wire-tap` c348a1a on main 49ca0dd: `test --capture desktop`, 29/29 passed. 44
  connections and 320 events, all decoded.
- 2026-10-29, the same: `test --capture android groups storage`, 60/60 passed. 85 connections
  and 1008 events, all decoded. The clients seen:
  - Signal-Android 8.32.1 (libsignal 0.105.0);
  - Signal-Desktop 44.7.2 (libsignal 0.105.0);
  - b33p and presage-cli (`presage-rs-0.8.0-dev`).

## Findings in b33p and its forks

Each finding was checked against the code at the pins: presage e81ac0b, libsignal-service-rs
cddb0bc, Signal-Server ccc22f9a.

1. **Stories from Signal's own clients never reach b33p.**
   - What the wire shows:
     - b33p's websocket upgrade says `X-Signal-Receive-Stories: false`; Android and Desktop say
       `true`.
     - presage hard-codes it in `identified_websocket` (presage/src/manager/registered.rs:300).
   - What the server does: it acknowledges a story envelope for such a connection without
     delivering it (Signal-Server `WebSocketConnection.sendMessage`, WebSocketConnection.java:204).
     The envelope is gone for good.
   - Yet b33p shows stories (b33p-core `stories::on_story`), and the tour's story steps pass.
     That is only because b33p sends its own stories unmarked (finding 2): b33p to b33p never
     sets the flag that the server filters by. A story from Android or Desktop goes out with
     `?story=true` and is dropped for b33p.
   - Fix:
     - presage: a setting for the header, or `true`, which is Signal's own clients' value.
     - b33p: say `true`.
     - A check: a story Android posts shows in b33p. None of the phases has one; the tour's
       stories are b33p's.
2. **b33p's stories go out as ordinary messages.**
   - Android and Desktop send `PUT /v1/messages/{aci}?story=true`. libsignal-service has no
     story parameter at all: `sender.rs` never mentions stories, and every send is a plain
     `PUT /v1/messages/{aci}`.
   - The server takes `story` from the query (Signal-Server MessageController.java:212). So
     b33p's stories are stored as non-story envelopes:
     - devices that opted out of stories still get them;
     - they count as urgent messages, so they trigger pushes.
   - Fix: libsignal-service sends with `story=true` when the content is a `StoryMessage`
     (upstream), then presage and b33p.
3. **b33p fetches its group credentials again for every group operation.**
   - In the groups phase: `GET /v1/certificate/auth/group` b33p 11 times, Android once,
     Desktop once.
   - presage makes a new `GroupsManager` with an empty `InMemoryCredentialsCache` on each call
     (presage/src/manager/registered.rs:698-713, called at :615, :718, :1137, :1611, …).
   - The credentials cover a 7-day redemption window.
   - Fix: keep the cache in the manager's state, shared by its clones (presage).
4. **Group messages go to each member in turn; Signal's clients send one multi-recipient
   message.**
   - Desktop sends `PUT /v1/messages/multi_recipient`, Android the gRPC
     `MessagesAnonymous/SendMultiRecipientMessage`. Both use sender keys: one encryption, one
     request.
   - libsignal-service's `send_message_to_group` loops over the recipients
     (sender.rs:473-477). Each one costs a request, and the content is encrypted once per
     device.
   - Not wrong, but slower, larger groups suffer most, and the server sees one request per
     member.
   - Fix: sender keys in libsignal-service. libsignal has them (`group_encrypt`,
     `sealed_sender_multi_recipient_encrypt`). Large.
5. **b33p reads a group whole on every change.**
   - In the groups phase: b33p 23 times `GET /v1/groups/` (the current state); never
     `/v2/groups/logs/{n}`.
   - Android reads `GET /v2/groups/logs/{n}?includeFirstState=…&maxSupportedChangeEpoch=…`
     9 times, `joined_at_version` 9 times and `/v2/groups/` once.
   - libsignal-service reads groups only through `/v1/groups/` (push_service/mod.rs:300).
   - Fix (efficiency, later): read the changes since the known version, as Android does.
6. **The chat's keepalive is every 55 s; Signal's clients send one every 30 s.**
   - lss `KEEPALIVE_TIMEOUT_SECONDS = 55` (push_service/mod.rs:20). Android and Desktop:
     30.0 s median in both boxes. Android also sends WebSocket pings.
   - A NAT or proxy that drops idle connections after 30-60 s would cut b33p's chat off first.
   - Fix: 30 s (lss), unless a reason for 55 s turns up.
7. **Smaller differences, maybe intended:**
   - b33p's REST requests send no `Accept-Language`. Android and Desktop do (`en;q=1`, `en`),
     and the server uses it for localized text.
   - b33p and presage-cli say the same agent (`presage-rs-<version>`). Something that names
     b33p would help the server's view, and this tool's attribution.
   - b33p uploads its pre-keys with its identity key and last-resort key each time
     (`PUT /v2/keys`); Android and Desktop send only what changed.
   - b33p sends without `?story=false`. The server defaults to false, so this is harmless
     apart from finding 2.

## Findings in the testbox

1. **Desktop's `Devices/GetDevices` over gRPC gets a 404 from Jetty on 8443.** The 404 comes
   back three times in each box. `provision` turns gRPC off for seven Desktop requests
   (`desktop.libsignalNet.grpc.*`, provision.rs:41-63), but not for this one. Desktop
   apparently goes on regardless. Fix: add it to the list, or point Desktop's gRPC at the
   omnibus port.
2. With the tap, the server sees every client as 127.0.0.2, the tap's address. Without it,
   every client already came from 127.0.0.1, so per-IP limits change nothing.

## The tool: open ends

- Unattributed connections remain:
  - `presage-rs`: unauthenticated sends and CDN traffic, which could be b33p or presage-cli;
  - `unknown`: connections closed before a request.
  Telling b33p and presage-cli apart there needs either their own agents or a b33p-only box.
- The content layer (plan M4): what each client puts in `Content`, seen where b33p decrypts it.
- A `wire` phase of symmetric scenarios, with an idle stretch, if the existing phases leave
  gaps. So far the keepalives and the background requests are visible enough without one.
