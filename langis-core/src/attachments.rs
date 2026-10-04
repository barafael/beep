//! Attachments of live and imported messages: describing them for the UI, and downloading,
//! decrypting and decoding them on demand. Signal's servers keep them for about 45 days.
//!
//! Downloads stay in memory. Only [`save`] writes a file, and only because the user asked.

use std::{path::PathBuf, rc::Rc, sync::Arc};

use futures::AsyncReadExt as _;
use presage::{
    libsignal_service::{
        attachment_cipher::decrypt_in_place, configuration::SignalServers,
        prelude::ProtobufMessage as _, push_service::PushService,
    },
    proto::{
        AttachmentPointer, DataMessage,
        attachment_pointer::{AttachmentIdentifier, Flags},
        data_message::Sticker,
    },
};
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;

use crate::{Attachment, AttachmentContent, AttachmentId, AttachmentKind, Event, Events, Picture};

/// Content type Signal uses for the full text of long messages.
pub(crate) const LONG_TEXT: &str = "text/x-signal-plain";

/// Longest edge of pictures sent to the UI: a 320 px bubble at 2x.
const PICTURE_EDGE: u32 = 640;

/// At most this many downloads at once, so opening a chat full of photos doesn't flood the
/// connection.
const PARALLEL_DOWNLOADS: usize = 4;

/// Everything worth showing that a data message carries.
pub(crate) fn of_message(data: &DataMessage) -> Vec<Attachment> {
    let view_once = data.is_view_once();
    let mut attachments: Vec<_> = data
        .attachments
        .iter()
        .map(|pointer| describe(pointer, view_once))
        .collect();
    if let Some(sticker) = data.sticker.as_ref().and_then(sticker) {
        attachments.push(sticker);
    }
    attachments
}

fn sticker(sticker: &Sticker) -> Option<Attachment> {
    let mut attachment = describe(sticker.data.as_ref()?, false);
    attachment.kind = AttachmentKind::Sticker;
    Some(attachment)
}

pub(crate) fn describe(pointer: &AttachmentPointer, view_once: bool) -> Attachment {
    let content_type = pointer.content_type().to_owned();
    let kind = if view_once {
        AttachmentKind::ViewOnce
    } else if content_type == LONG_TEXT {
        AttachmentKind::LongText
    } else if content_type.starts_with("image/") {
        AttachmentKind::Image
    } else if content_type.starts_with("video/") {
        AttachmentKind::Video
    } else if content_type.starts_with("audio/")
        && pointer.flags() & Flags::VoiceMessage as u32 != 0
    {
        AttachmentKind::VoiceNote
    } else if content_type.starts_with("audio/") {
        AttachmentKind::Audio
    } else {
        AttachmentKind::File
    };
    Attachment {
        id: id(pointer),
        kind,
        content_type,
        file_name: pointer.file_name.clone().filter(|name| !name.is_empty()),
        size: pointer.size().into(),
        width: pointer.width(),
        height: pointer.height(),
        pointer: pointer.encode_to_vec().into(),
    }
}

/// The CDN location identifies an upload; the digest or, failing both, the pointer itself
/// stands in.
fn id(pointer: &AttachmentPointer) -> AttachmentId {
    AttachmentId(match &pointer.attachment_identifier {
        Some(AttachmentIdentifier::CdnKey(key)) => format!("{}/{key}", pointer.cdn_number()),
        Some(AttachmentIdentifier::CdnId(id)) => format!("{}/{id}", pointer.cdn_number()),
        None => hex::encode(Sha256::digest(pointer.encode_to_vec())),
    })
}

/// Downloads and decrypts an attachment; see [`decrypt`].
async fn download(servers: SignalServers, attachment: &Attachment) -> Result<Vec<u8>, String> {
    let pointer = AttachmentPointer::decode(&*attachment.pointer)
        .map_err(|e| format!("invalid attachment: {e}"))?;
    let mut download = PushService::new(servers, None, "langis")
        .get_attachment(&pointer)
        .await
        .map_err(|e| match e {
            presage::libsignal_service::push_service::ServiceError::NotFoundError => {
                "This attachment is no longer available on Signal's servers.".to_string()
            }
            e => format!("failed to download the attachment: {e}"),
        })?;
    let mut data = Vec::with_capacity(download.content_length.unwrap_or_default() as usize);
    download
        .stream
        .read_to_end(&mut data)
        .await
        .map_err(|e| format!("failed to download the attachment: {e}"))?;
    tokio::task::spawn_blocking(move || decrypt(&pointer, data))
        .await
        .map_err(|e| e.to_string())?
}

/// Checks and decrypts a downloaded attachment. The HMAC under the attachment's own key always
/// authenticates it; the digest of the encrypted file is checked too when the pointer has one
/// (pointers from the history archive for media the phone already downloaded don't).
fn decrypt(pointer: &AttachmentPointer, mut data: Vec<u8>) -> Result<Vec<u8>, String> {
    let key: [u8; 64] = pointer
        .key()
        .try_into()
        .map_err(|_| "the attachment has no valid key".to_string())?;
    if let Some(digest) = &pointer.digest
        && Sha256::digest(&data)[..] != digest[..]
    {
        return Err("The attachment doesn't match what the sender sent.".into());
    }
    if data.len() < 16 + 32 {
        return Err("The attachment is incomplete.".into());
    }
    decrypt_in_place(key, &mut data)
        .map_err(|e| format!("failed to decrypt the attachment: {e}"))?;
    // Ciphertexts are padded; the pointer tells the real size.
    if let Some(size) = pointer.size.map(|size| size as usize)
        && size < data.len()
    {
        data.truncate(size);
    }
    Ok(data)
}

/// Downloads a picture or long text for display and reports it as [`Event::Attachment`].
pub(crate) async fn fetch(
    servers: SignalServers,
    attachment: Attachment,
    events: Events,
    limit: Rc<Semaphore>,
) {
    let content = async {
        let _permit = limit.acquire().await.map_err(|e| e.to_string())?;
        let data = download(servers, &attachment).await?;
        match attachment.kind {
            AttachmentKind::LongText => Ok(AttachmentContent::Text(
                String::from_utf8_lossy(&data).into_owned(),
            )),
            _ => tokio::task::spawn_blocking(move || decode_picture(&data))
                .await
                .map_err(|e| e.to_string())?
                .map(|picture| AttachmentContent::Picture(Arc::new(picture)))
                .ok_or_else(|| "This image can't be shown.".to_string()),
        }
    }
    .await;
    match &content {
        Ok(_) => {
            tracing::debug!(kind = ?attachment.kind, size = attachment.size, "fetched attachment")
        }
        Err(error) => {
            tracing::debug!(kind = ?attachment.kind, %error, "failed to fetch attachment")
        }
    }
    let _ = events.send(Event::Attachment {
        id: attachment.id,
        content,
    });
}

/// Downloads an attachment into the downloads folder and reports where as
/// [`Event::AttachmentSaved`].
pub(crate) async fn save(
    servers: SignalServers,
    attachment: Attachment,
    events: Events,
    limit: Rc<Semaphore>,
) {
    let path = async {
        let _permit = limit.acquire().await.map_err(|e| e.to_string())?;
        let data = download(servers, &attachment).await?;
        let path = free_path(&attachment)?;
        std::fs::write(&path, data)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
        Ok(path)
    }
    .await;
    let _ = events.send(Event::AttachmentSaved {
        id: attachment.id,
        path,
    });
}

/// Signal's limit for a single attachment.
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// Uploads `files` for sending, as the pointers to put in the message.
pub(crate) async fn upload(
    manager: &crate::signal::Mgr,
    files: &[PathBuf],
) -> Result<Vec<AttachmentPointer>, String> {
    let mut uploads = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_owned();
        let size = std::fs::metadata(path)
            .map_err(|e| format!("can't read {name}: {e}"))?
            .len();
        if size > MAX_UPLOAD_BYTES {
            return Err(format!("{name} is larger than Signal's limit of 100 MB."));
        }
        let data = std::fs::read(path).map_err(|e| format!("can't read {name}: {e}"))?;
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        // Receivers lay out pictures before downloading them, so they want the size.
        let (width, height) = if content_type.starts_with("image/") {
            image::ImageReader::new(std::io::Cursor::new(&data))
                .with_guessed_format()
                .ok()
                .and_then(|reader| reader.into_dimensions().ok())
                .map_or((None, None), |(w, h)| (Some(w), Some(h)))
        } else {
            (None, None)
        };
        let spec = presage::libsignal_service::sender::AttachmentSpec {
            content_type,
            length: data.len(),
            file_name: Some(name),
            preview: None,
            voice_note: None,
            borderless: None,
            width,
            height,
            caption: None,
            blur_hash: None,
        };
        uploads.push((spec, data));
    }
    let results = manager
        .upload_attachments(uploads)
        .await
        .map_err(|e| format!("failed to upload: {e}"))?;
    results
        .into_iter()
        .map(|result| result.map_err(|e| format!("failed to upload: {e}")))
        .collect()
}

/// Where opened attachments are put for the default app to read: private to the user, and on
/// Linux in the per-session runtime directory, which is memory-backed and cleared at logout.
fn open_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("langis"),
        None => std::env::temp_dir().join(format!("langis-{}", whoami())),
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

/// Removes attachments opened in an earlier run; by now their apps have read them.
pub(crate) fn clean_opened() {
    let _ = std::fs::remove_dir_all(open_dir());
}

/// Downloads an attachment and opens it with the desktop's default app. Errors are reported
/// as [`Event::Error`].
pub(crate) async fn open(
    servers: SignalServers,
    attachment: Attachment,
    events: Events,
    limit: Rc<Semaphore>,
) {
    let opened = async {
        let _permit = limit.acquire().await.map_err(|e| e.to_string())?;
        let data = download(servers, &attachment).await?;
        // One directory per attachment, so two files with the same name don't collide.
        let dir = open_dir().join(hex::encode(
            &Sha256::digest(attachment.id.0.as_bytes())[..8],
        ));
        crate::vault::restrict_dir(&open_dir()).map_err(|e| e.to_string())?;
        crate::vault::restrict_dir(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(safe_name(&attachment));
        crate::vault::write_private(&path, &data)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
        open::that_detached(&path).map_err(|e| format!("couldn't open {}: {e}", path.display()))
    }
    .await;
    if let Err(error) = opened {
        let _ = events.send(Event::Error(error));
    }
}

pub(crate) fn download_limit() -> Rc<Semaphore> {
    Rc::new(Semaphore::new(PARALLEL_DOWNLOADS))
}

/// Scales to at most [`PICTURE_EDGE`]; animated images show their first frame.
fn decode_picture(data: &[u8]) -> Option<Picture> {
    let image = decode_limited(data)?;
    let image = if image.width().max(image.height()) > PICTURE_EDGE {
        image.resize(
            PICTURE_EDGE,
            PICTURE_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Some(Picture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Decodes an image from the network with bounds, so a crafted file (tiny, but claiming huge
/// dimensions) can't make us allocate gigabytes. 12000 px a side covers any phone photo.
pub(crate) fn decode_limited(data: &[u8]) -> Option<image::DynamicImage> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    reader.decode().ok()
}

/// A file name in the downloads folder that isn't taken yet: the sender's name for the file
/// if it is safe, otherwise one made up from the type.
/// The sender's name for the file if it is safe to use (no directories, not hidden),
/// otherwise one made up from the type.
fn safe_name(attachment: &Attachment) -> String {
    attachment
        .file_name
        .as_deref()
        .and_then(|name| std::path::Path::new(name).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.starts_with('.'))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("signal-attachment.{}", extension(&attachment.content_type)))
}

fn free_path(attachment: &Attachment) -> Result<PathBuf, String> {
    let dirs = directories::UserDirs::new().ok_or("could not determine the downloads folder")?;
    let dir = dirs.download_dir().unwrap_or(dirs.home_dir()).to_path_buf();
    let name = safe_name(attachment);
    let path = std::path::Path::new(&name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment")
        .to_owned();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    (0..1000)
        .map(|n| {
            if n == 0 {
                dir.join(&name)
            } else {
                dir.join(format!("{stem} ({n}){ext}"))
            }
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| format!("too many files named {name} in {}", dir.display()))
}

fn extension(content_type: &str) -> &str {
    match content_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "audio/aac" => "aac",
        "audio/mpeg" => "mp3",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(content_type: &str, flags: u32) -> AttachmentPointer {
        AttachmentPointer {
            attachment_identifier: Some(AttachmentIdentifier::CdnKey("abc".into())),
            cdn_number: Some(3),
            content_type: Some(content_type.into()),
            flags: Some(flags),
            size: Some(1234),
            ..Default::default()
        }
    }

    #[test]
    fn classifies_by_type_and_flags() {
        let kind = |content_type, flags| describe(&pointer(content_type, flags), false).kind;
        assert_eq!(kind("image/jpeg", 0), AttachmentKind::Image);
        assert_eq!(kind("video/mp4", 0), AttachmentKind::Video);
        assert_eq!(
            kind("audio/aac", Flags::VoiceMessage as u32),
            AttachmentKind::VoiceNote
        );
        assert_eq!(kind("audio/aac", 0), AttachmentKind::Audio);
        assert_eq!(kind(LONG_TEXT, 0), AttachmentKind::LongText);
        assert_eq!(kind("application/pdf", 0), AttachmentKind::File);
        assert_eq!(
            describe(&pointer("image/jpeg", 0), true).kind,
            AttachmentKind::ViewOnce
        );
        let attachment = describe(&pointer("image/jpeg", 0), false);
        assert_eq!(attachment.id, AttachmentId("3/abc".into()));
        assert_eq!(attachment.size, 1234);
    }

    #[test]
    fn pointer_round_trips() {
        let attachment = describe(&pointer("image/png", 0), false);
        assert_eq!(
            AttachmentPointer::decode(&*attachment.pointer).unwrap(),
            pointer("image/png", 0)
        );
    }

    #[test]
    fn saved_names_stay_inside_the_folder() {
        let named = |name: &str| Attachment {
            file_name: Some(name.into()),
            ..describe(&pointer("application/pdf", 0), false)
        };
        for evil in ["../../etc/passwd", "/etc/passwd", ".bashrc"] {
            let path = free_path(&named(evil)).unwrap();
            let dirs = directories::UserDirs::new().unwrap();
            let dir = dirs.download_dir().unwrap_or(dirs.home_dir());
            assert_eq!(path.parent().unwrap(), dir, "{evil}");
            assert_ne!(path.file_name().unwrap(), ".bashrc");
        }
    }

    /// What a sender uploads: the padded plaintext, encrypted, and a pointer describing it.
    fn upload(plaintext: &[u8], padding: usize) -> (AttachmentPointer, Vec<u8>) {
        let key = [7; 64];
        let mut data = plaintext.to_vec();
        data.resize(plaintext.len() + padding, 0);
        presage::libsignal_service::attachment_cipher::encrypt_in_place([3; 16], key, &mut data);
        let pointer = AttachmentPointer {
            key: Some(key.to_vec()),
            size: Some(plaintext.len() as u32),
            digest: Some(Sha256::digest(&data).to_vec()),
            ..pointer("application/pdf", 0)
        };
        (pointer, data)
    }

    #[test]
    fn opens_what_was_uploaded() {
        let (pointer, data) = upload(b"route notes", 21);
        assert_eq!(decrypt(&pointer, data).unwrap(), b"route notes");
    }

    #[test]
    fn rejects_tampering_with_or_without_digest() {
        let (pointer, mut data) = upload(b"route notes", 0);
        data[20] ^= 1;
        assert!(decrypt(&pointer, data.clone()).is_err());
        // Without a digest (archive pointers), the HMAC still catches it.
        let pointer = AttachmentPointer {
            digest: None,
            ..pointer
        };
        assert!(decrypt(&pointer, data).unwrap_err().contains("decrypt"));
        assert!(decrypt(&pointer, vec![0; 10]).is_err());
    }

    #[test]
    fn decodes_and_scales_pictures() {
        let mut png = Vec::new();
        image::RgbaImage::new(1280, 640)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let picture = decode_picture(&png).unwrap();
        assert_eq!((picture.width, picture.height), (640, 320));
        assert!(decode_picture(b"not an image").is_none());
    }

    #[test]
    fn refuses_pictures_claiming_absurd_dimensions() {
        // A valid PNG header for 100000 x 100000 pixels, with no pixel data behind it.
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
        png.extend_from_slice(&100_000u32.to_be_bytes());
        png.extend_from_slice(&100_000u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        assert!(decode_limited(&png).is_none());
    }
}
