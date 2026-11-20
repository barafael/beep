# The storage manifest, fetched again and again (finding 8 of the sceptical run, 2026-11-11)

In the `storage` phase of the capture run, Signal-Android wrote the manifest 55 times and b33p made
55 full `GET /v1/storage/manifest` calls, 66 `PUT /v1/storage/read` and 83 `GET /v1/storage/auth`;
Android and Desktop each fetched the manifest once. This is what produces them, what is fixed, and
what is left.

## Where the requests come from

For each message from the phone that says "fetch the manifest again" (`SyncMessage.FetchLatest`,
44 in that run; the phone sends one per record it writes), b33p downloads the new manifest **three
times**, from three places:

1. **presage's receive loop**, at once (`presage/src/manager/registered/receive.rs`, the
   `FetchLatest` branch, calling `sync_groups_from_storage_service`): `GET /v1/storage/manifest`
   (the unversioned one, always the whole manifest) and a `PUT /v1/storage/read` of **every** group
   record, to fetch groups b33p doesn't know. Independent of b33p; before this, b33p-core could not
   influence it. It is the bulk of the 55.
2. **b33p's own sync** (`follow_storage`, `storage::sync`), two seconds after the first message
   (`SETTLE`): the versioned `GET /v1/storage/manifest/version/{n}` (204 if unchanged, the whole
   manifest if not), then a read of the records that changed.
3. **b33p's own write** (`storage::push`), straight after each sync: it asked the service for the
   **unversioned** manifest to compare versions *before* looking at whether it had anything to
   write. Usually it has not (the change came from the phone), so this was a whole manifest
   downloaded to be thrown away.

Evidence (the wire of `test storage --capture`, `beep-storage/target/testbox-sc2-record/android/wire`
and the baseline in `beep-sceptic/target/testbox-sc1-record/android/wire`): each fetch-latest
message is followed within 0.1 s by an unversioned GET, 2 s later by the versioned one and a read.
`push` is the only caller of the unversioned call in b33p-core; the presage branch above the other.

## Measured (b33p's own requests in the `storage` phase, one run each; Android writes the manifest
about the same number of times)

| | baseline (806c673) | b33p-core fix | b33p-core fix + presage fix |
|---|---|---|---|
| `GET /v1/storage/manifest` (whole, unversioned) | 15 | 13 | **2** (b33p's own two writes) |
| `GET …/manifest/version/{n}` answered 200 (a whole manifest) | 10 | 9 | 20 |
| `GET …/manifest/version/{n}` answered 204 (nothing) | 5 | 3 | 3 |
| manifest bodies downloaded in all | 25 | 22 | 22 |
| `PUT /v1/storage/read` | 12 | 11 | 11 |
| `GET /v1/storage/auth` | 30 | 25 | 25 |
| `PUT /v1/storage` (b33p's writes) | 2 | 2 | 2 |
| b33p's requests in the box in all | 145 | 134 | 133 |

All three runs: `storage` 14 of 14 checks passed (and the box's setup and the end-of-part checks).

So the two fixes remove the *unversioned* calls (15 to 2) and the reads b33p's own write made to
no purpose, but **not the duplication**: the phone's change makes a new manifest, and presage's
read and b33p's sync each download that new manifest in full (20 of 200 answers in the last column).
The number of whole manifests downloaded fell by about an eighth, not by two thirds. For a few
records that is nothing; for an account with thousands of records every phone change still costs
two whole-manifest downloads where one would do.

## What is changed

b33p-core (an uncommitted worktree then, committed to main on 2026-11-20):

- `storage::push` works out what it would write from b33p's own tables (`Plan`) **before** it asks
  the service anything; nothing to write makes no request. It also remembers a write that found no
  record for its changes (b33p edits records, it does not create them: a chat the phone keeps no
  record of stays "changed" for good), by the manifest version it tried and the plan
  (`Unplaced`), and does not try again on the same version with the same plan.
- `follow_storage` waits out a burst of change notices (`settle`): until nothing has said "changed"
  for 2 s but no longer than 10 s in all, so that the phone's run of writes is read once at its end
  (a debounce on the `Notify`). Three tests with paused time (`tokio` `test-util`, a dev
  dependency), and three on `Plan`/`Unplaced`.
- Clippy `-D warnings`, `cargo fmt --check` and the 16 storage tests pass.

`docs/storage-manifest-2026-11-11-presage.patch`, against presage 8057f23 (not pushed, not b33p's
pin): `sync_groups_from_storage_service` keeps what the last read saw (`StorageGroups`: the manifest
version and the group identifiers read) in the receive loop, asks `manifest_if_changed` (204 for
nothing new) and reads only group records it has not read. A failed read leaves the version unmarked,
so it is tried again. Built and tested here through b33p's workspace with a path override (a copy in
`vendor/presage`, since removed), because the box builds presage by git revision: using it in the
testbox, or in b33p, needs a commit to `barafael/presage` and a new pin in `Cargo.toml` and the
four box files (`xtask/src/pins.rs` checks them). **I did not push anything to your fork.**

## What is left, and the "watch channel" way to do it

Both consumers want the same thing: the newest manifest, once. The shape that gives one download
per phone change is a single task that fetches it when notified (b33p's `follow_storage`), publishes
`(version, manifest)` on a `tokio::sync::watch`, and lets the others read the value: b33p's settings
sync (already there), and the group discovery presage does in its hook, which would have to take its
manifest from outside (a callback or an injected provider on the manager) or be switched off and done
by b33p after its own read: its sync already decrypts the group records, it only lacks the "fetch a
group this account has that we don't know" step (`save_unknown_group`, private in presage). Either
needs a change in presage's API, so it is for you to say whether to go there; a `watch` inside
b33p-core alone cannot stop presage's own download.

## Not verified

- Only the `storage` phase's traffic was compared (one run per column). The phone's writes in
  `groups` and the rest of the Android box were not repeated after the fix (the full-run count of 55
  has not been rerun).
- The `Written::Stale` path (a write that finds the phone changed meanwhile) and conflicts (409)
  have no new test and were not provoked; the phase passes, which exercises only the common case.
- The debounce delays b33p's reading of a phone change by up to 10 s while changes keep coming
  (2 s when they stop); nothing in the phases looked at that latency beyond their patience.
- The presage patch drops the repeated read of groups known already; a group the phone lists
  that fails to fetch is retried only when the manifest changes again (or on restart), as before
  for the unauthorised case; no test for it in presage.
