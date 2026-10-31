//! Receiving: the connection kept open for the session, and what each incoming content does.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    time::{Duration, Instant},
};

use futures::{StreamExt, pin_mut};
use presage::{
    Error as PresageError,
    libsignal_service::{
        content::{Content, ContentBody},
        protocol::{DeviceId, ProtocolAddress, PublicKey, ServiceId, SignalProtocolError},
    },
    model::messages::Received,
    proto::{
        DecryptionErrorMessage, ReceiptMessage, SyncMessage, TypingMessage,
        data_message::Delete,
        receipt_message,
        sync_message::{self, Content as SyncContent, Sent},
        typing_message,
    },
    store::{ContentExt, ContentsStore, Store as _, Thread},
};
use tracing::{debug, info, warn};

use crate::{
    ChatMessage, ConversationId, Delivery, Event, MessageRef, now_millis,
    outbox::{Outbox, Sends as _},
    scrub::scrubbed,
};

use super::{
    Ctx, Mgr,
    avatars::{AvatarFetcher, send_group_avatar},
    conversations::{
        Names, accepted, conversation_id, e164_of, ensure_conversation, is_blocked, own_id,
        send_conversations, sender_name, thread_of, wrote_to,
    },
    groups::{
        accept_from_trusted, forget_membership, is_announcement, is_member, lets_us_in,
        own_membership_change, refresh_group, signed_group_change,
    },
    history::{
        author_of, checkpoint, data_of, erase_deleted, is_outgoing, original, start_timers,
        stored_at, stored_timers, to_chat_message, to_reaction,
    },
    stories::on_story,
    trust::{announce_identity_change, on_verified_elsewhere, refresh_trust_of},
};

/// Receives in the background (see [`receive`]), from `after` on.
pub(super) fn start_receiving(
    manager: &Mgr,
    ctx: &Ctx,
    removed: &Rc<tokio::sync::Notify>,
    avatars: AvatarFetcher,
    repairs: Rc<RefCell<Repairs>>,
    after: Duration,
) -> tokio::task::JoinHandle<()> {
    let (manager, ctx, removed) = (manager.clone(), ctx.clone(), removed.clone());
    tokio::task::spawn_local(async move {
        tokio::time::sleep(after).await;
        receive(manager, ctx, avatars, repairs, removed).await;
    })
}

/// Whether Signal refuses this device's credentials: it was removed from the account (the
/// server answers 403, or 401), so retrying can't help.
fn removed_from_account(error: &PresageError<presage_store_sqlite::SqliteStoreError>) -> bool {
    use presage::libsignal_service::push_service::ServiceError;
    use presage::libsignal_service::websocket::{HandshakeError, WebSocketError};
    match error {
        PresageError::ServiceError(ServiceError::Unauthorized) => true,
        PresageError::ServiceError(ServiceError::WebSocket(WebSocketError::Handshake(
            HandshakeError::UnexpectedStatusCode(status),
        ))) => matches!(status.as_u16(), 401 | 403),
        _ => false,
    }
}

/// What was done to mend sessions, kept for as long as we are linked (across restarts of
/// receiving), so that a peer can't have it done over and over: when we last reset the
/// session with each sender (see `SESSION_RESET_INTERVAL`), and which messages were sent
/// again on a retry receipt, by recipient and timestamp.
#[derive(Default)]
pub(super) struct Repairs {
    resets: HashMap<ServiceId, Instant>,
    resent: HashSet<(ServiceId, u64)>,
}

const SESSION_RESET_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Receives messages for the rest of the session, reconnecting with exponential backoff
/// whenever the connection drops (sleep, network changes, server timeouts), and at once when
/// the computer wakes from sleep: the old connection may look alive for minutes until a
/// keepalive fails, and a pending retry may be up to a minute away.
/// Notifies `removed` (and stops) if Signal no longer accepts this device.
async fn receive(
    mut manager: Mgr,
    ctx: Ctx,
    avatars: AvatarFetcher,
    repairs: Rc<RefCell<Repairs>>,
    removed: Rc<tokio::sync::Notify>,
) {
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut kept = Kept::default();
    let mut backoff = Duration::from_secs(1);
    let woke = Rc::new(tokio::sync::Notify::new());
    let watching = tokio::task::spawn_local(watch_wakeups(woke.clone()));
    // Stops watching when receiving stops.
    let _watching = AbortOnDrop(watching);
    loop {
        let mut reconnect_now = false;
        match manager.receive_messages().await {
            Ok(messages) => {
                info!("connected to Signal; syncing");
                if !ctx.events.sent(Event::Connected) {
                    return;
                }
                kept.receipts.live = false;
                pin_mut!(messages);
                loop {
                    // Only between messages, so one is never handled halfway.
                    let received = tokio::select! {
                        received = messages.next() => received,
                        () = woke.notified() => {
                            info!("woke from sleep; reconnecting to Signal");
                            reconnect_now = true;
                            break;
                        }
                    };
                    let Some(received) = received else {
                        info!("connection to Signal closed");
                        break;
                    };
                    // Something came through, so the connection works again.
                    backoff = Duration::from_secs(1);
                    let handled =
                        handle_received(&manager, &ctx, &avatars, &mut kept, &repairs, received)
                            .await;
                    if !handled {
                        return;
                    }
                }
            }
            Err(error) if removed_from_account(&error) => {
                warn!(error = %scrubbed(&error), "Signal no longer accepts this device");
                removed.notify_one();
                return;
            }
            Err(error) => warn!(error = %scrubbed(&error), "failed to connect to Signal"),
        }
        if !ctx.events.sent(Event::Reconnecting) {
            return;
        }
        if reconnect_now {
            backoff = Duration::from_secs(1);
            continue;
        }
        debug!(?backoff, "reconnecting to Signal");
        tokio::select! {
            () = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(MAX_BACKOFF),
            () = woke.notified() => {
                info!("woke from sleep; reconnecting to Signal");
                backoff = Duration::from_secs(1);
            }
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Notifies `woke` each time the computer wakes from sleep, as logind announces it. Ends
/// quietly where there is no logind (the backoff still reconnects, just later).
#[cfg(all(unix, not(target_os = "macos")))]
async fn watch_wakeups(woke: Rc<tokio::sync::Notify>) {
    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1"
    )]
    trait Login {
        /// `true` before sleeping, `false` after waking.
        #[zbus(signal)]
        fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
    }

    let watch = async {
        let connection = zbus::Connection::system().await?;
        let mut signals = LoginProxy::new(&connection)
            .await?
            .receive_prepare_for_sleep()
            .await?;
        while let Some(signal) = signals.next().await {
            if !signal.args()?.start {
                woke.notify_one();
            }
        }
        zbus::Result::Ok(())
    };
    if let Err(error) = watch.await {
        debug!(error = %scrubbed(&error), "can't tell when the computer wakes from sleep");
    }
}

/// Elsewhere only the backoff reconnects after sleep.
#[cfg(not(all(unix, not(target_os = "macos"))))]
async fn watch_wakeups(_: Rc<tokio::sync::Notify>) {}

/// Returns `false` once the UI has gone away.
async fn handle_received(
    manager: &Mgr,
    ctx: &Ctx,
    avatars: &AvatarFetcher,
    kept: &mut Kept,
    repairs: &Rc<RefCell<Repairs>>,
    received: Received,
) -> bool {
    let Kept {
        names,
        receipts,
        decrypted,
    } = kept;
    let events = &ctx.events;
    match received {
        Received::QueueEmpty => {
            info!("synced with Signal");
            receipts.live = true;
            receipts.send(&ctx.outbox).await;
            events.sent(Event::Synced)
        }
        Received::StickerPackInstalled { .. } => events.sent(Event::StickerPacksChanged),
        Received::MissedGroupChanges {
            master_key,
            changes,
        } => super::groups::on_missed_changes(manager, ctx, names, master_key, changes).await,
        Received::Contacts => {
            names.clear();
            send_conversations(manager, ctx).await;
            // A newly linked device first learns its contacts here.
            avatars.request();
            true
        }
        Received::DecryptionError {
            sender,
            sender_device,
            timestamp,
            error,
        } => {
            // The server delivers a message again now and then (after a reconnect, say); one
            // already read is turned away by the session. Nothing to mend, nothing to say.
            if matches!(error, SignalProtocolError::DuplicatedMessage(..)) {
                // Read before, here or before b33p last stopped: the server delivers what it
                // wasn't told was stored, which may have been.
                let stored = || async {
                    manager
                        .store()
                        .thread_for_sender_and_timestamp(&sender, timestamp)
                        .await
                        .is_ok_and(|thread| thread.is_some())
                };
                if decrypted.contains(&(sender, sender_device, timestamp)) || stored().await {
                    debug!(sender = %sender.service_id_string(), "dropped a message delivered again");
                } else {
                    // The session is sound; only this message is gone: its sender used its
                    // counter for another one, or it was read and b33p stopped before it was
                    // stored.
                    warn!(
                        sender = %sender.service_id_string(),
                        device = %sender_device,
                        timestamp,
                        "lost a message: its counter was used before, and it isn't stored"
                    );
                }
                return true;
            }
            warn!(sender = %sender.service_id_string(), error = %scrubbed(&error), "failed to decrypt a message");
            // Our own devices' messages: nothing to reset (a session with ourselves), and
            // nothing to tell in a chat with ourselves.
            if sender == own_id(manager) {
                return true;
            }
            // Nothing from blocked people, as for messages that do decrypt: no notice, and no
            // session reset (which is a message to them, and says we are here).
            if is_blocked(ctx, &conversation_id(&Thread::Contact(sender))).await {
                debug!("from a blocked contact; ignored");
                return true;
            }
            // A broken session stays broken until someone resets it; the sender's app then
            // starts a new one (and can resend). Not more than hourly, so a peer that keeps
            // failing can't keep us resetting. Not with someone we have no chat with: the
            // reset is a message from our account, which someone who has only our number
            // (and wrote to its identity) isn't given until we answer them.
            let known = ctx
                .threads
                .borrow()
                .contains_key(&conversation_id(&Thread::Contact(sender)));
            let reset = known
                && matches!(sender, ServiceId::Aci(_))
                && repairs
                    .borrow()
                    .resets
                    .get(&sender)
                    .is_none_or(|last| last.elapsed() > SESSION_RESET_INTERVAL);
            if reset {
                repairs.borrow_mut().resets.insert(sender, Instant::now());
                match ctx.outbox.session_reset(sender, now_millis()).await {
                    Ok(()) => info!(sender = %sender.service_id_string(), "reset the session"),
                    Err(error) => warn!(error = %scrubbed(&error), "failed to reset the session"),
                }
            }
            // The envelope doesn't say whether it was a group message, so it goes in the 1:1 chat.
            let conversation =
                ensure_conversation(manager, ctx, Thread::Contact(sender), now_millis()).await;
            let name = sender_name(manager, names, sender).await;
            let body = if reset {
                format!(
                    "⚠ A message from {name} couldn't be decrypted. The connection was reset; it may arrive again."
                )
            } else {
                format!("⚠ A message from {name} couldn't be decrypted.")
            };
            events.sent(Event::Message(ChatMessage {
                conversation,
                sender_id: sender.service_id_string(),
                body,
                sender: name,
                timestamp: now_millis(),
                ..Default::default()
            }))
        }
        Received::RetryReceipt {
            sender,
            sender_device,
            message,
        } => {
            on_retry_receipt(manager, ctx, repairs, sender, sender_device, &message).await;
            true
        }
        Received::Content(content) => {
            let metadata = &content.metadata;
            decrypted.note((metadata.sender, metadata.sender_device, stored_at(&content)));
            let handled = handle_content(manager, ctx, names, receipts, &content).await;
            if receipts.live {
                receipts.send(&ctx.outbox).await;
            }
            handled
        }
    }
}

/// One of `sender`'s devices couldn't decrypt a message of ours, the one `message` names by
/// its timestamp. As Signal's clients do: the session it failed in is dropped, if it is still
/// the current one (so the next message starts a new one), and the message goes to them again,
/// a 1:1 message that is. Each message is sent again once, so that nobody can have us resend
/// without end; a group message isn't (presage would file a copy in the 1:1 chat), the session
/// is mended for the messages to come.
async fn on_retry_receipt(
    manager: &Mgr,
    ctx: &Ctx,
    repairs: &Rc<RefCell<Repairs>>,
    sender: ServiceId,
    sender_device: DeviceId,
    message: &DecryptionErrorMessage,
) {
    let Some(timestamp) = message.timestamp else {
        debug!(sender = %sender.service_id_string(), "a retry receipt names no message");
        return;
    };
    // Our other devices get what we send as sync messages; a failure there is theirs to mend.
    if sender == own_id(manager) {
        debug!(
            timestamp,
            "one of our own devices failed to decrypt a message; ignored"
        );
        return;
    }
    // A blocked contact is told nothing, as for a failure of ours.
    if is_blocked(ctx, &conversation_id(&Thread::Contact(sender))).await {
        return;
    }
    // A message one of our other devices sent is that device's to send again (as Signal's
    // clients have it); ours would double it.
    if let Some(device) = message.device_id
        && device != u32::from(manager.device_id())
    {
        debug!(
            timestamp,
            device, "a retry receipt for another device of ours; ignored"
        );
        return;
    }
    if repairs.borrow().resent.contains(&(sender, timestamp)) {
        debug!(sender = %sender.service_id_string(), timestamp, "sent again already");
        return;
    }
    info!(
        sender = %sender.service_id_string(),
        device = %sender_device,
        timestamp,
        "they failed to decrypt a message of ours"
    );
    // The session the message was sent in: only if it is still the current one, or a stale
    // receipt would drop a session that works.
    if let Some(key) = message
        .ratchet_key
        .as_deref()
        .and_then(|bytes| PublicKey::deserialize(bytes).ok())
    {
        archive_session_with(manager, sender, sender_device, &key).await;
    }
    let conversations = ctx
        .tables
        .conversations_of(&[timestamp])
        .await
        .map_err(|error| warn!(error = %scrubbed(&error), "failed to look up a message"))
        .unwrap_or_default();
    // No ratchet key: they lack our sender key for a group message. Renewed, it goes to every
    // member's devices with the next message to the group, as Signal's apps send it again.
    if message.ratchet_key.is_none() {
        for (conversation, _) in &conversations {
            if let Some(Thread::Group(key)) = super::conversations::thread_of(conversation)
                && let Err(error) = manager.renew_sender_key(&key).await
            {
                warn!(error = %scrubbed(&error), "failed to renew a group's sender key");
            }
        }
        return;
    }
    let ours = conversations
        .into_iter()
        .find(|(conversation, _)| *conversation == conversation_id(&Thread::Contact(sender)));
    let Some((_, timestamp)) = ours else {
        debug!(timestamp, "no 1:1 message of ours to send again");
        return;
    };
    let thread = Thread::Contact(sender);
    let stored = match manager.store().message(&thread, timestamp).await {
        Ok(Some(content)) if content.metadata.sender == own_id(manager) => content,
        _ => return,
    };
    let ContentBody::DataMessage(mut data) = stored.body else {
        return;
    };
    // presage fills in the chat's timer as it is now (sent as is, the old one would count as
    // a change, one version up).
    data.expire_timer = None;
    data.expire_timer_version = None;
    match ctx.outbox.message(sender, data, timestamp).await {
        Ok(()) => {
            info!(sender = %sender.service_id_string(), timestamp, "sent a message again");
            let mut repairs = repairs.borrow_mut();
            // Bounded: a peer can name any number of timestamps.
            if repairs.resent.len() >= RESENT_REMEMBERED {
                repairs.resent.clear();
            }
            repairs.resent.insert((sender, timestamp));
        }
        Err(error) => warn!(error = %scrubbed(&error), "failed to send a message again"),
    }
}

/// How many messages sent again on a retry receipt are remembered; see [`Repairs`].
const RESENT_REMEMBERED: usize = 1024;

/// Archives the session with `sender`'s device `device` if `key` is its current ratchet key: a
/// session step, under presage's lock, so that no decryption or send changes it meanwhile.
async fn archive_session_with(manager: &Mgr, sender: ServiceId, device: DeviceId, key: &PublicKey) {
    use presage::libsignal_service::protocol::SessionStore;
    let address = ProtocolAddress::new(sender.service_id_string(), device);
    let _step = manager.session_lock().step().await;
    let mut sessions = manager.store().aci_protocol_store();
    let Ok(Some(mut session)) = sessions.load_session(&address).await else {
        return;
    };
    if !session.current_ratchet_key_matches(key).unwrap_or(false) {
        debug!(%address, "the session they failed in is gone already");
        return;
    }
    if session.archive_current_state().is_err() {
        return;
    }
    match sessions.store_session(&address, &session).await {
        Ok(()) => info!(%address, "dropped the session they couldn't decrypt"),
        Err(error) => warn!(error = %scrubbed(&error), "failed to drop a session"),
    }
}

/// Returns `false` once the UI has gone away.
async fn handle_content(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    receipts: &mut Receipts,
    content: &Content,
) -> bool {
    #[cfg(feature = "test-servers")]
    crate::content_log::received(
        content.metadata.sender.service_id_string(),
        content.metadata.sender_device.into(),
        content.timestamp(),
        &content.body,
    );
    let events = &ctx.events;
    // Sync messages speak for our account: what our other devices sent, read, blocked or
    // accepted. Only our own devices may say so (the sender is authenticated by the session);
    // from anyone else they are forged. Our libsignal-service fork drops them while
    // decrypting; this keeps the rule for content that reaches here another way.
    if !from_our_account(own_id(manager), content) {
        warn!(
            sender = %content.metadata.sender.service_id_string(),
            "dropped a sync message from another account"
        );
        return true;
    }
    // Nothing at all from someone we block, in groups too, as Signal's clients do. presage
    // has stored it by now, so it goes from the store again.
    let sender = content.metadata.sender;
    if sender != own_id(manager)
        && is_blocked(ctx, &ConversationId::contact(&sender.service_id_string())).await
    {
        forget_stored(manager, content).await;
        return true;
    }
    // From someone whose safety number changed: the store noted it while decrypting.
    if content.metadata.sender != own_id(manager) {
        announce_identity_change(manager, ctx, names, content.metadata.sender).await;
        refresh_trust_of(manager, ctx, content.metadata.sender).await;
    }
    match &content.body {
        ContentBody::ReceiptMessage(receipt) => {
            return on_receipt(manager, ctx, sender, receipt).await;
        }
        // Typing in a group names the group differently from other messages.
        ContentBody::TypingMessage(typing) => {
            return on_typing(manager, ctx, names, content, typing).await;
        }
        ContentBody::SynchronizeMessage(sync) if !sync.read.is_empty() => {
            on_read_elsewhere(manager, ctx, &sync.read).await;
        }
        ContentBody::StoryMessage(story) => {
            let author = content.metadata.sender;
            on_story(manager, ctx, author, content.timestamp(), story, false).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    story_message: Some(story),
                    timestamp,
                    ..
                })),
            ..
        }) => {
            let at = timestamp.unwrap_or_else(|| content.timestamp());
            on_story(manager, ctx, own_id(manager), at, story, true).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Verified(verified)),
            ..
        }) => {
            on_verified_elsewhere(manager, ctx, verified).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Blocked(blocked)),
            ..
        }) => {
            on_blocked_elsewhere(manager, ctx, blocked).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::MessageRequestResponse(answer)),
            ..
        }) => {
            on_answered_elsewhere(manager, ctx, answer).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::FetchLatest(latest)),
            ..
        }) => {
            if latest.r#type() == sync_message::fetch_latest::Type::StorageManifest {
                ctx.storage_changed.notify_one();
            }
            return true;
        }
        // A pack removed on another device: presage has removed it by now, so the packs gathered
        // again leave it out. One installed there presage downloads in a task of its own, and
        // says when it is stored (`Received::StickerPackInstalled`).
        ContentBody::SynchronizeMessage(sync) if !sync.sticker_pack_operation.is_empty() => {
            use sync_message::sticker_pack_operation::Type;
            if sync
                .sticker_pack_operation
                .iter()
                .any(|operation| operation.r#type() == Type::Remove)
            {
                ctx.events.send(Event::StickerPacksChanged);
            }
            return true;
        }
        // The storage key arrives with it (presage has kept it by now), so the storage
        // service can be read: `follow_storage` may be waiting for exactly this.
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Keys(_)),
            ..
        }) => {
            ctx.storage_changed.notify_one();
            return true;
        }
        _ => {}
    }
    let Ok(thread) = Thread::try_from(content) else {
        return true;
    };
    // A message for a group we aren't in (presage fetches a group from the server the first
    // time a message names it, which fails for one we aren't a member of): a made-up master
    // key, or a group we were removed from. Signal's clients drop these, so that anyone able
    // to write to us can't fill the chat list with groups.
    let known = ctx.threads.borrow().contains_key(&conversation_id(&thread));
    if let Thread::Group(key) = &thread
        && !known
        && !matches!(manager.store().group(*key).await, Ok(Some(_)))
    {
        debug!(
            sender = %content.metadata.sender.service_id_string(),
            "dropped a message for a group we aren't in"
        );
        return true;
    }
    // From someone not in the group (any more, or yet), unless it is the change of their own
    // place in it (`own_membership_change`): Signal's clients drop those. Only a change the
    // server signed, made by the sender, says so; anything else that claims to be a change is
    // as good as none.
    if let Thread::Group(key) = &thread
        && !is_outgoing(manager, content)
        && let Ok(Some(group)) = manager.store().group(*key).await
        && !group
            .members
            .iter()
            .any(|member| ServiceId::Aci(member.aci) == content.metadata.sender)
    {
        let own = match data_of(content).and_then(|(data, _)| data.group_v2.as_ref()) {
            Some(context) => signed_group_change(manager, *key, context)
                .await
                .is_some_and(|changes| own_membership_change(&changes, content.metadata.sender)),
            None => false,
        };
        if !own {
            debug!(
                sender = %content.metadata.sender.service_id_string(),
                "dropped a message from someone not in the group"
            );
            return true;
        }
    }
    let id = ensure_conversation(manager, ctx, thread.clone(), content.timestamp()).await;
    // Brought in by someone we accepted: no message request (Signal-Desktop's rule).
    if let Thread::Group(key) = thread
        && !is_outgoing(manager, content)
        && let Some((data, edited)) = data_of(content)
        && let Some(context) = data.group_v2.as_ref()
    {
        let announcement = !edited && is_announcement(data);
        accept_from_trusted(
            manager,
            ctx,
            key,
            &id,
            context,
            announcement,
            content.metadata.sender,
        )
        .await;
    }
    // presage has taken over a group change by now; show the group as it is.
    if let Thread::Group(key) = thread
        && let Some(context) = data_of(content)
            .and_then(|(data, _)| data.group_v2.as_ref())
            .filter(|context| context.group_change.is_some())
    {
        let changes = signed_group_change(manager, key, context).await;
        debug!(
            revision = context.revision,
            signed = changes.is_some(),
            changes = changes.as_ref().map_or(0, |changes| changes.changes.len()),
            "a group change came"
        );
        forget_membership(manager, key, context).await;
        refresh_group(manager, ctx, key).await;
        // A new picture (presage has fetched it with the group), or none any more; or we are let
        // in, and the picture shown from before may have changed meanwhile.
        let ids = &manager.registration_data().service_ids;
        if let Some(changes) = changes
            && (lets_us_in(&changes, ids.aci(), ids.pni())
                || changes.changes.iter().any(|change| {
                    matches!(
                        change,
                        presage::libsignal_service::groups_v2::GroupChange::Avatar(_)
                    )
                }))
        {
            send_group_avatar(manager.clone(), ctx, key).await;
        }
    }
    // Nothing new from blocked chats (what we send from another device still counts).
    if !is_outgoing(manager, content) && is_blocked(ctx, &id).await {
        return true;
    }
    // As Signal's clients: a delivery receipt for each message (a reaction, an edit, a delete
    // too) that came by sealed sender, which is when `needs_receipt` is set. It names the
    // envelope's timestamp, which for an edit is the edit's own. Not to an unanswered message
    // request: as with read receipts, a stranger isn't told the message arrived.
    if content.metadata.needs_receipt
        && !is_outgoing(manager, content)
        && data_of(content).is_some()
        && accepted(ctx, &id, *ctx.storage_read.borrow()).await
    {
        receipts.owe(content.metadata.sender, stored_at(content));
    }
    if let Some((target, reaction, remove)) = to_reaction(manager, names, content).await {
        let target = original(ctx, &id, target).await;
        return events.sent(Event::Reaction {
            conversation: id,
            target,
            reaction,
            remove,
        });
    }
    let Some((data, edit)) = data_of(content) else {
        return true;
    };
    let outgoing = is_outgoing(manager, content);
    let author = author_of(manager, content);
    if let Some(Delete {
        target_sent_timestamp: Some(timestamp),
    }) = data.delete
    {
        // As presage takes them: only for two days after sending.
        const DELETE_WINDOW: u64 = 2 * 24 * 60 * 60 * 1000;
        if stored_at(content).saturating_sub(timestamp) >= DELETE_WINDOW {
            debug!("ignored a delete of a message sent too long ago");
            return true;
        }
        let target = MessageRef {
            author_id: author.service_id_string(),
            timestamp,
        };
        let target = original(ctx, &id, target).await;
        // Only a message that was there, and by that author (presage checks too): a delete
        // of anything else would leave a "deleted" marker where nothing was.
        match erase_deleted(manager, ctx, &id, &thread, &target).await {
            Ok(true) => {}
            Ok(false) => {
                debug!("ignored a delete of a message that isn't there");
                return true;
            }
            Err(error) => warn!(error = %scrubbed(&error), "failed to erase a deleted message"),
        }
        return events.sent(Event::Deleted {
            conversation: id,
            target,
        });
    }
    let Some(message) = to_chat_message(manager, names, id.clone(), content).await else {
        return true;
    };
    // An edit's time names the message it edits, not one of its own.
    if outgoing && !edit {
        // Written from another device of ours: as when written here (`send.rs`), that accepts
        // the chat. Not only in the window (`ChatState`'s written_to): receipts go by the table.
        wrote_to(ctx, &id).await;
        // Sent from another device: receipts name only the time, so remember where it went.
        if let Err(error) = ctx
            .tables
            .set_delivery(&id, message.timestamp, Delivery::Sent)
            .await
        {
            warn!(error = %scrubbed(&error), "failed to remember a sent message");
        }
        if let ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    expiration_start_timestamp: Some(start),
                    ..
                })),
            ..
        }) = &content.body
        {
            let timers = [(message.reference(), data.expire_timer.unwrap_or(0))];
            start_timers(ctx, &id, &timers, *start).await;
        }
    }
    // The store holds only the new text now; the state still knows the one before.
    if edit {
        // An edit of an edit names the one before; what it edits is the message first sent.
        let target = original(ctx, &id, message.reference()).await;
        if let Some(edited_at) = message.edited_at
            && let Err(error) = ctx.tables.record_edit_time(&id, &target, edited_at).await
        {
            warn!(error = %scrubbed(&error), "failed to note when a message was edited");
        }
        // Edited from another device of ours: sent again, until a receipt names the edit.
        if outgoing && let Err(error) = ctx.tables.sent_again(&id, target.timestamp).await {
            warn!(error = %scrubbed(&error), "failed to note an edited message as sent");
        }
        return events.sent(Event::Edited {
            conversation: id,
            target,
            message,
        });
    }
    // A new message brings an archived conversation back, unless it is muted (as in Signal).
    if !outgoing {
        match ctx.tables.unarchive_unless_muted(&id, now_millis()).await {
            Ok(Some(settings)) => {
                events.send(Event::Settings {
                    conversation: id.clone(),
                    settings,
                });
            }
            Ok(None) => {}
            Err(error) => warn!(error = %scrubbed(&error), "failed to unarchive a conversation"),
        }
    }
    events.sent(Event::Message(message))
}

/// Removes what presage stored of `content`, if it is still there as its sender's.
async fn forget_stored(manager: &Mgr, content: &Content) {
    let Ok(thread) = Thread::try_from(content) else {
        return;
    };
    let at = stored_at(content);
    let mut store = manager.store().clone();
    if let Ok(Some(stored)) = store.message(&thread, at).await
        && stored.metadata.sender == content.metadata.sender
        && let Err(error) = store.delete_message(&thread, at).await
    {
        warn!(error = %scrubbed(&error), "failed to forget a message from someone blocked");
    }
}

/// The conversation a sync message names by ACI (as text or bytes) or group id.
fn named_conversation(
    ctx: &Ctx,
    aci: Option<&str>,
    aci_binary: Option<&[u8]>,
    group_id: Option<&[u8]>,
) -> Option<ConversationId> {
    let who = aci
        .and_then(ServiceId::parse_from_service_id_string)
        .or_else(|| aci_binary.and_then(ServiceId::parse_from_service_id_binary));
    if let Some(who) = who {
        return Some(ConversationId::contact(&who.service_id_string()));
    }
    let thread = ctx.group_ids.find(&ctx.threads, group_id?)?;
    Some(conversation_id(&thread))
}

/// The phone's whole list of whom we block: everything else is unblocked.
async fn on_blocked_elsewhere(manager: &Mgr, ctx: &Ctx, blocked: &sync_message::Blocked) {
    let mut now_blocked = std::collections::HashSet::new();
    for aci in &blocked.acis {
        now_blocked.extend(named_conversation(ctx, Some(aci), None, None));
    }
    for aci in &blocked.acis_binary {
        now_blocked.extend(named_conversation(ctx, None, Some(aci), None));
    }
    for group in &blocked.group_ids {
        now_blocked.extend(named_conversation(ctx, None, None, Some(group)));
    }
    let Ok(all) = ctx.tables.conversations().await else {
        return;
    };
    // Those known only by their phone number are listed by it.
    if !blocked.numbers.is_empty() {
        for id in all.keys() {
            if let Some(Thread::Contact(who @ ServiceId::Pni(_))) = thread_of(id)
                && let Some(number) = e164_of(manager, who).await
                && blocked.numbers.contains(&number)
            {
                now_blocked.insert(id.clone());
            }
        }
    }
    let mut blocking = Vec::new();
    for (id, settings) in all {
        let blocked = now_blocked.remove(&id);
        if settings.blocked != blocked {
            blocking.push((id, blocked));
        }
    }
    // Blocked, but nothing remembered about them yet.
    blocking.extend(now_blocked.into_iter().map(|id| (id, true)));
    // Only `blocked`: the rest may have changed here since `all` was read.
    let changed = match ctx.tables.set_blocked(&blocking).await {
        Ok(changed) => changed,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to remember whom the phone blocks");
            return;
        }
    };
    for (conversation, settings) in changed {
        ctx.events.send(Event::Settings {
            conversation,
            settings,
        });
    }
}

/// A message request was answered on another device.
async fn on_answered_elsewhere(
    manager: &Mgr,
    ctx: &Ctx,
    answer: &sync_message::MessageRequestResponse,
) {
    use sync_message::message_request_response::Type;
    let Some(id) = named_conversation(
        ctx,
        answer.thread_aci.as_deref(),
        answer.thread_aci_binary.as_deref(),
        answer.group_id.as_deref(),
    ) else {
        return;
    };
    let kind = answer.r#type();
    let block = matches!(
        kind,
        Type::Block | Type::BlockAndDelete | Type::BlockAndSpam
    );
    let delete = matches!(kind, Type::Delete | Type::BlockAndDelete | Type::Spam);
    let settings = ctx
        .tables
        .update(&id, |s| {
            s.accepted |= kind == Type::Accept;
            s.blocked |= block;
        })
        .await;
    if let Ok(settings) = settings {
        ctx.events.send(Event::Settings {
            conversation: id.clone(),
            settings,
        });
    }
    if delete && let Some(thread) = thread_of(&id) {
        let mut store = manager.store().clone();
        match store.clear_thread(&thread).await {
            Ok(()) => {
                checkpoint(ctx).await;
                ctx.events.send(Event::Cleared(id));
            }
            Err(error) => {
                warn!(error = %scrubbed(&error), "failed to delete a chat deleted elsewhere")
            }
        }
    }
}

/// Our messages got further: delivered to or read on someone's phone.
/// `sender` says our messages of the receipt's timestamps reached them, or were read. Only in
/// conversations they are part of: a timestamp names a message in every chat that has one
/// (and anyone could send one), so the receipt goes no further than their own chat and groups.
async fn on_receipt(manager: &Mgr, ctx: &Ctx, sender: ServiceId, receipt: &ReceiptMessage) -> bool {
    use receipt_message::Type;
    let delivery = match receipt.r#type() {
        Type::Delivery => Delivery::Delivered,
        Type::Read | Type::Viewed => Delivery::Read,
    };
    let mut found = match ctx.tables.conversations_of(&receipt.timestamp).await {
        Ok(found) => found,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to look up receipted messages");
            return true;
        }
    };
    let mut theirs = Vec::new();
    for (conversation, timestamp) in found {
        if is_party(manager, &conversation, sender).await {
            theirs.push((conversation, timestamp));
        }
    }
    found = theirs;
    if let Err(error) = ctx.tables.set_deliveries(&found, delivery).await {
        warn!(error = %scrubbed(&error), "failed to remember a receipt");
    }
    let mut by_conversation: HashMap<ConversationId, Vec<u64>> = HashMap::new();
    for (conversation, timestamp) in found {
        by_conversation
            .entry(conversation)
            .or_default()
            .push(timestamp);
    }
    by_conversation
        .into_iter()
        .all(|(conversation, timestamps)| {
            ctx.events.sent(Event::Delivered {
                conversation,
                timestamps,
                delivery,
            })
        })
}

async fn on_typing(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    content: &Content,
    typing: &TypingMessage,
) -> bool {
    let sender = content.metadata.sender;
    if sender == own_id(manager) {
        return true;
    }
    let thread = match &typing.group_id {
        Some(group_id) => match ctx.group_ids.find(&ctx.threads, group_id) {
            Some(thread) => thread,
            None => return true,
        },
        None => Thread::Contact(sender),
    };
    let conversation = conversation_id(&thread);
    if !ctx.threads.borrow().contains_key(&conversation) || is_blocked(ctx, &conversation).await {
        return true;
    }
    // Nobody who isn't in the group, as for its messages.
    if let Thread::Group(key) = &thread
        && !is_member(manager, *key, sender).await
    {
        return true;
    }
    let name = sender_name(manager, names, sender).await;
    ctx.events.sent(Event::Typing {
        conversation,
        sender_id: sender.service_id_string(),
        sender: name,
        typing: typing.action() == typing_message::Action::Started,
    })
}

/// Whether `who` takes part in `conversation`: it is the chat with them, or a group they are
/// in. A chat started by phone number is keyed by the number's identity (PNI), while the
/// person answers from their account (ACI), which the store doesn't tie to it; such a chat
/// counts as theirs.
async fn is_party(manager: &Mgr, conversation: &ConversationId, who: ServiceId) -> bool {
    match thread_of(conversation) {
        Some(Thread::Contact(contact)) => contact == who || matches!(contact, ServiceId::Pni(_)),
        Some(Thread::Group(key)) => is_member(manager, key, who).await,
        None => false,
    }
}

/// Messages were read on another of our devices.
async fn on_read_elsewhere(manager: &Mgr, ctx: &Ctx, reads: &[sync_message::Read]) {
    let mut read: HashMap<ConversationId, Vec<MessageRef>> = HashMap::new();
    for entry in reads {
        let (Some(sender), Some(timestamp)) = (
            entry.parse_sender_aci().map(ServiceId::Aci),
            entry.timestamp,
        ) else {
            continue;
        };
        let mut message = MessageRef {
            author_id: sender.service_id_string(),
            timestamp,
        };
        // As Signal's apps read an edited message: by its latest edit's time.
        let conversation = match ctx.tables.edited_anywhere(&message).await {
            Ok(Some((conversation, original))) => {
                message.timestamp = original;
                conversation
            }
            _ => {
                let Ok(Some(thread)) = manager
                    .store()
                    .thread_for_sender_and_timestamp(&sender, timestamp)
                    .await
                else {
                    continue;
                };
                conversation_id(&thread)
            }
        };
        read.entry(conversation).or_default().push(message);
    }
    let now = now_millis();
    for (conversation, messages) in read {
        let newest = messages
            .iter()
            .map(|m| m.timestamp)
            .max()
            .unwrap_or_default();
        match ctx.tables.mark_read(&conversation, newest).await {
            Ok(settings) => {
                ctx.events.send(Event::Settings {
                    conversation: conversation.clone(),
                    settings,
                });
            }
            Err(error) => warn!(error = %scrubbed(&error), "failed to remember what was read"),
        }
        let timers = stored_timers(manager, &conversation, &messages).await;
        start_timers(ctx, &conversation, &timers, now).await;
    }
}

/// `false` for a sync message that didn't come from one of our own devices; see
/// [`handle_content`]. Anything else is fine.
fn from_our_account(own: ServiceId, content: &Content) -> bool {
    !matches!(content.body, ContentBody::SynchronizeMessage(_)) || content.metadata.sender == own
}

/// What receiving keeps from one message to the next, across reconnects: what was owed before
/// a drop goes out once the queue drains again.
#[derive(Default)]
struct Kept {
    names: Names,
    receipts: Receipts,
    decrypted: Decrypted,
}

/// An envelope, as a retry receipt names it: by sender, device and the envelope's timestamp.
type EnvelopeRef = (ServiceId, DeviceId, u64);

/// The envelopes decrypted lately. A message that fails as one already decrypted
/// (`DuplicatedMessage`) but isn't among them was never read here: its sender encrypted it with
/// a counter it had used for another message, two sends at once, and it is lost. Only what
/// presage hands on as content is noted, so a retry receipt or the contacts sync delivered again
/// reads as lost too.
#[derive(Default)]
struct Decrypted {
    order: VecDeque<EnvelopeRef>,
    seen: HashSet<EnvelopeRef>,
}

/// Envelopes [`Decrypted`] remembers: one is delivered again soon after it first came, if at all.
const DECRYPTED_REMEMBERED: usize = 4096;

impl Decrypted {
    fn note(&mut self, envelope: EnvelopeRef) {
        if self.seen.insert(envelope) {
            self.order.push_back(envelope);
        }
        if self.order.len() > DECRYPTED_REMEMBERED
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
    }

    fn contains(&self, envelope: &EnvelopeRef) -> bool {
        self.seen.contains(envelope)
    }
}

/// The delivery receipts we owe, by sender, until they are sent. While the queue drains after
/// connecting, each sender's are gathered and go out together once it is empty, as
/// Signal-Desktop holds its own back until then; after that, each goes out at once.
#[derive(Default)]
struct Receipts {
    owed: HashMap<ServiceId, Vec<u64>>,
    /// The queue has drained since connecting.
    live: bool,
}

/// Timestamps per receipt at most, as Signal-Desktop batches them.
const RECEIPT_TIMESTAMPS: usize = 100;

impl Receipts {
    fn owe(&mut self, sender: ServiceId, timestamp: u64) {
        self.owed.entry(sender).or_default().push(timestamp);
    }

    async fn send(&mut self, outbox: &Outbox) {
        for (sender, timestamps) in std::mem::take(&mut self.owed) {
            for timestamps in timestamps.chunks(RECEIPT_TIMESTAMPS) {
                let receipt = ReceiptMessage {
                    r#type: Some(receipt_message::Type::Delivery as i32),
                    timestamp: timestamps.to_vec(),
                };
                outbox
                    .post_message(
                        sender,
                        ContentBody::ReceiptMessage(receipt),
                        now_millis(),
                        "a delivery receipt",
                    )
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use presage::libsignal_service::content::DataMessage;

    /// An envelope decrypted here is known again by sender, device and timestamp, until
    /// `DECRYPTED_REMEMBERED` newer ones have pushed it out.
    #[test]
    fn decrypted_envelopes_are_remembered_for_a_while() {
        let ann = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000001")
            .unwrap();
        let (one, two) = (DeviceId::new(1).unwrap(), DeviceId::new(2).unwrap());
        let mut decrypted = Decrypted::default();
        decrypted.note((ann, one, 5));
        decrypted.note((ann, one, 5));
        assert!(decrypted.contains(&(ann, one, 5)));
        assert!(!decrypted.contains(&(ann, two, 5)));
        assert!(!decrypted.contains(&(ann, one, 6)));
        for timestamp in 100..100 + DECRYPTED_REMEMBERED as u64 {
            decrypted.note((ann, two, timestamp));
        }
        assert!(!decrypted.contains(&(ann, one, 5)));
        assert!(decrypted.contains(&(ann, two, 100)));
        assert_eq!(decrypted.order.len(), DECRYPTED_REMEMBERED);
        assert_eq!(decrypted.seen.len(), DECRYPTED_REMEMBERED);
    }

    /// Delivery receipts go out one per sender, at most `RECEIPT_TIMESTAMPS` timestamps each,
    /// and only once.
    #[test]
    fn delivery_receipts_are_gathered_by_sender() {
        use crate::outbox::{Reply, Send};
        let who = |n: u8| {
            ServiceId::parse_from_service_id_string(&format!(
                "00000000-0000-0000-0000-0000000000{n:02}"
            ))
            .unwrap()
        };
        let (ann, bob) = (who(1), who(2));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (outbox, mut queued) = tokio::sync::mpsc::channel(8);
            let mut receipts = Receipts::default();
            for timestamp in 0..RECEIPT_TIMESTAMPS as u64 + 1 {
                receipts.owe(ann, timestamp);
            }
            receipts.owe(bob, 7);
            receipts.send(&outbox).await;
            receipts.send(&outbox).await;
            drop(outbox);
            let mut sent = Vec::new();
            while let Some(send) = queued.recv().await {
                let Send::Message {
                    to,
                    body: ContentBody::ReceiptMessage(receipt),
                    reply: Reply::Log(_),
                    ..
                } = send
                else {
                    panic!("not a receipt");
                };
                assert_eq!(receipt.r#type(), receipt_message::Type::Delivery);
                sent.push((to, receipt.timestamp));
            }
            sent.sort_by_key(|(to, timestamps)| (to.service_id_string(), timestamps.len()));
            let expected = [
                (ann, vec![RECEIPT_TIMESTAMPS as u64]),
                (ann, (0..RECEIPT_TIMESTAMPS as u64).collect()),
                (bob, vec![7]),
            ];
            assert_eq!(sent, expected);
        });
    }

    /// A sync message says what our account did elsewhere, so only our own devices may send
    /// one; a contact's is dropped before it can clear chats or unblock people.
    #[test]
    fn sync_messages_count_only_from_our_own_devices() {
        use presage::libsignal_service::{content::Metadata, push_service::DEFAULT_DEVICE_ID};
        let who = |n: u8| {
            ServiceId::parse_from_service_id_string(&format!(
                "00000000-0000-0000-0000-0000000000{n:02}"
            ))
            .unwrap()
        };
        let (own, other) = (who(1), who(2));
        let content = |sender: ServiceId, body: ContentBody| Content {
            metadata: Metadata {
                sender,
                destination: own,
                sender_device: *DEFAULT_DEVICE_ID,
                pni_verified: None,
                client_timestamp: chrono::DateTime::from_timestamp_millis(1).unwrap(),
                server_timestamp: chrono::DateTime::from_timestamp_millis(1).unwrap(),
                needs_receipt: false,
                unidentified_sender: false,
                was_plaintext: false,
                server_guid: None,
            },
            body,
        };
        let sync = || {
            ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::Blocked(sync_message::Blocked::default())),
                ..Default::default()
            })
        };
        let data = || {
            ContentBody::DataMessage(DataMessage {
                body: Some("hi".into()),
                ..Default::default()
            })
        };
        assert!(from_our_account(own, &content(own, sync())));
        assert!(!from_our_account(own, &content(other, sync())));
        assert!(from_our_account(own, &content(other, data())));
        assert!(from_our_account(own, &content(own, data())));
    }

    #[test]
    fn only_refused_credentials_mean_the_device_was_removed() {
        use presage::libsignal_service::push_service::ServiceError;
        use presage::libsignal_service::websocket::{HandshakeError, WebSocketError};
        let refused = |status: u16| {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            PresageError::ServiceError(ServiceError::WebSocket(WebSocketError::Handshake(
                HandshakeError::UnexpectedStatusCode(status),
            )))
        };
        assert!(removed_from_account(&refused(403)));
        assert!(removed_from_account(&refused(401)));
        assert!(!removed_from_account(&refused(500)));
        assert!(!removed_from_account(&refused(429)));
        assert!(removed_from_account(&PresageError::ServiceError(
            ServiceError::Unauthorized
        )));
        assert!(!removed_from_account(&PresageError::ServiceError(
            ServiceError::Timeout { attempts: 3 }
        )));
    }
}
