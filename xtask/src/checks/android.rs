use super::b33p::{b33p_chat, b33p_shows, delivered, select, type_and_send};
use super::clients::{android_containing, android_shows, desktop_shows, desktop_shows_whole};
use super::desktop::long_text;
use super::presage::Account;
use super::setup::{AndroidBox, NOTE};
use super::{Checks, PATIENCE_MS};
use crate::error::Result;
use testbox_tools::drivers;

/// Signal-Android's `local` flavor registers as a primary and b33p links to it; Signal-Desktop
/// links to a presage-cli peer as in `desktop`. Text and receipts pass between the two accounts,
/// with each one's linked device in step.
pub(super) fn android(checks: &mut Checks, fixture: &mut AndroidBox<'_>) -> Result<()> {
    let AndroidBox {
        peer,
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
    } = fixture;
    checks.record("Android's note reaches b33p in the history", || {
        select(b33p, &format!("c:{android_aci}"))?;
        b33p_shows(b33p, NOTE, |message| message.text == NOTE)
    });

    // Every Desktop check from here on looks in this conversation, which Desktop has from the
    // peer's own message, as a sync; opened once, so that one check failing doesn't leave the
    // next ones looking at nothing.
    checks.require("Desktop has the peer's conversation with Android", || {
        desktop.call(drivers::Desktop::OpenConversation {
            aci: android_aci.clone(),
        })
    })?;

    let b33p_text = "testbox android: from b33p";
    checks.record(
        "text from b33p reaches the peer and Desktop, and Android as a sync",
        || {
            select(b33p, conversation)?;
            type_and_send(b33p, b33p_text)?;
            peer.sync_printing(b33p_text)?;
            android_shows(android, b33p_text)?;
            desktop_shows(desktop, b33p_text)
        },
    );
    // As in `desktop`: Desktop, linked to the peer, sends a delivery receipt for b33p's text.
    checks.record("Desktop's delivery receipt reaches b33p", || {
        delivered(b33p, b33p_text)
    });

    let android_text = "testbox android: from Android";
    checks.record(
        "text from Android reaches the peer and Desktop, and b33p as a sync",
        || {
            android.call(drivers::Android::OpenConversation {
                text: b33p_text.to_owned(),
            })?;
            android.call(drivers::Android::Send {
                text: android_text.to_owned(),
            })?;
            peer.sync_printing(android_text)?;
            // b33p shows its account's own message as outgoing, wherever it was sent from.
            b33p_shows(b33p, android_text, |message| {
                message.text == android_text && message.outgoing
            })?;
            desktop_shows(desktop, android_text)
        },
    );

    let desktop_text = "testbox android: from Desktop";
    checks.record("text from Desktop reaches Android and b33p", || {
        desktop.call(drivers::Desktop::Send {
            text: desktop_text.to_owned(),
        })?;
        android_shows(android, desktop_text)?;
        b33p_shows(b33p, desktop_text, |message| {
            message.text == desktop_text && !message.outgoing
        })
    });

    // Signal-Android sends a delivery receipt only for a sealed-sender message
    // (DataMessageProcessor.kt `metadata.sealedSender`): Desktop's, not presage-cli's first one,
    // which goes out unsealed while the peer has no profile key of Android's.
    checks.record("Android's delivery receipt reaches Desktop", || {
        desktop.call(drivers::Desktop::WaitForDelivered {
            text: desktop_text.to_owned(),
            timeout_ms: PATIENCE_MS,
        })
    });

    // What the account does on its phone reaches b33p as syncs, and the peer's Desktop as the
    // messages themselves: TESTPLAN.md §1.4 and §1.5 from the phone's side. Android's
    // conversation with the peer is open from the send above.
    checks.record(
        "a reaction from Android shows in b33p and on Desktop",
        || {
            android.call(drivers::Android::ReactToMessage {
                text: desktop_text.to_owned(),
            })?;
            select(b33p, conversation)?;
            b33p_shows(b33p, "Android's reaction", |message| {
                message.text == desktop_text
                    && message.reactions.iter().any(|reaction| reaction.outgoing)
            })?;
            desktop.call(drivers::Desktop::WaitForReaction {
                text: desktop_text.to_owned(),
                timeout_ms: PATIENCE_MS,
            })
        },
    );
    let android_reply = "testbox android: a reply from Android";
    checks.record(
        "a reply from Android quotes Desktop's text in b33p and on Desktop",
        || {
            android.call(drivers::Android::LongPress {
                text: desktop_text.to_owned(),
            })?;
            android.call(drivers::Android::Tap {
                text: "Reply".to_owned(),
            })?;
            android.call(drivers::Android::Send {
                text: android_reply.to_owned(),
            })?;
            b33p_shows(b33p, "Android's reply", |message| {
                message.outgoing
                    && message.text == android_reply
                    && message
                        .quote
                        .as_ref()
                        .is_some_and(|quote| quote.text == desktop_text)
            })?;
            desktop.call(drivers::Desktop::WaitForQuote {
                text: android_reply.to_owned(),
                quoted: desktop_text.to_owned(),
                timeout_ms: PATIENCE_MS,
            })
        },
    );
    let android_edited = "testbox android: from Android, edited";
    checks.record("an edit from Android shows in b33p and on Desktop", || {
        android.call(drivers::Android::LongPress {
            text: android_text.to_owned(),
        })?;
        android.call(drivers::Android::Tap {
            text: "Edit".to_owned(),
        })?;
        android.call(drivers::Android::Send {
            text: android_edited.to_owned(),
        })?;
        b33p_shows(b33p, "Android's edit", |message| {
            message.outgoing && message.text == android_edited && message.edited
        })?;
        desktop_shows(desktop, android_edited)
    });
    checks.record(
        "a delete for everyone from Android shows in b33p and on Desktop",
        || {
            let timestamp = b33p
                .snapshot()?
                .messages
                .iter()
                .find(|message| message.outgoing && message.text == android_edited)
                .map(|message| message.timestamp)
                .ok_or("Android's edited message went from b33p's view")?;
            android.call(drivers::Android::LongPress {
                text: android_edited.to_owned(),
            })?;
            android.call(drivers::Android::Tap {
                text: "Delete".to_owned(),
            })?;
            android.call(drivers::Android::Tap {
                text: "Delete for everyone".to_owned(),
            })?;
            // Then its confirmation ("This message will be deleted for everyone in the chat").
            android_containing(android, "will be deleted for everyone")?;
            android.call(drivers::Android::Tap {
                text: "Delete for everyone".to_owned(),
            })?;
            b33p_shows(b33p, "Android's delete", |message| {
                message.timestamp == timestamp && message.deleted
            })?;
            // "{name} deleted this message" (messages.json `icu:message--deletedForEveryone--incoming`).
            desktop_shows(desktop, "deleted this message")
        },
    );
    checks.record(
        "a timer Android sets shows in b33p and on Desktop, and off again",
        || {
            android.call(drivers::Android::SetTimer { seconds: 5 * 60 })?;
            b33p_chat(b33p, conversation, "with a 5 min timer", |chat| {
                chat.expire_timer == 5 * 60
            })?;
            super::clients::desktop_text(desktop, "5 minutes")?;
            android.call(drivers::Android::SetTimer { seconds: 0 })?;
            b33p_chat(b33p, conversation, "without a timer", |chat| {
                chat.expire_timer == 0
            })
        },
    );
    // TESTPLAN.md §1.11: Android is this account's phone, so what b33p writes to itself is a
    // sync to Android, which lists it under Note to Self.
    let note = "testbox android: a note b33p writes to itself";
    checks.record("a note b33p writes to itself shows on Android", || {
        select(b33p, &format!("c:{android_aci}"))?;
        type_and_send(b33p, note)?;
        android.call(drivers::Android::OpenConversation {
            text: "Note to Self".to_owned(),
        })?;
        android_shows(android, note)
    });

    // Long texts, as Signal's apps carry them: the first 2000 bytes inline and the whole as a
    // `LongText` attachment. From Android, b33p gets the transcript of its account's own send,
    // with the attachment to fetch; the peer's Desktop the message itself.
    checks.record(
        "a long text from Android reaches b33p and Desktop whole",
        || {
            let head = "testbox android: a long text from Android";
            let text = long_text(head);
            // The peer's row, by the peer's name: its preview is cut short.
            android.call(drivers::Android::OpenConversation {
                text: Account::Peer.name().into_owned(),
            })?;
            android.call(drivers::Android::Send { text: text.clone() })?;
            select(b33p, conversation)?;
            b33p_shows(b33p, "Android's long text, whole", |message| {
                message.outgoing && message.text == text
            })?;
            desktop_shows_whole(desktop, head)
        },
    );
    // Android folds a long text behind "Read More", a span of the text that only a touch on it
    // unfolds: that it arrived shows, whole is for Desktop and the wire record's content log
    // (a `LongText` attachment beside the first 2000 bytes, or the whole text inline).
    checks.record(
        "a long text from b33p reaches Android, and Desktop whole",
        || {
            let head = "testbox android: a long text from b33p";
            select(b33p, conversation)?;
            type_and_send(b33p, &long_text(head))?;
            desktop_shows_whole(desktop, head)?;
            android_containing(android, head)
        },
    );

    Ok(())
}
