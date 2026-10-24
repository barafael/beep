# presage: a sessions actor

A proposal for [whisperfish/presage](https://github.com/whisperfish/presage), to be discussed
there before anything is written; b33p uses the fork
[barafael/presage](https://github.com/barafael/presage) (pinned at `27af46f`; line numbers below
are from that commit). It needs the `SessionSteps` hook proposed in
[libsignal-service-sessions.md](libsignal-service-sessions.md), or an equivalent.

## The problem

presage changes the account's sessions from several places at once on one `LocalSet`:

- **Receiving** (`receive_messages`, presage/src/manager/registered.rs:627) decrypts each
  envelope in the stream (`open_envelope`, :716 ACI and :728 PNI), one after the other.
- **Sending while receiving.** The stream answers requests in tasks of its own, each of which
  encrypts: the contacts sync (`send_contact_details`, `spawn_local` at :776), the keys sync
  (:811), the blocked list (:829), and the keys request when there is no account entropy pool
  (:1065).
- **The application's sends.** `send_message` (:1150), `send_message_to_group` (:1924),
  `send_to_members` for group changes (:1865), and through them `request_contacts` (:309) and
  `send_session_reset` (:2210), whenever the application calls them.
- **Session changes outside a sender.** `send_session_reset` archives each session itself
  (load, `archive_current_state`, store: :2234-2236); `clear_sessions` (:2063) deletes them all.

Each send builds a new `MessageSender` (`new_message_sender`, :2255) and each stream its own
`ServiceCipher`s, all on the same store (`Manager` and `Registered` are `Clone` and share it).
`presage-store-sqlite`'s `load_session` is a plain `SELECT` and `store_session` its own
transaction (protocol.rs:194, :215): nothing spans one ratchet step.

So two of these at once use one chain counter. Seen in b33p's testbox now and then: a sticker
install's sync dropped; b33p's copy of a message it sent lost on the phone (Signal-Android:
"Decryption error for a sync message!"). An application can order its own sends (b33p does,
in an actor) but not presage's, nor its sends against the stream's decryption.

## Proposal

One `Sessions` actor per registered `Manager`, which presage starts with it and every
`MessageSender` and `ServiceCipher` it makes is given (as their `SessionSteps`):

```rust
/// Runs the account's session steps one after the other.
struct Sessions;

/// A step to run: a future made by libsignal-service, and where its outcome goes.
type Step = Box<dyn FnOnce() -> LocalBoxFuture<'static, ()>>;

impl Sessions {
    async fn event_loop(self, mut steps: mpsc::Receiver<Step>) -> Self {
        while let Some(step) = steps.recv().await {
            step().await;
        }
        self
    }
}

/// What `MessageSender` and `ServiceCipher` are given: the queue.
#[derive(Clone)]
struct SessionQueue(mpsc::Sender<Step>);

impl SessionSteps for SessionQueue {
    async fn step<T: 'static>(&self, step: impl AsyncFnOnce() -> T + 'static) -> T {
        let (done, outcome) = oneshot::channel();
        let job: Step = Box::new(move || Box::pin(async move { drop(done.send(step().await)) }));
        self.0.send(job).await.expect("sessions run as long as the manager");
        outcome.await.expect("a step runs to its end")
    }
}
```

- The `Registered` state holds the `SessionQueue`; `Manager::clone` shares it, so every clone,
  every sender and every stream go through the one queue.
- `send_session_reset`'s archiving and `clear_sessions` become steps too.
- Steps never include network calls (libsignal-service's side), so the queue never waits on the
  server: a decryption waits at most for one encryption, not for a send.
- Where it runs: presage spawns the loop with `spawn_local` when it builds the registered
  manager, as it spawns its other tasks; it ends when the last `Manager` clone is dropped.
- A lock would do the same (`SessionSteps` for an `Arc<tokio::sync::Mutex<()>>`); the actor
  keeps a step from being cancelled halfway (a dropped caller doesn't stop a step that has
  loaded a session and not stored it yet), and keeps the order the steps were asked in.

## Without the libsignal-service hook

presage alone could put whole operations in the queue (each `send_message*` and each
`open_envelope`). That fixes the race but holds decryption up for a whole send, network round
trips and 409 retries included, so a slow group send would stall receiving. It is a fallback
if the hook isn't taken upstream, not the proposal.

## Tests

- Two `send_message`s to one contact at once, both decrypted by the recipient (with the
  in-memory store, which presage's tests already use).
- A send while the stream decrypts a message from the same contact: both go through.
- The contacts sync the stream answers, and an application send, at once.

## For the discussion upstream

- Whether presage should own the actor, or take a `SessionSteps` from the application (so one
  that already serializes, like b33p, can pass its own).
- The `'static` bound on steps: libsignal-service's steps borrow the sender; they may need to
  clone what they use (the stores are `Clone`) or the trait take a non-`'static` future and
  the actor run it in place.
