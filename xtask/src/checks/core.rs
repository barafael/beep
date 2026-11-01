use std::time::Duration;

use b33p_harness::{Action, Delivery};

use super::b33p::{
    chat_previewing, outgoing_index, peer_text_reaches, select, send_text, type_and_send,
};
use super::setup::{LinkedBox, started_b33p};
use super::{Checks, Failure, PATIENCE};
use crate::Sockets;
use crate::pod::{B33P, Pod, Ready, Service};
use crate::process;

/// How long a send to a server that doesn't answer takes to fail: until libsignal-service's
/// keepalive gives the connection up (55 s unanswered, found at its next tick), then presage's
/// one retry on a new connection.
const SILENT_SERVER: Duration = Duration::from_secs(5 * 60);

/// What the core checks find b33p showing that it shouldn't.
#[derive(Debug, thiserror::Error)]
enum CoreError {
    /// A send that failed shows in b33p's banner as the library's error, not in words.
    #[error("the banner reads {banner:?}, the library's error")]
    BannerUnworded { banner: String },
}

/// Text, reconnecting, and a send to a server that doesn't answer, between b33p and the
/// presage-cli peer.
pub(super) fn core(
    checks: &mut Checks,
    linked: &mut LinkedBox<'_>,
    containers: &Pod,
    sockets: &Sockets,
) -> Result<(), Failure> {
    let LinkedBox {
        peer,
        primary_aci,
        b33p,
        conversation,
        ..
    } = linked;

    let reply_text = "testbox core: from b33p to the peer";
    checks.record("text from b33p reaches the peer", || {
        select(b33p, conversation)?;
        type_and_send(b33p, reply_text)?;
        Ok(peer.sync_printing(reply_text)?)
    });

    // presage-cli sends no receipts (presage-cli/src/main.rs only prints a `ReceiptMessage`), so
    // b33p's "delivered" is checked in `desktop`. The other way round: Signal-Desktop, a linked
    // device too, sends a delivery receipt for each message it receives
    // (ts/messages/handleDataMessage.preload.ts `deliveryReceiptQueue`).
    checks.record("b33p sends a delivery receipt", || {
        peer_text_reaches(b33p, peer, primary_aci, "testbox core: to be receipted")?;
        Ok(peer.sync_printing("got Delivery receipt")?)
    });

    // TESTPLAN.md §1.2 item 5, the second half: written to while b33p is closed. What came
    // while it was down waits on the server, and b33p fetches it when it connects again.
    checks.record(
        "text the peer sends while b33p is down arrives when it starts again",
        || {
            let text = "testbox core: sent while b33p was down";
            let since = process::now()?;
            containers.kill(B33P)?;
            peer.send(primary_aci, text)?;
            // The pod may have started it again already (`restartPolicy: Always`).
            containers.start(B33P)?;
            *b33p = started_b33p(containers, &sockets.b33p, since)?;
            chat_previewing(b33p, text)?;
            Ok(select(b33p, conversation)?)
        },
    );

    checks.record("b33p reconnects after the server side drops", || {
        // The server drops every websocket as it goes down; b33p's must come back on its own.
        let since = process::now()?;
        containers.restart(Service::Server)?;
        containers.await_ready(Ready::Server, Some(since), PATIENCE)?;
        peer_text_reaches(b33p, peer, primary_aci, "testbox core: after the drop")?;
        Ok(())
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
            let failed = (|| -> Result<(), Failure> {
                send_text(b33p, text)?;
                Ok(
                    b33p.wait_for("b33p's message marked failed", SILENT_SERVER, |snapshot| {
                        snapshot
                            .messages
                            .iter()
                            .find(|message| message.outgoing && message.text == text)
                            .filter(|message| message.delivery == Some(Delivery::Failed))
                            .map(drop)
                    })?,
                )
            })();
            containers.unpause(Service::Server)?;
            failed?;
            // The banner says it in words, not as the library's error (another error, about
            // receiving, may stand in its place by now).
            if let Some(banner) = b33p.snapshot()?.error
                && banner.contains("failed to send")
            {
                return Err(CoreError::BannerUnworded { banner }.into());
            }
            // Once b33p has caught up again, which it lets nobody write before.
            let index = outgoing_index(b33p, text)?;
            b33p.wait_for("b33p caught up", PATIENCE, |snapshot| {
                snapshot.synced.then_some(())
            })?;
            b33p.act(Action::RetrySend { index })?;
            peer.sync_printing(text)?;
            b33p.wait_for("the banner about it gone", PATIENCE, |snapshot| {
                let error = snapshot.error.as_deref().unwrap_or_default();
                (!error.contains("couldn't be sent")).then_some(())
            })?;
            Ok(())
        },
    );

    Ok(())
}
