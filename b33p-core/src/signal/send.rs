//! Sending: messages, stickers, link previews, edits, and what our other devices are told.

use super::errors::{db, send_failed, store_failed};
use crate::Error;
use crate::outbox::{Outbox, SendFailure, Sends as _};
use std::{collections::HashMap, sync::Arc, time::Duration};

use futures::{StreamExt, future};
use presage::{
    Error as PresageError,
    libsignal_service::{
        content::{ContentBody, DataMessage, GroupContextV2},
        protocol::ServiceId,
    },
    proto::{
        BodyRange, EditMessage, ReceiptMessage, SyncMessage, TypingMessage, body_range,
        data_message::Flags,
        receipt_message,
        sync_message::{self, Content as SyncContent},
        typing_message,
    },
    store::{ContentsStore, Thread},
};
use tracing::{debug, warn};

use crate::{
    ChatMessage, ConversationId, Delivery, Event, MessageRef, Quote, Style, StyleRange,
    attachments,
    format::{self, RangeValue, WireRange},
    now_millis,
    scrub::scrubbed,
};

use super::{
    Ctx, Mgr,
    conversations::{
        Names, accepted, e164_of, expire_timer, is_blocked, or_logged, own_id, sender_name,
        thread_of, wrote_to,
    },
    groups::{group_identifier, member_aci},
    history::{checkpoint, start_timers},
    trust::{announce_identity_change, hold_changed},
};

/// Adds `thread` to the list of whom we block, the way current Signal apps read it. Those known
/// only by their phone `number` are listed by it; left out, the phone would unblock them, as it
/// takes the list as the whole one.
fn list_blocked(blocked: &mut sync_message::Blocked, thread: &Thread, number: Option<String>) {
    match thread {
        Thread::Contact(who @ ServiceId::Aci(_)) => {
            blocked.acis.push(who.service_id_string());
            blocked.acis_binary.push(who.service_id_binary());
        }
        Thread::Contact(_) => blocked.numbers.extend(number),
        Thread::Group(key) => blocked.group_ids.push(group_identifier(key).to_vec()),
    }
}

/// Asks our primary device to send something it keeps (the blocked list, the storage key).
pub(super) async fn request_sync(
    manager: &Mgr,
    outbox: &Outbox,
    kind: sync_message::request::Type,
) {
    let sync = SyncMessage {
        content: Some(SyncContent::Request(sync_message::Request {
            r#type: Some(kind.into()),
        })),
        ..SyncMessage::with_padding(&mut rand::rng())
    };
    let own = own_id(manager);
    if let Err(error) = outbox
        .message(own, ContentBody::SynchronizeMessage(sync), now_millis())
        .await
    {
        warn!(error = %scrubbed(&error), ?kind, "failed to ask the primary device");
    }
}

/// An ACI in the binary form current Signal apps read: they ignore the string fields b33p also
/// fills, for older clients. `None` for anything but an ACI.
pub(super) fn aci_binary(id: &str) -> Option<Vec<u8>> {
    member_aci(id).map(|aci| ServiceId::Aci(aci).service_id_binary())
}

/// What group messages carry to say which group, and in which revision.
pub(super) async fn group_context(manager: &Mgr, key: &[u8; 32]) -> GroupContextV2 {
    // Recipients compare the revision with their group state to spot stale context.
    let revision = match manager.store().group(*key).await {
        Ok(Some(group)) => group.revision,
        _ => 0,
    };
    GroupContextV2 {
        master_key: Some(key.to_vec()),
        revision: Some(revision),
        ..Default::default()
    }
}

/// Why sending failed: the store refused the identity a contact has now for sending (one other
/// than the verified one pinned for them, see [`pin_verified`]), or anything else.
enum SendError {
    Untrusted(ServiceId),
    Other(Error),
}

impl From<SendError> for Error {
    fn from(error: SendError) -> Self {
        match error {
            // Not until it is confirmed: as it is, it won't go.
            SendError::Untrusted(_) => Error::refused(
                "Their safety number changed since you verified it: confirm it to send.",
            ),
            SendError::Other(error) => error,
        }
    }
}

/// The contact whose identity a failed send refused, if that is why it failed: libsignal says
/// so directly, or libsignal-service when setting up a session with their new key.
fn untrusted(error: &PresageError<presage_store_sqlite::SqliteStoreError>) -> Option<ServiceId> {
    use presage::libsignal_service::{prelude::MessageSenderError, protocol::SignalProtocolError};
    let protocol = |error: &SignalProtocolError| match error {
        SignalProtocolError::UntrustedIdentity(address) => {
            ServiceId::parse_from_service_id_string(address.name())
        }
        _ => None,
    };
    match error {
        PresageError::MessageSenderError(error) => match error.as_ref() {
            MessageSenderError::UntrustedIdentity { address } => Some(*address),
            MessageSenderError::ProtocolError(error) => protocol(error),
            _ => None,
        },
        PresageError::ProtocolError(error) => protocol(error),
        _ => None,
    }
}

/// Sends to a contact, or to everyone in a group.
async fn send_to(
    outbox: &Outbox,
    thread: &Thread,
    body: ContentBody,
    timestamp: u64,
) -> Result<(), SendError> {
    let sent = match thread {
        Thread::Contact(recipient) => outbox.message(*recipient, body, timestamp).await,
        Thread::Group(key) => outbox.group_message(*key, body, timestamp).await,
    };
    sent.map_err(|failure| match &failure {
        SendFailure::Presage(error) if let Some(who) = untrusted(error) => {
            SendError::Untrusted(who)
        }
        _ => SendError::Other(send_failed("failed to send")(failure)),
    })
}

/// [`send_to`], holding the chat of a contact whose identity it refused: their verified safety
/// number changed, which they may not have heard of yet (the new key came with sending).
/// Nothing goes to a group while that holds for one of its members: presage would reach the
/// others before refusing them, and report the message as failed, so a retry would repeat it.
async fn deliver(
    manager: &mut Mgr,
    ctx: &Ctx,
    thread: &Thread,
    body: ContentBody,
    timestamp: u64,
) -> Result<(), Error> {
    if let Thread::Group(key) = thread {
        let held = held_members(manager, ctx, key).await;
        if !held.is_empty() {
            return Err(Error::refused(format!(
                "Not sent: your safety number with “{}” changed since you verified it. Confirm it \
                 in their chat first.",
                held.join(", ")
            )));
        }
    }
    match send_to(&ctx.outbox, thread, body, timestamp).await {
        Err(SendError::Untrusted(who)) => {
            hold_changed(manager, ctx, who).await;
            Err(SendError::Untrusted(who).into())
        }
        sent => sent.map_err(Error::from),
    }
}

/// The names of the members of group `key` whose verified safety number changed and isn't
/// confirmed yet (see [`hold_changed`]), as the UI was last told.
async fn held_members(manager: &Mgr, ctx: &Ctx, key: &[u8; 32]) -> Vec<String> {
    let Ok(Some(group)) = manager.store().group(*key).await else {
        return Vec::new();
    };
    let held: Vec<ServiceId> = {
        let trust = ctx.trust.borrow();
        group
            .members
            .iter()
            .map(|member| ServiceId::Aci(member.aci))
            .filter(|who| trust.get(who) == Some(&crate::Trust::Changed))
            .collect()
    };
    let mut names = Names::new();
    let mut found = Vec::with_capacity(held.len());
    for who in held {
        found.push(sender_name(manager, &mut names, who).await);
    }
    found
}

pub(super) async fn send_data(
    manager: &mut Mgr,
    ctx: &Ctx,
    thread: &Thread,
    mut message: DataMessage,
    timestamp: u64,
) -> Result<(), Error> {
    if let Thread::Group(key) = thread {
        message.group_v2 = Some(group_context(manager, key).await);
    }
    deliver(
        manager,
        ctx,
        thread,
        ContentBody::DataMessage(message),
        timestamp,
    )
    .await
}

/// Replaces the text of our message sent at `target`.
pub(super) async fn send_edit(
    manager: &mut Mgr,
    ctx: &Ctx,
    thread: &Thread,
    target: u64,
    mut message: DataMessage,
    timestamp: u64,
) -> Result<(), Error> {
    if let Thread::Group(key) = thread {
        message.group_v2 = Some(group_context(manager, key).await);
    }
    let edit = ContentBody::EditMessage(EditMessage {
        target_sent_timestamp: Some(target),
        data_message: Some(message),
    });
    deliver(manager, ctx, thread, edit, timestamp).await
}

/// Signal's body ranges for text we send.
pub(super) fn body_ranges(ranges: Vec<WireRange>) -> Vec<BodyRange> {
    use body_range::{AssociatedValue, Style as WireStyle};
    ranges
        .into_iter()
        .map(|range| BodyRange {
            start: Some(range.start),
            length: Some(range.length),
            associated_value: Some(match range.value {
                RangeValue::Mention(aci) => AssociatedValue::MentionAci(aci),
                RangeValue::Style(style) => AssociatedValue::Style(match style {
                    Style::Bold => WireStyle::Bold,
                    Style::Italic => WireStyle::Italic,
                    Style::Spoiler => WireStyle::Spoiler,
                    Style::Strikethrough => WireStyle::Strikethrough,
                    Style::Monospace => WireStyle::Monospace,
                } as i32),
            }),
        })
        .collect()
}

/// A new message of ours, ready to go (attachments uploaded).
pub(super) struct Outgoing {
    pub(super) body: String,
    pub(super) styles: Vec<StyleRange>,
    pub(super) mentions: Vec<crate::Mention>,
    pub(super) quote: Option<Quote>,
    pub(super) pointers: Vec<presage::proto::AttachmentPointer>,
    /// The preview of a link in it, as sent and as shown.
    pub(super) preview: Option<(presage::proto::Preview, crate::LinkPreview)>,
    /// It is this sticker.
    pub(super) sticker: Option<presage::proto::data_message::Sticker>,
}

/// Sends a new message and shows it; remembers it for receipts and starts its timer. Fails
/// only if it didn't go out: what is left to do after that is logged, not returned, so a sent
/// message always shows.
pub(super) async fn send_new(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: ConversationId,
    outgoing: Outgoing,
    timestamp: u64,
) -> Result<(), Error> {
    let Outgoing {
        body,
        styles,
        mentions,
        quote,
        pointers,
        preview,
        sticker,
    } = outgoing;
    let (sent_preview, shown_preview) = preview.unzip();
    let thread = ctx
        .threads
        .borrow()
        .get(&conversation)
        .cloned()
        .ok_or_else(|| Error::refused("unknown conversation"))?;
    let (text, ranges) = format::encode(&body, &styles, &mentions);
    // A quote names the version it shows, as Signal's apps do: an edited message's by its
    // latest edit's time (Signal-Desktop `makeQuote`), else they show the text first sent.
    let quoted_version = match &quote {
        Some(quote) => {
            let quoted = MessageRef {
                author_id: quote.author_id.clone(),
                timestamp: quote.timestamp,
            };
            or_logged(
                ctx.tables.latest_edit(&conversation, &quoted).await,
                "edit times",
            )
            .unwrap_or(quote.timestamp)
        }
        None => 0,
    };
    // presage fills in the data message's timestamp from the one passed here.
    let message = DataMessage {
        body: (!text.is_empty()).then_some(text),
        body_ranges: body_ranges(ranges),
        attachments: pointers.clone(),
        preview: sent_preview.into_iter().collect(),
        sticker,
        quote: quote
            .as_ref()
            .map(|quote| presage::proto::data_message::Quote {
                id: Some(quoted_version),
                author_aci: Some(quote.author_id.clone()),
                author_aci_binary: aci_binary(&quote.author_id),
                text: Some(quote.text.clone()),
                ..Default::default()
            }),
        ..Default::default()
    };
    // As receivers will see them, stickers included.
    let shown_attachments = attachments::of_message(&message);
    let sent = send_data(manager, ctx, &thread, message, timestamp).await;
    // Sending may have met their new safety number, in the keys fetched to write to them.
    if let Thread::Contact(recipient) = thread {
        announce_identity_change(manager, ctx, &mut Names::new(), recipient).await;
    }
    sent?;
    let own = own_id(manager).service_id_string();
    // presage sent it with the thread's timer.
    let timer = expire_timer(manager, &thread).await;
    let reference = MessageRef {
        author_id: own.clone(),
        timestamp,
    };
    ctx.events.send(Event::Message(ChatMessage {
        conversation: conversation.clone(),
        sender_id: own,
        sender: "You".into(),
        body,
        timestamp,
        outgoing: true,
        quote,
        attachments: shown_attachments,
        styles,
        expire_timer: (timer > 0).then_some(timer),
        preview: shown_preview,
        ..Default::default()
    }));
    if let Err(error) = ctx
        .tables
        .set_delivery(&conversation, timestamp, Delivery::Sent)
        .await
    {
        warn!(error = %scrubbed(&error), "failed to save that a message was sent");
    }
    wrote_to(ctx, &conversation).await;
    start_timers(ctx, &conversation, &[(reference, timer)], timestamp).await;
    Ok(())
}

/// Edge of the pictures in the sticker picker: 64 px at up to 2x scaling.
const STICKER_THUMBNAIL: u32 = 128;

/// The installed sticker packs with small pictures of their stickers. Decoding every sticker
/// takes a while: it happens off this thread, which receives and sends meanwhile, a few packs
/// at once.
pub(super) async fn sticker_packs(manager: &Mgr) -> Vec<crate::StickerPack> {
    let Ok(packs) = manager.store().sticker_packs().await else {
        return Vec::new();
    };
    futures::stream::iter(packs.flatten())
        .map(|pack| tokio::task::spawn_blocking(move || sticker_pack(&pack)))
        .buffered(4)
        .filter_map(|decoded| future::ready(decoded.ok().filter(|pack| !pack.stickers.is_empty())))
        .collect()
        .await
}

/// A sticker pack with small pictures of the stickers that are downloaded and decode.
fn sticker_pack(pack: &presage::store::StickerPack) -> crate::StickerPack {
    crate::StickerPack {
        id: hex::encode(&pack.id),
        title: pack.manifest.title.clone(),
        stickers: pack
            .manifest
            .stickers
            .iter()
            .filter_map(|sticker| {
                let picture = attachments::thumbnail(sticker.bytes.as_deref()?, STICKER_THUMBNAIL)?;
                Some(crate::StickerItem {
                    id: sticker.id,
                    emoji: sticker.emoji.clone().unwrap_or_default(),
                    picture: Arc::new(picture),
                })
            })
            .collect(),
    }
}

/// A sticker of an installed pack, ready to send: its picture uploaded.
pub(super) async fn sticker_message(
    manager: &Mgr,
    pack: &str,
    sticker: u32,
) -> Result<presage::proto::data_message::Sticker, Error> {
    let unknown = || Error::refused("unknown sticker pack");
    let pack_id = hex::decode(pack).map_err(|_| unknown())?;
    let pack = manager
        .store()
        .sticker_pack(&pack_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(unknown)?;
    let found = pack
        .manifest
        .stickers
        .iter()
        .find(|s| s.id == sticker)
        .ok_or_else(|| Error::refused("unknown sticker"))?;
    let bytes = found
        .bytes
        .clone()
        .ok_or_else(|| Error::refused("the sticker isn't downloaded"))?;
    let content_type = found
        .content_type
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "image/webp".into());
    let pointer = attachments::upload_bytes(manager, "sticker", &content_type, bytes).await?;
    Ok(presage::proto::data_message::Sticker {
        pack_id: Some(pack.id.clone()),
        pack_key: Some(pack.key.clone()),
        sticker_id: Some(sticker),
        data: Some(pointer),
        emoji: found.emoji.clone(),
    })
}

/// A preview of the first https link in `body`, its picture uploaded; `None` if there is no
/// link or the page says nothing about itself.
pub(super) async fn link_preview(
    manager: &Mgr,
    body: &str,
) -> Option<(presage::proto::Preview, crate::LinkPreview)> {
    let url = format::links(body)
        .into_iter()
        .find(|link| link.starts_with("https://"))?;
    let page = crate::preview::fetch_page(&url).await?;
    let mut pointer = None;
    if let Some(image) = &page.image
        && let Some((bytes, content_type)) = crate::preview::fetch_image(image).await
    {
        match attachments::upload_bytes(manager, "preview", &content_type, bytes).await {
            Ok(uploaded) => pointer = Some(uploaded),
            Err(error) => {
                debug!(error = %scrubbed(&error), "couldn't upload a link preview's picture")
            }
        }
    }
    let shown = crate::LinkPreview {
        url: url.clone(),
        title: page.title.clone(),
        description: page.description.clone(),
        image: pointer.as_ref().map(attachments::describe_preview),
    };
    let sent = presage::proto::Preview {
        url: Some(url),
        title: Some(page.title),
        description: (!page.description.is_empty()).then_some(page.description),
        image: pointer,
        date: None,
    };
    Some((sent, shown))
}

/// Tells our other devices about something done here.
pub(super) async fn send_sync(manager: &Mgr, outbox: &Outbox, sync: SyncMessage) {
    send_sync_at(manager, outbox, sync, now_millis()).await;
}

/// [`send_sync`], sent at `timestamp`: that of what it tells of, where the other devices take
/// the envelope's.
pub(super) async fn send_sync_at(
    manager: &Mgr,
    outbox: &Outbox,
    sync: SyncMessage,
    timestamp: u64,
) {
    let own = own_id(manager);
    if let Err(error) = outbox
        .message(own, ContentBody::SynchronizeMessage(sync), timestamp)
        .await
    {
        warn!(error = %scrubbed(&error), "failed to tell our other devices");
    }
}

/// Tells our other devices whom we block now: the whole list, as Signal expects.
pub(super) async fn send_blocked(manager: &Mgr, ctx: &Ctx) {
    let Ok(all) = ctx.tables.conversations().await else {
        return;
    };
    let mut blocked = sync_message::Blocked::default();
    for id in all.iter().filter(|(_, s)| s.blocked).map(|(id, _)| id) {
        let Some(thread) = thread_of(id) else {
            continue;
        };
        let number = match thread {
            Thread::Contact(who @ ServiceId::Pni(_)) => e164_of(manager, who).await,
            _ => None,
        };
        list_blocked(&mut blocked, &thread, number);
    }
    let sync = SyncMessage {
        content: Some(SyncContent::Blocked(blocked)),
        ..Default::default()
    };
    send_sync(manager, &ctx.outbox, sync).await;
}

/// Answers a message request, here and on our other devices.
pub(super) async fn respond(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    thread: &Thread,
    response: crate::RequestResponse,
) -> Result<(), Error> {
    use crate::RequestResponse;
    use sync_message::message_request_response::Type;
    let kind = match response {
        RequestResponse::Accept => {
            ctx.tables
                .update(conversation, |s| s.accepted = true)
                .await
                .map_err(db)?;
            // Accepting shares our profile (name, picture) with them: presage puts our profile
            // key into every data message.
            let message = DataMessage {
                flags: Some(Flags::ProfileKeyUpdate as u32),
                ..Default::default()
            };
            send_data(manager, ctx, thread, message, now_millis()).await?;
            Type::Accept
        }
        RequestResponse::Delete => {
            let mut store = manager.store().clone();
            store
                .clear_thread(thread)
                .await
                .map_err(store_failed("failed to delete the chat"))?;
            checkpoint(ctx).await;
            ctx.events.send(Event::Cleared(conversation.clone()));
            Type::Delete
        }
        RequestResponse::Block => {
            ctx.tables
                .update(conversation, |s| s.blocked = true)
                .await
                .map_err(db)?;
            send_blocked(manager, ctx).await;
            Type::Block
        }
    };
    let mut answer = sync_message::MessageRequestResponse {
        r#type: Some(kind as i32),
        ..Default::default()
    };
    match thread {
        Thread::Contact(who) => {
            answer.thread_aci = Some(who.service_id_string());
            answer.thread_aci_binary = Some(who.service_id_binary());
        }
        Thread::Group(key) => answer.group_id = Some(group_identifier(key).to_vec()),
    }
    let sync = SyncMessage {
        content: Some(SyncContent::MessageRequestResponse(answer)),
        ..Default::default()
    };
    send_sync(manager, &ctx.outbox, sync).await;
    Ok(())
}

/// Tells a conversation we started or stopped typing. Queued, not waited for: it goes out
/// before whatever is queued after it, such as the message that was being typed.
pub(super) async fn send_typing(outbox: &Outbox, thread: Thread, typing: bool) {
    use typing_message::Action;
    let timestamp = now_millis();
    let mut message = TypingMessage {
        timestamp: Some(timestamp),
        action: Some(if typing {
            Action::Started
        } else {
            Action::Stopped
        } as i32),
        group_id: None,
    };
    if let Thread::Group(key) = &thread {
        message.group_id = Some(group_identifier(key).to_vec());
    }
    let body = ContentBody::TypingMessage(message);
    const WHAT: &str = "a typing indicator";
    match thread {
        Thread::Contact(to) => outbox.post_message(to, body, timestamp, WHAT).await,
        Thread::Group(key) => outbox.post_group_message(key, body, timestamp, WHAT).await,
    }
}

/// Tells our other devices we read `messages` in `conversation`, and their senders, if read
/// receipts are on and they may know.
/// How long a read receipt waits for the phone's settings; see [`send_read`].
const STORAGE_WAIT: Duration = Duration::from_secs(2 * 60);

pub(super) async fn send_read(
    manager: Mgr,
    ctx: Ctx,
    conversation: ConversationId,
    messages: Vec<MessageRef>,
) {
    let own = own_id(&manager);
    // As Signal-Desktop reads an edited message: by its latest edit's time, and by its own
    // too, as it was unread; the other side knows it by either.
    let mut messages = messages;
    for message in messages.clone() {
        match ctx.tables.latest_edit(&conversation, &message).await {
            Ok(Some(edited_at)) => messages.push(MessageRef {
                timestamp: edited_at,
                ..message
            }),
            Ok(None) => {}
            Err(error) => warn!(error = %scrubbed(&error), "failed to look up an edit"),
        }
    }
    let read = messages
        .iter()
        .map(|message| sync_message::Read {
            sender_aci_binary: aci_binary(&message.author_id),
            sender_aci: Some(message.author_id.clone()),
            timestamp: Some(message.timestamp),
        })
        .collect();
    let sync = SyncMessage {
        read,
        ..Default::default()
    };
    ctx.outbox
        .post_message(
            own,
            ContentBody::SynchronizeMessage(sync),
            now_millis(),
            "what was read to our other devices",
        )
        .await;
    // Whether this is a message request, and who is blocked, comes with the phone's settings.
    // Right after linking they haven't been read yet, so wait: a read receipt would tell a
    // stranger we read their message. Not for ever, though (the phone may never answer): then
    // only to a chat accepted here, which is known without the phone.
    let settings_read = tokio::time::timeout(
        STORAGE_WAIT,
        ctx.storage_read.subscribe().wait_for(|read| *read),
    )
    .await
    .is_ok();
    if !ctx.preferences.get().read_receipts {
        return;
    }
    // Not to unanswered message requests, nor to anyone we block.
    if !accepted(&ctx, &conversation, settings_read).await {
        return;
    }
    let mut by_author: HashMap<&str, Vec<u64>> = HashMap::new();
    for message in &messages {
        if !is_blocked(&ctx, &ConversationId::contact(&message.author_id)).await {
            by_author
                .entry(&message.author_id)
                .or_default()
                .push(message.timestamp);
        }
    }
    let timestamp = now_millis();
    for (author, timestamps) in by_author {
        let Some(author) = ServiceId::parse_from_service_id_string(author) else {
            continue;
        };
        if author == own {
            continue;
        }
        let receipt = ReceiptMessage {
            r#type: Some(receipt_message::Type::Read as i32),
            timestamp: timestamps,
        };
        ctx.outbox
            .post_message(
                author,
                ContentBody::ReceiptMessage(receipt),
                timestamp,
                "a read receipt",
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A send the store refused for a contact's identity is recognised in each way it comes
    /// back, and names that contact; other failures are just failures.
    #[test]
    fn a_refused_identity_names_the_contact() {
        use presage::libsignal_service::{
            prelude::MessageSenderError,
            protocol::{DeviceId, ProtocolAddress, SignalProtocolError},
        };
        let who = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000002")
            .unwrap();
        let address = ProtocolAddress::new(who.service_id_string(), DeviceId::new(1).unwrap());
        let refused = || SignalProtocolError::UntrustedIdentity(address.clone());
        let errors: [PresageError<presage_store_sqlite::SqliteStoreError>; 3] = [
            MessageSenderError::UntrustedIdentity { address: who }.into(),
            MessageSenderError::ProtocolError(refused()).into(),
            PresageError::ProtocolError(refused()),
        ];
        for error in &errors {
            assert_eq!(untrusted(error), Some(who), "{error}");
        }
        let other: PresageError<presage_store_sqlite::SqliteStoreError> =
            MessageSenderError::MaximumRetriesLimitExceeded.into();
        assert_eq!(untrusted(&other), None);
    }

    /// The phone takes the list as the whole one and unblocks everyone left out of it, so
    /// everyone must be in it in a form it reads.
    #[test]
    fn the_blocked_list_names_everyone_as_the_phone_reads_them() {
        let aci = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000007")
            .unwrap();
        let pni =
            ServiceId::parse_from_service_id_string("PNI:00000000-0000-0000-0000-000000000008")
                .unwrap();
        let mut blocked = sync_message::Blocked::default();
        list_blocked(&mut blocked, &Thread::Contact(aci), None);
        list_blocked(
            &mut blocked,
            &Thread::Contact(pni),
            Some("+4915123456789".into()),
        );
        // Without a known number there is nothing to name them by.
        list_blocked(&mut blocked, &Thread::Contact(pni), None);
        assert_eq!(blocked.acis, [aci.service_id_string()]);
        assert_eq!(blocked.acis_binary, [aci.service_id_binary()]);
        assert_eq!(blocked.numbers, ["+4915123456789"]);
        assert!(blocked.group_ids.is_empty());
    }
}
