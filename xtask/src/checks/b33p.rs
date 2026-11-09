//! What the checks do in b33p, and wait for it to show.

use std::time::{Duration, Instant};

use b33p_harness::{
    Action, Answer, ConversationSnapshot, Delivery, DetailsSnapshot, MessageSnapshot,
    ScreenSnapshot, SettingsChange, Snapshot,
};

use super::presage::Presage;
use super::{Failure, PATIENCE};
use crate::harness::{Harness, HarnessError};

/// At least 8 characters: b33p refuses shorter ones ("Use at least 8 characters.").
const STORE_PASSWORD: &str = "testbox-store";

pub(super) const REQUEST_ACCEPTED: &str =
    "the peer's first message is a message request in b33p, accepted";

/// What the helpers here find b33p not doing, or fail at in its harness on the way.
#[derive(Debug, thiserror::Error)]
pub(super) enum B33pError {
    /// b33p's chat list has no such conversation.
    #[error("no {conversation} in b33p")]
    NoChat { conversation: String },
    #[error("reading {group}'s name in b33p")]
    Title {
        group: String,
        #[source]
        source: HarnessError,
    },
    /// No message in b33p's open conversation matched, in time; with what it showed instead,
    /// as of its last screen.
    #[error("{what} not shown in b33p ({})", shown(selected.as_deref(), last))]
    NotShown {
        what: String,
        /// The conversation open.
        selected: Option<String>,
        /// Its last few messages' texts, newest first.
        last: Vec<String>,
        #[source]
        source: HarnessError,
    },
    #[error("reading {what} in b33p's details, after {notice:?}")]
    Details {
        notice: String,
        what: String,
        #[source]
        source: HarnessError,
    },
    #[error("looking for the story {text:?} in b33p")]
    Story {
        text: String,
        #[source]
        source: HarnessError,
    },
    /// The open conversation's stories, stepped through, had none with the text.
    #[error("no story {text:?} among the {count}")]
    NoStory { text: String, count: usize },
    #[error("sending {text:?} in b33p")]
    Send {
        text: String,
        #[source]
        source: HarnessError,
    },
    /// b33p's send button sent no command to its backend.
    #[error("b33p sent nothing for {text:?}")]
    NothingSent { text: String },
    #[error("reading the timestamp of {what} in b33p")]
    Timestamp {
        what: String,
        #[source]
        source: HarnessError,
    },
    /// A message the check had seen is no longer in b33p's open conversation.
    #[error("{what} went from b33p's view")]
    Gone { what: String },
}

/// What [`B33pError::NotShown`] says b33p showed.
fn shown(selected: Option<&str>, last: &[String]) -> String {
    format!(
        "open {selected:?}, its last messages {}",
        last.iter()
            .map(|text| format!("{text:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Waits until b33p's chat list shows `conversation` as `wanted` says.
pub(super) fn b33p_chat(
    b33p: &mut Harness,
    conversation: &str,
    what: &str,
    wanted: impl Fn(&ConversationSnapshot) -> bool,
) -> Result<(), HarnessError> {
    b33p.wait_for(&format!("the chat {what} in b33p"), PATIENCE, |snapshot| {
        snapshot
            .conversations
            .iter()
            .any(|chat| chat.id == conversation && wanted(chat))
            .then_some(())
    })
}

/// The name b33p shows for `group`, from one snapshot: no wait.
pub(super) fn b33p_title(b33p: &mut Harness, group: &str) -> Result<String, B33pError> {
    b33p.snapshot()
        .map_err(|source| B33pError::Title {
            group: group.to_owned(),
            source,
        })?
        .conversations
        .into_iter()
        .find(|chat| chat.id == group)
        .map(|chat| chat.title)
        .ok_or_else(|| B33pError::NoChat {
            conversation: group.to_owned(),
        })
}

/// Waits for b33p's outgoing `text` to show as delivered (or read).
pub(super) fn delivered(b33p: &mut Harness, text: &str) -> Result<(), HarnessError> {
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
pub(super) fn read(b33p: &mut Harness, text: &str) -> Result<(), HarnessError> {
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
) -> Result<Found, B33pError> {
    b33p_shows(b33p, &format!("{notice:?}"), |message| {
        message.notice && message.text == notice
    })?;
    let details = |source| B33pError::Details {
        notice: notice.to_owned(),
        what: what.to_owned(),
        source,
    };
    b33p.act(Action::OpenDetails).map_err(details)?;
    let found = b33p.wait_for(&format!("{what} in b33p's details"), PATIENCE, |snapshot| {
        snapshot.details.as_ref().and_then(&pick)
    });
    b33p.act(Action::CloseDetails).map_err(details)?;
    found.map_err(details)
}

/// Waits for a message in b33p's open conversation that `matches`.
pub(super) fn b33p_shows(
    b33p: &mut Harness,
    what: &str,
    matches: impl Fn(&MessageSnapshot) -> bool,
) -> Result<(), B33pError> {
    // What it showed instead, as of the last screen: which chat was open, and its last messages.
    let (mut selected, mut last) = (None, Vec::new());
    b33p.wait_for(&format!("{what} in b33p"), PATIENCE, |snapshot| {
        if snapshot.messages.iter().any(&matches) {
            return Some(());
        }
        selected.clone_from(&snapshot.selected);
        last = snapshot
            .messages
            .iter()
            .rev()
            .take(4)
            .map(|message| message.text.clone())
            .collect();
        None
    })
    .map_err(|source| B33pError::NotShown {
        what: what.to_owned(),
        selected,
        last,
        source,
    })
}

pub(super) fn wait_for_chats(b33p: &mut Harness, patience: Duration) -> Result<(), HarnessError> {
    b33p.wait_for("b33p's chats", patience, |snapshot| {
        matches!(snapshot.screen, ScreenSnapshot::Chats).then_some(())
    })
}

/// Unlocks b33p's store, or sets its password on the first start.
pub(super) fn unlock(b33p: &mut Harness) -> Result<(), HarnessError> {
    b33p.act(Action::Unlock {
        password: STORE_PASSWORD.to_owned(),
        confirmation: STORE_PASSWORD.to_owned(),
    })
    .map(drop)
}

/// Waits for b33p to show a story with `text` among `conversation`'s (b33p files a group's
/// stories under the group, b33p-core `stories::on_story`), and closes it. The viewer opens on
/// the first story not viewed yet, or the first of all: the others are stepped to. It is
/// opened again until it shows: before the story came, there was none to open.
pub(super) fn story_shows(
    b33p: &mut Harness,
    conversation: &str,
    text: &str,
) -> Result<(), B33pError> {
    let story = |source| B33pError::Story {
        text: text.to_owned(),
        source,
    };
    let deadline = Instant::now() + PATIENCE;
    let shown = loop {
        b33p.act(Action::OpenStory {
            conversation: conversation.to_owned(),
        })
        .map_err(story)?;
        let shown = story_stepped_to(b33p, text);
        if shown.is_ok() || Instant::now() >= deadline {
            break shown;
        }
    };
    b33p.act(Action::CloseStory).map_err(story)?;
    shown
}

/// Steps through the open conversation's stories to the one with `text`, from the one shown
/// to the last, then from the first. A story's `position` counts from 1.
fn story_stepped_to(b33p: &mut Harness, text: &str) -> Result<(), B33pError> {
    let looking = |source| B33pError::Story {
        text: text.to_owned(),
        source,
    };
    let current = |snapshot: &Snapshot| snapshot.story.clone();
    let mut story = b33p
        .wait_for("a story open", Duration::from_secs(5), current)
        .map_err(looking)?;
    let mut steps = 0;
    loop {
        if story.text.as_deref() == Some(text) {
            return Ok(());
        }
        if steps >= story.count {
            return Err(B33pError::NoStory {
                text: text.to_owned(),
                count: story.count,
            });
        }
        // Past the last, the viewer closes: back to the first then.
        let by = if story.position >= story.count {
            1 - i32::try_from(story.position).unwrap_or(i32::MAX)
        } else {
            1
        };
        let position = story.position;
        b33p.act(Action::StepStory { by }).map_err(looking)?;
        story = b33p
            .wait_for("the next story", Duration::from_secs(5), |snapshot| {
                current(snapshot).filter(|story| story.position != position)
            })
            .map_err(looking)?;
        steps += 1;
    }
}

pub(super) fn select(b33p: &mut Harness, conversation: &str) -> Result<(), HarnessError> {
    b33p.act(Action::Select {
        conversation: conversation.to_owned(),
    })
    .map(drop)
}

/// The account b33p is linked to: the person its Note to Self is with.
pub(super) fn own_aci(b33p: &mut Harness) -> Result<String, HarnessError> {
    b33p.wait_for("b33p's Note to Self", PATIENCE, |snapshot| {
        let note = snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.note_to_self)?;
        b33p_core::ConversationId(note.id.clone())
            .contact_id()
            .map(str::to_owned)
    })
}

/// Types and sends `text` in b33p's open conversation, and waits until the server has taken it:
/// what reads it next, a presage-cli `sync` that stops at an empty queue say, finds it there. By
/// the message b33p sent, newer than any outgoing one before: its text isn't `text` once b33p
/// has formatted it (`*bold*`).
pub(super) fn type_and_send(b33p: &mut Harness, text: &str) -> Result<(), B33pError> {
    let sending = |source| B33pError::Send {
        text: text.to_owned(),
        source,
    };
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
    let before = newest(&b33p.snapshot().map_err(sending)?);
    send_text(b33p, text)?;
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
    .map_err(sending)
}

/// Types and sends `text` in b33p's open conversation, as its send button does: no wait for
/// the server, only that a command went to the backend.
pub(super) fn send_text(b33p: &mut Harness, text: &str) -> Result<(), B33pError> {
    let sending = |source| B33pError::Send {
        text: text.to_owned(),
        source,
    };
    b33p.act(Action::Type {
        text: text.to_owned(),
    })
    .map_err(sending)?;
    if b33p.act(Action::Send).map_err(sending)? {
        Ok(())
    } else {
        Err(B33pError::NothingSent {
            text: text.to_owned(),
        })
    }
}

/// The timestamp of the message `what` names in b33p's open conversation, the first that
/// `matches`, from one snapshot: no wait.
pub(super) fn message_timestamp(
    b33p: &mut Harness,
    what: &str,
    matches: impl Fn(&MessageSnapshot) -> bool,
) -> Result<u64, B33pError> {
    b33p.snapshot()
        .map_err(|source| B33pError::Timestamp {
            what: what.to_owned(),
            source,
        })?
        .messages
        .iter()
        .find(|message| matches(message))
        .map(|message| message.timestamp)
        .ok_or_else(|| B33pError::Gone {
            what: what.to_owned(),
        })
}

/// Edits b33p's message at `index` to `text`, as its edit and send buttons do. An edit keeps its
/// message's timestamp, so [`type_and_send`]'s wait for a newer message doesn't apply.
pub(super) fn edit(b33p: &mut Harness, index: u32, text: &str) -> Result<(), HarnessError> {
    b33p.act(Action::Edit { index })?;
    b33p.act(Action::Type {
        text: text.to_owned(),
    })?;
    b33p.act(Action::Send).map(drop)
}

/// The harness index of b33p's own message with `text` in the open conversation.
pub(super) fn outgoing_index(b33p: &mut Harness, text: &str) -> Result<u32, HarnessError> {
    b33p.wait_for(&format!("b33p's {text:?}"), PATIENCE, |snapshot| {
        snapshot
            .messages
            .iter()
            .find(|message| message.outgoing && message.text == text)
            .map(|message| message.index)
    })
}

/// The harness index of the newest message from someone else with `text` in the open
/// conversation.
pub(super) fn incoming_index(b33p: &mut Harness, text: &str) -> Result<u32, HarnessError> {
    b33p.wait_for(&format!("the incoming {text:?}"), PATIENCE, |snapshot| {
        snapshot
            .messages
            .iter()
            .rev()
            .find(|message| !message.outgoing && !message.notice && message.text == text)
            .map(|message| message.index)
    })
}

/// A first message from someone the account hasn't accepted is a message request, which b33p
/// knows once it has read the storage service (b33p-core/src/state.rs `requests_known`), and
/// won't let anyone write to before it is accepted.
pub(super) fn accept_request(b33p: &mut Harness, conversation: &str) -> Result<(), HarnessError> {
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
) -> Result<String, Failure> {
    peer.send(recipient_aci, text)?;
    Ok(chat_previewing(b33p, text)?)
}

/// The conversation b33p's chat list shows `text` as the last message of, once it does.
pub(super) fn chat_previewing(b33p: &mut Harness, text: &str) -> Result<String, HarnessError> {
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

/// Turns b33p's link previews on or off, as its settings switch does.
pub(super) fn link_previews(b33p: &mut Harness, on: bool) -> Result<(), HarnessError> {
    b33p.act(Action::SetSettings(SettingsChange {
        link_previews: Some(on),
        ..SettingsChange::default()
    }))?;
    b33p.wait_for("b33p's link previews", PATIENCE, |snapshot| {
        (snapshot.settings.link_previews == on).then_some(())
    })
}
