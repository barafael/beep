//! A chat's stored messages: pages of history, media, search, erasing, disappearing timers.

use super::errors::{db, store_failed};
use crate::Error;
use std::{collections::HashMap, time::Duration};

use presage::{
    libsignal_service::{
        content::{Content, ContentBody, DataMessage},
        protocol::ServiceId,
    },
    proto::{
        BodyRange, EditMessage, SyncMessage, body_range,
        data_message::Flags,
        sync_message::{Content as SyncContent, Sent},
    },
    store::{ContentExt, ContentsStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use tracing::{debug, warn};

use crate::{
    ChatMessage, ConversationId, Event, MessageRef, Quote, Reaction, Style, StyleRange,
    attachments,
    format::{self, RangeValue, WireRange},
    now_millis,
    scrub::scrubbed,
};

use super::{
    Ctx, Mgr,
    conversations::{Names, display_name, or_logged, own_id, thread_of},
    groups::group_notice,
    trust::identity_note,
};

/// The message a reaction is to. Current Signal apps name its author only in binary.
fn reaction_target(reaction: &presage::proto::data_message::Reaction) -> Option<MessageRef> {
    Some(MessageRef {
        author_id: ServiceId::Aci(reaction.parse_target_author_aci()?).service_id_string(),
        timestamp: reaction.target_sent_timestamp?,
    })
}

/// The message `target` names, by the time it was first sent: Signal's apps name an edited
/// message by its latest edit's (Signal-Desktop `getMessageSentTimestamp`).
pub(super) async fn original(
    ctx: &Ctx,
    conversation: &ConversationId,
    target: MessageRef,
) -> MessageRef {
    match or_logged(ctx.tables.edited(conversation, &target).await, "edit times") {
        Some(timestamp) => MessageRef {
            timestamp,
            ..target
        },
        None => target,
    }
}

/// When a content was stored: what the store orders and pages by.
pub(super) fn stored_at(content: &Content) -> u64 {
    content.metadata.client_timestamp.timestamp_millis() as u64
}

/// Messages shown per page of history.
const HISTORY_PAGE: usize = 100;

/// Stored contents of a thread before `before` (or the newest), newest first: enough for
/// [`HISTORY_PAGE`] messages, plus where the next older page starts, if there is one.
pub(super) async fn history_page(
    manager: &Mgr,
    thread: &Thread,
    before: Option<u64>,
) -> Result<(Vec<Content>, Option<u64>), Error> {
    // Receipts, reactions and the like are stored between messages; read a bit more.
    const BATCH: u32 = 200;
    let mut contents = Vec::new();
    let mut cursor = before;
    loop {
        let batch = manager
            .store()
            .messages_before(thread, cursor, BATCH)
            .await
            .map_err(store_failed("failed to load history"))?;
        let exhausted = batch.len() < BATCH as usize;
        cursor = batch.last().map(stored_at).or(cursor);
        contents.extend(batch);
        if exhausted {
            return Ok((contents, None));
        }
        let messages = contents
            .iter()
            .filter(|c| data_of(c).is_some_and(|(data, _)| data.reaction.is_none()))
            .count();
        if messages >= HISTORY_PAGE {
            return Ok((contents, cursor));
        }
    }
}

/// A thread's stored contents sent at `from` or later, newest first; `None` if they can't be
/// read.
///
/// Read a page at a time from the newest, which the store finds through the thread's index
/// (see [`LocalStore::index_messages`]). It can't use that for an open range
/// (`messages(thread, from..)`), which tests every message of the thread instead. The pages
/// start small and grow: most threads have only a few contents that recent.
pub(super) async fn stored_since(
    store: &SqliteStore,
    thread: &Thread,
    from: u64,
) -> Option<Vec<Content>> {
    let mut found = Vec::new();
    let mut before = None;
    let mut page = 8;
    loop {
        let Ok(batch) = store.messages_before(thread, before, page).await else {
            // A content that doesn't decode fails its whole page; the range skips just it.
            let contents = store.messages(thread, from..).await.ok()?;
            return Some(contents.flatten().collect());
        };
        let full = batch.len() == page as usize;
        before = batch.last().map(stored_at);
        found.extend(batch.into_iter().take_while(|c| stored_at(c) >= from));
        if !full || before.is_none_or(|oldest| oldest < from) {
            return Some(found);
        }
        page = (page * 4).min(512);
    }
}

/// Adds what b33p keeps beside the store to messages of one conversation: how far ours got,
/// earlier versions of edited ones, when disappearing ones go.
pub(super) async fn decorate(ctx: &Ctx, id: &ConversationId, messages: &mut [ChatMessage]) {
    // Only for the time these messages span, not the whole conversation's.
    let times = messages.iter().map(|m| m.timestamp);
    let (Some(first), Some(last)) = (times.clone().min(), times.max()) else {
        return;
    };
    let (deliveries, edits, expiries) = futures::join!(
        ctx.tables.deliveries(id, first..=last),
        ctx.tables.edits(id, first..=last),
        ctx.tables.expiries(id, first..=last),
    );
    let (mut deliveries, mut edits, mut expiries) = (
        or_logged(deliveries, "deliveries"),
        or_logged(edits, "edits"),
        or_logged(expiries, "expiries"),
    );
    for message in messages {
        if message.outgoing {
            message.delivery = deliveries.remove(&message.timestamp).unwrap_or_default();
        }
        let reference = message.reference();
        if let Some(earlier) = edits.remove(&reference) {
            message.edited_at = message.edited_at.max(earlier.last().map(|e| e.0));
            message.edits = earlier;
        }
        message.expires_at = expiries.remove(&reference);
    }
}

/// Deletes disappearing messages when their time is up, for the rest of the session.
pub(super) async fn sweep_expired(ctx: Ctx) {
    // After a failure to read or write which messages are due: they still are.
    const RETRY: Duration = Duration::from_secs(60);
    loop {
        let (due, next) = match ctx.tables.due(now_millis()).await {
            Ok(found) => found,
            Err(error) => {
                warn!(error = %scrubbed(&error), "failed to look for disappearing messages");
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        let deleted: Vec<_> = due
            .into_iter()
            .map(|(conversation, message)| {
                let thread = thread_of(&conversation);
                (conversation, message, thread)
            })
            .collect();
        // All in one transaction.
        if !deleted.is_empty()
            && let Err(error) = ctx.tables.delete_messages(&deleted).await
        {
            warn!(error = %scrubbed(&error), "failed to delete disappearing messages");
            tokio::time::sleep(RETRY).await;
            continue;
        }
        if !deleted.is_empty() {
            checkpoint(&ctx).await;
        }
        let mut gone: HashMap<ConversationId, Vec<MessageRef>> = HashMap::new();
        for (conversation, message, _) in deleted {
            gone.entry(conversation).or_default().push(message);
        }
        for (conversation, targets) in gone {
            if !ctx.events.sent(Event::Expired {
                conversation,
                targets,
            }) {
                return;
            }
        }
        let wait = next.map_or(Duration::from_secs(24 * 3600), |at| {
            Duration::from_millis(at.saturating_sub(now_millis()))
        });
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = ctx.expiring.notified() => {}
        }
    }
}

/// Starts the timers (seconds) of disappearing messages in a conversation, as of `start`:
/// incoming ones once read, ours once sent.
pub(super) async fn start_timers(
    ctx: &Ctx,
    conversation: &ConversationId,
    timers: &[(MessageRef, u32)],
    start: u64,
) {
    let expiries: Vec<_> = timers
        .iter()
        .filter(|(_, timer)| *timer > 0)
        .map(|(message, timer)| (message.clone(), start + u64::from(*timer) * 1000))
        .collect();
    if expiries.is_empty() {
        return;
    }
    if let Err(error) = ctx.tables.set_expiries(conversation, &expiries).await {
        warn!(error = %scrubbed(&error), "failed to start disappearing timers");
        return;
    }
    ctx.expiring.notify_one();
    ctx.events.send(Event::Expiring {
        conversation: conversation.clone(),
        expiries,
    });
}

/// The disappearing timers of stored messages, for messages read elsewhere.
pub(super) async fn stored_timers(
    manager: &Mgr,
    conversation: &ConversationId,
    messages: &[MessageRef],
) -> Vec<(MessageRef, u32)> {
    let Some(thread) = thread_of(conversation) else {
        return Vec::new();
    };
    let mut timers = Vec::new();
    for message in messages {
        if let Ok(Some(content)) = manager.store().message(&thread, message.timestamp).await
            && let Some(timer) = data_of(&content).and_then(|(data, _)| data.expire_timer)
        {
            timers.push((message.clone(), timer));
        }
    }
    timers
}

/// `text` made readable: mention placeholders become "@Name", and Signal's style ranges move
/// to its bytes.
async fn resolve_text(
    manager: &Mgr,
    names: &mut Names,
    text: &str,
    ranges: &[BodyRange],
) -> (String, Vec<StyleRange>, Vec<crate::Mention>) {
    use body_range::{AssociatedValue, Style as WireStyle};
    let mut wire = Vec::new();
    let mut mentioned = Vec::new();
    for range in ranges {
        let value = match &range.associated_value {
            Some(AssociatedValue::MentionAci(aci)) => ServiceId::parse_from_service_id_string(aci)
                .map(|who| RangeValue::Mention(who.service_id_string())),
            Some(AssociatedValue::MentionAciBinary(bytes)) => {
                ServiceId::parse_from_service_id_binary(bytes)
                    .map(|who| RangeValue::Mention(who.service_id_string()))
            }
            Some(AssociatedValue::Style(style)) => match WireStyle::try_from(*style) {
                Ok(WireStyle::Bold) => Some(RangeValue::Style(Style::Bold)),
                Ok(WireStyle::Italic) => Some(RangeValue::Style(Style::Italic)),
                Ok(WireStyle::Spoiler) => Some(RangeValue::Style(Style::Spoiler)),
                Ok(WireStyle::Strikethrough) => Some(RangeValue::Style(Style::Strikethrough)),
                Ok(WireStyle::Monospace) => Some(RangeValue::Style(Style::Monospace)),
                Ok(WireStyle::None) | Err(_) => None,
            },
            None => None,
        };
        let Some(value) = value else { continue };
        if let RangeValue::Mention(who) = &value
            && let Some(who) = ServiceId::parse_from_service_id_string(who)
        {
            let name = display_name(manager, names, who).await;
            mentioned.push((range.start(), name));
        }
        wire.push(WireRange {
            start: range.start(),
            length: range.length(),
            value,
        });
    }
    format::decode(text, &wire, &mentioned)
}

/// The data message a content carries: from someone, from us (synced from another device), or
/// an edit of either; `true` for an edit.
pub(super) fn data_of(content: &Content) -> Option<(&DataMessage, bool)> {
    match &content.body {
        ContentBody::DataMessage(data) => Some((data, false)),
        ContentBody::EditMessage(EditMessage {
            data_message: Some(data),
            ..
        }) => Some((data, true)),
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(sent)),
            ..
        }) => match sent {
            Sent {
                message: Some(data),
                ..
            } => Some((data, false)),
            Sent {
                edit_message:
                    Some(EditMessage {
                        data_message: Some(data),
                        ..
                    }),
                ..
            } => Some((data, true)),
            _ => None,
        },
        _ => None,
    }
}

/// Ours: sent from here (stored with us as the sender) or synced from another device (which
/// sends it as us too; [`handle_content`] has dropped sync messages from anyone else).
pub(super) fn is_outgoing(manager: &Mgr, content: &Content) -> bool {
    content.metadata.sender == own_id(manager)
}

/// Who wrote what a content carries: us for what we sent from any device.
pub(super) fn author_of(manager: &Mgr, content: &Content) -> ServiceId {
    if is_outgoing(manager, content) {
        own_id(manager)
    } else {
        content.metadata.sender
    }
}

/// Why: deleted content must not stay on disk (cf. CVE-2018-14023). `Ok(false)` if there was
/// no such message, in which case nothing is noted either.
pub(super) async fn erase_deleted(
    manager: &Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    thread: &Thread,
    target: &MessageRef,
) -> Result<bool, Error> {
    let mut store = manager.store().clone();
    if !erase_from_store(&mut store, thread, target).await? {
        return Ok(false);
    }
    ctx.tables
        .record_deleted(conversation, target)
        .await
        .map_err(db)?;
    checkpoint(ctx).await;
    Ok(true)
}

/// After erasing, so that nothing of it stays in the database's log. Failing leaves it there
/// until the next erasing or the database closing, which checkpoints too.
pub(super) async fn checkpoint(ctx: &Ctx) {
    match ctx.tables.checkpoint().await {
        Ok(true) => {}
        Ok(false) => warn!("a reader kept the database's log from emptying"),
        Err(error) => warn!(error = %scrubbed(&error), "failed to checkpoint the database"),
    }
}

/// The store keys messages by (thread, timestamp); the sender check keeps another author's
/// message at the same time. presage erases a received delete's target itself (leaving an
/// empty message with no sender), which counts as erased.
async fn erase_from_store(
    store: &mut SqliteStore,
    thread: &Thread,
    target: &MessageRef,
) -> Result<bool, Error> {
    let stored = store
        .message(thread, target.timestamp)
        .await
        .map_err(store_failed("failed to look up a deleted message"))?;
    let Some(content) = stored else {
        return Ok(false);
    };
    if is_erased(&content) {
        return Ok(true);
    }
    if content.metadata.sender.service_id_string() != target.author_id {
        return Ok(false);
    }
    store
        .delete_message(thread, target.timestamp)
        .await
        .map_err(store_failed("failed to erase a deleted message"))
}

/// What presage leaves of a message deleted for everyone: a null message from nobody.
fn is_erased(content: &Content) -> bool {
    matches!(content.body, ContentBody::NullMessage(_))
        && content.metadata.sender
            == ServiceId::Aci(presage::libsignal_service::protocol::Aci::from(
                presage::libsignal_service::prelude::Uuid::nil(),
            ))
}

/// A reaction, from someone else or (synced from the phone) from us.
pub(super) async fn to_reaction(
    manager: &Mgr,
    names: &mut Names,
    content: &Content,
) -> Option<(MessageRef, Reaction, bool)> {
    let (data, false) = data_of(content)? else {
        return None;
    };
    let outgoing = is_outgoing(manager, content);
    let reaction = data.reaction.as_ref()?;
    let target = reaction_target(reaction)?;
    let sender = display_name(manager, names, author_of(manager, content)).await;
    let reaction_view = Reaction {
        emoji: reaction.emoji.clone().unwrap_or_default(),
        sender_id: content.metadata.sender.service_id_string(),
        sender,
        outgoing,
    };
    Some((target, reaction_view, reaction.remove()))
}

pub(super) async fn to_chat_message(
    manager: &Mgr,
    names: &mut Names,
    conversation: ConversationId,
    content: &Content,
) -> Option<ChatMessage> {
    if let Some(state) = identity_note(content) {
        use presage::proto::verified::State;
        let who = content.metadata.sender;
        let name = display_name(manager, names, who).await;
        return Some(ChatMessage {
            conversation,
            sender_id: who.service_id_string(),
            body: match state {
                State::Default => format!("Your safety number with {name} has changed."),
                State::Verified => {
                    format!("You marked your safety number with {name} as verified.")
                }
                State::Unverified => {
                    format!("You marked your safety number with {name} as unverified.")
                }
            },
            sender: name,
            timestamp: content.timestamp(),
            notice: true,
            ..Default::default()
        });
    }
    let (data, edit) = data_of(content)?;
    let outgoing = is_outgoing(manager, content);
    let author = author_of(manager, content);
    let sender = display_name(manager, names, author).await;
    let (mut body, styles, mentions) = resolve_text(
        manager,
        names,
        data.body.as_deref().unwrap_or_default(),
        &data.body_ranges,
    )
    .await;
    let attachments = attachments::of_message(data);
    // A group's timer changes with the group (below), signed; the flag is anyone's to set.
    let mut timer_change = (data.flags() & Flags::ExpirationTimerUpdate as u32 != 0
        && data.group_v2.is_none())
    .then(|| data.expire_timer.unwrap_or(0));
    // Whether it says what changed in the conversation rather than what someone wrote.
    let mut notice = false;
    if let Some(seconds) = timer_change
        && body.is_empty()
    {
        body = crate::timer_notice(&sender, seconds);
        notice = true;
    }
    // A change to the group: what it says. An empty message with a flag (a profile key
    // update, say, which accepting a group sends) is no change, and not the group's creation.
    if body.is_empty()
        && attachments::of_message(data).is_empty()
        && data.flags.unwrap_or(0) == 0
        && let Some(context) = &data.group_v2
        && let Some((notice_text, timer)) =
            group_notice(manager, names, context, &sender, outgoing).await
    {
        body = notice_text;
        timer_change = timer_change.or(timer);
        notice = true;
    }
    if body.is_empty() && attachments.is_empty() {
        return None;
    }
    let quote = match &data.quote {
        Some(quote) => {
            let author_id = quote.parse_author_aci().map(ServiceId::Aci);
            let author = match author_id {
                Some(author) => display_name(manager, names, author).await,
                None => String::new(),
            };
            // By the time of the version it quotes, which for an edited message is an edit's.
            let stored = match (thread_of(&conversation), quote.id) {
                (Some(thread), Some(id)) => manager
                    .store()
                    .message_sent_or_edited_at(&thread, id)
                    .await
                    .ok()
                    .flatten(),
                _ => None,
            };
            // The quote's copy of the text is written by the quoting sender, and is the version
            // it quotes where the stored message has been edited since.
            let (quoted, ranges) = match stored
                .as_ref()
                .filter(|stored| Some(stored.metadata.sender) == author_id)
                .filter(|stored| quote.id.is_some_and(|id| is_quoted_version(stored, id)))
                .and_then(data_of)
            {
                Some((data, _)) => (
                    data.body.as_deref().unwrap_or_default(),
                    &data.body_ranges[..],
                ),
                None => (
                    quote.text.as_deref().unwrap_or_default(),
                    &quote.body_ranges[..],
                ),
            };
            let (text, styles, _) = resolve_text(manager, names, quoted, ranges).await;
            Some(Quote {
                author,
                text: format::hide_spoilers(&text, &styles),
                author_id: author_id
                    .map(|id| id.service_id_string())
                    .unwrap_or_default(),
                // The message's own time, which it is found by here.
                timestamp: stored
                    .as_ref()
                    .map(|stored| stored.timestamp())
                    .or(quote.id)
                    .unwrap_or_default(),
            })
        }
        None => None,
    };
    Some(ChatMessage {
        conversation,
        sender_id: author.service_id_string(),
        sender,
        body,
        // For an edit, the time of the message it replaces, which identifies it.
        timestamp: content.timestamp(),
        outgoing,
        quote,
        attachments,
        styles,
        mentions,
        expire_timer: data.expire_timer.filter(|&t| t > 0),
        timer_change,
        notice,
        preview: data.preview.first().and_then(|preview| {
            // Only for a link the text has (as Signal's clients require): otherwise a card
            // with a trusted site's title and picture could lead somewhere else entirely.
            let url = preview
                .url
                .clone()
                .filter(|url| url.starts_with("https://"))?;
            let text = data.body.as_deref().unwrap_or_default();
            if !format::links(text).contains(&url) {
                debug!("dropped a link preview for a link not in the message");
                return None;
            }
            Some(crate::LinkPreview {
                url,
                title: crate::format::name(preview.title.as_deref().unwrap_or_default()),
                description: preview.description.clone().unwrap_or_default(),
                image: preview.image.as_ref().map(attachments::describe_preview),
            })
        }),
        edited_at: edit.then(|| data.timestamp.unwrap_or_default()),
        ..Default::default()
    })
}

/// Messages of a thread with attachments (other than a long message's text) or links;
/// newest first, at most [`MEDIA_MESSAGES`].
pub(super) async fn media(
    manager: &Mgr,
    thread: &Thread,
    conversation: &ConversationId,
) -> Result<Vec<ChatMessage>, Error> {
    const MEDIA_MESSAGES: usize = 2000;
    const BATCH: u32 = 500;
    let mut names = Names::new();
    let mut found = Vec::new();
    let mut before = None;
    loop {
        let batch = manager
            .store()
            .messages_before(thread, before, BATCH)
            .await
            .map_err(store_failed("failed to gather media"))?;
        for content in &batch {
            let wanted = data_of(content).is_some_and(|(data, _)| {
                data.attachments
                    .iter()
                    .any(|a| a.content_type.as_deref() != Some(attachments::LONG_TEXT))
                    || data
                        .body
                        .as_deref()
                        .is_some_and(|body| body.contains("http://") || body.contains("https://"))
            });
            if wanted
                && let Some(message) =
                    to_chat_message(manager, &mut names, conversation.clone(), content).await
            {
                found.push(message);
                if found.len() == MEDIA_MESSAGES {
                    return Ok(found);
                }
            }
        }
        if batch.len() < BATCH as usize {
            return Ok(found);
        }
        before = batch.last().map(stored_at);
    }
}

/// Messages of a thread whose text contains `query`, ignoring case; newest first, at most
/// [`SEARCH_HITS`].
pub(super) async fn search(
    manager: &Mgr,
    thread: &Thread,
    conversation: &ConversationId,
    query: &str,
) -> Result<Vec<ChatMessage>, Error> {
    const SEARCH_HITS: usize = 100;
    const BATCH: u32 = 500;
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut names = Names::new();
    let mut hits = Vec::new();
    let mut before = None;
    loop {
        let batch = manager
            .store()
            .messages_before(thread, before, BATCH)
            .await
            .map_err(store_failed("failed to search"))?;
        // Cheap test first: most messages don't match. Off this thread, though: lowercasing
        // every text of a long history adds up.
        let (batch, matching) = {
            let query = query.clone();
            tokio::task::spawn_blocking(move || {
                let matching: Vec<usize> = (0..batch.len())
                    .filter(|&at| {
                        data_of(&batch[at])
                            .and_then(|(data, _)| data.body.as_deref())
                            .is_some_and(|body| body.to_lowercase().contains(&query))
                    })
                    .collect();
                (batch, matching)
            })
            .await
            .map_err(|e| Error::refused(e.to_string()))?
        };
        for content in matching.iter().map(|&at| &batch[at]) {
            if let Some(message) =
                to_chat_message(manager, &mut names, conversation.clone(), content).await
            {
                hits.push(message);
                if hits.len() == SEARCH_HITS {
                    return Ok(hits);
                }
            }
        }
        if batch.len() < BATCH as usize {
            return Ok(hits);
        }
        before = batch.last().map(stored_at);
    }
}

/// Whether `stored` is still the version of its message that a quote naming `quoted` shows: the
/// store keeps a message as last edited, and a quote names the version it showed, the first
/// sent or an edit (`send::send_data`'s `quoted_version`).
fn is_quoted_version(stored: &Content, quoted: u64) -> bool {
    stored.edited_at().unwrap_or_else(|| stored.timestamp()) == quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::send::aci_binary;
    use presage::model::identity::OnNewIdentity;

    /// A quote of a message edited since shows what it quoted, not the edit; a quote of the
    /// latest edit, that.
    #[test]
    fn a_quote_is_of_the_version_it_names() {
        use presage::libsignal_service::{content::Metadata, push_service::DEFAULT_DEVICE_ID};
        let alice = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000001")
            .unwrap();
        let content = |body: ContentBody| Content {
            metadata: Metadata {
                sender: alice,
                destination: alice,
                sender_device: *DEFAULT_DEVICE_ID,
                pni_verified: None,
                client_timestamp: chrono::DateTime::from_timestamp_millis(10).unwrap(),
                server_timestamp: chrono::DateTime::from_timestamp_millis(11).unwrap(),
                needs_receipt: false,
                unidentified_sender: false,
                was_plaintext: false,
                server_guid: None,
            },
            body,
        };
        let sent = content(ContentBody::DataMessage(DataMessage {
            body: Some("first".into()),
            timestamp: Some(10),
            ..Default::default()
        }));
        assert!(is_quoted_version(&sent, 10));
        let edited = content(ContentBody::EditMessage(presage::proto::EditMessage {
            target_sent_timestamp: Some(10),
            data_message: Some(DataMessage {
                body: Some("edited".into()),
                timestamp: Some(20),
                ..Default::default()
            }),
        }));
        assert!(!is_quoted_version(&edited, 10), "quoted before the edit");
        assert!(is_quoted_version(&edited, 20), "quoted after it");
    }

    #[test]
    fn deleting_for_everyone_erases_only_the_authors_message() {
        use presage::libsignal_service::{content::Metadata, push_service::DEFAULT_DEVICE_ID};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut store = SqliteStore::open(":memory:", OnNewIdentity::Trust)
                .await
                .unwrap();
            let who = |n: u8| {
                ServiceId::parse_from_service_id_string(&format!(
                    "00000000-0000-0000-0000-0000000000{n:02}"
                ))
                .unwrap()
            };
            let (alice, bob) = (who(1), who(2));
            let thread = Thread::Contact(alice);
            let at = |millis: i64| chrono::DateTime::from_timestamp_millis(millis).unwrap();
            store
                .save_message(
                    &thread,
                    Content {
                        metadata: Metadata {
                            sender: alice,
                            destination: bob,
                            sender_device: *DEFAULT_DEVICE_ID,
                            pni_verified: None,
                            client_timestamp: at(10),
                            server_timestamp: at(11),
                            needs_receipt: false,
                            unidentified_sender: false,
                            was_plaintext: false,
                            server_guid: None,
                        },
                        body: ContentBody::DataMessage(DataMessage {
                            body: Some("secret".into()),
                            timestamp: Some(10),
                            ..Default::default()
                        }),
                    },
                )
                .await
                .unwrap();
            let named = |author: ServiceId| MessageRef {
                author_id: author.service_id_string(),
                timestamp: 10,
            };
            assert!(
                !erase_from_store(&mut store, &thread, &named(bob))
                    .await
                    .unwrap()
            );
            assert!(store.message(&thread, 10).await.unwrap().is_some());
            assert!(
                erase_from_store(&mut store, &thread, &named(alice))
                    .await
                    .unwrap()
            );
            assert!(store.message(&thread, 10).await.unwrap().is_none());
            assert!(
                !erase_from_store(&mut store, &thread, &named(alice))
                    .await
                    .unwrap()
            );
        });
    }

    /// presage erases a received delete's target itself, leaving an empty message from
    /// nobody; that counts as erased, so that the deletion is noted, while a delete of a
    /// message that was never there is not.
    #[test]
    fn a_message_presage_erased_counts_as_erased() {
        use presage::libsignal_service::{content::Metadata, push_service::DEFAULT_DEVICE_ID};
        use presage::proto::NullMessage;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut store = SqliteStore::open(":memory:", OnNewIdentity::Trust)
                .await
                .unwrap();
            let alice =
                ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000001")
                    .unwrap();
            let nobody = ServiceId::Aci(presage::libsignal_service::protocol::Aci::from(
                presage::libsignal_service::prelude::Uuid::nil(),
            ));
            let thread = Thread::Contact(alice);
            let at = |millis: i64| chrono::DateTime::from_timestamp_millis(millis).unwrap();
            let content = |sender, body| Content {
                metadata: Metadata {
                    sender,
                    destination: alice,
                    sender_device: *DEFAULT_DEVICE_ID,
                    pni_verified: None,
                    client_timestamp: at(10),
                    server_timestamp: at(11),
                    needs_receipt: false,
                    unidentified_sender: false,
                    was_plaintext: false,
                    server_guid: None,
                },
                body,
            };
            let erased = content(nobody, ContentBody::NullMessage(NullMessage::default()));
            assert!(is_erased(&erased));
            assert!(!is_erased(&content(
                alice,
                ContentBody::NullMessage(NullMessage::default())
            )));
            store.save_message(&thread, erased).await.unwrap();
            let named = |timestamp| MessageRef {
                author_id: alice.service_id_string(),
                timestamp,
            };
            assert!(
                erase_from_store(&mut store, &thread, &named(10))
                    .await
                    .unwrap()
            );
            assert!(
                !erase_from_store(&mut store, &thread, &named(30))
                    .await
                    .unwrap()
            );
        });
    }

    /// A number typed internationally, or the way our own country writes them, is the same
    /// number; text that isn't one, or an invalid number, is none.
    /// Current Signal apps fill only the binary ACI fields, and read only those when given
    /// both; b33p reads either and fills both.
    #[test]
    fn acis_are_read_and_written_in_binary_too() {
        use presage::proto::data_message::Reaction;
        let id = "00000000-0000-0000-0000-000000000007";
        let aci = ServiceId::parse_from_service_id_string(id).unwrap();
        let binary = aci_binary(id).unwrap();
        assert_eq!(binary, aci.service_id_binary());
        assert_eq!(binary.len(), 16);
        assert_eq!(aci_binary(&format!("PNI:{id}")), None);
        assert_eq!(aci_binary("not an id"), None);

        let target = MessageRef {
            author_id: id.into(),
            timestamp: 5,
        };
        let from_phone = Reaction {
            target_author_aci_binary: Some(binary.clone()),
            target_sent_timestamp: Some(5),
            ..Default::default()
        };
        assert_eq!(reaction_target(&from_phone), Some(target.clone()));
        let from_older_client = Reaction {
            target_author_aci: Some(id.into()),
            target_sent_timestamp: Some(5),
            ..Default::default()
        };
        assert_eq!(reaction_target(&from_older_client), Some(target));
        let nameless = Reaction {
            target_sent_timestamp: Some(5),
            ..Default::default()
        };
        assert_eq!(reaction_target(&nameless), None);
    }
}
