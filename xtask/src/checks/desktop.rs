use b33p_harness::Action;

use super::b33p::{
    REQUEST_ACCEPTED, accept_request, b33p_chat, b33p_shows, chat_previewing, delivered, edit,
    incoming_index, outgoing_index, peer_text_reaches, select, type_and_send,
};
use super::clients::desktop_shows;
use super::presage::Account;
use super::setup::{LinkedBox, linked_box, linked_desktop};
use super::{Checks, PATIENCE_MS};
use crate::Sockets;
use crate::error::Result;
use crate::pod::Pod;
use testbox_tools::drivers;

/// Signal-Desktop linked to the peer's presage-cli primary, so a Signal-authored client is on the
/// other end of b33p. Linked rather than standalone (Desktop CONTRIBUTING.md:174): the link path
/// b33p takes, with no captcha or verification screen to drive.
pub(crate) fn desktop(checks: &mut Checks, containers: &Pod, sockets: &Sockets) -> Result<()> {
    let LinkedBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = linked_box(checks, containers, sockets)?;

    let mut desktop = linked_desktop(checks, &sockets.desktop, &peer)?;

    // b33p can't start a conversation by phone number (contact discovery, not in the box), so the
    // peer's primary writes first; Desktop gets it as a sync of the peer's own message.
    let conversation = checks.require("the peer's primary writes to b33p", || {
        peer_text_reaches(
            &mut b33p,
            &peer,
            &primary_aci,
            "testbox desktop: from the peer",
        )
    })?;
    checks.require(REQUEST_ACCEPTED, || {
        accept_request(&mut b33p, &conversation)
    })?;
    // presage saves a new sender as a contact, with their profile's name, in a task of its own;
    // the chat takes the name once it is there.
    checks.record("b33p titles the peer's chat with their name", || {
        let name = Account::Peer.name();
        b33p_chat(
            &mut b33p,
            &conversation,
            &format!("titled {name:?}"),
            |chat| chat.title == name,
        )
    });

    // Desktop opens the conversation (the peer's own message reached it as a sync), reads b33p's
    // text, and answers.
    let b33p_text = "testbox desktop: from b33p";
    checks.require("text from b33p reaches Desktop", || {
        select(&mut b33p, &conversation)?;
        type_and_send(&mut b33p, b33p_text)?;
        desktop.call(drivers::Desktop::OpenConversation {
            aci: primary_aci.clone(),
        })?;
        desktop_shows(&mut desktop, b33p_text)
    })?;

    // Signal-Desktop sends a delivery receipt for each message it receives
    // (ts/messages/handleDataMessage.preload.ts `deliveryReceiptQueue`).
    checks.record("Desktop's delivery receipt reaches b33p", || {
        delivered(&mut b33p, b33p_text)
    });

    // Read receipts are checked in `storage`: Desktop sends one only with the account's
    // read-receipt setting on, which a linked device takes from the storage service or the
    // primary's configuration sync, and presage-cli sends no such sync.

    // TESTPLAN.md §1.4 and §1.6, b33p to Desktop. Desktop's selectors as its own mock tests use
    // them (ts/test-mock/messaging/edit_test.node.ts `.module-message__text`; deleted messages
    // read "{name} deleted this message", _locales/en/messages.json
    // `icu:message--deletedForEveryone--incoming`).
    let edited_text = "testbox desktop: from b33p, edited";
    checks.record("an edit from b33p reaches Desktop", || {
        let index = outgoing_index(&mut b33p, b33p_text)?;
        edit(&mut b33p, index, edited_text)?;
        desktop_shows(&mut desktop, edited_text)
    });

    let formatted_text = "testbox desktop: *bold*";
    checks.record("formatting from b33p reaches Desktop", || {
        type_and_send(&mut b33p, formatted_text)?;
        desktop.call(drivers::Desktop::WaitForBold {
            text: "bold".to_owned(),
            timeout_ms: PATIENCE_MS,
        })
    });

    checks.record("a delete for everyone from b33p reaches Desktop", || {
        let index = outgoing_index(&mut b33p, "testbox desktop: bold")
            .or_else(|_| outgoing_index(&mut b33p, formatted_text))?;
        b33p.act(Action::Delete {
            index,
            everyone: true,
        })?;
        desktop_shows(&mut desktop, "deleted this message")
    });

    checks.record("text from Desktop reaches b33p", || {
        let text = "testbox desktop: from Desktop";
        desktop.call(drivers::Desktop::Send {
            text: text.to_owned(),
        })?;
        chat_previewing(&mut b33p, text).map(drop)
    });

    // TESTPLAN.md §1.4 item 3: an edit from the other side shows in b33p.
    checks.record("an edit from Desktop reaches b33p", || {
        let text = "testbox desktop: from Desktop, edited";
        desktop.call(drivers::Desktop::EditMessage {
            old_text: "testbox desktop: from Desktop".to_owned(),
            text: text.to_owned(),
        })?;
        b33p_shows(&mut b33p, &format!("{text:?} marked edited"), |message| {
            message.text == text && message.edited
        })
    });

    // Reactions and replies each way, and TESTPLAN.md §1.4 item 6, a delete for everyone from the
    // other side. On the messages above: Desktop's edited one and b33p's.
    let from_desktop = "testbox desktop: from Desktop, edited";
    checks.record("a reaction from b33p reaches Desktop", || {
        let index = incoming_index(&mut b33p, from_desktop)?;
        b33p.act(Action::React {
            index,
            emoji: "👍".to_owned(),
        })?;
        desktop.call(drivers::Desktop::WaitForReaction {
            text: from_desktop.to_owned(),
            timeout_ms: PATIENCE_MS,
        })
    });

    checks.record("a reaction from Desktop reaches b33p", || {
        desktop.call(drivers::Desktop::ReactToMessage {
            text: edited_text.to_owned(),
        })?;
        b33p_shows(&mut b33p, "Desktop's reaction", |message| {
            message.outgoing
                && message.text == edited_text
                && message.reactions.iter().any(|reaction| !reaction.outgoing)
        })
    });

    checks.record("a reply from b33p quotes Desktop's message there", || {
        let reply = "testbox desktop: a reply from b33p";
        let index = incoming_index(&mut b33p, from_desktop)?;
        b33p.act(Action::ReplyTo { index })?;
        type_and_send(&mut b33p, reply)?;
        desktop.call(drivers::Desktop::WaitForQuote {
            text: reply.to_owned(),
            quoted: from_desktop.to_owned(),
            timeout_ms: PATIENCE_MS,
        })
    });

    let reply_from_desktop = "testbox desktop: a reply from Desktop";
    checks.record("a reply from Desktop quotes b33p's message in b33p", || {
        desktop.call(drivers::Desktop::ReplyToMessage {
            quoted: edited_text.to_owned(),
            text: reply_from_desktop.to_owned(),
        })?;
        b33p_shows(&mut b33p, "Desktop's reply", |message| {
            !message.outgoing
                && message.text == reply_from_desktop
                && message
                    .quote
                    .as_ref()
                    .is_some_and(|quote| quote.text == edited_text)
        })
    });

    checks.record("a delete for everyone from Desktop reaches b33p", || {
        let index = incoming_index(&mut b33p, reply_from_desktop)?;
        let timestamp = b33p
            .snapshot()?
            .messages
            .iter()
            .find(|message| message.index == index)
            .map(|message| message.timestamp)
            .ok_or("Desktop's reply went from b33p's view")?;
        desktop.call(drivers::Desktop::DeleteForEveryone {
            text: reply_from_desktop.to_owned(),
        })?;
        b33p_shows(&mut b33p, "Desktop's reply deleted", |message| {
            message.timestamp == timestamp && message.deleted
        })
    });

    // Emoji in a message each way: drawn from a colour bitmap font, which Qt 6.4's QRawFont
    // overran the heap on (the Slint fork's Qt backend draws them itself now).
    checks.record("emoji from b33p reach Desktop", || {
        let text = "testbox desktop: emoji 🦀🎉";
        type_and_send(&mut b33p, text)?;
        desktop_shows(&mut desktop, text)
    });

    checks.record("emoji from Desktop reach b33p", || {
        let text = "testbox desktop: emoji 👍📎";
        desktop.call(drivers::Desktop::Send {
            text: text.to_owned(),
        })?;
        b33p_shows(&mut b33p, "Desktop's emoji", |message| {
            !message.outgoing && message.text == text
        })
    });

    Ok(())
}
