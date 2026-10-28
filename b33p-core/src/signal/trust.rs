//! Safety numbers: identity keys, verifying them here and elsewhere, how far each is trusted.

use super::errors::{db, send_failed, store_failed};
use crate::Error;
use crate::outbox::Sends as _;
use presage::{
    libsignal_service::{
        content::{Content, ContentBody},
        protocol::ServiceId,
    },
    proto::{SyncMessage, sync_message::Content as SyncContent},
    store::Thread,
};
use tracing::warn;

use crate::{ConversationId, Event, local::LocalStore, now_millis, scrub::scrubbed};

use super::{
    Ctx, Mgr,
    conversations::{Names, conversation_id, is_blocked, own_id},
    history::{stored_since, to_chat_message},
};

/// The kind of note a stored content is about a contact's safety number, if it is one: a
/// `Verified` sync message saved in their thread, without the contact real ones from our own
/// devices name. The store saves one with [`State::Default`] when their key changes; b33p saves
/// one with [`State::Verified`] or [`State::Unverified`] when we mark it verified or clear that
/// (here or on another device).
///
/// [`State::Default`]: presage::proto::verified::State::Default
/// [`State::Verified`]: presage::proto::verified::State::Verified
/// [`State::Unverified`]: presage::proto::verified::State::Unverified
pub(super) fn identity_note(content: &Content) -> Option<presage::proto::verified::State> {
    match &content.body {
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Verified(verified)),
            ..
        }) if verified.destination_aci.is_none() && verified.destination_aci_binary.is_none() => {
            Some(verified.state())
        }
        _ => None,
    }
}

/// Shows the notes about `contact`'s safety number saved in their chat since `since`: the
/// store saves one while decrypting a message from them or fetching their keys to write to
/// them (before b33p hears of either), b33p when it is verified. Only in their chat if there is
/// one and it isn't blocked; the UI ignores a note it already has.
async fn show_identity_notes(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    contact: ServiceId,
    since: u64,
) {
    let thread = Thread::Contact(contact);
    let id = conversation_id(&thread);
    if contact == own_id(manager)
        || !ctx.threads.borrow().contains_key(&id)
        || is_blocked(ctx, &id).await
    {
        return;
    }
    // This runs for every message that arrives: the newest few, not the whole thread.
    let Some(recent) = stored_since(manager.store(), &thread, since).await else {
        return;
    };
    for content in recent.iter().filter(|c| identity_note(c).is_some()) {
        if let Some(message) = to_chat_message(manager, names, id.clone(), content).await {
            ctx.events.send(Event::Message(message));
        }
    }
}

/// Shows the note of a changed safety number the store may have saved in the last few minutes;
/// see [`show_identity_notes`].
pub(super) async fn announce_identity_change(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    contact: ServiceId,
) {
    let since = crate::now_millis().saturating_sub(5 * 60 * 1000);
    show_identity_notes(manager, ctx, names, contact, since).await;
}

/// `who`'s identity key, as their messages and sessions here have it, if there is one yet.
async fn identity_of(
    manager: &Mgr,
    who: ServiceId,
) -> Option<presage::libsignal_service::protocol::IdentityKey> {
    use presage::libsignal_service::protocol::{DeviceId, IdentityKeyStore, ProtocolAddress};
    use presage::store::Store as _;
    let address = ProtocolAddress::new(who.service_id_string(), DeviceId::new(1).ok()?);
    manager
        .store()
        .aci_protocol_store()
        .get_identity(&address)
        .await
        .ok()?
}

/// The key `who` has now, as far as is known here: the one the store refused for sending while
/// their verified one is pinned (it came with their pre-key bundle; see [`hold_changed`]), else
/// the one their messages and sessions have. The safety number shown, and the key verified or
/// accepted, is of this one.
async fn key_shown(
    manager: &Mgr,
    who: ServiceId,
) -> Option<presage::libsignal_service::protocol::IdentityKey> {
    match manager.store().offered_identity(&who).await {
        Ok(Some(offered)) => Some(offered),
        Ok(None) => identity_of(manager, who).await,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to read the key the store refused for sending");
            identity_of(manager, who).await
        }
    }
}

/// How far the safety number with `who` is trusted: verified for the key they have now, or
/// verified before and changed since.
pub(super) async fn trust_of(manager: &Mgr, tables: &LocalStore, who: ServiceId) -> crate::Trust {
    let verification = tables.verification(&who.service_id_string()).await;
    trust_given(manager, who, verification.ok().flatten().as_ref()).await
}

/// [`trust_of`] with what is kept about `who`'s verification already read; their key is only
/// looked up if there is something to compare it with.
pub(super) async fn trust_given(
    manager: &Mgr,
    who: ServiceId,
    verification: Option<&crate::local::Verification>,
) -> crate::Trust {
    use presage::proto::verified::State;
    if !verification.is_some_and(|v| v.state != State::Default) {
        return crate::Trust::Unverified;
    }
    trust(verification, identity_of(manager, who).await.as_ref())
}

/// How far a safety number is trusted, given what is kept about its verification and the key
/// the contact has now: verified only for that key; changed if an earlier one was, or if the
/// change was seen on another device and not confirmed there (Signal's "unverified").
fn trust(
    verification: Option<&crate::local::Verification>,
    key: Option<&presage::libsignal_service::protocol::IdentityKey>,
) -> crate::Trust {
    use presage::proto::verified::State;
    match (verification, key) {
        (Some(v), Some(key)) => match v.state {
            State::Default => crate::Trust::Unverified,
            State::Verified if *v.identity_key == *key.serialize() => crate::Trust::Verified,
            State::Verified | State::Unverified => crate::Trust::Changed,
        },
        // Nothing kept, or no key yet: nothing to compare, nothing changed.
        _ => crate::Trust::Unverified,
    }
}

/// Tells the UI how far `who`'s safety number is trusted, if that isn't what it was last told.
fn publish_trust(ctx: &Ctx, who: ServiceId, trust: crate::Trust) {
    if ctx.trust.borrow_mut().insert(who, trust) != Some(trust) {
        ctx.events.send(Event::Trust {
            conversation: conversation_id(&Thread::Contact(who)),
            trust,
        });
    }
}

/// Works out again how far each contact with a kept verification (few) is trusted, and tells the
/// UI of those that changed. Their key can change in any message sent or received (the store
/// notices it first), so this runs after each.
pub(super) async fn refresh_trust(manager: &Mgr, ctx: &Ctx) {
    use presage::proto::verified::State;
    let Ok(all) = ctx.tables.verifications().await else {
        return;
    };
    for (aci, verification) in all.iter().filter(|(_, v)| v.state != State::Default) {
        if let Some(who) = ServiceId::parse_from_service_id_string(aci) {
            let trust = trust_given(manager, who, Some(verification)).await;
            publish_trust(ctx, who, trust);
        }
    }
}

/// [`refresh_trust`] for one contact: what came from them may have brought a new key.
pub(super) async fn refresh_trust_of(manager: &Mgr, ctx: &Ctx, who: ServiceId) {
    use presage::proto::verified::State;
    let Ok(all) = ctx.tables.verifications().await else {
        return;
    };
    if let Some(verification) = all.get(&who.service_id_string())
        && verification.state != State::Default
    {
        let trust = trust_given(manager, who, Some(verification)).await;
        publish_trust(ctx, who, trust);
    }
}

/// Where a verification comes from: marked here, or taken over from another device's message
/// or the storage service (`quiet` on the first read, which brings what was set before).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verifier {
    Here,
    Elsewhere { quiet: bool },
}

/// Marks the safety number with a contact verified, or clears that (which also accepts one that
/// changed): here first, with a note in the chat; then, as Signal's apps do, a null message to
/// them (so that what the server sees is an ordinary message to them) and the change to our
/// other devices with the same padding. Not reaching our devices is an error to show, but it
/// is done here: accepting a change is ours to decide, and writing to them shouldn't wait.
pub(super) async fn set_verified(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    verified: bool,
) -> Result<(), Error> {
    use presage::libsignal_service::content::NullMessage;
    use presage::proto::verified::State;
    let thread = ctx.threads.borrow().get(conversation).cloned();
    let who = match thread {
        Some(Thread::Contact(who @ ServiceId::Aci(_))) if who != own_id(manager) => who,
        _ => {
            return Err(Error::refused(
                "only a contact has a safety number to verify",
            ));
        }
    };
    let current = identity_of(manager, who).await;
    let key = key_shown(manager, who)
        .await
        .ok_or_else(|| Error::refused("there is no safety number with them yet"))?;
    if current.as_ref() != Some(&key) {
        take_over(manager, ctx, who, &key).await?;
    }
    let state = if verified {
        State::Verified
    } else {
        State::Default
    };
    remember_verification(manager, ctx, who, &key, state, Verifier::Here).await?;
    let padding = NullMessage::generate(&mut rand::rng()).padding;
    let timestamp = now_millis();
    if let Err(error) = ctx
        .outbox
        .message(
            who,
            ContentBody::NullMessage(NullMessage {
                padding: padding.clone(),
            }),
            timestamp,
        )
        .await
    {
        warn!(error = %scrubbed(&error), "failed to send the null message that goes with a verification");
    }
    let sync = verified_sync(who, &key, verified, padding);
    let own = own_id(manager);
    ctx.outbox
        .message(own, ContentBody::SynchronizeMessage(sync), timestamp)
        .await
        .map_err(send_failed(
            "done here, but your other devices couldn't be told",
        ))?;
    Ok(())
}

/// Takes `key`, which the store refused for sending to `who` and kept as offered, as theirs:
/// saved as their identity (the store notes the change in their chat; shown at once) and their
/// sessions on the old key dropped, as Signal's apps do when a key changes, so that the next
/// message to them sets up a session with the new one.
async fn take_over(
    manager: &Mgr,
    ctx: &Ctx,
    who: ServiceId,
    key: &presage::libsignal_service::protocol::IdentityKey,
) -> Result<(), Error> {
    use presage::libsignal_service::protocol::{DeviceId, IdentityKeyStore, ProtocolAddress};
    use presage::libsignal_service::session_store::SessionStoreExt;
    use presage::store::Store as _;
    let address = ProtocolAddress::new(
        who.service_id_string(),
        DeviceId::new(1).expect("a device id"),
    );
    let since = now_millis().saturating_sub(1000);
    let mut store = manager.store().aci_protocol_store();
    store
        .save_identity(&address, key)
        .await
        .map_err(store_failed("failed to take their new key"))?;
    if let Err(error) = store.delete_all_sessions(&who).await {
        warn!(error = %scrubbed(&error), "failed to drop the sessions on their old key");
    }
    show_identity_notes(manager, ctx, &mut Names::new(), who, since).await;
    Ok(())
}

/// What tells our other devices that the safety number with `who` (their `key`) is verified,
/// or not anymore: Signal's apps clear it to the default state. `padding` is the null message's
/// sent to them with it.
fn verified_sync(
    who: ServiceId,
    key: &presage::libsignal_service::protocol::IdentityKey,
    verified: bool,
    padding: Option<Vec<u8>>,
) -> SyncMessage {
    use presage::proto::verified::State;
    let state = if verified {
        State::Verified
    } else {
        State::Default
    };
    SyncMessage {
        content: Some(SyncContent::Verified(presage::proto::Verified {
            destination_aci: Some(who.service_id_string()),
            destination_aci_binary: Some(who.service_id_binary()),
            identity_key: Some(key.serialize().to_vec()),
            state: Some(state as i32),
            null_message: padding,
        })),
        ..SyncMessage::with_padding(&mut rand::rng())
    }
}

/// Whom and which key of theirs a verification from another device is about: a contact's ACI
/// and a key that decodes.
fn verified_target(
    verified: &presage::proto::Verified,
) -> Option<(ServiceId, presage::libsignal_service::protocol::IdentityKey)> {
    use presage::libsignal_service::protocol::IdentityKey;
    let who = verified
        .destination_aci_binary
        .as_deref()
        .and_then(ServiceId::parse_from_service_id_binary)
        .or_else(|| {
            verified
                .destination_aci
                .as_deref()
                .and_then(ServiceId::parse_from_service_id_string)
        })
        .filter(|who| matches!(who, ServiceId::Aci(_)))?;
    let key = IdentityKey::decode(verified.identity_key.as_deref()?).ok()?;
    Some((who, key))
}

/// Keeps the verification `state` of `who`'s `key`, notes the change in their chat and shows it,
/// unless it was already so. From another device, a key that isn't the one we have for them is
/// taken over as theirs, as Signal's apps do: the phone compared it.
pub(super) async fn remember_verification(
    manager: &Mgr,
    ctx: &Ctx,
    who: ServiceId,
    key: &presage::libsignal_service::protocol::IdentityKey,
    state: presage::proto::verified::State,
    verifier: Verifier,
) -> Result<(), Error> {
    use presage::libsignal_service::protocol::{DeviceId, IdentityKeyStore, ProtocolAddress};
    use presage::store::Store as _;
    let aci = who.service_id_string();
    let address = ProtocolAddress::new(aci.clone(), DeviceId::new(1).expect("a device id"));
    let mut current = identity_of(manager, who).await;
    if verifier != Verifier::Here && current.as_ref() != Some(key) {
        let mut store = manager.store().aci_protocol_store();
        match store.save_identity(&address, key).await {
            Ok(_) => current = identity_of(manager, who).await,
            Err(error) => {
                warn!(error = %scrubbed(&error), "failed to take over a key verified on another device")
            }
        }
    }
    let verification = crate::local::Verification {
        identity_key: key.serialize().to_vec(),
        state,
    };
    let before = ctx.tables.verification(&aci).await.map_err(db)?;
    if before.as_ref() == Some(&verification) {
        return Ok(());
    }
    // A note only for their key as it is here: one for another would contradict what the
    // details page says.
    let quiet = verifier == (Verifier::Elsewhere { quiet: true });
    let note = verification_note(before.as_ref(), &verification)
        .filter(|_| !quiet && current.as_ref() == Some(key));
    ctx.tables
        .set_verification(&aci, &verification)
        .await
        .map_err(db)?;
    pin_verified(manager, who, &verification).await;
    publish_trust(ctx, who, trust(Some(&verification), current.as_ref()));
    if let Some(state) = note {
        let since = now_millis();
        match presage::store::save_trusted_identity_message(manager.store(), &address, *key, state)
            .await
        {
            Ok(()) => show_identity_notes(manager, ctx, &mut Names::new(), who, since).await,
            // The verification itself is kept; only its note is missing.
            Err(error) => {
                warn!(error = %scrubbed(&error), "failed to save the note of a verification")
            }
        }
    }
    Ok(())
}

/// The note in the chat a change of a verification deserves: having verified the key, or having
/// cleared a verification of it. Nothing for no change, for clearing what wasn't verified, nor
/// for another device seeing their key change (Signal's "unverified": the change notice says so).
fn verification_note(
    before: Option<&crate::local::Verification>,
    after: &crate::local::Verification,
) -> Option<presage::proto::verified::State> {
    use presage::proto::verified::State;
    if before == Some(after) {
        return None;
    }
    match after.state {
        State::Verified => Some(State::Verified),
        State::Default
            if before.is_some_and(|b| {
                b.state == State::Verified && b.identity_key == after.identity_key
            }) =>
        {
            Some(State::Unverified)
        }
        _ => None,
    }
}

/// Keeps the store's pin in step with a kept verification: a verified key is the only one sent
/// to (a new one of theirs is refused for sending, see [`deliver`]); clearing it, or accepting
/// a change, lifts that; a change not confirmed yet keeps it.
pub(super) async fn pin_verified(
    manager: &Mgr,
    who: ServiceId,
    verification: &crate::local::Verification,
) {
    use presage::{libsignal_service::protocol::IdentityKey, proto::verified::State};
    let pinned = match verification.state {
        State::Verified => match IdentityKey::decode(&verification.identity_key) {
            Ok(key) => manager.store().pin_identity(&who, &key).await,
            Err(_) => return,
        },
        State::Default => manager.store().unpin_identity(&who).await,
        State::Unverified => return,
    };
    if let Err(error) = pinned {
        warn!(error = %scrubbed(&error), "failed to pin or unpin a verified safety number");
    }
}

/// Holds the chat with `who`, whose identity the store refused for sending: their verified key
/// is pinned, so their new one wasn't taken (the store keeps it as offered, see [`key_shown`],
/// until [`set_verified`] takes it over); keeping the pinned one as "unverified" (changed, not
/// confirmed) says so until they confirm or verify again.
pub(super) async fn hold_changed(manager: &Mgr, ctx: &Ctx, who: ServiceId) {
    use presage::proto::verified::State;
    let Some(key) = identity_of(manager, who).await else {
        return;
    };
    if let Err(error) =
        remember_verification(manager, ctx, who, &key, State::Unverified, Verifier::Here).await
    {
        warn!(error = %scrubbed(&error), "failed to hold a chat whose safety number changed");
    }
}

/// Our phone (or another device) verified the safety number with a contact, cleared that, or saw
/// it change.
pub(super) async fn on_verified_elsewhere(
    manager: &Mgr,
    ctx: &Ctx,
    verified: &presage::proto::Verified,
) {
    let Some((who, key)) = verified_target(verified) else {
        warn!("a verification from another device without whom or which key it is about");
        return;
    };
    let verifier = Verifier::Elsewhere { quiet: false };
    if let Err(error) =
        remember_verification(manager, ctx, who, &key, verified.state(), verifier).await
    {
        warn!(error = %scrubbed(&error), "failed to keep a verification from another device");
    }
}

/// The safety number with `who`, as Signal shows it: 60 digits in groups of five.
pub(super) async fn safety_number(manager: &Mgr, who: ServiceId) -> Option<String> {
    use presage::libsignal_service::protocol::{Fingerprint, IdentityKeyStore};
    use presage::store::Store as _;
    let store = manager.store().aci_protocol_store();
    let ours = store.get_identity_key_pair().await.ok()?;
    let theirs = key_shown(manager, who).await?;
    let own = own_id(manager);
    // Version 2 identifies both sides by their ACI.
    let fingerprint = Fingerprint::new(
        2,
        5200,
        own.raw_uuid().as_bytes(),
        ours.identity_key(),
        who.raw_uuid().as_bytes(),
        &theirs,
    )
    .ok()?;
    let digits = fingerprint.display_string().ok()?;
    let groups: Vec<&str> = digits
        .as_bytes()
        .chunks(5)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect();
    Some(groups.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use presage::{model::identity::OnNewIdentity, store::ContentsStore};
    use presage_store_sqlite::SqliteStore;

    /// What the store saves when a contact's safety number changes is recognised as that note
    /// (from the store itself, so a change in how presage writes it shows up here); a real
    /// `Verified` sync message from our devices, which names its contact, is not.
    #[test]
    fn a_changed_safety_number_is_recognised() {
        use presage::proto::verified::State;
        use presage::{
            libsignal_service::protocol::{
                DeviceId, IdentityKeyPair, IdentityKeyStore, ProtocolAddress,
            },
            store::Store as _,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let store = SqliteStore::open(":memory:", OnNewIdentity::Trust)
                .await
                .unwrap();
            let contact =
                ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000002")
                    .unwrap();
            let address =
                ProtocolAddress::new(contact.service_id_string(), DeviceId::new(1).unwrap());
            let mut identities = store.aci_protocol_store();
            // Their first key, then a new one (e.g. they reinstalled Signal).
            for _ in 0..2 {
                let key = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
                identities.save_identity(&address, &key).await.unwrap();
            }
            let stored: Vec<Content> = store
                .messages(&Thread::Contact(contact), ..)
                .await
                .unwrap()
                .flatten()
                .collect();
            assert_eq!(stored.len(), 1, "one note, for the one change");
            assert_eq!(identity_note(&stored[0]), Some(State::Default));

            let mut real = stored[0].clone();
            let ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::Verified(verified)),
                ..
            }) = &mut real.body
            else {
                panic!("the note is a Verified sync message");
            };
            let mut verifying = stored[0].clone();
            verified.destination_aci = Some(contact.service_id_string());
            assert_eq!(identity_note(&real), None);
            // Nor is a note of verifying the key.
            if let ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::Verified(verified)),
                ..
            }) = &mut verifying.body
            {
                verified.set_state(presage::proto::verified::State::Verified);
            }
            assert_eq!(
                identity_note(&verifying),
                Some(State::Verified),
                "a note of verifying, not of a change"
            );
        });
    }

    /// A note in the chat for verifying, and for clearing a verification; none for repeating
    /// what was already so, nor for clearing what wasn't verified (e.g. their key changed since).
    #[test]
    fn verifications_get_a_note_only_when_it_says_something() {
        use crate::local::Verification;
        use presage::proto::verified::State::{self, Default as Cleared, Unverified, Verified};
        let of = |key: u8, state: State| Verification {
            identity_key: vec![key; 33],
            state,
        };
        assert_eq!(verification_note(None, &of(1, Verified)), Some(Verified));
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(1, Verified)),
            None
        );
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(1, Cleared)),
            Some(Unverified),
            "cleared"
        );
        assert_eq!(verification_note(None, &of(1, Cleared)), None);
        assert_eq!(
            verification_note(Some(&of(1, Cleared)), &of(1, Cleared)),
            None
        );
        // Verified their old key; clearing for the new one says nothing of it.
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(2, Cleared)),
            None
        );
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(2, Verified)),
            Some(Verified)
        );
        // Another device saw their key change: the change notice says so already.
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(2, Unverified)),
            None
        );
    }

    /// What tells our other devices about a verification is what Signal's apps send: the
    /// contact's ACI both ways, their serialized key, the state (cleared to the default), and the
    /// null message's padding; and what they send back is read the same way.
    #[test]
    fn verifications_to_and_from_other_devices() {
        use presage::{libsignal_service::protocol::IdentityKeyPair, proto::verified::State};
        let who = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000002")
            .unwrap();
        let key = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let padding = Some(vec![7; 12]);
        let verified_of = |sync: SyncMessage| match sync.content {
            Some(SyncContent::Verified(verified)) => verified,
            other => panic!("not a verification: {other:?}"),
        };

        let marked = verified_of(verified_sync(who, &key, true, padding.clone()));
        assert_eq!(marked.destination_aci, Some(who.service_id_string()));
        assert_eq!(marked.destination_aci_binary, Some(who.service_id_binary()));
        assert_eq!(marked.identity_key.as_deref().map(<[u8]>::len), Some(33));
        assert_eq!(marked.state(), State::Verified);
        assert_eq!(marked.null_message, padding);
        let cleared = verified_of(verified_sync(who, &key, false, None));
        assert_eq!(cleared.state(), State::Default);

        assert_eq!(verified_target(&marked), Some((who, key)));
        let by_string = presage::proto::Verified {
            destination_aci_binary: None,
            ..marked.clone()
        };
        assert_eq!(verified_target(&by_string), Some((who, key)));
        let pni = presage::proto::Verified {
            destination_aci: None,
            destination_aci_binary: Some(
                ServiceId::parse_from_service_id_string("PNI:00000000-0000-0000-0000-000000000002")
                    .unwrap()
                    .service_id_binary(),
            ),
            ..marked.clone()
        };
        assert_eq!(verified_target(&pni), None, "only contacts' ACIs");
        let keyless = presage::proto::Verified {
            identity_key: None,
            ..marked
        };
        assert_eq!(verified_target(&keyless), None);
    }

    /// Verified only for the key it was verified for; a new key of a verified contact is a
    /// change to confirm; clearing (for any key) leaves nothing to confirm.
    #[test]
    fn trust_follows_the_key() {
        use crate::{Trust, local::Verification};
        use presage::{
            libsignal_service::protocol::{IdentityKey, IdentityKeyPair},
            proto::verified::State::{self, Default as Cleared, Unverified, Verified},
        };
        let old = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let new = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let of = |key: &IdentityKey, state: State| Verification {
            identity_key: key.serialize().to_vec(),
            state,
        };
        assert_eq!(trust(None, Some(&new)), Trust::Unverified);
        assert_eq!(
            trust(Some(&of(&new, Verified)), Some(&new)),
            Trust::Verified
        );
        assert_eq!(trust(Some(&of(&old, Verified)), Some(&new)), Trust::Changed);
        assert_eq!(
            trust(Some(&of(&old, Cleared)), Some(&new)),
            Trust::Unverified
        );
        assert_eq!(
            trust(Some(&of(&new, Cleared)), Some(&new)),
            Trust::Unverified,
            "confirmed"
        );
        // The phone saw it change after it was verified, and it isn't confirmed yet.
        assert_eq!(
            trust(Some(&of(&new, Unverified)), Some(&new)),
            Trust::Changed
        );
        assert_eq!(
            trust(Some(&of(&old, Verified)), None),
            Trust::Unverified,
            "no key yet"
        );
    }
}
