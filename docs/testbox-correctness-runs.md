# Testbox correctness runs, 2026-11-08 (after the dependency update)

Both windows, one after the other, on `origin/main` at 5635e27 (the merge of #155, the
dependency update), on a box of its own (`--instance correct`), in a debug build:

| Window | Command | Start | Wall time | Checks | Result |
|---|---|---|---|---|---|
| Slint | `cargo testbox run --ui slint` | 15:23 | 21 min | 170 passed, 1 failed | failed |
| GPUI | `cargo testbox run --ui gpui` | 15:44 | 19 min | 168 passed, 3 failed (1 cause) | failed |

No other box ran beside them (load average 1.1 at the start, 3.3 during GPUI's core phase).

## Checks per phase, passed/failed, and seconds

| Phase | Slint | GPUI |
|---|---|---|
| setup, linked box | 12/0, 134 | 12/0, 134 |
| desktop | 33/0, 44 | 33/0, 45 |
| attachments | 13/0, 14 | 13/0, 13 |
| network | 16/0, 317 | 16/0, 316 |
| core | 8/0, 121 | **5/3**, 132 |
| setup, android box | 16/0, 134 | 16/0, 134 |
| android | 18/0, 51 | 18/0, 50 |
| groups | 30/0, 96 | 30/0, 97 |
| storage | **13/1**, 76 | 14/0, 64 |
| large-group | 11/0, 26 | 11/0, 26 |

## The failures

### GPUI, core: b33p hangs after starting again with a message waiting

The check "text the peer sends while b33p is down arrives when it starts again" kills b33p's
container (SIGKILL, 15:54:20.18), has the peer send a text, and starts b33p again (the pod's
restart policy had already started it, 15:54:20.49). The new b33p:

- logged `connected to Signal; syncing` at 15:54:22.560, then decrypted the waiting message
  (`decrypted Whisper message … with current session state`, 15:54:22.574);
- never logged `synced with Signal` (the queue's end) in the 2 minutes after, nor an error or
  a panic;
- kept showing "Catching up on your messages…" to the agent harness for the check's 120 s;
- didn't end on the SIGTERM of the box's teardown (15:56:33): podman killed it after its 10 s
  timeout (15:56:43). On the other teardowns of both runs b33p ended within 0.3 s.

The next two checks, "b33p reconnects after the server side drops" and "a message sent while
the server doesn't answer fails, and goes out when sent again", failed on that same b33p
(`agent.sock`: broken pipe) and are consequences of it, not failures of their own.

What this rules in and out so far:

- b33p-core's receiver hands each received item to the session and waits until it is handled
  before taking the next (`b33p-core/src/signal/receive.rs`, `handled.await`); the session
  takes commands before received items (`signal/session.rs`, `biased`). The sync never
  reaching its end means handling the first item, or a command taken before it, never
  finished. GPUI quits only once the backend has closed the database
  (`b33p-gpui/src/shutdown.rs`), so a session stuck that way also explains the ignored SIGTERM.
- Not a store timeout: the stores' pools give up after 30 s at most (`busy_timeout`, sqlx's
  acquire timeout) and would have logged it.
- Not the shared `STORAGE_NAMES` lock (`signal/conversations.rs`): no guard outlives an await.

The root cause is not found yet. The same check passed with GPUI in the extended runs this
morning (`testbox-extended-runs.md`) and with Slint here.

Looking for it again (2026-11-08 and -09, `origin/main` with `presage=debug` in b33p's
`RUST_LOG`), with a watcher that takes the stacks of b33p's threads
(`podman unshare gdb -p <b33p's host pid>`, which resolves symbols in the debug build) once a
sync has gone 60 s without ending:

- `cargo testbox test core --ui gpui`, 21 rounds (9 on a quiet machine, then 12 more, the
  first 3 of them beside a second box): 8/0 every time, the watcher never fired.
- `cargo testbox run --ui gpui --rounds 20`, 7 rounds and part of an 8th before it was stopped
  for the failures below: core 8/0 every time, the watcher never fired.

What the logs narrow it to: on a restart that passes, `synced with Signal` follows the decrypt
of the waiting message within 30 ms. The hung b33p decrypted it and never got there, so it
stopped between the decrypt and the end of handling that first item: in presage's
`handle_content`/`save_message` (stores, profile key) or in b33p's handling. Next: the full
run again, with the watcher, until it hangs.

Record: `target/testbox-correct-record/run/1794149039/1/linked/` in the worktree
`/home/rafael/code/beep-deps`, which a cleanup by another session deleted on 2026-11-09 with
every record in it.

### Slint, storage: Signal-Android's nickname screen

"a nickname given on Android titles the chat in b33p" failed before b33p had a part in it:
the on-device agent on the emulator refused `set_nickname` after 33 s. A bug in the agent, not
in b33p or Signal-Android.

- Where: in typing the name (`fill` → `SetText`, `testbox/tools/src/bin/android.rs`), after
  the "First name" field was found and tapped. The logcat's times fit: Signal-Android focused
  the field at 14:42:27.117, the agent's tap showed the keyboard again at 27.70, and the next
  check's `am start` came at 14:42:59.72, which is 27.70 plus SetText's 30 s (`PATIENCE`) plus
  that check's 2 s look for "New chat". Save never ran, and from 27.7 on the app drew only the
  cursor blinking.
- Why: after `ACTION_SET_TEXT`, `awaitFocusedField`
  (`testbox/android/agent/…/Agent.kt:342`) waits for the field to read the name. It looks
  again only when an accessibility event comes, and it looks through UiAutomation's cache
  (`findFocus` without `clearCache()`), which the agent's other looks clear because the cache
  shows stale screens (`Agent.kt:426`). Once the text is set the screen is quiet, so the field
  is looked at again only if some unrelated event comes. In the GPUI run one came after
  about 16 s (from the tap at 15:01:43.2 to Save at 15:02:00.6, though Save is enabled as soon
  as there is text), so that run passed. In this run none came within 30 s.
- Not proven: whether the stale cache, the missing event, or both kept the field from being
  read. The agent's own message (`no focused field as wanted: …` with the screen) was dropped,
  because the driver answers with `error.to_string()` (`android.rs:2076`) where it logs the
  whole report (`android.rs:2061`).
- Fix (`android-agent-settext`): clear the cache before `findFocus`, look again at least
  every 500 ms even when no event comes, and send the agent's whole message back
  (`testbox_tools::Report`). With it, the storage phase passed 3 of 3 rounds (14/0 each), with
  the nickname check taking 6.7 s each time, against 16 s in the GPUI run that passed without it.

Record: `target/testbox-correct-record/run/1794147788/1/android/` (`logcat.txt`) in the same
worktree.

## Found in the full GPUI runs, 2026-11-09

`cargo testbox run --ui gpui --rounds 20` on `origin/main` (1e6423b, with #157), 10:14 to 12:20:
7 rounds and part of an 8th (stopped in its attachments phase) for these two failures. Record:
`target/testbox-correct-record/run/1794215681/` in `/home/rafael/code/beep-deps`, deleted since
(see above).

Run again with `--capture` from the worktree `/home/rafael/code/beep-hunt` (be05835, with #160),
19:20 to 21:05, 2 rounds and part of a 3rd before a benchmark needed the machine: the Android
box's setup passed both (17/0), the network check failed in round 1 and passed in round 2, core
passed both (8/0), the watcher never fired. Record:
`target/testbox-correct-record/run/1794252007/` in that worktree.

### Android box, setup: "Android registers" (3 of 7 rounds), a regression from #157, fixed

The agent answered `no focused field as wanted` on Signal-Android's "Create your PIN" screen.
The driver types the verification code with `Echo::Anything`, and the code submits itself: once
it is set, Signal moves on to "Create your PIN", which has no field with input focus. Since #157
the echo wait reads the focused field fresh, so it timed out whenever Signal had moved on before
the first look. Before #157 it passed because UiAutomation's cache still handed back the code
field. #160 skips the echo wait for `Echo::Anything` (its only use is that code); with it,
`cargo testbox test android --ui gpui` passed 3 of 3 (registering in 15 s). The agent's message
reaching the driver whole is #157's other half at work.

### Network: "a picture from b33p reaches Desktop over it" (4 of 7 rounds, then 1 of 2): a CDN connection closing under the upload

Desktop waited the check's 120 s for the picture. b33p never sent it: in exactly the failing
rounds (1, 4, 5, 7, and round 1 of the run with `--capture`) b33p logged one
`failed to upload “…”: … client error (SendRequest): connection closed before message completed`,
and in no passing round. The request was the upload's first, the tus create
(`POST /upload/attachments`): the CDN3 log has one create fewer in the failing rounds, the
picture's.

Why, from the capture of round 1 of the run with `--capture` (`wire/connections.jsonl`,
`wire/events.jsonl`, and the packets of `wire/raw/capture.pcap`), connection 80, b33p's to CDN3
(port 8446), times UTC:

- 19:24:24.060 to 24.104: b33p uploads an earlier file on it (create, `HEAD`, `PATCH`, all
  answered); 24.146 its last ACK. Then nothing.
- 29.276: the CDN side sends a 24-byte TLS record (a close_notify) and at 29.307 its FIN,
  5.17 s after the connection went idle.
- b33p had already written the picture's create on the same connection: the 508-byte request
  carries ACK 56069, the sequence before the close_notify, so b33p sent it before the close
  reached it. Through netem's 150 ± 40 ms it shows at 29.395, and again, retransmitted, at
  29.584. Nothing answers it; hyper then reads the close and fails the request with
  `IncompleteMessage`, which it does not retry once the request was written.

Who closes: every connection to CDN3, from b33p, Desktop or presage, ends 5.0 to 5.3 s after its
last answer. `front` (testbox/tools `front.rs`) has no timer of its own and closes a client's
connection only when wrangler closes its side, so wrangler closes connections idle for about 5 s
(the default keep-alive timeout of Node's HTTP server, which `wrangler dev` likely uses; not
checked further).

Why so often: b33p's HTTP client keeps idle connections far longer than 5 s, and the checks'
steady pace puts the picture's upload about 5 s after the file before it, at the edge of the
timeout; netem's delay holds the close back from b33p for about 150 ms, so a request written in
that time goes out on a connection already closed. The network phase alone never failed (12
rounds, and 6 with the attachments phase before it): there the uploads come at other times.

Not a fault of the test box: a real CDN closes idle connections as well, and on a slow link the
same race loses an upload. Signal-Android's HTTP client (OkHttp) tries a request again when a
kept-alive connection fails under it; b33p didn't.

Fix (`upload-retry-stale-connection`): b33p uploads an attachment once more, on a new
connection, when the first try failed with hyper's `IncompleteMessage`
(`signal::errors::closed_under_request`); the request never got an answer, so the second try is
safe. A test serves a request and closes without an answer, as the CDN did.
