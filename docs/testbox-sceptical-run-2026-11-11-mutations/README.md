# Artifacts of the sceptical testbox run of 2026-11-11

Used against 806c673 in throwaway worktrees (`git worktree add --detach ../x 806c673`, then
`git apply` the patch):

- `m1-drop-and-log.patch`: b33p drops everything from another account whose timestamp is even
  and logs an ERROR (negative control; `cargo testbox test desktop core`). It sits before the match
  on the body, so receipts, typing and stories go as well as data messages. What drops depends on
  the parity of wall-clock timestamps, so two runs differ; if the peer's first message in the
  setup is an even one, the required "text from the peer reaches b33p" fails and no phase runs.
- `m2-silent-handlers.patch`: `on_read_elsewhere`, `on_blocked_elsewhere`, `on_answered_elsewhere`
  and `on_typing` made no-ops, silently (`cargo testbox test desktop android groups storage`).
  Note `on_typing` returns `bool` and `false` stops b33p's session: the "control" killed the
  session, not only the handler.

The runs' output and scripts were in a scratchpad that a reboot cleared; the per-box records
(each container's log, Android's logcat, the wire of the `--capture` run) were in the worktrees'
`target/testbox-*-record/` (`../beep-sceptic`, `../beep-sceptic-gpui`, `../beep-sceptic-mut`) and
are gone since (2026-11-18: only stale sockets are left there).
