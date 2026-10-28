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

use crate::signal::errors::{failed, upload_kind};
use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, Error, Event, Events, Picture,
};
use presage::libsignal_service::sender::AttachmentUploadError;

/// Content type Signal uses for the full text of long messages.
pub(crate) const LONG_TEXT: &str = "text/x-signal-plain";
/// Longer long text isn't fetched: anyone can send one, and the chat would show all of it.
const LONG_TEXT_FETCHED: u64 = 1024 * 1024;
/// How much of a long text is shown, as in Signal's desktop app.
const LONG_TEXT_SHOWN: usize = 64 * 1024;

/// Longest edge of pictures sent to the UI: a 320 px bubble at 2x.
const PICTURE_EDGE: u32 = 640;
/// Longest edge of stickers sent to the UI: they show at 128 px.
const STICKER_EDGE: u32 = 256;
/// Height of link previews' pictures sent to the UI: they show at most 150 px tall, and as
/// wide as other pictures.
const PREVIEW_HEIGHT: u32 = 300;

/// Starts the ids of link previews' pictures; see [`describe_preview`].
const PREVIEW_ID: &str = "preview:";

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
        blurhash: pointer.blur_hash.clone().filter(|hash| !hash.is_empty()),
        caption: pointer
            .caption
            .clone()
            .filter(|caption| !caption.is_empty()),
        gif: pointer.flags() & Flags::Gif as u32 != 0,
        pointer: pointer.encode_to_vec().into(),
    }
}

/// A link preview's picture: as [`describe`], but with an id of its own. That says what it is
/// when it is fetched (by the attachment alone), so it is decoded smaller, and keeps it apart
/// from the same file attached to a message, which shows larger.
pub(crate) fn describe_preview(pointer: &AttachmentPointer) -> Attachment {
    let mut attachment = describe(pointer, false);
    attachment.id = AttachmentId(format!("{PREVIEW_ID}{}", attachment.id.0));
    attachment
}

/// What a picture is scaled to fit for the UI, width and height: twice what it is shown at.
fn picture_bounds(attachment: &Attachment) -> (u32, u32) {
    if attachment.kind == AttachmentKind::Sticker {
        (STICKER_EDGE, STICKER_EDGE)
    } else if attachment.id.0.starts_with(PREVIEW_ID) {
        (PICTURE_EDGE, PREVIEW_HEIGHT)
    } else {
        (PICTURE_EDGE, PICTURE_EDGE)
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

/// The Signal attachment pointer behind an attachment, e.g. to send it on.
pub(crate) fn pointer(attachment: &Attachment) -> Option<AttachmentPointer> {
    AttachmentPointer::decode(&*attachment.pointer).ok()
}

/// What a session's downloads share: one client for Signal's CDNs, which keeps its connections
/// to use again (one per download would pay a TLS handshake for every picture), the limit on
/// how many run at once, and the one on decoding.
pub(crate) struct Downloads {
    cdn: PushService,
    limit: Semaphore,
    /// One picture decoded at a time: a picture may take up to [`decode_limited`]'s 512 MiB
    /// decoded, and several arriving together took b33p past 2 GiB (pending/stress.md).
    decoding: Semaphore,
}

impl Downloads {
    pub(crate) fn new(servers: SignalServers) -> Rc<Self> {
        Rc::new(Self {
            cdn: PushService::new(servers, None, "b33p"),
            limit: Semaphore::new(PARALLEL_DOWNLOADS),
            decoding: Semaphore::new(1),
        })
    }
}

/// Downloads and decrypts an attachment; see [`decrypt`].
async fn download(cdn: &PushService, attachment: &Attachment) -> Result<Vec<u8>, Error> {
    let pointer = AttachmentPointer::decode(&*attachment.pointer)
        .map_err(|e| Error::refused(format!("invalid attachment: {e}")))?;
    // A copy shares the client, and with it the connections.
    let mut download = cdn
        .clone()
        .get_attachment(&pointer)
        .await
        .map_err(|e| match e {
            presage::libsignal_service::push_service::ServiceError::NotFoundError => {
                Error::refused("This attachment is no longer available on Signal's servers.")
            }
            e => Error::new(
                crate::signal::errors::service_kind(&e),
                format!("failed to download the attachment: {e}"),
            ),
        })?;
    let mut data = Vec::with_capacity(download.content_length.unwrap_or_default() as usize);
    download
        .stream
        .read_to_end(&mut data)
        .await
        .map_err(|e| Error::unreachable(format!("failed to download the attachment: {e}")))?;
    tokio::task::spawn_blocking(move || decrypt(&pointer, data))
        .await
        .map_err(|e| Error::refused(e.to_string()))?
        .map_err(Error::refused)
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
pub(crate) async fn fetch(downloads: Rc<Downloads>, attachment: Attachment, events: Events) {
    let content = async {
        if attachment.kind == AttachmentKind::LongText && attachment.size > LONG_TEXT_FETCHED {
            return Err(Error::refused("This message is too long to show."));
        }
        let data = {
            let _permit = downloads.limit.acquire().await.map_err(stopped)?;
            download(&downloads.cdn, &attachment).await?
        };
        match attachment.kind {
            AttachmentKind::LongText => Ok(AttachmentContent::Text(shown_text(&data))),
            AttachmentKind::Audio | AttachmentKind::VoiceNote => {
                Ok(AttachmentContent::Audio(data.into()))
            }
            _ => {
                let bounds = picture_bounds(&attachment);
                let _decoding = downloads.decoding.acquire().await.map_err(stopped)?;
                tokio::task::spawn_blocking(move || decode_picture(&data, bounds))
                    .await
                    .map_err(|e| Error::refused(e.to_string()))?
                    .map(|picture| AttachmentContent::Picture(Arc::new(picture)))
                    .ok_or_else(|| Error::refused("This image can't be shown."))
            }
        }
    }
    .await;
    match &content {
        Ok(_) => {
            tracing::debug!(kind = ?attachment.kind, size = attachment.size, "fetched attachment")
        }
        Err(error) => {
            tracing::debug!(kind = ?attachment.kind, error = %crate::scrub::scrubbed(&error), "failed to fetch attachment")
        }
    }
    events.send(Event::Attachment {
        id: attachment.id,
        content,
    });
}

/// What a long text shows: up to [`LONG_TEXT_SHOWN`] bytes of it, cut between characters.
fn shown_text(data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    if text.len() <= LONG_TEXT_SHOWN {
        return text.into_owned();
    }
    let end = text.floor_char_boundary(LONG_TEXT_SHOWN);
    format!("{}…", &text[..end])
}

/// Downloads an attachment into the downloads folder and reports where as
/// [`Event::AttachmentSaved`].
pub(crate) async fn save(downloads: Rc<Downloads>, attachment: Attachment, events: Events) {
    let path = async {
        let _permit = downloads.limit.acquire().await.map_err(stopped)?;
        let data = download(&downloads.cdn, &attachment).await?;
        let path = free_path(&attachment).map_err(Error::storage)?;
        tokio::fs::write(&path, data)
            .await
            .map_err(|e| Error::storage(format!("failed to write {}: {e}", path.display())))?;
        mark_downloaded(&path);
        Ok(path)
    }
    .await;
    events.send(Event::AttachmentSaved {
        id: attachment.id,
        path,
    });
}

/// Signal's limit for a single attachment.
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// How an upload of `data` is described to Signal.
fn spec(
    name: String,
    content_type: String,
    data: &[u8],
) -> presage::libsignal_service::sender::AttachmentSpec {
    // Receivers lay out pictures before downloading them, so they want the size.
    let (width, height) = if content_type.starts_with("image/") {
        image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .ok()
            .and_then(|reader| reader.into_dimensions().ok())
            .map_or((None, None), |(w, h)| (Some(w), Some(h)))
    } else {
        (None, None)
    };
    presage::libsignal_service::sender::AttachmentSpec {
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
    }
}

/// Uploads bytes that aren't a file, e.g. a link preview's picture.
pub(crate) async fn upload_bytes(
    manager: &crate::signal::Mgr,
    name: &str,
    content_type: &str,
    data: Vec<u8>,
) -> Result<AttachmentPointer, Error> {
    let spec = spec(name.to_owned(), content_type.to_owned(), &data);
    manager
        .upload_attachment(spec, data)
        .await
        .map_err(failed("failed to upload"))?
        .map_err(upload_failed)
}

/// A failed upload, of its kind.
fn upload_failed(error: AttachmentUploadError) -> Error {
    Error::new(upload_kind(&error), format!("failed to upload: {error}"))
}

/// The download limit's semaphore closed: the session is ending.
fn stopped(error: tokio::sync::AcquireError) -> Error {
    Error::unreachable(error.to_string())
}

/// Uploads `files` for sending, as the pointers to put in the message.
pub(crate) async fn upload(
    manager: &crate::signal::Mgr,
    files: &[PathBuf],
) -> Result<Vec<AttachmentPointer>, Error> {
    let mut uploads = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_owned();
        let size = tokio::fs::metadata(path)
            .await
            .map_err(|e| Error::storage(format!("can't read “{name}”: {e}")))?
            .len();
        if size > MAX_UPLOAD_BYTES {
            return Err(Error::refused(format!(
                "“{name}” is larger than Signal's limit of 100 MB."
            )));
        }
        let data = tokio::fs::read(path)
            .await
            .map_err(|e| Error::storage(format!("can't read “{name}”: {e}")))?;
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        let data = {
            let content_type = content_type.clone();
            tokio::task::spawn_blocking(move || without_metadata(&content_type, data))
                .await
                .map_err(|e| Error::refused(e.to_string()))?
        };
        let spec = spec(name, content_type, &data);
        uploads.push((spec, data));
    }
    let results = manager
        .upload_attachments(uploads)
        .await
        .map_err(failed("failed to upload"))?;
    results
        .into_iter()
        .map(|result| result.map_err(upload_failed))
        .collect()
}

/// Where opened attachments are put for the default app to read; see [`crate::scratch`].
const OPENED: &str = "b33p-opened";

/// Removes opened attachments: an earlier run's at the start (by now their apps have read
/// them), this run's at the end.
pub(crate) fn clean_opened() {
    crate::scratch::clean(OPENED);
}

/// File types the desktop would run rather than show, by extension: Signal Desktop's list,
/// plus what else Windows runs or mounts (shortcuts, disk images), and what macOS and Linux
/// desktops and shells run. The sender names the file, so "Open" on one of
/// these would run a program of the sender's choosing; they can still be saved, and run on
/// purpose from there.
#[rustfmt::skip]
const DANGEROUS_EXTENSIONS: &[&str] = &[
    "ade",
    "adp",
    "apk",
    "app",
    "appimage",
    "application",
    "appref-ms",
    "appx",
    "appxbundle",
    "bash",
    "bat",
    "cab",
    "chm",
    "cmd",
    "com",
    "command",
    "cpl",
    "deb",
    "desktop",
    "diagcab",
    "diagcfg",
    "diagpkg",
    "dll",
    "dmg",
    "exe",
    "fileloc",
    "flatpakref",
    "flatpakrepo",
    "hta",
    "img",
    "inetloc",
    "inf",
    "ins",
    "iso",
    "isp",
    "jar",
    "js",
    "jse",
    "lib",
    "library-ms",
    "lnk",
    "mde",
    "mht",
    "mhtml",
    "mpkg",
    "msc",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "mst",
    "nsh",
    "pif",
    "pkg",
    "pl",
    "ps1",
    "psc1",
    "psm1",
    "psrc",
    "py",
    "pyw",
    "rb",
    "reg",
    "rpm",
    "run",
    "scf",
    "scr",
    "sct",
    "search-ms",
    "searchconnector-ms",
    "settingcontent-ms",
    "sh",
    "shb",
    "snap",
    "sys",
    "terminal",
    "tool",
    "url",
    "vb",
    "vbe",
    "vbs",
    "vhd",
    "vhdx",
    "vxd",
    "webloc",
    "website",
    "workflow",
    "wsc",
    "wsf",
    "wsh",
    "xbap",
];

/// Marks a received file as downloaded from the internet, as browsers do, so that Windows and
/// macOS warn before running it (Mark of the Web, Gatekeeper's quarantine). Linux desktops
/// have no such mark.
fn mark_downloaded(path: &std::path::Path) {
    #[cfg(windows)]
    let marked = {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        // Zone 3: the internet.
        std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n")
    };
    #[cfg(target_os = "macos")]
    let marked = {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // Downloaded (0081), when, by whom.
        let value = format!("0081;{:08x};b33p;", since.as_secs());
        xattr::set(path, "com.apple.quarantine", value.as_bytes())
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let marked: std::io::Result<()> = {
        let _unmarked: &std::path::Path = path;
        Ok(())
    };
    if let Err(error) = marked {
        tracing::debug!(error = %crate::scrub::scrubbed(&error), "couldn't mark a file as downloaded");
    }
}

/// Whether opening a file called `name` with the desktop's default app could run it.
pub(crate) fn is_dangerous(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DANGEROUS_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Downloads an attachment and opens it with the desktop's default app. Errors are reported
/// as [`Event::Error`].
pub(crate) async fn open(downloads: Rc<Downloads>, attachment: Attachment, events: Events) {
    let opened = async {
        let name = safe_name(&attachment);
        if is_dangerous(&name) {
            return Err(Error::refused(format!(
                "{name} is a kind of file the desktop would run rather than show, so it isn't \
                 opened from here. Save it if you trust the sender, and open it from there."
            )));
        }
        let _permit = downloads.limit.acquire().await.map_err(stopped)?;
        let data = download(&downloads.cdn, &attachment).await?;
        // One directory per attachment, so two files with the same name don't collide.
        let subdir = hex::encode(&Sha256::digest(attachment.id.0.as_bytes())[..8]);
        // Writing up to 100 MB takes a while: not on this thread.
        tokio::task::spawn_blocking(move || {
            let dir = crate::scratch::dir(OPENED)
                .map_err(|e| format!("couldn't make a private directory: {e}"))?
                .join(subdir);
            crate::vault::restrict_dir(&dir).map_err(|e| e.to_string())?;
            let path = dir.join(name);
            crate::vault::write_private(&path, &data)
                .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
            mark_downloaded(&path);
            open::that_detached(&path).map_err(|e| format!("couldn't open {}: {e}", path.display()))
        })
        .await
        .map_err(|e| e.to_string())
        .flatten()
        // Writing the file, or the desktop opening it.
        .map_err(Error::storage)
    }
    .await;
    if let Err(error) = opened {
        events.send(Event::Error(error));
    }
}

/// Decodes a picture scaled to fit `width` by `height`, keeping its shape (see
/// [`picture_bounds`]); animated images show their first frame.
fn decode_picture(data: &[u8], (width, height): (u32, u32)) -> Option<Picture> {
    let image = decode_limited(data)?;
    let image = if image.width() > width || image.height() > height {
        image.resize(width, height, image::imageops::FilterType::Triangle)
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

/// Decodes a picture scaled to at most `edge` pixels a side.
pub(crate) fn thumbnail(data: &[u8], edge: u32) -> Option<Picture> {
    decode_picture(data, (edge, edge))
}

/// A photo drawn again from its pixels, as Signal's apps send them: without what its file
/// says besides (EXIF and XMP: where and when it was taken, with what), but turned the way
/// that said. Anything else, or what doesn't decode, stays as it is.
fn without_metadata(content_type: &str, data: Vec<u8>) -> Vec<u8> {
    use image::ImageDecoder as _;
    let format = match content_type {
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/png" => image::ImageFormat::Png,
        _ => return data,
    };
    let redraw = || -> image::ImageResult<Vec<u8>> {
        let mut decoder =
            image::ImageReader::with_format(std::io::Cursor::new(&data), format).into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut picture = image::DynamicImage::from_decoder(decoder)?;
        picture.apply_orientation(orientation);
        let mut out = std::io::Cursor::new(Vec::new());
        match format {
            image::ImageFormat::Jpeg => image::DynamicImage::ImageRgb8(picture.to_rgb8())
                .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                    &mut out, 90,
                ))?,
            _ => picture.write_to(&mut out, format)?,
        }
        Ok(out.into_inner())
    };
    match redraw() {
        Ok(redrawn) => redrawn,
        Err(error) => {
            tracing::warn!(error = %crate::scrub::scrubbed(&error), "couldn't take the metadata out of a picture; sending it as it is");
            data
        }
    }
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

/// The longest file name, in bytes, that Linux and macOS take (and Windows, in UTF-16 units).
const MAX_NAME: usize = 255;

/// The sender's name for the file if it is safe to use (no directories, not hidden, and
/// nothing Windows would quietly drop from the end, which could hide the real extension;
/// control characters such as line breaks as `_`), otherwise one made up from the type.
fn safe_name(attachment: &Attachment) -> String {
    attachment
        .file_name
        .as_deref()
        .and_then(|name| std::path::Path::new(name).file_name())
        .and_then(|name| name.to_str())
        .map(|name| name.trim_end_matches(['.', ' ']))
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .map(|name| {
            name.chars()
                .map(|c| if c.is_control() { '_' } else { c })
                .collect()
        })
        .unwrap_or_else(|| format!("signal-attachment.{}", extension(&attachment.content_type)))
}

/// `stem`, `suffix` and `ext` as one file name of at most [`MAX_NAME`] bytes: the stem cut
/// short, between characters, if it has to be.
fn fitted(stem: &str, suffix: &str, ext: &str) -> String {
    let room = MAX_NAME.saturating_sub(suffix.len() + ext.len());
    format!("{}{suffix}{ext}", &stem[..stem.floor_char_boundary(room)])
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
            let suffix = if n == 0 {
                String::new()
            } else {
                format!(" ({n})")
            };
            dir.join(fitted(&stem, &suffix, &ext))
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| format!("too many files named “{name}” in {}", dir.display()))
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

    /// A photo's EXIF (here, where it was taken) doesn't go out, but its orientation does.
    #[test]
    fn photos_go_without_their_metadata() {
        let mut photo = Vec::new();
        image::RgbImage::from_fn(2, 1, |x, _| image::Rgb([x as u8 * 200, 0, 0]))
            .write_to(
                &mut std::io::Cursor::new(&mut photo),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        // An APP1 segment: orientation 6 (turned right), and a GPS tag.
        #[rustfmt::skip]
        let exif: &[u8] = &[
            b'E', b'x', b'i', b'f', 0, 0,
            b'M', b'M', 0, 42, 0, 0, 0, 8,
            0, 2,
            0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0,
            0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0,
            0, 0, 0, 0,
        ];
        let length = u16::try_from(exif.len() + 2).unwrap();
        let mut tagged = photo[..2].to_vec();
        tagged.extend([0xff, 0xe1]);
        tagged.extend(length.to_be_bytes());
        tagged.extend(exif);
        tagged.extend(&photo[2..]);
        assert_eq!(image::load_from_memory(&tagged).unwrap().width(), 2);

        let sent = without_metadata("image/jpeg", tagged);
        assert!(!sent.windows(4).any(|w| w == b"Exif"));
        let shown = image::load_from_memory(&sent).unwrap();
        assert_eq!(
            (shown.width(), shown.height()),
            (1, 2),
            "turned as the EXIF said"
        );

        let text = b"not a picture".to_vec();
        assert_eq!(without_metadata("text/plain", text.clone()), text);
        assert_eq!(without_metadata("image/jpeg", text.clone()), text);
    }

    /// Anyone can send a long text, so only the start of a very long one is shown.
    #[test]
    fn long_text_shows_its_start() {
        assert_eq!(shown_text(b"short"), "short");
        let long = "é".repeat(LONG_TEXT_SHOWN);
        let shown = shown_text(long.as_bytes());
        assert!(shown.len() <= LONG_TEXT_SHOWN + '…'.len_utf8());
        assert!(shown.ends_with("é…"));
        assert_eq!(shown_text(&[0xff]), "\u{fffd}");
    }

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
    fn programs_are_not_opened() {
        for name in [
            "invoice.exe",
            "setup.EXE",
            "run.desktop",
            "x.sh",
            "notes.js",
            "a.lnk",
            "share.scf",
            "disk.iso",
            "page.mht",
            "open.SettingContent-ms",
            "site.webloc",
        ] {
            assert!(is_dangerous(name), "{name}");
        }
        for name in [
            "invoice.pdf",
            "photo.jpg",
            "exe",
            "notes.txt",
            "archive.tar.gz",
        ] {
            assert!(!is_dangerous(name), "{name}");
        }
        // Windows drops trailing dots and spaces, so these would end up as programs.
        let named = |name: &str| Attachment {
            file_name: Some(name.into()),
            ..describe(&pointer("application/octet-stream", 0), false)
        };
        assert_eq!(safe_name(&named("invoice.exe.")), "invoice.exe");
        assert_eq!(safe_name(&named("invoice.exe ")), "invoice.exe");
        assert_eq!(safe_name(&named("...")), "signal-attachment.bin");
    }

    /// A long name, and the same name saved again with its number, still make a file name;
    /// a line break doesn't go into one.
    #[test]
    fn saved_names_fit_and_have_no_control_characters() {
        let named = |name: &str| Attachment {
            file_name: Some(name.into()),
            ..describe(&pointer("text/plain", 0), false)
        };
        let long = format!("{}.txt", "文".repeat(100));
        assert_eq!(safe_name(&named(&long)), long);
        assert_eq!(fitted("文文文", "", ".txt"), "文文文.txt");
        let again = fitted(&"文".repeat(100), " (1)", ".txt");
        assert!(again.len() <= MAX_NAME, "{}", again.len());
        assert!(again.ends_with("文 (1).txt"));
        let path = free_path(&named(&long)).unwrap();
        assert!(path.file_name().unwrap().len() <= MAX_NAME);
        assert_eq!(safe_name(&named("line\nbreak.txt")), "line_break.txt");
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
        let bounds = (PICTURE_EDGE, PICTURE_EDGE);
        let picture = decode_picture(&png, bounds).unwrap();
        assert_eq!((picture.width, picture.height), (640, 320));
        assert!(decode_picture(b"not an image", bounds).is_none());
    }

    /// Stickers and link previews' pictures show smaller than photos, so they are decoded
    /// smaller; never smaller than shown, in either direction.
    #[test]
    fn pictures_are_as_large_as_they_show() {
        let photo = describe(&pointer("image/png", 0), false);
        let sticker = Attachment {
            kind: AttachmentKind::Sticker,
            ..photo.clone()
        };
        let preview = describe_preview(&pointer("image/png", 0));
        assert_eq!(preview.kind, AttachmentKind::Image);
        assert_ne!(preview.id, photo.id, "cached apart");
        assert_eq!(picture_bounds(&photo), (640, 640));
        assert_eq!(picture_bounds(&sticker), (256, 256));
        assert_eq!(picture_bounds(&preview), (640, 300));

        let png = |width, height| {
            let mut png = Vec::new();
            image::RgbaImage::new(width, height)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        };
        let size = |data: &[u8], of: &Attachment| {
            let picture = decode_picture(data, picture_bounds(of)).unwrap();
            (picture.width, picture.height)
        };
        assert_eq!(size(&png(512, 512), &sticker), (256, 256));
        // As wide as the bubble at 2x, as tall as the card allows.
        assert_eq!(size(&png(1200, 630), &preview), (571, 300));
        assert_eq!(size(&png(600, 1200), &preview), (150, 300));
        assert_eq!(size(&png(200, 100), &preview), (200, 100));
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
