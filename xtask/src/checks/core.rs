use b33p_harness::{Action, Delivery};

use super::b33p::{
    REQUEST_ACCEPTED, accept_request, outgoing_index, peer_text_reaches, select, type_and_send,
};
use super::setup::{LinkedBox, linked_box};
use super::{Checks, PATIENCE};
use crate::Sockets;
use crate::error::Result;
use crate::pod::{Pod, Ready, Service};
use crate::process;

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
    checks.record(
        "a message sent while the server is down fails, and goes out when sent again",
        || {
            let text = "testbox core: while the server is down";
            containers.stop(Service::Server)?;
            let failed = (|| {
                b33p.act(Action::Type {
                    text: text.to_owned(),
                })?;
                b33p.act(Action::Send)?;
                b33p.wait_for("b33p's message marked failed", PATIENCE, |snapshot| {
                    snapshot
                        .messages
                        .iter()
                        .find(|message| message.outgoing && message.text == text)
                        .filter(|message| message.delivery == Some(Delivery::Failed))
                        .map(drop)
                })
            })();
            let since = process::now()?;
            containers.start(Service::Server)?;
            containers.await_ready(Ready::Server, Some(since), PATIENCE)?;
            failed?;
            let index = outgoing_index(&mut b33p, text)?;
            b33p.act(Action::RetrySend { index })?;
            peer.sync_printing(text)
        },
    );

    Ok(())
}
