# Testbox run on main, 2026-10-31

**Result in one paragraph.** main at `ad1b30c` (origin/main plus PRs #128, #129, #130, merged
and pushed) passes every testbox check: linked box 12, desktop 17, attachments 10, core 6,
Android box 16, android 9, groups 25, storage 12, mesh 10, tour 78 (seed 1793482317), with
screenshots and the wire capture on, every connection decoded. The only change needed on the
way is one pin: Google moved the Android emulator to 37.2.13 and
`testbox/android/Containerfile` asserted 37.2.12 (bumped in the working tree, uncommitted; the
other session committed the same bump in its worktree). Getting there cost a day: podman
wasn't installed, the running kernel's modules were gone until a reboot, the box lock was held
by a second session's runs three times, and podman's builder crashed once under three
concurrent instances. b33p itself never faulted; the log lines that look like faults are
explained below. **Disk is the thing to act on**: free space went 314 → 119 GB today across all
sessions' runs, with 30+ GB of dangling Android layers reclaimable by `podman image prune`.
Added afterwards, on request: `--capture` no longer puts a relay in the path. tcpdump writes
the pod's loopback down and every server writes its own TLS key log; xtask decodes the
connections from those (validated against the relay's records: identical events), and the
record's `wire/capture.pcapng` opens decrypted in Wireshark (tshark: 40 of 40 TLS streams).
Eight checks were added (reactions taken back both ways, a long text, Desktop's typing, a
deleted picture, a message while b33p is down, and "b33p logged no unexpected error" at every
phase's end); two full rounds found the Android driver's stale-node failure, now retried, and
every phase passed afterwards (1 Nov, 04:00).

Notes taken as the run went, in the order things happened. Setup and merges first, then the
box's phases, then what the box could cover that it doesn't.

## Before the run

### Merges (by git, pushed to main)

- Local main was 297 commits behind origin/main; fast-forwarded.
- Open PRs #123, #125, #126, #127 are already contained in origin/main (the `consolidate`
  merges); GitHub still lists them open because their bases are the chained branches. Nothing to
  merge there; they can be closed.
- Merged cleanly, in this order: #128 (checkpoint the WAL after erasing; `secure_delete`), #129
  (window kept out of screen capture on Windows), #130 (read-receipt, typing and link-preview
  settings from the account record).
- Not merged: #108 (desktop actor) conflicts in `b33p-slint/src/ui/callbacks/{chat,groups,panels}.rs`;
  #109 (session actor) conflicts in `b33p-core/src/signal/{conversations,lifecycle}.rs` (mergiraf
  solved the ones in `receive.rs` and `history.rs`). Both are 81 commits behind main and need a
  rebase by their author, not a merge.
- CI's checks (fmt, clippy ×3, tests) run locally on the merged main before pushing; result below.

### The host

- Manjaro, kernel 7.1.13, 16 CPUs, 27 GiB RAM, 314 GB free. `/dev/kvm` is world-readable and
  writable; `bridge` and `veth` are loaded; subuid/subgid ranges exist.
- Podman was not installed at all (Docker was). Installed by the user during this session:
  podman 6.1.3, netavark, overlay, rootless, pasta; the OCI runtime podman picked is **runc**
  (from Docker's install), not crun. Worth noting if a container behaves oddly: the README and
  the nightly machine presumably run crun.
- No testbox images, volumes or records existed here: this is a first `up`, so every image
  (Signal-Server, storage service, Desktop, libsignal, Android) builds from source.
- Load average was already ~6 before anything started (the CI build), so timings in the first
  box are pessimistic.

### Usability of getting started

- The README says what the host needs clearly; the one thing it doesn't say is *which* packages
  to install on a non-Debian host. A line per distro (`pacman -S podman passt netavark`, `apt
  install podman passt`) would have saved the detour.
- `cargo testbox` can't be built while another cargo command holds the target lock (CI run), so
  `--help` is unavailable during a check; the source (`xtask/src/main.rs`) is readable instead.
- There is no `cargo testbox doctor`/preflight that checks podman, kvm, subuid, modules and disk
  before a multi-hour build starts. The failure modes the README lists (netavark bridge after a
  kernel update) are exactly what such a command would catch in a second.

## The run

### Starting the run (10:26 local)

- Command: `cargo testbox test --screenshots --capture desktop attachments core android groups storage tour`.
- The first thing `cargo testbox` prints is a screenful of cargo's own `redundant_homepage`
  warnings (new cargo lint, `homepage.workspace = true` beside `repository.workspace = true` in
  b33p-core and b33p-slint). Harmless, but it is the first thing a newcomer reads; one-line fix
  in the two Cargo.tomls.
- The log's timestamps are UTC (`09:26:04Z`) while the record directories and the host are local
  time; matching a line to a screenshot means converting in one's head.
- The image builds start one at a time: `building b33p-testbox-server` is the only build log
  for the first minutes, on a 16-CPU host that is mostly idle. Pulling the base images
  (temurin JDK/JRE, debian trixie, rust) is the first cost. Whether the other images build in
  parallel is answered below.


### First attempt ended after two minutes: the kernel's modules are gone (10:28)

- The server image's first `RUN` failed: `setup network: pasta failed with exit code 1: Failed
  to open() /dev/net/tun: No such device`. `tun` is not loaded, and cannot be: the running
  kernel is 7.1.13 but `/lib/modules` holds only 6.12, 6.18 and 7.2.9. The machine was updated
  to 7.2.9 and not rebooted since. `bridge` and `veth` happened to be loaded already, which is
  why the README's netavark symptom didn't show and the kvm, subuid and module checks all
  looked fine.
- **Blocked until a reboot.** Nothing in user space can load a module for a kernel whose
  modules are deleted; Docker's runtime doesn't help since the xtask is podman-only
  (`xtask/src/pod.rs` says so in its first lines).
- The xtask reported it well: one `testbox: building b33p-testbox-server failed (exit status:
  125)` line with the tail of the build log, and it exited with 1 at once rather than going on to
  the other images. Clean.
- Usability: the README's kernel-update note names only netavark's bridge error. `pasta` and
  `/dev/net/tun` are the other face of the same cause, and a preflight that compares `uname -r`
  with `ls /lib/modules` (or just tries `podman unshare --rootless-netns true`) would say
  "reboot first" in a second instead of after pulling base images. The pulled layers are cached,
  so nothing is lost but the minutes.
- To resume after the reboot: the same command, `cargo testbox test --screenshots --capture
  desktop attachments core android groups storage tour`.


### After the reboot (22:00): the box is taken

- Preflight by hand after the reboot: kernel 7.2.9, `tun` loads, `podman unshare
  --rootless-netns true` succeeds, kvm fine. (That one-liner is the preflight the xtask lacks.)
- My run ended at once: `testbox: another cargo testbox up, test or down holds the box
  b33p-testbox (/run/user/1000/b33p-testbox.lock); another box plays beside it with --instance`.
  A second Claude session, started from the presage checkout but running in this worktree, is
  running `cargo testbox test desktop attachments core` as a baseline. The lock did its job and
  the message says exactly what to do. Good.
- Not taking `--instance`: the images (all but Android) were built by that session eleven hours
  ago, every image rebuilds from cache in 0–8 s, but the Android image doesn't exist yet and
  builds from source during its linked box. Two first Android builds and two emulators side by
  side would make both runs' timings meaningless. Waiting for the baseline to release the box
  and watching it meanwhile; its record is in the same `target/testbox-record/`, which my run
  overwrites, so its records get copied aside first.
- Cached build times from the baseline's log: server 8 s, dynamodb 7 s, foundationdb 3 s, tus 0 s,
  cdn0 2 s, storage 6 s, presage 3 s. b33p's own image then builds from the working tree.

- b33p's image: 466 s, with 653 `Compiling` lines in its build log, i.e. a cold build although
  the Containerfile mounts a shared target cache "shared by every worktree's and every box's
  builds". Either the cache is keyed in a way the merged `Cargo.lock` (three PRs in) invalidated
  wholesale, or the eleven-hour-old images were built elsewhere. Eight minutes per b33p change
  is the inner loop's cost; worth checking what the cache actually keeps.


### The baseline run's linked box (the other session's, watched from its log)

Setup, in order, with what each took:

| check | s |
|---|---|
| no egress | 0.1 |
| the server is healthy | 17.3 |
| the storage service serves | 0.1 |
| two presage-cli accounts register | 9.5 |
| b33p links to the primary | 6.4 |
| Desktop links to the peer | 0.7 |
| Desktop shows its inbox | **121.4** |
| text from the peer reaches b33p | 0.5 |
| the peer's first message is a message request in b33p, accepted | 0.1 |
| no client lost a message / crashed | 0.4 / 0.2 |

- Desktop's first inbox is two minutes of the setup's ~2.6. Whatever Desktop does after linking
  (its initial sync, storage fetch, or just Electron starting under Xvfb) dominates a box's
  start; worth a line in the record saying what it waited on, since "shows its inbox" hides it.
- b33p's own linking and first message are fast (6.4 s and 0.5 s): the client under test isn't
  the slow part of the box.


The phases (baseline run, no screenshots or capture):

| phase | checks | time | slowest check |
|---|---|---|---|
| desktop | 17 passed | 10 s | a reaction from Desktop reaches b33p, 2.6 s |
| attachments | 10 passed | 24 s | a profile picture set on Desktop shows in b33p, 9.4 s |
| core | 6 passed | 165 s | a message sent while the server doesn't answer fails, and goes out when sent again, 130.5 s; reconnect after the server drops, 31.5 s |

- Every message-level round trip between b33p and Desktop is under a second; attachments
  through CDN3 under 2.2 s. The box's own costs dwarf b33p's: a linked box is ~2.6 min of setup
  + ~3.3 min of phases, of which 2 min is Desktop's inbox and 2.7 min is `core`'s two
  deliberate waits (a keepalive timeout and a reconnect).
- The failed-send check waits 130 s by construction (libsignal-service's keepalive). That's the
  one check whose cost is a protocol constant rather than a measurement; a `--quick` that
  skips the two timeout checks would make the linked box a 3-minute smoke test.


What b33p's own log from that green box says (record `linked/b33p-testbox-b33p.log`, 11 ERROR
and 19 WARN lines in a run where every check passed):

- **`ERROR presage::manager::registered: failed to register pre-keys, this is problematic and
  should never happen!`** at 21:16:54, the moment the keepalive failure closed the socket during
  core's "server doesn't answer" check. Expected in that scenario, but the line's wording is
  presage's alarm for an unexpected state, and nothing in the record says whether the pre-key
  upload was done again after the reconnect. To verify with `--capture` in my run: a `PUT
  /v2/keys` after the reconnect. If there isn't one, b33p runs on with stale one-time pre-keys
  until the next start; a check "pre-keys are uploaded again after a dropped upload" would pin it.
- `WARN presage::manager::linking: failed to sync groups from the storage service … Not found.`
  on every fresh link: the new account has no storage manifest yet. Benign here, but it is the
  same line a real storage-service outage would produce; a fresh-account case deserves its own
  words (or a lower level) so the real one stands out.
- `b33p::instance: can't tell whether b33p is already running` and `desktop notifications
  unavailable`, both because the container's session bus address is `/nonexistent`. So the box
  never exercises b33p-notify or the single-instance check. Coverage gap: a `dbus-daemon
  --session` in b33p's container (or a notification-capturing stub) would let a check assert
  "a message in an unselected chat notifies, a muted one doesn't" and "a second b33p hands over
  to the first".
- `cage`: `Cannot find Xwayland binary`, `Cannot create XWayland server`, twice each (b33p
  starts twice: the restart in `core`). Deliberate: the Containerfile removes Xwayland. Still two
  ERROR lines per start in every record; `cage -- -x`/an option to not try Xwayland, or a
  grep in `with-display`, would keep the record's ERRORs meaningful.
- `trusting new identity` twice: first contact, as designed. It also means a safety-number
  change never happens in the box (already listed in `pending/testbox-review.md`).

Host load during the baseline's end: loadavg 20, IO pressure `full avg60=13.9`. Not the box's
own doing: a second, unrelated job on this machine is compiling a Rust workspace
(`/work/analyze.sh cargo-leptos … cargo build --workspace`, rustc on swc crates) with load 21
as my run starts. Timings in my run are pessimistic by an unknown factor; the box records
`load.txt` only at the end, so a per-check load column (or `/proc/pressure` sampled with each
check) would let a slow check be told from a slow host afterwards.

- My run's b33p image: 202 s with nothing changed since the baseline built it (the record
  shows only 4 `Compiling` lines, the workspace crates, `Finished … in 50.41s`). That is by
  design: the Containerfile touches every source so cargo rebuilds b33p's own crates each time,
  since the target cache is shared across worktrees and goes by mtimes. The context is only ~6 MB, so
  the other ~150 s are `cargo fetch`, the touch, the runtime stage's apt (if not cached) and a
  loaded host; the build log has no timestamps, so it can't say which. Every run pays about
  three and a half minutes for b33p's image even when nothing changed. The baseline's 466 s was the cache
  volume's first fill, not a bug. A cheaper key than mtimes (a content hash per crate, or
  `CARGO_TARGET_DIR` per worktree) would make the no-change case seconds.


### My run's linked box (with `--screenshots --capture`)

- "Desktop shows its inbox": 119.3 s, against 121.4 s in the baseline. A constant, not load:
  the Desktop driver documents it (`driver.ts` `inbox`): after linking, Desktop waits for the
  primary's contact sync and the storage service, which the presage-cli primary never answers,
  until its own waits give up. Two minutes per box, twice per run (the linked and the Android
  box), four on a `--fresh` run. Cheapest fix on the box's side: have presage-cli answer the
  contact-sync request (an empty contacts sync message), or write an empty storage manifest
  for the peer at setup; then the inbox shows in seconds and the wait measures Desktop instead
  of a timeout.

- Linked box, this run: setup 12 passed in 152 s; desktop 17 in 30 s (baseline 10 s);
  attachments 10 in 39 s (24 s); core 6 in 116 s (165 s). Screenshots and the tap cost the
  message phases about 2–3×, still under a minute each. `core`'s two waits are not constants
  after all: the failed-send check took 89 s here against 130 s, the reconnect 16 s against 32 s,
  so they depend on where in the keepalive cycle the server is stopped. Fine for a check, but
  it means a run's total swings by a minute from nothing b33p did.
- The wire capture decoded every connection: 108 connections, 704 events (b33p 207, Desktop 219,
  presage-cli 233, presage-rs 41, cdn3-itself 4), none left undecoded.

### The Android image build crashed podman itself (22:19)

- `building b33p-testbox-android failed (exit status: 2)`: not the Containerfile. buildah hit
  `error unmounting container … layer not known` and then `panic: runtime error: invalid
  memory address or nil pointer dereference` in `stageExecutor.intermediateImageExists`
  (podman 6.1.3). Every step before it was `Using cache`.
- The likely trigger is concurrency in podman's storage: at that moment the host was running my
  box, the other session's `cargo testbox test --instance split …` from a second worktree
  (`code/worktrees/beep-split`, building `b33p-testbox.split-*` images), and a third pod,
  `b33p-testbox.pipeline`. Three boxes on one rootless store, all building stages in parallel
  (`--jobs 0`), and one of them pruned or replaced an intermediate layer another was reading.
  The xtask can't prevent a podman bug, but it could take its builds one at a time across
  instances (a host-wide build lock beside the per-box lock), since the cache is shared anyway.
- **The xtask went on**: after the Android box's failure it played the mesh box (the tour)
  instead of exiting. Right for a long run; the Android box then needs a second `test android
  groups storage` afterwards, which the summary should say in so many words.
- Disk: free space went from 314 GB this morning to 180 GB now. `podman system df`: 212 images,
  36 GB, 30.5 GB of it reclaimable (dangling `<none>` layers from Android builds, 2–6 GB each),
  and 39 GB in 35 containers. The box's `down --wipe` removes volumes by name but never the
  dangling images; a `cargo testbox prune` (or a note in the README to run `podman image
  prune`) would stop a week of runs filling a disk.


### What the wire comparison of the linked box says (`record/linked/wire/compare.md`)

- **The pre-key alarm is answered.** After each reconnect b33p does `PUT /v1/accounts/attributes`,
  then `GET /v2/keys?identity` and `POST /v2/keys/check` for both identities, and the checks pass:
  the keys the failed upload meant were already on the server, so presage's "should never
  happen" ERROR during the keepalive drop is about a redundant refresh. Not a bug in b33p; the
  log line overstates it.
- b33p sends `PUT /v1/accounts/attributes` on every connect (4 in this box; presage-cli 11),
  Desktop never (it sent `PUT /v1/devices/capabilities` once). Signal's clients set attributes
  when something changed. Harmless chatter, one request per reconnect.
- b33p's CDN requests carry `accept-language: en`; Desktop's and presage-cli's don't. reqwest's
  default header, and one more bit of fingerprint on the CDN for nothing. Easy to drop.
- Desktop's 11 `GET /stickers/{id}/manifest.proto 404` at 21:26:41 are eleven different pack
  ids, all at its first start: Desktop's built-in "blessed" packs, which the box's CDN0 doesn't
  have. Not about b33p's sticker, which Desktop showed. Seeding those packs (or expecting the
  404s in the comparison) would keep the table free of a known row.
- **A line from the previous box leaks into the next box's content log.** The Contents table has
  a sender `3b29ec83.2` beside `desktop` and `presage-cli`: that is the *baseline's* peer's
  Desktop, whose receipt (timestamp 21:18:50Z) arrived in the baseline's box after its record
  was taken (the last box stays up, b33p in it kept receiving). My run played from nothing at
  21:18:57Z but `target/testbox-run/content-b33p.jsonl` wasn't truncated, so the 29th line became
  the first of mine, with a login no client of this box has. Fix: remove the run directory's
  `content-*.jsonl` when a box plays from nothing, and have the attribution flag a login that
  belongs to no account the box made.
- Request counts worth a glance: b33p made 8 `GET /v1/websocket/` connects to Desktop's 6 and
  presage-cli's 14 in a box with two forced drops; 14 keepalives at a 30 s median, as Desktop.
  Nothing b33p does stands alone in the table except `GET /profiles/{id}` on CDN0 (its own
  profile picture fetch) and the provisioning socket.


### The mesh box and the tour (three b33ps, seed 1793482317)

- Setup 10 passed in 98 s (no Desktop, so no two-minute inbox); each b33p linked in 5.6–5.7 s.
- **Tour: 78 passed, 0 failed, in 381 s**, 68 steps of 34 kinds, each twice, with RTL, combining
  characters, emoji with ZWJ sequences and CJK in titles and texts, plus two kills and restarts.
- Delivery (a message showing on the other b33ps): median 0.49 s, p95 1.08 s, max 1.16 s over 30
  measured. Restart to usable: median 6.6 s, max 7.1 s (debug build).
- Step kinds by median: join-by-link 18.2 s, disappearing 16.1 s (by construction: "gone in 10
  s"), quote-then-edit 11.5 s, restart 11.4 s, create-group 11.0 s, add/remove member 8–9 s,
  story 8.5 s; everything message-shaped 2–5 s. Join by link at 18 s is the one to look at:
  it includes an admin turning the link on, approval, the request and the admission, each a
  group change b33p fetches through the log, so it is five round trips plus the model's
  waits, but it is the longest thing a user does in the tour.
- Memory (RSS): b33p 0 went 30 → 48 MiB (peak 73), b33p 1 31 → 39 (peak 52), b33p 2 31 → 61
  (peak 77); open files stayed at 47–54. The "didn't keep growing" checks passed. The peaks are
  after picture and story steps and come back down; six minutes is too short to tell a slow
  leak, which is what `stress-soak` is for.
- Harness answer time stayed under 27 ms at the worst sample (b33p 2), under 4 ms for b33p 1:
  the UI thread never stalled visibly under the tour's load.
- Content logs of the mesh box start at the box's own setup, so the previous-box leak above
  happens only when a box is *left up* after `test` and its b33p keeps receiving; the record's
  move of the file is otherwise fine.

- The other session's tour on the same tree, without `--screenshots --capture`: 79 passed in
  **167 s**, against my 78 in **381 s** with both. So screenshots (three 1280×900 PNGs per step,
  taken over the display sockets) and the tap more than double the tour. Fine for a record,
  but a nightly meant to measure b33p should run without them, or take screenshots only on a
  failed step.

Screenshots (`record/mesh/tour/*.png`, 1280×900 each b33p after each step) — two looked at:

- The final state of b33p 0 (`70-every-b33p-shows-every-chat…-b33p.png`): clean, readable; the
  audio bubble with Open/Save, the quoted reply, the group header with its member count all
  look right. Times are 21:3x: the containers run in UTC while the host is at 22:3x. A desktop
  user sees local time, so `TZ` in b33p's container (and Desktop's) would make screenshots
  match what people see, and catch a timezone bug if there ever is one.
- b33p 1 during step 66 (`67-…-b33p-1.png`): two things to verify by hand.
  1. The chat-list row of the RTL-titled group reads `tour 26: title, שלום עולם,` and stops
     without an ellipsis, while LTR rows end in `…`. Either the elision is dropped once bidi
     text is involved or the ellipsis is drawn at the logical end and lands off-row. Minor,
     but it's the kind of thing a tour screenshot exists to show.
  2. The reaction pill under "tour 3: after the blocked one" draws a small monochrome outline
     face, not a colour emoji, although Noto Color Emoji is in the image and the earlier emoji
     checks pass. Which emoji the step used (tour.json's step 14/19 `React`) and whether that
     glyph is a ZWJ sequence Qt's raster font path can't colour is worth a look.
  3. Wording: "Mesh 2 removed You." capitalises "You" mid-sentence in the list preview.


### The Android image, second and third try (22:39, 22:46)

- Second try, with the other instance's builds still running beside it: buildah logged `error
  unmounting container "": layer not known` twice more, but got through to a real failure this
  time: the Containerfile's revision check. `sdkmanager` takes no version for `emulator`, so
  the Containerfile asserts the revisions it was written against; Google has since moved the
  emulator from 37.2.12 to 37.2.13 (the cached layer after `sdkmanager` says `Pkg.Revision=37.2.13`;
  platform-tools 37.0.1 and the android-35 image r2 are unchanged). The check did exactly what
  its comment says it is for: a toolchain drift is a conscious bump, not a silent change.
- Bumped the one pin in the working tree (`testbox/android/Containerfile`, 37.2.12 → 37.2.13,
  uncommitted) and started the Android box again. Usability: the failure's message is the raw
  `RUN grep -qx …` line; a check that prints the revision it found beside the one it wanted
  ("emulator is 37.2.13, the Containerfile pins 37.2.12") would turn a ten-minute dig through
  cached layers into a one-line edit. Also worth knowing from the README: the Android image's
  first build will fail on this check whenever Google ships an emulator, which is monthly.

- Third try (22:46) ended at once on the box lock again: the other session had started `test
  android groups storage tour` on the default box from its `beep-baseline` worktree at 22:40,
  where it had committed the same pin bump independently (`44a22de`, emulator 37.2.13). Two
  sessions reaching the same one-line fix within minutes is a good sign for the check and a
  bad one for coordination: nothing on the host says "someone is already building Android".
  Watching that run's Android box; mine with `--screenshots --capture` follows when it frees
  the lock.

- The Android image, once past the pin: **1566 s** (26 min) in the other session's run, with
  the box's own cached stages and a host shared with two other pods. That is the fixed cost of
  a first Android box on a machine; afterwards it's cache.


### The Android box (the other session's run, from its log; mine follows with screenshots)

| part | checks | time |
|---|---|---|
| setup | 15 passed | 156 s |
| android | 9 passed | 9 s |
| groups | 25 passed | 100 s |
| storage | 12 passed | 44 s |

- Slowest: linking b33p to Android with the message history 26.9 s, "two changes Android makes
  while b33p is stopped show once it starts" 22.3 s, Android registering 16.9 s. Every group
  change made on Android (admin rights, removals, approvals) shows on b33p and Desktop in 8–9 s,
  which is the emulator's pace, not b33p's: the same changes b33p-to-b33p take 2–6 s in the tour.
- "Desktop shows its inbox" was 24.5 s here, not 120: in the Android box the setup links
  Desktop early and does Android's registration meanwhile, so most of the two-minute wait is
  hidden behind other work. The linked box could do the same (link Desktop first, then
  register presage-cli and link b33p while Desktop waits) and get its two minutes back without
  touching Desktop.
- Everything the groups phase added in #126 (changes read from the group's log, missed changes
  shown) passes on this host at first try, after the drivers' fixes; no Signal-Android crash in
  this run.


### My Android box, with `--screenshots --capture` (23:19–23:26)

| part | checks | time | other session's, without |
|---|---|---|---|
| android image (cached) | | 128 s | |
| setup | 16 passed | 180 s | 15 in 156 s |
| android | 9 passed | 20 s | 9 s |
| groups | 25 passed | 128 s | 100 s |
| storage | 12 passed | 65 s | 44 s |

- **Every check of every phase passed on main at `ad1b30c` plus the emulator pin**: linked box
  12 + desktop 17 + attachments 10 + core 6, android box 16 + android 9 + groups 25 + storage 12,
  mesh 10 + tour 78. Wire capture decoded all of it: 107 connections and 1053 events in the
  Android box (Android 371, b33p 303, Desktop 249, presage-cli 102), none undecoded.
- Even cached, the Android image "builds" for 128 s: thirteen stages' cache checks and the
  final stage's copies of the SDK, emulator and APK layers. Add it to the three and a half
  minutes b33p's image takes and a no-change Android box starts six minutes after the command.
- Two ERROR lines in b33p's log, both explained by the wire record and neither a fault:
  1. `libsignal_service::sender: could not create sync message from a group message` at
     22:23:05, in b33p's first group send with a sender key: two `PUT /v1/messages/{aci}` (the
     key distribution to peer and member), two `multi_recipient` sends, then one more
     `PUT /v1/messages/{own aci}`, which is the sent transcript, and Android showed the text (the
     check passed). ~~The ERROR is libsignal-service trying to make a transcript of the
     distribution message itself.~~ Corrected 2026-11-01 (see "The allow-list, read back"
     below): the content behind each such ERROR is a typing indicator to the group, not the key
     distribution; the line fires on every typing message b33p sends to a group.
  2. Two `websocket task ended … Connection reset without closing handshake` at 22:26:05: the
     box's last second. The record stops the tap and Signal-Server for their logs, which resets
     b33p's relayed sockets; b33p's log is collected after that, so the record's b33p log always
     ends in these. A reader who greps for ERROR meets them first. Collecting b33p's log before
     the tap is stopped (or marking the teardown in the log) would keep "ERROR in the record"
     meaning something.
- The same `trusting new identity` and `/nonexistent` D-Bus warnings as in the linked box; no
  `open_envelope` failure, no panic, no lost message.

### Contents: what b33p puts in a data message against what Desktop does

From the two `compare.md` files (Desktop as sender in the linked box, b33ps in the mesh box).
Fields Desktop fills that b33p never did in these boxes: `attachments.12` (blurHash),
`attachments.20` (incrementalMac), `body` as bytes (the same field, bytes when non-UTF-8-safe),
`isViewOnce`, `quote.7` (quote type). Fields b33p fills that Desktop never did here:
`expireTimer`, `groupV2`, `preview`, `quote.8`, `reaction.6`, `sticker` — all because the linked
box's Desktop checks never send those kinds (no group, no link preview, no sticker, no timer from
Desktop). So the comparison can only say "same fields" for the kinds both sides send; the
missing-from-b33p list is short and real: **no blurHash and no incrementalMac on b33p's
attachments**, which Signal's apps use for the blurred preview and for streaming
verification. `isViewOnce=false` and `quote.type` are defaults.

### Afterwards: a packet capture Wireshark decrypts (1 Nov, 00:29–01:50)

Asked for after the run: the box's traffic as a pcap, decryptable like Wireshark does with
exfiltrated session keys, instead of only the tap's plaintext. Built and verified on the default
box with `test desktop --capture`.

What was added:

- `testbox/tap/pod.yaml`: a `pcap` container (tcpdump on the pod's loopback, NET_RAW, the cdn0
  image with tcpdump installed) writing `capture.pcap` next to the tap's files, and
  `SSLKEYLOGFILE=/tap/keys.log` for the tap.
- `testbox/tools/src/bin/tap.rs`: rustls's `KeyLogFile` on both of the tap's TLS sessions per
  connection, the clients' side and the servers' side. 450 lines for 90 sessions, five per TLS 1.3
  session.
- `xtask/src/wire/pcap.rs`: the record's `wire/capture.pcapng`, pcap and key log in one file,
  the key log in a Decryption Secrets Block. Written as part of `wire::decode`, so
  `cargo testbox wire <record>` makes it again. Unit tests for the block layout and the frame
  rewrite below.

The run: images rebuilt in 69 s (cdn0 with tcpdump 12 s, b33p 42 s), linked box setup 12/12 in
140 s, desktop 17/17 in 7 s, 45 tap connections and 312 events as before, 16631 packets in the
record's pcapng, 12 MB. The pod stays up and tcpdump keeps writing to the moved file, so the
record's `raw/capture.pcap` grows after the record is taken (31564 packets by the third decode);
the tap's open connections do the same, by design. `wire/capture.pcapng` is whatever the decode
saw.

Two things the first pcapng got wrong, both found with tshark 4.4.19 in a throwaway Debian
container (no Wireshark on the host):

| | first pcapng | after the fix |
|---|---|---|
| TLS streams (client hellos) | 84 | 97 (the pcap had grown) |
| of them decrypted to HTTP, HTTP/2 or WebSocket | 40, all the tap's leg to the servers | 86; the 11 others carry no application data (the tap's `handshake eof` connections and their server legs) |
| bad TCP checksums with validation on | 1547, all in rewritten frames | 0 of 31564 |

1. **The redirect's NAT splits each client connection in two.** `tap redirect` rewrites the
   destination port in the output hook, so a client's packets reach the loopback with the tap's
   port (9444) in them, while the tap's replies are un-NATed before transmission and arrive from
   8444. Captured, they are two one-way TCP conversations, and Wireshark decrypts neither (no
   client hello on the reply side, no server hello on the client's). The pcapng writer now puts
   the port the client meant back into the client's frames, so the halves meet. The servers' leg
   is untouched and recognisable by its source, 127.0.0.2.
2. **Loopback checksums are offloaded.** The TCP checksum field of a loopback frame holds the
   pseudo-header's partial sum (the kernel never finishes it, and NAT leaves a partial alone);
   Wireshark knows the pattern and calls it good. Adjusting it for the port change (RFC 1624)
   turned it into a real wrong value. Now the checksum is recomputed only when it verified
   before the rewrite, and an offloaded one stays.

Verified in the final pcapng: Desktop's own leg (from ::1) shows `CONNECT /v1/websocket/` and
gRPC `GetDevices` in clear; b33p's and presage-cli's `GET /v1/websocket/` upgrades and their
WebSocket frames on both legs; no undecrypted stream has an application-data record.

Still open from this: the pcap is a lot bigger than the tap's files (attachments cross the
loopback twice, once per leg), and Wireshark's TLS heuristics find the box's ports on their own,
but anyone using `Decode As` should know 8443–8448 are TLS and 127.0.0.2 is the tap.

### Afterwards, second step: the servers' own key logs, the relay retired (1 Nov, 02:00–03:10)

Asked for next: the session keys from the servers themselves, Wireshark-style, rather than
from a relay in the path. Done, and the relay is gone; `--capture` is now tcpdump plus one key
log a server, and xtask decodes the pcap itself.

Where each server's secrets come from:

| server | TLS stack | key log |
|---|---|---|
| Signal-Server 8443, 8444 (Jetty) | JDK 26 JSSE | jSSLKeyLog 1.4's Java 24+ agent, by `-javaagent` when `SSLKEYLOGFILE` is set |
| Signal-Server 8445 (Netty, gRPC and Desktop's chat websocket over h2) | BoringSSL by default | `grpc.sslProvider: JDK` in the server's configuration under `--capture`, then the agent |
| storage-service 8448 | JDK 25 JSSE | the same agent |
| CDN0 and preview fronts (testbox/tools) | rustls | rustls's `KeyLogFile` |
| CDN3 (wrangler dev, workerd's HTTPS, logs nothing) | a new `front` tool, rustls, in front of wrangler on plain HTTP | rustls's `KeyLogFile` |

Two dead ends on the way, both costing a box each: `-Dio.netty.handler.ssl.noOpenSsl=true`
makes Signal-Server refuse to start (`SniMapper` calls `OpenSsl.ensureAvailability()` whenever
asked for OpenSSL, and the default is OpenSSL); and the first pod with the key logs didn't
parse, because the marker `# @CAPTURE_ENV <name>@` also matched a sentence in pod.yaml's own
header comment, and the broken rendered file then broke `podman kube down` on the next run too
(the teardown now goes on by the pod's name when `kube down` fails).

The decoder (xtask/src/wire `tcp.rs`, `tls.rs`, `capture.rs`): TCP put back together, TLS 1.3
opened with the key logs (ring's HKDF and AEADs, key updates, the three suites), the hello's
suites and schemes named as rustls names them so fingerprints stay comparable. Validated on the
relay-era record, which had both: the same 45 connections, the same 312 events, identical
headers, noted values, body kinds, shapes and sizes, request timings within 0.02 ms. One scheme
name was missing (`RSA_PKCS1_SHA1`), and two connections the relay called `handshake eof` the
capture calls `the handshake didn't finish`.

First box on the servers' keys (desktop, attachments, core): every server wrote secrets, 133
connections, 557 events, nothing undecoded, but Desktop's chat traffic dark: 9 sessions on 8445
without secrets (Netty's BoringSSL, above). With the provider switched: 49 connections, 343
events, Desktop's h2 websocket and gRPC decoded; tshark decrypts 40 of 40 TLS streams in the
pcapng. The only `failed` connections left are Desktop's idle preconnects, which never send a
byte (`nothing sent`), and under `core`'s server restart, client hellos no one answers that time
out after 10 s.

New checks, from the gaps the first day left (the Desktop driver gained `type_text`,
`wait_for_no_reaction` and `set_typing_indicators`):

| phase | check | first run |
|---|---|---|
| desktop | a reaction b33p takes back is gone on Desktop | passed, 0.4 s |
| desktop | a reaction Desktop takes back is gone in b33p | passed, 1.1 s |
| desktop | a long text from Desktop reaches b33p whole (2.6 kB, a `LongText` attachment) | passed, 0.8 s |
| desktop | Desktop's typing shows in b33p, and the text follows | failed twice: Desktop sent no typing message at all (the transcript shows only keepalives). A linked Desktop's typing-indicator setting is off until its primary syncs one, and presage-cli syncs none; the check now turns it on first, as the read-receipt checks do, and passes in 2.2 s. |
| attachments | a delete for everyone of Desktop's picture removes it in b33p | passed, 0.6 s |
| core | text the peer sends while b33p is down arrives when it starts again | passed, 4.5 s |

With that, the linked box's three phases on the servers' keys: 12 + 21 + 11 + 7 checks, all
passed, 129 connections and 754 events decoded, nothing undecoded (03:16).

The box lock, the pods and podman's pause process were lost to a relogin once; nothing in the
tree, and the images survived.

### Working the observations in (1 Nov, 03:20 onwards)

Done from the lists below, while the two rounds run:

- **"No unexpected ERROR" as a check** (deepen): `b33p logged no unexpected error`, at every
  phase's end beside "no client crashed" (xtask/src/checks/errors.rs). It reads every b33p's
  log for ERROR lines, tracing's and cage's, and fails on any kind not on its list of known
  ones: cage's missing Xwayland, the websocket and receiving-loop errors when the server side
  goes, libsignal-service's sync-transcript failure on typing indicators to groups, presage's
  pre-key alarm. Dry-run over every record on disk: 64 ERROR lines, none unexpected. New in
  kind is what it will catch. (The list's first version named the pre-key alarm by a wording
  presage doesn't use, and carried a "keepalive failed" that nothing logs; both corrected on
  reading the list back, below.)
- **Content logs from an interrupted run** (tool): a fresh box now removes leftover
  `content-*.jsonl` from `target/testbox-run` before it starts, so a record never takes a
  previous run's lines for its own.
- **Revision checks that say what they found** (tool): the Android image's pin check prints the
  three `Pkg.Revision` lines when one moved, instead of a bare failure.
- **cargo's `redundant_homepage` warnings** (tool): gone, with the three `homepage.workspace`
  keys.
- **The Android driver's stale node** (found by round 1): "Android writes a note to itself"
  failed with `node 45 went`, the agent's word for an accessibility node that vanished between
  the look and the tap (Compose redraws on its own schedule). `tap` and the message field now
  look once more when that happens. Round 1 lost the whole Android box to it; round 2 will say
  whether it comes back.
- **No new pull requests** since #130 (checked by the remote's `refs/pull/*` refs: the GitHub
  CLI's token expired with the relogin; `gh auth login` would restore it).

**The two rounds** (`run --rounds 2 --capture`, every phase, 03:17–03:37, records in
`target/testbox-record/run/1793499434/`):

| round | linked box | desktop | attachments | core | android box | mesh box | tour |
|---|---|---|---|---|---|---|---|
| 1 | 12/12, 143 s | 21/21, 23 s | 11/11, 14 s | 7/7, 101 s | 9/10, 57 s: "Android writes a note to itself", `node 45 went` | 10/10, 14 s | 78/78, 112 s |
| 2 | 12/12, 135 s | 21/21, 9 s | 11/11, 12 s | 7/7, 141 s | the same, at the same 3.9 s | 10/10, 15 s | 78/78, 110 s |

Not a flake, then: the same node at the same moment twice, where yesterday's Android box (with
`--screenshots`, which pause between checks) passed. Without the pauses the tap after "New
chat" comes while Compose is still redrawing. The driver's retry above is the answer; it goes
into the Android image's next build. The android, groups and storage phases didn't run in
either round for it. Everything the wire decoded: 129 and 127 connections in the linked boxes,
785 and 748 events, 38 connections in each Android box setup, nothing undecoded, nothing
unexpected in b33p's logs.

**After the retry and the new check** (`test desktop android --capture`, 03:41–03:49, the
Android image's driver stage rebuilt in 475 s): linked box 13/13, desktop 22/22, Android box
17/17 with "Android writes a note to itself" passing in 5.6 s, android 10/10; "b33p logged no
unexpected error" passed at every one of the four ends. The wire: 47 connections and 341 events
in the linked box, 74 and 432 in the Android box (Android 167 of them), nothing undecoded.
Then `test groups storage --capture` (03:52–04:00): Android box 17/17, groups 26/26, storage
13/13, 98 connections and 927 events decoded. With that, every phase has passed on the servers'
own key logs with the new checks in: linked 13, desktop 22, attachments 11, core 7, Android box
17, android 10, groups 26, storage 13, mesh 10, tour 78.

## Opportunities to broaden and deepen the coverage

`pending/testbox-review.md` already lists the user flows the phases miss (long text, typing
with Desktop, read receipts to Desktop, disappearing with Desktop, forwarding, link previews with
Desktop, blocking via the storage service, invite links with Android, safety-number change,
unlink and relink, SIGKILL while receiving, a CDN that drops mid-upload). Everything below is in
addition to that list, from what this run showed.

### Broaden: what the box never puts in front of b33p

1. **b33p's own account seen by a Signal client.** Desktop always links to the *peer*; no
   Signal-authored client is ever on b33p's own account. So b33p's sync transcripts, read syncs,
   view-once syncs, delete syncs, its storage-service writes and its "sent" story transcripts
   (#125) are only ever read by presage-cli or another b33p. A linked box variant where Desktop
   links to b33p's primary would check the half of the protocol a second device of one's own
   exercises, which is exactly what a real user has.
2. **A tour with Signal's clients in it.** The tour is three b33ps; its 34 step kinds are only
   ever checked b33p-to-b33p. The drivers already exist; letting the model's "others" include
   Desktop and Android for the kinds the drivers can read (text, edit, react, reply, delete,
   attachment, group changes) would multiply the cross-client surface at no new driver cost.
3. **Notifications and the single instance.** The container has no session bus, so b33p-notify
   and the already-running check are dead code in the box (two WARNs per start). A
   `dbus-daemon --session` plus a notification-capturing stub lets checks assert "a message in
   an unselected chat notifies; a muted chat doesn't; the reply from the notification sends".
4. **Android as a story poster and transcript author** (open since #125): the content layer
   can't compare b33p's story transcripts with Android's until the Android driver posts one.
5. **A second b33p device of the same account across a restart with history**: the Android
   box checks "b33p links to Android, with the message history" once; relinking after an unlink
   (TESTPLAN §5.8) and a history transfer of a *large* account (hundreds of chats, thousands of
   messages, big attachments) is what the first start of a real user looks like, and nothing
   in the box has a chat longer than a dozen messages. A seeded-history box (presage-cli
   writing N messages before linking) would also give the history/scrollback and
   search checks (TESTPLAN §1.9, §2.2) something to scroll.
6. **Locale, scale and theme.** Everything runs in `en`, UTC, scale 1, light theme, 1280×900.
   The tour's texts cover scripts; the window doesn't cover its own variants: a `de_DE` run,
   `TZ=Europe/Berlin`, `QT_SCALE_FACTOR=2`, dark theme, and a narrow window (TESTPLAN's
   layouts) are environment variables away, and the screenshots would show what a user there
   sees.
7. **Accessibility.** TESTPLAN §2.14 (Orca) has no automated counterpart; the harness snapshot
   could carry accessible names and roles so a check can say "every chat row has a name".
8. **Message requests from unknowns, blocked senders, and spam-shaped traffic**: the setup
   accepts the peer's first message as a request once; a request *declined*, *blocked*, and
   one answered on the phone (TESTPLAN §2.7) aren't checked, and neither is a message from a
   blocked contact arriving while blocked (the tour does block/unblock between b33ps only).

Added 1 Nov, from the day's work:

9. **A long text from b33p to Desktop, whole.** The Desktop side of the new long-text check is
   missing: Desktop folds a long message behind "Read more", so a wait for the full text never
   matches. A driver command that clicks "Read more" in the message and then matches the tail
   would close it, and would say whether b33p sends the `LongText` attachment at all.
10. **Typing indicators from Android**, and b33p's typing on Desktop and Android (§1.3 items 2
    and 3): the Android agent's calls are low-level, but a `SetText` into the composer is a
    typing start, and Desktop shows "<name> is typing…" in the conversation header.
11. **Each client's TLS the real way.** With the relay gone, the capture shows what Desktop,
    Android and b33p really negotiate with Jetty, Netty and rustls (suites, versions, resumption,
    tickets). A comparison section on it would catch a client falling back to TLS 1.2 or losing
    session resumption.

### Deepen: make what passes say more

1. **Assert on the content, not only on arrival.** The content log now records which fields
   b33p fills; turn the comparison into checks: an attachment from b33p carries `blurHash` and
   `incrementalMac` like Desktop's (today it doesn't; see above), a quote carries its type, a
   reaction its target author. Each is a one-line assertion on `content/*.jsonl`.
2. **Pre-key hygiene across a dropped upload**: after the keepalive-drop check, assert that the
   next connect's `POST /v2/keys/check` succeeds and that one-time pre-key count is replenished
   within N minutes; it turns the "should never happen" ERROR into a checked state.
3. **Per-check host load in the record.** `load.txt` is taken once at the end. Sampling
   `/proc/loadavg` and `/proc/pressure/*` with each check (a column in `spans.jsonl`) would
   let anyone tell a slow b33p from a busy host, which this run couldn't: three boxes and a
   Rust build shared the machine.
4. **Timestamps in the build logs**, and the `wire` events' request latency (server-side time
   from request to answer) as a column: then "profile picture 9.4 s" decomposes into CDN
   upload, profile write, and b33p's fetch.
5. **Expected-noise lists for the comparison.** *(Done for b33p's log, 1 Nov: the check
   "b33p logged no unexpected error", above.)* Desktop's 11 blessed-pack 404s and cage's
   Xwayland ERRORs appear in every record; an allow-list with a reason, and a check that the
   *rest* of the ERROR lines is empty ("b33p logged no ERROR it didn't expect"), makes the log
   level mean something again. The tour already has "logged no panic"; "logged no unexpected
   ERROR" is the next rung.
6. **Harness snapshots beside screenshots** (also in testbox-review.md): with them, the two
   rendering questions above (the RTL row's missing ellipsis, the monochrome reaction glyph)
   could be answered from data instead of pixels, and a screenshot diff against the last green
   run would flag visual regressions.
7. **Optimized and small-machine runs on a schedule.** `--optimized` and `--b33p-memory` exist
   but nothing runs them; the nightly runs the debug tour only. Restart time (6.6 s median
   here) and memory peaks (77 MiB) are debug-build numbers; a weekly optimized tour and a
   `--b33p-memory 256` soak would give real ones and catch a leak the six-minute tour can't.
8. **More seeds, longer tours.** The tour did 68 steps from one seed, each kind twice. Running
   the nightly with `TESTBOX_TOUR_REPEAT=5` and a new seed each night is free coverage of
   orderings; the record already keeps the seed to replay a failure.

### The tool itself (usability found on the way)

1. `cargo testbox doctor`: podman present and rootless, `tun`/`bridge`/`veth` loadable for the
   *running* kernel, `/dev/kvm`, subuid, disk, and whether another box's build is in flight.
   Both of today's first failures (missing podman, modules gone after a kernel update) would
   have been a one-line answer.
2. A host-wide build lock across instances: image stages are shared cache, and three instances
   building at once crashed buildah (`layer not known`, nil dereference) once and logged it
   twice more.
3. *(Done, 1 Nov.)* Clear `target/testbox-run/content-*.jsonl` when a box plays from nothing, so a box left up
   can't leak a line into the next record; flag any content login that belongs to no account
   the box made.
4. `cargo testbox prune` (or a README line): dangling Android layers ate 30 GB today;
   free space fell from 314 to 119 GB in a day of runs across sessions and three instances,
   and three pods are still up at the end (`b33p-testbox`, `.split`, `.pipeline`) with 39 GB
   of container layers between them.
5. Revision checks that print what they found ("emulator 37.2.13, pinned 37.2.12"), and a
   README note that the Android image's pins drift monthly.
6. `TZ` for the clients' containers, so screenshots and logs read like a user's desk.
7. The summary after a run with a failed box should say what to run next
   (`cargo testbox test android groups storage`); today it printed the build log's tail and
   left the reader to work out that the other boxes had passed.
8. *(Done, 1 Nov.)* Silence cargo's `redundant_homepage` warnings (drop `homepage.workspace` in the two crates):
   they are the first screenful of every `cargo testbox` invocation.

## The allow-list, read back (1 Nov, late morning)

Each of the unexpected-error check's known lines, traced to the code that emits it, with
`git log -S` in the presage and libsignal-service clones and the pinned checkouts
(presage 53fb55a, libsignal-service 0f9454b). Across every record on disk the client logs hold
139 ERROR lines of 12 shapes, which fold to 6 kinds. b33p's own crates emit none: its receive
loop logs the same events at INFO and WARN.

1. **`Cannot find Xwayland binary` / `Cannot create XWayland server`** (38 each, two per
   client start). Debian's cage 0.2.0 depends on the xwayland package; both client images
   install it and then `rm /usr/bin/Xwayland`. cage has no runtime switch (its options are
   `dhm:sv`, Xwayland is a build-time define), so it logs the pair and runs on. The lines are
   the images' own doing. Either keep Xwayland, or build cage without it; the allow-list is
   the third-best answer.
2. **`libsignal_service::websocket: websocket task ended`** (30). Ours: fork commit 710b11fa1
   added an `inspect_err` in `SignalWebSocket::new`, "logged as well as returned: the socket's
   users only see `WsClosing`, and whoever holds the task may never ask". That is a library
   logging at ERROR because its error has no path to its owner. Twice per drop, since the
   identified and unidentified sockets both end.
3. **`presage::manager::registered: unexpected error in message receiving loop`** (15,
   upstream wording since presage #274). The same drop, seen by the one who does hold the
   task: presage's stream gets `Some(Err(close reason))`, logs it, `continue`s, and the next
   poll ends the stream with a bare `None`. b33p then logs "connection to Signal closed" at
   INFO and reconnects with backoff. One event, three lines at three levels, and the two
   ERRORs come from layers that cannot know whether the application minds. Their occasions
   here are all deliberate: `core`'s server restart, the kills in `core` and the tour, and the
   record itself, which stops the server before it takes the clients' logs, so every record
   ends with a pair of them. The fix is structural, not a level change: libsignal-service
   returns the reason (it already does) and logs at debug; presage ends its stream with the
   reason instead of `None`; the application logs once, at the level it chooses. The record
   could take the clients' logs before it stops the server.
4. **`libsignal_service::sender: could not create sync message from a group message`** (17).
   Upstream. `create_multi_device_sent_transcript_content` returns `None` for anything but a
   data or edit message. whisperfish's 269976b58 ("don't try to sync invalid messages") taught
   the one-to-one path that ("not getting a content body to sync is not an error", silent) and
   left the group path's `error!`. What b33p sends to groups that isn't a data message is a
   typing indicator (`send_typing` → `post_group_message`), and every one of them logs this:
   of the 7 occurrences in records with a content log, 4 have a content whose first protobuf
   field is 6, `typing_message`, and 3 were sends with `recipients=0`, which no one received.
   The 31 Oct note that blamed the sender-key distribution was wrong (that goes through
   `try_send_content`, which syncs nothing); corrected above. A two-line upstream PR: decide
   syncability before the send, or return the `None` as quietly as the one-to-one path does.
5. **`presage::manager::registered: failed to register pre-keys, this is problematic and
   should never happen!`** (1, upstream since #343). The refresh task that each
   `receive_messages()` spawns beside the stream sets the attributes and registers pre-keys
   over the identified socket, then pends forever so that `take_until` never fires; its
   errors have no consumer but the log. "Should never happen" happens whenever the socket
   drops within the first seconds of a connect, which `core`'s keepalive-drop check does on
   purpose; the next connect runs the task again, and the wire showed `POST /v2/keys/check`
   passing. A fire-and-forget task whose failure nobody reads, so it shouts. The list's first
   version named it "failed to upload pre-keys", which would never have matched; the check
   would have failed on the next such flake. Fixed.
6. **`keepalive failed`**: nothing logs it. libsignal-service's keepalive line is a WARN
   ("Websocket will be closed due to failed keepalives."). The entry was a guess; removed.

Two lessons for the check itself: an allow-list entry nothing matches is a bug the check
can't see, so a run should report which known kinds it saw and which it didn't; and the list's
comments must come from the emitting code, not from the first explanation that fit.
