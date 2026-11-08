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

The root cause is not found yet. Next: run `cargo testbox test core --ui gpui` again until it
hangs, and take the stacks of b33p's threads then (not done yet because a benchmark is running
on the machine). The same check passed with GPUI in the extended runs this morning
(`testbox-extended-runs.md`) and with Slint here.

Record: `target/testbox-correct-record/run/1794149039/1/linked/` in the worktree
`/home/rafael/code/beep-deps`.

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
