# Container logs: k8s-file, not journald

The box plays its pod with `podman kube play --log-driver k8s-file` (xtask/src/pod.rs `play`).
Each container's output goes to a file that conmon writes as the lines come. Before this, it went
to journald, podman's default on this host. Everything that reads the logs uses `podman logs`, so
it works the same with either driver:
- a box's record (checks/record.rs `keep_logs`);
- the readiness waits (pod.rs `await_logs`);
- the lost-message and crash checks (checks/lost.rs, checks/crashed.rs).

What changes is when the lines get there.

## What went wrong with journald

In a tour round on a loaded host (run 17, 2026-10-27), journald took in Signal-Server's output
up to 41 s after the server wrote it:
- The ERROR line for an HTTP 500, logged at 16:07:20, reached the journal at 16:08:01.
- The box's record is taken once its checks end. Its copy of the server's log stopped at
  16:06:48, half a minute before the 500, with the cause of the failure missing from it.
- Only `journalctl --user` had the whole log, under the container's name.

journald can also drop lines. It rate-limits each service by default, and a Java server that
starts up and prints stack traces sends a lot at once. A record could therefore lose lines in
the middle of a log as well as at its end.

## Why k8s-file

conmon appends each line to the container's log file as it reads it, with no service in
between to fall behind or throttle. A record taken at a box's end therefore holds everything
written until then. In the tour round that confirmed the change, the record's server log ended
at 18:39:21, and the record was written at 18:39:22.

## What it costs

- The containers' output no longer appears in `journalctl --user`. Use `podman logs <container>`
  or `cargo testbox logs <service>`, or the box's record.
- A log file grows with its container and goes away with it. Boxes are played from nothing and
  removed (`down`), so that doesn't build up.

When reading a log on a loaded host, compare the timestamps the service writes with where its
lines sit in the log. With journald, those two could be a minute apart.
