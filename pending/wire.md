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

- 2026-10-29, `wire-verify` (main with #119, #120 and #121): `run --capture desktop attachments
  core android groups storage tour`. Every phase passed except one groups check, the invite
  link's approval: the Android driver's, root-caused and fixed in #121 (testbox finding 3).
  Before against after:

  | | before | after |
  |---|---|---|
  | b33p's websocket says `x-signal-receive-stories` | `false` | `true` (every b33p upgrade in all three boxes; presage-cli's stay `false`) |
  | a story Desktop posts reaches b33p | never (the new check fails on main) | in 0.8 s |
  | b33p's stories | `PUT /v1/messages/{aci}`, identified or sealed | `?story=true`, sealed, 200 (1 in the Android box, 3 in the tour) |
  | `GET /v1/certificate/auth/group` by b33p, Android box | 11 | 2 |
  | b33p's keepalive, median | 55 s | 30.0 s in every box |

## Findings in b33p and its forks

Each finding was checked against the code at the pins: presage e81ac0b, libsignal-service-rs
cddb0bc, Signal-Server ccc22f9a.

1. **Fixed** (presage #489, b33p #120; checked by #121). **Stories from Signal's own clients never reach b33p.**
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
2. **Fixed** (libsignal-service #493, presage #490, b33p #120). **b33p's stories go out as ordinary messages.**
   - Android and Desktop send `PUT /v1/messages/{aci}?story=true`. libsignal-service has no
     story parameter at all: `sender.rs` never mentions stories, and every send is a plain
     `PUT /v1/messages/{aci}`.
   - The server takes `story` from the query (Signal-Server MessageController.java:212). So
     b33p's stories are stored as non-story envelopes:
     - devices that opted out of stories still get them;
     - they count as urgent messages, so they trigger pushes.
   - Fix: libsignal-service sends with `story=true` when the content is a `StoryMessage`
     (upstream), then presage and b33p.
3. **Fixed** (presage #491, b33p #120). **b33p fetches its group credentials again for every group operation.**
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
5. **Fixed** (libsignal-service #496 and #497, presage #493, b33p #126). **b33p read a group whole on
   every change.**
   - In the groups phase: b33p 23 times `GET /v1/groups/` (the current state); never
     `/v2/groups/logs/{n}`.
   - Android reads `GET /v2/groups/logs/{n}?includeFirstState=…&maxSupportedChangeEpoch=…`
     9 times, `joined_at_version` 9 times and `/v2/groups/` once.
   - Now presage applies the change a message carries when its server signature checks out and
     it is the next revision, reads the log for a gap, and fetches the group whole only when
     none is stored or the log fails. b33p's own edits read the log first and apply the change
     the server returns; an admin's details read the log (requests to join), as Desktop does
     when a chat opens.
   - Changes no message told of come from presage as such, and b33p shows them as it shows the
     others. The groups phase checks two changes made while b33p is stopped.
   - 2026-10-29, `test --capture android groups storage`: b33p `GET /v1/groups/` once (after
     creating the group), `GET /v2/groups/logs/{n}` 10 times, all 200.
   - Left: a change shown twice if an admin's details read it from the log before the message
     telling of it arrives (b33p narrates every stored change message).
6. **Fixed** (libsignal-service #494, b33p #120). **The chat's keepalive is every 55 s; Signal's clients send one every 30 s.**
   - lss `KEEPALIVE_TIMEOUT_SECONDS = 55` (push_service/mod.rs:20). Android and Desktop:
     30.0 s median in both boxes. Android also sends WebSocket pings.
   - A NAT or proxy that drops idle connections after 30-60 s would cut b33p's chat off first.
   - Fix: 30 s (lss), unless a reason for 55 s turns up.
7. **Smaller differences, maybe intended:**
   - **Fixed** (presage #492, libsignal-service #495, b33p #124). b33p's REST requests sent no
     `Accept-Language`. Android and Desktop do (`en;q=1`, `en`), and the server uses it for
     localized text. b33p now sends `en`, its text's language, on every request and websocket
     upgrade (2026-10-29: all 42 and 81 of b33p's HTTP requests in the two boxes).
   - **Fixed** (the same). b33p and presage-cli said the same agent (`presage-rs-<version>`).
     b33p now says `b33p/<version> presage-rs`, and its CDN downloads, which said `b33p`, too.
   - b33p uploads its pre-keys with its identity key and last-resort key each time
     (`PUT /v2/keys`); Android and Desktop send only what changed.
   - b33p sends without `?story=false`. The server defaults to false, so this is harmless
     apart from finding 2.

## Findings in the testbox

1. **Desktop's `Devices/GetDevices` over gRPC gets a 404 from Jetty on 8443.** The 404 comes
   back three times in each box.
   - There is no remote config key to switch it to the websocket: libsignal 0.105 sends
     GetDevices over gRPC only (`require_grpc`).
   - Desktop's chat reaches Jetty, which serves no gRPC. Desktop calls it only to sync device
     names (onDeviceNameChangeSync).
   - Left open: pointing Desktop at the omnibus port would move its plain REST requests there
     too.
2. With the tap, the server sees every client as 127.0.0.2, the tap's address. Without it,
   every client already came from 127.0.0.1, so per-IP limits change nothing.

3. **The Android driver turned the invite link's approval on too early.** It failed twice in five
   runs. b33p showed "You turned the invite link on." without the approval, and Android made one
   group change, not two.
   - The driver tapped "Require admin approval" while the app was still turning the link on,
     and the tap was lost.
   - #121: it waits until the link's switch shows on, then until the approval's does.
   - Found with `b33p_shows`' error, which now names the open chat and its last messages (#121).
4. **The Android driver exited on `adb root` right after boot**, and the box waited an hour for
   it.
   - Right after boot the emulator's transport was `offline` for a moment, and `adb root`
     answered "closed".
   - #121: `adb root` is tried again once `adb wait-for-device` sees the device.
   - Left open: the xtask waits `START` (an hour) for a driver whose process has already exited.
     It could stop at the driver's exit line instead.
5. **Signal-Android crashed when the driver relaunched it right after a chat's menu closed**
   ("ConversationListFragment … is not currently in the FragmentManager", Compose disposing the
   menu after the fragment went). The storage phase failed so once; two earlier runs (other
   worktrees) logged the same crash. The driver's `launch()` cleared Signal's task each time.
   - #126: `launch()` leaves Signal be when it shows its chat list already. Then twice green.

## Still open, from these findings

- **Sender keys:** group messages one request per member (finding 4); libsignal-service work,
  large.
- **Fixed** (#125). **A story's sync transcript** set no `storyMessageRecipients`, and went out
  at the time it was sent, not the story's; Desktop files a sent story under its envelope's
  timestamp. Now it names every other member, allowed to reply, as Desktop's sendStory does for
  a group story, and goes at the story's time (2026-10-29: the transcript's `timestamp` is the
  `?story=true` send's, in the transcript's `timestamp=` notes the decoder now keeps for message
  sends). Still to see: what Android puts in its own transcripts (the content layer, with an
  Android driver that posts a story).

## The tool: open ends

- Unattributed connections remain:
  - `presage-rs`: unauthenticated sends and CDN traffic. Since b33p names itself, these are
    presage-cli's;
  - `unknown`: connections closed before a request.
- **Done** (#127): the content layer. b33p's `test-servers` builds write each content they take
  in (b33p-core content_log.rs); the record keeps them in `content/`, and compare.md's Contents
  section says, for each kind, which fields each sender filled in. What b33p sends shows where
  another b33p takes it in (the mesh box). 2026-10-29, first look: b33p's data messages carry
  the fields Desktop's do, but `isViewOnce` (Desktop says false). Not seen this way: b33p's own
  sync transcripts (the mesh's primaries are presage-cli's), and Android's story transcripts
  (no Android driver posts a story yet).
- A `wire` phase of symmetric scenarios, with an idle stretch, if the existing phases leave
  gaps. So far the keepalives and the background requests are visible enough without one.
