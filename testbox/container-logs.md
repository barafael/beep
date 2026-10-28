# Container logs: k8s-file, not journald

The box plays its pod with `podman kube play --log-driver k8s-file` (xtask/src/pod.rs `play`).
Each container's output goes to a file that conmon writes as the lines come. Before this, it went
to journald, podman's default on this host. Everything that reads the logs uses `podman logs`, so
it works the same with either driver:
- a box's record (checks/record.rs `keep_logs`);
- the readiness waits (pod.rs `await_logs`);
- the lost-message and crash checks (checks/lost.rs, checks/crashed.rs).

What differs is how late the lines arrive. Measured, journald is the smaller part of that.

## What prompted it

In a tour round on a loaded host (run 17, 2026-10-27), Signal-Server's ERROR line for an HTTP
500 carried the server's timestamp 16:07:20 and reached the journal at 16:08:01, 41 s later. The
box's record, taken when the checks ended, held the server's log only up to 16:06:48, without the
failure's cause. That 41 s was first put down to journald. The measurements below show that most
of it wasn't.

## Measured

The time from a line's own timestamp to the moment the driver took it, on this 32-core host with
NVMe storage.

**Experiment A: the drivers alone.** A container writes 3000 lines, each stamped with its own
clock (`date +%s%N`).

| Load | journald p50 / p95 / max | k8s-file p50 / p95 / max |
|---|---|---|
| idle | 0.1 / 0.3 / 6.6 ms | 0.1 / 0.2 / 1.7 ms |
| the host's own load | 0.1 / 0.1 / 4.6 ms | 0.1 / 0.1 / 0.4 ms |
| 48 busy CPU loops | 0.6 / 2.8 / 16 ms | 0.3 / 1.9 / 10 ms |
| 8 writers doing fsync on the journal's disk | 285 / 884 / 939 ms | 0.1 / 1.0 / 7.6 ms |

journald falls behind only under heavy disk writes, by up to about a second. k8s-file stays in
milliseconds. journald never reported suppressing lines; its rate limit didn't come into it.

**Experiment B: Signal-Server in a tour round**, under the same disk-write load. This compares the
server's own Dropwizard timestamp with the driver's.

| Driver | p50 | p95 | max |
|---|---|---|---|
| journald | 20 ms | 1135 ms | 1211 ms |
| k8s-file | 1.7 ms | 915 ms | 935 ms |

With k8s-file too, the server's lines arrive up to about a second after their timestamps. So most
of the delay is inside the JVM, not in the driver. Dropwizard wraps every appender in an
asynchronous queue (256 entries), which drains late when the host is loaded, and drops INFO and
DEBUG lines once it is 80 % full. journald adds a little on top.

Neither experiment comes near 41 s, and that delay is still unexplained. Run 17 also had image
builds and three b33ps running on the host.

## Why k8s-file stays

- It is never the slower driver, and under disk load it is by far the faster one.
- File-based logs are the common setup: Docker's default `json-file`, and Kubernetes' CRI log
  files.
- It needs nothing of journald's.

It doesn't solve the record missing lines, though. Lines still sitting in a service's own queue
when the record is taken are lost with either driver. So the record stops Signal-Server before it
takes the logs, and starts it again after (checks/record.rs `keep_logs`). On SIGTERM, Jetty stops,
and Dropwizard's `ServerCommand` then stops its logging, which waits for the async appenders to
drain (`DefaultLoggingFactory.stop`, `flushAppender`, in dropwizard 5.0.2, the version the server
runs). The other services log straight to their output.

The record also keeps the host's load average and pressure and each container's `cpu.stat`
(`load.txt`), so a late line can be put down to a loaded host, or not.

## What it costs

- The containers' output no longer appears in `journalctl --user`. Use `podman logs <container>`
  or `cargo testbox logs <service>`, or the box's record.
- A log file grows with its container and goes away with it. Boxes are played from nothing and
  removed (`down`), so that doesn't build up.

When reading a log on a loaded host, compare the timestamps the service writes with where its
lines sit in the log. A Java service's lines can be a second or more behind their own
timestamps, whatever the driver.
