# Actors in b33p-core

A plan to move b33p-core's signal backend from tasks that share state to actors that own it,
after [More actors with tokio](https://barafael.github.io/posts/more-actors-with-tokio/). The
first one, `Sending` (b33p-core/src/outbox.rs, #60), is done; the steps below take the rest one
pull request at a time. The changes this needs in presage and libsignal-service are proposed in
[presage-sessions.md](presage-sessions.md) and
[libsignal-service-sessions.md](libsignal-service-sessions.md); they are those projects' to take
or not, and nothing here waits for them.

## Why

b33p-core runs on one thread (a `LocalSet`), so nothing runs at the same time; but every
`.await` lets another task in, and the tasks share state:

- **Sessions.** A send loads a session, ratchets, and saves it, with awaits in between; two
  sends at once used the same counter (#51). `Sending` puts b33p's sends in one queue; presage's
  own sends and decryption are still beside it.
- **Chat settings.** `LocalStore::update` (local.rs) reads a row, changes it, and writes it
  back: two updates of one chat at once (the phone's read sync from `receive`, a pin from the
  command loop, a merge from `follow_storage`) and one is lost.
- **`Ctx`.** `threads` and `trust` are `Rc<RefCell<…>>` that the command loop, `receive`,
  `follow_storage`, `AvatarFetcher` and others read and write; `expiring` and `storage_changed`
  are `Notify`s that stand in for messages; `Repairs` is another shared `RefCell`.
- **Hand-made schedulers.** `AvatarFetcher` keeps its own Idle / Running / Again state to run
  one pass at a time; `sweep_expired` and `expire_stories` re-read the database when woken.

Each of these is right today only by reasoning about where the awaits are. An actor makes it
right by construction: what it owns, only it changes, one message at a time.

## How (the post's rules, applied)

- **The actor is data plus an event loop.** `async fn event_loop(mut self, rx) -> Self`: it
  ends when every sender is dropped and the queue is empty, and hands back its state (for
  tests). Spawning is the caller's: `spawn_local`, as now.
- **No handle types.** Callers hold the `mpsc::Sender`; the actor's module adds an extension
  trait on it (`Sends` for `Outbox`).
- **Commands over questions.** A message that needs an answer carries a `oneshot::Sender`; most
  shouldn't (a receipt, a typing indicator, a settings change: `Reply::Log` in `Sending`).
- **Bounded queues, and a DAG.** A full queue holds up the sender, which is the backpressure we
  want; so no actor may wait on one that waits on it. Sources (the command loop, `receive`)
  send down to services (sending, settings, timers, avatars) and on to the UI's events;
  services never send back up except on a oneshot.
- **Tests without sleeps.** Queue the messages, drop the sender, run the loop, assert on the
  state it returns.

## Where it stands

| Today | Shares | Becomes |
|---|---|---|
| `SENDING` mutex | — | `Sending` (done, #60) |
| `LocalStore::update` | the settings table | one statement, then a `Settings` actor (step 1) |
| `sweep_expired`, `expire_stories` | `ctx.expiring` (`Notify`), the tables | `Expiry` (step 2) |
| `follow_storage`, `write_storage` | `ctx.storage_changed`, `storage_read`, tables | `Storage` (step 3) |
| `AvatarFetcher` | `Rc<Cell<AvatarRun>>`, `threads` | `Avatars` (step 4) |
| `ctx.threads`, `ctx.trust` | 17 and 4 uses in signal.rs | `Registry` (step 5) |
| `receive`, the command loop | all of the above | sources that send to the actors (step 6) |
| attachment fetch / save / open | `download_limit` only | stay tasks |

## Steps

Each is one pull request that ships on its own, with the testbox phases (`cargo testbox test
core`, `groups`, `storage`) run before and after.

0. **`Sending`** — done (#60). Every presage call that sends goes through `Ctx::outbox`.

1. **Settings without lost updates.** First make `LocalStore::update` atomic: the read and the
   write in one `BEGIN IMMEDIATE` transaction, so the fix lands before any refactor. Then a
   `Settings` actor owns writes to `b33p_conversation` (`Pin`, `Mute`, `Archive`, `Read`,
   `Block`, `Accept`, `Merge(remote)`); reads stay direct queries. Test: two queued changes to
   one chat both land.

2. **`Expiry`.** One actor with a timer queue for disappearing messages and stories. The
   command loop and `receive` send `Started { conversation, timestamp, at }` instead of
   notifying `ctx.expiring`; the actor sleeps until the earliest, erases, and tells the UI. The
   startup scan (`stored_timers`) fills its queue before the loop starts. Test: queued timers
   expire in order with a paused tokio clock (`start_paused`).

3. **`Storage`.** `follow_storage` and `write_storage` become one actor whose messages are
   `Changed` (the phone said so: `handle_content`), `LocalChange` (pin, reorder: debounced, the
   `SETTLE` wait as a timer of its own) and `Retry`. `storage_read` stays a `watch` (many
   readers, one writer, which is what `watch` is for), owned by the actor. Its writes to the
   settings go through `Settings`.

4. **`Avatars`.** `Request` messages, coalesced: on each wake it drains the queue
   (`try_recv`) and runs one pass. That is `AvatarRun` without the state machine. It reads
   the threads from `Registry` (step 5) or, until then, from `ctx.threads`.

5. **`Registry`.** Owns the conversation id → thread map and the trust map. Writes are
   commands (`Register`, `SetTrust`); reads are frequent and from everywhere, so the actor
   publishes snapshots on a `watch` (`Rc<HashMap<…>>` behind it) rather than answering
   questions. This is the largest step: every `ctx.threads.borrow()` changes.

6. **Sources.** With the state owned elsewhere, `receive` and the command loop keep no shared
   state: `Ctx` shrinks to the senders of the actors above, the tables (for reads) and the
   events. `Repairs` moves into `receive`, its only user (with the session resets it limits
   going through `Sending`).

7. **Split signal.rs.** One module per actor (outbox.rs already), so that signal.rs keeps the
   session's start and end, and the handlers.

8. **When presage serializes sessions** (presage-sessions.md): `Sending` stays, for ordering
   (a typing indicator before its message) and as the one place sends happen, but its comment
   about sessions goes, and #51 closes.

## Open questions

- **Head-of-line blocking.** One queue means a slow group send (many members, a 409 that
  fetches prekeys) holds up a read receipt behind it. Signal-Android has the same one lock.
  Per-recipient lanes would help only once presage serializes per session rather than per
  account; not before.
- **Testing `Sending`.** It owns a `Manager`, which needs a registered store. A small trait
  over the calls it makes (`send_message`, `send_message_to_group`, …) would let a test give it
  a fake and check order and replies; worth it once a second actor needs the same.
- **Shutdown.** `run_linked` aborts `Sending` at the end instead of letting it drain, because
  after an unlink there is nobody to send to and the next session must not send beside it.
  Once every actor ends when its senders drop, the end of `run_linked` can drop `Ctx` and await
  them in DAG order instead.
- **Queue sizes.** 64 for `Sending` fits a chat's worth of read receipts; the others start at
  the same and get measured in the testbox.
