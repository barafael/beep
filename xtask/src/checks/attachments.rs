use std::path::{Path, PathBuf};

use b33p_harness::{Action, AttachmentKind, AttachmentSnapshot, Download, Saved, StickersSnapshot};

use super::b33p::{
    b33p_chat, b33p_shows, incoming_index, message_timestamp, outgoing_index, select, type_and_send,
};
use super::setup::{LinkedBox, restart_b33p};
use super::{Checks, Failure, PATIENCE, PATIENCE_MS};
use crate::Sockets;
use crate::harness::{Harness, HarnessError};
use crate::pod::{Pod, Service};
use crate::process;
use testbox_tools::drivers;

/// What the attachment checks find b33p not doing, or fail at on the way.
#[derive(Debug, thiserror::Error)]
pub(super) enum AttachmentsError {
    /// The picture the checks send, copied where the clients see the test files.
    #[error("copying {} to {}", from.display(), to.display())]
    CopyPicture {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("waiting for b33p's {step} of {id}")]
    Waiting {
        id: String,
        step: Step,
        #[source]
        source: HarnessError,
    },
    /// b33p shows the download or the saving of an attachment failed, with its own words.
    #[error("b33p's {step} of {id} failed: {error}")]
    Failed {
        id: String,
        step: Step,
        error: String,
    },
    /// b33p's sticker picker lists installed packs, not the one seeded.
    #[error("b33p lists the packs {installed:?}, not the pack {pack} (or it has no sticker)")]
    NoPack {
        pack: String,
        installed: Vec<String>,
    },
}

/// What b33p does with a received attachment, which [`AttachmentsError`] waits for.
#[derive(Debug, Clone, Copy)]
pub(super) enum Step {
    Download,
    Save,
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Download => "download",
            Self::Save => "saving",
        })
    }
}

/// Copies `picture` into `files` as `picture.png`, where the clients see it as
/// `/files/picture.png`.
pub(super) fn stage_picture(picture: &Path, files: &Path) -> Result<(), AttachmentsError> {
    let to = files.join("picture.png");
    std::fs::copy(picture, &to)
        .map(drop)
        .map_err(|source| AttachmentsError::CopyPicture {
            from: picture.to_owned(),
            to,
            source,
        })
}

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
) -> Result<(), Failure> {
    let LinkedBox {
        primary,
        primary_aci,
        b33p,
        desktop,
        conversation,
        ..
    } = linked;

    // The directory `cargo testbox up` made (xtask/src/pod.rs FILES).
    stage_picture(picture, files)?;
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
        Ok(desktop.call(drivers::Desktop::WaitForImage {
            text: picture_text.to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
    });

    // TESTPLAN.md §2.1 item 1, with Desktop for the phone: the copy keeps the picture and the
    // caption, so Desktop has two messages with the caption, each with a picture.
    checks.record(
        "the picture message forwarded from b33p reaches Desktop with its picture",
        || {
            select(b33p, conversation)?;
            let index = outgoing_index(b33p, picture_text)?;
            let original =
                message_timestamp(b33p, "b33p's picture", |message| message.index == index)?;
            b33p.act(Action::Forward { index })?;
            b33p.act(Action::ForwardTo {
                conversation: conversation.clone(),
            })?;
            select(b33p, conversation)?;
            b33p_shows(b33p, "the forwarded picture", |message| {
                message.outgoing
                    && message.timestamp > original
                    && message.text == picture_text
                    && !message.attachments.is_empty()
            })?;
            Ok(desktop.call(drivers::Desktop::WaitForImages {
                text: picture_text.to_owned(),
                count: 2,
                timeout_ms: PATIENCE_MS,
            })?)
        },
    );

    checks.record("a file from b33p reaches Desktop", || {
        b33p.act(Action::StageFiles {
            paths: vec![in_box("notes.txt")],
        })?;
        type_and_send(b33p, "testbox attachments: a file from b33p")?;
        Ok(desktop.call(drivers::Desktop::WaitForFile {
            file_name: "notes.txt".to_owned(),
            timeout_ms: PATIENCE_MS,
        })?)
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
            Ok(downloaded(b33p, &id)?)
        },
    );

    // TESTPLAN.md §1.4 item 6 for a message with an attachment: the picture goes with it.
    checks.record(
        "a delete for everyone of Desktop's picture removes it in b33p",
        || {
            let text = "testbox attachments: a picture from Desktop";
            let index = incoming_index(b33p, text)?;
            let timestamp =
                message_timestamp(b33p, "Desktop's picture", |message| message.index == index)?;
            desktop.call(drivers::Desktop::DeleteForEveryone {
                text: text.to_owned(),
            })?;
            Ok(b33p_shows(b33p, "Desktop's picture deleted", |message| {
                message.timestamp == timestamp && message.deleted && message.attachments.is_empty()
            })?)
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
        Ok(saved(b33p, &id)?)
    });

    // TESTPLAN.md §3.2. A voice message has no text, and b33p records none: what it sends the
    // other way is an audio file, which Desktop shows as audio as well (item 3).
    checks.record("a voice message from Desktop reaches b33p, saved", || {
        desktop.call(drivers::Desktop::SendVoiceMessage)?;
        let id = incoming_attachment(b33p, None, AttachmentKind::VoiceNote)?;
        b33p.act(Action::SaveAttachment { id: id.clone() })?;
        Ok(saved(b33p, &id)?)
    });

    checks.record("an audio file from b33p reaches Desktop as audio", || {
        b33p.act(Action::StageFiles {
            paths: vec![in_box("tone.wav")],
        })?;
        type_and_send(b33p, "testbox attachments: audio from b33p")?;
        Ok(desktop.call(drivers::Desktop::WaitForIncomingAudio {
            timeout_ms: PATIENCE_MS,
        })?)
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
        Ok(b33p_chat(
            b33p,
            conversation,
            "with the peer's picture",
            |chat| chat.avatar,
        )?)
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
                        .ok_or_else(|| AttachmentsError::NoPack {
                            pack: pack.clone(),
                            installed: packs.iter().map(|installed| installed.id.clone()).collect(),
                        }),
                )
            })??;
            b33p.act(Action::SendSticker { pack, sticker })?;
            Ok(desktop.call(drivers::Desktop::WaitForSticker {
                timeout_ms: PATIENCE_MS,
            })?)
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
) -> Result<String, HarnessError> {
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
pub(super) fn downloaded(b33p: &mut Harness, id: &str) -> Result<(), AttachmentsError> {
    settled(b33p, id, Step::Download, |attachment| {
        match &attachment.download {
            Some(Download::Ready) => Some(Ok(())),
            Some(Download::Failed(error)) => Some(Err(error.clone())),
            Some(Download::Loading) | None => None,
        }
    })
}

/// Waits until the attachment `id` is saved.
pub(super) fn saved(b33p: &mut Harness, id: &str) -> Result<(), AttachmentsError> {
    settled(b33p, id, Step::Save, |attachment| match &attachment.saved {
        Some(Saved::Saved(_)) => Some(Ok(())),
        Some(Saved::Failed(error)) => Some(Err(error.clone())),
        Some(Saved::Saving) | None => None,
    })
}

/// Waits until `outcome` says the attachment `id`'s `step` is done, or failed with b33p's words.
fn settled(
    b33p: &mut Harness,
    id: &str,
    step: Step,
    outcome: impl Fn(&AttachmentSnapshot) -> Option<Result<(), String>>,
) -> Result<(), AttachmentsError> {
    let waiting = match step {
        Step::Download => format!("{id} downloaded"),
        Step::Save => format!("{id} saved"),
    };
    b33p.wait_for(&waiting, PATIENCE, |snapshot| {
        outcome(snapshot.attachment(id)?)
    })
    .map_err(|source| AttachmentsError::Waiting {
        id: id.to_owned(),
        step,
        source,
    })?
    .map_err(|error| AttachmentsError::Failed {
        id: id.to_owned(),
        step,
        error,
    })
}
