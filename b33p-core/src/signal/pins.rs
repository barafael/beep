//! Messages pinned in a chat, as Signal's apps pin them (Signal-Desktop's `PinnedMessages`): up
//! to [`PIN_LIMIT`] per chat, for a while or until unpinned; in a chat with one person by either,
//! in a group by whoever may change its details. presage stores the pin and unpin messages but
//! keeps no state: b33p's table does (`local.rs`).

use presage::{
    libsignal_service::{content::Content, protocol::ServiceId},
    proto::{DataMessage, data_message::pin_message::PinDuration},
    store::{ContentExt as _, ContentsStore as _, Thread},
};
use tracing::{debug, warn};

use crate::{ChatMessage, ConversationId, Event, MessageRef, PinnedMessage, now_millis};

use super::{
    Ctx, Mgr,
    conversations::{Names, is_blocked, or_logged, own_id, thread_of},
    history::{author_of, original, to_chat_message},
    send::{SendError, aci_binary, send_data},
};

// The pins a chat keeps (the server's `global.pinnedMessageLimit` by default).
use crate::state::PIN_LIMIT;

/// What a pin or unpin in a message asks.
#[derive(Debug, PartialEq)]
pub(super) enum PinChange {
    /// Pins the message by `author` sent (or last edited) at `timestamp`, for `seconds` or,
    /// with `None`, until unpinned.
    Pin {
        author: ServiceId,
        timestamp: u64,
        seconds: Option<u32>,
    },
    Unpin {
        author: ServiceId,
        timestamp: u64,
    },
}

/// The pin or unpin `data` carries, if it carries one that names its message.
pub(super) fn pin_change(data: &DataMessage) -> Option<PinChange> {
    // An ACI's 16 bytes, as Signal's apps name a message's author in a modifier.
    let author_of =
        |binary: Option<&[u8]>| binary.and_then(ServiceId::parse_from_service_id_binary);
    if let Some(pin) = &data.pin_message {
        let seconds = match pin.pin_duration {
            Some(PinDuration::PinDurationSeconds(seconds)) if seconds > 0 => Some(seconds),
            _ => None,
        };
        return Some(PinChange::Pin {
            author: author_of(pin.target_author_aci_binary.as_deref())?,
            timestamp: pin.target_sent_timestamp?,
            seconds,
        });
    }
    let unpin = data.unpin_message.as_ref()?;
    Some(PinChange::Unpin {
        author: author_of(unpin.target_author_aci_binary.as_deref())?,
        timestamp: unpin.target_sent_timestamp?,
    })
}

/// What became of a pin or unpin received.
#[derive(Debug, PartialEq)]
pub(super) enum PinOutcome {
    /// A pin that took: its message stays in the store, where it reads "… pinned a message".
    Pinned,
    /// Done with: an unpin, or a pin that changed nothing (an older one of a message pinned
    /// since, one past the limit). Nothing to show.
    Done,
    /// Not taken, as Signal-Desktop takes none: a chat that is blocked, a message that isn't
    /// here (deleted, or from before we joined), someone a group doesn't let change it. It
    /// leaves the store again, so no notice of it shows.
    Refused,
}

/// Takes `change`, which `content` in conversation `id` carries, and tells the window the
/// chat's pins if they changed.
pub(super) async fn on_pin_change(
    manager: &Mgr,
    ctx: &Ctx,
    id: &ConversationId,
    content: &Content,
    change: PinChange,
) -> PinOutcome {
    let by = author_of(manager, content);
    let (author, timestamp) = match &change {
        PinChange::Pin {
            author, timestamp, ..
        }
        | PinChange::Unpin { author, timestamp } => (*author, *timestamp),
    };
    let Some(thread) = thread_of(id) else {
        return PinOutcome::Refused;
    };
    if is_blocked(ctx, id).await {
        debug!("ignored a pin in a blocked chat");
        return PinOutcome::Refused;
    }
    if !may_change_pins(manager, &thread, by).await {
        debug!("ignored a pin by someone the group doesn't let change it");
        return PinOutcome::Refused;
    }
    // Named by its latest edit's time, as Signal's apps name edited messages.
    let named = MessageRef {
        author_id: author.service_id_string(),
        timestamp,
    };
    let target = original(ctx, id, named).await;
    if !is_stored(manager, &thread, &target).await {
        debug!("ignored a pin of a message that isn't here");
        return PinOutcome::Refused;
    }
    let changed = match change {
        PinChange::Pin { seconds, .. } => {
            // From when it came, as Signal-Desktop counts.
            let pinned_at = now_millis();
            let expires_at = seconds.map(|s| pinned_at + u64::from(s) * 1000);
            let pinned = ctx
                .tables
                .pin_message(id, &target, pinned_at, expires_at, PIN_LIMIT)
                .await;
            match pinned {
                Ok(pinned) => pinned,
                Err(error) => {
                    warn!(error = %crate::scrub::chain(&error), "failed to pin a message");
                    false
                }
            }
        }
        PinChange::Unpin { .. } => or_logged(ctx.tables.unpin_message(id, &target).await, "unpin"),
    };
    if !changed {
        return PinOutcome::Done;
    }
    tell_pins(manager, ctx, id).await;
    match change_is_pin(content) {
        true => PinOutcome::Pinned,
        false => PinOutcome::Done,
    }
}

fn change_is_pin(content: &Content) -> bool {
    super::history::data_of(content).is_some_and(|(data, _)| data.pin_message.is_some())
}

/// Whether `who` may pin and unpin in `thread`: anyone in a chat with one person; in a group,
/// an admin, or any member if the group lets members change its details.
async fn may_change_pins(manager: &Mgr, thread: &Thread, who: ServiceId) -> bool {
    use presage::libsignal_service::groups_v2::{AccessRequired, Role};
    let Thread::Group(key) = thread else {
        return true;
    };
    let Ok(Some(group)) = manager.store().group(*key).await else {
        return false;
    };
    let Some(member) = group.members.iter().find(|m| ServiceId::Aci(m.aci) == who) else {
        return false;
    };
    member.role == Role::Administrator
        || group
            .access_control
            .is_some_and(|access| access.attributes == AccessRequired::Member)
}

/// Whether the store has `message`, by its author.
async fn is_stored(manager: &Mgr, thread: &Thread, message: &MessageRef) -> bool {
    manager
        .store()
        .message_sent_or_edited_at(thread, message.timestamp)
        .await
        .ok()
        .flatten()
        .is_some_and(|stored| {
            stored.metadata.sender.service_id_string() == message.author_id
                && stored.timestamp() == message.timestamp
        })
}

/// Pins `target` in `conversation` for everyone in it (`Some(seconds)`, `seconds` as in
/// [`Command::PinMessage`](crate::Command::PinMessage)), or unpins it (`None`), as
/// Signal-Desktop's `sendPinMessage` and `sendUnpinMessage` do, then keeps and shows it here.
pub(super) async fn send_pin_change(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    target: &MessageRef,
    pin: Option<Option<u32>>,
) -> Result<(), SendError> {
    use presage::proto::data_message::{PinMessage, UnpinMessage};
    let thread = thread_of(conversation).ok_or_else(|| SendError::UnknownConversation {
        conversation: conversation.clone(),
    })?;
    // Named by its latest edit's time, as Signal's apps name an edited message.
    let named_at = or_logged(
        ctx.tables.latest_edit(conversation, target).await,
        "edit times",
    )
    .unwrap_or(target.timestamp);
    let author = aci_binary(&target.author_id);
    let now = now_millis();
    let message = match pin {
        Some(seconds) => DataMessage {
            pin_message: Some(PinMessage {
                target_author_aci_binary: author,
                target_sent_timestamp: Some(named_at),
                pin_duration: Some(match seconds {
                    Some(seconds) => PinDuration::PinDurationSeconds(seconds),
                    None => PinDuration::PinDurationForever(true),
                }),
            }),
            ..Default::default()
        },
        None => DataMessage {
            unpin_message: Some(UnpinMessage {
                target_author_aci_binary: author,
                target_sent_timestamp: Some(named_at),
            }),
            ..Default::default()
        },
    };
    send_data(manager, ctx, &thread, message, now).await?;
    let kept = match pin {
        Some(seconds) => {
            let expires_at = seconds.map(|s| now + u64::from(s) * 1000);
            ctx.tables
                .pin_message(conversation, target, now, expires_at, PIN_LIMIT)
                .await
        }
        None => ctx.tables.unpin_message(conversation, target).await,
    };
    if let Err(error) = kept {
        warn!(error = %crate::scrub::chain(&error), "failed to keep a pin sent");
    }
    tell_pins(manager, ctx, conversation).await;
    // The pin's notice, as the stored pin reads in the history from now on.
    if pin.is_some() {
        ctx.events.send(Event::Message(ChatMessage {
            conversation: conversation.clone(),
            sender_id: own_id(manager).service_id_string(),
            sender: "You".into(),
            body: "You pinned a message.".into(),
            timestamp: now,
            outgoing: true,
            notice: true,
            ..Default::default()
        }));
    }
    Ok(())
}

/// Tells the window what is pinned in `id` now: each message as its latest version reads.
pub(super) async fn tell_pins(manager: &Mgr, ctx: &Ctx, id: &ConversationId) {
    let Some(thread) = thread_of(id) else {
        return;
    };
    let pinned = or_logged(ctx.tables.pinned_messages(id, now_millis()).await, "pins");
    let mut names = Names::new();
    let mut pins = Vec::with_capacity(pinned.len());
    for (target, expires_at) in pinned {
        // The version shown: its latest edit's, stored under the edit's time.
        let at = or_logged(ctx.tables.latest_edit(id, &target).await, "edit times")
            .unwrap_or(target.timestamp);
        let stored = manager
            .store()
            .message_sent_or_edited_at(&thread, at)
            .await
            .ok()
            .flatten();
        let Some(stored) = stored else {
            continue;
        };
        let Some(mut message) = to_chat_message(manager, &mut names, id.clone(), &stored).await
        else {
            continue;
        };
        message.timestamp = target.timestamp;
        pins.push(PinnedMessage {
            message,
            expires_at,
        });
    }
    ctx.events.send(Event::PinnedMessages {
        conversation: id.clone(),
        pins,
    });
}

#[cfg(test)]
mod tests {
    use presage::proto::data_message::{PinMessage, UnpinMessage};

    use super::*;

    const ANN: [u8; 16] = [7; 16];

    fn ann() -> ServiceId {
        ServiceId::parse_from_service_id_binary(&ANN).unwrap()
    }

    #[test]
    fn pins_and_unpins_are_read_as_signal_sends_them() {
        let pin = |duration| DataMessage {
            pin_message: Some(PinMessage {
                target_author_aci_binary: Some(ANN.to_vec()),
                target_sent_timestamp: Some(42),
                pin_duration: duration,
            }),
            ..Default::default()
        };
        assert_eq!(
            pin_change(&pin(Some(PinDuration::PinDurationSeconds(86_400)))),
            Some(PinChange::Pin {
                author: ann(),
                timestamp: 42,
                seconds: Some(86_400)
            })
        );
        for until_unpinned in [Some(PinDuration::PinDurationForever(true)), None] {
            assert_eq!(
                pin_change(&pin(until_unpinned)),
                Some(PinChange::Pin {
                    author: ann(),
                    timestamp: 42,
                    seconds: None
                })
            );
        }
        let unpin = DataMessage {
            unpin_message: Some(UnpinMessage {
                target_author_aci_binary: Some(ANN.to_vec()),
                target_sent_timestamp: Some(42),
            }),
            ..Default::default()
        };
        assert_eq!(
            pin_change(&unpin),
            Some(PinChange::Unpin {
                author: ann(),
                timestamp: 42
            })
        );
        // Without its message's author, or anything to pin, it is no pin.
        let nameless = DataMessage {
            pin_message: Some(PinMessage {
                target_sent_timestamp: Some(42),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(pin_change(&nameless), None);
        assert_eq!(pin_change(&DataMessage::default()), None);
    }
}
