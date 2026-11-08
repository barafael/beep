# Extended testbox runs, 2026-11-08

Five runs of `cargo testbox` on `origin/main` at 31083b5, on a box of its own
(`--instance ext`), one after the other, from 02:28 to 04:11. Every check passed in every run:
no failures, no retries, no unexpected ERROR lines in b33p's log, no client lost a message or
crashed. The other sessions' boxes (`names`, `sigwin`) were running beside this one, so the
timings below are for a shared machine (load average 1.4 to 6.1 at the start of a run).

| # | Command | Result | Wall time |
|---|---|---|---|
| 1 | `run --rounds 2` (all default phases, Slint window, debug build) | 2 of 2 rounds passed | 36 min |
| 2 | `run --ui gpui` (all default phases, GPUI window) | 1 of 1 rounds passed | 18 min |
| 3 | `test stress-volume stress-interrupt stress-content stress-concurrency` | 61 of 61 checks passed | 16 min |
| 4 | `test tour` (3 b33ps, seed 1794105513, each step twice) | 79 of 79 passed | 2.6 min |
| 5 | `TESTBOX_SOAK_MINUTES=30 test stress-soak` (seed 1794105677) | 12 of 12 passed | 31 min |

## Default phases, checks passed and seconds per phase

| Phase | Checks | Slint round 1 | Slint round 2 | GPUI |
|---|---|---|---|---|
| desktop | 33 | 47 | 46 | 45 |
| attachments | 13 | 12 | 15 | 13 |
| network | 16 | 288 | 312 | 317 |
| core | 8 | 121 | 124 | 102 |
| android | 18 | 51 | 49 | 51 |
| groups | 30 | 97 | 97 | 98 |
| storage | 14 | 73 | 72 | 47 |
| large-group | 11 | 28 | 26 | 28 |

The two windows pass the same 143 checks with the same phase times, within a few seconds (the
`network` phase dominates, since it waits for impaired links; `storage` and `core` are about
20 to 40% faster with GPUI in this one run, which is one sample and not shown to be real).

## Stress phases

| Phase | Checks | Time |
|---|---|---|
| stress-volume | 13 | 75 s |
| stress-interrupt | 15 | 506 s |
| stress-content | 16 | 84 s |
| stress-concurrency | 17 | 163 s |

- Volume: the peer sent 200 messages in 29 s; b33p showed the last one 83 ms later; memory went
  from 94 MB to 109 MB during the burst and back to 90 MB after catching up.
- Interrupt: 200 messages sent while b33p is stopped reach it once it starts; b33p killed
  while a burst arrives loses none of it.
- Concurrency: presage-cli, Desktop and b33p sending at once each get the others' messages in
  order, once; an edit, a reaction and a delete close together leave the message deleted.

## Tour

Three b33ps, every feature twice in a drawn order. A message took 0.3 to 1.5 s (27 of them) to show on another
b33p. Each b33p ended at 52 to 67 MiB of memory (from 34 to 35 MiB at the start; the page cache
18 to 24 MiB) and 51 to 56 open files (from 47 to 49).

## Soak, 30 minutes of mixed traffic

Memory 35 MiB at the start, 68 MiB at the end (page cache 24 to 33 MiB), 47 to 48 open files
throughout; the check "nothing lost, and nothing keeps growing" passed. The sampled memory moved
between 48 and 56 MiB in the last minutes, so the end point is one sample above that range; the
check's own threshold is in `xtask/src/checks/stress.rs`. Not examined here: whether the growth
from 35 to 52 MiB over the first minutes is the history filling or a leak. A longer soak with
messages-received subtracted would tell (Q7 in `performance-open-questions.md`).

## Not covered

These ran in a debug build with Slint's Qt window and with GPUI under Mesa's software Vulkan, so
they check behaviour against Signal-Server's test mode, not performance or the non-Qt Slint
builds. `--optimized`, `--capture`, `--b33p-memory` and `--fresh` were not used.

Logs and records: `target/ext-runs/` and `target/testbox-ext-record/` in the worktree
`/home/rafael/code/beep-testbox-ext`; they are not committed.
