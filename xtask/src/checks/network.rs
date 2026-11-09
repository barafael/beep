//! The clients' link to the servers impaired (`tc netem` on the pod's loopback, for the
//! servers' ports: xtask/src/pod.rs `impair`), between b33p and Signal-Desktop: slow and lossy,
//! narrow, dead for a while with nothing to say so, and flapping. TESTPLAN.md §1.2 items 3 and
//! 4, §4b items 9, 13 and 17. What each took is printed: the link's cost, against the phases
//! before.

use std::path::Path;
use std::time::{Duration, Instant};

use b33p_harness::{Action, AttachmentKind, Delivery};

use super::attachments::{incoming_attachment, saved, stage_picture};
use super::b33p::{b33p_shows, delivered, outgoing_index, select, send_text, type_and_send};
use super::clients::desktop_shows;
use super::setup::LinkedBox;
use super::{Checks, Failure, PATIENCE, PATIENCE_MS};
use crate::harness::{Harness, HarnessError};
use crate::pod::{Pod, PodError};
use testbox_tools::drivers;

/// A slow, lossy link: a mobile network at its worst, packets reordered and doubled too.
const LOSSY: &str = "delay 150ms 40ms distribution normal loss 1% duplicate 0.5% reorder 5% 50%";
/// A narrow link.
const NARROW: &str = "rate 1mbit delay 40ms";
/// No link at all, and nothing to say so: a cable pulled, Wi-Fi out of range.
const DEAD: &str = "loss 100%";
/// How long the link stays dead.
const DEAD_FOR: Duration = Duration::from_secs(90);
/// How long the link flaps for, dead and back by turns of [`FLAP`].
const FLAPPING_FOR: Duration = Duration::from_secs(160);
const FLAP: Duration = Duration::from_secs(20);
/// How long after the link is back everything must be through: the keepalive that finds the
/// connection dead (libsignal-service, 30 s apart, 55 s unanswered), then the reconnect's
/// backoff (b33p-core receive.rs, up to a minute), then the send again.
const RECOVERY: Duration = Duration::from_secs(4 * 60);
/// The file sent over the narrow link: 1 MiB, eight seconds at the link's rate.
const FILE_SIZE: usize = 1 << 20;

/// What the network checks find wrong of their own: texts sent over the flapping link that
/// b33p shows wrong, or a thread that panicked.
#[derive(Debug, thiserror::Error)]
enum NetworkError {
    /// Fewer of the texts than were sent showed in time; with those that did, as of the last
    /// screen.
    #[error("{} of {sent} shown in {open:?}: {shown:?}", shown.len())]
    NotAllShown {
        shown: Vec<String>,
        sent: usize,
        /// The conversation open.
        open: Option<String>,
        #[source]
        source: HarnessError,
    },
    /// Some text showed more than once.
    #[error("{} shown for {sent} sent: {shown:?}", shown.len())]
    Duplicated { shown: Vec<String>, sent: usize },
    #[error("out of order: {shown:?}")]
    OutOfOrder { shown: Vec<String> },
    #[error("{thread} panicked")]
    Panicked { thread: &'static str },
}

pub(super) fn network(
    checks: &mut Checks,
    linked: &mut LinkedBox<'_>,
    containers: &Pod,
    files: &Path,
    picture: &Path,
) -> Result<(), Failure> {
    let outcome = impaired(checks, linked, containers, files, picture);
    // Whatever happened, the link is whole again for the phases after.
    checks.require("the link is whole again", || Ok(containers.unimpair()?))?;
    outcome
}

fn impaired(
    checks: &mut Checks,
    linked: &mut LinkedBox<'_>,
    containers: &Pod,
    files: &Path,
    picture: &Path,
) -> Result<(), Failure> {
    let LinkedBox {
        primary_aci,
        b33p,
        desktop,
        conversation,
        ..
    } = linked;
    stage_picture(picture, files)?;
    let in_box = |name: &str| format!("/files/{name}");

    checks.require("the link is made slow and lossy", || {
        Ok(containers.impair(LOSSY)?)
    })?;
    let b33p_text = "testbox network: from b33p over a slow, lossy link";
    let desktop_text = "testbox network: from Desktop over a slow, lossy link";
    checks.record("text both ways over the slow, lossy link", || {
        select(b33p, conversation)?;
        let started = Instant::now();
        type_and_send(b33p, b33p_text)?;
        desktop.call(drivers::Desktop::OpenConversation {
            aci: primary_aci.clone(),
        })?;
        desktop_shows(desktop, b33p_text)?;
        let to_desktop = started.elapsed();
        let started = Instant::now();
        desktop.call(drivers::Desktop::Send {
            text: desktop_text.to_owned(),
        })?;
        b33p_shows(b33p, "Desktop's text", |message| {
            !message.outgoing && message.text == desktop_text
        })?;
        println!(
            "  b33p to Desktop in {:.1} s, Desktop to b33p in {:.1} s",
            to_desktop.as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
        Ok(())
    });
    checks.record("Desktop's delivery receipt comes back over it", || {
        Ok(delivered(b33p, b33p_text)?)
    });
    checks.record("a picture from b33p reaches Desktop over it", || {
        let text = "testbox network: a picture over a slow, lossy link";
        let started = Instant::now();
        b33p.act(Action::StageFiles {
            paths: vec![in_box("picture.png")],
        })?;
        type_and_send(b33p, text)?;
        desktop.call(drivers::Desktop::WaitForImage {
            text: text.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?;
        println!("  the picture in {:.1} s", started.elapsed().as_secs_f64());
        Ok(())
    });

    checks.require("the link is made narrow", || Ok(containers.impair(NARROW)?))?;
    checks.record(
        "a 1 MiB file from Desktop arrives over a 1 Mbit/s link, downloaded",
        || {
            testbox_tools::write(
                &files.join("narrow.bin"),
                super::rng::incompressible(FILE_SIZE),
            )?;
            let text = "testbox network: a file over a narrow link";
            let started = Instant::now();
            desktop.call(drivers::Desktop::SendFiles {
                paths: vec![in_box("narrow.bin")],
                text: text.to_owned(),
            })?;
            let id = incoming_attachment(b33p, Some(text), AttachmentKind::File)?;
            let arrived = started.elapsed();
            // A file is fetched when it is saved (`attachments`), not on its own.
            b33p.act(Action::SaveAttachment { id: id.clone() })?;
            saved(b33p, &id)?;
            println!(
                "  uploaded and announced in {:.1} s, downloaded and saved {:.1} s later",
                arrived.as_secs_f64(),
                (started.elapsed() - arrived).as_secs_f64()
            );
            Ok(())
        },
    );

    // TESTPLAN.md §1.2 item 4 and §5 item 10, with the link itself gone rather than the server
    // paused (`core`): the connections stay open and nothing answers, and when the link is back
    // the old connections are dead. Both sides write meanwhile.
    checks.require("the link goes dead", || Ok(containers.impair(DEAD)?))?;
    let dead_desktop = "testbox network: from Desktop while the link was dead";
    let dead_b33p = "testbox network: from b33p while the link was dead";
    let written = checks.report("both write while the link is dead", || {
        desktop.call(drivers::Desktop::Send {
            text: dead_desktop.to_owned(),
        })?;
        // Queued on Desktop, at least: its own timeline shows it at once.
        desktop_shows(desktop, dead_desktop)?;
        send_text(b33p, dead_b33p)?;
        std::thread::sleep(DEAD_FOR);
        Ok(())
    });
    checks.require("the link is back", || Ok(containers.unimpair()?))?;
    if written.is_some() {
        let back = Instant::now();
        checks.record(
            "what Desktop wrote while the link was dead reaches b33p once it is back",
            || {
                b33p.wait_for("Desktop's text from the dead spell", RECOVERY, |snapshot| {
                    snapshot
                        .messages
                        .iter()
                        .any(|message| !message.outgoing && message.text == dead_desktop)
                        .then_some(())
                })?;
                println!(
                    "  {:.1} s after the link came back",
                    back.elapsed().as_secs_f64()
                );
                Ok(())
            },
        );
        checks.record(
            "what b33p wrote while the link was dead reaches Desktop, sent again if it failed",
            || {
                // libsignal-service gives the dead connection up at its keepalive, presage sends
                // once more on a new one: through, or failed and sent again by hand (`core`).
                let index = outgoing_index(b33p, dead_b33p)?;
                let delivery = b33p.wait_for("b33p's text settled", RECOVERY, |snapshot| {
                    snapshot
                        .messages
                        .iter()
                        .find(|message| message.index == index)
                        .and_then(|message| message.delivery)
                        .filter(|delivery| *delivery != Delivery::Sending)
                })?;
                if delivery == Delivery::Failed {
                    println!("  failed; sent again by hand");
                    b33p.wait_for("b33p caught up", PATIENCE, |snapshot| {
                        snapshot.synced.then_some(())
                    })?;
                    b33p.act(Action::RetrySend { index })?;
                }
                desktop.call(drivers::Desktop::WaitForMessage {
                    text: dead_b33p.to_owned(),
                    timeout_ms: RECOVERY.as_millis() as u64,
                })?;
                println!(
                    "  on Desktop {:.1} s after the link came back",
                    back.elapsed().as_secs_f64()
                );
                Ok(())
            },
        );
    }

    // The link dead and back by turns while Desktop writes: Desktop sends in order and again
    // until each is through (its conversation job queue), and b33p's connection goes when a
    // keepalive meets a dead turn. Everything, in order, once.
    let flapped: Vec<String> = (1..=10)
        .map(|n| format!("testbox network: {n} of 10 over a flapping link"))
        .collect();
    checks.record(
        "ten texts from Desktop over a flapping link reach b33p in order, once",
        || {
            let started = Instant::now();
            let sent = std::thread::scope(|scope| -> Result<(), Failure> {
                let flapping = scope.spawn(|| flap(containers));
                let sent = (|| -> Result<(), HarnessError> {
                    for text in &flapped {
                        desktop.call(drivers::Desktop::Send { text: text.clone() })?;
                        desktop_shows(desktop, text)?;
                        std::thread::sleep(FLAPPING_FOR / 12);
                    }
                    Ok(())
                })();
                let flapped = flapping.join().map_err(|_| NetworkError::Panicked {
                    thread: "the flapping thread",
                })?;
                sent?;
                Ok(flapped?)
            });
            containers.unimpair()?;
            sent?;
            let back = Instant::now();
            in_order_once(b33p, &flapped, RECOVERY)?;
            println!(
                "  all ten {:.1} s after the last flap, {:.1} s in all",
                back.elapsed().as_secs_f64(),
                started.elapsed().as_secs_f64()
            );
            Ok(())
        },
    );
    Ok(())
}

/// The link dead and back by turns of [`FLAP`], for [`FLAPPING_FOR`]; back at the end.
fn flap(containers: &Pod) -> Result<(), PodError> {
    let until = Instant::now() + FLAPPING_FOR;
    while Instant::now() < until {
        containers.impair(DEAD)?;
        std::thread::sleep(FLAP);
        containers.unimpair()?;
        std::thread::sleep(FLAP);
    }
    Ok(())
}

/// Waits until b33p's open conversation shows each of `texts` from someone else, in their
/// order, and none twice.
fn in_order_once(
    b33p: &mut Harness,
    texts: &[String],
    patience: Duration,
) -> Result<(), NetworkError> {
    let matching = |snapshot: &b33p_harness::Snapshot| -> Vec<String> {
        snapshot
            .messages
            .iter()
            .filter(|message| !message.outgoing && !message.notice)
            .map(|message| message.text.as_str())
            .filter(|text| texts.iter().any(|wanted| wanted == text))
            .map(str::to_owned)
            .collect()
    };
    // Which came, as of the last screen, for the failure.
    let (mut open, mut last) = (None, Vec::new());
    let shown = b33p
        .wait_for("every text", patience, |snapshot| {
            let shown = matching(snapshot);
            if shown.len() >= texts.len() {
                return Some(shown);
            }
            open.clone_from(&snapshot.selected);
            last = shown;
            None
        })
        .map_err(|source| NetworkError::NotAllShown {
            shown: last,
            sent: texts.len(),
            open,
            source,
        })?;
    if shown.len() > texts.len() {
        return Err(NetworkError::Duplicated {
            shown,
            sent: texts.len(),
        });
    }
    if shown != texts {
        return Err(NetworkError::OutOfOrder { shown });
    }
    Ok(())
}
