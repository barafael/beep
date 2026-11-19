//! Attachments of live and imported messages: describing them for the UI, and downloading,
//! decrypting and decoding them on demand. Signal's servers keep them for about 45 days.
//!
//! What is downloaded is kept on disk, encrypted under the database's key ([`cache`]), and
//! shown from there afterwards. Only [`save`] and [`open`] write a file anyone can read, and only
//! because the user asked.

pub(crate) mod cache;
mod scale;

use cache::{Cache, Entry};

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

use crate::signal::errors::{closed_under_request, kind_of, service_kind, upload_kind};
use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, ErrorKind, Event, Events,
    FullPicture, Picture,
};
use presage::libsignal_service::{
    attachment_cipher::AttachmentCipherError, push_service::ServiceError,
    sender::AttachmentUploadError,
};

/// What fails in getting an attachment: downloading and decrypting it, and showing, saving or
/// opening it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AttachmentError {
    #[error("the attachment's pointer doesn't decode")]
    Pointer(#[source] prost::DecodeError),
    #[error("This attachment is no longer available on Signal's servers.")]
    Gone,
    #[error("failed to download the attachment")]
    Fetch(#[source] ServiceError),
    #[error("failed to download the attachment")]
    Read(#[source] std::io::Error),
    #[error("the attachment has no valid key")]
    Key { len: usize },
    #[error("The attachment doesn't match what the sender sent.")]
    Digest,
    #[error("The attachment is incomplete.")]
    Incomplete { len: usize },
    #[error("failed to decrypt the attachment")]
    Decrypt(#[source] AttachmentCipherError),
    #[error("This message is too long to show.")]
    TooLong { size: u64 },
    #[error("This image can't be shown.")]
    NotShowable,
    #[error("could not determine the downloads folder")]
    NoDownloadsDir,
    #[error("too many files named “{name}” in {}", dir.display())]
    TooManyNamed { name: String, dir: PathBuf },
    #[error("failed to write {}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "{name} is a kind of file the desktop would run rather than show, so it isn't opened \
         from here. Save it if you trust the sender, and open it from there."
    )]
    Dangerous { name: String },
    #[error("couldn't make a private directory")]
    ScratchDir(#[source] crate::scratch::ScratchError),
    #[error("couldn't write the attachment privately")]
    WritePrivate(#[source] crate::vault::PrivateFileError),
    #[error("couldn't open {}", path.display())]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A blocking step (decrypting, decoding, writing) panicked or was cancelled.
    #[error("{step} stopped")]
    Stopped {
        step: &'static str,
        source: tokio::task::JoinError,
    },
    #[error("the session is ending")]
    Ending(#[source] tokio::sync::AcquireError),
}

impl crate::Classified for AttachmentError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Fetch(error) => service_kind(error),
            Self::Read(_) | Self::Ending(_) => ErrorKind::Unreachable,
            Self::NoDownloadsDir
            | Self::TooManyNamed { .. }
            | Self::Write { .. }
            | Self::ScratchDir(_)
            | Self::WritePrivate(_)
            | Self::Open { .. } => ErrorKind::Storage,
            Self::Pointer(_)
            | Self::Gone
            | Self::Key { .. }
            | Self::Digest
            | Self::Incomplete { .. }
            | Self::Decrypt(_)
            | Self::TooLong { .. }
            | Self::NotShowable
            | Self::Dangerous { .. }
            | Self::Stopped { .. } => ErrorKind::Refused,
        }
    }
}

/// What fails in uploading files to send.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UploadError {
    #[error("can't read “{name}”")]
    ReadFile {
        name: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("“{name}” is larger than Signal's limit of 100 MB.")]
    TooLarge { name: String, size: u64 },
    #[error("making “{name}” ready to send stopped")]
    StripStopped {
        name: String,
        source: tokio::task::JoinError,
    },
    #[error("failed to upload")]
    Upload(#[source] presage::Error<presage_store_sqlite::SqliteStoreError>),
    #[error("failed to upload “{name}”")]
    Attachment {
        name: String,
        source: AttachmentUploadError,
    },
}

impl crate::Classified for UploadError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::ReadFile { .. } => ErrorKind::Storage,
            Self::TooLarge { .. } | Self::StripStopped { .. } => ErrorKind::Refused,
            Self::Upload(error) => kind_of(error),
            Self::Attachment { source, .. } => upload_kind(source),
        }
    }
}

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

/// The ids of everything of `data` that may be kept on disk: its attachments, its sticker and
/// its link previews' pictures; for forgetting them with the message.
pub(crate) fn kept_of(data: &DataMessage) -> Vec<AttachmentId> {
    of_message(data)
        .into_iter()
        .map(|attachment| attachment.id)
        .chain(
            data.preview
                .iter()
                .filter_map(|preview| preview.image.as_ref())
                .map(|image| describe_preview(image).id),
        )
        .collect()
}

/// [`kept_of`] for a message as presage stores it (`thread_messages.content_body`, a
/// `Content`): what it sent, or what a linked device of ours sent, and their edits.
pub(crate) fn kept_of_stored(body: &[u8]) -> Vec<AttachmentId> {
    use presage::libsignal_service::prelude::ProtobufMessage as _;
    use presage::proto::content::Content as Body;
    let Ok(content) = presage::proto::Content::decode(body) else {
        return Vec::new();
    };
    let data = match &content.content {
        Some(Body::DataMessage(data)) => Some(data),
        Some(Body::EditMessage(edit)) => edit.data_message.as_ref(),
        Some(Body::SyncMessage(presage::proto::SyncMessage {
            content: Some(presage::proto::sync_message::Content::Sent(sent)),
            ..
        })) => sent.message.as_ref().or_else(|| {
            sent.edit_message
                .as_ref()
                .and_then(|edit| edit.data_message.as_ref())
        }),
        _ => None,
    };
    data.map(kept_of).unwrap_or_default()
}

/// Forgets what `cache` keeps of `ids`, on a blocking thread.
pub(crate) async fn forget(cache: &Option<Arc<Cache>>, ids: Vec<AttachmentId>) {
    let Some(cache) = cache.clone() else {
        return;
    };
    if ids.is_empty() {
        return;
    }
    let forgotten = tokio::task::spawn_blocking(move || ids.iter().for_each(|id| cache.forget(id)));
    if forgotten.await.is_err() {
        tracing::debug!("stopped before forgetting kept attachments");
    }
}

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
pub(crate) fn describe_preview(pointer: &AttachmentPointer) -> Box<Attachment> {
    let mut attachment = describe(pointer, false);
    attachment.id = AttachmentId(format!("{PREVIEW_ID}{}", attachment.id.0));
    Box::new(attachment)
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
/// to use again (one per download would pay a TLS handshake for every picture), the attachments
/// kept on disk, the limit on how many downloads run at once, and the one on decoding.
pub(crate) struct Downloads {
    cdn: PushService,
    cache: Option<Arc<Cache>>,
    limit: Semaphore,
    /// One picture decoded at a time: a picture may take up to [`decode_limited`]'s 512 MiB
    /// decoded, and several arriving together took b33p past 2 GiB (pending/stress.md).
    decoding: Semaphore,
    /// What [`prefetch`] may take of `limit`: leaves the rest for what is shown.
    prefetching: Semaphore,
    /// One download of an attachment at a time, so that one shown while it downloads as it
    /// came (or opened in the viewer meanwhile) waits for it, rather than downloading it again.
    downloading:
        std::cell::RefCell<std::collections::HashMap<AttachmentId, Rc<tokio::sync::Mutex<()>>>>,
}

/// At most this many of [`PARALLEL_DOWNLOADS`] are [`prefetch`]es.
const PREFETCHING: usize = 2;

impl Downloads {
    pub(crate) fn new(servers: SignalServers, cache: Option<Arc<Cache>>) -> Rc<Self> {
        if let Some(cache) = cache.clone() {
            // Last session's downloads may have gone past the bound.
            tokio::task::spawn_blocking(move || cache.trim(KEPT_ORIGINALS));
        }
        Rc::new(Self {
            cdn: crate::client::push_service(servers),
            cache,
            limit: Semaphore::new(PARALLEL_DOWNLOADS),
            decoding: Semaphore::new(1),
            prefetching: Semaphore::new(PREFETCHING),
            downloading: Default::default(),
        })
    }

    /// Whether `id` is kept as `entry`: that its file is there, not read.
    async fn kept(&self, id: &AttachmentId, entry: Entry) -> bool {
        let Some(cache) = self.cache.clone() else {
            return false;
        };
        let id = id.clone();
        tokio::task::spawn_blocking(move || cache.has(&id, entry))
            .await
            .unwrap_or(false)
    }

    /// What is kept of `id` as `entry`, read on a blocking thread.
    async fn cached(&self, id: &AttachmentId, entry: Entry) -> Option<Vec<u8>> {
        let cache = self.cache.clone()?;
        let id = id.clone();
        tokio::task::spawn_blocking(move || cache.read(&id, entry))
            .await
            .ok()
            .flatten()
    }

    /// Keeps `data` as `entry` of `id`, on a blocking thread, and hands it back. A failure only
    /// costs a download later.
    async fn keep(&self, id: &AttachmentId, entry: Entry, data: Vec<u8>) -> Vec<u8> {
        let Some(cache) = self.cache.clone() else {
            return data;
        };
        let id = id.clone();
        let kept = tokio::task::spawn_blocking(move || {
            if let Err(error) = cache.write(&id, entry, &data) {
                tracing::warn!(error = %crate::scrub::chain(&error), "could not keep an attachment");
            }
            if entry == Entry::Original {
                cache.trim(KEPT_ORIGINALS);
            }
            data
        })
        .await;
        // Fails only when the runtime shuts down, the data with it.
        kept.unwrap_or_default()
    }

    /// The attachment as sent, decrypted: kept from before, or downloaded (within the limit on
    /// downloads at once) and kept.
    async fn original(&self, attachment: &Attachment) -> Result<Vec<u8>, AttachmentError> {
        let lock = self
            .downloading
            .borrow_mut()
            .entry(attachment.id.clone())
            .or_default()
            .clone();
        let original = {
            let _held = lock.lock().await;
            self.original_once(attachment).await
        };
        // The last one done with it lets it go.
        if Rc::strong_count(&lock) == 2 {
            self.downloading.borrow_mut().remove(&attachment.id);
        }
        original
    }

    /// [`Self::original`], with the attachment's download to itself.
    async fn original_once(&self, attachment: &Attachment) -> Result<Vec<u8>, AttachmentError> {
        if let Some(data) = self.cached(&attachment.id, Entry::Original).await {
            return Ok(data);
        }
        let data = {
            let _permit = self
                .limit
                .acquire()
                .await
                .map_err(AttachmentError::Ending)?;
            download(&self.cdn, attachment).await?
        };
        tracing::debug!(kind = ?attachment.kind, size = data.len(), "{}", crate::DOWNLOADED_LOG);
        Ok(self.keep(&attachment.id, Entry::Original, data).await)
    }
}

/// How much the originals kept on disk may take; what goes first is what was used longest ago,
/// and is downloaded again if wanted. The pictures as chats show them aren't counted (see
/// [`Cache::trim`]).
const KEPT_ORIGINALS: u64 = 2 * 1024 * 1024 * 1024;

/// Downloads and decrypts an attachment; see [`decrypt`].
async fn download(cdn: &PushService, attachment: &Attachment) -> Result<Vec<u8>, AttachmentError> {
    let pointer =
        AttachmentPointer::decode(&*attachment.pointer).map_err(AttachmentError::Pointer)?;
    // A copy shares the client, and with it the connections.
    let mut download = cdn
        .clone()
        .get_attachment(&pointer)
        .await
        .map_err(|e| match e {
            ServiceError::NotFoundError => AttachmentError::Gone,
            e => AttachmentError::Fetch(e),
        })?;
    let mut data = Vec::with_capacity(download.content_length.unwrap_or_default() as usize);
    download
        .stream
        .read_to_end(&mut data)
        .await
        .map_err(AttachmentError::Read)?;
    tokio::task::spawn_blocking(move || decrypt(&pointer, data))
        .await
        .map_err(|source| AttachmentError::Stopped {
            step: "decrypting the attachment",
            source,
        })?
}

/// Checks and decrypts a downloaded attachment. The HMAC under the attachment's own key always
/// authenticates it; the digest of the encrypted file is checked too when the pointer has one
/// (pointers from the history archive for media the phone already downloaded don't).
fn decrypt(pointer: &AttachmentPointer, mut data: Vec<u8>) -> Result<Vec<u8>, AttachmentError> {
    let key: [u8; 64] = pointer.key().try_into().map_err(|_| AttachmentError::Key {
        len: pointer.key().len(),
    })?;
    if let Some(digest) = &pointer.digest
        && Sha256::digest(&data)[..] != digest[..]
    {
        return Err(AttachmentError::Digest);
    }
    if data.len() < 16 + 32 {
        return Err(AttachmentError::Incomplete { len: data.len() });
    }
    decrypt_in_place(key, &mut data).map_err(AttachmentError::Decrypt)?;
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
            return Err(AttachmentError::TooLong {
                size: attachment.size,
            });
        }
        if is_picture(attachment.kind) {
            // As shown before: small, and quick to decode.
            if let Some(shown) = downloads.cached(&attachment.id, Entry::Shown).await
                && let Ok(Some(picture)) =
                    tokio::task::spawn_blocking(move || decode_shown(&shown)).await
            {
                return Ok(AttachmentContent::Picture(Arc::new(picture)));
            }
        }
        let data = downloads.original(&attachment).await?;
        match attachment.kind {
            AttachmentKind::LongText => Ok(AttachmentContent::Text(shown_text(&data))),
            AttachmentKind::Audio | AttachmentKind::VoiceNote => {
                Ok(AttachmentContent::Audio(data.into()))
            }
            _ => shown_picture(&downloads, &attachment, data)
                .await
                .map(|picture| AttachmentContent::Picture(Arc::new(picture))),
        }
    }
    .await;
    match &content {
        Ok(_) => {
            tracing::debug!(kind = ?attachment.kind, size = attachment.size, "fetched attachment")
        }
        Err(error) => {
            tracing::debug!(kind = ?attachment.kind, error = %crate::scrub::chain(error), "failed to fetch attachment")
        }
    }
    events.send(Event::Attachment {
        id: attachment.id,
        content: content.map_err(crate::Error::from),
    });
}

/// `data`, the original of the picture `attachment`, decoded as the chat shows it (one at a
/// time), and kept so.
async fn shown_picture(
    downloads: &Downloads,
    attachment: &Attachment,
    data: Vec<u8>,
) -> Result<Picture, AttachmentError> {
    let bounds = picture_bounds(attachment);
    let _decoding = downloads
        .decoding
        .acquire()
        .await
        .map_err(AttachmentError::Ending)?;
    let (picture, shown) = tokio::task::spawn_blocking(move || {
        let picture = decode_picture(&data, bounds);
        let shown = picture.as_ref().and_then(encode_shown);
        (picture, shown)
    })
    .await
    .map_err(|source| AttachmentError::Stopped {
        step: "decoding the picture",
        source,
    })?;
    if let Some(shown) = shown {
        downloads.keep(&attachment.id, Entry::Shown, shown).await;
    }
    picture.ok_or(AttachmentError::NotShowable)
}

/// What of a message that just came is downloaded at once, as Signal's apps do: what its chat
/// shows (pictures, stickers, a link preview's picture, the rest of a long text) and voice
/// messages, each at most [`PREFETCHED_AT_MOST`]. Files, videos and audio wait to be asked
/// for, and view-once media for its viewing.
pub(crate) fn prefetched(message: &crate::ChatMessage) -> Vec<Attachment> {
    message
        .attachments
        .iter()
        .chain(
            message
                .preview
                .iter()
                .filter_map(|preview| preview.image.as_deref()),
        )
        .filter(|attachment| {
            matches!(
                attachment.kind,
                AttachmentKind::Image
                    | AttachmentKind::Sticker
                    | AttachmentKind::LongText
                    | AttachmentKind::VoiceNote
            ) && attachment.size <= PREFETCHED_AT_MOST
        })
        .cloned()
        .collect()
}

/// The largest attachment [`prefetched`] downloads unasked.
const PREFETCHED_AT_MOST: u64 = 25 * 1024 * 1024;

/// Downloads `attachment` into the cache unless it is there, and for a picture keeps it as the
/// chat shows it, so that showing it takes neither: in the background, as a message comes.
/// At most [`PREFETCHING`] at once, so that what a chat shows when opened doesn't wait long
/// behind them.
pub(crate) async fn prefetch(downloads: Rc<Downloads>, attachment: Attachment) {
    let Ok(_turn) = downloads.prefetching.acquire().await else {
        return;
    };
    let picture = is_picture(attachment.kind);
    let wanted = if picture {
        Entry::Shown
    } else {
        Entry::Original
    };
    if downloads.cache.is_none() || downloads.kept(&attachment.id, wanted).await {
        return;
    }
    let done = async {
        let data = downloads.original(&attachment).await?;
        if picture {
            shown_picture(&downloads, &attachment, data).await?;
        }
        Ok::<_, AttachmentError>(())
    };
    if let Err(error) = done.await {
        tracing::debug!(kind = ?attachment.kind, error = %crate::scrub::chain(&error), "failed to download an attachment as it came");
    }
}

/// Downloads a picture for the picture viewer and reports it as [`Event::FullPicture`]: the
/// whole picture rather than [`fetch`]'s bubble-sized one.
pub(crate) async fn fetch_full(downloads: Rc<Downloads>, attachment: Attachment, events: Events) {
    let picture = async {
        let data = downloads.original(&attachment).await?;
        let _decoding = downloads
            .decoding
            .acquire()
            .await
            .map_err(AttachmentError::Ending)?;
        tokio::task::spawn_blocking(move || decode_full(&data))
            .await
            .map_err(|source| AttachmentError::Stopped {
                step: "decoding the picture",
                source,
            })?
            .ok_or(AttachmentError::NotShowable)
    }
    .await;
    if let Err(error) = &picture {
        tracing::debug!(error = %crate::scrub::chain(error), "failed to fetch a picture to view");
    }
    events.send(Event::FullPicture {
        id: attachment.id,
        picture: picture.map_err(crate::Error::from),
    });
}

/// Longest edge of pictures for the viewer. What Signal's apps send at their highest quality;
/// larger ones (from elsewhere) are scaled down to it, which keeps the copies the window
/// makes to draw it (each 64 MB at most) and the textures of GPU renderers in bounds.
const VIEW_EDGE: u32 = 4096;

/// Decodes a picture for the viewer (within [`decode_limited`]'s bounds, against decompression
/// bombs), at most [`VIEW_EDGE`] a side; animated images show their first frame.
fn decode_full(data: &[u8]) -> Option<FullPicture> {
    let rgba = decode_limited(data)?.into_rgba8();
    let (width, height) = rgba.dimensions();
    let picture = Picture {
        width,
        height,
        rgba: rgba.into_raw(),
    };
    let edge = width.max(height);
    let picture = if edge > VIEW_EDGE {
        let (w, h) =
            crate::resample::scaled((width, height), f64::from(VIEW_EDGE) / f64::from(edge));
        crate::resample::shrink(&picture, w, h)?
    } else {
        picture
    };
    Some(FullPicture {
        pixels: Arc::new(picture),
        width,
        height,
    })
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
    tracing::debug!(kind = ?attachment.kind, size = attachment.size, "saving attachment");
    let path = async {
        let data = downloads.original(&attachment).await?;
        let path = free_path(&attachment)?;
        if let Err(source) = tokio::fs::write(&path, data).await {
            return Err(AttachmentError::Write { path, source });
        }
        mark_downloaded(&path);
        Ok(path)
    }
    .await;
    match &path {
        Ok(_) => {
            tracing::debug!(kind = ?attachment.kind, size = attachment.size, "saved attachment")
        }
        Err(error) => {
            tracing::debug!(kind = ?attachment.kind, error = %crate::scrub::chain(error), "failed to save attachment")
        }
    }
    events.send(Event::AttachmentSaved {
        id: attachment.id,
        path: path.map_err(crate::Error::from),
    });
}

/// Signal's limit for a single attachment.
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// How an upload of `data` is described to Signal.
fn spec(
    name: String,
    content_type: String,
    data: &[u8],
    blur_hash: Option<String>,
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
        blur_hash,
    }
}

/// Uploads bytes that aren't a file, e.g. a link preview's picture.
pub(crate) async fn upload_bytes(
    manager: &crate::signal::Mgr,
    name: &str,
    content_type: &str,
    data: Vec<u8>,
) -> Result<AttachmentPointer, UploadError> {
    upload_retrying(manager, name, content_type, data, None)
        .await
        .map_err(UploadError::Upload)?
        .map_err(|source| UploadError::Attachment {
            name: name.to_owned(),
            source,
        })
}

/// Uploads `data` as `name`; once more, on a new connection, if the first try went out on a
/// kept-alive connection the CDN closed under it ([`closed_under_request`]): a CDN closes idle
/// connections, and the next upload may come just as it does.
async fn upload_retrying(
    manager: &crate::signal::Mgr,
    name: &str,
    content_type: &str,
    data: Vec<u8>,
    blur_hash: Option<String>,
) -> Result<
    Result<AttachmentPointer, AttachmentUploadError>,
    presage::Error<presage_store_sqlite::SqliteStoreError>,
> {
    let first = spec(
        name.to_owned(),
        content_type.to_owned(),
        &data,
        blur_hash.clone(),
    );
    match manager.upload_attachment(first, data.clone()).await? {
        Err(error) if closed_under_request(&error) => {
            tracing::info!(
                error = %crate::scrub::chain(&error),
                "the CDN closed the connection under an upload; uploading it once more"
            );
            let again = spec(name.to_owned(), content_type.to_owned(), &data, blur_hash);
            manager.upload_attachment(again, data).await
        }
        uploaded => Ok(uploaded),
    }
}

/// Uploads `files` for sending, as the pointers to put in the message: pictures made ready as
/// Signal's apps make them ([`scale`], at its high-quality level with `high`). What was
/// uploaded is kept in `cache` as if downloaded, so that showing what was sent doesn't
/// download it.
pub(crate) async fn upload(
    manager: &crate::signal::Mgr,
    cache: &Option<Arc<Cache>>,
    files: &[PathBuf],
    high: bool,
) -> Result<Vec<AttachmentPointer>, UploadError> {
    let mut uploads = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_owned();
        let unreadable = |source| UploadError::ReadFile {
            name: name.clone(),
            path: path.clone(),
            source,
        };
        let size = tokio::fs::metadata(path).await.map_err(unreadable)?.len();
        if size > MAX_UPLOAD_BYTES {
            return Err(UploadError::TooLarge { name, size });
        }
        let data = tokio::fs::read(path).await.map_err(unreadable)?;
        let content_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        let ready =
            tokio::task::spawn_blocking(move || scale::for_sending(&content_type, data, high))
                .await
                .map_err(|source| UploadError::StripStopped {
                    name: name.clone(),
                    source,
                })?;
        // A PNG that went as JPEG is named so.
        let name = match (ready.content_type.as_str(), name.rsplit_once('.')) {
            ("image/jpeg", Some((stem, extension)))
                if !matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg") =>
            {
                format!("{stem}.jpg")
            }
            _ => name,
        };
        uploads.push((name, ready.content_type, ready.data, ready.blur_hash));
    }
    // All at once, as presage's `upload_attachments`, each with its own retry.
    let results = futures::future::join_all(uploads.into_iter().map(
        |(name, content_type, data, blur_hash)| async move {
            let uploaded =
                upload_retrying(manager, &name, &content_type, data.clone(), blur_hash).await;
            if let Ok(Ok(pointer)) = &uploaded {
                keep_sent(cache, pointer, data).await;
            }
            (name, uploaded)
        },
    ))
    .await;
    results
        .into_iter()
        .map(|(name, uploaded)| {
            uploaded
                .map_err(UploadError::Upload)?
                .map_err(|source| UploadError::Attachment { name, source })
        })
        .collect()
}

/// Keeps what was sent as `pointer` as its original, as if downloaded.
async fn keep_sent(cache: &Option<Arc<Cache>>, pointer: &AttachmentPointer, data: Vec<u8>) {
    let Some(cache) = cache.clone() else {
        return;
    };
    let id = id(pointer);
    let kept = tokio::task::spawn_blocking(move || cache.write(&id, Entry::Original, &data)).await;
    if let Ok(Err(error)) = kept {
        tracing::warn!(error = %crate::scrub::chain(&error), "could not keep a sent attachment");
    }
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
        tracing::debug!(error = %crate::scrub::chain(&error), "couldn't mark a file as downloaded");
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
            return Err(AttachmentError::Dangerous { name });
        }
        let data = downloads.original(&attachment).await?;
        // One directory per attachment, so two files with the same name don't collide.
        let subdir = hex::encode(&Sha256::digest(attachment.id.0.as_bytes())[..8]);
        // Writing up to 100 MB takes a while: not on this thread.
        tokio::task::spawn_blocking(move || {
            let dir = crate::scratch::dir(OPENED)
                .map_err(AttachmentError::ScratchDir)?
                .join(subdir);
            crate::vault::restrict_dir(&dir).map_err(AttachmentError::WritePrivate)?;
            let path = dir.join(name);
            crate::vault::write_private(&path, &data).map_err(AttachmentError::WritePrivate)?;
            mark_downloaded(&path);
            open::that_detached(&path).map_err(|source| AttachmentError::Open { path, source })
        })
        .await
        .map_err(|source| AttachmentError::Stopped {
            step: "writing the attachment to open it",
            source,
        })?
    }
    .await;
    if let Err(error) = opened {
        events.send(Event::Error(error.into()));
    }
}

/// Pictures, which chats show decoded; the rest (long text, audio) is used as it is.
fn is_picture(kind: AttachmentKind) -> bool {
    !matches!(
        kind,
        AttachmentKind::LongText | AttachmentKind::Audio | AttachmentKind::VoiceNote
    )
}

/// A picture as the chat shows it, as PNG, for the cache: a fraction of the original to decode.
fn encode_shown(picture: &Picture) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut png,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        &picture.rgba,
        picture.width,
        picture.height,
        image::ExtendedColorType::Rgba8,
    )
    .ok()?;
    Some(png)
}

/// The picture [`encode_shown`] kept.
fn decode_shown(png: &[u8]) -> Option<Picture> {
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    Some(Picture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Decodes a picture scaled to fit `width` by `height`, keeping its shape (see
/// [`picture_bounds`]); animated images show their first frame.
fn decode_picture(data: &[u8], (width, height): (u32, u32)) -> Option<Picture> {
    let rgba = decode_limited(data)?.into_rgba8();
    let picture = Picture {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    };
    if picture.width <= width && picture.height <= height {
        return Some(picture);
    }
    // Within the bounds, keeping its shape. `fast_image_resize` rather than `image`'s resize:
    // a 12 MP photo took 295 ms with `image`'s triangle filter (release build), most of what
    // showing one cost.
    let scale = (f64::from(width) / f64::from(picture.width))
        .min(f64::from(height) / f64::from(picture.height));
    let (width, height) = crate::resample::scaled((picture.width, picture.height), scale);
    Some(crate::resample::shrink(&picture, width, height).unwrap_or(picture))
}

/// Decodes a picture scaled to at most `edge` pixels a side.
pub(crate) fn thumbnail(data: &[u8], edge: u32) -> Option<Picture> {
    decode_picture(data, (edge, edge))
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

fn free_path(attachment: &Attachment) -> Result<PathBuf, AttachmentError> {
    let dirs = directories::UserDirs::new().ok_or(AttachmentError::NoDownloadsDir)?;
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
        .ok_or_else(|| AttachmentError::TooManyNamed {
            name: name.clone(),
            dir: dir.clone(),
        })
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

        let sent = scale::for_sending("image/jpeg", tagged, false).data;
        assert!(!sent.windows(4).any(|w| w == b"Exif"));
        let shown = image::load_from_memory(&sent).unwrap();
        assert_eq!(
            (shown.width(), shown.height()),
            (1, 2),
            "turned as the EXIF said"
        );

        let text = b"not a picture".to_vec();
        assert_eq!(
            scale::for_sending("text/plain", text.clone(), false).data,
            text
        );
        assert_eq!(
            scale::for_sending("image/jpeg", text.clone(), false).data,
            text
        );
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
        assert!(matches!(
            decrypt(&pointer, data),
            Err(AttachmentError::Decrypt(_))
        ));
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

    /// The viewer gets pictures as large as they are, up to its limit, which keeps their
    /// shape and tells their real size; bombs are refused as for the bubbles.
    #[test]
    fn viewed_pictures_are_whole_up_to_the_limit() {
        let png = |width, height| {
            let mut png = Vec::new();
            image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 255]))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        };
        let full = decode_full(&png(1600, 1200)).unwrap();
        assert_eq!((full.pixels.width, full.pixels.height), (1600, 1200));
        assert_eq!((full.width, full.height), (1600, 1200));
        let large = decode_full(&png(8192, 2048)).unwrap();
        assert_eq!((large.pixels.width, large.pixels.height), (VIEW_EDGE, 1024));
        assert_eq!((large.width, large.height), (8192, 2048));
        // A flat colour stays that colour: no fringes, nothing see-through.
        assert!(
            large
                .pixels
                .rgba
                .chunks(4)
                .all(|p| p == [200, 100, 50, 255])
        );
        assert!(decode_full(b"not an image").is_none());
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    /// A picture's pointer that no CDN can serve: without its location, fetching it fails
    /// before any request, so a test only passes on what the cache has.
    fn unfetchable(id: &str) -> Attachment {
        let mut attachment = describe(
            &AttachmentPointer {
                content_type: Some("image/png".into()),
                size: Some(1234),
                ..Default::default()
            },
            false,
        );
        attachment.id = AttachmentId(id.into());
        attachment
    }

    fn with_cache<T>(test: impl AsyncFnOnce(Rc<Downloads>, Arc<Cache>) -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::open(dir.path(), &zeroize::Zeroizing::new([5; 32])).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let downloads = Downloads::new(SignalServers::Production, Some(cache.clone()));
            test(downloads, cache).await
        })
    }

    #[test]
    fn what_is_kept_is_not_downloaded_again() {
        with_cache(async |downloads, cache| {
            let attachment = unfetchable("3/kept");
            assert!(matches!(
                downloads.original(&attachment).await,
                Err(AttachmentError::Fetch(_))
            ));
            cache
                .write(&attachment.id, Entry::Original, &png(40, 30))
                .unwrap();
            assert_eq!(downloads.original(&attachment).await.unwrap(), png(40, 30));

            // Shown in a chat: decoded from the original, and the picture as shown kept.
            let (sender, events) = async_channel::unbounded();
            fetch(
                downloads.clone(),
                attachment.clone(),
                Events::new(sender.clone()),
            )
            .await;
            let Ok(Event::Attachment {
                content: Ok(AttachmentContent::Picture(first)),
                ..
            }) = events.try_recv()
            else {
                panic!("no picture");
            };
            let shown = cache
                .read(&attachment.id, Entry::Shown)
                .expect("kept as shown");
            assert_eq!(decode_shown(&shown).unwrap().rgba, first.rgba);

            // Shown again: from what was kept as shown, though the original is gone.
            cache.forget(&attachment.id);
            cache.write(&attachment.id, Entry::Shown, &shown).unwrap();
            fetch(downloads.clone(), attachment, Events::new(sender)).await;
            let Ok(Event::Attachment {
                content: Ok(AttachmentContent::Picture(again)),
                ..
            }) = events.try_recv()
            else {
                panic!("no picture the second time");
            };
            assert_eq!((again.width, again.height), (first.width, first.height));
        });
    }

    #[test]
    fn a_picture_as_shown_comes_back_as_it_was() {
        let picture = Picture {
            width: 3,
            height: 2,
            rgba: (0..24).map(|n| n * 10).collect(),
        };
        let back = decode_shown(&encode_shown(&picture).unwrap()).unwrap();
        assert_eq!((back.width, back.height, back.rgba), (3, 2, picture.rgba));
    }

    /// What a deleted or expired message had kept is found from it as stored: its attachments
    /// and link previews' pictures, also in what another device of ours sent.
    #[test]
    fn what_a_stored_message_kept_is_found() {
        use presage::libsignal_service::prelude::ProtobufMessage as _;
        use presage::proto::{Content, DataMessage, Preview, SyncMessage, sync_message};
        let photo = pointer("image/jpeg", 0);
        let mut preview = pointer("image/png", 0);
        preview.attachment_identifier = Some(AttachmentIdentifier::CdnKey("pre".into()));
        let data = DataMessage {
            attachments: vec![photo.clone()],
            preview: vec![Preview {
                image: Some(preview.clone()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let wanted = vec![id(&photo), describe_preview(&preview).id];
        let received = Content {
            content: Some(presage::proto::content::Content::DataMessage(data.clone())),
            ..Default::default()
        };
        assert_eq!(kept_of_stored(&received.encode_to_vec()), wanted);
        let sent = Content {
            content: Some(presage::proto::content::Content::SyncMessage(SyncMessage {
                content: Some(sync_message::Content::Sent(sync_message::Sent {
                    message: Some(data),
                    ..Default::default()
                })),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert_eq!(kept_of_stored(&sent.encode_to_vec()), wanted);
        assert!(kept_of_stored(b"not a message").is_empty());
    }

    #[test]
    fn what_a_chat_shows_is_downloaded_as_it_comes_and_files_wait() {
        let with = |kind: AttachmentKind, size: u64| {
            let mut attachment = unfetchable(&format!("3/{kind:?}{size}"));
            attachment.kind = kind;
            attachment.size = size;
            attachment
        };
        let preview = with(AttachmentKind::Image, 10);
        let message = crate::ChatMessage {
            attachments: vec![
                with(AttachmentKind::Image, 1000),
                with(AttachmentKind::Image, PREFETCHED_AT_MOST + 1),
                with(AttachmentKind::Sticker, 10),
                with(AttachmentKind::VoiceNote, 10),
                with(AttachmentKind::LongText, 10),
                with(AttachmentKind::File, 10),
                with(AttachmentKind::Audio, 10),
                with(AttachmentKind::Video, 10),
                with(AttachmentKind::ViewOnce, 10),
            ],
            preview: Some(crate::LinkPreview {
                url: String::new(),
                title: String::new(),
                description: String::new(),
                image: Some(Box::new(preview.clone())),
            }),
            ..Default::default()
        };
        let kinds: Vec<AttachmentKind> = prefetched(&message).iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            [
                AttachmentKind::Image,
                AttachmentKind::Sticker,
                AttachmentKind::VoiceNote,
                AttachmentKind::LongText,
                AttachmentKind::Image,
            ]
        );
    }

    #[test]
    fn a_picture_downloaded_as_it_came_is_kept_as_its_chat_shows_it() {
        with_cache(async |downloads, cache| {
            let attachment = unfetchable("3/came");
            cache
                .write(&attachment.id, Entry::Original, &png(1600, 1200))
                .unwrap();
            prefetch(downloads.clone(), attachment.clone()).await;
            let shown = cache
                .read(&attachment.id, Entry::Shown)
                .expect("kept as shown");
            let picture = decode_shown(&shown).unwrap();
            assert_eq!((picture.width, picture.height), (PICTURE_EDGE, 480));
            // Already there: nothing more is done.
            cache.forget(&attachment.id);
            cache.write(&attachment.id, Entry::Shown, &shown).unwrap();
            prefetch(downloads, attachment.clone()).await;
            assert!(cache.read(&attachment.id, Entry::Original).is_none());
        });
    }
}
