use std::path::Path;

use b33p_harness::{
    Action, AttachmentKind, AttachmentSnapshot, Download, Saved, Snapshot, StickersSnapshot,
};

use super::b33p::{b33p_chat, select, type_and_send};
use super::setup::{LinkedBox, restart_b33p};
use super::{Checks, PATIENCE, PATIENCE_MS};
use crate::Sockets;
use crate::error::{Context, Error, Result};
use crate::harness::Harness;
use crate::pod::{Pod, Service};
use crate::process;
use testbox_tools::drivers;

/// Pictures, files and audio through CDN3 between b33p and Signal-Desktop, in `desktop`'s box,
/// then a profile picture and a sticker pack through CDN0. `files` is where both see the test
/// files: the host's side of their read-only `/files` mount, which no container can write to.
pub(super) fn attachments(
    checks: &mut Checks,
    linked: &mut LinkedBox<'_>,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
    picture: &Path,
) -> Result<()> {
    let LinkedBox {
        primary,
        primary_aci,
        b33p,
        desktop,
        conversation,
        ..
    } = linked;

    // The directory `cargo testbox up` made (xtask/src/pod.rs FILES).
    std::fs::copy(picture, files.join("picture.png")).context(picture.display().to_string())?;
    testbox_tools::write(&files.join("notes.txt"), "testbox attachments: a file\n")?;
    testbox_tools::write(&files.join("tone.wav"), b33p_core::mock_voice_message())?;
    let in_box = |name: &str| format!("/files/{name}");

    let picture_text = "testbox attachments: a picture from b33p";
    checks.record("a picture from b33p reaches Desktop", || {
        select(b33p, conversation)?;
        b33p.act(Action::StageFiles {
            paths: vec![in_box("picture.png")],
        })?;
        type_and_send(b33p, picture_text)?;
        desktop.call(drivers::Desktop::OpenConversation {
            aci: primary_aci.clone(),
        })?;
        desktop.call(drivers::Desktop::WaitForImage {
            text: picture_text.to_owned(),
            timeout_ms: PATIENCE_MS,
        })
    });

    checks.record("a file from b33p reaches Desktop", || {
        b33p.act(Action::StageFiles {
            paths: vec![in_box("notes.txt")],
        })?;
        type_and_send(b33p, "testbox attachments: a file from b33p")?;
        desktop.call(drivers::Desktop::WaitForFile {
            file_name: "notes.txt".to_owned(),
            timeout_ms: PATIENCE_MS,
        })
    });

    checks.record(
        "a picture from Desktop reaches b33p, downloaded and decrypted",
        || {
            let text = "testbox attachments: a picture from Desktop";
            desktop.call(drivers::Desktop::SendFiles {
                paths: vec![in_box("picture.png")],
                text: text.to_owned(),
            })?;
            let id = incoming_attachment(b33p, Some(text), AttachmentKind::Image)?;
            b33p.act(Action::FetchAttachment { id: id.clone() })?;
            downloaded(b33p, &id)
        },
    );

    checks.record("a file from Desktop reaches b33p, saved", || {
        let text = "testbox attachments: a file from Desktop";
        desktop.call(drivers::Desktop::SendFiles {
            paths: vec![in_box("notes.txt")],
            text: text.to_owned(),
        })?;
        let id = incoming_attachment(b33p, Some(text), AttachmentKind::File)?;
        b33p.act(Action::SaveAttachment { id: id.clone() })?;
        saved(b33p, &id)
    });

    // TESTPLAN.md §3.2. A voice message has no text, and b33p records none: what it sends the
    // other way is an audio file, which Desktop shows as audio as well (item 3).
    checks.record("a voice message from Desktop reaches b33p, saved", || {
        desktop.call(drivers::Desktop::SendVoiceMessage)?;
        let id = incoming_attachment(b33p, None, AttachmentKind::VoiceNote)?;
        b33p.act(Action::SaveAttachment { id: id.clone() })?;
        saved(b33p, &id)
    });

    checks.record("an audio file from b33p reaches Desktop as audio", || {
        b33p.act(Action::StageFiles {
            paths: vec![in_box("tone.wav")],
        })?;
        type_and_send(b33p, "testbox attachments: audio from b33p")?;
        desktop.call(drivers::Desktop::WaitForIncomingAudio {
            timeout_ms: PATIENCE_MS,
        })
    });

    // CDN0: Desktop uploads the picture with a POST policy (testbox/tools `cdn0`). b33p
    // fetches profile pictures when it starts and after a contact sync
    // (b33p-core/src/signal.rs `AvatarFetcher`), not when one changes: it restarts. First of
    // what reaches b33p from the primary: presage keeps a profile once fetched, avatar or not
    // (presage `retrieve_profile_by_uuid`, "Is there some way to know if this is outdated?"), and
    // the primary's first receive answers b33p's request for contacts, which has b33p fetch them.
    checks.record("a profile picture set on Desktop shows in b33p", || {
        desktop.call(drivers::Desktop::SetProfileAvatar)?;
        *b33p = restart_b33p(containers, &sockets.b33p)?;
        b33p_chat(b33p, conversation, "with the peer's picture", |chat| {
            chat.avatar
        })
    });

    // TESTPLAN.md §2.11: a pack seeded on CDN0, installed on the primary as on a
    // phone, which tells b33p with a sync; b33p downloads it in the background (presage) and its
    // picker gathers the installed packs when first opened. After the profile picture: the
    // primary's first receive has b33p fetch profiles. The primary takes in what waits for it in
    // a presage-cli of its own first, so that its answers (b33p's request for contacts) go out
    // before the install's sync, not alongside it with the same session counter, which b33p
    // turns away ("message with old counter"; presage answers in tasks of their own). The pack is
    // in b33p's store once b33p has downloaded its last sticker through CDN0, after the tool did
    // (testbox/tools `cdn0` logs each request).
    checks.record(
        "a sticker pack seeded on CDN0 and installed on the primary shows in b33p's picker, \
         and its sticker shows on Desktop",
        || {
            primary.sync()?;
            let since = process::now()?;
            let pack = primary.seed_stickers()?;
            let last = testbox_tools::STICKERS - 1;
            containers.await_logs(
                Service::Cdn0,
                &testbox_tools::cdn0_request_line(
                    "GET",
                    &format!("/stickers/{pack}/full/{last}"),
                    200,
                ),
                // The tool's download, then b33p's.
                2,
                Some(since),
                PATIENCE,
            )?;
            select(b33p, conversation)?;
            b33p.act(Action::OpenStickers)?;
            let sticker = b33p.wait_for("the pack in b33p's picker", PATIENCE, |snapshot| {
                let StickersSnapshot::Installed(packs) = &snapshot.stickers else {
                    return None;
                };
                Some(
                    packs
                        .iter()
                        .find(|installed| installed.id == pack)
                        .and_then(|installed| installed.stickers.first())
                        .map(|sticker| sticker.id)
                        .ok_or_else(|| {
                            Error::Failed(format!("b33p lists {packs:?}, not the pack {pack}"))
                        }),
                )
            })??;
            b33p.act(Action::SendSticker { pack, sticker })?;
            desktop.call(drivers::Desktop::WaitForSticker {
                timeout_ms: PATIENCE_MS,
            })
        },
    );

    Ok(())
}

/// The id of the `kind` attachment on an incoming message, reading `text` if given, once b33p
/// shows it.
pub(super) fn incoming_attachment(
    b33p: &mut Harness,
    text: Option<&str>,
    kind: AttachmentKind,
) -> Result<String> {
    b33p.wait_for(
        &format!("{text:?} with a {kind:?} in b33p"),
        PATIENCE,
        |snapshot| {
            snapshot
                .messages
                .iter()
                .filter(|message| !message.outgoing && text.is_none_or(|text| message.text == text))
                .flat_map(|message| &message.attachments)
                .find(|attachment| attachment.kind == kind)
                .map(|attachment| attachment.id.clone())
        },
    )
}

/// Waits until the attachment `id`, fetched, is downloaded and decrypted.
pub(super) fn downloaded(b33p: &mut Harness, id: &str) -> Result<()> {
    b33p.wait_for(
        &format!("{id} downloaded"),
        PATIENCE,
        |snapshot| match &attachment(snapshot, id)?.download {
            Some(Download::Ready) => Some(Ok(())),
            Some(Download::Failed(error)) => Some(Err(Error::Failed(error.clone()))),
            Some(Download::Loading) | None => None,
        },
    )?
}

/// Waits until the attachment `id` is saved.
fn saved(b33p: &mut Harness, id: &str) -> Result<()> {
    b33p.wait_for(
        &format!("{id} saved"),
        PATIENCE,
        |snapshot| match &attachment(snapshot, id)?.saved {
            Some(Saved::Saved(_)) => Some(Ok(())),
            Some(Saved::Failed(error)) => Some(Err(Error::Failed(error.clone()))),
            Some(Saved::Saving) | None => None,
        },
    )?
}

fn attachment<'a>(snapshot: &'a Snapshot, id: &str) -> Option<&'a AttachmentSnapshot> {
    snapshot
        .messages
        .iter()
        .flat_map(|message| &message.attachments)
        .find(|attachment| attachment.id == id)
}
