//! b33p run harder than the other phases run it (pending/stress.md): many messages at once,
//! b33p or the server stopped at awkward moments, texts and files b33p hasn't seen, several
//! clients at once, and a long run of mixed traffic. Each check says what b33p must still show
//! afterwards; what it measures on the way (how long, how much memory) it prints.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use b33p_harness::{Action, AttachmentKind, Delivery, Download, Saved, Snapshot};

use super::attachments::incoming_attachment;
use super::b33p::{
    REQUEST_ACCEPTED, accept_request, b33p_shows, chat_previewing, edit, peer_text_reaches, select,
    type_and_send,
};
use super::clients::desktop_shows;
use super::presage::Presage;
use super::setup::{PlainBox, linked_desktop, plain_box, started_b33p};
use super::{Checks, EnvNumberError, Failure, HISTORY_PATIENCE, PATIENCE, env_number, rng};
use crate::Sockets;
use crate::harness::{Harness, HarnessError};
use crate::pod::{B33P, Pod, Service};
use crate::process;
use testbox_tools::drivers;

/// Messages in a burst.
const BURST: usize = 200;
/// How long the server stays paused: past libsignal-service's keepalive, which gives up on a
/// connection unanswered for 55 s.
const HANG: Duration = Duration::from_secs(3 * 60);
/// The file b33p is stopped while downloading: close to the box's 100 MiB attachment cap
/// (testbox/tools `provision`), so the download takes long enough to stop it in.
const LARGE_FILE: usize = 90 * 1024 * 1024;
/// How often [`all_texts`] asks for older messages without getting any before it takes the
/// conversation as loaded.
const QUIET_LOADS: u32 = 4;
/// A side of the pictures b33p decodes in full: under its 12000 px limit, and at 4 bytes a
/// pixel under its 512 MiB one (b33p-core `decode_limited`), so each is 484 MB decoded.
const HUGE_SIDE: u32 = 11_000;
/// How many of them come in one message.
const HUGE_PICTURES: usize = 3;
/// What b33p may take at once while it shows them.
const MEMORY_BUDGET: u64 = 1 << 30;
/// The longest file name Linux takes is 255 bytes; saved again, " (1)" makes this one too long.
const LONG_NAME: usize = 254;

/// What the stress phases found b33p, or the peer, doing other than they should.
#[derive(Debug, thiserror::Error)]
pub(super) enum StressError {
    /// The thread sending `sender`'s messages panicked rather than return.
    #[error("{sender}'s sending panicked")]
    Panicked { sender: &'static str },
    /// The messages of ours that `found`, a burst numbered by [`numbered`], should show once
    /// each and in order.
    #[error(
        "b33p shows {found} of {expected}; missing {missing:?}; more than once {repeated:?}; \
         first out of order {out_of_order:?}"
    )]
    NotInOrderOnce {
        found: usize,
        expected: usize,
        missing: Vec<String>,
        repeated: Vec<String>,
        out_of_order: Option<(String, String)>,
    },
    /// What b33p was sending when it was killed, neither reached the peer and shown so nor shown
    /// as failed.
    #[error("{}", lines(sends))]
    Unaccounted { sends: Vec<Unaccounted> },
    /// A message of ours b33p showed as failed is no longer in the chat to send again.
    #[error("{text:?} is gone from the chat")]
    Gone { text: String },
    /// b33p answered [`Action::RetrySend`] for it with no.
    #[error("b33p didn't send {text:?} again")]
    NotRetried { text: String },
    /// Messages sent again that reached the peer other than once, with how often each did.
    #[error("sent again, these didn't reach the peer once: {received:?}")]
    RetriedNotOnce { received: Vec<(String, usize)> },
    /// b33p answered [`Action::SaveAttachment`] for it with no.
    #[error("b33p didn't start saving attachment {id}{}", if *again { " again" } else { "" })]
    NotSaving { id: String, again: bool },
    /// b33p's attachment row says saving failed, as `error`.
    #[error("b33p failed to save attachment {id}: {error}")]
    SaveFailed { id: String, error: String },
    /// Messages of b33p's that the peer's presage-cli didn't receive as sent: `missing` of the
    /// `sent`.
    #[error("the peer didn't get {} of b33p's {sent} as sent: {missing:?}", missing.len())]
    PeerMissing { missing: Vec<String>, sent: usize },
    /// The [`awkward_texts`] b33p doesn't show as they were sent.
    #[error("{}", lines(texts))]
    Mangled { texts: Vec<Mangled> },
    /// b33p's memory peak while it showed `pictures` bytes of pictures.
    #[error(
        "b33p took {} MiB at once, over {} MiB, for {} KiB of pictures",
        peak >> 20,
        budget >> 20,
        pictures >> 10
    )]
    OverBudget {
        peak: u64,
        budget: u64,
        pictures: usize,
    },
    /// The files with [`odd_names`] that weren't saved, or not where they should be.
    #[error("{}", lines(saves))]
    OddSaves { saves: Vec<OddSave> },
    /// The message deleted for everyone shows as not deleted after all.
    #[error("the message no longer shows as deleted")]
    NotDeleted,
    /// The message shows as deleted, yet texts it had show still.
    #[error("deleted, yet these show: {shown:?}")]
    DeletedShows { shown: Vec<String> },
    /// What [`soak`] samples, grown over the run.
    #[error("{}", growth.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Growing { growth: Vec<Growth> },
    /// A snapshot took longer than a second: b33p's window too busy to answer.
    #[error("a snapshot took {took:?}")]
    Unresponsive { took: Duration },
    /// The open conversation's messages didn't stay the same for a while within `patience`.
    #[error("the conversation kept changing for {patience:?}")]
    KeptChanging { patience: Duration },
    /// `service`'s log has Rust panics: their lines.
    #[error("{service} panicked:\n{}", panics.join("\n"))]
    Panics {
        service: String,
        panics: Vec<String>,
    },
    /// The number after `key` in `what`, read from b33p's container, isn't there.
    #[error("no {} in {what}", key.trim_end())]
    NoField {
        key: &'static str,
        what: &'static str,
    },
    #[error("encoding a picture of {side} px a side")]
    EncodePicture {
        side: u32,
        #[source]
        source: image::ImageError,
    },
    /// A burst of no texts, whose last b33p can't be waited for.
    #[error("no texts to wait for")]
    NoTexts,
}

/// Each of `items`, a line each.
fn lines<T: std::fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A message b33p was sending when it was killed: whether it `reached` the peer, and how b33p
/// shows it, if at all.
#[derive(Debug)]
pub(super) struct Unaccounted {
    text: String,
    reached: bool,
    shown: Option<Delivery>,
}

impl std::fmt::Display for Unaccounted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            text,
            reached,
            shown,
        } = self;
        match (reached, shown) {
            (true, None) => write!(f, "{text:?} reached the peer, b33p lost it"),
            (false, None) => write!(
                f,
                "{text:?} didn't reach the peer, and b33p doesn't show it at all"
            ),
            (true, Some(delivery)) => {
                write!(f, "{text:?} reached the peer, b33p shows it {delivery:?}")
            }
            (false, Some(delivery)) => write!(
                f,
                "{text:?} didn't reach the peer, b33p shows it {delivery:?}"
            ),
        }
    }
}

/// One of [`awkward_texts`], named `what`, that b33p doesn't show as sent: as a text of
/// `shown` characters, if it shows something close, instead of `sent`.
#[derive(Debug)]
pub(super) struct Mangled {
    what: &'static str,
    shown: Option<usize>,
    sent: usize,
}

impl std::fmt::Display for Mangled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { what, shown, sent } = self;
        match shown {
            Some(shown) => write!(f, "{what}: shown as {shown} chars instead of {sent}"),
            None => write!(f, "{what}: not shown"),
        }
    }
}

/// How a file with one of [`odd_names`] wasn't saved as it should be.
#[derive(Debug)]
pub(super) enum OddSave {
    /// Saved, but at `path`: outside the downloads folder.
    Outside { name: String, path: String },
    /// Not saved the `time` time (first or second), b33p saying `error`.
    Failed {
        name: String,
        time: &'static str,
        error: String,
    },
}

impl std::fmt::Display for OddSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outside { name, path } => write!(f, "{name:?} saved at {path:?}"),
            Self::Failed { name, time, error } => {
                write!(f, "{name:?} not saved the {time} time: {error}")
            }
        }
    }
}

/// What [`growing`] found grown between the first and the last third of the samples.
#[derive(Debug)]
pub(super) enum Growth {
    /// b33p's memory, in bytes.
    Memory {
        from: u64,
        to: u64,
    },
    OpenFiles {
        from: u64,
        to: u64,
    },
    /// A snapshot in the last third took `took`, over a second.
    Slow {
        took: Duration,
    },
}

impl std::fmt::Display for Growth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory { from, to } => {
                write!(f, "memory grew from {} to {} MiB", from >> 20, to >> 20)
            }
            Self::OpenFiles { from, to } => write!(f, "open files grew from {from} to {to}"),
            Self::Slow { took } => write!(f, "a snapshot took {took:?}"),
        }
    }
}

/// Bursts of messages from the peer, with b33p running and with it stopped.
pub(crate) fn volume(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
) -> Result<(), Failure> {
    let PlainBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = plain_box(checks, containers, sockets)?;
    let conversation = accepted(checks, &mut b33p, &peer, &primary_aci, "volume")?;

    checks.record(
        &format!("{BURST} messages in a burst reach b33p, in order, once each"),
        || {
            let texts = numbered("testbox stress: burst", BURST);
            memory(containers, "before the burst");
            let started = Instant::now();
            peer.send_all(&primary_aci, &texts)?;
            println!("  the peer sent {BURST} in {:?}", started.elapsed());
            let sent = Instant::now();
            chat_previewing(&mut b33p, last(&texts)?)?;
            println!("  b33p showed the last {:?} after", sent.elapsed());
            memory(containers, "after the burst");
            responsive(&mut b33p)?;
            select(&mut b33p, &conversation)?;
            Ok(in_order_once(
                &all_texts(&mut b33p, "testbox stress: burst ")?,
                &texts,
            )?)
        },
    );

    checks.record(
        &format!("{BURST} messages sent while b33p is stopped reach it once it starts"),
        || {
            let texts = numbered("testbox stress: while stopped", BURST);
            containers.stop(B33P)?;
            peer.send_all(&primary_aci, &texts)?;
            let since = process::now()?;
            containers.start(B33P)?;
            b33p = started_b33p(containers, &sockets.b33p, since)?;
            let started = Instant::now();
            chat_previewing(&mut b33p, last(&texts)?)?;
            println!("  b33p caught up in {:?}", started.elapsed());
            memory(containers, "after catching up");
            responsive(&mut b33p)?;
            select(&mut b33p, &conversation)?;
            Ok(in_order_once(
                &all_texts(&mut b33p, "testbox stress: while stopped ")?,
                &texts,
            )?)
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers, B33P));
    Ok(())
}

/// b33p killed while it receives, sends and downloads, and the server hanging.
pub(crate) fn interrupt(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<(), Failure> {
    let PlainBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = plain_box(checks, containers, sockets)?;
    let conversation = accepted(checks, &mut b33p, &peer, &primary_aci, "interrupt")?;

    checks.record("b33p killed while a burst arrives loses none of it", || {
        let prefix = "testbox stress: killed while receiving";
        let texts = numbered(prefix, BURST);
        // Three times: each kill loses what was acknowledged and not yet stored only if it
        // comes at that moment.
        let kills = [BURST / 5, BURST / 2, BURST * 4 / 5];
        thread::scope(|scope| -> Result<(), Failure> {
            let sending = scope.spawn(|| peer.send_all(&primary_aci, &texts));
            let killed = kills
                .iter()
                .try_for_each(|&killed_at| -> Result<(), Failure> {
                    let shown = b33p.wait_for(
                        &format!("message {killed_at} in b33p's chat list"),
                        PATIENCE,
                        |snapshot| {
                            preview_number(snapshot, &conversation, prefix)
                                .filter(|shown| *shown >= killed_at)
                        },
                    )?;
                    println!("  killed b33p at message {shown}");
                    b33p = kill_and_start(containers, sockets)?;
                    Ok(())
                });
            let sent = sending.join();
            killed?;
            sent.map_err(|_| StressError::Panicked { sender: "the peer" })??;
            Ok(())
        })?;
        chat_previewing(&mut b33p, last(&texts)?)?;
        select(&mut b33p, &conversation)?;
        Ok(in_order_once(
            &all_texts(&mut b33p, &format!("{prefix} "))?,
            &texts,
        )?)
    });

    checks.record(
        "what b33p sent just before it was killed reached the peer, or shows as failed and goes when sent again",
        || {
            let prefix = "testbox stress: killed while sending";
            let texts = numbered(prefix, 10);
            select(&mut b33p, &conversation)?;
            for text in &texts {
                b33p.act(Action::Type { text: text.clone() })?;
                b33p.act(Action::Send)?;
            }
            // Killed once the first is out, so some are sent and some still on their way.
            let before = b33p.wait_for("the first of them sent", PATIENCE, |snapshot| {
                let mine = deliveries(snapshot, prefix);
                mine.iter()
                    .any(|(_, delivery)| *delivery != Delivery::Sending)
                    .then_some(mine)
            })?;
            b33p = kill_and_start(containers, sockets)?;
            println!("  before the kill b33p showed {before:?}");
            select(&mut b33p, &conversation)?;
            // Opened, the chat shows what comes first and its stored history after.
            settled(&mut b33p)?;
            let shown = b33p.wait_for("none of them still sending", PATIENCE, |snapshot| {
                let mine = deliveries(snapshot, prefix);
                mine.iter()
                    .all(|(_, delivery)| *delivery != Delivery::Sending)
                    .then_some(mine)
            })?;
            println!("  after the restart b33p shows {shown:?}");
            let received = peer.sync()?;
            let mut wrong = Vec::new();
            for text in &texts {
                let reached = received.contains(text.as_str());
                let delivery = shown
                    .iter()
                    .find(|(shown, _)| shown == text)
                    .map(|(_, delivery)| *delivery);
                match (reached, delivery) {
                    // Sent and shown so; or not sent, and shown so, as Signal's apps do. Or
                    // sent, and b33p killed before it noted so: sent again, it goes at the same
                    // time, which the peer takes for the same message.
                    (true, Some(Delivery::Sent | Delivery::Delivered | Delivery::Read))
                    | (_, Some(Delivery::Failed)) => {}
                    (reached, shown) => wrong.push(Unaccounted {
                        text: text.clone(),
                        reached,
                        shown,
                    }),
                }
            }
            if !wrong.is_empty() {
                return Err(StressError::Unaccounted { sends: wrong }.into());
            }
            // Those that failed, sent again: they reach the peer, once each.
            let failed: Vec<String> = shown
                .iter()
                .filter(|(_, delivery)| *delivery == Delivery::Failed)
                .map(|(text, _)| text.clone())
                .collect();
            for text in &failed {
                let snapshot = b33p.snapshot()?;
                let index = snapshot
                    .messages
                    .iter()
                    .find(|message| message.outgoing && &message.text == text)
                    .map(|message| message.index)
                    .ok_or_else(|| StressError::Gone { text: text.clone() })?;
                if !b33p.act(Action::RetrySend { index })? {
                    return Err(StressError::NotRetried { text: text.clone() }.into());
                }
            }
            b33p.wait_for("those that failed sent again", PATIENCE, |snapshot| {
                let mine = deliveries(snapshot, prefix);
                mine.iter()
                    .filter(|(text, _)| failed.contains(text))
                    .all(|(_, delivery)| {
                        matches!(
                            delivery,
                            Delivery::Sent | Delivery::Delivered | Delivery::Read
                        )
                    })
                    .then_some(())
            })?;
            let received = peer.sync()?;
            let twice: Vec<(String, usize)> = failed
                .iter()
                .map(|text| (text.clone(), received.matches(text.as_str()).count()))
                .filter(|(_, count)| *count != 1)
                .collect();
            if twice.is_empty() {
                println!("  sent again: {failed:?}");
                Ok(())
            } else {
                Err(StressError::RetriedNotOnce { received: twice }.into())
            }
        },
    );

    checks.record(
        "b33p killed while it saves a large file saves it after",
        || {
            let name = "large.bin";
            testbox_tools::write(&files.join(name), large_file())?;
            let text = "testbox stress: a large file";
            peer.send_file(&primary_aci, text, &format!("/files/{name}"))?;
            select(&mut b33p, &conversation)?;
            let id = incoming_attachment(&mut b33p, Some(text), AttachmentKind::File)?;
            // A file is saved, not fetched: b33p fetches only what it shows inline.
            if !b33p.act(Action::SaveAttachment { id: id.clone() })? {
                return Err(StressError::NotSaving {
                    id: id.clone(),
                    again: false,
                }
                .into());
            }
            b33p.wait_for("the saving under way", PATIENCE, |snapshot| {
                snapshot
                    .messages
                    .iter()
                    .flat_map(|message| &message.attachments)
                    .find(|attachment| attachment.id == id)?
                    .saved
                    .as_ref()
                    .map(drop)
            })?;
            b33p = kill_and_start(containers, sockets)?;
            select(&mut b33p, &conversation)?;
            incoming_attachment(&mut b33p, Some(text), AttachmentKind::File)?;
            if !b33p.act(Action::SaveAttachment { id: id.clone() })? {
                return Err(StressError::NotSaving {
                    id: id.clone(),
                    again: true,
                }
                .into());
            }
            let started = Instant::now();
            // Downloading and decrypting 90 MiB takes minutes in the box's debug build.
            b33p.wait_for(
                "the file saved",
                HISTORY_PATIENCE,
                |snapshot| match &snapshot
                    .messages
                    .iter()
                    .flat_map(|message| &message.attachments)
                    .find(|attachment| attachment.id == id)?
                    .saved
                {
                    Some(Saved::Saved(_)) => Some(Ok(())),
                    Some(Saved::Failed(error)) => Some(Err(StressError::SaveFailed {
                        id: id.clone(),
                        error: error.clone(),
                    })),
                    Some(Saved::Saving) | None => None,
                },
            )??;
            println!("  saved in {:?}", started.elapsed());
            memory(containers, "after saving it");
            Ok(())
        },
    );

    checks.record(
        "b33p receives and sends again after the server hangs past its keepalive",
        || {
            containers.pause(Service::Server)?;
            thread::sleep(HANG);
            containers.unpause(Service::Server)?;
            peer_text_reaches(
                &mut b33p,
                &peer,
                &primary_aci,
                "testbox stress: after the hang",
            )?;
            b33p.wait_for("b33p caught up", PATIENCE, |snapshot| {
                snapshot.synced.then_some(())
            })?;
            select(&mut b33p, &conversation)?;
            let text = "testbox stress: from b33p after the hang";
            type_and_send(&mut b33p, text)?;
            Ok(peer.sync_printing(text)?)
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers, B33P));
    Ok(())
}

/// Texts and files b33p hasn't been sent before: awkward Unicode and sizes, broken, huge and
/// animated pictures, and odd file names.
pub(crate) fn content(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<(), Failure> {
    let PlainBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = plain_box(checks, containers, sockets)?;
    let conversation = accepted(checks, &mut b33p, &peer, &primary_aci, "content")?;

    checks.record("awkward texts from the peer reach b33p as sent", || {
        let texts: Vec<String> = awkward_texts().into_iter().map(|(_, text)| text).collect();
        peer.send_all(&primary_aci, &texts)?;
        chat_previewing(&mut b33p, last(&texts)?)?;
        select(&mut b33p, &conversation)?;
        settled(&mut b33p)?;
        responsive(&mut b33p)?;
        memory(containers, "with them shown");
        let shown: Vec<String> = b33p
            .snapshot()?
            .messages
            .into_iter()
            .filter(|message| !message.outgoing)
            .map(|message| message.text)
            .collect();
        Ok(missing_texts(&shown)?)
    });

    checks.record("awkward texts from b33p reach the peer as sent", || {
        select(&mut b33p, &conversation)?;
        // Whitespace alone isn't a message to send, and what ends one isn't sent, here as in
        // Signal's apps; b33p takes `*bold*` and the like as formatting (the desktop phase).
        let texts: Vec<(&str, String)> = awkward_texts()
            .into_iter()
            .filter(|(what, text)| !text.trim().is_empty() && *what != MARKUP)
            .map(|(what, text)| (what, text.trim_end().to_owned()))
            .collect();
        for (_, text) in &texts {
            type_and_send(&mut b33p, text)?;
        }
        let received = peer.sync()?;
        // presage-cli prints what comes inline; that a long text's attachment makes it whole
        // again is for Signal's apps to say, in the desktop and android phases.
        let missing: Vec<String> = texts
            .iter()
            .filter(|(_, text)| !received.contains(inline_part(text)))
            .map(|(what, _)| (*what).to_owned())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(StressError::PeerMissing {
                missing,
                sent: texts.len(),
            }
            .into())
        }
    });

    let in_box = |name: &str| format!("/files/{name}");
    checks.record(
        "broken pictures show as broken, and b33p carries on",
        || {
            let pictures = broken_pictures();
            for (name, bytes) in &pictures {
                testbox_tools::write(&files.join(name), bytes)?;
            }
            let paths: Vec<String> = pictures.iter().map(|(name, _)| in_box(name)).collect();
            let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
            let text = "testbox stress: broken pictures";
            peer.send_files(&primary_aci, text, &paths)?;
            select(&mut b33p, &conversation)?;
            let shown = fetched(&mut b33p, text, pictures.len())?;
            println!("  b33p shows {shown:?}");
            responsive(&mut b33p)?;
            no_panic(containers, B33P)
        },
    );

    checks.record(
        &format!(
            "{HUGE_PICTURES} pictures of {HUGE_SIDE}×{HUGE_SIDE} px take b33p under {} MiB",
            MEMORY_BUDGET >> 20
        ),
        || {
            let picture = huge_picture()?;
            println!("  each is {} KiB", picture.len() >> 10);
            let names: Vec<String> = (1..=HUGE_PICTURES)
                .map(|n| format!("huge-{n}.png"))
                .collect();
            for name in &names {
                testbox_tools::write(&files.join(name), &picture)?;
            }
            let paths: Vec<String> = names.iter().map(|name| in_box(name)).collect();
            let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
            let before = containers.memory_peak(B33P)?;
            let text = "testbox stress: huge pictures";
            peer.send_files(&primary_aci, text, &paths)?;
            select(&mut b33p, &conversation)?;
            let shown = fetched(&mut b33p, text, HUGE_PICTURES)?;
            println!("  b33p shows {shown:?}");
            let peak = containers.memory_peak(B33P)?;
            println!(
                "  b33p's peak: {} MiB before, {} MiB after",
                before >> 20,
                peak >> 20
            );
            memory(containers, "after showing them");
            responsive(&mut b33p)?;
            if peak > MEMORY_BUDGET {
                return Err(StressError::OverBudget {
                    peak,
                    budget: MEMORY_BUDGET,
                    pictures: picture.len() * HUGE_PICTURES,
                }
                .into());
            }
            Ok(())
        },
    );

    checks.record(
        "files with odd names save, inside the downloads folder",
        || {
            let names = odd_names();
            for name in &names {
                testbox_tools::write(&files.join(name), format!("testbox stress: {name}\n"))?;
            }
            let mut wrong = Vec::new();
            for name in &names {
                let text = format!("testbox stress: a file named {}", name.escape_debug());
                peer.send_file(&primary_aci, &text, &in_box(name))?;
                select(&mut b33p, &conversation)?;
                let id = incoming_attachment(&mut b33p, Some(&text), AttachmentKind::File)?;
                // Twice: the second needs a name of its own.
                for time in ["first", "second"] {
                    match save(&mut b33p, &id)? {
                        Ok(path) if path.starts_with("/home/") && !path.contains("/../") => {
                            println!(
                                "  {} ({time}): {}",
                                name.escape_debug(),
                                path.escape_debug()
                            )
                        }
                        Ok(path) => wrong.push(OddSave::Outside {
                            name: name.clone(),
                            path,
                        }),
                        Err(error) => wrong.push(OddSave::Failed {
                            name: name.clone(),
                            time,
                            error,
                        }),
                    }
                }
            }
            if wrong.is_empty() {
                Ok(())
            } else {
                Err(StressError::OddSaves { saves: wrong }.into())
            }
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers, B33P));
    Ok(())
}

/// Desktop (linked to the peer), presage-cli (the peer's primary) and b33p sending at once; and
/// one message edited, reacted to and deleted close together, with b33p running and stopped.
pub(crate) fn concurrency(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
) -> Result<(), Failure> {
    let PlainBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = plain_box(checks, containers, sockets)?;
    let mut desktop = linked_desktop(checks, &sockets.desktop, &peer)?;
    let conversation = accepted(checks, &mut b33p, &peer, &primary_aci, "concurrency")?;
    checks.require("Desktop opens the chat with b33p", || {
        Ok(desktop.call(drivers::Desktop::OpenConversation {
            aci: primary_aci.clone(),
        })?)
    })?;

    checks.record(
        "presage-cli, Desktop and b33p sending at once: each gets the others' messages in order, once",
        || {
            let from_cli = numbered("testbox stress: from presage-cli", 60);
            let from_desktop = numbered("testbox stress: from Desktop", 15);
            let from_b33p = numbered("testbox stress: from b33p", 15);
            select(&mut b33p, &conversation)?;
            thread::scope(|scope| -> Result<(), Failure> {
                let cli = scope.spawn(|| peer.send_all(&primary_aci, &from_cli));
                let desktop = scope.spawn(|| {
                    from_desktop.iter().try_for_each(|text| {
                        desktop.call(drivers::Desktop::Send { text: text.clone() })
                    })
                });
                let mine = from_b33p.iter().try_for_each(|text| {
                    b33p.act(Action::Type { text: text.clone() })?;
                    b33p.act(Action::Send).map(drop)
                });
                let (cli, desktop) = (cli.join(), desktop.join());
                mine?;
                cli.map_err(|_| StressError::Panicked {
                    sender: "presage-cli",
                })??;
                desktop.map_err(|_| StressError::Panicked { sender: "Desktop" })??;
                Ok(())
            })?;
            for last in [last(&from_cli)?, last(&from_desktop)?] {
                b33p_shows(&mut b33p, &format!("{last:?}"), |message| message.text == last)?;
            }
            b33p.wait_for("b33p's own sent", PATIENCE, |snapshot| {
                deliveries(snapshot, "testbox stress: from b33p")
                    .iter()
                    .all(|(_, delivery)| *delivery != Delivery::Sending)
                    .then_some(())
            })?;
            settled(&mut b33p)?;
            in_order_once(
                &all_texts(&mut b33p, "testbox stress: from presage-cli ")?,
                &from_cli,
            )?;
            in_order_once(
                &all_texts(&mut b33p, "testbox stress: from Desktop ")?,
                &from_desktop,
            )?;
            in_order_once(&all_texts(&mut b33p, "testbox stress: from b33p ")?, &from_b33p)?;
            // On the peer's side, Desktop: presage-cli's sends receive in the background and
            // exit, which can drop a message between decrypting and storing it (finding 1b),
            // and the check every phase ends with names those.
            for text in &from_b33p {
                desktop_shows(&mut desktop, text)?;
            }
            Ok(())
        },
    );

    checks.record(
        "an edit, a reaction and a delete close together leave the message deleted",
        || {
            let text = "testbox stress: raced";
            let edited = "testbox stress: raced, edited";
            desktop.call(drivers::Desktop::Send {
                text: text.to_owned(),
            })?;
            select(&mut b33p, &conversation)?;
            let timestamp = b33p.wait_for("Desktop's message in b33p", PATIENCE, |snapshot| {
                snapshot
                    .messages
                    .iter()
                    .find(|message| message.text == text)
                    .map(|message| message.timestamp)
            })?;
            desktop.call(drivers::Desktop::EditMessage {
                old_text: text.to_owned(),
                text: edited.to_owned(),
            })?;
            desktop.call(drivers::Desktop::ReactToMessage {
                text: edited.to_owned(),
            })?;
            desktop.call(drivers::Desktop::DeleteForEveryone {
                text: edited.to_owned(),
            })?;
            b33p_shows(&mut b33p, "the message deleted", |message| {
                message.timestamp == timestamp && message.deleted
            })?;
            settled(&mut b33p)?;
            still_deleted(&mut b33p, timestamp, &[text, edited])
        },
    );

    checks.record(
        "an edit, a reaction and a delete that wait for b33p leave the message deleted",
        || {
            let text = "testbox stress: raced while stopped";
            let edited = "testbox stress: raced while stopped, edited";
            containers.stop(B33P)?;
            desktop.call(drivers::Desktop::Send {
                text: text.to_owned(),
            })?;
            for n in 1..=3 {
                desktop.call(drivers::Desktop::EditMessage {
                    old_text: if n == 1 {
                        text.to_owned()
                    } else {
                        format!("{edited} {}", n - 1)
                    },
                    text: format!("{edited} {n}"),
                })?;
            }
            let last_edit = format!("{edited} 3");
            desktop.call(drivers::Desktop::ReactToMessage {
                text: last_edit.clone(),
            })?;
            desktop.call(drivers::Desktop::DeleteForEveryone { text: last_edit })?;
            let since = process::now()?;
            containers.start(B33P)?;
            b33p = started_b33p(containers, &sockets.b33p, since)?;
            select(&mut b33p, &conversation)?;
            // The newest of Desktop's messages, deleted: its place shows no text.
            let timestamp =
                b33p.wait_for("a deleted message from Desktop", PATIENCE, |snapshot| {
                    snapshot
                        .messages
                        .iter()
                        .rfind(|message| !message.outgoing)
                        .filter(|message| message.deleted)
                        .map(|message| message.timestamp)
                })?;
            settled(&mut b33p)?;
            let edits: Vec<String> = (1..=3).map(|n| format!("{edited} {n}")).collect();
            let mut gone: Vec<&str> = edits.iter().map(String::as_str).collect();
            gone.push(text);
            still_deleted(&mut b33p, timestamp, &gone)
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers, B33P));
    Ok(())
}

/// Fails unless the message at `timestamp` shows as deleted, and none of `texts` shows.
fn still_deleted(b33p: &mut Harness, timestamp: u64, texts: &[&str]) -> Result<(), Failure> {
    let snapshot = b33p.snapshot()?;
    let shown: Vec<String> = snapshot
        .messages
        .iter()
        .filter(|message| texts.contains(&message.text.as_str()))
        .map(|message| message.text.clone())
        .collect();
    let deleted = snapshot
        .messages
        .iter()
        .any(|message| message.timestamp == timestamp && message.deleted);
    match (deleted, shown.is_empty()) {
        (true, true) => Ok(()),
        (false, _) => Err(StressError::NotDeleted.into()),
        (true, false) => Err(StressError::DeletedShows { shown }.into()),
    }
}

/// The name of [`awkward_texts`]' text of formatting marks.
const MARKUP: &str = "markup characters";

/// Mixed traffic for [`soak_minutes`] minutes, from a seed it prints (`TESTBOX_SEED` repeats a
/// run): the peer's texts and pictures; b33p's texts, reactions, edits and deletes. b33p's
/// memory, open files and answering time, sampled each minute, mustn't keep growing, and every
/// text of the peer's must be there at the end.
pub(crate) fn soak(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<(), Failure> {
    let PlainBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = plain_box(checks, containers, sockets)?;
    let conversation = accepted(checks, &mut b33p, &peer, &primary_aci, "soak")?;

    checks.record(
        "mixed traffic for a long while: nothing lost, and nothing keeps growing",
        || {
            let seed = rng::seed()?;
            let minutes = soak_minutes()?;
            println!("  seed {seed}, {minutes} minutes (TESTBOX_SEED, TESTBOX_SOAK_MINUTES)");
            let mut random = rng::Xorshift(seed);
            let picture = small_picture();
            testbox_tools::write(&files.join("soak.png"), &picture)?;
            let ends = Instant::now() + Duration::from_secs(60 * minutes);
            let mut next_sample = Instant::now();
            let mut samples = Vec::new();
            let mut from_peer = Vec::new();
            let mut mine = 0;
            select(&mut b33p, &conversation)?;
            while Instant::now() < ends {
                if Instant::now() >= next_sample {
                    let sample = sample(containers, B33P, &mut b33p)?;
                    println!("  {sample}");
                    samples.push(sample);
                    next_sample += Duration::from_secs(60);
                }
                match random.below(100) {
                    0..40 => {
                        let count = 1 + random.below(5) as usize;
                        let texts: Vec<String> = (0..count)
                            .map(|n| format!("testbox soak: peer {:05}", from_peer.len() + n + 1))
                            .collect();
                        peer.send_all(&primary_aci, &texts)?;
                        from_peer.extend(texts);
                    }
                    40..70 => {
                        mine += 1;
                        type_and_send(&mut b33p, &format!("testbox soak: b33p {mine:05}"))?;
                    }
                    70..80 => {
                        if let Some(index) = newest(&b33p.snapshot()?, false) {
                            b33p.act(Action::React {
                                index,
                                emoji: "👍".to_owned(),
                            })?;
                        }
                    }
                    80..88 => {
                        if let Some(index) = newest(&b33p.snapshot()?, true) {
                            edit(
                                &mut b33p,
                                index,
                                &format!("testbox soak: b33p {mine:05}, edited"),
                            )?;
                        }
                    }
                    88..93 => {
                        if let Some(index) = newest(&b33p.snapshot()?, true) {
                            b33p.act(Action::Delete {
                                index,
                                everyone: true,
                            })?;
                        }
                    }
                    _ => {
                        let text = format!("testbox soak: a picture {}", random.below(1_000_000));
                        peer.send_file(&primary_aci, &text, "/files/soak.png")?;
                    }
                }
                thread::sleep(Duration::from_millis(200 + random.below(1800)));
            }
            if let Some(last) = from_peer.last() {
                b33p_shows(&mut b33p, &format!("{last:?}"), |message| {
                    &message.text == last
                })?;
            }
            settled(&mut b33p)?;
            samples.push(sample(containers, B33P, &mut b33p)?);
            println!(
                "  at the end: {}",
                samples.last().map(ToString::to_string).unwrap_or_default()
            );
            in_order_once(&all_texts(&mut b33p, "testbox soak: peer ")?, &from_peer)?;
            // b33p's own, as it still shows them (edited, the deleted gone), on the peer.
            let mine = all_texts(&mut b33p, "testbox soak: b33p ")?;
            peer.sync()?;
            let listed = peer.messages_with(&primary_aci)?;
            let missing: Vec<String> = mine
                .iter()
                .filter(|text| {
                    !listed
                        .lines()
                        .any(|line| line.ends_with(&format!(" / {text}")))
                })
                .cloned()
                .collect();
            if !missing.is_empty() {
                return Err(StressError::PeerMissing {
                    missing,
                    sent: mine.len(),
                }
                .into());
            }
            Ok(growing(&samples)?)
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers, B33P));
    Ok(())
}

/// How long [`soak`] runs: `TESTBOX_SOAK_MINUTES`, or half an hour.
fn soak_minutes() -> Result<u64, EnvNumberError> {
    Ok(env_number("TESTBOX_SOAK_MINUTES")?.unwrap_or(30))
}

/// The index of the newest message, ours or the peer's, that is neither deleted nor a notice.
fn newest(snapshot: &Snapshot, outgoing: bool) -> Option<u32> {
    snapshot
        .messages
        .iter()
        .rfind(|message| {
            message.outgoing == outgoing
                && !message.deleted
                && message.text.starts_with("testbox soak:")
                && message.delivery != Some(Delivery::Sending)
        })
        .map(|message| message.index)
}

/// What [`soak`] watches, at one moment.
pub(super) struct Sample {
    /// What b33p's process holds itself (`RssAnon` in its /proc status), in bytes: what a leak
    /// grows. Not its container's cgroup, which counts wayvnc too, and while a viewer or the
    /// screenshots are connected that is some 150 MiB; nor `memory.current`, which counts the
    /// page cache too, the files b33p read and wrote, and which grows on its own after a restart
    /// (pending/tour.md).
    pub(super) memory: u64,
    /// The container's page cache (cgroup `memory.stat` `file`), in bytes, beside it.
    pub(super) cache: u64,
    pub(super) files: u64,
    pub(super) answer: Duration,
}

impl std::fmt::Display for Sample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory {} MiB (and {} MiB of page cache), {} open files, a snapshot in {:?}",
            self.memory >> 20,
            self.cache >> 20,
            self.files,
            self.answer
        )
    }
}

/// What `service`'s b33p uses and how fast it answers, now.
pub(super) fn sample(
    containers: &Pod,
    service: Service,
    b33p: &mut Harness,
) -> Result<Sample, Failure> {
    let started = Instant::now();
    b33p.snapshot()?;
    let answer = started.elapsed();
    let stat = containers.exec(service, &["cat", "/sys/fs/cgroup/memory.stat"], "")?;
    let cache = field(&stat, "file ", "memory.stat")?;
    // b33p isn't the container's first process (with-display is). An `if` rather than `&&`,
    // which would leave the loop failing whenever the last process isn't b33p.
    let script = r#"for p in /proc/[0-9]*; do
  if [ "$(cat "$p/comm" 2>/dev/null)" = b33p ]; then
    echo "files $(ls "$p/fd" | wc -l)"
    cat "$p/status"
  fi
done"#;
    let process = containers.exec(service, &["sh", "-c", script], "")?;
    Ok(Sample {
        // In kB.
        memory: field(&process, "RssAnon:", "b33p's status")? << 10,
        cache,
        files: field(&process, "files ", "b33p's open files")?,
        answer,
    })
}

/// The number after `key` on its line of `text`, which is `what`.
fn field(text: &str, key: &'static str, what: &'static str) -> Result<u64, StressError> {
    text.lines()
        .find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .ok_or(StressError::NoField { key, what })
}

/// Fails if the last third of `samples` is well above the first third: memory by half and
/// 64 MiB, open files by 32, the answering time past a second.
pub(super) fn growing(samples: &[Sample]) -> Result<(), StressError> {
    let third = samples.len() / 3;
    if third == 0 {
        return Ok(());
    }
    let median = |part: &[Sample], of: fn(&Sample) -> u64| {
        let mut values: Vec<u64> = part.iter().map(of).collect();
        values.sort_unstable();
        values[values.len() / 2]
    };
    let (first, last) = (&samples[..third], &samples[samples.len() - third..]);
    let mut wrong = Vec::new();
    let memory = (median(first, |s| s.memory), median(last, |s| s.memory));
    if memory.1 > memory.0 + memory.0 / 2 && memory.1 > memory.0 + (64 << 20) {
        wrong.push(Growth::Memory {
            from: memory.0,
            to: memory.1,
        });
    }
    let files = (median(first, |s| s.files), median(last, |s| s.files));
    if files.1 > files.0 + 32 {
        wrong.push(Growth::OpenFiles {
            from: files.0,
            to: files.1,
        });
    }
    if let Some(slow) = last.iter().find(|s| s.answer > Duration::from_secs(1)) {
        wrong.push(Growth::Slow { took: slow.answer });
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(StressError::Growing { growth: wrong })
    }
}

/// A 64 px picture, for the peer to send now and then.
fn small_picture() -> Vec<u8> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(64, 64, image::Rgba([30, 160, 90, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encodes");
    png
}

/// Texts b33p hasn't been sent before, each named for what is awkward about it.
fn awkward_texts() -> Vec<(&'static str, String)> {
    let combining: String = (0..200)
        .filter_map(|n| char::from_u32(0x300 + n % 0x70))
        .collect();
    vec![
        (
            "over 2 KB",
            format!("testbox stress: {}", "lorem ipsum ".repeat(250)),
        ),
        ("one line of 20000 characters", "x".repeat(20_000)),
        (
            "500 lines",
            (1..=500)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ("right to left", "مرحبا يا عالم، هذا اختبار".into()),
        ("mixed direction", "abc שלום 123 עולם def (נקודה).".into()),
        ("a direction override", "invoice\u{202E}fdp.exe".into()),
        ("200 combining marks", format!("Z{combining}algo")),
        (
            "zero-width characters",
            "a\u{200B}b\u{200C}c\u{200D}d\u{2060}e\u{FEFF}f".into(),
        ),
        ("long emoji sequences", "👨‍👩‍👧‍👦🏳️‍⚧️🧑🏽‍🤝‍🧑🏿🇩🇪".repeat(40)),
        ("one emoji", "😀".into()),
        ("only spaces", "   ".into()),
        ("only line breaks", "\n\n\n".into()),
        (
            "control characters",
            "bell\u{7} escape\u{1b}[31mred\u{1b}[0m tab\tend".into(),
        ),
        (
            "soft hyphens",
            "super\u{AD}cali\u{AD}fragi\u{AD}listic ".repeat(100),
        ),
        ("30 KB", "Signal ".repeat(4500)),
        (
            MARKUP,
            "*not bold* _not italic_ ~not struck~ `not code` ||no spoiler||".into(),
        ),
    ]
}

/// What of `text` b33p sends inline: as Signal's apps, up to 2000 bytes, and whole characters
/// (b33p-core's `split_long`).
fn inline_part(text: &str) -> &str {
    const INLINE: usize = 2000;
    if text.len() <= INLINE {
        return text;
    }
    let cut = (0..=INLINE)
        .rev()
        .find(|&at| text.is_char_boundary(at))
        .unwrap_or(0);
    &text[..cut]
}

/// Fails, naming them, unless `shown` has each of [`awkward_texts`] as it is.
fn missing_texts(shown: &[String]) -> Result<(), StressError> {
    let wrong: Vec<Mangled> = awkward_texts()
        .into_iter()
        .filter(|(_, text)| !shown.contains(text))
        .map(|(what, text)| {
            // What b33p made of it, if something close.
            let close = shown.iter().find(|shown| {
                shown.trim() == text.trim()
                    || shown.chars().count() > 100
                        && text.starts_with(&shown[..shown.floor_char_boundary(50)])
            });
            Mangled {
                what,
                shown: close.map(|close| close.chars().count()),
                sent: text.chars().count(),
            }
        })
        .collect();
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(StressError::Mangled { texts: wrong })
    }
}

/// Fetches the pictures of the message `text` until each is shown or refused; how each ended.
fn fetched(b33p: &mut Harness, text: &str, count: usize) -> Result<Vec<String>, HarnessError> {
    let ids: Vec<String> = b33p.wait_for(
        &format!("{count} attachments of {text:?}"),
        PATIENCE,
        |snapshot| {
            let message = snapshot.messages.iter().find(|m| m.text == text)?;
            (message.attachments.len() == count)
                .then(|| message.attachments.iter().map(|a| a.id.clone()).collect())
        },
    )?;
    for id in &ids {
        b33p.act(Action::FetchAttachment { id: id.clone() })?;
    }
    b33p.wait_for("each shown or refused", HISTORY_PATIENCE, |snapshot| {
        let message = snapshot.messages.iter().find(|m| m.text == text)?;
        message
            .attachments
            .iter()
            .map(|attachment| match &attachment.download {
                Some(Download::Ready) => Some(format!("{:?}: shown", attachment.file_name)),
                Some(Download::Failed(why)) => {
                    Some(format!("{:?}: refused ({why})", attachment.file_name))
                }
                Some(Download::Loading) | None => None,
            })
            .collect()
    })
}

/// Saves the attachment `id`: where to, or why it wasn't.
fn save(b33p: &mut Harness, id: &str) -> Result<Result<String, String>, Failure> {
    if !b33p.act(Action::SaveAttachment { id: id.to_owned() })? {
        return Err(StressError::NotSaving {
            id: id.to_owned(),
            again: false,
        }
        .into());
    }
    // The state the save before left: wait for this one's.
    thread::sleep(Duration::from_millis(500));
    Ok(b33p.wait_for("saved, or not", PATIENCE, |snapshot| {
        let attachment = snapshot
            .messages
            .iter()
            .flat_map(|message| &message.attachments)
            .find(|attachment| attachment.id == id)?;
        match &attachment.saved {
            Some(Saved::Saved(path)) => Some(Ok(path.clone())),
            Some(Saved::Failed(error)) => Some(Err(error.clone())),
            Some(Saved::Saving) | None => None,
        }
    })?)
}

/// Pictures a decoder may trip on, by file name: their type is guessed from it.
fn broken_pictures() -> Vec<(String, Vec<u8>)> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 30, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encodes");
    let mut claims_huge = png.clone();
    // IHDR's width and height, at bytes 16 to 24: 100000 a side, with 64 × 64 pixels' data.
    claims_huge[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    claims_huge[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    vec![
        ("empty.png".into(), Vec::new()),
        ("truncated.png".into(), png[..png.len() / 2].to_vec()),
        ("claims-huge.png".into(), claims_huge),
        ("png-named.jpg".into(), png.clone()),
        ("text-named.png".into(), b"not a picture at all\n".to_vec()),
        ("noise.gif".into(), large_file()[..64 * 1024].to_vec()),
        ("frames.gif".into(), animated_gif()),
    ]
}

/// [`HUGE_SIDE`] a side, RGBA, all one colour: small as a file, 484 MB decoded.
fn huge_picture() -> Result<Vec<u8>, StressError> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(HUGE_SIDE, HUGE_SIDE, image::Rgba([10, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|source| StressError::EncodePicture {
            side: HUGE_SIDE,
            source,
        })?;
    Ok(png)
}

/// A GIF of 300 frames, 400 px a side.
fn animated_gif() -> Vec<u8> {
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .expect("encodes");
        for n in 0..300u32 {
            let shade = (n % 256) as u8;
            let frame = image::Frame::new(image::RgbaImage::from_pixel(
                400,
                400,
                image::Rgba([shade, 255 - shade, 128, 255]),
            ));
            encoder.encode_frame(frame).expect("encodes");
        }
    }
    gif
}

/// File names a sender's app may give, which b33p mustn't save outside the downloads folder,
/// nor fail to save.
fn odd_names() -> Vec<String> {
    vec![
        "Ünïcødé 文件 🎉.txt".into(),
        "..\\..\\evil.txt".into(),
        "line\nbreak.txt".into(),
        "trailing dots...".into(),
        "-starts-with-a-dash.txt".into(),
        format!("{}.txt", "n".repeat(LONG_NAME - 4)),
        // 254 bytes in 3-byte characters.
        format!("{}.txt", "文".repeat((LONG_NAME - 4) / 3)),
    ]
}

/// The peer's first message, its request accepted; the conversation b33p shows it in.
fn accepted(
    checks: &mut Checks,
    b33p: &mut Harness,
    peer: &Presage<'_>,
    primary_aci: &str,
    phase: &str,
) -> Result<String, Failure> {
    let conversation = checks.require("text from the peer reaches b33p", || {
        peer_text_reaches(
            b33p,
            peer,
            primary_aci,
            &format!("testbox stress {phase}: from the peer"),
        )
    })?;
    checks.require(REQUEST_ACCEPTED, || {
        Ok(accept_request(b33p, &conversation)?)
    })?;
    Ok(conversation)
}

/// `prefix 001` to `prefix <count>`: in order when compared as text, and told apart in a preview.
fn numbered(prefix: &str, count: usize) -> Vec<String> {
    (1..=count).map(|n| format!("{prefix} {n:03}")).collect()
}

fn last(texts: &[String]) -> Result<&str, StressError> {
    texts.last().map(String::as_str).ok_or(StressError::NoTexts)
}

/// b33p's own messages in the open conversation that start with `prefix`, and how far each got.
fn deliveries(snapshot: &Snapshot, prefix: &str) -> Vec<(String, Delivery)> {
    snapshot
        .messages
        .iter()
        .filter(|message| message.outgoing && message.text.starts_with(prefix))
        .map(|message| {
            (
                message.text.clone(),
                message.delivery.unwrap_or(Delivery::Sending),
            )
        })
        .collect()
}

/// The number of the [`numbered`] text `conversation`'s preview shows, if it shows one.
fn preview_number(snapshot: &Snapshot, conversation: &str, prefix: &str) -> Option<usize> {
    snapshot
        .conversations
        .iter()
        .find(|chat| chat.id == conversation)?
        .preview
        .strip_prefix(prefix)?
        .trim()
        .parse()
        .ok()
}

/// The texts of the open conversation's messages that start with `prefix`, oldest first, all of
/// them: loading older pages until none comes.
fn all_texts(b33p: &mut Harness, prefix: &str) -> Result<Vec<String>, HarnessError> {
    let mut quiet = 0;
    let mut seen = 0;
    loop {
        let texts: Vec<String> = b33p
            .snapshot()?
            .messages
            .into_iter()
            .filter(|message| message.text.starts_with(prefix))
            .map(|message| message.text)
            .collect();
        if texts.len() == seen {
            quiet += 1;
            if quiet == QUIET_LOADS {
                return Ok(texts);
            }
        } else {
            seen = texts.len();
            quiet = 0;
        }
        b33p.act(Action::LoadOlder)?;
        thread::sleep(Duration::from_millis(500));
    }
}

/// Waits until the open conversation's messages stay the same for a while.
fn settled(b33p: &mut Harness) -> Result<(), Failure> {
    let deadline = Instant::now() + PATIENCE;
    let mut last = None;
    let mut quiet = 0;
    while Instant::now() < deadline {
        let shown: Vec<_> = b33p
            .snapshot()?
            .messages
            .into_iter()
            .map(|message| (message.text, message.delivery))
            .collect();
        if last.as_ref() == Some(&shown) {
            quiet += 1;
            if quiet == QUIET_LOADS {
                return Ok(());
            }
        } else {
            last = Some(shown);
            quiet = 0;
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err(StressError::KeptChanging { patience: PATIENCE }.into())
}

/// Fails, saying how, unless `found` is `expected`.
fn in_order_once(found: &[String], expected: &[String]) -> Result<(), StressError> {
    if found == expected {
        return Ok(());
    }
    let missing: Vec<String> = expected
        .iter()
        .filter(|text| !found.contains(text))
        .cloned()
        .collect();
    let mut repeated: Vec<String> = found
        .iter()
        .filter(|text| found.iter().filter(|other| other == text).count() > 1)
        .cloned()
        .collect();
    repeated.dedup();
    let out_of_order = found
        .windows(2)
        .find(|pair| pair[0] > pair[1])
        .map(|pair| (pair[0].clone(), pair[1].clone()));
    Err(StressError::NotInOrderOnce {
        found: found.len(),
        expected: expected.len(),
        missing,
        repeated,
        out_of_order,
    })
}

/// Kills b33p's container, starts it again, and connects to it unlocked.
fn kill_and_start(containers: &Pod, sockets: &Sockets) -> Result<Harness, Failure> {
    let since = process::now()?;
    containers.kill(B33P)?;
    // The pod may have started it again already (`restartPolicy: Always`): then this does
    // nothing.
    containers.start(B33P)?;
    started_b33p(containers, &sockets.b33p, since)
}

/// Fails if a snapshot takes over a second: b33p's window too busy to answer.
fn responsive(b33p: &mut Harness) -> Result<(), Failure> {
    let started = Instant::now();
    b33p.snapshot()?;
    let took = started.elapsed();
    println!("  a snapshot took {took:?}");
    if took > Duration::from_secs(1) {
        return Err(StressError::Unresponsive { took }.into());
    }
    Ok(())
}

fn memory(containers: &Pod, when: &str) {
    match containers.memory(B33P) {
        Ok(usage) => println!("  b33p's memory {when}: {usage}"),
        Err(error) => println!(
            "  b33p's memory {when}: unknown ({})",
            testbox_tools::Report(&error)
        ),
    }
}

/// Fails, quoting them, if `service`'s b33p's log has a Rust panic.
pub(super) fn no_panic(containers: &Pod, service: Service) -> Result<(), Failure> {
    let log = containers.log(service)?;
    let panics: Vec<String> = log
        .lines()
        .filter(|line| line.contains("panicked at"))
        .map(str::to_owned)
        .collect();
    if panics.is_empty() {
        Ok(())
    } else {
        Err(StressError::Panics {
            service: service.name(),
            panics,
        }
        .into())
    }
}

/// [`LARGE_FILE`] bytes that don't compress: a counter through xorshift.
fn large_file() -> Vec<u8> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut bytes = Vec::with_capacity(LARGE_FILE);
    while bytes.len() < LARGE_FILE {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes.truncate(LARGE_FILE);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(memory_mib: u64, files: u64) -> Sample {
        Sample {
            memory: memory_mib << 20,
            cache: 0,
            files,
            answer: Duration::from_millis(5),
        }
    }

    #[test]
    fn growth_is_told_from_noise() {
        let steady: Vec<Sample> = [100, 120, 90, 110, 105, 115].map(|m| at(m, 40)).into();
        assert!(growing(&steady).is_ok());
        let leaking: Vec<Sample> = [100, 110, 160, 220, 300, 380].map(|m| at(m, 40)).into();
        assert!(growing(&leaking).is_err());
        let files: Vec<Sample> = [10, 20, 40, 60, 80, 100].map(|f| at(100, f)).into();
        assert!(growing(&files).is_err());
    }

    #[test]
    fn numbered_texts_sort_in_order() {
        let texts = numbered("p", 120);
        let mut sorted = texts.clone();
        sorted.sort();
        assert_eq!(texts, sorted);
    }

    #[test]
    fn in_order_once_names_what_is_wrong() {
        let expected = numbered("p", 3);
        assert!(in_order_once(&expected, &expected).is_ok());
        let found = vec![
            expected[1].clone(),
            expected[0].clone(),
            expected[0].clone(),
        ];
        let Err(StressError::NotInOrderOnce {
            missing,
            repeated,
            out_of_order,
            ..
        }) = in_order_once(&found, &expected)
        else {
            panic!("passed");
        };
        assert_eq!(missing, ["p 003"]);
        assert_eq!(repeated, ["p 001"]);
        assert_eq!(out_of_order, Some(("p 002".to_owned(), "p 001".to_owned())));
    }
}
