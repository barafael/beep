use super::b33p::{b33p_shows, delivered, select, type_and_send};
use super::clients::{android_shows, desktop_shows};
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

    Ok(())
}
