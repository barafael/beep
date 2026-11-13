# Onboarding the presage family into the testbox, 2026-11-13

Branch `worktree-agent-a2c243c6025d84365`, box instance `fam`. The question: which other clients
run on presage / libsignal-service-rs, how far each is from b33p's pin, what the smallest change is
that lets each talk to the box, and what the box then says about them and about b33p.

## Who runs on what (read from the lock files, 2026-11-13)

| Client | Where | presage | libsignal-service-rs | libsignal | Behind b33p's fork (presage / lss) | Verdict |
|---|---|---|---|---|---|---|
| **gurk-rs** 0.10.2 (`49f4295`) | github.com/boxdot/gurk-rs | whisperfish `94b3978` | whisperfish `fb92b13` | v0.99.0 | 32 / 31 commits | onboarded (tmux) |
| **Flare** 0.22.8, backend `93a6331` | gitlab.com/schmiddi-on-mobile/flare-backend (`flare-manager`, `flare-store`, `flare-types`); the GTK app is a thin layer over it | whisperfish `290effe` | whisperfish `7a75267` | v0.99.0 | 15 (+4 not in the fork) / 27 | onboarded as `flare-box`, a one-shot driver on the real manager; the GTK UI is not run (below) |
| **siggy** 1.15.0 | github.com/johnsideserf/siggy | default build: **signal-cli** JSON-RPC (no presage at all); opt-in `native-backend` feature: whisperfish `63482ef` | `fd48165` | v0.99.0 (presage's) | – / 66 | not onboarded: default backend belongs with signal-cli; the native one is experimental, opt-in, absent from releases (crates.io builds are signal-cli only) |
| **Whisperfish** 0.6.0-dev | gitlab.com/whisperfish | no presage: `libsignal-service` straight, branch `main` | – | v0.99.0 | – | not onboarded: Qt (qmetaobject, cpp) everywhere, no headless binary (`bin/` is create-store, storage_key, migration dry-run), diesel store of its own, 4.8k-line actor in `worker/client.rs` with the server chosen by `SignalServers` there; running it means a Qt build and a Qt event loop, a day's work |
| purple-presage, sigwin | presage | – | – | – | – | not looked at; same family, same patch |

None of them is independent of libsignal-service-rs. Flare is not an exception: `flare-manager`
calls `presage::Manager` for linking, registering, receiving and sending, and keeps its own store
beside presage's (`flare-store`, a wrapper over `presage-store-sqlite`).

All of them are on **libsignal v0.99.0**, as b33p: what differs is libsignal-service-rs and
presage, which is where b33p's fork carries its work (sender keys for groups, the session lock, ack
handles, keepalive, group changes from the log).

## The smallest patch

b33p's lss fork is a descendant of both clients' lss revisions (`git rev-list --left-right
--count`: 0 behind), and carries `SignalServers::Custom` in one commit, `88016e929` ("servers of
one's own"). That commit alone (`testbox/family/lss-custom-servers.patch`, 5 files, 250 added
lines of which 100 are tests) applies **cleanly** to gurk's and Flare's lss revisions. It makes
`SignalServers` no longer `Copy`, which costs presage three `.clone()`s
(`testbox/family/presage-servers-not-copy.patch`, the same three lines at both presage revisions).
Nothing else in either client's dependency tree needed touching; Flare's backend compiled
unmodified, gurk needed its one `SignalServers::Production` replaced (`GURK_SIGNAL_SERVERS_FILE`,
and a `println!` of the link URL, as gurk shows the link only as a QR code:
`testbox/family/gurk-testbox.patch`).

Cost grows with age: the same commit fails one of three hunks on siggy's lss (`fd48165`, 66
commits behind).

## Where it is run

`testbox/family/Containerfile` builds both on their own revisions (`pin` fetches each repository at
one revision, the patches go on with `git apply`, the lock files are ours: `gurk.lock`,
`flare-backend.lock`, builds `--locked`), `testbox/family/pod.yaml` adds the `family` container
(`cargo testbox up --family`), `xtask/src/checks/family.rs` has the phases `family-gurk` and
`family-flare` (only when named, each on a box of its own).

