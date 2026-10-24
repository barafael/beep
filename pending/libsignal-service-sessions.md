# libsignal-service-rs: one session step at a time

A proposal for [whisperfish/libsignal-service-rs](https://github.com/whisperfish/libsignal-service-rs),
to be discussed there before anything is written; b33p uses it through presage, from the fork
[barafael/libsignal-service-rs](https://github.com/barafael/libsignal-service-rs) (pinned at
`710b11f`; line numbers below are from that commit). It is what
[presage-sessions.md](presage-sessions.md) builds on.

## The problem

A Signal session is changed in steps: load it from the `SessionStore`, ratchet, store it, with
`.await`s in between (the store is async, e.g. SQLite). Two steps on one session that interleave
use the same chain counter: the recipient refuses the second message ("message with old
counter"), or a decrypted ratchet step is lost and the next message can't be decrypted.

Nothing in libsignal-service orders these steps: `MessageSender` and `ServiceCipher` are
`Clone`, and every clone (and every new one, which presage makes per operation) works on the
same store. The only lock in the crate is around the websocket (`src/websocket/mod.rs`). An
application that sends from two tasks, or sends while it receives, interleaves them.

Signal-Android takes one lock, `SignalSessionLock`, around every encryption and decryption
([SignalSessionLock.java](https://github.com/signalapp/Signal-Android/blob/main/lib/libsignal-service/src/main/java/org/whispersystems/signalservice/api/SignalSessionLock.java)),
and the network calls stay outside it.

## Where the steps are

Encryption (`src/sender.rs`, `src/cipher.rs`):

- `MessageSender::create_encrypted_message` (sender.rs:1025): `load_session` (:1042); without
  one, `get_pre_keys` over the network (:1052) and `process_prekey_bundle` (:1086); then
  `ServiceCipher::encrypt` (:1102).
- `ServiceCipher::encrypt` (cipher.rs:562): `load_session` (:571) to check the sender chain, then
  `sealed_sender_encrypt` (:602) or `message_encrypt` (:621), each of which loads and stores the
  session again inside libsignal.
- `MessageSender::create_encrypted_messages` (sender.rs:915): `get_sub_device_sessions` (:923),
  one `create_encrypted_message` per device, and on `SessionNotFound` `delete_session` (:981)
  and again.
- `MessageSender::try_send_message` (sender.rs:592), on 409: `delete_service_addr_device_session`
  for extra devices (:672), `get_pre_key` over the network (:689) and `process_prekey_bundle`
  (:692) for missing ones; on 410, `delete_service_addr_device_session` (:720).

Decryption (`src/cipher.rs`):

- `ServiceCipher::decrypt` (:279): `get_preferred_protocol_address` (a `load_session`,
  :703-714), then `message_decrypt_prekey` (:366) or `message_decrypt_signal` (:434), then a
  second `load_session` for the padding version (:381-389, :446).
- Sealed sender: `sealed_sender_decrypt` (:793) and from there `message_decrypt_signal` (:881),
  `message_decrypt_prekey` (:893) or `group_decrypt` (:907).
- `ServiceCipher::open_envelope` (:106): after `decrypt`,
  `process_sender_key_distribution_message` (:129).

## Proposal

A trait that marks a step, passed to `MessageSender` and `ServiceCipher`, and no opinion on how
an application keeps steps apart:

```rust
/// Runs session steps: a load, the change, and the store, with nothing on the same sessions
/// in between. Steps never include network calls.
pub trait SessionSteps: Clone {
    async fn step<T>(&self, step: impl AsyncFnOnce() -> T) -> T;
}

/// One step after another as they are called: the behaviour today, for those that never send
/// and receive at once.
#[derive(Clone, Default)]
pub struct Unordered;

impl SessionSteps for Unordered {
    async fn step<T>(&self, step: impl AsyncFnOnce() -> T) -> T {
        step().await
    }
}
```

- `MessageSender::new` and `ServiceCipher::new` take one (or `MessageSender<S, Steps =
  Unordered>` keeps today's signatures for the default; to be decided with the maintainers).
- A step is each of: one `create_encrypted_message` for one device, from the `load_session`
  to the end of `encrypt` (the network `get_pre_keys` taken out of it: fetch first, then the
  step re-checks for a session and processes the bundle); each `process_prekey_bundle` on 409
  (after its `get_pre_key`); each session delete on 409 / 410 / `SessionNotFound`; one
  `decrypt` with its second `load_session`; `process_sender_key_distribution_message`.
- No step is held across a network call, so a slow server never holds up decryption.
- Granularity: per account, like Android's. Per address would allow more at once but needs
  the trait to name the address; that can come later as `step_for(&ProtocolAddress, …)` with
  the per-account one as its default.

An application then chooses: a `tokio::sync::Mutex` (`SessionSteps` for an `Arc<Mutex<()>>`),
or an actor that runs the steps one after the other (presage-sessions.md). The crate needs
neither tokio's sync feature nor a runtime for it.

## Tests

- With an `Unordered` cipher and two concurrent encrypts to one address over an in-memory store
  that yields between load and store, the counters collide (the bug, as a test).
- With a serializing `SessionSteps`, the same test sends two messages the recipient decrypts.
- The same pair for one encrypt and one decrypt on one session.

## For the discussion upstream

- Whether `AsyncFnOnce` (Rust 1.85) is acceptable for the crate's MSRV, or a boxed future
  instead.
- Whether the PNI cipher shares the ACI's steps (it should: one account).
- Whether `AccountManager::pnp_initialize_devices` (account_manager.rs:708), which encrypts
  through a `MessageSender`, takes the sender's steps (it should).
