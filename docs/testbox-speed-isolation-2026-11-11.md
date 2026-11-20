# Testbox: faster and better isolated, 2026-11-11

Worktree `../beep-speed` on 806c673; boxes `spd`, `spd-j1` … `spd-j4`. It stayed uncommitted
until 2026-11-20, when it was ported onto main (the interop and family phases, added since, are
units of their own under `--jobs`, and `--android-app` is passed on to each box). The baseline is the
sceptical run of the same day (`docs/testbox-sceptical-run-2026-11-11.md`: `main` at 806c673, on a
machine with other sessions' boxes on it, load average 3 to 13). All the runs below ran on that
machine too, beside other sessions' work, so the seconds are for a shared machine.

## In one paragraph

Two waits that no check needs were most of a run, and a third is why boxes could not play at
once. **Every `podman build` looked each base image up in Docker Hub, which answers "too many
requests" and makes the build wait about 30 s: ten builds, five minutes before the first check,
every round, with every layer cached.** **Desktop waited two minutes for syncs that nobody
answered, in every box.** Both are gone (a cached build takes 1 s; Desktop's inbox, 9 s). The
boxes of a run can now play at once (`--jobs N`). A run of the default phases went from 18.7
minutes a round to 13.7 minutes one box after the other and **8.7 minutes with `--jobs 2`**; every
phase on a box of its own (`--fresh`) from 36.4 to **8.9 minutes with `--jobs 4`**. All the checks
still pass (171 and 255), and still fail when they should (below). Beside that: `doctor` and
`clean` for what killed runs leave on the machine, `down --wipe` removes the network (35 were left,
of the 256 a netavark pool has), `test`'s records are not overwritten by the next `test`, and
b33p's image no longer compiles again for a change to the xtask or the docs.

## Measured

The default phases are the linked box's (desktop, attachments, network, core) and the Android box's
(android, groups, storage, large-group); one round each:

| Run | Before | After | Checks |
|---|---|---|---|
| `run` (one box after the other) | 18.7 min a round (A: 37.4 min for 2 rounds) | **13.7 min** (V1) | 171 passed |
| `run --jobs 2` | – | **8.7 min** (V2; units of 507 s and 388 s) | 171 passed |
| `run --fresh` | 36.4 min (C) | – | 255 passed, setups included |
| `run --fresh --jobs 4` | – | **8.9 min** (V3; the longest unit, `network`, 366 s) | 255 passed |
| a cached image build | 32 s each, 9 to 10 of them (295 s before F's first check) | **1 to 4 s** (16 s in all in V1) | – |
| linked box's setup | 133 to 141 s | **25 to 33 s** | 12 passed |
| Android box's setup | 135 s | **79 to 90 s** | 16 passed |
| "Desktop shows its inbox" | 120.1 s | **9.1 s** (linked box) | passed |
| b33p's image, after a docs or xtask edit | 29 to 43 s | **2.5 s** | – |

The phases' own times are as before (V1: desktop 45, attachments 12, network 288, core 101,
android 50, groups 98, storage 47, large-group 26 s). With `--jobs` the second box's phases take a
little longer (V2: groups 106 s against 98, storage 76 against 48, large-group 51 against 26)
because two boxes and other sessions share the cores. A box's own overhead (play, ready waits,
record, teardown) is about 15 s: V2's linked unit took 507 s, its setup and phases 491 s.

## Ideas, one by one

| Idea | Measured gain | Risk | Status |
|---|---|---|---|
| `podman build --pull=never`: the base images are pinned by digest and in the store | 32 s → 1 s a build; about 5 minutes a `test`, `run` round or `up`; the images build offline | a base image not in the store: the build says "image not known" and runs again with the registry (a test covers it) | done |
| presage-cli answers Desktop's sync requests | inbox 120 → 9 s; every linked box −110 s, Android box −55 s | the patched presage-cli is the testbox's only. Desktop now receives an (empty) contact sync as it does from a phone, where it ran into its own timeout. The 33 `desktop` checks and every other phase pass as before | done |
| `run`/`test --jobs N`: units on N boxes at once | 18.7 → 8.7 min with 2; 36.4 → 8.9 with `--fresh` and 4 | the boxes slow each other a little (above); a unit's output comes when it ends | done |
| b33p's image: a narrower build context | an xtask or docs edit no longer compiles b33p again (about 40 s here, minutes with a cold target) | a file the build reads must not be in the ignore list (`testbox/b33p/Containerfile.containerignore`); it would fail the build at once, not pass wrong | done |
| `test`'s records by when it started, `<box>` leads to the latest | nothing lost to the next `test` (the sceptical run lost its first tour's record) | none seen | done |
| `down --wipe` removes the box's network | 35 left on this machine; netavark has 256 /24s in 10.89.0.0/16 | none | done |
| `doctor`, `clean [--strays] [--yes]` | finds the strays in a second (31 of 41 boxes here); a look first, `--yes` removes | removes only a box's pod, volumes, network, image tags (named exactly) and this worktree's scratch directories for it | done |
| A snapshot of the linked box, to start each phase from | at most the 25 to 33 s that setup takes now (the 120 s was the wait above) | volumes copied in the middle of a Signal account: state to get right | not done: no longer worth it |
| Trimming the `network` phase's waits | none: 90 s dead, 160 s flapping and the recovery are what it tests (libsignal-service's keepalive gives a link up after 55 s) | would weaken it | not done; with `--jobs` it is the longest unit (366 s) instead of a part of 19 minutes |
| `stress-interrupt` (464 s) | none: 180 s of it is the server hanging on purpose (`HANG`) | – | not done; parallel units |
| Building images in parallel | none now (1 s each); the Rust builds share a `sharing=locked` cache of cargo's target, so they take turns anyway | – | not done |
| Readiness by polling and sleeping | nothing to gain: boxes already wait on log lines (`await_log`), and nothing sleeps but the checks' own | – | already so |
| An emulator snapshot (the box boots Android cold, `-no-snapshot`) | about 20 to 25 s an Android box (not measured) | a snapshot of an AVD with an account in it | not done |
| CPU and memory limits per pod | not measured. While a box starts Signal-Server peaks at 230% of a core and the storage service at 140%, the Android emulator at 55% | a limit on a startup burst can make a wait run out, for no gain to the other boxes, whose patience is minutes | not done; use `--jobs` 2 or 3 on a shared host |
| Pinning the apt packages (snapshot.debian.org) | none | rebuilds every image, Signal-Android's from source among them | not done; the README's "every upstream is pinned" holds for sources and base images only |

## What was slow, and why

- 295 s of image builds before F's first check, and as much before every box of `run`: all of it
  Docker Hub's rate limit (`level=debug … Too many requests to https://registry-1.docker.io/v2/…/manifests/sha256:…`,
  retried with a back-off). A build looks every `FROM` up in its registry, digest-pinned and
  present or not. The probe: the same build with `--pull=never` took 1.1 s, and with the registry
  answering, 2 s. It looked like contention between concurrent builds; it was the registry. A
  machine whose Docker Hub quota is not spent never sees it.
- 120 s of every setup was Desktop's `waitForEvent('contactSync:complete')` (`short-lived`
  timeout, two minutes), after it asks its primary for contacts, configuration and the blocked
  list. `app-loaded` comes at once; the inbox waits (ts/background.preload.ts, step 7). presage's
  receiving loop does answer (`registered.rs`, `RequestType::Contacts` and `Blocked`), but from
  `spawn_local` tasks, which `presage-cli sync --stop-after-empty-queue` took with it when it
  exited: the blocked list went through (it is quick), the contacts did not. The fix is
  `sync --linger-ms` (`testbox/patches/presage-cli-linger.patch`, applied in
  `testbox/presage/Containerfile`): the xtask runs the peer's sync a few seconds apart while
  Desktop's inbox is awaited, and in the Android box also while Android and b33p set up.
- With the boxes one after the other, the longest phase (`network`) and the Android box's setup
  were in the sum, not beside it.

## What changed, and how to use it

- `cargo testbox [--instance X] run|test [--fresh] --jobs N [phases]`. The units: the linked box
  and the Android box (one each, or each phase with `--fresh`), each stress phase, the mesh; the
  soak, the longest, first. A unit is `cargo testbox --instance X-j<k> test …` on the box `k`, with
  the images this run built tagged for it (`--prebuilt`, hidden). Their records are in
  `target/testbox-X-j<k>-record/test/<when>/<box>/` (the summary lists them). At most two Android
  boxes at once. A unit that fails leaves its box up; the others are taken down (`down --wipe`)
  when the units are done. N up to 8; 1 as before.
- `cargo testbox doctor`; `cargo testbox --instance X clean [--yes]`; `cargo testbox clean --strays [--yes]`.
- `test`'s records: `target/testbox-record/test/<when>/<box>/`, `<box>` beside it leads to the
  latest (a directory an older xtask left there is moved to `test/earlier-…`).
- `sync --linger-ms` in presage-cli; the driver's `inbox` can be asked again after a timeout.

Files: `xtask/src/{jobs,doctor}.rs` (new), `xtask/src/{main,pod}.rs`,
`xtask/src/checks/{mod,presage,record,setup}.rs`, `testbox/desktop/driver.ts`,
`testbox/presage/Containerfile`, `testbox/patches/presage-cli-linger.patch`,
`testbox/b33p/{Containerfile,Containerfile.containerignore}`, `testbox/README.md`. 56 xtask tests
pass (49 before; units, instance names, the build's fallback); `cargo clippy -p xtask
--all-targets -- -D warnings` and `cargo fmt --check` are clean.

## Do the checks still fail?

- The Desktop inbox check still has ten minutes and fails if Desktop never shows one or its driver
  goes (any error but the driver's timeout ends it at once). The old path, when the peer's answer
  doesn't come, is intact: T1 (the loop with presage-cli exiting at once, as before) passed at
  120.1 s, as it did before.
- What the speedups removed is waiting: Docker Hub's and Desktop's. None of the 255 checks'
  expectations changed. The sceptical run's negative controls (a b33p that drops messages, a
  memory limit) are the same code and were not run again.
- A failed unit under `--jobs` fails the run (`N of M units failed: …`) and leaves its box up;
  not exercised, as the runs passed.

## What boxes share, and what leaked

| Shared or leaked | Finding | Fix |
|---|---|---|
| Pod, volumes, internal network, image tags | named by the instance; ports are the pod's own network namespace (no `hostPort`), the web viewer's is the kernel's | – |
| The box's lock | `$XDG_RUNTIME_DIR/<pod>.lock`, host-wide, so two worktrees on one instance fail at once and name `--instance`; the lock files stay (harmless) | – |
| The default box | one pod name for every worktree | the lock; use an instance |
| Networks | `down --wipe` left each box's network: 35 here, 31 of them with no pod | removed on wipe; `clean` for the old ones |
| Volumes and tags of boxes whose runs were killed or that were `down`ed without `--wipe` | 31 strays of 41 boxes | `doctor` shows, `clean --strays` removes |
| Idle pods left up (`test` leaves the last box up) | `mem` on this machine; costs the cores of Signal-Server, storage and the rest | `doctor` says "idle" |
| Records | `test`'s overwrote the same box's | by time |
| Image tags | every instance's tag of an image is one image (14 names for `dynamodb`), so removing a tag removes only the name | `clean` removes exact tags, so `spd` doesn't take `spd-j1`'s |
| Build caches | cargo's registry and target caches are host-wide and `sharing=locked`: builds from different worktrees take turns | – |
| `/dev/kvm`, cores | the only things boxes truly share | `--jobs` lets two emulators run |

An instance name with a dash (`spd-j1`) is ambiguous with `<box>-<image>` and `<box>-<volume>`:
`doctor` and `clean` take the known images' names and the longest known box as the owner.

## Not done

- A `--jobs` that picks N from the cores; N is the user's.
- `--jobs` across the rounds of `run`: rounds stay one after the other.
- Android's emulator snapshot, per-pod CPU weights and pinned apt packages (above).
