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

```sh
cargo testbox --instance fam test family-gurk family-flare --capture   # both phases, the wire decoded
cargo testbox --instance fam up --family                                # a box with the clients, to look at by hand
podman exec b33p-testbox.fam-family flare-box /presage/family-flare.db sync
podman exec b33p-testbox.fam-family tmux capture-pane -p -J -t gurk     # what gurk's screen shows
```

## What the phases check (2026-11-13, `main` 806c673 + this branch, debug b33p, instance `fam`)

Last run (run 7, then run 8 for a repeat, below): **family-gurk 22 of 22, family-flare 25 of 25**,
34 s and 33 s of checks, each on a box from nothing, 0 connections of 154 and 176 not wholly decoded.

| | family-gurk | family-flare |
|---|---|---|
| setup | b33p linked to a presage-cli primary, a peer registered; gurk linked as a secondary device of a third presage-cli primary (`gurk --relink`, the link URL its patch prints, added by presage-cli `add-device`) | the same; `flare-box register` (Flare's own register path, through `flare_manager::Manager`) then `set-profile` |
| text | peer → gurk, gurk → peer, account of gurk → b33p (a request, accepted), b33p → gurk, gurk → b33p | the same with Flare; plus a group b33p makes of the peer and Flare: b33p → group reaches Flare, Flare → group reaches b33p and the peer |
| load | 20 messages from the peer in a burst, once each, in order, on gurk's screen | the same, by flare-box's printout |
| race | gurk types 10 messages while the peer's presage-cli sends 10 to it, at once: each side shows every one once, in order (the peer judged by its store) | 6 rounds of Flare and the peer sending each other one message at once (one-shot flare-box receives as it sends, as presage-cli does): both stores have every one, once; the peer's and b33p's sends to Flare at once |
| crash | gurk's terminal closed (SIGHUP) in the middle of a 20-message burst, started again: nothing lost, nothing twice; also 5 sent while it is down | flare-box SIGKILLed 4 s into a 20-message burst, then synced: its store has each once, in order |
| end | gurk still running, its log has no `ERROR` line; no client lost a message; none crashed; b33p logged no unexpected error | the same, less the log |

Not covered: attachments, receipts from the family's side (presage-cli sends none; gurk's `○`
shows sent only), edits, reactions, sealed-sender-only senders, the storage service (neither
client reads it; b33p does: `GET /v1/storage/manifest` is b33p's alone in the capture), group
*creation* by the family (neither client has it), long runs.

### Failures on the way, each with its cause (none was in the box's server, b33p or the clients)

1. **Flare's group checks failed in the first run** (2 of 21). Cause: `flare-box register` left the
   account without a profile; b33p's log says `no profile key credential; inviting`, so Flare was
   a *pending* member, and group messages go to members only (b33p: `dropped a message from
   someone not in the group`). presage-cli's accounts get `update-profile` after registering; Flare's
   manager has no profile to set (`flare-manager` never calls `update_profile`), so an account
   Flare registers can't be added to a group outright, only invited. `flare-box set-profile`
   (presage's own `update_profile`, as presage-cli) fixes the box, and is a finding for Flare: a
   Flare that registers as a primary has no profile until the user sets one elsewhere.
2. **gurk "didn't end within 30 s of its terminal closing"**. Cause: gurk did end (SIGHUP); it is
   a *zombie* (`State: Z`, `PPid: 1`): tmux's server exits with its last session, gurk is
   re-parented to the container's first process (`sleep infinity`), which reaps nothing. The check
   now counts a zombie as gone. Same class as the pod's other long-lived `sleep infinity`
   containers; the family container, if it ever runs more processes, wants an init.
3. **Burst and race checks failing on "0 times"**: the checks' own. A `presage-cli send` and a
   `flare-box send` receive while they send and print what they received (that is why presage's
   own comment says "not only `sync` receives"); judging by one `sync`'s printout loses those.
   The checks now read the stores. Both stores list a thread newest first (presage-cli
   `list-messages`, flare-box `messages`): the "out of order" failures were that.
4. Wire attribution: with presage's default `User-Agent` (`presage-rs-0.8.0-dev`) gurk and Flare
   were counted as **b33p** (a linked device of presage's) and presage-cli. The family's images
   set their own agent (`gurk/0.10.2 presage-rs`, `flare-box/0.1 presage-rs`, one `sed` on
   presage's `USER_AGENT`) and `xtask/src/wire/mod.rs` names them.

### Observations

- **gurk gates its whole receive loop on `TcpStream::connect("detectportal.firefox.com:80")`**
  (`src/main.rs` `is_online`). Without internet (or with that host blocked, or DNS down while Signal
  is up) it never connects to Signal at all, silently, retrying every 10 s; and it tells Mozilla's
  host that gurk is running. The box needs a patch (the probe answers yes when the servers
  file is set). Not b33p's problem, but the shape (a connectivity probe of a third party gating the
  real connection) is worth never copying.
- **Flare's echo of a message it just sent carries the wrong thread** for a 1:1 chat: flare-box
  printed `To <Flare's own ACI>` for a text sent to the peer, because the `Content` presage's
  `send_message` returns is read by `Thread::try_from(&Content)`, whose last arm is
  `Contact(content.metadata.sender)` (presage `store.rs`); presage saves the message under the right
  thread (`save_message(.., Some(thread))`) and Flare's store lists it there (`flare-box messages
  <peer>` shows it, the self thread is empty), but the `on_message` callback that tells a UI to
  show it names the sender's own thread. Whether Flare's GTK UI papers over it was not run. Seen
  on one send; not asserted by a check.
- **What a presage send prints**: see 3 above; b33p's harness-based checks don't have this.
- **The family says less than b33p, and nothing different**: against b33p, in the capture
  (`compare.md` of both records): the same TLS client hello for every one of them (one fingerprint,
  rustls), identical `POST /v1/verification/session` → `PATCH` → `/code` → `POST /v1/registration`
  bodies for presage-rs's (the unauthenticated socket, i.e. registrations and sealed-sender sends
  of gurk, Flare and presage-cli alike), `PUT /v1/accounts/attributes` with the same capabilities.
  The differences are b33p's own: `accept-language: en` and `x-signal-receive-stories: true` on its
  sockets (the family sends neither, stories `false`); sender keys for groups
  (`PUT /v1/messages/multi_recipient`, 4 in the Flare box, none for the family: they send a group
  message to each member, the fork's `0f9454bf3`); the storage service (`GET /v1/storage/auth`,
  `/v1/storage/manifest`: b33p only; gurk none; Flare's `GET /v1/groups/` on the storage port only);
  gurk's `GET /v1/keepalive` request over its sockets (3 in 34 s; b33p's keepalive is the fork's
  websocket-level one, `ab23dc470`, and shows no `GET /v1/keepalive`); envelope types differ only
  where the session state does (`type=1` from gurk, a session that is already open; b33p's `type=3`
  pre-key messages in the same box).
- **Chatty by design, one-shot**: flare-box starts a receive loop each time; each start costs a
  websocket, `GET /v2/keys?identity`, `POST /v2/keys/check` and `PUT /v1/accounts/attributes`
  (176 connections for the Flare phase). That is presage at `290effe` on every `receive_messages`,
  not Flare's usage (a running Flare connects once).
- A `GET /v2/keys/{aci}/*` that b33p makes was answered with `preKey: null` (no one-time EC
  pre-key) where the same request by gurk and presage-cli got one. Not investigated.

## Cost and where it is fragile

- Image: `family` builds in about 3 minutes warm (7 cold), both clients from source, `--locked`.
- Fragile: the lss patch is one commit of b33p's fork and applies cleanly to both clients' lss
  revisions *today*; it fails a hunk on siggy's (66 commits behind) and will on gurk's and Flare's
  as they move. The lock files (`gurk.lock`, `flare-backend.lock`, 300 KB) have to be refreshed
  with them, the way `presage-cli.lock` is. `xtask/src/pins.rs`' drift test guards b33p's pin, not
  these.
- Flare's GTK application is not run: its server list is `Production` and `Staging`
  (`src/gui/setup_window.rs`), its store password comes from the Secret Service over D-Bus
  (`oo7`, `backend/manager.rs` `encryption_password`), it has no scriptable surface, and what it
  adds to `flare-manager` is GTK widgets. flare-box runs `flare-manager` itself, so the receive loop,
  message conversion, the store and register path are Flare's; what is not exercised is its UI and
  the keyring-encrypted store (flare-box opens it without a password).

