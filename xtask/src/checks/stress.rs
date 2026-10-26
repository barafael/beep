//! b33p run harder than the other phases run it (pending/stress.md): many messages at once,
//! b33p or the server stopped at awkward moments, and texts and files b33p hasn't seen. Each check says what b33p must still show
//! afterwards; what it measures on the way (how long, how much memory) it prints.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use b33p_harness::{Action, AttachmentKind, Delivery, Download, Saved, Snapshot};

use super::attachments::incoming_attachment;
use super::b33p::{
    REQUEST_ACCEPTED, accept_request, chat_previewing, peer_text_reaches, select, type_and_send,
};
use super::presage::Presage;
use super::setup::{LinkedBox, linked_box, started_b33p};
use super::{Checks, HISTORY_PATIENCE, PATIENCE};
use crate::Sockets;
use crate::error::{Error, Result};
use crate::harness::Harness;
use crate::pod::{Pod, Service};
use crate::process;

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

/// Bursts of messages from the peer, with b33p running and with it stopped.
pub(crate) fn volume(checks: &mut Checks, containers: &Pod, sockets: &Sockets) -> Result<()> {
    let LinkedBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = linked_box(checks, containers, sockets)?;
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
            in_order_once(&all_texts(&mut b33p, "testbox stress: burst ")?, &texts)
        },
    );

    checks.record(
        &format!("{BURST} messages sent while b33p is stopped reach it once it starts"),
        || {
            let texts = numbered("testbox stress: while stopped", BURST);
            containers.stop(Service::B33p)?;
            peer.send_all(&primary_aci, &texts)?;
            let since = process::now()?;
            containers.start(Service::B33p)?;
            b33p = started_b33p(containers, &sockets.b33p, since)?;
            let started = Instant::now();
            chat_previewing(&mut b33p, last(&texts)?)?;
            println!("  b33p caught up in {:?}", started.elapsed());
            memory(containers, "after catching up");
            responsive(&mut b33p)?;
            select(&mut b33p, &conversation)?;
            in_order_once(
                &all_texts(&mut b33p, "testbox stress: while stopped ")?,
                &texts,
            )
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers));
    Ok(())
}

/// b33p killed while it receives, sends and downloads, and the server hanging.
pub(crate) fn interrupt(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<()> {
    let LinkedBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = linked_box(checks, containers, sockets)?;
    let conversation = accepted(checks, &mut b33p, &peer, &primary_aci, "interrupt")?;

    checks.record("b33p killed while a burst arrives loses none of it", || {
        let prefix = "testbox stress: killed while receiving";
        let texts = numbered(prefix, BURST);
        // Three times: each kill loses what was acknowledged and not yet stored only if it
        // comes at that moment.
        let kills = [BURST / 5, BURST / 2, BURST * 4 / 5];
        thread::scope(|scope| {
            let sending = scope.spawn(|| peer.send_all(&primary_aci, &texts));
            let killed = kills.iter().try_for_each(|&killed_at| {
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
            let sent = sending
                .join()
                .unwrap_or_else(|_| Err("the peer's sending panicked".into()));
            killed.and(sent)
        })?;
        chat_previewing(&mut b33p, last(&texts)?)?;
        select(&mut b33p, &conversation)?;
        in_order_once(&all_texts(&mut b33p, &format!("{prefix} "))?, &texts)
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
                    (true, None) => wrong.push(format!("{text:?} reached the peer, b33p lost it")),
                    (false, None) => wrong.push(format!(
                        "{text:?} didn't reach the peer, and b33p doesn't show it at all"
                    )),
                    (true, Some(delivery)) => wrong.push(format!(
                        "{text:?} reached the peer, b33p shows it {delivery:?}"
                    )),
                    (false, Some(delivery)) => wrong.push(format!(
                        "{text:?} didn't reach the peer, b33p shows it {delivery:?}"
                    )),
                }
            }
            if !wrong.is_empty() {
                return Err(Error::Failed(wrong.join("\n")));
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
                    .ok_or_else(|| Error::Failed(format!("{text:?} is gone from the chat")))?;
                if !b33p.act(Action::RetrySend { index })? {
                    return Err(Error::Failed(format!("b33p didn't send {text:?} again")));
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
            let twice: Vec<&String> = failed
                .iter()
                .filter(|text| received.matches(text.as_str()).count() != 1)
                .collect();
            if twice.is_empty() {
                println!("  sent again: {failed:?}");
                Ok(())
            } else {
                Err(Error::Failed(format!(
                    "sent again, these didn't reach the peer once: {twice:?}"
                )))
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
                return Err("b33p didn't start saving it".into());
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
                return Err("b33p didn't start saving it again".into());
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
                    Some(Saved::Failed(error)) => Some(Err(Error::Failed(error.clone()))),
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
            peer.sync_printing(text)
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers));
    Ok(())
}

/// Texts and files b33p hasn't been sent before: awkward Unicode and sizes, broken, huge and
/// animated pictures, and odd file names.
pub(crate) fn content(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<()> {
    let LinkedBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = linked_box(checks, containers, sockets)?;
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
        missing_texts(&shown)
    });

    checks.record("awkward texts from b33p reach the peer as sent", || {
        select(&mut b33p, &conversation)?;
        // Whitespace alone isn't a message to send, here as in Signal's apps.
        let texts: Vec<(&str, String)> = awkward_texts()
            .into_iter()
            .filter(|(_, text)| !text.trim().is_empty())
            .collect();
        for (_, text) in &texts {
            type_and_send(&mut b33p, text)?;
        }
        let received = peer.sync()?;
        let missing: Vec<&str> = texts
            .iter()
            .filter(|(_, text)| !received.contains(text.as_str()))
            .map(|(what, _)| *what)
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "the peer didn't get these as typed: {missing:?}"
            )))
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
            no_panic(containers)
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
            let before = containers.memory_peak(Service::B33p)?;
            let text = "testbox stress: huge pictures";
            peer.send_files(&primary_aci, text, &paths)?;
            select(&mut b33p, &conversation)?;
            let shown = fetched(&mut b33p, text, HUGE_PICTURES)?;
            println!("  b33p shows {shown:?}");
            let peak = containers.memory_peak(Service::B33p)?;
            println!(
                "  b33p's peak: {} MiB before, {} MiB after",
                before >> 20,
                peak >> 20
            );
            memory(containers, "after showing them");
            responsive(&mut b33p)?;
            if peak > MEMORY_BUDGET {
                return Err(Error::Failed(format!(
                    "b33p took {} MiB at once for {} KiB of pictures",
                    peak >> 20,
                    (picture.len() * HUGE_PICTURES) >> 10
                )));
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
                        Ok(path) => wrong.push(format!("{name:?} saved at {path:?}")),
                        Err(error) => {
                            wrong.push(format!("{name:?} not saved the {time} time: {error}"))
                        }
                    }
                }
            }
            if wrong.is_empty() {
                Ok(())
            } else {
                Err(Error::Failed(wrong.join("\n")))
            }
        },
    );

    checks.record("b33p logged no panic", || no_panic(containers));
    Ok(())
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
            "markup characters",
            "*not bold* _not italic_ ~not struck~ `not code` ||no spoiler||".into(),
        ),
    ]
}

/// Fails, naming them, unless `shown` has each of [`awkward_texts`] as it is.
fn missing_texts(shown: &[String]) -> Result<()> {
    let wrong: Vec<String> = awkward_texts()
        .into_iter()
        .filter(|(_, text)| !shown.contains(text))
        .map(|(what, text)| {
            // What b33p made of it, if something close.
            let close = shown.iter().find(|shown| {
                shown.trim() == text.trim()
                    || shown.chars().count() > 100
                        && text.starts_with(&shown[..shown.floor_char_boundary(50)])
            });
            match close {
                Some(close) => format!(
                    "{what}: shown as {} chars instead of {}",
                    close.chars().count(),
                    text.chars().count()
                ),
                None => format!("{what}: not shown"),
            }
        })
        .collect();
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(wrong.join("\n")))
    }
}

/// Fetches the pictures of the message `text` until each is shown or refused; how each ended.
fn fetched(b33p: &mut Harness, text: &str, count: usize) -> Result<Vec<String>> {
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
fn save(b33p: &mut Harness, id: &str) -> Result<std::result::Result<String, String>> {
    if !b33p.act(Action::SaveAttachment { id: id.to_owned() })? {
        return Err("b33p didn't start saving it".into());
    }
    // The state the save before left: wait for this one's.
    thread::sleep(Duration::from_millis(500));
    b33p.wait_for("saved, or not", PATIENCE, |snapshot| {
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
    })
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
fn huge_picture() -> Result<Vec<u8>> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(HUGE_SIDE, HUGE_SIDE, image::Rgba([10, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| Error::Failed(error.to_string()))?;
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
) -> Result<String> {
    let conversation = checks.require("text from the peer reaches b33p", || {
        peer_text_reaches(
            b33p,
            peer,
            primary_aci,
            &format!("testbox stress {phase}: from the peer"),
        )
    })?;
    checks.require(REQUEST_ACCEPTED, || accept_request(b33p, &conversation))?;
    Ok(conversation)
}

/// `prefix 001` to `prefix <count>`: in order when compared as text, and told apart in a preview.
fn numbered(prefix: &str, count: usize) -> Vec<String> {
    (1..=count).map(|n| format!("{prefix} {n:03}")).collect()
}

fn last(texts: &[String]) -> Result<&str> {
    texts
        .last()
        .map(String::as_str)
        .ok_or_else(|| "no texts".into())
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
fn all_texts(b33p: &mut Harness, prefix: &str) -> Result<Vec<String>> {
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
fn settled(b33p: &mut Harness) -> Result<()> {
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
    Err("the conversation kept changing".into())
}

/// Fails, saying how, unless `found` is `expected`.
fn in_order_once(found: &[String], expected: &[String]) -> Result<()> {
    if found == expected {
        return Ok(());
    }
    let missing: Vec<&String> = expected
        .iter()
        .filter(|text| !found.contains(text))
        .collect();
    let mut repeated: Vec<&String> = found
        .iter()
        .filter(|text| found.iter().filter(|other| other == text).count() > 1)
        .collect();
    repeated.dedup();
    let out_of_order = found.windows(2).find(|pair| pair[0] > pair[1]);
    Err(Error::Failed(format!(
        "b33p shows {} of {}; missing {missing:?}; more than once {repeated:?}; first out of \
         order {out_of_order:?}",
        found.len(),
        expected.len()
    )))
}

/// Kills b33p's container, starts it again, and connects to it unlocked.
fn kill_and_start(containers: &Pod, sockets: &Sockets) -> Result<Harness> {
    let since = process::now()?;
    containers.kill(Service::B33p)?;
    // The pod may have started it again already (`restartPolicy: Always`): then this does
    // nothing.
    containers.start(Service::B33p)?;
    started_b33p(containers, &sockets.b33p, since)
}

/// Fails if a snapshot takes over a second: b33p's window too busy to answer.
fn responsive(b33p: &mut Harness) -> Result<()> {
    let started = Instant::now();
    b33p.snapshot()?;
    let took = started.elapsed();
    println!("  a snapshot took {took:?}");
    if took > Duration::from_secs(1) {
        return Err(Error::Failed(format!("a snapshot took {took:?}")));
    }
    Ok(())
}

fn memory(containers: &Pod, when: &str) {
    match containers.memory(Service::B33p) {
        Ok(usage) => println!("  b33p's memory {when}: {usage}"),
        Err(error) => println!("  b33p's memory {when}: unknown ({error})"),
    }
}

/// Fails, quoting them, if b33p's log has a Rust panic.
fn no_panic(containers: &Pod) -> Result<()> {
    let log = containers.log(Service::B33p)?;
    let panics: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("panicked at"))
        .collect();
    if panics.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(panics.join("\n")))
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
        let Err(Error::Failed(reason)) = in_order_once(&found, &expected) else {
            panic!("passed");
        };
        assert!(reason.contains("missing [\"p 003\"]"), "{reason}");
        assert!(reason.contains("more than once [\"p 001\"]"), "{reason}");
        assert!(reason.contains("Some([\"p 002\", \"p 001\"])"), "{reason}");
    }
}
