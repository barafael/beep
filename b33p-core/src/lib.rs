//! UI-agnostic Signal backend.
//!
//! A [`Backend`] runs on its own thread. The UI sends [`Command`]s and receives [`Event`]s,
//! either by awaiting them in its event loop or by blocking on a worker thread; neither side
//! ever blocks the other. All types here are plain data so any toolkit can
//! render them.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

#[cfg(feature = "signal")]
mod attachments;
#[cfg(feature = "signal")]
mod client;
#[cfg(feature = "test-servers")]
mod content_log;
mod error;
pub mod format;
#[cfg(feature = "signal")]
mod import;
#[cfg(feature = "signal")]
mod local;
mod mock;
#[cfg(feature = "signal")]
mod outbox;
#[cfg(feature = "signal")]
mod preview;
mod qr;
pub mod scratch;
pub mod scrub;
pub mod settings;
#[cfg(feature = "signal")]
mod signal;
pub mod state;
#[cfg(feature = "signal")]
mod storage;
pub mod timeline;
#[cfg(feature = "signal")]
mod transfer;
#[cfg(feature = "signal")]
mod vault;
#[cfg(feature = "signal")]
mod wire;

pub use error::{Error, ErrorKind};
pub use qr::QrMatrix;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

/// Opaque, stable identifier of a 1:1 chat or group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConversationId(pub String);

impl ConversationId {
    /// The 1:1 conversation with the person whose service ID is `service_id`, e.g. a message's
    /// `sender_id`.
    pub fn contact(service_id: &str) -> Self {
        Self(format!("c:{service_id}"))
    }

    /// The group whose master key is `master_key`.
    pub fn group(master_key: &[u8]) -> Self {
        let key: String = master_key.iter().map(|b| format!("{b:02x}")).collect();
        Self(format!("g:{key}"))
    }

    /// The service ID of the person a 1:1 conversation is with.
    pub fn contact_id(&self) -> Option<&str> {
        self.0.strip_prefix("c:")
    }

    /// A group conversation's master key, in hex.
    pub fn group_key(&self) -> Option<&str> {
        self.0.strip_prefix("g:")
    }
}

/// What a conversation is: a 1:1 chat with a contact (or with ourselves, "Note to Self"),
/// or a group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConversationKind {
    /// A 1:1 chat with a contact, or with ourselves ("Note to Self").
    #[default]
    Contact,
    Group,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    pub id: ConversationId,
    pub title: String,
    pub kind: ConversationKind,
    /// A line under the title: a phone number, a group's size, "Messages to yourself".
    pub subtitle: String,
    pub settings: ChatSettings,
    /// Disappearing messages: seconds a message lasts once read (incoming) or sent; 0 is off.
    /// Signal keeps it with the contact or group, and every change comes as a message.
    pub expire_timer: u32,
    /// Messages to ourselves ("Note to Self"), listed even before there are any.
    pub note_to_self: bool,
    /// A group's other members, who can be mentioned.
    pub members: Vec<Person>,
    /// A group we are an administrator of.
    pub admin: bool,
    /// A group we are no longer in: it can be read, not written to.
    pub left: bool,
    /// A group we are invited to (and not in yet): it can be accepted or declined.
    pub invited: bool,
    /// A group we asked to join through its invite link, waiting for an administrator.
    pub requested: bool,
    /// How far a contact's safety number is trusted; nothing can be written to them while it
    /// [`Trust::Changed`]. Groups and Note to Self: [`Trust::Unverified`].
    pub trust: Trust,
}

impl Conversation {
    /// A group.
    pub fn is_group(&self) -> bool {
        self.kind == ConversationKind::Group
    }
}

/// What to do with someone found by phone number; see [`Command::FindNumber`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterFinding {
    /// Open the chat with them.
    Open,
    /// Choose them for the group being made or added to.
    Choose,
}

/// How far the safety number with a contact is trusted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Trust {
    /// Not compared, or cleared.
    #[default]
    Unverified,
    /// Compared with theirs and marked verified, for the key they have now.
    Verified,
    /// Verified before, but their key changed since: confirm (or verify again) before writing
    /// to them, as Signal asks.
    Changed,
}

/// A change to a group; see [`Command::EditGroup`]. Members are service IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupEdit {
    Title(String),
    /// Empty clears it.
    Description(String),
    /// The group's picture, from an image file.
    SetPicture(std::path::PathBuf),
    RemovePicture,
    AddMembers(Vec<String>),
    RemoveMember(String),
    /// Makes a member an administrator, or takes that back.
    SetAdmin(String, bool),
    Leave,
    /// Joins a group we were invited to.
    AcceptInvitation,
    /// Turns an invitation down.
    DeclineInvitation,
    /// Turns the group's invite link on, for anyone or with an admin's approval, or off.
    InviteLink(InviteLinkAccess),
    /// Makes a new link; the one given out so far stops working.
    ResetInviteLink,
    /// Lets someone in who asked to join through the link (a service ID).
    ApproveRequest(String),
    /// Turns their request down.
    DenyRequest(String),
    /// Takes back our own request to join; see [`Command::JoinGroup`].
    CancelJoinRequest,
}

/// Who a group's invite link lets in; see [`GroupEdit::InviteLink`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InviteLinkAccess {
    #[default]
    Off,
    /// Anyone with the link joins at once.
    Anyone,
    /// Anyone with the link may ask; an administrator lets them in.
    Approval,
}

/// What a group's invite link shows of it before joining; see [`Command::PreviewGroupLink`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupPreview {
    pub link: String,
    pub title: String,
    pub members: u32,
    /// An administrator has to let new members in.
    pub approval: bool,
    /// We asked to join already, and wait.
    pub requested: bool,
}

/// More about a conversation, for its details page; see [`Command::LoadDetails`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Details {
    /// Their profile's "about" line, with its emoji.
    pub about: Option<String>,
    pub phone: Option<String>,
    /// The safety number (60 digits) of a 1:1 conversation, to compare in person or by
    /// another channel; `None` if there is no session yet.
    pub safety_number: Option<String>,
    /// How far that safety number is trusted: verified (here or on another device) for the key
    /// they have now, or changed since it was.
    pub trust: Trust,
    /// A group's description.
    pub description: Option<String>,
    /// A group's members, with whether each is an admin.
    pub members: Vec<(Person, bool)>,
    /// A group's invite link, if it is turned on: anyone with it can join, or ask to.
    pub invite_link: Option<String>,
    pub invite_link_access: InviteLinkAccess,
    /// Who asked to join through the link, for an administrator to let in or not.
    pub requests: Vec<Person>,
}

/// A chat folder, as set up on the phone: a tab of the chat list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatFolder {
    pub name: String,
    pub only_unread: bool,
    pub show_muted: bool,
    /// Every 1:1 chat, unless excluded.
    pub all_individual: bool,
    /// Every group, unless excluded.
    pub all_groups: bool,
    pub included: Vec<ConversationId>,
    pub excluded: Vec<ConversationId>,
}

/// A story: shown for a day to the author's contacts, or to a group.
#[derive(Clone, Debug, PartialEq)]
pub struct Story {
    pub author_id: String,
    pub author: String,
    /// The author's chat, or the group it was posted to.
    pub conversation: ConversationId,
    pub timestamp: u64,
    pub content: StoryContent,
    pub viewed: bool,
    pub outgoing: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StoryContent {
    Picture(Attachment),
    /// Text on a coloured background (colours as 0xAARRGGBB).
    Text {
        text: String,
        foreground: u32,
        background: u32,
    },
}

/// A story to post; see [`Command::SendStory`].
#[derive(Clone, Debug, PartialEq)]
pub enum NewStory {
    Text(String),
    Picture(std::path::PathBuf),
}

/// An installed sticker pack, for the sticker picker.
#[derive(Clone, Debug, PartialEq)]
pub struct StickerPack {
    /// Hex, as Signal shows pack ids.
    pub id: String,
    pub title: String,
    pub stickers: Vec<StickerItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StickerItem {
    pub id: u32,
    /// The emoji it stands for.
    pub emoji: String,
    /// Small, for the picker.
    pub picture: Arc<Picture>,
}

/// What to do with a message request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestResponse {
    /// Accept the chat, and share our profile with them.
    Accept,
    /// Delete the chat and what is in it.
    Delete,
    /// Block them, keeping the chat.
    Block,
}

/// Someone who can be mentioned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub service_id: String,
    pub name: String,
}

/// What b33p remembers about a conversation besides its messages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatSettings {
    /// Position among the pinned chats, from 0; pinned chats come first.
    pub pinned: Option<u32>,
    /// Muted until then (ms since the epoch; `u64::MAX` for good): no notifications.
    pub muted_until: Option<u64>,
    pub archived: bool,
    /// Incoming messages up to this time (ms) were read, here or on another device.
    pub last_read: u64,
    /// Marked unread by hand.
    pub marked_unread: bool,
    /// Nothing new from it is shown.
    pub blocked: bool,
    /// Accepted as a chat, not a message request: here, on the phone, or by writing to it.
    pub accepted: bool,
}

impl ChatSettings {
    pub fn muted(&self, now: u64) -> bool {
        self.muted_until.is_some_and(|until| until > now)
    }

    /// Archived chats aren't pinned, and pinned ones aren't archived.
    pub fn set_archived(&mut self, archived: bool) {
        self.archived = archived;
        if archived {
            self.pinned = None;
        }
    }

    pub fn set_pinned(&mut self, position: Option<u32>) {
        self.pinned = position;
        if position.is_some() {
            self.archived = false;
        }
    }
}

/// Preferences the backend acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    /// Tell senders when their messages were read.
    pub read_receipts: bool,
    /// Show others when we are typing.
    pub typing_indicators: bool,
    /// Add a preview of the first link to messages we send. The linked site then sees our
    /// address, so it is off unless turned on.
    pub link_previews: bool,
    /// Write pins, mutes, the archive, blocks and accepted requests back to the storage
    /// service, so the phone (and other devices) get them.
    pub sync_to_phone: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            read_receipts: true,
            typing_indicators: true,
            link_previews: false,
            sync_to_phone: false,
        }
    }
}

/// How far one of our messages got. Ordered: a later state implies the earlier ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Delivery {
    #[default]
    Sent = 0,
    Delivered = 1,
    Read = 2,
}

impl Delivery {
    #[cfg(feature = "signal")]
    pub(crate) fn from_index(index: i64) -> Self {
        match index {
            2 => Self::Read,
            1 => Self::Delivered,
            _ => Self::Sent,
        }
    }
}

/// A mention in text being sent: `body[start..end]` reads "@Name" and stands for `service_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    pub start: usize,
    pub end: usize,
    pub service_id: String,
}

/// Formatting of part of a message's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Bold,
    Italic,
    Strikethrough,
    Monospace,
    /// Hidden until revealed.
    Spoiler,
}

/// A formatted range of a message's text, in bytes of [`ChatMessage::body`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleRange {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// The mock backend's voice message (a WAV file), e.g. for tests of playing audio.
pub fn mock_voice_message() -> Vec<u8> {
    mock::tone_wav()
}

/// A disappearing-message timer as people say it: "5 minutes", "1 week".
pub fn format_timer(seconds: u32) -> String {
    const UNITS: [(u32, &str); 5] = [
        (7 * 24 * 3600, "week"),
        (24 * 3600, "day"),
        (3600, "hour"),
        (60, "minute"),
        (1, "second"),
    ];
    let (size, unit) = UNITS
        .into_iter()
        .find(|(size, _)| seconds >= *size && seconds.is_multiple_of(*size))
        .unwrap_or((1, "second"));
    match seconds / size {
        1 => format!("1 {unit}"),
        n => format!("{n} {unit}s"),
    }
}

/// What a message changing the disappearing-message timer says, `who` being "You" or a name.
/// Windows draw a timer beside it ([`ChatMessage::timer_change`] says it is one).
pub fn timer_notice(who: &str, seconds: u32) -> String {
    match seconds {
        0 => format!("{who} turned off disappearing messages."),
        seconds => format!(
            "{who} set disappearing messages to {}.",
            format_timer(seconds)
        ),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatMessage {
    pub conversation: ConversationId,
    /// Stable identity of the author (a Signal service ID); `sender` is only a display name.
    pub sender_id: String,
    pub sender: String,
    pub body: String,
    /// Milliseconds since the Unix epoch, as used by Signal.
    pub timestamp: u64,
    pub outgoing: bool,
    /// The message this one replies to.
    pub quote: Option<Quote>,
    pub attachments: Vec<Attachment>,
    /// At most one per person, in the order they came.
    pub reactions: Vec<Reaction>,
    /// For our own messages.
    pub delivery: Delivery,
    /// When it was last edited, if it was.
    pub edited_at: Option<u64>,
    /// Earlier versions known here, oldest first, with when they were replaced. Others' edits
    /// replace the stored text, so this can be shorter than the history, or empty.
    pub edits: Vec<(u64, String)>,
    /// Deleted for everyone: shown as such, without content.
    pub deleted: bool,
    /// When a disappearing message goes (ms since the epoch); `None` if it doesn't, or its
    /// timer hasn't started (it starts when read).
    pub expires_at: Option<u64>,
    /// Its disappearing timer in seconds, if it has one.
    pub expire_timer: Option<u32>,
    /// It is the notice that the conversation's timer changed, to this many seconds.
    pub timer_change: Option<u32>,
    /// A preview of the link in it, if the sender made one.
    pub preview: Option<LinkPreview>,
    /// Formatting, in [`ChatMessage::body`]'s bytes.
    pub styles: Vec<StyleRange>,
    /// Whom it mentions, as "@Name" in [`ChatMessage::body`].
    pub mentions: Vec<Mention>,
    /// Ours, shown before it went out: still sending, or it failed. `None` once it is sent,
    /// and for everything else.
    pub unsent: Option<Unsent>,
    /// A line about the conversation that nobody wrote, e.g. that a contact's safety number
    /// changed: not notified, not counted as unread, nothing to reply or react to.
    pub notice: bool,
}

/// One of our messages that hasn't gone out (yet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsent {
    Sending,
    /// It didn't go out; sending it again may work.
    Failed,
    /// Signal refused it (the recipient left Signal, it is too large, …): sending it again as
    /// it is won't work.
    Refused,
}

impl ChatMessage {
    /// How Signal refers to this message, e.g. from reactions.
    pub fn reference(&self) -> MessageRef {
        MessageRef {
            author_id: self.sender_id.clone(),
            timestamp: self.timestamp,
        }
    }

    /// Whether `target` refers to this message.
    pub fn is(&self, target: &MessageRef) -> bool {
        self.timestamp == target.timestamp && self.sender_id == target.author_id
    }
}

/// What a link in a message points to, as the sender's app found it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkPreview {
    pub url: String,
    pub title: String,
    pub description: String,
    pub image: Option<Attachment>,
}

/// Identifies a message the way Signal does: its author and sent time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub author_id: String,
    pub timestamp: u64,
}

/// Someone's emoji reaction to a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub sender_id: String,
    /// Display name.
    pub sender: String,
    /// Ours.
    pub outgoing: bool,
}

/// Puts a reaction on, or takes it off, the message it targets among `messages`. Each person
/// has at most one reaction per message; a new one replaces theirs. Returns whether a message
/// changed.
pub(crate) fn apply_reaction(
    messages: &mut [ChatMessage],
    target: &MessageRef,
    reaction: Reaction,
    remove: bool,
) -> bool {
    let Some(message) = messages.iter_mut().rev().find(|m| m.is(target)) else {
        return false;
    };
    let before = message.reactions.len();
    message
        .reactions
        .retain(|r| r.sender_id != reaction.sender_id);
    if remove {
        message.reactions.len() != before
    } else {
        message.reactions.push(reaction);
        true
    }
}

/// What a reply shows of the message it answers, and which one that is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quote {
    /// Display name of the quoted message's author.
    pub author: String,
    pub text: String,
    /// The quoted message, as Signal identifies it: author and sent time. Empty and 0 if the
    /// sender didn't say.
    pub author_id: String,
    pub timestamp: u64,
}

/// A decoded image: an avatar, an attached picture or a sticker. RGBA, row-major, 4 bytes
/// per pixel.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Picture({}x{})", self.width, self.height)
    }
}

/// Identifies an attachment across events; see [`Attachment::id`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AttachmentId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    VoiceNote,
    Sticker,
    /// The full text of a long message; the message body holds only its beginning.
    LongText,
    /// Meant to be viewed once, on the phone.
    ViewOnce,
    File,
}

/// A file attached to a message. Signal keeps attachments on its servers for about 45 days;
/// they are downloaded on demand, never stored unencrypted except when saved by the user.
#[derive(Clone, PartialEq)]
pub struct Attachment {
    pub id: AttachmentId,
    pub kind: AttachmentKind,
    pub content_type: String,
    pub file_name: Option<String>,
    /// In bytes.
    pub size: u64,
    /// In pixels, if the sender said; 0 otherwise.
    pub width: u32,
    pub height: u32,
    /// A blurred stand-in to show before it's downloaded (a BlurHash), if the sender made one.
    pub blurhash: Option<String>,
    pub caption: Option<String>,
    /// A video meant to loop silently, as GIFs are sent in Signal.
    pub gif: bool,
    /// Where and how to download it, opaque to UIs (an encoded Signal attachment pointer).
    pub(crate) pointer: Arc<[u8]>,
}

impl std::fmt::Debug for Attachment {
    // Leaves out the pointer, which holds the attachment's key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("content_type", &self.content_type)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// A downloaded attachment, as far as UIs can show it; see [`Event::Attachment`].
#[derive(Clone, Debug, PartialEq)]
pub enum AttachmentContent {
    /// Images and stickers, scaled down for display.
    Picture(Arc<Picture>),
    /// Long message text.
    Text(String),
    /// Audio and voice messages, as downloaded (decoded by the player).
    Audio(Arc<[u8]>),
}

/// Stages of the message history transfer; see [`Event::HistoryTransfer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryTransfer {
    /// Waiting for the phone to upload the history.
    Waiting,
    Downloading,
    /// This many messages imported so far.
    Importing {
        messages: usize,
    },
    /// Done, failed or skipped; errors arrive separately.
    Finished,
}

/// What the start-up screen is waiting for, until the first [`Event::Synced`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStage {
    /// Opening the local store.
    Starting,
    Connecting,
    Transfer(HistoryTransfer),
    /// Connected; the messages that arrived meanwhile are coming in, this many so far.
    CatchingUp {
        messages: usize,
    },
}

/// A user-entered password. Wiped from memory on drop and never printed.
#[derive(Clone)]
pub struct Password(zeroize::Zeroizing<String>);

impl Password {
    pub fn new(password: impl Into<String>) -> Self {
        Self(zeroize::Zeroizing::new(password.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(***)")
    }
}

/// Why the backend can't go on; see [`Event::Failed`]. What the user can do about it differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The system keyring holding the database key can't be reached or is locked. The
    /// backend keeps trying by itself, e.g. at login before the keyring is unlocked.
    KeyringUnavailable,
    /// First start, and the system keyring is locked: wait for it, or use a password.
    KeyringLocked,
    /// The keyring is there, but the database key isn't in it.
    KeyMissing,
    /// A database whose key was never recorded.
    NoKeyRecorded,
    /// The data was written by a newer b33p.
    StoreTooNew,
    /// The data was written by a build this one can't read.
    StoreIncompatible,
    /// Another b33p has the same data open.
    AlreadyRunning,
    /// Linking this device didn't work (no connection, or the code expired).
    LinkFailed,
    Other,
    /// The backend is gone (it panicked); only restarting helps. Said by the UI, which notices.
    Stopped,
}

impl Failure {
    /// Whether deleting the local data and linking again ([`Command::StartOver`]) is offered.
    /// Not where waiting can help, nor where there is nothing to lose by trying again.
    pub fn can_start_over(self) -> bool {
        !matches!(
            self,
            Self::KeyringUnavailable | Self::KeyringLocked | Self::AlreadyRunning | Self::Stopped
        )
    }

    /// Whether trying again ([`Command::Retry`]) is offered.
    pub fn can_retry(self) -> bool {
        self != Self::Stopped
    }

    /// Whether protecting the data with a password instead ([`Command::UsePassword`]) is
    /// offered.
    pub fn can_use_password(self) -> bool {
        self == Self::KeyringLocked
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The backend can't go on until the user answers with [`Command::Retry`],
    /// [`Command::StartOver`] or [`Command::UsePassword`]; `message` says why, for the user.
    Failed {
        failure: Failure,
        message: String,
    },
    /// No system keyring is available: the local database is protected by a password.
    /// `new` means none has been chosen yet; `error` explains why the last attempt failed.
    PasswordRequired {
        new: bool,
        error: Option<String>,
    },
    /// Not linked yet: show this QR code to be scanned from the phone.
    LinkQr(QrMatrix),
    /// Linked (or loaded an existing registration) as `account`.
    Linked {
        account: String,
    },
    /// Lost the connection to Signal and retrying; [`Event::Synced`] follows once back.
    Reconnecting,
    /// Connected to Signal; what arrived meanwhile comes next, then [`Event::Synced`].
    Connected,
    /// Progress of the message history transfer right after linking.
    HistoryTransfer(HistoryTransfer),
    /// Initial message queue drained; it is now safe to send.
    Synced,
    /// New or changed conversations, merged by id.
    Conversations(Vec<Conversation>),
    /// Profile or group picture of a conversation, replacing any earlier one; `None` once it
    /// has none.
    Avatar {
        conversation: ConversationId,
        avatar: Option<Arc<Picture>>,
    },
    /// Someone reacted to a message, or took their reaction back.
    Reaction {
        conversation: ConversationId,
        target: MessageRef,
        reaction: Reaction,
        remove: bool,
    },
    /// Answer to [`Command::FetchAttachment`].
    Attachment {
        id: AttachmentId,
        content: Result<AttachmentContent, Error>,
    },
    /// Answer to [`Command::SaveAttachment`]: where the file was written.
    AttachmentSaved {
        id: AttachmentId,
        path: Result<std::path::PathBuf, Error>,
    },
    /// Our messages sent at `timestamps` in `conversation` got this far.
    Delivered {
        conversation: ConversationId,
        timestamps: Vec<u64>,
        delivery: Delivery,
    },
    /// Someone started or stopped typing. Stops by itself after a while if no stop arrives.
    Typing {
        conversation: ConversationId,
        sender_id: String,
        sender: String,
        typing: bool,
    },
    /// A message was edited: `message` is its new version, with the old ones in `edits`.
    Edited {
        conversation: ConversationId,
        target: MessageRef,
        message: ChatMessage,
    },
    /// A message was deleted for everyone.
    Deleted {
        conversation: ConversationId,
        target: MessageRef,
    },
    /// Disappearing messages started counting down (ms since the epoch when each goes).
    Expiring {
        conversation: ConversationId,
        expiries: Vec<(MessageRef, u64)>,
    },
    /// Disappearing messages are gone.
    Expired {
        conversation: ConversationId,
        targets: Vec<MessageRef>,
    },
    /// A conversation's settings changed (pinned, muted, read on another device, …).
    Settings {
        conversation: ConversationId,
        settings: ChatSettings,
    },
    /// Messages of a conversation containing `query`, newest first, answering
    /// [`Command::SearchChat`].
    SearchResults {
        conversation: ConversationId,
        query: String,
        messages: Vec<ChatMessage>,
    },
    /// Someone found by phone number (see [`Command::FindNumber`]), and their chat.
    Found {
        conversation: ConversationId,
        then: AfterFinding,
    },
    /// How far the safety number with a contact is trusted now: after it was marked verified,
    /// or cleared, here or on another device, or their key changed.
    Trust {
        conversation: ConversationId,
        trust: Trust,
    },
    /// Answers [`Command::LoadDetails`].
    Details {
        conversation: ConversationId,
        details: Details,
    },
    /// A conversation's messages with attachments or links, newest first, answering
    /// [`Command::LoadMedia`].
    Media {
        conversation: ConversationId,
        messages: Vec<ChatMessage>,
    },
    /// The stories of the last day, all of them (replacing those known before).
    Stories(Vec<Story>),
    /// A group made with [`Command::CreateGroup`] is there.
    GroupCreated(ConversationId),
    /// Answers [`Command::PreviewGroupLink`].
    GroupPreview(GroupPreview),
    /// A group joined through its link with [`Command::JoinGroup`] is there; if its
    /// administrators have to let us in, the conversation says we asked.
    GroupJoined(ConversationId),
    /// The chat folders, in their order (replacing those known before).
    Folders(Vec<ChatFolder>),
    /// The installed sticker packs, answering [`Command::LoadStickers`].
    StickerPacks(Vec<StickerPack>),
    /// A sticker pack was installed or removed on another device: the packs, if gathered,
    /// are gathered again.
    StickerPacksChanged,
    /// A conversation's stored messages were deleted (here, or on another device).
    Cleared(ConversationId),
    /// What the phone keeps in the storage service has been read, so chats it didn't accept
    /// can be told apart as message requests.
    StorageRead,
    /// The backend's preferences, after the phone changed the account's settings, for the UI
    /// to keep.
    Preferences(Preferences),
    /// Unread incoming messages per conversation, counted from the store at start.
    Unread(Vec<(ConversationId, u32)>),
    /// A page of a conversation's stored history, answering [`Command::LoadHistory`]: the
    /// messages sent before the time asked for, oldest first.
    History {
        conversation: ConversationId,
        messages: Vec<ChatMessage>,
        /// Reactions in this page to messages not in it (older ones, not loaded yet).
        reactions: Vec<(MessageRef, Reaction, bool)>,
        /// Where the next older page starts; `None` once the beginning is reached.
        older: Option<u64>,
    },
    /// A conversation's newest stored message, so it can be sorted and previewed before its
    /// history is loaded. Unlike [`Event::Message`], it is not new and not unread.
    Latest(ChatMessage),
    Message(ChatMessage),
    /// Our messages that hadn't gone out when b33p last stopped, each a [`Command::Send`] to
    /// send again: shown as failed, as if [`Event::SendFailed`] had come for them.
    Interrupted(Vec<Command>),
    /// Sending the message the UI showed at `timestamp` failed; [`Event::CommandFailed`]
    /// says why. `retryable`: sending it again may work (see [`Error::retryable`]); if not,
    /// Signal refused it as it is.
    SendFailed {
        conversation: ConversationId,
        timestamp: u64,
        retryable: bool,
    },
    /// Something failed that no command asked for, e.g. receiving.
    Error(Error),
    /// Carrying out `command` failed; the UI may offer to try it again if the error says that
    /// may work.
    CommandFailed {
        error: Error,
        command: Command,
    },
}

#[derive(Clone, Debug)]
pub enum Command {
    /// Answer to [`Event::PasswordRequired`].
    Unlock(Password),
    /// After [`Event::Failed`]: try again.
    Retry,
    /// After [`Event::Failed`], or instead of a forgotten password: deletes the messages and
    /// keys stored here (the settings stay), and starts again with linking.
    StartOver,
    /// After [`Failure::KeyringLocked`]: protect the data with a password instead.
    UsePassword,
    /// Removes this device from the account, deletes what is stored here as
    /// [`Command::StartOver`] does, and starts again with linking.
    Unlink,
    /// How this computer appears under Linked devices on the phone. While a linking code is
    /// shown, it is replaced by one for the new name.
    SetDeviceName(String),
    /// A page of stored history: the newest messages, or those sent before `before`.
    LoadHistory {
        conversation: ConversationId,
        before: Option<u64>,
    },
    /// Stop waiting for the phone to send the message history, and continue without it.
    SkipHistory,
    /// Download an image, sticker or long text for display; see [`Event::Attachment`].
    FetchAttachment(Attachment),
    /// Download an attachment into the user's downloads folder; see [`Event::AttachmentSaved`].
    SaveAttachment(Attachment),
    /// `quote` makes it a reply to that message; `files` are attached (the body may then be
    /// empty).
    Send {
        conversation: ConversationId,
        /// Chosen by the UI, which shows the message while it is sent; the sent message
        /// ([`Event::Message`]) or [`Event::SendFailed`] has the same.
        timestamp: u64,
        body: String,
        quote: Option<Quote>,
        files: Vec<std::path::PathBuf>,
        /// Formatting, in `body`'s bytes.
        styles: Vec<StyleRange>,
        /// People mentioned: where "@Name" stands in `body` (bytes), and who it is.
        mentions: Vec<Mention>,
    },
    /// Drops one of our messages that didn't go out (see [`Event::Interrupted`]), which the
    /// user deleted: it isn't shown as failed again.
    DropUnsent {
        conversation: ConversationId,
        timestamp: u64,
    },
    /// Looks for messages containing `query` (ignoring case) in a conversation's whole stored
    /// history; see [`Event::SearchResults`].
    SearchChat {
        conversation: ConversationId,
        query: String,
    },
    /// A story was seen: told to its author (if read receipts are on) and our other devices.
    ViewStory(MessageRef),
    /// Posts a story to a group.
    SendStory {
        conversation: ConversationId,
        story: NewStory,
    },
    /// Makes a group with us as its administrator and these members (service IDs); see
    /// [`Event::GroupCreated`].
    CreateGroup {
        title: String,
        members: Vec<String>,
    },
    /// Changes a group: its name, description or members, or leaves it.
    EditGroup {
        conversation: ConversationId,
        edit: GroupEdit,
    },
    /// Looks up what a `https://signal.group/#…` link shows of its group; see
    /// [`Event::GroupPreview`]. An error if it isn't such a link, or the link is turned off.
    PreviewGroupLink(String),
    /// Joins the group behind the link, or asks to; see [`Event::GroupJoined`].
    JoinGroup(String),
    /// Gathers the installed sticker packs; see [`Event::StickerPacks`].
    LoadStickers,
    /// Sends a sticker from an installed pack.
    SendSticker {
        conversation: ConversationId,
        pack: String,
        sticker: u32,
    },
    /// Blocks a conversation, or unblocks it; told to our other devices.
    Block {
        conversation: ConversationId,
        blocked: bool,
    },
    /// Answers a message request; told to our other devices.
    Respond {
        conversation: ConversationId,
        response: RequestResponse,
    },
    /// Gathers what the details page shows; see [`Event::Details`].
    LoadDetails(ConversationId),
    /// Finds who a phone number (as typed: international, or in our own country's way) belongs
    /// to on Signal, makes a chat with them, and answers with [`Event::Found`]; an error if
    /// nobody has it.
    FindNumber {
        number: String,
        then: AfterFinding,
    },
    /// Marks the safety number with a contact as verified (compared with theirs), or clears
    /// that (which also accepts a changed one); tells our other devices, and answers with
    /// [`Event::Trust`] if it changed.
    SetVerified {
        conversation: ConversationId,
        verified: bool,
    },
    /// Gathers a conversation's media, files and links from its whole stored history; see
    /// [`Event::Media`].
    LoadMedia(ConversationId),
    /// Sends a copy of a message (its text and attachments) to `conversation`.
    Forward {
        conversation: ConversationId,
        body: String,
        styles: Vec<StyleRange>,
        attachments: Vec<Attachment>,
        /// The message forwarded: its conversation and timestamp. A sticker goes on as the
        /// sticker it is, which only the stored message knows.
        original: (ConversationId, u64),
    },
    /// Downloads an attachment and opens it with the desktop's default app for its type.
    OpenAttachment(Attachment),
    /// We started or stopped typing in a conversation.
    Typing {
        conversation: ConversationId,
        typing: bool,
    },
    /// These incoming messages were seen: tells their senders (if read receipts are on) and
    /// our other devices, and starts the timers of those that disappear (seconds).
    MarkRead {
        conversation: ConversationId,
        messages: Vec<MessageRef>,
        expiring: Vec<(MessageRef, u32)>,
    },
    /// Marks a conversation unread by hand (reading it clears that; see `MarkRead`).
    MarkUnread(ConversationId),
    /// Replaces the text of one of our messages.
    Edit {
        conversation: ConversationId,
        target: MessageRef,
        body: String,
        styles: Vec<StyleRange>,
        mentions: Vec<Mention>,
    },
    /// Deletes a message here, or for everyone (only our own, within a day).
    Delete {
        conversation: ConversationId,
        target: MessageRef,
        for_everyone: bool,
    },
    /// Pins exactly these conversations, in this order.
    SetPins(Vec<ConversationId>),
    /// Mutes until then (ms since the epoch; `u64::MAX` for good), or unmutes.
    Mute {
        conversation: ConversationId,
        until: Option<u64>,
    },
    Archive {
        conversation: ConversationId,
        archived: bool,
    },
    /// Sets the disappearing-message timer (seconds; 0 turns it off).
    SetTimer {
        conversation: ConversationId,
        seconds: u32,
    },
    Preferences(Preferences),
    /// Reacts to a message with `emoji`, or takes our reaction back.
    React {
        conversation: ConversationId,
        target: MessageRef,
        emoji: String,
        remove: bool,
    },
}

pub struct Config {
    /// Name shown in the phone's "Linked devices" list.
    pub device_name: String,
    /// What the backend does on our behalf from the start, before (and whether or not) a
    /// [`Command::Preferences`] changes it: the password prompt and linking come first.
    pub preferences: Preferences,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: "b33p".into(),
            preferences: Preferences::default(),
        }
    }
}

/// Backend side of the event channel. It is unbounded, so sending never blocks or awaits.
#[derive(Clone)]
pub(crate) struct Events {
    sender: async_channel::Sender<Event>,
    /// Whether we said the UI has gone, so we say it once.
    told_gone: Arc<AtomicBool>,
}

impl Events {
    pub(crate) fn new(sender: async_channel::Sender<Event>) -> Self {
        Self {
            sender,
            told_gone: Arc::default(),
        }
    }

    /// Sends `event`, or drops it once the UI has gone away: nobody is left to show it.
    pub(crate) fn send(&self, event: Event) {
        self.try_send(event);
    }

    /// Like [`Events::send`], but returns `false` once the UI has gone away, for loops that
    /// should stop then.
    #[cfg(feature = "signal")]
    #[must_use]
    pub(crate) fn sent(&self, event: Event) -> bool {
        self.try_send(event)
    }

    fn try_send(&self, event: Event) -> bool {
        let sent = self.sender.try_send(event).is_ok();
        if !sent && !self.told_gone.swap(true, Ordering::Relaxed) {
            tracing::debug!("the UI has gone; dropping events");
        }
        sent
    }
}

/// What travels to the backend: UI commands, then possibly the order to stop.
enum Request {
    // Boxed: commands are big, and few.
    Command(Box<Command>),
    Shutdown,
}

/// Backend side of the command channel.
pub(crate) struct Inbox(Receiver<Request>);

impl Inbox {
    /// The next command; `None` once the backend should stop.
    pub(crate) fn recv(&self) -> Option<Command> {
        match self.0.recv() {
            Ok(Request::Command(command)) => Some(*command),
            Ok(Request::Shutdown) | Err(_) => None,
        }
    }

    /// Like [`Inbox::recv`], but gives up after `timeout`.
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Command, RecvTimeoutError> {
        match self.0.recv_timeout(timeout)? {
            Request::Command(command) => Ok(*command),
            Request::Shutdown => Err(RecvTimeoutError::Disconnected),
        }
    }

    /// Commands until the backend should stop.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Command> + '_ {
        std::iter::from_fn(|| self.recv())
    }
}

/// Cheap, cloneable handle for sending commands, e.g. from UI callbacks. It doesn't keep the
/// backend alive; the [`Backend`] does.
#[derive(Clone)]
pub struct Commands(Sender<Request>);

impl Commands {
    pub fn send(&self, command: Command) {
        // The backend only goes away when it has failed fatally, which it reports as an event,
        // or when it was shut down.
        self.0.send(Request::Command(Box::new(command))).ok();
    }
}

/// Owner of a running backend. Dropping it stops the backend and waits until it has closed its
/// database, so drop it before the process exits: library teardown in `exit()` crashes threads
/// still using the database.
pub struct Backend {
    commands: Commands,
    events: async_channel::Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Backend {
    /// Starts the real Signal backend.
    #[cfg(feature = "signal")]
    pub fn start(config: Config) -> Self {
        Self::spawn(move |inbox, events| signal::run(config, inbox, events))
    }

    /// Starts an offline backend with canned data, for UI work.
    pub fn mock() -> Self {
        Self::spawn(mock::run)
    }

    /// Whether [`Backend::from_env`] runs the mock. It sends nothing, so UIs should say so.
    /// Debug builds only: in a release, a stray variable in the environment must not turn the
    /// app into a convincing fake that swallows messages.
    pub fn mock_requested() -> bool {
        !cfg!(feature = "signal")
            || (cfg!(debug_assertions) && std::env::var_os("B33P_MOCK").is_some())
    }

    /// Starts the real backend, or the mock one when [`Backend::mock_requested`].
    pub fn from_env(config: Config) -> Self {
        #[cfg(feature = "signal")]
        if !Self::mock_requested() {
            return Self::start(config);
        }
        drop(config);
        Self::mock()
    }

    fn spawn(run: impl FnOnce(Inbox, Events) + Send + 'static) -> Self {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = async_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("b33p-backend".into())
            // presage/libsignal futures are huge and the runtime is current-thread, so they all
            // live on this stack; the 2 MiB default overflows in debug builds.
            .stack_size(64 * 1024 * 1024)
            .spawn(move || run(Inbox(command_rx), Events::new(event_tx)))
            .expect("failed to spawn backend thread");
        Self {
            commands: Commands(command_tx),
            events: event_rx,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: Command) {
        self.commands.send(command);
    }

    pub fn commands(&self) -> Commands {
        self.commands.clone()
    }

    /// Cloneable receiver: `.recv().await` it from an async event loop, or `.recv_blocking()`
    /// on a worker thread.
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Gone already if it failed fatally.
        self.commands.0.send(Request::Shutdown).ok();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the backend thread panicked");
        }
    }
}

/// Current time in Signal's timestamp format.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
