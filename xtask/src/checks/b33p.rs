//! What the checks do in b33p, and wait for it to show.

use std::time::Duration;

use b33p_harness::{
    Action, Answer, ConversationSnapshot, Delivery, DetailsSnapshot, MessageSnapshot,
    ScreenSnapshot, Snapshot,
};

use super::PATIENCE;
use super::presage::Presage;
use crate::error::{Error, Result};
use crate::harness::Harness;

/// At least 8 characters: b33p refuses shorter ones ("Use at least 8 characters.").
const STORE_PASSWORD: &str = "testbox-store";

pub(super) const REQUEST_ACCEPTED: &str =
    "the peer's first message is a message request in b33p, accepted";

/// Waits until b33p's chat list shows `conversation` as `wanted` says.
pub(super) fn b33p_chat(
    b33p: &mut Harness,
    conversation: &str,
    what: &str,
    wanted: impl Fn(&ConversationSnapshot) -> bool,
) -> Result<()> {
    b33p.wait_for(&format!("the chat {what} in b33p"), PATIENCE, |snapshot| {
        snapshot
            .conversations
            .iter()
            .any(|chat| chat.id == conversation && wanted(chat))
            .then_some(())
    })
}

/// The name b33p shows for `group`, from one snapshot: no wait.
pub(super) fn b33p_title(b33p: &mut Harness, group: &str) -> Result<String> {
    b33p.snapshot()?
        .conversations
        .into_iter()
        .find(|chat| chat.id == group)
        .map(|chat| chat.title)
        .ok_or_else(|| Error::Failed(format!("no {group} in b33p")))
}

/// Waits for b33p's outgoing `text` to show as delivered (or read).
pub(super) fn delivered(b33p: &mut Harness, text: &str) -> Result<()> {
    b33p.wait_for(
        &format!("\"delivered\" on {text:?}"),
        PATIENCE,
        |snapshot| {
            outgoing_delivery(snapshot, text)
                .filter(|delivery| matches!(delivery, Delivery::Delivered | Delivery::Read))
        },
    )
    .map(drop)
}

/// Waits for b33p's outgoing `text` to show as read.
pub(super) fn read(b33p: &mut Harness, text: &str) -> Result<()> {
    b33p.wait_for(&format!("\"read\" on {text:?}"), PATIENCE, |snapshot| {
        (outgoing_delivery(snapshot, text) == Some(Delivery::Read)).then_some(())
    })
}

/// What `pick` finds in the details of b33p's open chat, once `notice` shows there: the details
/// are read as they open, so the change must have come first.
pub(super) fn b33p_details<Found>(
    b33p: &mut Harness,
    notice: &str,
    what: &str,
    pick: impl Fn(&DetailsSnapshot) -> Option<Found>,
) -> Result<Found> {
    b33p_shows(b33p, &format!("{notice:?}"), |message| {
        message.notice && message.text == notice
    })?;
    b33p.act(Action::OpenDetails)?;
    let found = b33p.wait_for(&format!("{what} in b33p's details"), PATIENCE, |snapshot| {
        snapshot.details.as_ref().and_then(&pick)
    });
    b33p.act(Action::CloseDetails)?;
    found
}

/// Waits for a message in b33p's open conversation that `matches`.
pub(super) fn b33p_shows(
    b33p: &mut Harness,
    what: &str,
    matches: impl Fn(&MessageSnapshot) -> bool,
) -> Result<()> {
    b33p.wait_for(&format!("{what} in b33p"), PATIENCE, |snapshot| {
        snapshot.messages.iter().any(&matches).then_some(())
    })
}

pub(super) fn wait_for_chats(b33p: &mut Harness, patience: Duration) -> Result<()> {
    b33p.wait_for("b33p's chats", patience, |snapshot| {
        matches!(snapshot.screen, ScreenSnapshot::Chats).then_some(())
    })
}

/// Unlocks b33p's store, or sets its password on the first start.
pub(super) fn unlock(b33p: &mut Harness) -> Result<()> {
    b33p.act(Action::Unlock {
        password: STORE_PASSWORD.to_owned(),
        confirmation: STORE_PASSWORD.to_owned(),
    })
    .map(drop)
}

pub(super) fn select(b33p: &mut Harness, conversation: &str) -> Result<()> {
    b33p.act(Action::Select {
        conversation: conversation.to_owned(),
    })
    .map(drop)
}

/// The account b33p is linked to: its Note to Self is `c:` and the account's ACI
/// (b33p-core/src/signal.rs `conversation_id`).
pub(super) fn own_aci(b33p: &mut Harness) -> Result<String> {
    b33p.wait_for("b33p's Note to Self", PATIENCE, |snapshot| {
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.note_to_self)?
            .id
            .strip_prefix("c:")
            .map(str::to_owned)
    })
}

/// Types and sends `text` in b33p's open conversation, and waits until the server has taken it:
/// what reads it next, a presage-cli `sync` that stops at an empty queue say, finds it there. By
/// the message b33p sent, newer than any outgoing one before: its text isn't `text` once b33p
/// has formatted it (`*bold*`).
pub(super) fn type_and_send(b33p: &mut Harness, text: &str) -> Result<()> {
    // Messages, not notices: b33p's own notices are outgoing too ("You created the group.").
    let sent = |message: &&MessageSnapshot| message.outgoing && !message.notice;
    let newest = |snapshot: &Snapshot| {
        snapshot
            .messages
            .iter()
            .filter(sent)
            .map(|message| message.timestamp)
            .max()
    };
    let before = newest(&b33p.snapshot()?);
    b33p.act(Action::Type {
        text: text.to_owned(),
    })?;
    if !b33p.act(Action::Send)? {
        return Err(Error::Failed(format!("b33p sent nothing for {text:?}")));
    }
    b33p.wait_for(
        &format!("the server taking b33p's {text:?}"),
        PATIENCE,
        |snapshot| {
            snapshot
                .messages
                .iter()
                .filter(|message| sent(message) && Some(message.timestamp) > before)
                .filter_map(|message| message.delivery)
                .find(|delivery| *delivery != Delivery::Sending)
                .map(drop)
        },
    )
}

/// Edits b33p's message at `index` to `text`, as its edit and send buttons do. An edit keeps its
/// message's timestamp, so [`type_and_send`]'s wait for a newer message doesn't apply.
pub(super) fn edit(b33p: &mut Harness, index: u32, text: &str) -> Result<()> {
    b33p.act(Action::Edit { index })?;
    b33p.act(Action::Type {
        text: text.to_owned(),
    })?;
    b33p.act(Action::Send).map(drop)
}

/// The harness index of b33p's own message with `text` in the open conversation.
pub(super) fn outgoing_index(b33p: &mut Harness, text: &str) -> Result<u32> {
    b33p.wait_for(&format!("b33p's {text:?}"), PATIENCE, |snapshot| {
        snapshot
            .messages
            .iter()
            .find(|message| message.outgoing && message.text == text)
            .map(|message| message.index)
    })
}

/// A first message from someone the account hasn't accepted is a message request, which b33p
/// knows once it has read the storage service (b33p-core/src/state.rs `requests_known`), and
/// won't let anyone write to before it is accepted.
pub(super) fn accept_request(b33p: &mut Harness, conversation: &str) -> Result<()> {
    b33p_chat(b33p, conversation, "a message request", |chat| chat.request)?;
    select(b33p, conversation)?;
    b33p.act(Action::Respond {
        answer: Answer::Accept,
    })?;
    b33p_chat(b33p, conversation, "accepted", |chat| !chat.request)
}

/// The peer sends `text` to `recipient_aci`; returns the conversation b33p shows it in.
pub(super) fn peer_text_reaches(
    b33p: &mut Harness,
    peer: &Presage<'_>,
    recipient_aci: &str,
    text: &str,
) -> Result<String> {
    peer.send(recipient_aci, text)?;
    chat_previewing(b33p, text)
}

/// The conversation b33p's chat list shows `text` as the last message of, once it does.
pub(super) fn chat_previewing(b33p: &mut Harness, text: &str) -> Result<String> {
    b33p.wait_for(
        &format!("{text:?} in b33p's chat list"),
        PATIENCE,
        |snapshot| {
            snapshot
                .conversations
                .iter()
                .find(|conversation| conversation.preview == text)
                .map(|conversation| conversation.id.clone())
        },
    )
}

fn outgoing_delivery(snapshot: &Snapshot, text: &str) -> Option<Delivery> {
    snapshot
        .messages
        .iter()
        .find(|message| message.outgoing && message.text == text)
        .and_then(|message| message.delivery)
}
