//! Who has received and read our messages, person by person (`b33p_receipt`): a message's
//! info, and in a group each member's newest message read.

use presage::{
    libsignal_service::protocol::ServiceId,
    store::{ContentsStore as _, Thread},
};

use crate::{ConversationId, Event, MessageInfo, MessageRef, Person, Receipt};

use super::{
    Ctx, Mgr,
    conversations::{Names, display_name, or_logged, own_id, thread_of},
};

/// Who our message `target` in `conversation` went to, and what each said of it: the contact,
/// or the group's members now (who a message went to isn't kept; members who joined since show
/// as not having received it).
pub(super) async fn message_info(
    manager: &Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    target: &MessageRef,
) -> MessageInfo {
    let receipts = or_logged(
        ctx.tables.receipts(conversation, target.timestamp).await,
        "receipts",
    );
    let own = own_id(manager);
    let recipients: Vec<ServiceId> = match thread_of(conversation) {
        Some(Thread::Contact(contact)) => vec![contact],
        Some(Thread::Group(key)) => match manager.store().group(key).await {
            Ok(Some(group)) => group
                .members
                .iter()
                .map(|member| ServiceId::Aci(member.aci))
                .filter(|member| *member != own)
                .collect(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    let mut names = Names::new();
    let mut info = MessageInfo::default();
    for recipient in recipients {
        let id = recipient.service_id_string();
        let (delivered_at, read_at) = receipts
            .iter()
            .find(|(who, ..)| *who == id)
            .map_or((None, None), |(_, delivered, read)| (*delivered, *read));
        info.recipients.push(Receipt {
            person: Person {
                name: display_name(manager, &mut names, recipient).await,
                service_id: id,
            },
            delivered_at,
            read_at,
        });
    }
    info
}

/// Tells the window the newest of our messages each member of group `conversation` has read.
/// Nothing for a chat of two, where the ticks say it.
pub(super) async fn tell_readers(ctx: &Ctx, conversation: &ConversationId) {
    if !matches!(thread_of(conversation), Some(Thread::Group(_))) {
        return;
    }
    let newest = or_logged(ctx.tables.newest_read(conversation).await, "readers");
    ctx.events.send(Event::Readers {
        conversation: conversation.clone(),
        newest,
    });
}
