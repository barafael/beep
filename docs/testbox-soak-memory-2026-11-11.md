# Where the soak's memory growth comes from (finding 2 of the sceptical run, 2026-11-11)

Finding 2: b33p's own memory (`RssAnon`) grew from 70 to 113 MiB over the 45-minute soak, about
0.7 MiB a minute and not levelling off, and the check meant to notice (`growing`) would not trip for
hours. Question: retained history, allocator fragmentation, or a leak?

## Short answer

**Not the history and not fragmentation: about a quarter of the memory of a long-running b33p is
live heap that a freshly started b33p with the same history does not have.** Whether that is a leak
or an unbounded cache is not decided; nothing in b33p has been found to own it yet.

## Experiments (instrumented b33p: `docs/testbox-soak-memory-2026-11-11-probes.patch`, scratch only)

The patch makes b33p-slint's memory logger (`b33p-slint/src/memory.rs`) write glibc's `mallinfo2`
split (arena, live ("in use"), free chunks, mmapped) beside `RssAnon` every 20 s, and makes the soak
(`TESTBOX_SOAK_RESTART_PROBE=1`) end by killing b33p, starting it again on the same data, opening the
soak's chat and loading all its history, sampling the memory at each step.

### 1. A 30-minute soak (seed 4243, 1,563 messages received), allocator split

`RssAnon` 43 MiB at the start, 100 MiB at the end; the live heap ("in use") 44 to 105 MiB
(maximum 113), free chunks 15 to 32 MiB of a 59 to 137 MiB arena, mmapped about 0.3 MiB, 78 to 85
threads. Growth rate in minutes: 0.71 MiB a minute over the whole run and **0.28 in the second half**
(it slows: most of the rise, about 45 MiB, is in the first 7 minutes, then 0.2 to 0.3 a minute).
So it is live objects (the free list is a quarter of the arena), not fragmentation.

### 2. A 20-minute soak, then a restart with the same history (clean run, 12 of 12 checks)

| moment | `RssAnon` | live heap | free chunks | arena |
|---|---|---|---|---|
| at the soak's start | 69 MiB* | | | |
| the soak's end (1,151 messages received, chat open, long-running) | **93 MiB** | 105 MiB | 24 MiB | 130 MiB |
| restarted, chat open (the latest page of history) | **61 MiB** | 73 MiB | 15 MiB | 88 MiB |
| restarted, all the history loaded (`LoadOlder` until nothing more) | **67 MiB** | 77 MiB | 18 MiB | 95 MiB |

\* the harness's first sample, after setup.

Reading: loading **all** the history costs 6 MiB (61 to 67) on a fresh b33p. The same history in the
b33p that received it live costs 26 MiB more (93 against 67), and the live heap differs by the
same amount (105 against 77, a difference of 28 MiB). So roughly **26 to 28 MiB, for about 2,000
messages (received and sent), is retained by the running process beyond what the history needs**:
about 13 KiB a message, against 235 bytes of content per message in the content log.

### 3. The check's blindness, from the same data

The `growing` check compares the first third's median with the last third's: it fails only at +50%
*and* +64 MiB. This soak: 78 to 99 MiB, nowhere near. A growth of 0.3 MiB a minute (the second half's
rate) takes about 3.5 hours to reach +64 MiB over a start of 70.

## What this rules in and out

- Ruled out: history held in memory (6 MiB, not 26), allocator fragmentation (free chunks are a
  fifth of the arena and the live heap itself differs), page cache (not in `RssAnon`), the open-file
  count (46 to 48 throughout).
- Open: which structure holds the 26 MiB. Candidates to look at first, none checked: the state the
  window keeps per message besides the text (reaction, receipt and delivery maps, `Delivery`
  tables, quote and link-preview caches), what the receive loop remembers (`Decrypted`, the
  envelope memory "for a while", `receive.rs`), the avatar and image caches, the agent harness
  of a test build (snapshots, subscribed events: a release build does not run it, so the testbox may
  be measuring something users never have; a soak against a build without it would tell),
  presage's own state in the process (session caches, the group credentials cache).
- The next experiment that settles it: run the soak under a heap profiler (heaptrack, or dhat-rs in
  an instrumented build) and compare allocation sites still live at the end against the same after a
  restart with history loaded; or bisect by turning the candidates off in turn (harness, avatar
  fetching, receipts). Not done.

## A failure on the way, with its cause

The first rerun of the 20-minute soak failed ("no client lost a message": two presage-cli
"message with old counter" failures, and presage-cli's picture upload to CDN3 "operation timed out").
Cause (evidence kept in `target/testbox-mem-record/stress-soak-io-stall-evidence`): the host's disk
was saturated by another run of mine building images on the same disk at the same time. Wrangler's
own log shows the uploads taking 8, 31 and 18 seconds (`POST /upload/attachments 201 Created
(8014ms)`, `PATCH … (30707ms)`, `POST … (18477ms)`), presage-cli's database logged `DELETE FROM
decrypted_contents` taking 1.4 s and 4.0 s, and `/proc/pressure/io` showed `full avg60=13%` at the end
(`load.txt`). The timeout made presage-cli's send go twice (the "old counter" duplicates), which the
lost-message check counts. Rerun alone, the same seed and parameters passed. So: host I/O contention
from my own concurrent runs, not b33p; but it shows that **a testbox run is exposed to the host's disk
and the box has no I/O isolation** (the speed and isolation report left per-pod resource limits
undone; CPU and memory limits would not cover this anyway, `io.weight` or `io.max` on the pods'
cgroups would).
