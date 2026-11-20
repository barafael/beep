# A sceptical, far-reaching testbox run, 2026-11-11

`main` at 806c673 ("testbox: Skia's binaries for the offline build"), run from 00:49 to 03:07
(CET) on this machine, in debug builds unless noted. The question was not "does it pass" but
"would a pass mean anything": so besides the default phases in both windows there are
different configurations (`--fresh`, `--optimized`, `--capture`, `--b33p-memory`), the stress
phases, three tours, a 45-minute soak, two negative controls (the box made to fail on purpose),
and a reading of every container's log and the decoded wire afterwards.

## Verdict

- **Every check of every ordinary run passed**: about 1,660 checks in the 11 commands that were not
  controls, no failure, no retry, no unexplained ERROR line in b33p's log. There was no failure to
  root-cause.
- **One real defect, found before any box started: `b33p-gpui` with the `harness` feature does
  not build on `main`** at 806c673, so `--ui gpui` cannot run on it (finding 1). CI is red on it
  already (see the correction in finding 1); a local `just check` does not run that step. (Fixed
  on `main` since, by 5afec5a on 2026-11-15.)
- **The pass/fail signal does detect what it says**, with one scope limit worth knowing (finding 3).
  A b33p that dropped half of what it received failed 16 checks.
- **Not shown to be fine**: b33p's memory grows about 0.7 MiB a minute for 45 minutes of mixed
  traffic and does not level off, and the check that should notice would not trip for hours
  (finding 2).
- Everything else below is an observation with its evidence, and says whether its cause is found.

## Follow-ups (2026-11-11, after this report)

Each is uncommitted in a worktree of its own beside this repository; nothing was pushed. (As of
2026-11-18 that still holds, except that finding 1's patch is on `main` as 5afec5a; the `just test`
step and the clippy runs with the feature are not.)

- Finding 1, **fixed**: `../beep-fixes` (the patch, the same step in `just test`, clippy with the
  feature in `just lint` and CI; also an ERROR line when b33p's backend stops without saying why,
  found by a second control, M2, in the investigation of finding 3). Verified: GPUI `desktop` and
  `core`, Slint `core`, 127 GPUI e2e tests, clippy, fmt.
- Finding 2, **half answered**: [testbox-soak-memory-2026-11-11.md](testbox-soak-memory-2026-11-11.md). The history
  costs about 6 MiB; a running b33p holds about 26 MiB more than a fresh one with the same history,
  in live heap, not fragmentation. The owner of that memory is not found.
- Finding 3, **investigated**: [testbox-silent-loss-2026-11-11.md](testbox-silent-loss-2026-11-11.md). Three handlers
  nothing checks (read on the phone, blocked on the phone, a request answered on the phone), and a
  list of what to add.
- Speed and isolation: [testbox-speed-isolation-2026-11-11.md](testbox-speed-isolation-2026-11-11.md): a round from 18.7 to 13.7 minutes, 8.7 with `--jobs 2`; `--fresh` from 36 to 9 minutes with
  `--jobs 4`.
- Finding 8, **partly fixed**: [storage-manifest-2026-11-11.md](storage-manifest-2026-11-11.md). The unversioned
  whole-manifest calls are gone (15 to 2 in a run of the `storage` phase alone), but the manifests
  downloaded in all fell only from 25 to 22: b33p still downloads each new manifest twice;
  the rest needs a change in presage (the `FetchLatest` branch of its receive loop, a patch is
  written), which needs a commit to your fork.
- A rerun of a soak failed once on the host's disk (last section of the memory report): the testbox
  has no I/O isolation and my own concurrent runs caused it.

## What ran

Boxes of my own (`--instance sc1`, `sc2`, `sc3`), in worktrees pinned to 806c673, beside another
session's 20-round GPUI hunt on `correct` (`../beep-hunt`) and, early on, a machine under heavy
load that was not mine (load average 35 over the 15 minutes before 00:48, 3 to 13 during the
runs). Timings are for a shared machine.

| # | Command | Start–end | Result |
|---|---|---|---|
| A | `run --rounds 2` (Slint) | 00:49–01:27 | 2 of 2 rounds passed, 342 checks |
| B | `run --ui gpui --rounds 2` (patched, finding 1) | 01:00–01:37 | 2 of 2 passed, 342 checks |
| C | `run --fresh` (each phase on a box of its own, Slint) | 01:27–02:03 | passed; 8 boxes, 255 checks (each box's setup counted) |
| D | `run --optimized` (the shipping profile with debug assertions) | 02:03–02:26 | passed, 171 checks |
| E | `run --capture` (tcpdump and TLS key logs, decoded) | 02:26–02:48 | passed, 173 checks; 385 + 143 connections, none wholly undecoded |
| F | `test stress-volume stress-interrupt stress-content stress-concurrency` | 01:37–01:56 | 61 of 61 |
| G1 | `test tour`, seed 1111, 3 b33ps, each step twice | 01:56–02:02 | 80 of 80 |
| G2 | `test tour --b33ps 5`, seed 2222, each step 3 times | 02:02–02:06 | 117 of 117 |
| G3 | `test --ui gpui tour`, seed 3333 (patched) | 02:06–02:08 | 79 of 79 |
| H | `TESTBOX_SOAK_MINUTES=45 test stress-soak` | 02:08–02:54 | 12 of 12 |
| J | `test desktop attachments core --b33p-memory 256` | 03:02–03:07 | 29 of 29 |
| I | `test … --b33p-memory 16` / `64` (control, meant to fail) | 02:48–02:58 | failed as intended (finding 4) |
| M1 | `test desktop core` on a b33p that drops messages (control, meant to fail) | 02:26–03:01 | failed as intended (finding 3) |

Also: `cargo test -p xtask` (49 tests, including the pin-drift test) passed.

Seconds per phase, checks passed/failed. Phase times agree to within about ten seconds across the
seven rounds and both windows, even `network` (316 s), which is fixed waiting; the exceptions are
C's `network` (294 s) and the optimized build's `groups` and `large-group`.

| part | A r1 / r2 | B (GPUI) r1 / r2 | C (fresh) | D (optimized) | E (capture) |
|---|---|---|---|---|---|
| setup, linked box | 12/0 136, 135 | 12/0 135, 135 | 12/0 136 ×4 | 12/0 133 | 13/0 135 |
| desktop | 33/0 44, 44 | 33/0 44, 45 | 33/0 44 | 33/0 45 | 33/0 44 |
| attachments | 13/0 12, 12 | 13/0 14, 13 | 13/0 12 | 13/0 10 | 13/0 12 |
| network | 16/0 317, 316 | 16/0 317, 316 | 16/0 294 | 16/0 316 | 16/0 317 |
| core | 8/0 92, 102 | 8/0 104, 102 | 8/0 101 | 8/0 100 | 8/0 102 |
| setup, Android box | 16/0 136, 136 | 16/0 135, 135 | 16/0 135–140 ×4 | 16/0 135 | 17/0 136 |
| android | 18/0 51, 50 | 18/0 50, 50 | 18/0 52 | 18/0 50 | 18/0 54 |
| groups | 30/0 98, 96 | 30/0 98, 97 | 30/0 98 | 30/0 86 | 30/0 97 |
| storage | 14/0 47, 48 | 14/0 47, 48 | 14/0 48 | 14/0 47 | 14/0 48 |
| large-group | 11/0 30, 27 | 11/0 27, 26 | 11/0 28 | 11/0 14 | 11/0 27 |

The check counts add up: 12 + 33 + 13 + 16 + 8 + 16 + 18 + 30 + 14 + 11 = 171 per round, the same
as the 2026-11-08 runs, so no phase silently dropped checks.

Stress (F): volume 13/0 in 55 s, interrupt 15/0 in 464 s, content 16/0 in 79 s, concurrency 17/0 in
175 s. Tours: a message showed on another b33p after 0.2 to 0.9 s (G1, 24 messages), 0.3 to 8.6 s
(G2, 49; median 0.28 s, p95 1.2 s) and 0.2 to 0.5 s (G3, 25); restarts took 2.1 to 4.6 s.

## Findings

### 1. `b33p-gpui --features harness` does not build on main (defect; fixed in a scratch patch)

The first GPUI image build failed, in both rounds of B:

```
error[E0769]: tuple variant `Action::SetSettings` written as struct variant
   --> b33p-gpui/src/agent.rs:808:9
```

- Cause: 29d98a3 ("Keep the messages presage's store has no place for…", 2026-11-09) made
  `b33p_harness::Action::SetSettings` hold a `SettingsChange` and updated `b33p-slint/src/agent.rs`,
  but not `b33p-gpui/src/agent.rs:808` or `b33p-gpui/src/e2e_tests.rs:270`.
- Confirmed on the host, not only in the image: `cargo check -p b33p-gpui --features harness --tests`
  fails the same way on 806c673 (CI's log counts 14 errors: the E0769 above, and an E0559 for each
  of the 13 fields of the struct literal in `e2e_tests.rs`).
- **Correction to this report's first version, which said CI does not build that feature: it
  does, and it caught it.** `ci.yml` runs `cargo test -p b33p-gpui --features harness --locked`,
  and the CI run on 806c673 (run 42801655710, 2026-11-10 10:44Z) is red at exactly that step with
  these errors. The runs before it (up to 86c3562) were green, and 29d98a3 came after them. So
  `main` has been red since; I did not look at CI before writing this. What is true is that the
  local `just check` does not run that step (`just test` is `cargo test --workspace`, `just lint`
  has no clippy for the feature), so a local check misses it, and the GPUI window's `--features
  harness` code (agent.rs, the testbox's GPUI) is not linted by CI's clippy either.
- Consequence: `cargo testbox --ui gpui`, the nightly's GPUI use and the GPUI e2e tests are broken
  on `main` until the patch below lands.
- The patch I used (10 lines added, 18 removed: wrap the pattern in `SettingsChange { … }`, build
  the test's action with `..SettingsChange::default()`) is in
  `docs/testbox-sceptical-run-2026-11-11-mutations/gpui-setsettings.patch` (deleted with the GPUI
  client, #178) and applied only in the
  worktree `../beep-sceptic-gpui`; `main` is untouched. With it `cargo check -p
  b33p-gpui --features harness --tests` passes and B and G3 passed.
- Fixed in `../beep-fixes` (uncommitted): the patch, plus the same step in `just test`, and clippy
  with the feature in `just lint` and `ci.yml`. `cargo test -p b33p-gpui --features harness
  --locked` passes (127 tests), as does clippy with `-D warnings` and `cargo fmt --check`.
- This also means runs B and G3 are of 806c673 plus that patch.
- Since then (noted 2026-11-18): the same change is on `main` as 5afec5a (2026-11-15), so the
  consequence above no longer holds and the patch no longer applies there. The `just test` step and
  the clippy runs with the feature are still only in `../beep-fixes`.

### 2. Memory grows steadily in the soak, and the check that watches it is too blunt to see it (not root-caused)

The 45-minute soak (seed 1794359349, 2,550 messages received) passed
"nothing lost, and nothing keeps growing". b33p's own `RssAnon`, sampled each minute (46 samples):

- 70 MiB at the start, 113 MiB at the end; slope about 0.69 MiB a minute over the whole run and
  0.76 MiB a minute over the second half, so it had not levelled off. Open files stayed 46 to 48.
- The check compares the median of the first third (about 90 MiB) with the median of the last
  third (about 111 MiB), and fails only if the last is more than 50% *and* 64 MiB above the first
  (`xtask/src/checks/stress.rs`, `growing`). A steady 0.7 MiB a minute would pass that for a
  soak of up to about 2.5 hours. So a slow leak is invisible to it by construction.
- Not separated: whether this is history held in memory (2,550 messages, about 17 KiB of growth
  per message if all of it is that, against about 235 bytes of content per message in
  `content-b33p.jsonl`) or a leak. The 2026-11-08 extended run saw the same shape (35 to 68 MiB
  in 30 minutes, "Q7 in `performance-open-questions.md`"; the question about this soak is Q13
  there, which extends Q7; (deleted 2026-11-20 with the GPUI client, #178: `git show 1c74cc0^:docs/performance-open-questions.md`)). The experiment that would tell: two
  soaks with different message rates, or one soak followed by a long idle stretch, and compare
  growth per message with growth per minute.
- Tours show the same direction: each b33p went from 66–71 MiB to 78–118 MiB in a few minutes.

### 3. "No client lost a message" does not see a message b33p drops after it has stored it (by design; scope to know)

Negative control M1: a worktree (`../beep-sceptic-mut`, patch `docs/testbox-sceptical-run-2026-11-11-mutations/m1-drop-and-log.patch`)
where `handle_content` drops everything from another account whose timestamp is even, and logs
an ERROR line saying so. Not only data messages: the patch sits before the match on the body, so
receipts, typing and stories from others go too. Run: `test desktop core`.

- Caught: 12 of the 33 `desktop` checks and 4 of the 8 `core` checks failed, each after its
  120 s patience (for example "text from Desktop reaches b33p: waiting 120 s for … in b33p's
  chat list"), and "b33p logged no unexpected error" failed in both phases (13 and 3 lines).
- **Not caught**: "no client lost a message" passed in every part. That is what its source says
  it is for (decryption errors, duplicates, counter reuse; `xtask/src/checks/lost.rs`); a b33p
  that decrypts and stores a message and then never shows it is outside it.
- So what stands between a silent drop (no ERROR line) and a green run is the direct checks, which
  look at the messages they sent. A drop in a path no check sends through would pass. The soak's
  content log (`content-b33p.jsonl`) records what b33p received, at the same point the mutation
  drops after, so it would not see it either.
- Suggested: a check that compares what b33p's content log received against what the window
  shows or the store holds, at the end of every phase.

### 4. The memory-limit controls: 16 and 64 MiB kill b33p at once; 256 MiB is fine

- `--b33p-memory 16` and `64` (`core` and `desktop`): b33p was SIGKILLed during setup (the linked
  box: 5 passed, 3 failed, in 10 s), and "no client crashed" failed listing 69 and 44 crashes. So
  that detector works, and fails closed.
- "No client lost a message" also failed in both, but only because Desktop had no logs to read
  (`cat /home/desktop/data/logs/*.log: No such file`); it fails when it cannot read a log, rather
  than passing. That is the right way round, but the message ("lost") is misleading when the
  cause is a box that never came up.
- The container's baseline is about 120 to 160 MB (the cgroup, with the compositor and VNC), so
  limits under about 150 MiB say nothing about b33p. At `--b33p-memory 256`, `desktop`,
  `attachments` and `core` passed (29/29): b33p survives a 256 MiB machine for the ordinary phases.
- Related, from F: three 11000×11000 pictures took b33p's peak to 778 MiB, under the check's
  1024 MiB budget with about 24% to spare, about 200 MiB per picture; the check uses three, and a
  chat with more would go past it. (The refused pictures are refused: empty, truncated,
  claims-huge, wrong extension.)

### 5. Each linked box's setup spends about two minutes waiting for Desktop's inbox (an Android box's, about one)

"Desktop shows its inbox" passed in 121.3, 121.4, 120.9 and 121.4 s on the four linked boxes of C
(53 to 61 s on the Android boxes). Desktop waits for syncs that nobody in the box answers, and the
driver's patience is ten minutes (`xtask/src/checks/setup.rs:400`), so the wait is Desktop's own
timeout, not a hang. It is not a failure, but a setup is about 135 s, and each of C's 8 boxes (and
every round) pays it: about 18 of C's 36 minutes (8 boxes of about 135 s each). If a presage-cli peer answered Desktop's sync
requests, or Desktop were told its timeout, `--fresh` would be much quicker.

### 6. Records of `test` (not `run`) are overwritten by the next one on the same instance

`run` keeps its records under `record/run/<when>/<round>/`, but `test tour` writes to
`record/mesh/`, so G2's record replaced G1's (and the same for any repeated `test X`). I lost
G1's per-step data (its log and summary are kept). Not a problem when the phase is run once; it
is when a sequence of runs is wanted for comparing. Suggested: put `test`'s records under a
timestamp too, or refuse to overwrite.

### 7. Warnings and errors in the logs: all explained except two

Every container's log in the records of A, B, C, D, E, F, G2, G3 and H (27 boxes) was scanned for
panic, OOM, SIGSEGV, core-dump and FATAL lines, and b33p's, the server's and the storage service's
WARN and ERROR lines were grouped by kind (not scanned: G1, whose record G2's had replaced, finding
6; J; the controls). No panic, OOM, segfault or core
dump anywhere; b33p's ERROR lines are only the seven kinds `errors.rs` lists. What the rest is:

| line | where | what it is |
|---|---|---|
| `Illegal pseudo-header ':protocol'` (Netty, WARN, 3 per Android box) | server | Signal-Android tries an HTTP/2 websocket (RFC 8441) on the gRPC port; refused, it falls back. A client of the box, not b33p. |
| `onErrorDropped … ClosedChannelException` (1) | server | the server sends on a websocket that just closed |
| `a-bucket.s3.…amazonaws.com UnknownHost` (185 `S3ObjectMonitor`) | server | the pod has no egress, as intended; a monitored object can't be refreshed |
| `empty decrypted content` (2, Android box) | b33p | follows a sender-key distribution message that carries no content (logged right after "applied sender key distribution") |
| `trusting new identity` (44 in the Android box) | b33p | presage trusts a first-seen key (TOFU) |
| `Failed to connect to the bus`, dconf, `Xwayland` | b33p, Desktop | the clients' containers have no D-Bus session, nor Xwayland (known) |
| `adb … screen_off_timeout failed: device offline` (8) | Android | emulator still booting when the setting is made |
| `failed to sync groups from the storage service: Not found` (18) | b33p | a new account has no manifest |
| `GET /v2/groups/logs/N … 403` and b33p's `failed to read the group's changes; fetching it whole error=Authorization failed` (8, tour only) | b33p, storage | b33p asks for a group's log before it is (or after it is no longer) a member and falls back to the whole group; the first one lines up with a join by invite link. **Not proven for all 8**: the storage log has 5 403s against 8 warnings. |
| `ignoring edit of a message by someone else or not found` (2), `could not find message to delete` (1) | b33p, tour | tour steps made by other members (edit/delete the model says b33p must refuse); not matched to steps one by one |

No further failure surfaced.

### 8. b33p fetches the whole storage manifest on every remote change

In E's Android box (the whole box, its setup and four phases, not the `storage` phase alone: a
run of that phase by itself makes 15 of the whole-manifest calls) Android wrote the manifest 55
times (`PUT
/v1/storage?sendRemoteManifestOnConflict=…`, 53 × 200 and 2 × 409). b33p made 55 full `GET
/v1/storage/manifest` calls, 66 `PUT /v1/storage/read` and 83 `GET /v1/storage/auth`, plus 28
`manifest/version/{n}` calls (18 of 200, 10 of 204); Android and Desktop each fetched the manifest
once. The version call answers 204 when nothing changed and the whole manifest otherwise, so its
18 answers of 200 are whole manifests as well. For a few records this is nothing;
for a real account with thousands of records, 55 whole-manifest downloads are a cost that none of
Signal's clients pays. Not measured beyond the counts, and "full manifest" is read from the path
(`/v1/storage/manifest` vs `/v1/storage/manifest/version/{n}`), not from the response size.

### 9. "Every upstream is pinned" is true of sources and images, not of everything

Audited in `testbox/*/Containerfile`: every `FROM` is pinned by digest, every upstream (Signal-Server,
storage-service, tus, Desktop, Android, libsignal, presage) by full commit, the Android command-line
tools and the Skia binaries by SHA-256 (the emulator's archive by SHA-1), dependencies by
`Cargo.lock`. Not pinned: the Debian packages (`apt-get install` in 9 of the 12 Containerfiles
under `testbox/`, on a digest-pinned image whose package lists are current at build time) and what
Android's `sdkmanager` installs without a version: `platform-tools` and the system image, whose
revisions the Containerfile then asserts (one that moved fails the build), and
`platforms;android-37.0`, whose revision it does not. A rebuild without the layer cache can pull
newer packages and a newer platform. The emulator was already caught once, when Google
moved it ([testbox-runs-2026-10-31.md](testbox-runs-2026-10-31.md)). `testbox/gpui-view/Containerfile` (now `testbox/view/Containerfile`) uses `archlinux:latest`
(not in the pod). The README's sentence is slightly stronger than that.

### 10. Small things

- `TESTBOX_SEED=2222` printed and recorded seed 2223: `rng::seed()` does `| 1` (xorshift can't
  start at zero). Reproducible (2222 and 2223 draw the same tour), but not what was asked for.
- G2's slowest message took 8.6 s to show on another b33p (median 0.28 s); every other tour's
  slowest was under 1 s. Probably a restarting b33p catching up (restarts took up to 4.6 s), but I
  did not find the step it belongs to.
- "A record of Sent/Received didn't open" and "the handshake didn't finish" (10 of 143 connections
  in E's linked box) are the network phase's cut links; the Android box had 4 handshakes that
  never finished and nothing else undecoded.
- b33p's `keepalive` on the wire: every 30 s, as Desktop and Android; one `GET /v1/keepalive` of
  60 s in E's linked box is the `network` phase's dead link.

## What the runs do not cover

- Identity changes: nothing checks what b33p does when a contact's identity key changes (a
  re-registration; the log shows 44 `trusting new identity`). The tour checks that two b33ps show
  the same safety number and that verifying works, nothing more.
- The wire comparison with Signal's clients is by request counts and shapes (`compare.md`); I did
  not compare bodies beyond what xtask's decoder does.
- Window (Slint with Skia) and GPUI were run on a software renderer in a container, debug and
  `testbox` profiles, not on a desktop with a GPU, a screen scale or a locale other than English.
- `network` impairs the link with netem; it does not cover a real carrier's behaviour.
- Everything runs against Signal-Server's test mode: not the enclaves (contact discovery, SVR),
  a production phone's history or Signal's servers.
- The mutation tested one kind of fault (a drop after storing). Other detectors (a hang, a
  duplicate, reordering, a wrong receipt) were not tried by mutation; a hang is what "text from
  Desktop reaches b33p" already catches (it took 120 s to say so).

## Where things are

- The run's own output (each step's stdout, its timings) and the scripts that ran the steps were in
  a scratchpad that a reboot cleared afterwards; the numbers above were taken from them before. The
  patches of the experiments are kept in `docs/testbox-sceptical-run-2026-11-11-mutations/`.
- Records (each container's log, Android's logcat, Desktop's own logs, the wire of E):
  `../beep-sceptic/target/testbox-sc1-record/run/{1794354577 (A), 1794356832 (C), 1794359029 (D), 1794360375 (E)}`
  (the numbers are the run's start in seconds since the epoch; check `ls`), and `testbox-sc2-record/`
  for F, G2 and H; `../beep-sceptic-gpui/target/testbox-sc2-record/` for B and G3;
  `../beep-sceptic-mut/target/testbox-sc3-record/` for M1.
  They were not committed (12 GB of disk in the three worktrees) and **are gone since**: on
  2026-11-18 the three worktrees' `target/` hold only the boxes' stale sockets, so the logs and
  E's wire can no longer be read again.
- Worktrees made for this: `../beep-sceptic` (806c673, untouched), `../beep-sceptic-gpui` (plus the
  patch of finding 1), `../beep-sceptic-mut` (plus the mutation). All pods of mine are down.
- Reproduce: `cargo testbox --instance sc1 run --rounds 2`, `… run --fresh`, `… run --optimized`,
  `… run --capture`, `… test stress-volume stress-interrupt stress-content stress-concurrency`,
  `TESTBOX_SEED=2222 TESTBOX_TOUR_REPEAT=3 cargo testbox test tour --b33ps 5`,
  `TESTBOX_SEED=1794359349 TESTBOX_SOAK_MINUTES=45 cargo testbox test stress-soak`.
