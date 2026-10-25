use std::time::Duration;

use b33p_harness::{Action, Delivery};

use super::b33p::{
    REQUEST_ACCEPTED, accept_request, outgoing_index, peer_text_reaches, select, type_and_send,
};
use super::setup::{LinkedBox, linked_box};
use super::{Checks, PATIENCE};
use crate::Sockets;
use crate::error::{Error, Result};
use crate::pod::{Pod, Ready, Service};
use crate::process;

/// How long a send to a server that doesn't answer takes to fail: until libsignal-service's
/// keepalive gives the connection up (55 s unanswered, found at its next tick), then presage's
/// one retry on a new connection.
const SILENT_SERVER: Duration = Duration::from_secs(5 * 60);

/// b33p linked to a presage-cli primary, with a presage-cli peer.
pub(crate) fn core(checks: &mut Checks, containers: &Pod, sockets: &Sockets) -> Result<()> {
    let LinkedBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = linked_box(checks, containers, sockets)?;

    let conversation = checks.require("text from the peer reaches b33p", || {
        peer_text_reaches(
            &mut b33p,
            &peer,
            &primary_aci,
            "testbox core: from the peer",
        )
    })?;
    checks.require(REQUEST_ACCEPTED, || {
        accept_request(&mut b33p, &conversation)
    })?;

    let reply_text = "testbox core: from b33p to the peer";
    checks.record("text from b33p reaches the peer", || {
        select(&mut b33p, &conversation)?;
        type_and_send(&mut b33p, reply_text)?;
        peer.sync_printing(reply_text)
    });

    // presage-cli sends no receipts (presage-cli/src/main.rs only prints a `ReceiptMessage`), so
    // b33p's "delivered" is checked in `desktop`. The other way round: Signal-Desktop, a linked
    // device too, sends a delivery receipt for each message it receives
    // (ts/messages/handleDataMessage.preload.ts `deliveryReceiptQueue`).
    checks.record("b33p sends a delivery receipt", || {
        peer_text_reaches(
            &mut b33p,
            &peer,
            &primary_aci,
            "testbox core: to be receipted",
        )?;
        peer.sync_printing("got Delivery receipt")
    });

    checks.record("b33p reconnects after the server side drops", || {
        // The server drops every websocket as it goes down; b33p's must come back on its own.
        let since = process::now()?;
        containers.restart(Service::Server)?;
        containers.await_ready(Ready::Server, Some(since), PATIENCE)?;
        peer_text_reaches(
            &mut b33p,
            &peer,
            &primary_aci,
            "testbox core: after the drop",
        )
        .map(drop)
    });

    // TESTPLAN.md §1.2 item 3: a message that can't go out says so, and goes out when sent again.
    // With the server paused, not stopped: a stopped one closes b33p's connection, and b33p lets
    // nobody write until it has caught up again (b33p-core `can_write_to`). A paused one keeps
    // the connection and answers nothing, as a network that goes away without a word does.
    checks.record(
        "a message sent while the server doesn't answer fails, and goes out when sent again",
        || {
            let text = "testbox core: while the server doesn't answer";
            containers.pause(Service::Server)?;
            let failed = (|| {
                b33p.act(Action::Type {
                    text: text.to_owned(),
                })?;
                if !b33p.act(Action::Send)? {
                    return Err(Error::Failed("b33p didn't send it".to_owned()));
                }
                b33p.wait_for("b33p's message marked failed", SILENT_SERVER, |snapshot| {
                    snapshot
                        .messages
                        .iter()
                        .find(|message| message.outgoing && message.text == text)
                        .filter(|message| message.delivery == Some(Delivery::Failed))
                        .map(drop)
                })
            })();
            containers.unpause(Service::Server)?;
            failed?;
            // Once b33p has caught up again, which it lets nobody write before.
            let index = outgoing_index(&mut b33p, text)?;
            b33p.wait_for("b33p caught up", PATIENCE, |snapshot| {
                snapshot.synced.then_some(())
            })?;
            b33p.act(Action::RetrySend { index })?;
            peer.sync_printing(text)
        },
    );

    Ok(())
}
