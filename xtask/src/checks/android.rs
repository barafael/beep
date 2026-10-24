use super::b33p::{
    REQUEST_ACCEPTED, accept_request, b33p_shows, delivered, peer_text_reaches, select,
    type_and_send,
};
use super::clients::{android_shows, desktop_shows};
use super::setup::{AndroidBox, android_box};
use super::{Checks, PATIENCE_MS};
use crate::Sockets;
use crate::error::Result;
use crate::pod::Pod;
use testbox_tools::drivers;

/// Signal-Android's `local` flavor registers as a primary and b33p links to it; Signal-Desktop
/// links to a presage-cli peer as in `desktop`. Text and receipts pass between the two accounts,
/// with each one's linked device in step.
pub(crate) fn android(checks: &mut Checks, containers: &Pod, sockets: &Sockets) -> Result<()> {
    // Written before b33p links: it reaches b33p in the message history, through CDN3.
    let note = "testbox android: a note from before b33p linked";
    let AndroidBox {
        peer,
        mut android,
        mut b33p,
        mut desktop,
        android_aci,
    } = android_box(checks, containers, sockets, Some(note))?;
    checks.record("Android's note reaches b33p in the history", || {
        select(&mut b33p, &format!("c:{android_aci}"))?;
        b33p_shows(&mut b33p, note, |message| message.text == note)
    });

    let peer_text = "testbox android: from the peer";
    let conversation = checks.require("text from the peer reaches b33p", || {
        peer_text_reaches(&mut b33p, &peer, &android_aci, peer_text)
    })?;
    // A first message from someone Android doesn't know is a message request: its row in the chat
    // list reads "Message Request", and the text shows once it is open.
    checks.record("text from the peer reaches Android", || {
        android.call(drivers::Android::OpenConversation {
            text: "Message Request".to_owned(),
        })?;
        android_shows(&mut android, peer_text)
    });
    checks.require(REQUEST_ACCEPTED, || {
        accept_request(&mut b33p, &conversation)
    })?;

    // Every Desktop check from here on looks in this conversation, which Desktop has from the
    // peer's own message, as a sync; opened once, so that one check failing doesn't leave the
    // next ones looking at nothing.
    desktop.call(drivers::Desktop::OpenConversation {
        aci: android_aci.clone(),
    })?;

    let b33p_text = "testbox android: from b33p";
    checks.record(
        "text from b33p reaches the peer and Desktop, and Android as a sync",
        || {
            select(&mut b33p, &conversation)?;
            type_and_send(&mut b33p, b33p_text)?;
            peer.sync_printing(b33p_text)?;
            android_shows(&mut android, b33p_text)?;
            desktop_shows(&mut desktop, b33p_text)
        },
    );
    // As in `desktop`: Desktop, linked to the peer, sends a delivery receipt for b33p's text.
    checks.record("Desktop's delivery receipt reaches b33p", || {
        delivered(&mut b33p, b33p_text)
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
            b33p_shows(&mut b33p, android_text, |message| {
                message.text == android_text && message.outgoing
            })?;
            desktop_shows(&mut desktop, android_text)
        },
    );

    let desktop_text = "testbox android: from Desktop";
    checks.record("text from Desktop reaches Android and b33p", || {
        desktop.call(drivers::Desktop::Send {
            text: desktop_text.to_owned(),
        })?;
        android_shows(&mut android, desktop_text)?;
        b33p_shows(&mut b33p, desktop_text, |message| {
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
