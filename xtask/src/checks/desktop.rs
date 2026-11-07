use std::time::Duration;

use b33p_harness::Action;

use super::b33p::{
    B33pError, b33p_chat, b33p_shows, chat_previewing, delivered, edit, incoming_index,
    message_timestamp, outgoing_index, select, type_and_send,
};
use super::clients::{desktop_shows, desktop_shows_whole, desktop_text};
use super::presage::Account;
use super::setup::LinkedBox;
use super::{Checks, Failure, PATIENCE, PATIENCE_MS};
use crate::harness::{Harness, HarnessError};
use testbox_tools::drivers;

/// Signal-Desktop linked to the peer's presage-cli primary, so a Signal-authored client is on the
/// other end of b33p. Linked rather than standalone (Desktop CONTRIBUTING.md:174): the link path
/// b33p takes, with no captcha or verification screen to drive.
pub(super) fn desktop(checks: &mut Checks, linked: &mut LinkedBox<'_>) -> Result<(), Failure> {
    let LinkedBox {
        primary_aci,
        b33p,
        desktop,
        conversation,
        ..
    } = linked;

    // presage saves a new sender as a contact, with their profile's name, in a task of its own;
    // the chat takes the name once it is there.
    checks.record("b33p titles the peer's chat with their name", || {
        let name = Account::Peer.name();
        Ok(b33p_chat(
            b33p,
            conversation,
            &format!("titled {name:?}"),
            |chat| chat.title == name,
        )?)
    });

    // Desktop opens the conversation (the peer's own message reached it as a sync), reads b33p's
    // text, and answers.
    let b33p_text = "testbox desktop: from b33p";
    checks.require("text from b33p reaches Desktop", || {
        select(b33p, conversation)?;
        type_and_send(b33p, b33p_text)?;
        desktop.call(drivers::Desktop::OpenConversation {
            aci: primary_aci.clone(),
        })?;
        Ok(desktop_shows(desktop, b33p_text)?)
    })?;

    // Signal-Desktop sends a delivery receipt for each message it receives
    // (ts/messages/handleDataMessage.preload.ts `deliveryReceiptQueue`).
    checks.record("Desktop's delivery receipt reaches b33p", || {
        Ok(delivered(b33p, b33p_text)?)
    });

    // Read receipts are checked in `storage`.

    // TESTPLAN.md §1.4 and §1.6, b33p to Desktop. Desktop's selectors as its own mock tests use
    // them (ts/test-mock/messaging/edit_test.node.ts `.module-message__text`; deleted messages
    // read "{name} deleted this message", _locales/en/messages.json
    // `icu:message--deletedForEveryone--incoming`).
    let edited_text = "testbox desktop: from b33p, edited";
    checks.record("an edit from b33p reaches Desktop", || {
        let index = outgoing_index(b33p, b33p_text)?;
        edit(b33p, index, edited_text)?;
        Ok(desktop_shows(desktop, edited_text)?)
    });

    let formatted_text = "testbox desktop: *bold*";
    checks.record("formatting from b33p reaches Desktop", || {
        type_and_send(b33p, formatted_text)?;
        Ok(desktop.call(drivers::Desktop::WaitForBold {
            text: "bold".to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    checks.record("a delete for everyone from b33p reaches Desktop", || {
        let index = outgoing_index(b33p, "testbox desktop: bold")
            .or_else(|_| outgoing_index(b33p, formatted_text))?;
        b33p.act(Action::Delete {
            index,
            everyone: true,
        })?;
        Ok(desktop_shows(desktop, "deleted this message")?)
    });

    checks.record("text from Desktop reaches b33p", || {
        let text = "testbox desktop: from Desktop";
        desktop.call(drivers::Desktop::Send {
            text: text.to_owned(),
        })?;
        chat_previewing(b33p, text)?;
        Ok(())
    });

    // TESTPLAN.md §1.4 item 3: an edit from the other side shows in b33p.
    checks.record("an edit from Desktop reaches b33p", || {
        let text = "testbox desktop: from Desktop, edited";
        desktop.call(drivers::Desktop::EditMessage {
            old_text: "testbox desktop: from Desktop".to_owned(),
            text: text.to_owned(),
        })?;
        Ok(b33p_shows(
            b33p,
            &format!("{text:?} marked edited"),
            |message| message.text == text && message.edited,
        )?)
    });

    // Reactions and replies each way, and TESTPLAN.md §1.4 item 6, a delete for everyone from the
    // other side. On the messages above: Desktop's edited one and b33p's.
    let from_desktop = "testbox desktop: from Desktop, edited";
    checks.record("a reaction from b33p reaches Desktop", || {
        let index = incoming_index(b33p, from_desktop)?;
        b33p.act(Action::React {
            index,
            emoji: "👍".to_owned(),
        })?;
        Ok(desktop.call(drivers::Desktop::WaitForReaction {
            text: from_desktop.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    checks.record("a reaction from Desktop reaches b33p", || {
        desktop.call(drivers::Desktop::ReactToMessage {
            text: edited_text.to_owned(),
        })?;
        Ok(b33p_shows(b33p, "Desktop's reaction", |message| {
            message.outgoing
                && message.text == edited_text
                && message.reactions.iter().any(|reaction| !reaction.outgoing)
        })?)
    });

    checks.record("a reply from b33p quotes Desktop's message there", || {
        let reply = "testbox desktop: a reply from b33p";
        let index = incoming_index(b33p, from_desktop)?;
        b33p.act(Action::ReplyTo { index })?;
        type_and_send(b33p, reply)?;
        Ok(desktop.call(drivers::Desktop::WaitForQuote {
            text: reply.to_owned(),
            quoted: from_desktop.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    let reply_from_desktop = "testbox desktop: a reply from Desktop";
    checks.record("a reply from Desktop quotes b33p's message in b33p", || {
        desktop.call(drivers::Desktop::ReplyToMessage {
            quoted: edited_text.to_owned(),
            text: reply_from_desktop.to_owned(),
        })?;
        Ok(b33p_shows(b33p, "Desktop's reply", |message| {
            !message.outgoing
                && message.text == reply_from_desktop
                && message
                    .quote
                    .as_ref()
                    .is_some_and(|quote| quote.text == edited_text)
        })?)
    });

    // Desktop names the quoted message as it sends quotes: by its author and a sent time,
    // here of a message edited since.
    checks.record("a click on Desktop's quote shows b33p's message", || {
        let index = incoming_index(b33p, reply_from_desktop)?;
        b33p.act(Action::OpenQuote { index })?;
        Ok(b33p_shows(b33p, "the quoted message marked", |message| {
            message.outgoing && message.text == edited_text && message.highlighted
        })?)
    });

    checks.record("a delete for everyone from Desktop reaches b33p", || {
        let index = incoming_index(b33p, reply_from_desktop)?;
        let timestamp =
            message_timestamp(b33p, "Desktop's reply", |message| message.index == index)?;
        desktop.call(drivers::Desktop::DeleteForEveryone {
            text: reply_from_desktop.to_owned(),
        })?;
        Ok(b33p_shows(b33p, "Desktop's reply deleted", |message| {
            message.timestamp == timestamp && message.deleted
        })?)
    });

    // Pins (Signal-Desktop's PinnedMessages): Desktop names b33p's message by its edit's time,
    // and its pin says "… pinned a message" in the chat. After Desktop's reply is gone: it
    // quotes the same text, and the driver finds a message by its text.
    checks.record("a pin from Desktop shows in b33p, with its notice", || {
        desktop.call(drivers::Desktop::PinMessage {
            text: edited_text.to_owned(),
            duration: "24 hours".to_owned(),
        })?;
        b33p.wait_for("Desktop's pin in b33p", PATIENCE, |snapshot| {
            (snapshot.pinned == [edited_text]).then_some(())
        })?;
        Ok(b33p_shows(b33p, "the pin's notice", |message| {
            message.notice && message.text.ends_with("pinned a message.")
        })?)
    });

    checks.record("an unpin from Desktop shows in b33p", || {
        desktop.call(drivers::Desktop::UnpinMessage {
            text: edited_text.to_owned(),
        })?;
        Ok(
            b33p.wait_for("Desktop's unpin in b33p", PATIENCE, |snapshot| {
                snapshot.pinned.is_empty().then_some(())
            })?,
        )
    });

    checks.record("a pin from b33p shows on Desktop", || {
        let index = outgoing_index(b33p, edited_text)?;
        b33p.act(Action::PinMessage {
            index,
            seconds: Some(24 * 3600),
        })?;
        Ok(desktop.call(drivers::Desktop::WaitForPinned {
            text: edited_text.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    checks.record("an unpin from b33p shows on Desktop", || {
        let index = outgoing_index(b33p, edited_text)?;
        b33p.act(Action::UnpinMessage { index })?;
        Ok(desktop.call(drivers::Desktop::WaitForNoPins {
            timeout_ms: PATIENCE_MS,
        })?)
    });

    // Emoji in a message each way: drawn from a colour bitmap font, which Qt 6.4's QRawFont
    // overran the heap on (the Slint fork's Qt backend draws them itself now).
    checks.record("emoji from b33p reach Desktop", || {
        let text = "testbox desktop: emoji 🦀🎉";
        type_and_send(b33p, text)?;
        Ok(desktop_shows(desktop, text)?)
    });

    checks.record("emoji from Desktop reach b33p", || {
        let text = "testbox desktop: emoji 👍📎";
        desktop.call(drivers::Desktop::Send {
            text: text.to_owned(),
        })?;
        Ok(b33p_shows(b33p, "Desktop's emoji", |message| {
            !message.outgoing && message.text == text
        })?)
    });

    // A reaction taken back each way: the same emoji on the same message removes it
    // (TESTPLAN.md §1.4), and the other side shows none.
    checks.record("a reaction b33p takes back is gone on Desktop", || {
        let index = incoming_index(b33p, from_desktop)?;
        b33p.act(Action::React {
            index,
            emoji: "👍".to_owned(),
        })?;
        Ok(desktop.call(drivers::Desktop::WaitForNoReaction {
            text: from_desktop.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    checks.record("a reaction Desktop takes back is gone in b33p", || {
        desktop.call(drivers::Desktop::ReactToMessage {
            text: edited_text.to_owned(),
        })?;
        Ok(b33p_shows(b33p, "Desktop's reaction gone", |message| {
            message.outgoing
                && message.text == edited_text
                && message.reactions.iter().all(|reaction| reaction.outgoing)
        })?)
    });

    // TESTPLAN.md §1.3 item 1, with Desktop typing. Each side shows and sends them only with
    // its own setting on: b33p's, and Desktop's, which a linked Desktop has as its primary
    // synced it, and presage-cli syncs nothing. The text follows, and the indicator goes.
    checks.record(
        "Desktop's typing shows in b33p, and the text follows",
        || {
            typing_indicators(b33p, true)?;
            desktop.call(drivers::Desktop::SetTypingIndicators {
                on: true,
                timeout_ms: PATIENCE_MS,
            })?;
            let text = "testbox desktop: typed on Desktop";
            desktop.call(drivers::Desktop::TypeText {
                text: text.to_owned(),
            })?;
            b33p.wait_for("Desktop typing", super::PATIENCE, |snapshot| {
                (!snapshot.typing.is_empty()).then_some(())
            })?;
            desktop.call(drivers::Desktop::Send {
                text: text.to_owned(),
            })?;
            b33p_shows(b33p, "Desktop's typed text", |message| {
                !message.outgoing && message.text == text
            })?;
            Ok(b33p.wait_for("no one typing", super::PATIENCE, |snapshot| {
                snapshot.typing.is_empty().then_some(())
            })?)
        },
    );

    // TESTPLAN.md §1.3 item 3, with Desktop for the phone: b33p's typing shows there as the
    // typing bubble, which goes with the text. Both settings are on from the check before.
    checks.record(
        "b33p's typing shows on Desktop, and goes with the text",
        || {
            let text = "testbox desktop: typed in b33p";
            b33p.act(Action::Type {
                text: text.to_owned(),
            })?;
            desktop.call(drivers::Desktop::WaitForTyping {
                timeout_ms: PATIENCE_MS,
            })?;
            if !b33p.act(Action::Send)? {
                return Err(B33pError::NothingSent {
                    text: text.to_owned(),
                }
                .into());
            }
            desktop_shows(desktop, text)?;
            Ok(desktop.call(drivers::Desktop::WaitForNoTyping {
                timeout_ms: PATIENCE_MS,
            })?)
        },
    );

    // A long message: past Signal's inline body (2000 bytes) the text travels as a `LongText`
    // attachment, which the receiver has to fetch and splice in (Desktop
    // ts/util/longAttachment.ts; b33p-core). The whole text, or nothing.
    checks.record("a long text from Desktop reaches b33p whole", || {
        let text = long_text("testbox desktop: a long text from Desktop");
        desktop.call(drivers::Desktop::Send { text: text.clone() })?;
        Ok(b33p_shows(b33p, "Desktop's long text, whole", |message| {
            !message.outgoing && message.text == text
        })?)
    });
    // The other way: whether b33p sends the whole text inline or as Signal's apps do (the
    // first 2000 bytes inline, the whole as a `LongText` attachment), Desktop must show it all.
    // Which of the two it was is in the wire record's content log.
    checks.record("a long text from b33p reaches Desktop whole", || {
        let head = "testbox desktop: a long text from b33p";
        type_and_send(b33p, &long_text(head))?;
        Ok(desktop_shows_whole(desktop, head)?)
    });

    // TESTPLAN.md §2.10: a link with the box's preview page behind it (testbox/tools `preview`),
    // its preview fetched by b33p and sent along, which Desktop shows as a card.
    checks.record("a link from b33p reaches Desktop with its preview", || {
        link_previews(b33p, true)?;
        let head = "testbox desktop: a link from b33p";
        let sent = type_and_send(
            b33p,
            &format!("{head} {}", testbox_tools::keys::preview_url("/page/7")),
        );
        let off = link_previews(b33p, false);
        sent?;
        off?;
        Ok(desktop.call(drivers::Desktop::WaitForLinkPreview {
            text: head.to_owned(),
            title: testbox_tools::preview_title(7),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    // TESTPLAN.md §1.5, with Desktop on the other end. Desktop's notice (messages.json
    // `icu:DisappearingTimer--set`) names the time; its timer on an incoming message starts when
    // it reads it, which the open conversation does at once; b33p's on its own starts at the send.
    checks.record(
        "a timer b33p sets shows on Desktop, and a message then goes on both",
        || {
            b33p.act(Action::SetTimer {
                conversation: conversation.clone(),
                seconds: 30,
            })?;
            b33p_chat(b33p, conversation, "with a 30 s timer", |chat| {
                chat.expire_timer == 30
            })?;
            desktop_text(desktop, "30 seconds")?;
            let text = "testbox desktop: gone in 30 seconds";
            type_and_send(b33p, text)?;
            desktop_shows(desktop, text)?;
            // Desktop's timer starts when it reads the message, which it does for the open
            // conversation once its window counts as active: a click on the conversation's
            // row makes it so (`storage`'s read receipts the same way).
            desktop.call(drivers::Desktop::OpenConversation {
                aci: primary_aci.clone(),
            })?;
            desktop.call(drivers::Desktop::WaitForMessageGone {
                text: text.to_owned(),
                timeout_ms: EXPIRY_PATIENCE_MS,
            })?;
            Ok(b33p.wait_for(
                "b33p's message gone",
                Duration::from_millis(EXPIRY_PATIENCE_MS),
                |snapshot| {
                    (!snapshot.messages.iter().any(|message| message.text == text)).then_some(())
                },
            )?)
        },
    );
    checks.record("a timer Desktop sets shows in b33p, and off again", || {
        desktop.call(drivers::Desktop::SetTimer { seconds: 5 * 60 })?;
        b33p_chat(b33p, conversation, "with a 5 min timer", |chat| {
            chat.expire_timer == 5 * 60
        })?;
        b33p_shows(b33p, "the timer's notice", |message| {
            message.notice && message.text.contains("5 minutes")
        })?;
        desktop.call(drivers::Desktop::SetTimer { seconds: 0 })?;
        Ok(b33p_chat(b33p, conversation, "without a timer", |chat| {
            chat.expire_timer == 0
        })?)
    });

    Ok(())
}

/// How long a message with a 30 s timer may take to go, on either side.
const EXPIRY_PATIENCE_MS: u64 = 90_000;

/// Turns b33p's link previews on or off, as its settings switch does.
fn link_previews(b33p: &mut Harness, on: bool) -> Result<(), HarnessError> {
    b33p.act(Action::SetSettings {
        theme: None,
        style: None,
        text_size: None,
        animations: None,
        show_avatars: None,
        notifications: None,
        notification_content: None,
        show_activity: None,
        show_calendar: None,
        close_to_tray: None,
        read_receipts: None,
        typing_indicators: None,
        link_previews: Some(on),
        sync_to_phone: None,
    })?;
    b33p.wait_for("b33p's link previews", super::PATIENCE, |snapshot| {
        (snapshot.settings.link_previews == on).then_some(())
    })
}

/// Turns b33p's typing indicators on or off, as its settings switch does.
fn typing_indicators(b33p: &mut Harness, on: bool) -> Result<(), HarnessError> {
    b33p.act(Action::SetSettings {
        theme: None,
        style: None,
        text_size: None,
        animations: None,
        show_avatars: None,
        notifications: None,
        notification_content: None,
        show_activity: None,
        show_calendar: None,
        close_to_tray: None,
        read_receipts: None,
        typing_indicators: Some(on),
        link_previews: None,
        sync_to_phone: None,
    })?;
    b33p.wait_for("b33p's typing indicators", super::PATIENCE, |snapshot| {
        (snapshot.settings.typing_indicators == on).then_some(())
    })
}

/// `head`, then numbered lines up to well past 2000 bytes, ending in a line that says so: a
/// text cut at the inline limit would lack it.
pub(super) fn long_text(head: &str) -> String {
    let mut text = head.to_owned();
    let mut n = 1;
    while text.len() < 2600 {
        text.push_str(&format!(
            "\nline {n} of a long message, to be carried whole"
        ));
        n += 1;
    }
    text.push('\n');
    text.push_str(LAST_LINE);
    text
}

/// [`long_text`]'s last line, which a text cut at the inline limit lacks.
pub(super) const LAST_LINE: &str = "the last line, past the inline limit";
