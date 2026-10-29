//! Stories: keeping those that arrive, listing the last day's, and saying we viewed one.

use std::time::Duration;

use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::{
    libsignal_service::{
        content::ContentBody,
        protocol::{Aci, ServiceId},
    },
    proto::{ReceiptMessage, SyncMessage, receipt_message, sync_message},
    store::{ContentsStore as _, Thread},
};
use tracing::{debug, warn};

use crate::outbox::{Outbox, Sends as _};
use crate::{ConversationId, Event, MessageRef, attachments, now_millis, scrub::scrubbed};

use super::{
    Ctx, Mgr,
    conversations::{
        Names, conversation_id, display_name, is_blocked, is_unanswered_request, own_id, tag,
    },
    groups::is_member,
    send::{aci_binary, send_sync},
};

/// Stories last a day.
const STORY_LIFETIME: u64 = 24 * 60 * 60 * 1000;

/// How soon stories that couldn't be read are read again.
const RETRY: Duration = Duration::from_secs(5 * 60);

/// Whether a story of `timestamp` is one to show at `now`: not expired, and not dated so far
/// ahead (beyond clocks differing) that it would never expire.
fn story_is_current(now: u64, timestamp: u64) -> bool {
    const CLOCK_SKEW: u64 = 60 * 60 * 1000;
    now.saturating_sub(timestamp) <= STORY_LIFETIME && timestamp.saturating_sub(now) <= CLOCK_SKEW
}

/// The colours of text stories posted here: white on Signal's blue.
pub(super) const STORY_FOREGROUND: u32 = 0xffff_ffff;

pub(super) const STORY_BACKGROUND: u32 = 0xff3a_76f0;

/// Whom a group story went to, as our other devices hear of it: [`recipients`] of the group.
pub(super) async fn story_recipients(
    manager: &Mgr,
    key: [u8; 32],
) -> Vec<sync_message::sent::StoryMessageRecipient> {
    let own = manager.registration_data().service_ids.aci();
    match manager.store().group(key).await {
        Ok(Some(group)) => recipients(group.members.iter().map(|member| member.aci), own),
        _ => Vec::new(),
    }
}

/// Every member but us, each allowed to reply, and no distribution list: a group story's
/// recipients as Signal's apps say them (Desktop's sendStory.preload.ts).
fn recipients(
    members: impl Iterator<Item = Aci>,
    own: Aci,
) -> Vec<sync_message::sent::StoryMessageRecipient> {
    members
        .filter(|member| *member != own)
        .map(|member| {
            let member = ServiceId::from(member);
            sync_message::sent::StoryMessageRecipient {
                destination_service_id: Some(member.service_id_string()),
                destination_service_id_binary: Some(member.service_id_binary()),
                distribution_list_ids: Vec::new(),
                is_allowed_to_reply: Some(true),
            }
        })
        .collect()
}

/// Keeps a story from someone (or from us, on another device) and tells the UI.
pub(super) async fn on_story(
    manager: &Mgr,
    ctx: &Ctx,
    author: ServiceId,
    timestamp: u64,
    story: &presage::proto::StoryMessage,
    outgoing: bool,
) {
    use presage::proto::{story_message::Attachment, text_attachment::Background};
    if !story_is_current(now_millis(), timestamp) {
        return;
    }
    // A group story, or one to the author's contacts.
    let group = story
        .group
        .as_ref()
        .and_then(|g| g.master_key.clone())
        .and_then(|key| <[u8; 32]>::try_from(key).ok());
    let conversation = match group {
        Some(key) => conversation_id(&Thread::Group(key)),
        None => ConversationId::contact(&author.service_id_string()),
    };
    // Only from people we know, as Signal's clients show them: a stranger's isn't shown, nor
    // one for a group we aren't in, or from someone who isn't (any more). Ours from any
    // device pass.
    if !outgoing {
        let known = ctx.threads.borrow().contains_key(&conversation);
        let allowed = match group {
            Some(key) => known && is_member(manager, key, author).await,
            None => known && !is_unanswered_request(ctx, &conversation).await,
        };
        if !allowed {
            debug!(conversation = %tag(&conversation), "dropped a story from a stranger");
            return;
        }
    }
    let mut stored = crate::local::StoredStory {
        author: author.service_id_string(),
        timestamp,
        conversation,
        viewed: outgoing,
        outgoing,
        ..Default::default()
    };
    match &story.attachment {
        Some(Attachment::FileAttachment(pointer)) => {
            stored.pointer = Some(pointer.encode_to_vec());
        }
        Some(Attachment::TextAttachment(text)) => {
            stored.text = text.text.clone().unwrap_or_default();
            stored.foreground = text.text_foreground_color.unwrap_or(STORY_FOREGROUND);
            stored.background = match &text.background {
                Some(Background::Color(color)) => *color,
                // A gradient's first colour.
                Some(Background::Gradient(gradient)) => gradient
                    .colors
                    .first()
                    .copied()
                    .or(gradient.start_color)
                    .unwrap_or(STORY_BACKGROUND),
                None => STORY_BACKGROUND,
            };
        }
        None => return,
    }
    if let Err(error) = keep_story(ctx, &stored).await {
        warn!(error = %scrubbed(&error), "failed to keep a story");
        return;
    }
    send_stories(manager, ctx).await;
}

/// Keeps `story`, and has [`expire_stories`] take it off the list once its day is over.
pub(super) async fn keep_story(
    ctx: &Ctx,
    story: &crate::local::StoredStory,
) -> Result<(), sqlx::Error> {
    ctx.tables.add_story(story).await?;
    ctx.story_kept.notify_one();
    Ok(())
}

/// Tells the UI the stories of the last day. Returns when that list changes by itself next:
/// when its first story is over, or, if the stories couldn't be read, when to try again.
pub(super) async fn send_stories(manager: &Mgr, ctx: &Ctx) -> Option<u64> {
    let since = now_millis().saturating_sub(STORY_LIFETIME);
    let stored = match ctx.tables.stories(since).await {
        Ok(stored) => stored,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to read the stories");
            return Some(now_millis() + RETRY.as_millis() as u64);
        }
    };
    // Oldest first; it is gone once more than a day old.
    let next = stored
        .first()
        .map(|story| story.timestamp + STORY_LIFETIME + 1);
    let mut names = Names::new();
    let mut stories = Vec::new();
    for story in stored {
        // Not from those we block, also in groups. (Strangers' are left out when listing.)
        let by_author = ConversationId::contact(&story.author);
        if !story.outgoing
            && (is_blocked(ctx, &by_author).await || is_blocked(ctx, &story.conversation).await)
        {
            continue;
        }
        let author = match ServiceId::parse_from_service_id_string(&story.author) {
            Some(who) => display_name(manager, &mut names, who).await,
            None => String::new(),
        };
        let content = match &story.pointer {
            Some(pointer) => match presage::proto::AttachmentPointer::decode(&pointer[..]) {
                Ok(pointer) => crate::StoryContent::Picture(attachments::describe(&pointer, false)),
                Err(_) => continue,
            },
            None => crate::StoryContent::Text {
                text: story.text,
                foreground: story.foreground,
                background: story.background,
            },
        };
        stories.push(crate::Story {
            author_id: story.author,
            author,
            conversation: story.conversation,
            timestamp: story.timestamp,
            content,
            viewed: story.viewed,
            outgoing: story.outgoing,
        });
    }
    ctx.events.send(Event::Stories(stories));
    next
}

/// Tells a story's author we viewed it (with `receipts`), and our other devices.
pub(super) async fn send_viewed(manager: Mgr, outbox: Outbox, story: MessageRef, receipts: bool) {
    let timestamp = now_millis();
    let Some(author) = ServiceId::parse_from_service_id_string(&story.author_id) else {
        return;
    };
    if author == own_id(&manager) {
        return;
    }
    if receipts {
        let receipt = ReceiptMessage {
            r#type: Some(receipt_message::Type::Viewed as i32),
            timestamp: vec![story.timestamp],
        };
        outbox
            .post_message(
                author,
                ContentBody::ReceiptMessage(receipt),
                timestamp,
                "a viewed receipt",
            )
            .await;
    }
    let sync = SyncMessage {
        viewed: vec![sync_message::Viewed {
            sender_aci_binary: aci_binary(&story.author_id),
            sender_aci: Some(story.author_id),
            timestamp: Some(story.timestamp),
        }],
        ..Default::default()
    };
    send_sync(&manager, &outbox, sync).await;
}

/// Takes stories off the list once their day is over, for the rest of the session. `next` is
/// when that is for the first of them, as [`send_stories`] said.
pub(super) async fn expire_stories(manager: Mgr, ctx: Ctx, mut next: Option<u64>) {
    loop {
        let over = async {
            match next {
                Some(at) => {
                    let wait = Duration::from_millis(at.saturating_sub(now_millis()));
                    tokio::time::sleep(wait).await;
                }
                // Until a story is kept.
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = over => {}
            // It may be over before the first one known, or be the first one.
            () = ctx.story_kept.notified() => {}
        }
        next = send_stories(&manager, &ctx).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_story_goes_to_every_other_member_who_may_reply() {
        let own = Aci::from_uuid_bytes([1; 16]);
        let other = Aci::from_uuid_bytes([2; 16]);
        let recipients = recipients([own, other].into_iter(), own);
        assert_eq!(recipients.len(), 1);
        let recipient = &recipients[0];
        assert_eq!(
            recipient.parse_destination_service_id(),
            Some(ServiceId::from(other))
        );
        assert_eq!(
            recipient.destination_service_id.as_deref(),
            Some(ServiceId::from(other).service_id_string().as_str())
        );
        assert!(recipient.distribution_list_ids.is_empty());
        assert_eq!(recipient.is_allowed_to_reply, Some(true));
    }

    /// A story lasts a day from its timestamp; one dated far ahead (which would otherwise
    /// stay for as long) is not shown, while a clock a little ahead is fine.
    #[test]
    fn stories_from_the_future_are_not_current() {
        const HOUR: u64 = 60 * 60 * 1000;
        let now = 100 * 24 * HOUR;
        assert!(story_is_current(now, now));
        assert!(story_is_current(now, now - 23 * HOUR));
        assert!(!story_is_current(now, now - 25 * HOUR));
        assert!(story_is_current(now, now + HOUR / 2));
        assert!(!story_is_current(now, now + 2 * HOUR));
    }
}
