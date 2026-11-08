# Testbox correctness runs, 2026-11-08 (after the dependency update)

Both windows, one after the other, on `origin/main` at 5635e27 (the merge of #155, the
dependency update), on a box of its own (`--instance correct`), in a debug build:

| Window | Command | Start | Wall time | Checks | Result |
|---|---|---|---|---|---|
| Slint | `cargo testbox run --ui slint` | 15:23 | 21 min | 170 passed, 1 failed | failed |
| GPUI | `cargo testbox run --ui gpui` | 15:44 | 19 min | 168 passed, 3 failed | failed |

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
- logged nothing after that: no `synced with Signal`, no error, no panic;
- kept showing "Catching up on your messages…" to the agent harness for the check's 120 s;
- ignored the SIGTERM of the pod's teardown at 15:56:33 (no `asked to quit signal=15`, which
  `b33p_gpui::shutdown` logs on every other stop in this record) until podman's 10 s
  timeout killed it at 15:56:43.

The next two checks, "b33p reconnects after the server side drops" and "a message sent while
the server doesn't answer fails, and goes out when sent again", failed on that same b33p
(`agent.sock`: broken pipe) and are consequences of it, not failures of their own.

A process that answers neither the harness nor SIGTERM points at GPUI's main thread blocked;
the root cause is being looked for. The same check passed with GPUI in the extended runs this
morning (`testbox-extended-runs.md`) and with Slint here.

Record: `target/testbox-correct-record/run/1794149039/1/linked/` in the worktree
`/home/rafael/code/beep-deps`.

### Slint, storage: Signal-Android's nickname screen

"a nickname given on Android titles the chat in b33p" failed before b33p had a part in it:
the on-device agent on the emulator refused `set_nickname` after 33 s. Signal-Android opened
`NicknameActivity` at 14:42:27 and the agent did nothing on it until it closed at 14:42:59.
The same check passed in the GPUI run, 20 minutes later, on the same images. The root cause is
being looked for in the Android driver (`testbox/tools/src/bin/android.rs`).

Record: `target/testbox-correct-record/run/1794147788/1/android/` (`logcat.txt`) in the same
worktree.
