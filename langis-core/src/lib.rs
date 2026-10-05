//! UI-agnostic Signal backend.
//!
//! A [`Backend`] runs on its own thread. The UI sends [`Command`]s and receives [`Event`]s,
//! either by awaiting them in its event loop or by blocking on a worker thread; neither side
//! ever blocks the other. All types here are plain data so any toolkit can
//! render them.

#[cfg(feature = "signal")]
mod attachments;
pub mod format;
#[cfg(feature = "signal")]
mod import;
#[cfg(feature = "signal")]
mod local;
mod mock;
mod qr;
pub mod settings;
#[cfg(feature = "signal")]
mod signal;
pub mod state;
#[cfg(feature = "signal")]
mod transfer;
#[cfg(feature = "signal")]
mod vault;

pub use qr::QrMatrix;

use std::sync::Arc;
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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conversation {
    pub id: ConversationId,
    pub title: String,
    pub is_group: bool,
    /// A line under the title: a phone number, a group's size, "Messages to yourself".
    pub subtitle: String,
    pub settings: ChatSettings,
}

/// What langis remembers about a conversation besides its messages.
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
    /// Disappearing messages: seconds a message lasts once read (incoming) or sent; 0 is off.
    pub expire_timer: u32,
}

impl ChatSettings {
    pub fn muted(&self, now: u64) -> bool {
        self.muted_until.is_some_and(|until| until > now)
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

/// Replaces the placeholders Signal puts where a mention goes (U+FFFC) with "@Name".
/// `mentions` are `(start, length, name)` in UTF-16 code units, as Signal counts them. A
/// placeholder without one (e.g. from history imported before mentions were) reads "@…".
pub fn resolve_mentions(body: &str, mentions: &[(u32, u32, String)]) -> String {
    let ranges: Vec<_> = mentions
        .iter()
        .map(|(start, length, _)| format::WireRange {
            start: *start,
            length: *length,
            value: format::RangeValue::Mention(String::new()),
        })
        .collect();
    let names: Vec<_> = mentions
        .iter()
        .map(|(start, _, name)| (*start, name.clone()))
        .collect();
    format::decode(body, &ranges, &names).0
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
    /// Earlier versions, oldest first, with when they were replaced; empty if never edited.
    pub edits: Vec<(u64, String)>,
    /// Deleted for everyone: shown as such, without content.
    pub deleted: bool,
    /// When a disappearing message goes (ms since the epoch); `None` if it doesn't, or its
    /// timer hasn't started (it starts when read).
    pub expires_at: Option<u64>,
    /// Its disappearing timer in seconds, if it has one.
    pub expire_timer: Option<u32>,
    /// Formatting, in [`ChatMessage::body`]'s bytes.
    pub styles: Vec<StyleRange>,
}

impl ChatMessage {
    /// How Signal refers to this message, e.g. from reactions.
    pub fn reference(&self) -> MessageRef {
        MessageRef {
            author_id: self.sender_id.clone(),
            timestamp: self.timestamp,
        }
    }
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
    let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|m| m.timestamp == target.timestamp && m.sender_id == target.author_id)
    else {
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

#[derive(Clone, Debug)]
pub enum Event {
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
    /// Progress of the message history transfer right after linking.
    HistoryTransfer(HistoryTransfer),
    /// Initial message queue drained; it is now safe to send.
    Synced,
    /// New or changed conversations, merged by id.
    Conversations(Vec<Conversation>),
    /// Profile or group picture of a conversation, replacing any earlier one.
    Avatar {
        conversation: ConversationId,
        avatar: Arc<Picture>,
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
        content: Result<AttachmentContent, String>,
    },
    /// Answer to [`Command::SaveAttachment`]: where the file was written.
    AttachmentSaved {
        id: AttachmentId,
        path: Result<std::path::PathBuf, String>,
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
    /// Unread incoming messages per conversation, counted from the store at start.
    Unread(Vec<(ConversationId, u32)>),
    /// The complete stored history of a conversation, answering [`Command::LoadHistory`].
    History {
        conversation: ConversationId,
        messages: Vec<ChatMessage>,
    },
    /// A conversation's newest stored message, so it can be sorted and previewed before its
    /// history is loaded. Unlike [`Event::Message`], it is not new and not unread.
    Latest(ChatMessage),
    Message(ChatMessage),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum Command {
    /// Answer to [`Event::PasswordRequired`].
    Unlock(Password),
    LoadHistory(ConversationId),
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
        body: String,
        quote: Option<Quote>,
        files: Vec<std::path::PathBuf>,
        /// Formatting, in `body`'s bytes.
        styles: Vec<StyleRange>,
        /// People mentioned: where "@Name" stands in `body` (bytes), and who it is.
        mentions: Vec<Mention>,
    },
    /// Downloads an attachment and opens it with the desktop's default app for its type.
    OpenAttachment(Attachment),
    /// We started or stopped typing in a conversation.
    Typing {
        conversation: ConversationId,
        typing: bool,
    },
    /// These incoming messages were seen: tells their senders (if read receipts are on) and
    /// our other devices, and starts disappearing timers.
    MarkRead {
        conversation: ConversationId,
        messages: Vec<MessageRef>,
    },
    /// Marks a conversation unread by hand, or clears that.
    MarkUnread {
        conversation: ConversationId,
        unread: bool,
    },
    /// Replaces the text of one of our messages.
    Edit {
        conversation: ConversationId,
        target: MessageRef,
        body: String,
        styles: Vec<StyleRange>,
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
    /// Privacy preferences the backend acts on.
    Preferences {
        read_receipts: bool,
        typing_indicators: bool,
    },
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: "langis".into(),
        }
    }
}

/// Backend side of the event channel. It is unbounded, so sending never blocks or awaits.
#[derive(Clone)]
pub(crate) struct Events(async_channel::Sender<Event>);

impl Events {
    /// Returns `false` once the UI has gone away.
    pub(crate) fn send(&self, event: Event) -> bool {
        self.0.try_send(event).is_ok()
    }
}

/// What travels to the backend: UI commands, then possibly the order to stop.
enum Request {
    Command(Command),
    Shutdown,
}

/// Backend side of the command channel.
pub(crate) struct Inbox(Receiver<Request>);

impl Inbox {
    /// The next command; `None` once the backend should stop.
    pub(crate) fn recv(&self) -> Option<Command> {
        match self.0.recv() {
            Ok(Request::Command(command)) => Some(command),
            Ok(Request::Shutdown) | Err(_) => None,
        }
    }

    /// Like [`Inbox::recv`], but gives up after `timeout`.
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Command, RecvTimeoutError> {
        match self.0.recv_timeout(timeout)? {
            Request::Command(command) => Ok(command),
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
        let _ = self.0.send(Request::Command(command));
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
            || (cfg!(debug_assertions) && std::env::var_os("LANGIS_MOCK").is_some())
    }

    /// Starts the real backend, or the mock one when [`Backend::mock_requested`].
    pub fn from_env(config: Config) -> Self {
        #[cfg(feature = "signal")]
        if !Self::mock_requested() {
            return Self::start(config);
        }
        let _ = config;
        Self::mock()
    }

    fn spawn(run: impl FnOnce(Inbox, Events) + Send + 'static) -> Self {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = async_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("langis-backend".into())
            // presage/libsignal futures are huge and the runtime is current-thread, so they all
            // live on this stack; the 2 MiB default overflows in debug builds.
            .stack_size(64 * 1024 * 1024)
            .spawn(move || run(Inbox(command_rx), Events(event_tx)))
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
        let _ = self.commands.0.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Current time in Signal's timestamp format.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
