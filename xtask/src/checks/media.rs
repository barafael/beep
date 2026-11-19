use std::path::{Path, PathBuf};
use std::time::Duration;

use b33p_harness::{Action, AttachmentKind, Download};

use super::attachments::{AttachmentsError, downloaded};
use super::b33p::{select, type_and_send};
use super::clients::android_shows;
use super::setup::{AndroidBox, restart_b33p};
use super::{Checks, Failure, PATIENCE, PATIENCE_MS};
use crate::Sockets;
use crate::harness::{Harness, HarnessError};
use crate::pod::{B33P, Pod};
use crate::process;
use testbox_tools::drivers;

#[derive(Debug, thiserror::Error)]
enum MediaError {
    /// b33p went to the CDN for what it should have had: kept from before, or sent itself.
    #[error("b33p downloaded {got} attachments from the CDN, not {wanted}")]
    Downloads { got: usize, wanted: usize },
    /// b33p sent the photo larger than Signal's apps would (`b33p_core` `attachments::scale`).
    #[error("b33p sent the photo as {size} bytes, more than {SENT_AT_MOST}")]
    SentWhole { size: u64 },
}

/// What a photo weighs at most as Signal's apps send it, at their standard level.
const SENT_AT_MOST: u64 = 1024 * 1024;

/// Fails unless b33p downloaded `wanted` attachments from a CDN since `since`, as its log
/// says (`b33p_core::DOWNLOADED_LOG`): what it had kept doesn't count.
fn b33p_downloaded(containers: &Pod, since: u64, wanted: usize) -> Result<(), Failure> {
    let got = containers
        .logs_since(B33P, Some(since))?
        .lines()
        .filter(|line| line.contains(b33p_core::DOWNLOADED_LOG))
        .count();
    if got != wanted {
        return Err(MediaError::Downloads { got, wanted }.into());
    }
    Ok(())
}

/// How long the clients get to fetch what they fetch on their own (Android's and Desktop's
/// automatic downloads) before the next check, so that `--capture` has each check's traffic
/// under its own name. Over the box's loopback a download takes milliseconds; it is when they
/// start that varies.
const SETTLE: Duration = Duration::from_secs(5);

/// Media through all three clients on the Android box, for `--capture` to compare how each
/// moves it: Desktop sends photos to b33p's account (b33p and Android, its primary, download
/// them), b33p sends one (Desktop downloads it, Android as a sync), then b33p restarts and
/// shows the chat again, and opens a photo in its viewer. `photo` is a camera-sized picture
/// (`TESTBOX_MEDIA_PHOTO`), `picture` a small one. b33p downloads each picture once: not what
/// it sent, and not again after a restart or for its viewer (it keeps them on disk).
pub(super) fn media(
    checks: &mut Checks,
    fixture: &mut AndroidBox<'_>,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
    photo: &Path,
    picture: &Path,
) -> Result<(), Failure> {
    let AndroidBox {
        android,
        b33p,
        desktop,
        android_aci,
        conversation,
        ..
    } = fixture;
    stage(photo, &files.join("photo.jpg"))?;
    stage(picture, &files.join("picture.png"))?;
    let in_box = |name: &str| format!("/files/{name}");

    checks.require("Desktop has the peer's conversation with Android", || {
        Ok(desktop.call(drivers::Desktop::OpenConversation {
            aci: android_aci.clone(),
        })?)
    })?;

    let from_desktop = "testbox media: two pictures from Desktop";
    checks.record(
        "two pictures from Desktop reach b33p and Android, and download",
        || {
            let since = process::now()?;
            select(b33p, conversation)?;
            desktop.call(drivers::Desktop::SendFiles {
                paths: vec![in_box("photo.jpg"), in_box("picture.png")],
                text: from_desktop.to_owned(),
            })?;
            for id in incoming_images(b33p, from_desktop, 2)? {
                b33p.act(Action::FetchAttachment { id: id.clone() })?;
                downloaded(b33p, &id)?;
            }
            android_shows(android, from_desktop)?;
            std::thread::sleep(SETTLE);
            b33p_downloaded(containers, since, 2)
        },
    );

    let from_b33p = "testbox media: a photo from b33p";
    checks.record(
        "a photo from b33p, as large as Signal's apps send one, reaches Desktop, and Android as \
         a sync, and b33p doesn't download it",
        || {
            let since = process::now()?;
            select(b33p, conversation)?;
            b33p.act(Action::StageFiles {
                paths: vec![in_box("photo.jpg")],
            })?;
            type_and_send(b33p, from_b33p)?;
            desktop.call(drivers::Desktop::WaitForImage {
                text: from_b33p.to_owned(),
                timeout_ms: PATIENCE_MS,
            })?;
            // Shown in b33p's chat, from what it sent.
            let (id, size) = outgoing_image(b33p, from_b33p)?;
            if size > SENT_AT_MOST {
                return Err(MediaError::SentWhole { size }.into());
            }
            b33p.act(Action::FetchAttachment { id: id.clone() })?;
            downloaded(b33p, &id)?;
            android_shows(android, from_b33p)?;
            std::thread::sleep(SETTLE);
            b33p_downloaded(containers, since, 0)
        },
    );

    checks.record(
        "b33p restarted shows the chat's pictures again, without downloading them",
        || {
            let since = process::now()?;
            *b33p = restart_b33p(containers, &sockets.b33p)?;
            select(b33p, conversation)?;
            for id in incoming_images(b33p, from_desktop, 2)? {
                b33p.act(Action::FetchAttachment { id: id.clone() })?;
                downloaded(b33p, &id)?;
            }
            let (id, _) = outgoing_image(b33p, from_b33p)?;
            b33p.act(Action::FetchAttachment { id: id.clone() })?;
            downloaded(b33p, &id)?;
            std::thread::sleep(SETTLE);
            b33p_downloaded(containers, since, 0)
        },
    );

    checks.record(
        "b33p's viewer opens Desktop's photo, without downloading it",
        || {
            let since = process::now()?;
            select(b33p, conversation)?;
            let ids = incoming_images(b33p, from_desktop, 2)?;
            b33p.act(Action::OpenViewer { id: ids[0].clone() })?;
            b33p.wait_for(
                "the photo in b33p's viewer",
                PATIENCE,
                |snapshot| match &snapshot.viewer.as_ref()?.download {
                    Download::Ready => Some(Ok(())),
                    Download::Failed(error) => Some(Err(error.clone())),
                    Download::Loading => None,
                },
            )?
            .map_err(|error| AttachmentsError::Failed {
                id: ids[0].clone(),
                step: super::attachments::Step::Download,
                error,
            })?;
            b33p.act(Action::CloseViewer)?;
            std::thread::sleep(SETTLE);
            b33p_downloaded(containers, since, 0)
        },
    );

    Ok(())
}

fn stage(from: &Path, to: &Path) -> Result<(), AttachmentsError> {
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|source| AttachmentsError::CopyPicture {
            from: from.to_owned(),
            to: PathBuf::from(to),
            source,
        })
}

/// The id and size of the picture on b33p's own message reading `text`, once b33p shows it.
fn outgoing_image(b33p: &mut Harness, text: &str) -> Result<(String, u64), HarnessError> {
    b33p.wait_for(
        &format!("{text:?} with its picture"),
        PATIENCE,
        |snapshot| {
            snapshot
                .messages
                .iter()
                .filter(|message| message.outgoing && message.text == text)
                .flat_map(|message| &message.attachments)
                .find(|attachment| attachment.kind == AttachmentKind::Image)
                .map(|attachment| (attachment.id.clone(), attachment.size))
        },
    )
}

/// The ids of the `count` pictures on the incoming message reading `text`, once b33p shows them.
fn incoming_images(
    b33p: &mut Harness,
    text: &str,
    count: usize,
) -> Result<Vec<String>, HarnessError> {
    b33p.wait_for(
        &format!("{text:?} with {count} pictures"),
        PATIENCE,
        |snapshot| {
            let ids: Vec<String> = snapshot
                .messages
                .iter()
                .filter(|message| !message.outgoing && message.text == text)
                .flat_map(|message| &message.attachments)
                .filter(|attachment| attachment.kind == AttachmentKind::Image)
                .map(|attachment| attachment.id.clone())
                .collect();
            (ids.len() >= count).then_some(ids)
        },
    )
}
