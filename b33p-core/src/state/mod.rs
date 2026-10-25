//! Toolkit-independent view model: everything a chat UI needs to know, derived from [`Event`]s.
//! Frontends render from this and translate user actions into [`Command`]s through it.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    ops::Range,
    sync::Arc,
};

use crate::settings::NotificationContent;
use crate::timeline;
use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, ChatMessage, ChatSettings,
    Command, Conversation, ConversationId, Details, Event, Failure, HistoryTransfer, Mention,
    MessageRef, Password, Person, Picture, QrMatrix, Quote, Reaction, RequestResponse, StyleRange,
    SyncStage, Unsent, apply_reaction, format, now_millis,
};

mod attachments;
mod clock;
mod composer;
mod events;
mod history;
mod list;
mod panels;
mod reading;
mod selection;
mod session;
mod text;
mod views;

pub use attachments::*;
pub use clock::*;
use history::*;
pub use text::*;
use views::*;

/// Minimum length for a newly chosen database password.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Why `password` can't become the database password, if it can't.
pub fn password_problem(password: &str) -> Option<String> {
    (password.chars().count() < MIN_PASSWORD_LEN)
        .then(|| format!("Use at least {MIN_PASSWORD_LEN} characters."))
}

/// Someone typing counts as stopped after this long without hearing from them, as in Signal.
const TYPING_TIMEOUT_MS: u64 = 15_000;

/// While we keep typing, Signal expects a fresh "started" this often.
const TYPING_REPEAT_MS: u64 = 10_000;

/// Messages can be deleted for everyone this long after they were sent.
pub const DELETE_FOR_EVERYONE_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    /// Starting up: until the first [`Event::Synced`], or the first failure to connect.
    Syncing(SyncStage),
    /// See [`Event::PasswordRequired`].
    Password {
        new: bool,
        error: Option<String>,
    },
    Link(QrMatrix),
    Chats,
    /// See [`Event::Failed`]: nothing goes on until the user answers.
    Failed {
        failure: Failure,
        message: String,
    },
}

impl Default for Screen {
    fn default() -> Self {
        Self::Syncing(SyncStage::Starting)
    }
}

/// See [`ChatState::connection`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Connected, and caught up with what came meanwhile.
    Online,
    /// Connected; what came meanwhile is arriving.
    CatchingUp,
    /// Not connected; trying again.
    Offline,
}

/// What a call to [`ChatState::apply`] changed, so UIs can update incrementally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub screen: bool,
    pub conversations: bool,
    pub messages: MessagesUpdate,
    /// Messages already shown changed in place (an attachment downloaded, a reaction came
    /// in); their rows need redrawing: those [`ChatState::take_changed_rows`] names, or all.
    pub rows: bool,
    /// The user sent a message: show the end of the open chat, wherever it was scrolled to.
    pub scroll_to_end: bool,
}

impl Update {
    /// The chat list and the open chat's messages need redrawing, e.g. after another chat was
    /// opened or this one closed.
    pub fn reset() -> Self {
        Self {
            conversations: true,
            messages: MessagesUpdate::Reset,
            ..Self::default()
        }
    }

    /// Combines updates from several events applied in a row.
    pub fn merge(self, other: Self) -> Self {
        Self {
            screen: self.screen || other.screen,
            conversations: self.conversations || other.conversations,
            messages: self.messages.merge(other.messages),
            rows: self.rows || other.rows,
            scroll_to_end: self.scroll_to_end || other.scroll_to_end,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MessagesUpdate {
    #[default]
    Unchanged,
    /// This many messages were appended to the selected conversation; see
    /// [`ChatState::last_messages`].
    Appended(usize),
    /// This many older messages were put before the selected conversation's; the one after
    /// them may have changed too (its day label, its run).
    Prepended(usize),
    /// The selected conversation's messages must be re-read.
    Reset,
}

impl MessagesUpdate {
    fn merge(self, other: Self) -> Self {
        use MessagesUpdate::*;
        match (self, other) {
            (Reset, _) | (_, Reset) => Reset,
            (Appended(a), Appended(b)) => Appended(a + b),
            (Appended(n), Unchanged) | (Unchanged, Appended(n)) => Appended(n),
            (Prepended(a), Prepended(b)) => Prepended(a + b),
            (Prepended(n), Unchanged) | (Unchanged, Prepended(n)) => Prepended(n),
            (Prepended(_), Appended(_)) | (Appended(_), Prepended(_)) => Reset,
            (Unchanged, Unchanged) => Unchanged,
        }
    }
}

/// Someone (or a group) with stories, for the stories list.
pub struct StoryRing<'a> {
    pub conversation: &'a ConversationId,
    /// Their name, or the group's.
    pub title: String,
    /// When the newest was posted.
    pub latest: u64,
    pub count: usize,
    pub unviewed: bool,
}

pub struct ConversationView<'a> {
    pub conversation: &'a Conversation,
    /// Shared, so UIs can cache what they convert it into and tell when it changed.
    pub avatar: Option<&'a Arc<Picture>>,
    pub last_message: Option<&'a ChatMessage>,
    pub unread: u32,
    pub selected: bool,
    /// Names of who is typing there right now.
    pub typing: Vec<&'a str>,
    /// When it was made, from [`ChatState`]'s [`Clock`].
    now: u64,
}

impl ConversationView<'_> {
    pub fn pinned(&self) -> bool {
        self.conversation.settings.pinned.is_some()
    }

    pub fn muted(&self) -> bool {
        self.conversation.settings.muted(self.now)
    }

    /// Marked unread by hand, without a count.
    pub fn marked_unread(&self) -> bool {
        self.unread == 0 && self.conversation.settings.marked_unread
    }
}

impl ConversationView<'_> {
    /// First line of the newest message, prefixed with "You: " if we sent it.
    pub fn preview(&self) -> String {
        let Some(message) = self.last_message else {
            return String::new();
        };
        if message.deleted {
            return DELETED.into();
        }
        let body = format::hide_spoilers(&message.body, &message.styles);
        let body = match body.lines().next() {
            Some(line) => line.to_string(),
            None => message
                .attachments
                .first()
                .map_or_else(String::new, |a| describe_kind(a.kind).to_string()),
        };
        if message.outgoing {
            format!("You: {body}")
        } else {
            body.to_string()
        }
    }

    /// What the preview is about, for an icon before it: what is attached to a message without
    /// text, or a change of the disappearing-message timer.
    pub fn preview_kind(&self) -> Option<PreviewKind> {
        let message = self.last_message.filter(|m| !m.deleted)?;
        if message.timer_change.is_some() {
            return Some(PreviewKind::Timer);
        }
        if !message.body.trim().is_empty() {
            return None;
        }
        Some(PreviewKind::Attachment(message.attachments.first()?.kind))
    }
}

/// See [`ConversationView::preview_kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Attachment(AttachmentKind),
    Timer,
}

/// A desktop notification to show or take back; see [`ChatState::take_notices`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `conversation` is `None` for a summary, which just opens the app. A newer notice for
    /// the same conversation replaces the older one.
    Show {
        conversation: Option<ConversationId>,
        title: String,
        body: String,
    },
    /// The conversation was read; its notification is stale.
    Withdraw(ConversationId),
}

pub struct MessageView<'a> {
    pub message: &'a ChatMessage,
    /// Show the sender's name above the message (incoming group messages, first of a run).
    pub show_sender: bool,
    /// The message text: its full text if it is long and that has been fetched; [`DELETED`]
    /// if it was deleted.
    pub body: &'a str,
    /// The formatting of `body` (none for a fetched long text).
    pub styles: &'a [StyleRange],
    /// "Today", "Yesterday", "Monday", "Oct 2": set on the first message of each day.
    pub day: Option<String>,
    /// Messages by the same person within a few minutes form a run, drawn close together.
    pub first_of_run: bool,
    pub last_of_run: bool,
    /// The sender's picture, if known (for incoming group messages).
    pub sender_avatar: Option<&'a Arc<Picture>>,
    /// The first message that arrived after the conversation was last read.
    pub first_unread: bool,
    /// Its spoilers were clicked open.
    pub revealed: bool,
    /// The search result last shown.
    pub highlighted: bool,
    /// Picked, with others, for an action on all of them; see [`ChatState::toggle_selected`].
    pub selected: bool,
}

/// What a message deleted for everyone shows instead.
pub const DELETED: &str = "This message was deleted.";

/// Messages further apart than this start a new run, even from the same person.
const RUN_GAP_MS: u64 = 5 * 60 * 1000;

#[derive(Default)]
pub struct ChatState {
    screen: Screen,
    account: String,
    synced: bool,
    /// Connected to Signal; see [`ChatState::connection`].
    connected: bool,
    transfer: Option<HistoryTransfer>,
    error: Option<crate::Error>,
    /// What failed, to try again from the error banner; see [`ChatState::retry_error`].
    error_retry: Option<Command>,
    /// The message whose failed send the error is about: sending it again takes the error
    /// away. See [`ChatState::show_error`].
    error_send: Option<(ConversationId, u64)>,
    /// What was sent for each message shown as sending or failed, to send it again.
    pending_sends: HashMap<(ConversationId, u64), Command>,
    /// When the last message was sent; see [`ChatState::send_timestamp`].
    last_send: u64,
    /// Chats with a message of ours in their history: not requests, whatever the phone says.
    written_to: HashSet<ConversationId>,
    /// The chat list's order is stale; sorted once at the end of [`ChatState::apply`], or of
    /// a batch of them.
    sort_pending: bool,
    /// Most recently active first; see [`ChatState::sort_conversations`].
    conversations: Vec<Conversation>,
    /// Where each conversation is in `conversations`, kept up to date as it is added to and
    /// sorted.
    conversation_at: HashMap<ConversationId, usize>,
    unread: HashMap<ConversationId, u32>,
    /// Incoming messages that came while their chat wasn't looked at, dated before what was
    /// read already (late, e.g. from a group): unread although `last_read` says otherwise.
    late: HashMap<ConversationId, Vec<MessageRef>>,
    /// Others' messages edited since they came, by their first time: as Signal's apps do, an
    /// edit is read anew, and its sender told.
    unread_edits: HashMap<ConversationId, Vec<MessageRef>>,
    /// Chats opened before their stored history was here: read once it is.
    opened_unloaded: HashSet<ConversationId>,
    /// Messages counted as read this session, so none is again (one dated in the future stays
    /// after `last_read`, which never passes the present).
    read: MessageSet,
    /// The selected conversation, looked at while events brought it messages: read at the end
    /// of [`ChatState::apply`], or of a batch of them, with one receipt for all that came;
    /// sooner if another is opened, or its settings or messages change meanwhile.
    seen_pending: Option<ConversationId>,
    /// Per conversation, sorted and unique by [`message_key`].
    history: HashMap<ConversationId, Vec<ChatMessage>>,
    /// Pictures of groups (of chats with someone: `people_avatars`).
    avatars: HashMap<ConversationId, Arc<Picture>>,
    /// Pictures of people, by service ID: what messages know their senders by.
    people_avatars: HashMap<String, Arc<Picture>>,
    attachments: HashMap<AttachmentId, AttachmentState>,
    /// The downloads in `attachments` that are there, in the order they came, and their bytes
    /// together: kept within [`KEPT_DOWNLOADS`].
    downloaded: std::collections::VecDeque<AttachmentId>,
    downloaded_bytes: usize,
    /// Counts downloads let go of, for caches that hold on to them; see
    /// [`ChatState::downloads_forgotten`].
    forgotten: u64,
    /// Another bound than [`KEPT_DOWNLOADS`], for tests.
    #[cfg(test)]
    kept_downloads: Option<usize>,
    saves: HashMap<AttachmentId, SaveState>,
    /// Search text; while set, the chat list holds every matching conversation.
    filter: String,
    /// Files to send with the next message.
    staged: Vec<std::path::PathBuf>,
    /// What the composer replies to; belongs to the selected conversation.
    replying: Option<Quote>,
    /// Conversations whose stored history has arrived (its newest page, at least).
    /// Where each loaded conversation's next older page starts; `None` at the beginning.
    /// Having an entry is what makes a conversation loaded.
    older: HashMap<ConversationId, Option<u64>>,
    /// Conversations whose next older page was asked for.
    loading_older: HashSet<ConversationId>,
    /// Reactions to messages not loaded yet, oldest first, applied when their page comes.
    pending_reactions: HashMap<ConversationId, Vec<(MessageRef, Reaction, bool)>>,
    selected: Option<ConversationId>,
    notifications: Notifications,
    /// Who is typing where, and until when (ms) that counts without news.
    typing: HashMap<ConversationId, Vec<Typist>>,
    /// When we last told the selected conversation we are typing.
    typing_sent: Option<(ConversationId, u64)>,
    /// One of our messages whose text the composer replaces.
    editing: Option<MessageRef>,
    /// Shows the archived conversations instead of the others.
    show_archived: bool,
    /// Where the "New messages" line goes in the selected conversation: its `last_read`
    /// when it was opened, kept while it is open so the line doesn't jump away.
    read_mark: Option<u64>,
    /// Commands that follow from state changes rather than a single user action, e.g. read
    /// receipts; see [`ChatState::take_commands`].
    queued: Vec<Command>,
    /// Messages whose spoilers were clicked open.
    revealed: MessageSet,
    /// Messages picked to copy, forward or delete together; see [`ChatState::toggle_selected`].
    /// Those that left the chat since stay in here, unseen.
    selection: MessageSet,
    /// The message last picked or unpicked: Shift+click picks everything between it and
    /// another.
    selection_anchor: Option<MessageRef>,
    /// People picked to mention in the draft; their "@Name" becomes a mention when sent.
    draft_mentions: Vec<Person>,
    /// What was being written in each chat (not an edit), with whom it mentions: it waits
    /// there while other chats are open, and while editing.
    drafts: HashMap<ConversationId, (String, Vec<Person>)>,
    /// The messages being forwarded, while their target is chosen; empty otherwise.
    forwarding: Vec<ChatMessage>,
    /// Searching the selected conversation.
    chat_search: Option<ChatSearch>,
    /// A search result to show once its page of history has loaded.
    pending_jump: Option<MessageRef>,
    /// A chat just opened: once its history is there, show its first unread message rather
    /// than its end.
    jump_to_unread: bool,
    /// The row of [`ChatState::messages`] to scroll to, and a number counting such requests;
    /// see [`ChatState::jump`].
    jump: Option<(usize, u64)>,
    /// The search result shown, marked in the chat.
    highlight: Option<MessageRef>,
    /// The selected conversation's media, files and links, while shown: `None` inside while
    /// they are gathered.
    media: Option<(ConversationId, Option<Gathered>)>,
    /// The selected conversation's details page, while shown: `None` inside while gathered.
    details: Option<(ConversationId, Option<Details>)>,
    /// What the phone accepted is known; see [`ChatState::is_request`].
    requests_known: bool,
    /// The phone's chat folders, and the one shown (`None` for all chats).
    folders: Vec<crate::ChatFolder>,
    /// Each folder's chats picked by name, as sets: asked about every chat in the list.
    folder_chats: Vec<FolderChats>,
    folder: Option<usize>,
    /// Choosing people for a new group, or to add to one.
    group_draft: Option<GroupDraft>,
    /// Joining a group through its invite link.
    join: Option<JoinDraft>,
    /// The stories of the last day, oldest first.
    stories: Vec<crate::Story>,
    /// The story shown: whose (a conversation) and which of theirs.
    story_view: Option<(ConversationId, usize)>,
    /// The installed sticker packs, once asked for: `Some(None)` while they are gathered.
    sticker_packs: Option<Option<Vec<crate::StickerPack>>>,
    /// The open chat's messages that changed in place since the UI last asked; see
    /// [`ChatState::take_changed_rows`]. A `Cell`, so that drawing, which only reads the
    /// state, can take it.
    changed_rows: std::cell::Cell<ChangedRows>,
    /// The chats whose rows in the list events changed since the UI last asked; see
    /// [`ChatState::take_changed_chats`].
    changed_chats: std::cell::Cell<ChangedChats>,
    clock: Clock,
}

/// See [`ChatState::take_changed_chats`].
#[derive(Default)]
struct ChangedChats {
    chats: Vec<ConversationId>,
    all: bool,
}

impl ChangedChats {
    fn note(&mut self, chat: Option<ConversationId>) {
        match chat {
            _ if self.all => {}
            Some(chat) if self.chats.len() < CHANGED_ROWS_NAMED => {
                if !self.chats.contains(&chat) {
                    self.chats.push(chat);
                }
            }
            _ => {
                *self = Self {
                    chats: Vec::new(),
                    all: true,
                };
            }
        }
    }
}

/// See [`ChatState::take_changed_rows`].
#[derive(Default)]
struct ChangedRows {
    messages: Vec<MessageRef>,
    /// Too many to name, or changes that aren't one message's (a sender's picture).
    all: bool,
}

/// Beyond this many changed messages, every row is redrawn instead: what waits for a redraw
/// stays small, and many changes at once (picking a long stretch) are rare.
const CHANGED_ROWS_NAMED: usize = 100;

impl ChangedRows {
    fn note(&mut self, messages: impl IntoIterator<Item = MessageRef>) {
        for message in messages {
            if self.all {
                return;
            }
            if self.messages.len() == CHANGED_ROWS_NAMED {
                self.note_all();
                return;
            }
            self.messages.push(message);
        }
    }

    fn note_all(&mut self) {
        *self = Self {
            messages: Vec::new(),
            all: true,
        };
    }
}

/// What [`ChatState::media`] shows on one of its tabs.
pub struct MediaOverview<'a> {
    /// Pictures and videos, newest first.
    pub media: Vec<&'a Attachment>,
    /// Other files, newest first, with the message they came with.
    pub files: Vec<(&'a ChatMessage, &'a Attachment)>,
    /// Web addresses, newest first, with the message they were in.
    pub links: Vec<(&'a ChatMessage, String)>,
}

/// What [`Event::Media`] brought, with the links in it found once rather than whenever they
/// are shown.
struct Gathered {
    messages: Vec<ChatMessage>,
    /// In the order of the messages they are in, which they point to.
    links: Vec<(usize, String)>,
}

impl Gathered {
    fn new(messages: Vec<ChatMessage>) -> Self {
        let links = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.deleted)
            .flat_map(|(at, m)| format::links(&m.body).into_iter().map(move |l| (at, l)))
            .collect();
        Self { messages, links }
    }
}

/// A [`crate::ChatFolder`]'s chats picked by name, as sets.
struct FolderChats {
    included: HashSet<ConversationId>,
    excluded: HashSet<ConversationId>,
}

impl FolderChats {
    fn of(folder: &crate::ChatFolder) -> Self {
        Self {
            included: folder.included.iter().cloned().collect(),
            excluded: folder.excluded.iter().cloned().collect(),
        }
    }
}

/// See [`ChatState::open_group_draft`].
struct GroupDraft {
    /// The group they are added to; `None` for a new one.
    adding_to: Option<ConversationId>,
    chosen: Vec<ConversationId>,
}

/// See [`ChatState::open_join`].
struct JoinDraft {
    link: String,
    preview: Option<crate::GroupPreview>,
    /// The link is being looked up, or the group joined.
    busy: bool,
}

struct ChatSearch {
    conversation: ConversationId,
    query: String,
    /// `None` while the search runs.
    results: Option<Vec<ChatMessage>>,
}

struct Typist {
    sender_id: String,
    sender: String,
    until: u64,
}

/// Inputs and output of the notification policy.
#[derive(Default)]
struct Notifications {
    enabled: bool,
    content: NotificationContent,
    window_focused: bool,
    /// Incoming messages that arrived while catching up (not synced), by conversation;
    /// announced as one summary once synced.
    caught_up: HashMap<ConversationId, usize>,
    pending: Vec<Notice>,
}

/// A set of messages that can be asked about one without making its [`MessageRef`], as
/// rows ask for every message they show.
#[derive(Default)]
struct MessageSet {
    /// Authors by timestamp: messages rarely share one.
    by_time: HashMap<u64, Vec<String>>,
}

impl MessageSet {
    fn contains(&self, (timestamp, author): (u64, &str)) -> bool {
        self.by_time
            .get(&timestamp)
            .is_some_and(|authors| authors.iter().any(|a| a == author))
    }

    /// Whether it wasn't there yet.
    fn insert(&mut self, (timestamp, author): (u64, &str)) -> bool {
        let authors = self.by_time.entry(timestamp).or_default();
        let new = !authors.iter().any(|a| a == author);
        if new {
            authors.push(author.to_owned());
        }
        new
    }

    /// Whether it was there.
    fn remove(&mut self, (timestamp, author): (u64, &str)) -> bool {
        let Some(authors) = self.by_time.get_mut(&timestamp) else {
            return false;
        };
        let Some(at) = authors.iter().position(|a| a == author) else {
            return false;
        };
        authors.swap_remove(at);
        if authors.is_empty() {
            self.by_time.remove(&timestamp);
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.by_time.is_empty()
    }

    fn clear(&mut self) {
        self.by_time.clear();
    }

    fn iter(&self) -> impl Iterator<Item = (u64, &str)> {
        self.by_time
            .iter()
            .flat_map(|(&timestamp, authors)| authors.iter().map(move |a| (timestamp, a.as_str())))
    }
}

#[cfg(test)]
mod benches;
#[cfg(test)]
mod edit_props;
#[cfg(test)]
mod helper_props;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod read_props;
#[cfg(test)]
mod tests;
