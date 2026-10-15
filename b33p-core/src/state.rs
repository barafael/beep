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
    Command, Conversation, ConversationId, Details, Event, HistoryTransfer, Mention, MessageRef,
    Password, Person, Picture, QrMatrix, Quote, Reaction, RequestResponse, StyleRange, Unsent,
    apply_reaction, format, now_millis,
};

/// Minimum length for a newly chosen database password.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Someone typing counts as stopped after this long without hearing from them, as in Signal.
const TYPING_TIMEOUT_MS: u64 = 15_000;
/// While we keep typing, Signal expects a fresh "started" this often.
const TYPING_REPEAT_MS: u64 = 10_000;
/// Messages can be deleted for everyone this long after they were sent.
pub const DELETE_FOR_EVERYONE_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    Connecting,
    /// See [`Event::PasswordRequired`].
    Password {
        new: bool,
        error: Option<String>,
    },
    Link(QrMatrix),
    Chats,
}

/// What a call to [`ChatState::apply`] changed, so UIs can update incrementally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub screen: bool,
    pub conversations: bool,
    pub messages: MessagesUpdate,
    /// Messages already shown changed in place (an attachment downloaded, a reaction came
    /// in); their rows need redrawing.
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
}

impl ConversationView<'_> {
    pub fn pinned(&self) -> bool {
        self.conversation.settings.pinned.is_some()
    }

    pub fn muted(&self) -> bool {
        self.conversation.settings.muted(now_millis())
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

/// Where a download for display stands; see [`ChatState::attachment`].
#[derive(Clone, Debug, PartialEq)]
pub enum AttachmentState {
    Loading,
    Ready(AttachmentContent),
    /// Can be asked for again; see [`ChatState::fetch_attachment`].
    Failed(String),
}

/// Where saving an attachment stands; see [`ChatState::saved`]. Independent of
/// [`AttachmentState`], so a shown picture stays shown while and after it is saved.
#[derive(Clone, Debug, PartialEq)]
pub enum SaveState {
    Saving,
    Saved(std::path::PathBuf),
    Failed(String),
}

/// A word or two for what an attachment is, e.g. for previews.
pub fn describe_kind(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "📷 Photo",
        AttachmentKind::Video => "🎞 Video",
        AttachmentKind::Audio => "🎵 Audio",
        AttachmentKind::VoiceNote => "🎤 Voice message",
        AttachmentKind::Sticker => "Sticker",
        AttachmentKind::LongText => "Long message",
        AttachmentKind::ViewOnce => "View-once media",
        AttachmentKind::File => "📎 File",
    }
}

/// Whether an attachment is fetched as soon as its message is shown.
pub fn shown_inline(kind: AttachmentKind) -> bool {
    matches!(
        kind,
        AttachmentKind::Image | AttachmentKind::Sticker | AttachmentKind::LongText
    )
}

#[derive(Default)]
pub struct ChatState {
    screen: Screen,
    account: String,
    synced: bool,
    transfer: Option<HistoryTransfer>,
    error: Option<String>,
    /// Most recently active first; see [`ChatState::sort_conversations`].
    conversations: Vec<Conversation>,
    unread: HashMap<ConversationId, u32>,
    /// Incoming messages that came while their chat wasn't looked at, dated before what was
    /// read already (late, e.g. from a group): unread although `last_read` says otherwise.
    late: HashMap<ConversationId, Vec<MessageRef>>,
    /// Chats opened before their stored history was here: read once it is.
    opened_unloaded: HashSet<ConversationId>,
    /// Messages counted as read this session, so none is again (one dated in the future stays
    /// after `last_read`, which never passes the present).
    read: HashSet<MessageRef>,
    /// Per conversation, sorted and unique by [`message_key`].
    history: HashMap<ConversationId, Vec<ChatMessage>>,
    avatars: HashMap<ConversationId, Arc<Picture>>,
    attachments: HashMap<AttachmentId, AttachmentState>,
    saves: HashMap<AttachmentId, SaveState>,
    /// Search text; while set, the chat list holds every matching conversation.
    filter: String,
    /// Files to send with the next message.
    staged: Vec<std::path::PathBuf>,
    /// What the composer replies to; belongs to the selected conversation.
    replying: Option<Quote>,
    /// Conversations whose stored history has arrived (its newest page, at least).
    loaded: HashSet<ConversationId>,
    /// Where each loaded conversation's next older page starts; `None` at the beginning.
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
    revealed: HashSet<MessageRef>,
    /// Messages picked to copy, forward or delete together; see [`ChatState::toggle_selected`].
    selection: Vec<MessageRef>,
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
    /// The row of [`ChatState::messages`] to scroll to, and a number counting such requests;
    /// see [`ChatState::jump`].
    jump: Option<(usize, u64)>,
    /// The search result shown, marked in the chat.
    highlight: Option<MessageRef>,
    /// The selected conversation's media, files and links, while shown: `None` inside while
    /// they are gathered.
    media: Option<(ConversationId, Option<Vec<ChatMessage>>)>,
    /// The selected conversation's details page, while shown: `None` inside while gathered.
    details: Option<(ConversationId, Option<Details>)>,
    /// What the phone accepted is known; see [`ChatState::is_request`].
    requests_known: bool,
    /// The phone's chat folders, and the one shown (`None` for all chats).
    folders: Vec<crate::ChatFolder>,
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

impl ChatState {
    pub fn apply(&mut self, event: Event) -> Update {
        let mut update = Update::default();
        match event {
            Event::PasswordRequired { new, error } => {
                self.screen = Screen::Password { new, error };
                update.screen = true;
            }
            Event::LinkQr(qr) => {
                self.screen = Screen::Link(qr);
                update.screen = true;
            }
            Event::Linked { account } => {
                self.account = account;
                self.screen = Screen::Chats;
                update.screen = true;
            }
            Event::Synced => {
                self.synced = true;
                self.announce_caught_up();
            }
            Event::Reconnecting => self.synced = false,
            Event::HistoryTransfer(HistoryTransfer::Finished) => self.transfer = None,
            Event::HistoryTransfer(stage) => self.transfer = Some(stage),
            Event::Conversations(list) => {
                for conversation in list {
                    match self
                        .conversations
                        .iter_mut()
                        .find(|c| c.id == conversation.id)
                    {
                        Some(existing) => *existing = conversation,
                        None => self.conversations.push(conversation),
                    }
                }
                self.sort_conversations();
                update.conversations = true;
            }
            Event::Attachment { id, content } => {
                let state = match content {
                    Ok(content) => AttachmentState::Ready(content),
                    Err(error) => AttachmentState::Failed(error),
                };
                self.attachments.insert(id, state);
                update.rows = true;
            }
            Event::AttachmentSaved { id, path } => {
                let state = match path {
                    Ok(path) => SaveState::Saved(path),
                    Err(error) => SaveState::Failed(error),
                };
                self.saves.insert(id, state);
                update.rows = true;
            }
            Event::Reaction {
                conversation,
                target,
                reaction,
                remove,
            } => {
                let Some(history) = self.history.get_mut(&conversation) else {
                    // Not loaded at all: the stored history will include it.
                    return update;
                };
                if history.iter().any(|m| m.is(&target)) {
                    apply_reaction(history, &target, reaction, remove);
                    update.rows = self.selected.as_ref() == Some(&conversation);
                } else if self.loaded.contains(&conversation) {
                    // To an older message, applied when its page loads.
                    self.pending_reactions
                        .entry(conversation)
                        .or_default()
                        .push((target, reaction, remove));
                }
            }
            Event::Avatar {
                conversation,
                avatar,
            } => {
                match avatar {
                    Some(avatar) => self.avatars.insert(conversation, avatar),
                    None => self.avatars.remove(&conversation),
                };
                update.conversations = true;
            }
            Event::History {
                conversation,
                messages,
                reactions,
                older,
            } => {
                let newest_page = self.loaded.insert(conversation.clone());
                self.older.insert(conversation.clone(), older);
                self.loading_older.remove(&conversation);
                let history = self.history.entry(conversation.clone()).or_default();
                let first = history.first().map(|m| (m.timestamp, m.sender_id.clone()));
                let (mut added, mut replaced) = (0, 0);
                // The stored history is complete (delivery, edits, …); it replaces what came
                // before it, e.g. the bare newest message from `Event::Latest`.
                for message in messages {
                    if upsert(history, message) {
                        added += 1;
                    } else {
                        replaced += 1;
                    }
                }
                // An older page goes before what is shown, unless some of it falls between
                // (messages that came late): then the rows are redone.
                let before_first = first.map_or(history.len(), |(timestamp, sender)| {
                    history.partition_point(|m| message_key(m) < (timestamp, sender.as_str()))
                });
                let only_prepended = before_first == added;
                // This page's reactions are older than those already waiting.
                let waiting = self
                    .pending_reactions
                    .entry(conversation.clone())
                    .or_default();
                let mut reactions = reactions;
                reactions.append(waiting);
                for (target, reaction, remove) in reactions {
                    if history.iter().any(|m| m.is(&target)) {
                        apply_reaction(history, &target, reaction, remove);
                        replaced += 1;
                    } else {
                        waiting.push((target, reaction, remove));
                    }
                }
                if newest_page || added > 0 {
                    self.sort_conversations();
                    update.conversations = true;
                }
                if newest_page
                    && self.opened_unloaded.remove(&conversation)
                    && let Some(command) = self.mark_read(conversation.clone())
                {
                    self.queued.push(command);
                }
                if self.selected.as_ref() == Some(&conversation) {
                    if newest_page || !only_prepended {
                        update.messages = MessagesUpdate::Reset;
                    } else if added > 0 {
                        update.messages = MessagesUpdate::Prepended(added);
                    }
                    update.rows |= replaced > 0;
                    if newest_page {
                        self.mark_seen();
                    }
                    self.continue_jump();
                }
            }
            Event::Latest(message) => {
                let id = message.conversation.clone();
                let history = self.history.entry(id.clone()).or_default();
                if insert(history, message).is_some() {
                    self.sort_conversations();
                    update.conversations = true;
                    if self.selected.as_ref() == Some(&id) {
                        update.messages = MessagesUpdate::Reset;
                    }
                }
            }
            Event::Message(message)
                if message.outgoing
                    && message.unsent.is_none()
                    && self
                        .history
                        .get(&message.conversation)
                        .is_some_and(|history| {
                            history.iter().any(|m| is_unsent_at(m, message.timestamp))
                        }) =>
            {
                // Sent: it takes the place of the message shown while sending.
                let id = message.conversation.clone();
                let history = self.history.get_mut(&id).expect("checked above");
                let shown = history
                    .iter()
                    .rposition(|m| is_unsent_at(m, message.timestamp))
                    .expect("checked above");
                history.remove(shown);
                let pos = insert(history, message);
                if self.selected.as_ref() == Some(&id) {
                    if pos == Some(shown) {
                        update.rows = true;
                    } else {
                        update.messages = MessagesUpdate::Reset;
                    }
                }
                update.conversations = true;
            }
            Event::SendFailed {
                conversation,
                timestamp,
            } => {
                if let Some(message) = self
                    .history
                    .get_mut(&conversation)
                    .and_then(|history| history.iter_mut().rfind(|m| is_unsent_at(m, timestamp)))
                {
                    message.unsent = Some(Unsent::Failed);
                    update.rows = self.selected.as_ref() == Some(&conversation);
                }
            }
            Event::Message(message) => {
                let id = message.conversation.clone();
                let outgoing = message.outgoing;
                let history = self.history.entry(id.clone()).or_default();
                let Some(pos) = insert(history, message) else {
                    return update;
                };
                // Reactions that came before it (to a message on its way).
                if let Some(waiting) = self.pending_reactions.get_mut(&id) {
                    let reference = history[pos].reference();
                    for (target, reaction, remove) in
                        waiting.extract_if(.., |(t, ..)| *t == reference)
                    {
                        apply_reaction(history, &target, reaction, remove);
                    }
                }
                let appended = pos + 1 == history.len();
                if let Some(timer) = history[pos].timer_change
                    && let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == id)
                {
                    conversation.expire_timer = timer;
                }
                if let Some(typists) = self.typing.get_mut(&id) {
                    let sender_id = &history[pos].sender_id;
                    typists.retain(|t| &t.sender_id != sender_id);
                }
                let looking =
                    self.selected.as_ref() == Some(&id) && self.notifications.window_focused;
                let notice = history[pos].notice;
                let reference = history[pos].reference();
                if !outgoing && !notice {
                    let message = &history[pos];
                    // A photo or sticker alone says what it is.
                    let body = match (message.body.is_empty(), message.attachments.first()) {
                        (true, Some(attachment)) => describe_kind(attachment.kind).to_string(),
                        _ => format::hide_spoilers(&message.body, &message.styles),
                    };
                    let sender = message.sender.clone();
                    self.notify_message(&id, sender, body);
                }
                if self.selected.as_ref() == Some(&id) {
                    update.messages = if appended {
                        MessagesUpdate::Appended(1)
                    } else {
                        MessagesUpdate::Reset
                    };
                }
                if !outgoing && !notice {
                    let last_read = self.conversation(&id).map(|c| c.settings.last_read);
                    if last_read.is_some_and(|last_read| reference.timestamp <= last_read) {
                        self.late.entry(id.clone()).or_default().push(reference);
                    }
                }
                if looking {
                    self.mark_seen();
                } else if !outgoing && !notice {
                    *self.unread.entry(id).or_default() += 1;
                }
                self.sort_conversations();
                update.conversations = true;
            }
            Event::Delivered {
                conversation,
                timestamps,
                delivery,
            } => {
                let Some(history) = self.history.get_mut(&conversation) else {
                    return update;
                };
                let mut changed = false;
                for message in history
                    .iter_mut()
                    .filter(|m| m.outgoing && m.delivery < delivery)
                    .filter(|m| timestamps.contains(&m.timestamp))
                {
                    message.delivery = delivery;
                    changed = true;
                }
                update.rows = changed && self.selected.as_ref() == Some(&conversation);
            }
            Event::Typing {
                conversation,
                sender_id,
                sender,
                typing,
            } => {
                let typists = self.typing.entry(conversation).or_default();
                typists.retain(|t| t.sender_id != sender_id);
                if typing {
                    typists.push(Typist {
                        sender_id,
                        sender,
                        until: now_millis() + TYPING_TIMEOUT_MS,
                    });
                }
                update.conversations = true;
            }
            Event::Edited {
                conversation,
                target,
                mut message,
            } => {
                if let Some(old) = self.find_message(&conversation, &target) {
                    // The backend may not know the text before (incoming edits replace it in
                    // the store); here it is known.
                    let edited_at = message.edited_at.unwrap_or_else(now_millis);
                    message.edited_at = Some(edited_at);
                    message.edits = std::mem::take(&mut old.edits);
                    message
                        .edits
                        .push((edited_at, std::mem::take(&mut old.body)));
                    // Known by its original time; what reacted to it still applies.
                    message.timestamp = old.timestamp;
                    message.reactions = std::mem::take(&mut old.reactions);
                    message.delivery = old.delivery;
                    message.expires_at = message.expires_at.or(old.expires_at);
                    // An edit changes the text; Signal's clients can't take attachments or
                    // a quote away with one, so a backend that reports only the text keeps
                    // them.
                    if message.attachments.is_empty() {
                        message.attachments = std::mem::take(&mut old.attachments);
                    }
                    message.quote = message.quote.or_else(|| old.quote.take());
                    // The preview too, as long as its link is still in the text.
                    if message.preview.is_none()
                        && let Some(preview) = old.preview.take()
                        && format::links(&message.body).contains(&preview.url)
                    {
                        message.preview = Some(preview);
                    }
                    *old = message;
                    update.rows = self.selected.as_ref() == Some(&conversation);
                    update.conversations = true;
                }
            }
            Event::Deleted {
                conversation,
                target,
            } => {
                if let Some(message) = self.find_message(&conversation, &target) {
                    tombstone(message);
                    update.rows = self.selected.as_ref() == Some(&conversation);
                    update.conversations = true;
                }
            }
            Event::Expiring {
                conversation,
                expiries,
            } => {
                for (target, at) in expiries {
                    if let Some(message) = self.find_message(&conversation, &target) {
                        message.expires_at = Some(at);
                        update.rows = self.selected.as_ref() == Some(&conversation);
                    }
                }
            }
            Event::Expired {
                conversation,
                targets,
            } => {
                if let Some(history) = self.history.get_mut(&conversation) {
                    let before = history.len();
                    history.retain(|m| !targets.contains(&m.reference()));
                    if history.len() != before {
                        if self.selected.as_ref() == Some(&conversation) {
                            update.messages = MessagesUpdate::Reset;
                        }
                        update.conversations = true;
                    }
                }
            }
            Event::Settings {
                conversation,
                settings,
            } => {
                let Some(existing) = self.conversations.iter_mut().find(|c| c.id == conversation)
                else {
                    return update;
                };
                let read_further = settings.last_read > existing.settings.last_read;
                existing.settings = settings;
                if read_further {
                    // Read on another device: what is still unread is what came after.
                    let last_read = existing.settings.last_read;
                    let unread = self
                        .history
                        .get(&conversation)
                        .map_or(0, |history| unread_after(history, last_read).count());
                    match unread {
                        0 => {
                            self.unread.remove(&conversation);
                            self.notifications
                                .pending
                                .push(Notice::Withdraw(conversation.clone()));
                        }
                        n => {
                            let count = self.unread.entry(conversation).or_default();
                            *count = (*count).min(n as u32);
                        }
                    }
                }
                self.sort_conversations();
                update.conversations = true;
            }
            Event::Stories(stories) => {
                // The story shown stays shown, wherever it is in the new list.
                let shown = self
                    .current_story()
                    .map(|(story, _, _)| (story.author_id.clone(), story.timestamp));
                self.stories = stories;
                self.story_view = self.story_view.take().and_then(|(id, _)| {
                    let (author, timestamp) = shown.as_ref()?;
                    let at = self
                        .stories_of(&id)
                        .position(|s| &s.author_id == author && s.timestamp == *timestamp)?;
                    Some((id, at))
                });
                update.conversations = true;
            }
            Event::GroupCreated(id) => {
                if let Some(command) = self.select(&id) {
                    self.queued.push(command);
                }
                update = update.merge(Update::reset());
            }
            Event::GroupPreview(preview) => {
                if let Some(join) = &mut self.join
                    && join.link.trim() == preview.link
                {
                    join.preview = Some(preview);
                    join.busy = false;
                }
            }
            Event::GroupJoined(id) => {
                self.join = None;
                if let Some(command) = self.select(&id) {
                    self.queued.push(command);
                }
                update = update.merge(Update::reset());
            }
            Event::Folders(folders) => {
                self.folders = folders;
                self.folder = self.folder.filter(|&at| at < self.folders.len());
                update.conversations = true;
            }
            Event::StickerPacks(packs) => {
                self.sticker_packs = Some(Some(packs));
            }
            Event::StorageRead => {
                self.requests_known = true;
                update.conversations = true;
            }
            Event::Cleared(conversation) => {
                self.history.remove(&conversation);
                self.loaded.remove(&conversation);
                self.older.remove(&conversation);
                self.unread.remove(&conversation);
                if self.selected.as_ref() == Some(&conversation) {
                    update = update.merge(self.deselect());
                }
                update.conversations = true;
            }
            Event::Found { conversation, then } => match then {
                crate::AfterFinding::Open => {
                    if let Some(command) = self.select(&conversation) {
                        self.queued.push(command);
                    }
                    update = update.merge(Update::reset());
                    update.conversations = true;
                }
                crate::AfterFinding::Choose => {
                    let chosen = self
                        .group_draft
                        .as_ref()
                        .is_some_and(|draft| draft.chosen.contains(&conversation));
                    if !chosen {
                        self.toggle_group_member(&conversation);
                    }
                    update.conversations = true;
                }
            },
            Event::Trust {
                conversation,
                trust,
            } => {
                if let Some((shown, Some(details))) = &mut self.details
                    && *shown == conversation
                {
                    details.trust = trust;
                }
                if let Some(listed) = self.conversations.iter_mut().find(|c| c.id == conversation) {
                    listed.trust = trust;
                    update.conversations = true;
                }
            }
            Event::Details {
                conversation,
                details,
            } => {
                if let Some((shown, shown_details)) = &mut self.details
                    && *shown == conversation
                {
                    *shown_details = Some(details);
                }
            }
            Event::Media {
                conversation,
                messages,
            } => {
                if let Some((shown, media)) = &mut self.media
                    && *shown == conversation
                {
                    *media = Some(messages);
                }
            }
            Event::SearchResults {
                conversation,
                query,
                messages,
            } => {
                if let Some(search) = &mut self.chat_search
                    && search.conversation == conversation
                    && search.query == query
                {
                    search.results = Some(messages);
                }
            }
            Event::Unread(counts) => {
                for (conversation, count) in counts {
                    if count > 0 && self.selected.as_ref() != Some(&conversation) {
                        self.unread.insert(conversation, count);
                    }
                }
                update.conversations = true;
            }
            Event::Error(error) => {
                if let Some(join) = &mut self.join {
                    join.busy = false;
                }
                self.error = Some(error);
            }
        }
        update
    }

    fn find_message(
        &mut self,
        conversation: &ConversationId,
        target: &MessageRef,
    ) -> Option<&mut ChatMessage> {
        self.history
            .get_mut(conversation)?
            .iter_mut()
            .find(|m| m.is(target))
    }

    /// The selected conversation is in front of the user: its messages count as read. Not
    /// before its stored history is here, though: only part of what is unread is known until
    /// then, and the rest would be passed over for good. The history's arrival marks it.
    fn mark_seen(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.loaded.contains(&id) {
            if let Some(command) = self.mark_read(id) {
                self.queued.push(command);
            }
        } else {
            self.unread.remove(&id);
            self.opened_unloaded.insert(id);
        }
    }

    /// Counts a conversation's messages as read; returns the [`Command::MarkRead`] for those
    /// that weren't.
    fn mark_read(&mut self, id: ConversationId) -> Option<Command> {
        self.unread.remove(&id);
        let conversation = self.conversations.iter_mut().find(|c| c.id == id)?;
        let settings = &mut conversation.settings;
        let late = self.late.remove(&id).unwrap_or_default();
        let unread: Vec<&ChatMessage> = self
            .history
            .get(&id)
            .map(|history| {
                history
                    .iter()
                    .filter(|m| !m.outgoing && !m.notice)
                    .filter(|m| m.timestamp > settings.last_read || late.contains(&m.reference()))
                    .filter(|m| !self.read.contains(&m.reference()))
                    .collect()
            })
            .unwrap_or_default();
        if unread.is_empty() && !settings.marked_unread {
            return None;
        }
        settings.marked_unread = false;
        // Senders choose their messages' dates: one from the future mustn't make everything
        // until then count as read.
        if let Some(newest) = unread.iter().map(|m| m.timestamp).max() {
            settings.last_read = settings.last_read.max(newest.min(now_millis()));
        }
        self.read.extend(unread.iter().map(|m| m.reference()));
        let expiring = unread
            .iter()
            .filter_map(|m| Some((m.reference(), m.expire_timer?)))
            .collect();
        let messages = unread.iter().map(|m| m.reference()).collect();
        Some(Command::MarkRead {
            conversation: id,
            messages,
            expiring,
        })
    }

    /// Commands that followed from what happened since the last call (read receipts and
    /// the like); to be sent to the backend.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.queued)
    }

    /// Selects a conversation; until its stored history has arrived, returns the command
    /// that fetches it (afterwards live events keep it current). Asking again after a failed
    /// load retries it; a duplicate answer is harmless because inserts are deduplicated.
    pub fn select(&mut self, id: &ConversationId) -> Option<Command> {
        if !self.conversations.iter().any(|c| &c.id == id) {
            return None;
        }
        if self.selected.as_ref() != Some(id) {
            self.media = None;
            self.details = None;
            self.chat_search = None;
            self.pending_jump = None;
            self.highlight = None;
            self.replying = None;
            self.editing = None;
            self.selection.clear();
            self.selection_anchor = None;
            self.keep_draft_mentions();
            self.draft_mentions = self
                .drafts
                .get(id)
                .map(|(_, m)| m.clone())
                .unwrap_or_default();
            self.staged.clear();
            self.read_mark = self.conversation(id).map(|c| c.settings.last_read);
        }
        self.selected = Some(id.clone());
        self.mark_seen();
        // Picking a search result ends the search; the chosen chat stays on the list.
        self.filter.clear();
        self.notifications
            .pending
            .push(Notice::Withdraw(id.clone()));
        (!self.loaded.contains(id)).then(|| Command::LoadHistory {
            conversation: id.clone(),
            before: None,
        })
    }

    /// Asks for the selected conversation's next older page of history, e.g. when scrolled to
    /// the top; once at a time, and not past the beginning.
    pub fn load_older(&mut self) -> Option<Command> {
        let id = self.selected.clone()?;
        let before = (*self.older.get(&id)?)?;
        if !self.loading_older.insert(id.clone()) {
            return None;
        }
        Some(Command::LoadHistory {
            conversation: id,
            before: Some(before),
        })
    }

    /// The selected conversation's newest page is loaded but older messages are still in the
    /// store.
    pub fn has_older(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.older.get(id))
            .is_some_and(Option::is_some)
    }

    /// Answers a conversation that needn't be the selected one, e.g. from a notification.
    /// Counts as reading it. Under the same conditions as the composer: not while catching
    /// up, nor to blocked chats, left groups or unanswered requests (writing would accept
    /// one, and hand the stranger our profile).
    pub fn reply(&mut self, id: &ConversationId, text: &str) -> (Option<Command>, Update) {
        let body = text.trim();
        if body.is_empty() || !self.conversation(id).is_some_and(|c| self.can_write_to(c)) {
            return (None, Update::default());
        }
        if let Some(command) = self.mark_read(id.clone()) {
            self.queued.push(command);
        }
        self.notifications
            .pending
            .push(Notice::Withdraw(id.clone()));
        let (body, styles) = format::parse(body);
        let command = Command::Send {
            files: Vec::new(),
            conversation: id.clone(),
            timestamp: now_millis(),
            body,
            quote: None,
            styles,
            mentions: Vec::new(),
        };
        let update = self.show_sending(&command);
        (Some(command), update)
    }

    pub fn set_notifications(&mut self, enabled: bool, content: NotificationContent) {
        self.notifications.enabled = enabled;
        self.notifications.content = content;
    }

    /// Whether the window has the user's attention; messages for the open conversation of a
    /// focused window don't notify.
    pub fn set_window_focused(&mut self, focused: bool) {
        let gained = focused && !self.notifications.window_focused;
        self.notifications.window_focused = focused;
        if let (true, Some(id)) = (focused, &self.selected) {
            self.notifications
                .pending
                .push(Notice::Withdraw(id.clone()));
        }
        if gained {
            self.mark_seen();
        }
    }

    /// Notifications to show or withdraw since the last call.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notifications.pending)
    }

    /// A conversation's picture, if known.
    pub fn avatar(&self, id: &ConversationId) -> Option<&Arc<Picture>> {
        self.avatars.get(id)
    }

    pub fn conversation(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| &c.id == id)
    }

    fn notify_message(&mut self, id: &ConversationId, sender: String, body: String) {
        let muted = self
            .conversation(id)
            .is_some_and(|c| c.settings.muted(now_millis()));
        let n = &mut self.notifications;
        if !n.enabled || muted || (n.window_focused && self.selected.as_ref() == Some(id)) {
            return;
        }
        if !self.synced {
            *n.caught_up.entry(id.clone()).or_default() += 1;
            return;
        }
        let group = self
            .conversations
            .iter()
            .find(|c| &c.id == id)
            .filter(|c| c.is_group());
        let name = match group {
            Some(group) => format!("{sender} in {}", group.title),
            None => sender,
        };
        let (title, body) = match n.content {
            NotificationContent::NameAndMessage => (name, body),
            NotificationContent::NameOnly => (name, "New message".into()),
            NotificationContent::Nothing => ("b33p".into(), "New message".into()),
        };
        n.pending.push(Notice::Show {
            conversation: Some(id.clone()),
            title,
            body,
        });
    }

    fn announce_caught_up(&mut self) {
        let n = &mut self.notifications;
        let chats = n.caught_up.len();
        let messages: usize = n.caught_up.drain().map(|(_, count)| count).sum();
        if messages == 0 || n.window_focused {
            return;
        }
        let body = match (messages, chats) {
            (1, _) => "1 new message".to_string(),
            (_, 1) => format!("{messages} new messages"),
            _ => format!("{messages} new messages in {chats} chats"),
        };
        n.pending.push(Notice::Show {
            conversation: None,
            title: "b33p".into(),
            body,
        });
    }

    /// Validates the password form. On success returns the unlock command and shows the
    /// connecting screen; otherwise puts the reason on the password screen.
    pub fn unlock(&mut self, password: &str, confirmation: &str) -> Option<Command> {
        let Screen::Password { new, error } = &mut self.screen else {
            return None;
        };
        let problem = if password.is_empty() {
            Some("Enter a password.".to_string())
        } else if *new && password.chars().count() < MIN_PASSWORD_LEN {
            Some(format!("Use at least {MIN_PASSWORD_LEN} characters."))
        } else if *new && password != confirmation {
            Some("The passwords don't match.".to_string())
        } else {
            None
        };
        if problem.is_some() {
            *error = problem;
            return None;
        }
        self.screen = Screen::Connecting;
        Some(Command::Unlock(Password::new(password)))
    }

    /// Whether [`ChatState::compose`] would send `text`: not to blocked chats or unanswered
    /// requests.
    pub fn can_compose(&self, text: &str) -> bool {
        self.selected().is_some_and(|c| self.can_write_to(c))
            && (!text.trim().is_empty() || !self.staged.is_empty())
    }

    /// Whether anything may be sent to `conversation` now: once caught up, and not to a
    /// blocked chat, a group we left, a contact whose verified safety number changed (until
    /// that is confirmed), or a message request not answered yet. Every way of
    /// sending (composing, replying from a notification, forwarding, stickers, reactions)
    /// goes by this.
    pub fn can_write_to(&self, conversation: &Conversation) -> bool {
        self.synced
            && !conversation.settings.blocked
            && !conversation.left
            && conversation.trust != crate::Trust::Changed
            && !self.is_request(conversation)
    }

    /// Accepts the new safety number of a contact whose verified one changed, so that they can
    /// be written to again: it is no longer verified (that takes comparing it again).
    pub fn confirm_safety_number(&self, id: &ConversationId) -> Option<Command> {
        let conversation = self.conversation(id)?;
        (conversation.trust == crate::Trust::Changed).then(|| Command::SetVerified {
            conversation: id.clone(),
            verified: false,
        })
    }

    /// Adds files to send with the next message.
    pub fn stage_files(&mut self, files: impl IntoIterator<Item = std::path::PathBuf>) {
        self.staged.extend(files);
    }

    pub fn unstage(&mut self, index: usize) {
        if index < self.staged.len() {
            self.staged.remove(index);
        }
    }

    /// Files waiting to go out with the next message.
    pub fn staged(&self) -> &[std::path::PathBuf] {
        &self.staged
    }

    /// Opens an attachment of the selected conversation in the desktop's default app.
    pub fn open_attachment(&self, id: &AttachmentId) -> Option<Command> {
        let attachment = self
            .find_attachment(id)
            .filter(|a| a.kind != AttachmentKind::ViewOnce)?;
        Some(Command::OpenAttachment(attachment.clone()))
    }

    /// Back to the chat list, e.g. on a narrow window that shows one or the other.
    pub fn deselect(&mut self) -> Update {
        self.keep_draft_mentions();
        self.selected = None;
        self.replying = None;
        self.editing = None;
        self.selection.clear();
        self.selection_anchor = None;
        self.read_mark = None;
        self.draft_mentions.clear();
        self.staged.clear();
        Update::reset()
    }

    /// The selected chat's draft, as left there; empty if none.
    pub fn draft(&self) -> &str {
        self.selected
            .as_ref()
            .and_then(|id| self.drafts.get(id))
            .map_or("", |(text, _)| text)
    }

    /// Before leaving the selected chat: its draft keeps whom it mentions.
    fn keep_draft_mentions(&mut self) {
        let mentions = std::mem::take(&mut self.draft_mentions);
        if let Some((_, kept)) = self
            .selected
            .as_ref()
            .and_then(|id| self.drafts.get_mut(id))
        {
            *kept = mentions;
        }
    }

    /// Keeps `text` as the selected chat's draft, unless it is an edit.
    fn keep_draft(&mut self, text: &str) {
        let Some(id) = self.selected.clone().filter(|_| self.editing.is_none()) else {
            return;
        };
        if text.is_empty() {
            self.drafts.remove(&id);
        } else {
            self.drafts.entry(id).or_default().0 = text.to_owned();
        }
    }

    /// Sends `text` to the selected conversation, as a reply if one is being written, or
    /// saves it as the edit being written. A new message shows right away, as sending, until
    /// the sent one replaces it ([`Event::Message`]) or sending fails ([`Event::SendFailed`]).
    pub fn compose(&mut self, text: &str) -> (Option<Command>, Update) {
        if !self.can_compose(text) {
            return (None, Update::default());
        }
        let Some(conversation) = self.selected.clone() else {
            return (None, Update::default());
        };
        self.typing_sent = None;
        // An edit of a message that has gone since is no edit.
        if self.editing().is_none() {
            self.editing = None;
        }
        let (body, styles) = format::parse(text.trim());
        let mentions = mentions_in(&body, &std::mem::take(&mut self.draft_mentions));
        if let Some(target) = self.editing.take() {
            let command = Command::Edit {
                conversation,
                target,
                body,
                styles,
                mentions,
            };
            return (Some(command), Update::default());
        }
        self.drafts.remove(&conversation);
        let command = Command::Send {
            conversation,
            timestamp: now_millis(),
            body,
            quote: self.replying.take(),
            files: std::mem::take(&mut self.staged),
            styles,
            mentions,
        };
        let mut update = self.show_sending(&command);
        update.scroll_to_end = true;
        (Some(command), update)
    }

    /// Shows the message `send` sends, as sending, until it is sent or fails.
    fn show_sending(&mut self, send: &Command) -> Update {
        let Command::Send {
            conversation,
            timestamp,
            body,
            quote,
            files,
            styles,
            mentions,
            ..
        } = send
        else {
            return Update::default();
        };
        // The files go up first, which takes a while: until then, their names stand in.
        let mut shown = body.clone();
        for file in files {
            let name = file.file_name().map_or_else(
                || file.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            if !shown.is_empty() {
                shown.push('\n');
            }
            shown.push_str(&format!("📎 {name}"));
        }
        self.apply(Event::Message(ChatMessage {
            conversation: conversation.clone(),
            sender: "You".into(),
            body: shown,
            timestamp: *timestamp,
            outgoing: true,
            quote: quote.clone(),
            styles: styles.clone(),
            mentions: mentions.clone(),
            unsent: Some(Unsent::Sending),
            ..Default::default()
        }))
    }

    /// A chat from someone the user hasn't accepted (here, on the phone, or by writing to
    /// them): it waits for Accept, Delete or Block, and sends no read receipts. Unknown until
    /// the phone's settings were read.
    pub fn is_request(&self, conversation: &Conversation) -> bool {
        self.requests_known
            && !conversation.note_to_self
            && !conversation.settings.accepted
            && !conversation.settings.blocked
            // Written to from another device, before b33p knew.
            && !self
                .history
                .get(&conversation.id)
                .is_some_and(|history| history.iter().any(|m| m.outgoing))
    }

    /// Answers the message request in `id`.
    pub fn respond(&mut self, id: &ConversationId, response: RequestResponse) -> Option<Command> {
        let conversation = self.conversations.iter_mut().find(|c| &c.id == id)?;
        match response {
            RequestResponse::Accept => conversation.settings.accepted = true,
            RequestResponse::Block => conversation.settings.blocked = true,
            // When the backend confirms with `Event::Cleared`.
            RequestResponse::Delete => {}
        }
        Some(Command::Respond {
            conversation: id.clone(),
            response,
        })
    }

    /// Asks for the installed sticker packs, the first time.
    pub fn load_stickers(&mut self) -> Option<Command> {
        if self.sticker_packs.is_some() {
            return None;
        }
        self.sticker_packs = Some(None);
        Some(Command::LoadStickers)
    }

    /// The installed sticker packs: `Some(None)` while they are gathered.
    pub fn sticker_packs(&self) -> Option<Option<&[crate::StickerPack]>> {
        self.sticker_packs.as_ref().map(Option::as_deref)
    }

    /// Sends a sticker to the selected conversation.
    pub fn send_sticker(&mut self, pack: &str, sticker: u32) -> Option<Command> {
        let conversation = self.selected()?;
        if !self.can_write_to(conversation) {
            return None;
        }
        Some(Command::SendSticker {
            conversation: conversation.id.clone(),
            pack: pack.to_owned(),
            sticker,
        })
    }

    /// Starts choosing people for a new group, or to add to `adding_to`.
    pub fn open_group_draft(&mut self, adding_to: Option<ConversationId>) {
        self.group_draft = Some(GroupDraft {
            adding_to,
            chosen: Vec::new(),
        });
    }

    pub fn close_group_draft(&mut self) {
        self.group_draft = None;
    }

    /// Starts joining a group through its invite link, looking `link` up at once if given.
    pub fn open_join(&mut self, link: Option<&str>) -> Option<Command> {
        self.join = Some(JoinDraft {
            link: link.unwrap_or_default().to_owned(),
            preview: None,
            busy: false,
        });
        link.and_then(|link| self.preview_link(link))
    }

    pub fn close_join(&mut self) {
        self.join = None;
    }

    /// Looks up what the group behind `link` shows of itself; nothing while not synced or
    /// not joining.
    pub fn preview_link(&mut self, link: &str) -> Option<Command> {
        let synced = self.synced;
        let join = self.join.as_mut().filter(|_| synced)?;
        let link = link.trim();
        if link.is_empty() {
            return None;
        }
        join.link = link.to_owned();
        join.preview = None;
        join.busy = true;
        Some(Command::PreviewGroupLink(link.to_owned()))
    }

    /// Joins the group looked up with [`Self::preview_link`], or asks to.
    pub fn join_group(&mut self) -> Option<Command> {
        let join = self.join.as_mut()?;
        let preview = join.preview.as_ref().filter(|_| !join.busy)?;
        join.busy = true;
        Some(Command::JoinGroup(preview.link.clone()))
    }

    /// While joining through a link: the link typed, what its group shows of itself once
    /// looked up, and whether that or the joining is under way.
    pub fn join(&self) -> Option<(&str, Option<&crate::GroupPreview>, bool)> {
        let join = self.join.as_ref()?;
        Some((&join.link, join.preview.as_ref(), join.busy))
    }

    /// Looks up who a typed phone number belongs to, then opens their chat or chooses them for
    /// the group being made; nothing for text that isn't a phone number.
    pub fn find_number(&self, text: &str, then: crate::AfterFinding) -> Option<Command> {
        (self.synced && looks_like_number(text)).then(|| Command::FindNumber {
            number: text.trim().to_owned(),
            then,
        })
    }

    /// Chooses someone for the group, or unchooses them.
    pub fn toggle_group_member(&mut self, id: &ConversationId) {
        if let Some(draft) = &mut self.group_draft {
            match draft.chosen.iter().position(|c| c == id) {
                Some(at) => {
                    draft.chosen.remove(at);
                }
                None => draft.chosen.push(id.clone()),
            }
        }
    }

    /// While choosing: whether it is for a new group, and who is chosen.
    pub fn group_draft(&self) -> Option<(bool, &[ConversationId])> {
        let draft = self.group_draft.as_ref()?;
        Some((draft.adding_to.is_none(), &draft.chosen))
    }

    /// Makes the new group named `title`, or adds the chosen people to the group.
    pub fn finish_group_draft(&mut self, title: &str) -> Option<Command> {
        let draft = self.group_draft.as_ref()?;
        let command = match draft.adding_to.clone() {
            None => self.create_group(title, &draft.chosen.clone())?,
            Some(group) => {
                let members = draft
                    .chosen
                    .iter()
                    .filter_map(ConversationId::contact_id)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if members.is_empty() {
                    return None;
                }
                self.edit_group(&group, crate::GroupEdit::AddMembers(members))?
            }
        };
        self.group_draft = None;
        Some(command)
    }

    /// People who can be chosen: contacts whose name contains `filter`, without those
    /// already in the group being added to, and without strangers whose message request is
    /// unanswered (adding them would answer it).
    pub fn group_candidates(&self, filter: &str) -> Vec<&Conversation> {
        let filter = filter.trim().to_lowercase();
        let already: Vec<String> = self
            .group_draft
            .as_ref()
            .and_then(|draft| self.conversation(draft.adding_to.as_ref()?))
            .map(|group| {
                group
                    .members
                    .iter()
                    .map(|p| ConversationId::contact(&p.service_id).0)
                    .collect()
            })
            .unwrap_or_default();
        let mut candidates: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|c| !c.is_group() && !c.note_to_self && !c.settings.blocked)
            .filter(|c| !self.is_request(c))
            .filter(|c| !already.contains(&c.id.0))
            .filter(|c| c.title.to_lowercase().contains(&filter))
            .collect();
        candidates.sort_by_cached_key(|c| c.title.to_lowercase());
        candidates
    }

    /// Makes a group named `title` with these contacts.
    pub fn create_group(&mut self, title: &str, members: &[ConversationId]) -> Option<Command> {
        let title = title.trim();
        if title.is_empty() || !self.synced {
            return None;
        }
        let members = members
            .iter()
            .filter_map(ConversationId::contact_id)
            .map(str::to_owned)
            .collect();
        Some(Command::CreateGroup {
            title: title.to_owned(),
            members,
        })
    }

    /// Changes a group.
    pub fn edit_group(&mut self, id: &ConversationId, edit: crate::GroupEdit) -> Option<Command> {
        use crate::GroupEdit::{AcceptInvitation, CancelJoinRequest, DeclineInvitation};
        let invitation = matches!(edit, AcceptInvitation | DeclineInvitation);
        let cancelling = edit == CancelJoinRequest;
        // An invitation is answered, and a request taken back, by someone not in the group
        // yet; anything else needs being in.
        let group = self.conversation(id).filter(|c| {
            c.is_group()
                && if invitation {
                    c.invited
                } else if cancelling {
                    c.requested
                } else {
                    !c.left
                }
        })?;
        if let crate::GroupEdit::Title(title) = &edit
            && (title.trim().is_empty() || title.trim() == group.title)
        {
            return None;
        }
        Some(Command::EditGroup {
            conversation: id.clone(),
            edit,
        })
    }

    /// Blocks or unblocks a conversation.
    pub fn block(&mut self, id: &ConversationId, blocked: bool) -> Option<Command> {
        self.change_settings(id, |s| s.blocked = blocked)?;
        Some(Command::Block {
            conversation: id.clone(),
            blocked,
        })
    }

    /// Shows the selected conversation's details page; returns the command that gathers what it
    /// shows.
    pub fn open_details(&mut self) -> Option<Command> {
        let id = self.selected.clone()?;
        self.details = Some((id.clone(), None));
        Some(Command::LoadDetails(id))
    }

    pub fn close_details(&mut self) {
        self.details = None;
    }

    /// Marks the safety number on the open details page verified, or clears that.
    pub fn set_verified(&mut self, verified: bool) -> Option<Command> {
        let (conversation, details) = self.details.as_ref()?;
        details.as_ref()?.safety_number.as_ref()?;
        Some(Command::SetVerified {
            conversation: conversation.clone(),
            verified,
        })
    }

    /// The details page, if shown: `Some(None)` while it is gathered.
    pub fn details(&self) -> Option<Option<&Details>> {
        self.details.as_ref().map(|(_, details)| details.as_ref())
    }

    /// Shows the selected conversation's media, files and links; returns the command that
    /// gathers them.
    pub fn open_media(&mut self) -> Option<Command> {
        let id = self.selected.clone()?;
        self.media = Some((id.clone(), None));
        Some(Command::LoadMedia(id))
    }

    pub fn close_media(&mut self) {
        self.media = None;
    }

    /// The media overview, if shown: `Some(None)` while it is gathered.
    pub fn media(&self) -> Option<Option<MediaOverview<'_>>> {
        let (_, messages) = self.media.as_ref()?;
        let Some(messages) = messages else {
            return Some(None);
        };
        let mut overview = MediaOverview {
            media: Vec::new(),
            files: Vec::new(),
            links: Vec::new(),
        };
        for message in messages.iter().filter(|m| !m.deleted) {
            for attachment in &message.attachments {
                match attachment.kind {
                    AttachmentKind::Image | AttachmentKind::Video => {
                        overview.media.push(attachment)
                    }
                    AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::VoiceNote => {
                        overview.files.push((message, attachment))
                    }
                    AttachmentKind::Sticker
                    | AttachmentKind::LongText
                    | AttachmentKind::ViewOnce => {}
                }
            }
            for link in format::links(&message.body) {
                overview.links.push((message, link));
            }
        }
        Some(Some(overview))
    }

    /// Who has stories: unseen ones first, then the newest.
    pub fn story_rings(&self) -> Vec<StoryRing<'_>> {
        let mut rings: Vec<StoryRing<'_>> = Vec::new();
        for story in self.stories.iter().filter(|s| self.story_shown(s)) {
            match rings
                .iter_mut()
                .find(|r| *r.conversation == story.conversation)
            {
                Some(ring) => {
                    ring.latest = ring.latest.max(story.timestamp);
                    ring.count += 1;
                    ring.unviewed |= !story.viewed;
                }
                None => rings.push(StoryRing {
                    conversation: &story.conversation,
                    title: self
                        .conversation(&story.conversation)
                        .map_or_else(|| story.author.clone(), |c| c.title.clone()),
                    latest: story.timestamp,
                    count: 1,
                    unviewed: !story.viewed,
                }),
            }
        }
        rings.sort_by_key(|r| (!r.unviewed, Reverse(r.latest)));
        rings
    }

    /// Stories not seen yet, e.g. for a dot on the stories button.
    pub fn unviewed_stories(&self) -> usize {
        self.stories
            .iter()
            .filter(|s| !s.viewed && self.story_shown(s))
            .count()
    }

    /// Whether a story is listed: not from strangers (chats that are message requests, or
    /// none at all), as watching it would tell them.
    fn story_shown(&self, story: &crate::Story) -> bool {
        story.outgoing
            || self
                .conversation(&story.conversation)
                .is_some_and(|c| !self.is_request(c) && !c.settings.blocked)
    }

    /// A conversation's stories, if they are shown at all.
    fn stories_of<'a>(&'a self, id: &'a ConversationId) -> impl Iterator<Item = &'a crate::Story> {
        self.stories
            .iter()
            .filter(move |s| &s.conversation == id && self.story_shown(s))
    }

    /// Shows a conversation's stories, from the first not seen yet.
    pub fn open_story(&mut self, id: &ConversationId) -> Option<Command> {
        let at = self
            .stories_of(id)
            .position(|s| !s.viewed)
            .unwrap_or_default();
        self.stories_of(id).nth(at)?;
        self.story_view = Some((id.clone(), at));
        self.view_current()
    }

    /// The next (`by` 1) or previous (-1) story of the same conversation; past the last, the
    /// viewer closes.
    pub fn step_story(&mut self, by: isize) -> Option<Command> {
        let (id, at) = self.story_view.clone()?;
        let count = self.stories_of(&id).count();
        match at.checked_add_signed(by).filter(|&next| next < count) {
            Some(next) => {
                self.story_view = Some((id, next));
                self.view_current()
            }
            None if by > 0 => {
                self.story_view = None;
                None
            }
            None => None,
        }
    }

    pub fn close_story(&mut self) {
        self.story_view = None;
    }

    /// The story shown, which of how many it is.
    pub fn current_story(&self) -> Option<(&crate::Story, usize, usize)> {
        let (id, at) = self.story_view.as_ref()?;
        let story = self.stories_of(id).nth(*at)?;
        Some((story, *at, self.stories_of(id).count()))
    }

    /// Marks the story shown as seen, the first time.
    fn view_current(&mut self) -> Option<Command> {
        let (id, at) = self.story_view.clone()?;
        // A conversation's stories are shown all or none, so its index holds among them all.
        self.stories_of(&id).nth(at)?;
        let story = self
            .stories
            .iter_mut()
            .filter(|s| s.conversation == id)
            .nth(at)?;
        if story.viewed {
            return None;
        }
        story.viewed = true;
        Some(Command::ViewStory(MessageRef {
            author_id: story.author_id.clone(),
            timestamp: story.timestamp,
        }))
    }

    /// Groups a story can be posted to, by name.
    pub fn story_groups(&self) -> Vec<&Conversation> {
        let mut groups: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|c| c.is_group() && !c.left)
            .collect();
        groups.sort_by_cached_key(|c| c.title.to_lowercase());
        groups
    }

    /// Posts a story to a group.
    pub fn send_story(&mut self, id: &ConversationId, story: crate::NewStory) -> Option<Command> {
        self.conversation(id).filter(|c| c.is_group() && !c.left)?;
        if let crate::NewStory::Text(text) = &story
            && text.trim().is_empty()
        {
            return None;
        }
        Some(Command::SendStory {
            conversation: id.clone(),
            story,
        })
    }

    /// Opens the search in the selected conversation.
    pub fn open_chat_search(&mut self) {
        if let Some(conversation) = self.selected.clone() {
            self.chat_search = Some(ChatSearch {
                conversation,
                query: String::new(),
                results: None,
            });
        }
    }

    pub fn close_chat_search(&mut self) {
        self.chat_search = None;
    }

    /// Searches the selected conversation for `query`.
    pub fn search_chat(&mut self, query: &str) -> Option<Command> {
        let search = self.chat_search.as_mut()?;
        let query = query.trim();
        search.query = query.to_owned();
        search.results = (query.is_empty()).then(Vec::new);
        (!query.is_empty()).then(|| Command::SearchChat {
            conversation: search.conversation.clone(),
            query: query.to_owned(),
        })
    }

    /// The search in the selected conversation, if open: what is looked for, and what was found
    /// (`None` while looking).
    pub fn chat_search(&self) -> Option<(&str, Option<&[ChatMessage]>)> {
        let search = self.chat_search.as_ref()?;
        Some((&search.query, search.results.as_deref()))
    }

    /// Shows the `index`th search result in the chat: closes the search, marks the message, and
    /// scrolls to it, loading older history first if needed (through
    /// [`ChatState::take_commands`]).
    pub fn show_result(&mut self, index: usize) -> Update {
        let target = self
            .chat_search
            .as_ref()
            .and_then(|search| search.results.as_ref()?.get(index))
            .map(ChatMessage::reference);
        let Some(target) = target else {
            return Update::default();
        };
        self.chat_search = None;
        self.highlight = Some(target.clone());
        self.pending_jump = Some(target);
        self.continue_jump();
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// Scrolls to the pending search result if it is loaded, or asks for the page before.
    fn continue_jump(&mut self) {
        let Some(target) = self.pending_jump.clone() else {
            return;
        };
        let id = self.selected.clone();
        let found = id
            .as_ref()
            .and_then(|id| self.history.get(id))
            .and_then(|history| history.iter().position(|m| m.is(&target)));
        match found {
            Some(row) => {
                self.pending_jump = None;
                let count = self.jump.map_or(1, |(_, count)| count + 1);
                self.jump = Some((row, count));
            }
            None => match self.load_older() {
                Some(command) => self.queued.push(command),
                // Not in the store after all (deleted meanwhile).
                None if !self.has_older() => self.pending_jump = None,
                None => {}
            },
        }
    }

    /// The row the UI should scroll to, with a number that changes with each request, so the
    /// UI scrolls once per request.
    pub fn jump(&self) -> Option<(usize, u64)> {
        self.jump
    }

    /// Whether a message can be forwarded: not deleted, not view-once.
    pub fn can_forward(message: &ChatMessage) -> bool {
        !message.deleted
            && message.unsent.is_none()
            && !message.notice
            && message.timer_change.is_none()
            && !message
                .attachments
                .iter()
                .any(|a| a.kind == AttachmentKind::ViewOnce)
    }

    /// Starts forwarding the `index`th message of [`ChatState::messages`]; the target is chosen
    /// next, from [`ChatState::forward_targets`].
    pub fn start_forward(&mut self, index: usize) -> bool {
        let message = self.messages().nth(index).map(|view| view.message.clone());
        self.forwarding = message.filter(Self::can_forward).into_iter().collect();
        !self.forwarding.is_empty()
    }

    /// Starts forwarding the picked messages (see [`ChatState::toggle_selected`]), if every one
    /// of them can go.
    pub fn start_forward_selection(&mut self) -> bool {
        if !self.can_forward_selection() {
            return false;
        }
        self.forwarding = self.selection().into_iter().cloned().collect();
        true
    }

    pub fn forwarding(&self) -> bool {
        !self.forwarding.is_empty()
    }

    /// What is being forwarded, to confirm where it goes: a message's text, or what is
    /// attached when it has none; how many, for several.
    pub fn forwarded(&self) -> Option<String> {
        match self.forwarding.as_slice() {
            [] => None,
            [message] => Some(
                match (message.body.is_empty(), message.attachments.first()) {
                    (true, Some(attachment)) => describe_kind(attachment.kind).to_string(),
                    _ => message.body.clone(),
                },
            ),
            messages => Some(format!("{} messages", messages.len())),
        }
    }

    pub fn cancel_forward(&mut self) {
        self.forwarding.clear();
    }

    /// Where a message can be forwarded to: chats whose title contains `filter`, in list order;
    /// not those it couldn't go to (blocked, left, or asking to write).
    pub fn forward_targets(&self, filter: &str) -> Vec<&Conversation> {
        let filter = filter.trim().to_lowercase();
        self.conversations
            .iter()
            .filter(|c| self.can_write_to(c))
            .filter(|c| {
                if filter.is_empty() {
                    // As in the chat list, without the archive split.
                    c.is_group() || c.note_to_self || self.last_of(&c.id).is_some()
                } else {
                    c.title.to_lowercase().contains(&filter)
                }
            })
            .collect()
    }

    /// Forwards what is being forwarded to `id`: the command for the first message; those for
    /// the others, in order, come from [`ChatState::take_commands`].
    pub fn forward_to(&mut self, id: &ConversationId) -> Option<Command> {
        if !self.conversation(id).is_some_and(|c| self.can_write_to(c)) {
            return None;
        }
        let messages = std::mem::take(&mut self.forwarding);
        if messages.is_empty() {
            return None;
        }
        // The picked messages are done with once they went.
        let selection = self.selection();
        if messages.len() == selection.len()
            && messages
                .iter()
                .zip(selection)
                .all(|(m, s)| m.is(&s.reference()))
        {
            self.selection.clear();
            self.selection_anchor = None;
        }
        let mut commands = messages.into_iter().map(|message| {
            // A long message's full text, once fetched, goes as the body; until then, the
            // attachment holding it goes along.
            let (body, styles, attachments) = match self.full_text(&message) {
                Some(text) => (
                    text.to_owned(),
                    Vec::new(),
                    message
                        .attachments
                        .into_iter()
                        .filter(|a| a.kind != AttachmentKind::LongText)
                        .collect(),
                ),
                None => (message.body, message.styles, message.attachments),
            };
            Command::Forward {
                conversation: id.clone(),
                body,
                styles,
                attachments,
            }
        });
        let first = commands.next();
        let rest: Vec<_> = commands.collect();
        self.queued.extend(rest);
        first
    }

    /// Members of the selected group matching the "@…" the draft ends with, to mention.
    pub fn mention_candidates(&self, draft: &str) -> Vec<&Person> {
        const MAX: usize = 8;
        let Some(query) = mention_query(draft) else {
            return Vec::new();
        };
        let query = query.to_lowercase();
        let Some(group) = self.selected().filter(|c| c.is_group()) else {
            return Vec::new();
        };
        group
            .members
            .iter()
            .filter(|p| {
                p.name
                    .to_lowercase()
                    .split_whitespace()
                    .any(|word| word.starts_with(&query))
            })
            .take(MAX)
            .collect()
    }

    /// Mentions the `index`th of [`ChatState::mention_candidates`]: returns the draft with its
    /// "@…" completed to "@Name ".
    pub fn pick_mention(&mut self, draft: &str, index: usize) -> Option<String> {
        let person = (*self.mention_candidates(draft).get(index)?).clone();
        let query = mention_query(draft)?;
        let start = draft.len() - query.len() - 1;
        let completed = format!("{}@{} ", &draft[..start], person.name);
        self.keep_draft(&completed);
        if !self.draft_mentions.contains(&person) {
            self.draft_mentions.push(person);
        }
        Some(completed)
    }

    /// The composer's text changed: keeps it as the chat's draft, and tells the conversation we
    /// are typing, at most every few seconds, or that we stopped once it is empty.
    pub fn draft_changed(&mut self, text: &str) -> Option<Command> {
        self.keep_draft(text);
        let conversation = self.selected.clone()?;
        let now = now_millis();
        let typing = !text.trim().is_empty() && self.editing.is_none();
        let sent = self
            .typing_sent
            .as_ref()
            .filter(|(id, _)| *id == conversation)
            .map(|(_, at)| *at);
        match (typing, sent) {
            (true, Some(at)) if now.saturating_sub(at) < TYPING_REPEAT_MS => None,
            (false, None) => None,
            (true, _) => {
                self.typing_sent = Some((conversation.clone(), now));
                Some(Command::Typing {
                    conversation,
                    typing: true,
                })
            }
            (false, Some(_)) => {
                self.typing_sent = None;
                Some(Command::Typing {
                    conversation,
                    typing: false,
                })
            }
        }
    }

    /// Starts editing the `index`th message of [`ChatState::messages`], if it is ours and
    /// can still be edited; returns its text for the composer.
    pub fn edit(&mut self, index: usize) -> Option<String> {
        let view = self.messages().nth(index)?;
        let message = view.message;
        if !self.can_edit(message) {
            return None;
        }
        // As typed, so that sending it again keeps the formatting, spoilers above all, and
        // the mentions.
        let text = format::typed(view.body, view.styles);
        let reference = message.reference();
        let mentions = message
            .mentions
            .iter()
            .filter_map(|m| {
                let name = message.body.get(m.start..m.end)?.strip_prefix('@')?;
                Some(Person {
                    service_id: m.service_id.clone(),
                    name: name.to_owned(),
                })
            })
            .collect();
        self.draft_mentions = mentions;
        self.editing = Some(reference);
        self.replying = None;
        Some(text)
    }

    /// Our own message that is still there and recent enough; Signal allows editing as long as
    /// deleting for everyone.
    /// Also whether it can be deleted for everyone: the same rule.
    pub fn can_edit(&self, message: &ChatMessage) -> bool {
        message.outgoing
            && !message.deleted
            && !message.notice
            && message.unsent.is_none()
            && now_millis().saturating_sub(message.timestamp) < DELETE_FOR_EVERYONE_MS
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    /// The message whose text the composer replaces, if any.
    pub fn editing(&self) -> Option<&ChatMessage> {
        let target = self.editing.as_ref()?;
        self.history
            .get(self.selected.as_ref()?)?
            .iter()
            .find(|m| m.is(target))
    }

    /// Deletes the `index`th message of [`ChatState::messages`] here, or for everyone.
    pub fn delete(&mut self, index: usize, for_everyone: bool) -> (Option<Command>, Update) {
        let Some(message) = self.messages().nth(index).map(|v| v.message) else {
            return (None, Update::default());
        };
        if message.notice || for_everyone && !self.can_edit(message) {
            return (None, Update::default());
        }
        let target = message.reference();
        let conversation = message.conversation.clone();
        if message.unsent.is_some() {
            // Never went out (or is still going): nothing to tell anyone. A send still under
            // way finishes regardless, and the sent message shows up.
            let timestamp = message.timestamp;
            let history = self.history.get_mut(&conversation).expect("shown");
            history.retain(|m| !is_unsent_at(m, timestamp));
            return (None, Update::reset());
        }
        if self.editing.as_ref() == Some(&target) {
            self.editing = None;
        }
        let history = self.history.get_mut(&conversation).expect("shown");
        if for_everyone {
            if let Some(message) = history.iter_mut().find(|m| m.is(&target)) {
                tombstone(message);
            }
        } else {
            history.retain(|m| !m.is(&target));
        }
        let command = Command::Delete {
            conversation,
            target,
            for_everyone,
        };
        let update = Update::reset();
        (Some(command), update)
    }

    /// Picks the `index`th message of [`ChatState::messages`], or unpicks it. Picked messages
    /// are copied, forwarded or deleted together; see [`ChatState::selection`]. Notices can't
    /// be picked: there is nothing to do with them.
    pub fn toggle_selected(&mut self, index: usize) -> Update {
        let Some(message) = self.messages().nth(index).map(|v| v.message) else {
            return Update::default();
        };
        if message.notice {
            return Update::default();
        }
        let target = message.reference();
        match self.selection.iter().position(|s| *s == target) {
            Some(at) => {
                self.selection.remove(at);
            }
            None => self.selection.push(target.clone()),
        }
        self.selection_anchor = Some(target);
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// Picks every message from the one last picked (or unpicked) through the `index`th; just
    /// the `index`th if none was.
    pub fn select_through(&mut self, index: usize) -> Update {
        let history = self.selected_history();
        let anchor = self
            .selection_anchor
            .as_ref()
            .and_then(|anchor| history.iter().position(|m| m.is(anchor)));
        let (Some(from), true) = (anchor, index < history.len()) else {
            return self.toggle_selected(index);
        };
        let picked: Vec<MessageRef> = history[from.min(index)..=from.max(index)]
            .iter()
            .filter(|m| !m.notice)
            .map(ChatMessage::reference)
            .collect();
        for target in picked {
            if !self.selection.contains(&target) {
                self.selection.push(target);
            }
        }
        Update {
            rows: true,
            ..Update::default()
        }
    }

    pub fn clear_selection(&mut self) -> Update {
        if self.selection.is_empty() {
            return Update::default();
        }
        self.selection.clear();
        self.selection_anchor = None;
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// The picked messages still in the selected chat, in its order.
    pub fn selection(&self) -> Vec<&ChatMessage> {
        self.selected_history()
            .iter()
            .filter(|m| self.selection.iter().any(|s| m.is(s)))
            .collect()
    }

    /// What the picked messages say, one after the other, for copying: each its text, or what
    /// is attached when it has none.
    pub fn selected_text(&self) -> String {
        let text = |m: &ChatMessage| -> String {
            if m.deleted {
                return DELETED.to_string();
            }
            let body = self.full_text(m).unwrap_or(&m.body);
            if body.is_empty() {
                m.attachments
                    .first()
                    .map(|a| describe_kind(a.kind).to_string())
                    .unwrap_or_default()
            } else {
                body.to_string()
            }
        };
        self.selection()
            .into_iter()
            .map(text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Whether every picked message can be forwarded; see [`ChatState::can_forward`].
    pub fn can_forward_selection(&self) -> bool {
        let selection = self.selection();
        !selection.is_empty() && selection.iter().all(|m| Self::can_forward(m))
    }

    /// Whether every picked message can still be deleted for everyone.
    pub fn can_delete_selection_for_everyone(&self) -> bool {
        let selection = self.selection();
        !selection.is_empty() && selection.iter().all(|m| self.can_edit(m))
    }

    /// Deletes the picked messages, each as [`ChatState::delete`] would: the command for the
    /// first one; those for the others come from [`ChatState::take_commands`].
    pub fn delete_selection(&mut self, for_everyone: bool) -> (Option<Command>, Update) {
        let targets: Vec<MessageRef> = self.selection().iter().map(|m| m.reference()).collect();
        let mut commands = Vec::new();
        let mut update = Update::default();
        for target in targets {
            let Some(index) = self.messages().position(|v| v.message.is(&target)) else {
                continue;
            };
            let (command, done) = self.delete(index, for_everyone);
            commands.extend(command);
            update = update.merge(done);
        }
        self.selection.clear();
        self.selection_anchor = None;
        let mut commands = commands.into_iter();
        let first = commands.next();
        self.queued.extend(commands);
        (first, update)
    }

    /// Changes a conversation's settings here at once; the command makes it last.
    fn change_settings(
        &mut self,
        id: &ConversationId,
        change: impl FnOnce(&mut ChatSettings),
    ) -> Option<()> {
        let conversation = self.conversations.iter_mut().find(|c| &c.id == id)?;
        change(&mut conversation.settings);
        self.sort_conversations();
        Some(())
    }

    /// The pinned conversations, in order.
    pub fn pinned(&self) -> Vec<ConversationId> {
        let mut pinned: Vec<_> = self
            .conversations
            .iter()
            .filter_map(|c| Some((c.settings.pinned?, c.id.clone())))
            .collect();
        pinned.sort_by_key(|(at, _)| *at);
        pinned.into_iter().map(|(_, id)| id).collect()
    }

    /// Pins a conversation at the end of the pinned ones, or unpins it.
    pub fn toggle_pin(&mut self, id: &ConversationId) -> Option<Command> {
        let mut pinned = self.pinned();
        match pinned.iter().position(|p| p == id) {
            Some(at) => {
                pinned.remove(at);
            }
            None => {
                self.conversation(id)?;
                pinned.push(id.clone());
            }
        }
        Some(self.set_pins(pinned))
    }

    /// Moves a pinned conversation to `to` among the pinned ones, e.g. by dragging.
    pub fn move_pin(&mut self, id: &ConversationId, to: usize) -> Option<Command> {
        let mut pinned = self.pinned();
        let from = pinned.iter().position(|p| p == id)?;
        let id = pinned.remove(from);
        pinned.insert(to.min(pinned.len()), id);
        Some(self.set_pins(pinned))
    }

    /// Moves a pinned conversation `by` places up (negative) or down among the pinned ones.
    pub fn move_pin_by(&mut self, id: &ConversationId, by: i32) -> Option<Command> {
        let from = self.pinned().iter().position(|p| p == id)?;
        let to = from.checked_add_signed(by as isize)?;
        (to != from && to < self.pinned().len()).then(|| self.move_pin(id, to))?
    }

    fn set_pins(&mut self, pinned: Vec<ConversationId>) -> Command {
        for conversation in &mut self.conversations {
            let position = pinned.iter().position(|p| *p == conversation.id);
            // Pinning brings an archived chat back.
            conversation.settings.set_pinned(position.map(|i| i as u32));
        }
        self.sort_conversations();
        Command::SetPins(pinned)
    }

    /// Opens the `n`th pinned conversation (from 0), e.g. for Alt+1…9.
    pub fn select_pinned(&mut self, n: usize) -> Option<Option<Command>> {
        let id = self.pinned().into_iter().nth(n)?;
        self.show_archived = false;
        Some(self.select(&id))
    }

    /// Opens the chat `by` places below (positive) or above the selected one in the list.
    pub fn select_relative(&mut self, by: isize) -> Option<Option<Command>> {
        let listed: Vec<ConversationId> = self.listed().map(|c| c.id.clone()).collect();
        let to = match self.selected_index() {
            Some(at) => at.checked_add_signed(by)?,
            None if by > 0 => 0,
            None => return None,
        };
        let id = listed.get(to)?.clone();
        Some(self.select(&id))
    }

    /// Opens the next chat below the selected one (wrapping around) with unread messages.
    pub fn select_next_unread(&mut self) -> Option<Option<Command>> {
        let listed: Vec<ConversationId> = self.listed().map(|c| c.id.clone()).collect();
        let start = self.selected_index().map_or(0, |at| at + 1);
        let id = (0..listed.len())
            .map(|i| &listed[(start + i) % listed.len()])
            .find(|id| {
                self.unread.contains_key(*id)
                    || self
                        .conversation(id)
                        .is_some_and(|c| c.settings.marked_unread)
            })?
            .clone();
        Some(self.select(&id))
    }

    /// Starts editing our newest message in the selected chat, if it can still be edited;
    /// returns its text for the composer.
    pub fn edit_last(&mut self) -> Option<String> {
        let index = self
            .messages()
            .enumerate()
            .filter(|(_, view)| view.message.outgoing && !view.message.notice)
            .last()
            .map(|(index, _)| index)?;
        self.edit(index)
    }

    /// Mutes until `until` (ms; `u64::MAX` for good), or unmutes.
    pub fn mute(&mut self, id: &ConversationId, until: Option<u64>) -> Option<Command> {
        self.change_settings(id, |s| s.muted_until = until)?;
        Some(Command::Mute {
            conversation: id.clone(),
            until,
        })
    }

    pub fn archive(&mut self, id: &ConversationId, archived: bool) -> Option<Command> {
        self.change_settings(id, |s| s.set_archived(archived))?;
        if archived && self.selected.as_ref() == Some(id) {
            self.deselect();
        }
        Some(Command::Archive {
            conversation: id.clone(),
            archived,
        })
    }

    /// Marks a conversation unread by hand, or read.
    pub fn mark_unread(&mut self, id: &ConversationId, unread: bool) -> Option<Command> {
        if !unread {
            return self.mark_read(id.clone());
        }
        self.change_settings(id, |s| s.marked_unread = true)?;
        if self.selected.as_ref() == Some(id) {
            self.deselect();
        }
        Some(Command::MarkUnread(id.clone()))
    }

    /// Sets how long new messages in a conversation last (seconds; 0 for good).
    pub fn set_timer(&mut self, id: &ConversationId, seconds: u32) -> Option<Command> {
        let conversation = self.conversations.iter_mut().find(|c| &c.id == id)?;
        conversation.expire_timer = seconds;
        Some(Command::SetTimer {
            conversation: id.clone(),
            seconds,
        })
    }

    /// Shows the archived conversations in the chat list instead of the others, or back.
    pub fn show_archived(&mut self, show: bool) -> Update {
        self.show_archived = show;
        Update {
            conversations: true,
            ..Update::default()
        }
    }

    pub fn showing_archived(&self) -> bool {
        self.show_archived
    }

    /// How many conversations are archived, and how many of those have unread messages.
    pub fn archived(&self) -> (usize, usize) {
        let archived = self.conversations.iter().filter(|c| c.settings.archived);
        archived.fold((0, 0), |(all, unread), c| {
            (
                all + 1,
                unread + usize::from(self.unread.contains_key(&c.id)),
            )
        })
    }

    /// Shows the spoilers of the `index`th message of [`ChatState::messages`].
    pub fn reveal(&mut self, index: usize) -> Update {
        let reference = self.messages().nth(index).map(|v| v.message.reference());
        self.revealed.extend(reference);
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// Names of who is typing in a conversation.
    pub fn typing(&self, id: &ConversationId) -> Vec<&str> {
        let now = now_millis();
        self.typing.get(id).map_or_else(Vec::new, |typists| {
            typists
                .iter()
                .filter(|t| t.until > now)
                .map(|t| t.sender.as_str())
                .collect()
        })
    }

    /// When someone typing next counts as stopped, so the UI can redraw then.
    pub fn typing_expires(&self) -> Option<u64> {
        let now = now_millis();
        self.typing
            .values()
            .flatten()
            .map(|t| t.until)
            .filter(|&until| until > now)
            .min()
    }

    /// Reacts to the `index`th message of [`ChatState::messages`] with `emoji`; reacting with
    /// the emoji we already put there takes it back.
    pub fn react(&self, index: usize, emoji: &str) -> Option<Command> {
        let conversation = self.selected()?;
        if !self.can_write_to(conversation) {
            return None;
        }
        let conversation = conversation.id.clone();
        let message = self.messages().nth(index)?.message;
        if message.unsent.is_some() || message.notice {
            return None;
        }
        let ours = message.reactions.iter().find(|r| r.outgoing);
        Some(Command::React {
            conversation,
            target: message.reference(),
            emoji: emoji.to_owned(),
            remove: ours.is_some_and(|r| r.emoji == emoji),
        })
    }

    /// Starts a reply to the `index`th message of [`ChatState::messages`].
    pub fn reply_to(&mut self, index: usize) -> Update {
        let quote = self
            .messages()
            .nth(index)
            .filter(|view| view.message.unsent.is_none() && !view.message.notice)
            .map(|view| Quote {
                author: view.message.sender.clone(),
                text: format::hide_spoilers(view.body, view.styles)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                author_id: view.message.sender_id.clone(),
                timestamp: view.message.timestamp,
            });
        self.replying = quote;
        Update::default()
    }

    pub fn cancel_reply(&mut self) {
        self.replying = None;
    }

    /// The message the composer answers, if any.
    pub fn replying(&self) -> Option<&Quote> {
        self.replying.as_ref()
    }

    pub fn dismiss_error(&mut self) {
        self.error = None;
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// The download of an attachment for display, if it was asked for.
    pub fn attachment(&self, id: &AttachmentId) -> Option<&AttachmentState> {
        self.attachments.get(id)
    }

    /// Saving an attachment, if it was asked for.
    pub fn saved(&self, id: &AttachmentId) -> Option<&SaveState> {
        self.saves.get(id)
    }

    /// Asks for an inline attachment of a message being shown, unless it is already there or
    /// on its way. After a failure, asking again retries. Not in a message request, as in
    /// Signal's apps.
    pub fn fetch_attachment(&mut self, id: &AttachmentId) -> Option<Command> {
        if matches!(
            self.attachments.get(id),
            Some(AttachmentState::Loading | AttachmentState::Ready(_))
        ) {
            return None;
        }
        let story = self.current_story().is_some_and(|(story, _, _)| {
            matches!(&story.content, crate::StoryContent::Picture(picture) if &picture.id == id)
        });
        if !story && self.selected().is_some_and(|chat| self.is_request(chat)) {
            return None;
        }
        let attachment = self
            .find_attachment(id)
            .filter(|a| shown_inline(a.kind))?
            .clone();
        self.attachments
            .insert(id.clone(), AttachmentState::Loading);
        Some(Command::FetchAttachment(attachment))
    }

    /// Asks for an audio or voice message of the selected conversation, to play it; `None` if
    /// it is there or on its way.
    pub fn fetch_audio(&mut self, id: &AttachmentId) -> Option<Command> {
        if matches!(
            self.attachments.get(id),
            Some(AttachmentState::Loading | AttachmentState::Ready(_))
        ) {
            return None;
        }
        let attachment = self
            .find_attachment(id)
            .filter(|a| matches!(a.kind, AttachmentKind::Audio | AttachmentKind::VoiceNote))?
            .clone();
        self.attachments
            .insert(id.clone(), AttachmentState::Loading);
        Some(Command::FetchAttachment(attachment))
    }

    /// Saves an attachment of the selected conversation to the downloads folder.
    pub fn save_attachment(&mut self, id: &AttachmentId) -> Option<Command> {
        if matches!(self.saves.get(id), Some(SaveState::Saving)) {
            return None;
        }
        let attachment = self
            .find_attachment(id)
            .filter(|a| a.kind != AttachmentKind::ViewOnce)?
            .clone();
        self.saves.insert(id.clone(), SaveState::Saving);
        Some(Command::SaveAttachment(attachment))
    }

    /// Only attachments of the selected conversation can be asked for.
    fn find_attachment(&self, id: &AttachmentId) -> Option<&Attachment> {
        // A story's picture, while it is shown.
        if let Some((story, _, _)) = self.current_story()
            && let crate::StoryContent::Picture(picture) = &story.content
            && &picture.id == id
        {
            return Some(picture);
        }
        let selected = self.selected.as_ref()?;
        let shown = self.history.get(selected).into_iter().flatten();
        // The media overview reaches further back than the loaded history.
        let gathered = self
            .media
            .iter()
            .filter(|(of, _)| of == selected)
            .flat_map(|(_, media)| media.iter().flatten());
        shown
            .chain(gathered)
            .flat_map(|m| {
                let preview = m.preview.iter().filter_map(|p| p.image.as_ref());
                m.attachments.iter().chain(preview)
            })
            .find(|a| &a.id == id)
    }

    /// While waiting for the phone, gives up on the history transfer.
    pub fn skip_history(&mut self) -> Option<Command> {
        (self.transfer == Some(HistoryTransfer::Waiting)).then_some(Command::SkipHistory)
    }

    /// The message history transfer in progress, if any.
    pub fn transfer(&self) -> Option<HistoryTransfer> {
        self.transfer
    }

    pub fn synced(&self) -> bool {
        self.synced
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Unread messages over all conversations, e.g. for the tray icon.
    /// Muted conversations don't count; ones marked unread by hand count as one.
    pub fn unread_total(&self) -> u32 {
        let now = now_millis();
        self.conversations
            .iter()
            .filter(|c| !c.settings.muted(now))
            .map(|c| match self.unread.get(&c.id) {
                Some(&n) => n,
                None => u32::from(c.settings.marked_unread),
            })
            .sum()
    }

    /// The chat list: groups and conversations that have messages, most recently active
    /// first. Synced contacts never written to stay out of it.
    pub fn conversations(&self) -> impl Iterator<Item = ConversationView<'_>> {
        self.listed().map(|c| self.view(c))
    }

    fn view<'a>(&'a self, c: &'a Conversation) -> ConversationView<'a> {
        ConversationView {
            conversation: c,
            avatar: self.avatars.get(&c.id),
            last_message: self.last_of(&c.id),
            unread: self.unread.get(&c.id).copied().unwrap_or_default(),
            selected: self.selected.as_ref() == Some(&c.id),
            typing: self.typing(&c.id),
        }
    }

    pub fn selected(&self) -> Option<&Conversation> {
        let id = self.selected.as_ref()?;
        self.conversations.iter().find(|c| &c.id == id)
    }

    /// The open conversation as the chat list shows it, also while a search leaves it out of
    /// [`ChatState::conversations`]: it stays open meanwhile.
    pub fn selected_view(&self) -> Option<ConversationView<'_>> {
        self.selected().map(|c| self.view(c))
    }

    /// Position of the selected conversation in [`ChatState::conversations`].
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        self.listed().position(|c| &c.id == id)
    }

    fn listed(&self) -> impl Iterator<Item = &Conversation> {
        let filter = self.filter.to_lowercase();
        self.conversations.iter().filter(move |c| {
            if filter.is_empty() {
                c.settings.archived == self.show_archived
                    && (c.is_group()
                        || c.note_to_self
                        || c.settings.pinned.is_some()
                        || self.last_of(&c.id).is_some()
                        || self.selected.as_ref() == Some(&c.id))
                    && (self.show_archived
                        || self
                            .folder
                            .is_none_or(|at| self.in_folder(c, &self.folders[at])))
            } else {
                c.title.to_lowercase().contains(&filter)
            }
        })
    }

    /// Whether a folder shows a conversation, by Signal's rules. The open one always stays.
    fn in_folder(&self, c: &Conversation, folder: &crate::ChatFolder) -> bool {
        if self.selected.as_ref() == Some(&c.id) {
            return true;
        }
        let member = folder.included.contains(&c.id)
            || (folder.all_individual && !c.is_group())
            || (folder.all_groups && c.is_group());
        member
            && !folder.excluded.contains(&c.id)
            && (folder.show_muted || !c.settings.muted(now_millis()))
            && (!folder.only_unread || self.unread.contains_key(&c.id) || c.settings.marked_unread)
    }

    pub fn folders(&self) -> &[crate::ChatFolder] {
        &self.folders
    }

    /// The folder shown, as an index into [`ChatState::folders`]; `None` for all chats.
    pub fn folder(&self) -> Option<usize> {
        self.folder
    }

    /// Shows a folder's chats, or all (`None`).
    pub fn show_folder(&mut self, folder: Option<usize>) -> Update {
        self.folder = folder.filter(|&at| at < self.folders.len());
        self.show_archived = false;
        Update {
            conversations: true,
            ..Update::default()
        }
    }

    /// Unread messages in a folder's chats, for its tab.
    pub fn folder_unread(&self, at: usize) -> u32 {
        let Some(folder) = self.folders.get(at) else {
            return 0;
        };
        self.conversations
            .iter()
            .filter(|c| !c.settings.archived && self.in_folder(c, folder))
            .filter_map(|c| self.unread.get(&c.id))
            .sum()
    }

    /// Narrows the chat list to conversations whose title contains `text`, including contacts
    /// never written to, so a new chat can be started. Empty shows the usual list again.
    pub fn search(&mut self, text: &str) -> Update {
        self.filter = text.trim().to_owned();
        Update {
            conversations: true,
            ..Update::default()
        }
    }

    pub fn searching(&self) -> bool {
        !self.filter.is_empty()
    }

    /// Messages of the selected conversation, oldest first.
    pub fn messages(&self) -> impl ExactSizeIterator<Item = MessageView<'_>> {
        let group = self.selected().is_some_and(|c| c.is_group());
        let history = self.selected_history();
        let now = jiff::Zoned::now();
        // Not in chats never read here: everything would be "new".
        let read_mark = self.read_mark.filter(|&mark| mark > 0);
        let first_unread = read_mark.and_then(|mark| {
            let first = unread_after(history, mark).next()?;
            history.iter().position(|m| std::ptr::eq(m, first))
        });
        let same_run = |a: &ChatMessage, b: &ChatMessage| {
            !a.notice
                && !b.notice
                && a.sender_id == b.sender_id
                && b.timestamp.saturating_sub(a.timestamp) <= RUN_GAP_MS
                && local_date(a.timestamp) == local_date(b.timestamp)
        };
        history.iter().enumerate().map(move |(i, message)| {
            let previous = i.checked_sub(1).map(|i| &history[i]);
            let next = history.get(i + 1);
            let new_day =
                previous.is_none_or(|p| local_date(p.timestamp) != local_date(message.timestamp));
            let first_of_run = previous.is_none_or(|p| !same_run(p, message));
            MessageView {
                message,
                show_sender: group && !message.outgoing && first_of_run,
                body: match (message.deleted, self.full_text(message)) {
                    (true, _) => DELETED,
                    (false, Some(text)) => text,
                    (false, None) => &message.body,
                },
                // A fetched long text has no formatting of its own.
                styles: if message.deleted || self.full_text(message).is_some() {
                    &[]
                } else {
                    &message.styles
                },
                day: new_day.then(|| day_label(message.timestamp, &now)),
                first_of_run,
                last_of_run: next.is_none_or(|n| !same_run(message, n)),
                first_unread: first_unread == Some(i),
                revealed: self.revealed.contains(&message.reference()),
                highlighted: self.highlight.as_ref().is_some_and(|h| message.is(h)),
                selected: self.selection.iter().any(|s| message.is(s)),
                sender_avatar: if group && !message.outgoing {
                    self.avatars
                        .get(&ConversationId::contact(&message.sender_id))
                } else {
                    None
                },
            }
        })
    }

    /// The selected conversation's history counted per `span`; see [`timeline::buckets`].
    /// Bucket indexes point into [`ChatState::messages`].
    pub fn activity(&self, span: timeline::Span) -> Vec<timeline::Bucket> {
        timeline::buckets(self.selected_history(), span)
    }

    /// The span that suits an overview of the selected conversation.
    pub fn activity_span(&self) -> timeline::Span {
        timeline::Span::fitting(self.selected_history())
    }

    fn selected_history(&self) -> &[ChatMessage] {
        self.selected
            .as_ref()
            .and_then(|id| self.history.get(id))
            .map_or(&[][..], Vec::as_slice)
    }

    /// The `n` most recent messages of the selected conversation, oldest first, for
    /// [`MessagesUpdate::Appended`].
    pub fn last_messages(&self, n: usize) -> impl Iterator<Item = MessageView<'_>> {
        let messages = self.messages();
        let skip = messages.len().saturating_sub(n);
        messages.skip(skip)
    }

    /// The fetched full text of a long message.
    fn full_text(&self, message: &ChatMessage) -> Option<&str> {
        let long = message
            .attachments
            .iter()
            .find(|a| a.kind == AttachmentKind::LongText)?;
        match self.attachments.get(&long.id)? {
            AttachmentState::Ready(AttachmentContent::Text(text)) => Some(text),
            _ => None,
        }
    }

    fn last_of(&self, id: &ConversationId) -> Option<&ChatMessage> {
        self.history.get(id).and_then(|h| h.last())
    }

    /// Pinned ones first, in their order; then newest activity first; conversations without messages alphabetically after those,
    /// names before bare phone numbers.
    fn sort_conversations(&mut self) {
        let mut conversations = std::mem::take(&mut self.conversations);
        conversations.sort_by_cached_key(|c| {
            (
                c.settings.pinned.unwrap_or(u32::MAX),
                Reverse(self.last_of(&c.id).map(|m| m.timestamp)),
                !c.title.starts_with(char::is_alphabetic),
                c.title.to_lowercase(),
            )
        });
        self.conversations = conversations;
    }
}

/// A line of `body` around where it contains `query` (ignoring case), for search results; with
/// where in it the match is (in bytes), to highlight.
pub fn snippet(body: &str, query: &str) -> (String, Option<Range<usize>>) {
    const AROUND: usize = 40;
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    let wanted: Vec<char> = query.trim().to_lowercase().chars().collect();
    // Lowercasing can change the length (rarely); then just show the start.
    let at = if lower.len() == chars.len() && !wanted.is_empty() {
        lower
            .windows(wanted.len())
            .position(|w| w == wanted.as_slice())
    } else {
        None
    };
    let start = at.map_or(0, |at| at.saturating_sub(AROUND));
    let end = (at.unwrap_or(0) + wanted.len() + AROUND * 2)
        .min(chars.len())
        .max(start);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let found = match at {
        Some(at) => {
            out.extend(&chars[start..at]);
            let from = out.len();
            out.extend(&chars[at..at + wanted.len()]);
            let found = from..out.len();
            out.extend(&chars[at + wanted.len()..end]);
            Some(found)
        }
        None => {
            out.extend(&chars[start..end]);
            None
        }
    };
    if end < chars.len() {
        out.push('…');
    }
    (out, found)
}

/// "Ann is typing…", "Ann and Bob are typing…"; empty if nobody is.
pub fn typing_label(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("{one} is typing…"),
        [one, two] => format!("{one} and {two} are typing…"),
        _ => "Several people are typing…".into(),
    }
}

/// How long until `at` (ms since the epoch), roughly: "5m", "3h", "2d".
pub fn format_remaining(at: u64, now: u64) -> String {
    let seconds = at.saturating_sub(now).div_ceil(1000);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds.div_ceil(60)),
        3600..86400 => format!("{}h", seconds.div_ceil(3600)),
        _ => format!("{}d", seconds.div_ceil(86400)),
    }
}

/// What is left of a message deleted for everyone.
fn tombstone(message: &mut ChatMessage) {
    message.deleted = true;
    message.body.clear();
    message.attachments.clear();
    message.quote = None;
    message.reactions.clear();
    message.styles.clear();
    message.edits.clear();
}

/// Signal identifies a message by its author and sent timestamp.
/// Whether `message` is one of ours shown before it went out, sent at `timestamp`.
fn is_unsent_at(message: &ChatMessage, timestamp: u64) -> bool {
    message.unsent.is_some() && message.timestamp == timestamp
}

fn message_key(message: &ChatMessage) -> (u64, &str) {
    (message.timestamp, &message.sender_id)
}

/// Whether `text` is typed like a phone number, to look up: digits (at least six), maybe a
/// leading "+", and the spaces, dashes, dots, slashes and brackets people write them with.
pub fn looks_like_number(text: &str) -> bool {
    let text = text.trim();
    let text = text.strip_prefix('+').unwrap_or(text);
    let digits = text.chars().filter(char::is_ascii_digit).count();
    (6..=15).contains(&digits)
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || " -./()".contains(c))
}

/// What the draft's last word says after "@", if it starts with one.
fn mention_query(draft: &str) -> Option<&str> {
    let word = draft.rsplit(char::is_whitespace).next()?;
    word.strip_prefix('@')
}

/// Where the picked people's "@Name" stands in `body`.
fn mentions_in(body: &str, people: &[Person]) -> Vec<Mention> {
    let mut mentions: Vec<Mention> = Vec::new();
    // Longer names first, so "@Ann Lee" isn't taken for "@Ann".
    let mut people: Vec<&Person> = people.iter().collect();
    people.sort_by_key(|p| std::cmp::Reverse(p.name.len()));
    for person in people {
        let tag = format!("@{}", person.name);
        for (start, _) in body.match_indices(&tag) {
            let end = start + tag.len();
            if !mentions.iter().any(|m| start < m.end && m.start < end) {
                mentions.push(Mention {
                    start,
                    end,
                    service_id: person.service_id.clone(),
                });
            }
        }
    }
    mentions.sort_by_key(|m| m.start);
    mentions
}

/// Incoming messages after `last_read`.
fn unread_after(history: &[ChatMessage], last_read: u64) -> impl Iterator<Item = &ChatMessage> {
    history
        .iter()
        .filter(move |m| !m.outgoing && !m.notice && m.timestamp > last_read)
}

/// Inserts in order, or replaces the message if it is already there; returns whether it was
/// new.
fn upsert(history: &mut Vec<ChatMessage>, message: ChatMessage) -> bool {
    match history.binary_search_by(|m| message_key(m).cmp(&message_key(&message))) {
        Ok(pos) => {
            history[pos] = message;
            false
        }
        Err(pos) => {
            history.insert(pos, message);
            true
        }
    }
}

/// Inserts in order unless already present; returns the new position.
fn insert(history: &mut Vec<ChatMessage>, message: ChatMessage) -> Option<usize> {
    let pos = history
        .binary_search_by(|m| message_key(m).cmp(&message_key(&message)))
        .err()?;
    history.insert(pos, message);
    Some(pos)
}

/// Up to two uppercase initials for an avatar, `?` if the title has none.
pub fn initials(title: &str) -> String {
    let letters: String = title
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        // Some letters are more than one in uppercase ("ﬃ" is "FFI").
        .take(2)
        .collect();
    if letters.is_empty() {
        "?".into()
    } else {
        letters
    }
}

/// Stable index into a UI's avatar palette of `len` colors.
pub fn avatar_index(id: &ConversationId, len: usize) -> usize {
    let hash =
        id.0.bytes()
            .fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    hash as usize % len.max(1)
}

/// "240 KB", "1.2 MB": sizes as people read them.
pub fn format_size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{} KB", bytes / 1_000),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}

/// "14:05" for today, "Mar  3, 14:05" otherwise, in local time.
fn zoned(timestamp: u64) -> Option<jiff::Zoned> {
    let time = jiff::Timestamp::from_millisecond(timestamp as i64).ok()?;
    Some(time.to_zoned(jiff::tz::TimeZone::system()))
}

fn local_date(timestamp: u64) -> Option<jiff::civil::Date> {
    zoned(timestamp).map(|time| time.date())
}

/// "14:05": the time of day, for message bubbles (days have their own separators).
pub fn format_clock(timestamp: u64) -> String {
    zoned(timestamp).map_or_else(String::new, |time| time.strftime("%H:%M").to_string())
}

/// For the chat list: "14:05" today, "Mon" this week, "Oct 2" this year, "Oct 2, 2024" before.
pub fn format_list_time(timestamp: u64) -> String {
    format_list_time_at(timestamp, &jiff::Zoned::now())
}

fn format_list_time_at(timestamp: u64, now: &jiff::Zoned) -> String {
    let Some(time) = zoned(timestamp) else {
        return String::new();
    };
    let days = (now.date() - time.date()).get_days();
    let format = match days {
        0 => "%H:%M",
        1..=6 => "%a",
        _ if time.year() == now.year() => "%b %-d",
        _ => "%b %-d, %Y",
    };
    time.strftime(format).to_string()
}

/// For day separators: "Today", "Yesterday", "Monday", "Oct 2", "Oct 2, 2024".
fn day_label(timestamp: u64, now: &jiff::Zoned) -> String {
    let Some(time) = zoned(timestamp) else {
        return String::new();
    };
    match (now.date() - time.date()).get_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => time.strftime("%A").to_string(),
        _ if time.year() == now.year() => time.strftime("%B %-d").to_string(),
        _ => time.strftime("%B %-d, %Y").to_string(),
    }
}

pub fn format_time(timestamp: u64) -> String {
    let Ok(time) = jiff::Timestamp::from_millisecond(timestamp as i64) else {
        return String::new();
    };
    let time = time.to_zoned(jiff::tz::TimeZone::system());
    let format = if time.date() == jiff::Zoned::now().date() {
        "%H:%M"
    } else {
        "%b %e, %H:%M"
    };
    time.strftime(format).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConversationKind, Delivery};

    fn conversation(id: &str, is_group: bool) -> Conversation {
        Conversation {
            id: ConversationId(id.into()),
            title: id.into(),
            kind: if is_group {
                ConversationKind::Group
            } else {
                ConversationKind::Contact
            },
            ..Conversation::default()
        }
    }

    fn message(conversation: &str, sender: &str, timestamp: u64, outgoing: bool) -> ChatMessage {
        ChatMessage {
            conversation: ConversationId(conversation.into()),
            sender_id: sender.to_lowercase(),
            sender: sender.into(),
            body: format!("{sender} at {timestamp}\nsecond line"),
            timestamp,
            outgoing,
            ..Default::default()
        }
    }

    fn state() -> ChatState {
        let mut state = ChatState::default();
        state.apply(Event::Linked {
            account: "me".into(),
        });
        state.apply(Event::Synced);
        state.apply(Event::Conversations(vec![
            conversation("a", false),
            conversation("g", true),
        ]));
        state
    }

    fn notifying() -> ChatState {
        let mut state = state();
        state.set_notifications(true, NotificationContent::NameAndMessage);
        state.take_notices();
        state
    }

    fn shown(notices: &[Notice]) -> Vec<(Option<&str>, &str, &str)> {
        notices
            .iter()
            .filter_map(|n| match n {
                Notice::Show {
                    conversation,
                    title,
                    body,
                } => Some((
                    conversation.as_ref().map(|c| c.0.as_str()),
                    title.as_str(),
                    body.as_str(),
                )),
                Notice::Withdraw(_) => None,
            })
            .collect()
    }

    #[test]
    fn incoming_messages_notify_unless_their_chat_is_in_front() {
        let mut state = notifying();
        state.apply(Event::Message(message("a", "Alice", 1, false)));
        assert_eq!(
            shown(&state.take_notices()),
            [(Some("a"), "Alice", "Alice at 1\nsecond line")]
        );

        state.select(&ConversationId("a".into()));
        state.set_window_focused(true);
        state.take_notices();
        state.apply(Event::Message(message("a", "Alice", 2, false)));
        assert!(
            shown(&state.take_notices()).is_empty(),
            "open chat in a focused window"
        );
        state.apply(Event::Message(message("g", "Carol", 3, false)));
        assert_eq!(
            shown(&state.take_notices()),
            [(Some("g"), "Carol in g", "Carol at 3\nsecond line")]
        );

        state.set_window_focused(false);
        state.apply(Event::Message(message("a", "Alice", 4, false)));
        assert_eq!(
            shown(&state.take_notices()).len(),
            1,
            "open chat, but window in the background"
        );
    }

    #[test]
    fn outgoing_duplicate_and_disabled_dont_notify() {
        let mut state = notifying();
        state.apply(Event::Message(message("a", "You", 1, true)));
        state.apply(Event::Message(message("a", "Alice", 2, false)));
        state.take_notices();
        state.apply(Event::Message(message("a", "Alice", 2, false)));
        state.apply(Event::Latest(message("a", "Alice", 3, false)));
        assert!(state.take_notices().is_empty());
        state.set_notifications(false, NotificationContent::NameAndMessage);
        state.apply(Event::Message(message("a", "Alice", 4, false)));
        assert!(state.take_notices().is_empty());
    }

    #[test]
    fn catching_up_is_summarized_once_synced() {
        let mut state = notifying();
        state.apply(Event::Reconnecting);
        for (chat, ts) in [("a", 1), ("a", 2), ("g", 3)] {
            state.apply(Event::Message(message(chat, "Alice", ts, false)));
        }
        assert!(state.take_notices().is_empty());
        state.apply(Event::Synced);
        assert_eq!(
            shown(&state.take_notices()),
            [(None, "b33p", "3 new messages in 2 chats")]
        );
        state.apply(Event::Synced);
        assert!(state.take_notices().is_empty());
    }

    #[test]
    fn content_setting_controls_what_is_revealed() {
        let mut state = notifying();
        state.set_notifications(true, NotificationContent::NameOnly);
        state.apply(Event::Message(message("a", "Alice", 1, false)));
        assert_eq!(
            shown(&state.take_notices()),
            [(Some("a"), "Alice", "New message")]
        );
        state.set_notifications(true, NotificationContent::Nothing);
        state.apply(Event::Message(message("a", "Alice", 2, false)));
        assert_eq!(
            shown(&state.take_notices()),
            [(Some("a"), "b33p", "New message")]
        );
    }

    #[test]
    fn attachment_only_messages_say_what_they_are() {
        let mut state = notifying();
        state.apply(Event::Message(ChatMessage {
            body: String::new(),
            ..with_attachment("a", 9, "p", AttachmentKind::Image)
        }));
        assert_eq!(
            shown(&state.take_notices()),
            [(Some("a"), "Alice", "📷 Photo")]
        );
    }

    #[test]
    fn reading_or_replying_withdraws_the_notification() {
        let mut state = notifying();
        let a = ConversationId("a".into());
        state.apply(Event::Message(message("a", "Alice", 1, false)));
        state.take_notices();
        let reply = state.reply(&a, " thanks ").0;
        assert!(
            matches!(reply, Some(Command::Send { conversation, body, quote: None, .. }) if conversation == a && body == "thanks")
        );
        assert_eq!(state.take_notices(), [Notice::Withdraw(a.clone())]);
        assert_eq!(
            state
                .conversations()
                .find(|c| c.conversation.id == a)
                .unwrap()
                .unread,
            0
        );
        assert!(state.reply(&a, "  ").0.is_none());
        assert!(
            state
                .reply(&ConversationId("nope".into()), "hi")
                .0
                .is_none()
        );
        state.select(&a);
        assert_eq!(state.take_notices(), [Notice::Withdraw(a)]);
    }

    /// A reply from a notification goes by the composer's rules: it would otherwise accept a
    /// message request (and share our profile key), write to a blocked chat, or be attempted
    /// while catching up.
    #[test]
    fn replying_follows_the_composers_rules() {
        let mut state = notifying();
        let a = ConversationId("a".into());
        state.apply(Event::Message(message("a", "Alice", 1, false)));
        // An unanswered request, once the phone's settings are known.
        state.apply(Event::StorageRead);
        assert!(state.reply(&a, "hi").0.is_none());
        state.apply(Event::Settings {
            conversation: a.clone(),
            settings: ChatSettings {
                accepted: true,
                ..ChatSettings::default()
            },
        });
        assert!(state.reply(&a, "hi").0.is_some());
        state.apply(Event::Settings {
            conversation: a.clone(),
            settings: ChatSettings {
                accepted: true,
                blocked: true,
                ..ChatSettings::default()
            },
        });
        assert!(state.reply(&a, "hi").0.is_none());
        // Reconnecting: nothing goes out until caught up again.
        let mut state = notifying();
        state.apply(Event::Message(message("a", "Alice", 1, false)));
        state.apply(Event::Reconnecting);
        assert!(state.reply(&a, "hi").0.is_none());
        state.apply(Event::Synced);
        assert!(state.reply(&a, "hi").0.is_some());
        // A group we left.
        let mut state = notifying();
        let g = ConversationId("g".into());
        state.apply(Event::Conversations(vec![Conversation {
            left: true,
            ..conversation("g", true)
        }]));
        assert!(state.reply(&g, "hi").0.is_none());
        state.select(&g);
        assert!(state.react(0, "👍").is_none());
    }

    #[test]
    fn incoming_message_moves_conversation_up_and_counts_unread() {
        let mut state = state();
        let update = state.apply(Event::Message(message("g", "Carol", 1, false)));
        assert_eq!(
            update,
            Update {
                conversations: true,
                ..Update::default()
            }
        );
        let views: Vec<_> = state.conversations().collect();
        assert_eq!(views[0].conversation.id.0, "g");
        assert_eq!(views[0].unread, 1);
        assert_eq!(views[0].preview(), "Carol at 1");
    }

    #[test]
    fn selecting_requests_history_until_it_arrives() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Alice", 1, false)));
        let id = ConversationId("a".into());
        assert!(
            matches!(state.select(&id), Some(Command::LoadHistory { conversation: i, before: None }) if i == id)
        );
        assert_eq!(
            state.conversations().find(|c| c.selected).unwrap().unread,
            0
        );
        // Not loaded yet (in flight, or the load failed): asking again retries.
        assert!(state.select(&id).is_some());
        state.apply(Event::History {
            conversation: id.clone(),
            messages: Vec::new(),
            reactions: Vec::new(),
            older: None,
        });
        assert!(state.select(&id).is_none());
        assert!(state.select(&ConversationId("nope".into())).is_none());
    }

    #[test]
    fn latest_message_previews_without_counting_as_loaded() {
        let mut state = state();
        let id = ConversationId("a".into());
        state.apply(Event::Latest(message("a", "Alice", 5, false)));
        let view = state
            .conversations()
            .find(|c| c.conversation.id == id)
            .unwrap();
        assert_eq!(view.last_message.unwrap().timestamp, 5);
        assert_eq!(view.unread, 0);
        // Opening it still fetches the full history.
        assert!(
            matches!(state.select(&id), Some(Command::LoadHistory { conversation: i, before: None }) if i == id)
        );
    }

    /// Signal's edits change the text; a backend reporting only the text doesn't lose the
    /// photo (or quote) the message was sent with.
    #[test]
    fn an_edit_keeps_the_attachments_and_quote() {
        let mut state = state();
        state.select(&id("a"));
        state.apply(Event::History {
            conversation: id("a"),
            messages: vec![ChatMessage {
                outgoing: true,
                quote: Some(Quote {
                    author: "Alice".into(),
                    text: "hello".into(),
                    author_id: "alice".into(),
                    timestamp: 5,
                }),
                ..with_attachment("a", 10, "p", AttachmentKind::Image)
            }],
            reactions: Vec::new(),
            older: None,
        });
        let target = state.messages().next().unwrap().message.reference();
        state.apply(Event::Edited {
            conversation: id("a"),
            target,
            message: ChatMessage {
                body: "better".into(),
                ..message("a", "You", 10, true)
            },
        });
        let edited = state.messages().next().unwrap().message;
        assert_eq!(edited.body, "better");
        assert_eq!(edited.attachments.len(), 1);
        assert_eq!(
            edited.quote.as_ref().map(|q| q.text.as_str()),
            Some("hello")
        );
    }

    fn with_attachment(
        conversation: &str,
        timestamp: u64,
        id: &str,
        kind: AttachmentKind,
    ) -> ChatMessage {
        let attachment = Attachment {
            id: AttachmentId(id.into()),
            kind,
            content_type: String::new(),
            file_name: None,
            size: 1,
            width: 0,
            height: 0,
            blurhash: None,
            caption: None,
            gif: false,
            pointer: Arc::from(Vec::new()),
        };
        ChatMessage {
            attachments: vec![attachment],
            ..message(conversation, "Alice", timestamp, false)
        }
    }

    #[test]
    fn attachments_are_fetched_once_and_only_for_the_selected_chat() {
        let mut state = state();
        let photo = AttachmentId("photo".into());
        state.apply(Event::Message(with_attachment(
            "a",
            1,
            "photo",
            AttachmentKind::Image,
        )));
        assert!(state.fetch_attachment(&photo).is_none(), "not selected");
        state.select(&ConversationId("a".into()));
        assert!(
            matches!(state.fetch_attachment(&photo), Some(Command::FetchAttachment(a)) if a.id == photo)
        );
        assert!(state.fetch_attachment(&photo).is_none(), "already asked");
        let update = state.apply(Event::Attachment {
            id: photo.clone(),
            content: Err("gone".into()),
        });
        assert!(update.rows);
        assert_eq!(
            state.attachment(&photo),
            Some(&AttachmentState::Failed("gone".into()))
        );
    }

    #[test]
    fn long_text_replaces_the_body_once_fetched() {
        let mut state = state();
        state.apply(Event::Message(with_attachment(
            "a",
            1,
            "long",
            AttachmentKind::LongText,
        )));
        state.select(&ConversationId("a".into()));
        assert_ne!(state.messages().next().unwrap().body, "the whole story");
        state.apply(Event::Attachment {
            id: AttachmentId("long".into()),
            content: Ok(AttachmentContent::Text("the whole story".into())),
        });
        assert_eq!(state.messages().next().unwrap().body, "the whole story");
    }

    #[test]
    fn view_once_media_is_not_saved() {
        let mut state = state();
        state.apply(Event::Message(with_attachment(
            "a",
            1,
            "once",
            AttachmentKind::ViewOnce,
        )));
        state.apply(Event::Message(with_attachment(
            "a",
            2,
            "file",
            AttachmentKind::File,
        )));
        state.select(&ConversationId("a".into()));
        assert!(
            state
                .save_attachment(&AttachmentId("once".into()))
                .is_none()
        );
        assert!(
            state
                .save_attachment(&AttachmentId("file".into()))
                .is_some()
        );
        assert!(
            state
                .save_attachment(&AttachmentId("file".into()))
                .is_none(),
            "already saving"
        );
    }

    #[test]
    fn saving_keeps_the_shown_picture_and_failures_can_be_retried() {
        let mut state = state();
        let photo = AttachmentId("photo".into());
        state.apply(Event::Message(with_attachment(
            "a",
            1,
            "photo",
            AttachmentKind::Image,
        )));
        state.select(&ConversationId("a".into()));
        assert!(state.fetch_attachment(&photo).is_some());
        state.apply(Event::Attachment {
            id: photo.clone(),
            content: Err("offline".into()),
        });
        assert!(
            state.fetch_attachment(&photo).is_some(),
            "retry after failure"
        );
        let picture = AttachmentContent::Picture(Arc::new(Picture {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        }));
        state.apply(Event::Attachment {
            id: photo.clone(),
            content: Ok(picture.clone()),
        });
        assert!(state.save_attachment(&photo).is_some());
        state.apply(Event::AttachmentSaved {
            id: photo.clone(),
            path: Ok("/tmp/photo.jpg".into()),
        });
        assert_eq!(
            state.attachment(&photo),
            Some(&AttachmentState::Ready(picture))
        );
        assert_eq!(
            state.saved(&photo),
            Some(&SaveState::Saved("/tmp/photo.jpg".into()))
        );
        assert!(state.fetch_attachment(&photo).is_none(), "already shown");
    }

    #[test]
    fn search_finds_contacts_never_written_to() {
        let mut state = state();
        // "a" is a contact without messages, so it isn't listed until searched for.
        assert!(state.conversations().all(|c| c.conversation.id.0 != "a"));
        state.search("A");
        let found: Vec<_> = state
            .conversations()
            .map(|c| c.conversation.id.0.clone())
            .collect();
        assert_eq!(found, ["a"]);
        state.select(&ConversationId("a".into()));
        assert!(!state.searching());
        assert!(
            state.conversations().any(|c| c.conversation.id.0 == "a"),
            "selected stays listed"
        );
    }

    #[test]
    fn replies_quote_the_chosen_message_and_reset_with_the_chat() {
        let mut state = state();
        state.apply(Event::Synced);
        state.apply(Event::Message(message("a", "Alice", 7, false)));
        state.select(&ConversationId("a".into()));
        state.reply_to(0);
        let quote = state.replying().unwrap().clone();
        assert_eq!(
            (
                quote.author.as_str(),
                quote.author_id.as_str(),
                quote.timestamp
            ),
            ("Alice", "alice", 7)
        );
        let sent = state.compose("sure").0;
        assert!(matches!(sent, Some(Command::Send { quote: Some(q), .. }) if q == quote));
        assert!(state.replying().is_none(), "sending uses it up");

        state.reply_to(0);
        state.select(&ConversationId("g".into()));
        assert!(state.replying().is_none(), "another chat drops it");
    }

    fn reaction(emoji: &str, sender: &str, outgoing: bool) -> crate::Reaction {
        crate::Reaction {
            emoji: emoji.into(),
            sender_id: sender.into(),
            sender: sender.into(),
            outgoing,
        }
    }

    #[test]
    fn one_reaction_per_person_and_ours_toggles() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Alice", 7, false)));
        state.select(&ConversationId("a".into()));
        let target = state.messages().next().unwrap().message.reference();
        let react = |state: &mut ChatState, r, remove| {
            state.apply(Event::Reaction {
                conversation: ConversationId("a".into()),
                target: target.clone(),
                reaction: r,
                remove,
            })
        };
        assert!(react(&mut state, reaction("👍", "bob", false), false).rows);
        react(&mut state, reaction("❤️", "bob", false), false);
        react(&mut state, reaction("😂", "me", true), false);
        let emojis: Vec<_> = state
            .messages()
            .next()
            .unwrap()
            .message
            .reactions
            .iter()
            .map(|r| r.emoji.clone())
            .collect();
        assert_eq!(
            emojis,
            ["❤️", "😂"],
            "Bob's second reaction replaced his first"
        );

        // Reacting with our current emoji takes it back; another one replaces it.
        assert!(matches!(
            state.react(0, "😂"),
            Some(Command::React { remove: true, .. })
        ));
        assert!(matches!(
            state.react(0, "👍"),
            Some(Command::React { remove: false, .. })
        ));
        react(&mut state, reaction("😂", "me", true), true);
        assert_eq!(state.messages().next().unwrap().message.reactions.len(), 1);
    }

    #[test]
    fn list_times_and_day_labels_get_coarser_with_age() {
        let now: jiff::Zoned = "2026-10-05T12:00:00[Europe/Berlin]".parse().unwrap();
        let at = |s: &str| {
            let t: jiff::Zoned = format!("{s}[Europe/Berlin]").parse().unwrap();
            t.timestamp().as_millisecond() as u64
        };
        // Formatting uses the system time zone; compare against what it says for the same instant.
        let local = |s: &str, f: &str| zoned(at(s)).unwrap().strftime(f).to_string();
        assert_eq!(
            format_list_time_at(at("2026-10-05T09:30:00"), &now),
            local("2026-10-05T09:30:00", "%H:%M")
        );
        assert_eq!(
            format_list_time_at(at("2026-10-01T09:30:00"), &now),
            local("2026-10-01T09:30:00", "%a")
        );
        assert_eq!(
            format_list_time_at(at("2026-03-02T09:30:00"), &now),
            local("2026-03-02T09:30:00", "%b %-d")
        );
        assert!(format_list_time_at(at("2024-03-02T09:30:00"), &now).ends_with("2024"));
        assert_eq!(day_label(at("2026-10-05T09:30:00"), &now), "Today");
        assert_eq!(day_label(at("2026-10-04T09:30:00"), &now), "Yesterday");
        assert_eq!(
            day_label(at("2026-03-02T09:30:00"), &now),
            local("2026-03-02T09:30:00", "%B %-d")
        );
    }

    #[test]
    fn runs_break_on_author_gap_and_day() {
        let mut state = state();
        let minute = 60_000;
        let base = 1_790_000_000_000; // some afternoon
        for (sender, t) in [("Alice", 0), ("Alice", 1), ("Bob", 2), ("Bob", 20)] {
            state.apply(Event::Message(message(
                "g",
                sender,
                base + t * minute,
                false,
            )));
        }
        state.select(&ConversationId("g".into()));
        let runs: Vec<_> = state
            .messages()
            .map(|m| (m.first_of_run, m.last_of_run, m.show_sender))
            .collect();
        assert_eq!(
            runs,
            [
                (true, false, true),
                (false, true, false),
                (true, true, true),
                (true, true, true)
            ]
        );
        assert!(state.messages().next().unwrap().day.is_some());
        assert!(state.messages().nth(1).unwrap().day.is_none());
    }

    #[test]
    fn staged_files_go_with_the_next_message() {
        let mut state = state();
        state.apply(Event::Synced);
        state.select(&ConversationId("a".into()));
        assert!(!state.can_compose(""));
        state.stage_files(["/tmp/a.pdf".into(), "/tmp/b.png".into()]);
        state.unstage(0);
        assert!(state.can_compose(""), "files alone can be sent");
        let sent = state.compose("").0;
        assert!(
            matches!(sent, Some(Command::Send { files, .. }) if files == [std::path::PathBuf::from("/tmp/b.png")])
        );
        assert!(state.staged().is_empty());
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(245_760), "245 KB");
        assert_eq!(format_size(1_250_000), "1.2 MB");
    }

    #[test]
    fn history_can_be_skipped_only_while_waiting() {
        let mut state = state();
        assert!(state.skip_history().is_none());
        state.apply(Event::HistoryTransfer(HistoryTransfer::Waiting));
        assert!(matches!(state.skip_history(), Some(Command::SkipHistory)));
        state.apply(Event::HistoryTransfer(HistoryTransfer::Importing {
            messages: 3,
        }));
        assert!(state.skip_history().is_none());
        state.apply(Event::HistoryTransfer(HistoryTransfer::Finished));
        assert_eq!(state.transfer(), None);
    }

    #[test]
    fn history_merges_without_duplicates_in_order() {
        let mut state = state();
        let id = ConversationId("a".into());
        state.select(&id);
        state.apply(Event::Message(message("a", "Alice", 3, false)));
        let update = state.apply(Event::History {
            conversation: id,
            messages: vec![
                message("a", "Alice", 3, false),
                message("a", "You", 1, true),
            ],
            reactions: Vec::new(),
            older: None,
        });
        assert_eq!(update.messages, MessagesUpdate::Reset);
        let stamps: Vec<_> = state.messages().map(|m| m.message.timestamp).collect();
        assert_eq!(stamps, [1, 3]);
    }

    #[test]
    fn history_dedupe_ignores_display_name_changes() {
        let mut state = state();
        let id = ConversationId("a".into());
        state.select(&id);
        // Received live before contacts synced, so only the raw id was known as a name.
        let mut live = message("a", "Alice", 5, false);
        live.sender = "3f2a…".into();
        state.apply(Event::Message(live));
        state.apply(Event::History {
            conversation: id.clone(),
            messages: vec![message("a", "Alice", 5, false)],
            reactions: Vec::new(),
            older: None,
        });
        assert_eq!(state.messages().count(), 1);
        // Same timestamp from a different author is a different message.
        state.apply(Event::History {
            conversation: id,
            messages: vec![message("a", "Bob", 5, false)],
            reactions: Vec::new(),
            older: None,
        });
        assert_eq!(state.messages().count(), 2);
    }

    #[test]
    fn conversations_upsert_in_activity_order() {
        let mut state = state();
        state.apply(Event::Message(message("g", "Carol", 1, false)));
        state.apply(Event::Message(message("a", "Alice", 2, false)));
        // A partial update never drops conversations, and renames apply in place.
        let mut renamed = conversation("a", false);
        renamed.title = "Alice".into();
        state.apply(Event::Conversations(vec![conversation("n", true), renamed]));
        let titles: Vec<_> = state
            .conversations()
            .map(|c| c.conversation.title.as_str())
            .collect();
        assert_eq!(titles, ["Alice", "g", "n"]);
    }

    #[test]
    fn list_holds_groups_and_active_chats_names_before_numbers() {
        let mut state = ChatState::default();
        let titled = |id: &str, title: &str, is_group| Conversation {
            title: title.into(),
            ..conversation(id, is_group)
        };
        state.apply(Event::Conversations(vec![
            titled("1", "+49 151 1234567", true),
            titled("2", "bob", true),
            titled("3", "Alice", true),
            // A synced contact never written to.
            titled("4", "Zoe", false),
        ]));
        let titles: Vec<_> = state
            .conversations()
            .map(|c| c.conversation.title.as_str())
            .collect();
        assert_eq!(titles, ["Alice", "bob", "+49 151 1234567"]);
        // Writing to them puts them on the list, at the top.
        state.apply(Event::Message(message("4", "Zoe", 1, false)));
        assert_eq!(
            state.conversations().next().unwrap().conversation.title,
            "Zoe"
        );
        assert_eq!(state.selected_index(), None);
        state.select(&ConversationId("4".into()));
        assert_eq!(state.selected_index(), Some(0));
    }

    #[test]
    fn appends_in_one_batch_are_all_reported() {
        let mut state = state();
        state.select(&ConversationId("a".into()));
        let update = [1, 2, 3].into_iter().fold(Update::default(), |update, ts| {
            update.merge(state.apply(Event::Message(message("a", "Alice", ts, false))))
        });
        assert_eq!(update.messages, MessagesUpdate::Appended(3));
        let stamps: Vec<_> = state
            .last_messages(2)
            .map(|m| m.message.timestamp)
            .collect();
        assert_eq!(stamps, [2, 3]);
        // An out-of-order message in the same batch forces a full re-read.
        let update = update.merge(state.apply(Event::Message(message("a", "Bob", 0, false))));
        assert_eq!(update.messages, MessagesUpdate::Reset);
    }

    #[test]
    fn reconnecting_disables_sending_until_synced() {
        let mut state = state();
        state.select(&ConversationId("a".into()));
        state.apply(Event::Reconnecting);
        assert!(!state.can_compose("hi"));
        state.apply(Event::Synced);
        assert!(state.can_compose("hi"));
    }

    #[test]
    fn live_messages_dedupe_and_insert_in_order() {
        let mut state = state();
        state.select(&ConversationId("a".into()));
        assert_eq!(
            state
                .apply(Event::Message(message("a", "Alice", 5, false)))
                .messages,
            MessagesUpdate::Appended(1)
        );
        assert_eq!(
            state.apply(Event::Message(message("a", "Alice", 5, false))),
            Update::default()
        );
        assert_eq!(
            state
                .apply(Event::Message(message("a", "Bob", 2, false)))
                .messages,
            MessagesUpdate::Reset
        );
        let stamps: Vec<_> = state.messages().map(|m| m.message.timestamp).collect();
        assert_eq!(stamps, [2, 5]);
    }

    #[test]
    fn group_sender_shown_once_per_run() {
        let mut state = state();
        state.select(&ConversationId("g".into()));
        for (sender, ts) in [("Carol", 1), ("Carol", 2), ("Dave", 3)] {
            state.apply(Event::Message(message("g", sender, ts, false)));
        }
        state.apply(Event::Message(message("g", "You", 4, true)));
        let shown: Vec<_> = state.messages().map(|m| m.show_sender).collect();
        assert_eq!(shown, [true, false, true, false]);
        assert!(state.last_messages(1).next().unwrap().message.outgoing);
    }

    #[test]
    fn compose_requires_selection_sync_and_text() {
        let mut state = state();
        assert!(state.compose("hi").0.is_none());
        state.select(&ConversationId("a".into()));
        assert!(state.compose("   ").0.is_none());
        assert!(
            matches!(state.compose(" hi ").0, Some(Command::Send { body, .. }) if body == "hi")
        );
    }

    /// The timestamp `command` sends with; panics for anything but a send.
    fn sent_at(command: Option<Command>) -> u64 {
        match command {
            Some(Command::Send { timestamp, .. }) => timestamp,
            other => panic!("not a send: {other:?}"),
        }
    }

    #[test]
    fn a_new_message_shows_while_sending_and_the_sent_one_takes_its_place() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Alice", 7, false)));
        state.select(&ConversationId("a".into()));
        let (command, update) = state.compose("hi");
        let timestamp = sent_at(command);
        assert_eq!(update.messages, MessagesUpdate::Appended(1));
        assert!(update.scroll_to_end, "sending shows the end of the chat");
        let shown = &state.last_messages(1).next().unwrap().message;
        assert_eq!(
            (shown.body.as_str(), shown.unsent),
            ("hi", Some(Unsent::Sending))
        );

        let update = state.apply(Event::Message(ChatMessage {
            body: "hi".into(),
            ..message("a", "Me", timestamp, true)
        }));
        assert!(update.rows, "redrawn in place");
        assert_eq!(update.messages, MessagesUpdate::Unchanged);
        let bodies: Vec<_> = state.messages().map(|m| m.message.unsent).collect();
        assert_eq!(bodies, [None, None], "replaced, not added");
        assert_eq!(
            state.last_messages(1).next().unwrap().message.sender_id,
            "me"
        );
    }

    #[test]
    fn a_failed_send_stays_shown_as_failed() {
        let mut state = state();
        state.select(&ConversationId("a".into()));
        let timestamp = sent_at(state.compose("hi").0);
        let update = state.apply(Event::SendFailed {
            conversation: ConversationId("a".into()),
            timestamp,
        });
        assert!(update.rows);
        let shown = state.last_messages(1).next().unwrap().message.clone();
        assert_eq!(shown.unsent, Some(Unsent::Failed));

        // Nothing points at a message that never went out; deleting it only removes it here.
        assert!(state.react(0, "👍").is_none());
        state.reply_to(0);
        assert!(state.replying().is_none());
        assert!(!state.can_edit(&shown));
        assert!(!ChatState::can_forward(&shown));
        let (command, _) = state.delete(0, false);
        assert!(command.is_none());
        assert_eq!(state.messages().count(), 0);
    }

    #[test]
    fn files_show_by_name_until_sent() {
        let mut state = state();
        state.select(&ConversationId("a".into()));
        state.stage_files(vec![std::path::PathBuf::from("/tmp/b.png")]);
        state.compose("look");
        let shown = &state.last_messages(1).next().unwrap().message;
        assert_eq!(shown.body, "look\n📎 b.png");
    }

    #[test]
    fn a_reply_from_a_notification_shows_while_sending() {
        let mut state = state();
        let a = ConversationId("a".into());
        state.select(&a);
        let (command, update) = state.reply(&a, "thanks");
        sent_at(command);
        assert_eq!(update.messages, MessagesUpdate::Appended(1));
        let shown = &state.last_messages(1).next().unwrap().message;
        assert_eq!(shown.unsent, Some(Unsent::Sending));
    }

    #[test]
    fn new_password_is_validated_before_unlocking() {
        let mut state = ChatState::default();
        assert!(
            state.unlock("whatever", "whatever").is_none(),
            "no password requested yet"
        );
        state.apply(Event::PasswordRequired {
            new: true,
            error: None,
        });
        assert!(state.unlock("short", "short").is_none());
        assert!(
            matches!(state.screen(), Screen::Password { error: Some(e), .. } if e.contains("8"))
        );
        assert!(state.unlock("long enough", "long enougj").is_none());
        let command = state.unlock("long enough", "long enough");
        assert!(matches!(command, Some(Command::Unlock(p)) if p.expose() == "long enough"));
        assert_eq!(state.screen(), &Screen::Connecting);
    }

    #[test]
    fn existing_password_needs_no_confirmation() {
        let mut state = ChatState::default();
        state.apply(Event::PasswordRequired {
            new: false,
            error: Some("Wrong password.".into()),
        });
        assert!(state.unlock("", "").is_none());
        assert!(state.unlock("x", "").is_some());
    }

    #[test]
    fn time_shows_date_only_for_other_days() {
        let now = jiff::Timestamp::now().as_millisecond() as u64;
        assert_eq!(format_time(now).len(), "14:05".len());
        assert!(format_time(86_400_000).contains(", "));
    }

    #[test]
    fn initials_handle_symbols_and_empty() {
        assert_eq!(initials("Rustaceans 🦀"), "R");
        assert_eq!(initials("ada lovelace"), "AL");
        assert_eq!(initials(""), "?");
    }

    fn id(s: &str) -> ConversationId {
        ConversationId(s.into())
    }

    fn listed(state: &ChatState) -> Vec<String> {
        state
            .conversations()
            .map(|c| c.conversation.id.0.clone())
            .collect()
    }

    /// Typed like a phone number (as people write them) is looked up; names, short numbers and
    /// text with letters aren't.
    #[test]
    fn phone_numbers_are_told_from_names() {
        for number in [
            "+49 151 23456789",
            "0151/2345678",
            "(030) 123-4567",
            "+4915123456789",
        ] {
            assert!(looks_like_number(number), "{number}");
        }
        for text in [
            "Ann",
            "12345",
            "call 0151 2345678",
            "+49 151 2345678 9999999",
            "",
        ] {
            assert!(!looks_like_number(text), "{text}");
        }
    }

    /// Looking a number up opens the chat with them, or chooses them for the group being made.
    #[test]
    fn someone_found_by_number_is_opened_or_chosen() {
        use crate::AfterFinding::{Choose, Open};
        let mut state = state();
        assert!(state.find_number("Ann", Open).is_none(), "not a number");
        assert!(matches!(
            state.find_number(" +49 151 23456789 ", Open),
            Some(Command::FindNumber { number, then: Open }) if number == "+49 151 23456789"
        ));
        state.apply(Event::Found {
            conversation: id("a"),
            then: Open,
        });
        assert_eq!(state.selected().map(|c| c.id.clone()), Some(id("a")));

        state.open_group_draft(Some(id("g")));
        state.apply(Event::Found {
            conversation: id("a"),
            then: Choose,
        });
        assert!(matches!(state.group_draft(), Some((false, [chosen])) if *chosen == id("a")));
        state.apply(Event::Found {
            conversation: id("a"),
            then: Choose,
        });
        assert!(
            matches!(state.group_draft(), Some((false, [_]))),
            "found again: still chosen, not unchosen"
        );
    }

    /// An invitation is answered while we're invited, not in the group yet; other changes need
    /// being in it.
    #[test]
    fn invitations_are_answered_before_joining() {
        use crate::GroupEdit::{AcceptInvitation, DeclineInvitation, Title};
        let mut state = state();
        let invited = Conversation {
            left: true,
            invited: true,
            ..conversation("club", true)
        };
        state.apply(Event::Conversations(vec![invited]));
        let club = id("club");
        assert!(matches!(
            state.edit_group(&club, AcceptInvitation),
            Some(Command::EditGroup {
                edit: AcceptInvitation,
                ..
            })
        ));
        assert!(state.edit_group(&club, DeclineInvitation).is_some());
        assert!(
            state.edit_group(&club, Title("Ours".into())).is_none(),
            "not in it yet"
        );
        // A group we're in has nothing to accept.
        assert!(state.edit_group(&id("g"), AcceptInvitation).is_none());
        assert!(state.edit_group(&id("g"), Title("Ours".into())).is_some());
    }

    /// Verifying acts on the open details page, and only with a safety number to verify; the
    /// answer updates what it shows.
    #[test]
    fn verifying_from_the_details_page() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Ann", 1, false)));
        assert!(state.set_verified(true).is_none(), "no details page open");
        state.select(&id("a"));
        assert!(state.open_details().is_some());
        assert!(state.set_verified(true).is_none(), "details not loaded yet");
        state.apply(Event::Details {
            conversation: id("a"),
            details: crate::Details::default(),
        });
        assert!(state.set_verified(true).is_none(), "no safety number yet");
        state.apply(Event::Details {
            conversation: id("a"),
            details: crate::Details {
                safety_number: Some("12345".into()),
                ..Default::default()
            },
        });
        assert!(matches!(
            state.set_verified(true),
            Some(Command::SetVerified { conversation, verified: true }) if conversation == id("a")
        ));
        state.apply(Event::Trust {
            conversation: id("b"),
            trust: crate::Trust::Verified,
        });
        assert_eq!(
            state.details().unwrap().unwrap().trust,
            crate::Trust::Unverified,
            "another chat's"
        );
        state.apply(Event::Trust {
            conversation: id("a"),
            trust: crate::Trust::Verified,
        });
        assert_eq!(
            state.details().unwrap().unwrap().trust,
            crate::Trust::Verified
        );
    }

    /// When the safety number of a contact we verified changes, nothing is sent to them until
    /// that is confirmed (or it is verified again), as Signal asks.
    #[test]
    fn a_changed_verified_safety_number_stops_sending_until_confirmed() {
        let mut state = state();
        state.apply(Event::Message(message("a", "a", 1, false)));
        state.select(&id("a"));
        assert!(state.can_compose("hi"), "writable before");
        assert!(
            state.confirm_safety_number(&id("a")).is_none(),
            "nothing to confirm"
        );

        state.apply(Event::Trust {
            conversation: id("a"),
            trust: crate::Trust::Changed,
        });
        assert!(!state.can_compose("hi"));
        assert!(state.compose("hi").0.is_none(), "not sent");
        assert!(state.react(0, "👍").is_none(), "no reaction either");
        assert!(matches!(
            state.confirm_safety_number(&id("a")),
            Some(Command::SetVerified { conversation, verified: false }) if conversation == id("a")
        ));

        state.apply(Event::Trust {
            conversation: id("a"),
            trust: crate::Trust::Unverified,
        });
        assert!(state.can_compose("hi"), "writable once confirmed");
    }

    /// Our own notices (a timer we set, a change we made to a group) aren't messages we wrote:
    /// nothing to edit or delete for everyone, and "edit the last one" passes over them.
    #[test]
    fn our_notices_cannot_be_edited() {
        let mut state = state();
        let now = now_millis();
        state.apply(Event::Message(message("a", "You", now - 2, true)));
        state.apply(Event::Message(ChatMessage {
            notice: true,
            body: "You set disappearing messages to 1 hour.".into(),
            ..message("a", "You", now - 1, true)
        }));
        state.select(&id("a"));
        let at = state.messages().position(|v| v.message.notice).unwrap();
        let notice = state.messages().nth(at).unwrap().message;
        assert!(
            !state.can_edit(notice),
            "nothing to edit or delete for everyone"
        );
        assert!(state.edit_last().is_some(), "the message before it");
        assert_eq!(state.editing().unwrap().timestamp, now - 2);
    }

    /// A notice (e.g. a changed safety number) is shown, but nobody wrote it: no notification,
    /// not unread, and no read receipt for it.
    #[test]
    fn notices_are_shown_and_nothing_more() {
        let notice = |timestamp| ChatMessage {
            notice: true,
            ..message("a", "Ann", timestamp, false)
        };
        let mut state = notifying();
        state.apply(Event::Message(notice(10)));
        assert!(shown(&state.take_notices()).is_empty(), "no notification");
        assert_eq!(state.unread_total(), 0, "not unread");

        state.apply(Event::Message(message("a", "Ann", 20, false)));
        state.take_notices();
        assert_eq!(state.unread_total(), 1, "only the real message");
        state.set_window_focused(true);
        state.select(&id("a"));
        history_arrives(&mut state, "a");
        assert!(
            matches!(
                &state.take_commands()[..],
                [Command::MarkRead { messages, .. }]
                    if messages.len() == 1 && messages[0].timestamp == 20
            ),
            "a receipt for the message only"
        );

        // Nothing to answer, react to, forward or delete; and it stands apart from Ann's run.
        let at = state.messages().position(|v| v.message.notice).unwrap();
        state.reply_to(at);
        assert!(state.replying().is_none(), "no reply");
        assert!(state.react(at, "👍").is_none(), "no reaction");
        assert!(!state.start_forward(at), "no forwarding");
        assert!(state.delete(at, false).0.is_none(), "no deleting");
        let view = state.messages().nth(at).unwrap();
        assert!(view.first_of_run && view.last_of_run, "a run of its own");
    }

    /// The chat's stored history, as far as this state has it (nothing more).
    fn history_arrives(state: &mut ChatState, chat: &str) {
        state.apply(Event::History {
            conversation: id(chat),
            messages: Vec::new(),
            reactions: Vec::new(),
            older: None,
        });
    }

    /// What arrived while b33p was closed is read with the rest once its page is here, not
    /// only what was known when the chat was opened (the newest, from `Event::Latest`), and
    /// so is a message that came late, dated before what was read. A message dated in the
    /// future doesn't make everything until then count as read.
    #[test]
    fn reading_covers_everything_unread() {
        let mut state = state();
        state.set_window_focused(true);
        state.apply(Event::Latest(message("a", "Ann", 50, false)));
        state.select(&id("a"));
        state.apply(Event::History {
            conversation: id("a"),
            messages: [10, 20, 30, 40, 50]
                .map(|ts| message("a", "Ann", ts, false))
                .into(),
            reactions: Vec::new(),
            older: None,
        });
        let read = |state: &mut ChatState| -> Vec<u64> {
            state
                .take_commands()
                .into_iter()
                .flat_map(|command| match command {
                    Command::MarkRead { messages, .. } => messages,
                    _ => Vec::new(),
                })
                .map(|m| m.timestamp)
                .collect()
        };
        let mut all = read(&mut state);
        all.sort_unstable();
        assert_eq!(all, [10, 20, 30, 40, 50]);

        // Late, while another chat is open.
        state.select(&id("g"));
        state.apply(Event::Message(message("a", "Ann", 35, false)));
        assert_eq!(state.unread_total(), 1);
        state.select(&id("a"));
        assert_eq!(read(&mut state), [35]);

        // From the future: the mark stays in the present, and the message is read once.
        let future = now_millis() + 365 * 24 * 3600 * 1000;
        state.apply(Event::Message(message("a", "Ann", future, false)));
        assert_eq!(read(&mut state), [future]);
        assert!(state.conversation(&id("a")).unwrap().settings.last_read <= now_millis());
        state.set_window_focused(false);
        state.set_window_focused(true);
        assert!(read(&mut state).is_empty());
    }

    #[test]
    fn viewing_a_chat_reads_it_and_queues_receipts_once() {
        let mut state = state();
        state.set_window_focused(true);
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        state.apply(Event::Message(message("a", "Ann", 20, false)));
        assert_eq!(state.unread_total(), 2);
        state.select(&id("a"));
        // Read once what is stored has arrived: until then, more may be unread than is known.
        assert!(state.take_commands().is_empty());
        assert_eq!(state.unread_total(), 0, "the badge goes at once");
        history_arrives(&mut state, "a");
        let commands = state.take_commands();
        assert!(matches!(
            &commands[..],
            [Command::MarkRead { conversation, messages, .. }]
                if *conversation == id("a") && messages.len() == 2
        ));
        assert_eq!(state.unread_total(), 0);
        assert_eq!(state.conversation(&id("a")).unwrap().settings.last_read, 20);
        // Seen as it arrives while the window has focus; nothing twice.
        state.apply(Event::Message(message("a", "Ann", 30, false)));
        let commands = state.take_commands();
        assert!(matches!(
            &commands[..],
            [Command::MarkRead { messages, .. }] if messages.len() == 1 && messages[0].timestamp == 30
        ));
        state.set_window_focused(false);
        state.apply(Event::Message(message("a", "Ann", 40, false)));
        assert!(state.take_commands().is_empty());
        assert_eq!(state.unread_total(), 1);
        state.set_window_focused(true);
        assert_eq!(state.take_commands().len(), 1);
        assert_eq!(state.unread_total(), 0);
    }

    #[test]
    fn new_messages_line_stays_where_reading_began() {
        let mut state = state();
        state.apply(Event::History {
            conversation: id("a"),
            messages: vec![
                message("a", "Ann", 10, false),
                message("a", "Ann", 20, false),
            ],
            reactions: Vec::new(),
            older: None,
        });
        state.apply(Event::Settings {
            conversation: id("a"),
            settings: ChatSettings {
                last_read: 10,
                ..ChatSettings::default()
            },
        });
        state.select(&id("a"));
        let firsts: Vec<bool> = state.messages().map(|m| m.first_unread).collect();
        assert_eq!(firsts, [false, true]);
    }

    #[test]
    fn read_elsewhere_clears_what_was_read_there() {
        let mut state = state();
        for ts in [10, 20, 30] {
            state.apply(Event::Message(message("a", "Ann", ts, false)));
        }
        state.take_notices();
        state.apply(Event::Settings {
            conversation: id("a"),
            settings: ChatSettings {
                last_read: 20,
                ..ChatSettings::default()
            },
        });
        assert_eq!(state.unread_total(), 1);
        state.apply(Event::Settings {
            conversation: id("a"),
            settings: ChatSettings {
                last_read: 30,
                ..ChatSettings::default()
            },
        });
        assert_eq!(state.unread_total(), 0);
        assert_eq!(state.take_notices(), [Notice::Withdraw(id("a"))]);
    }

    #[test]
    fn pinned_chats_come_first_in_their_order() {
        let mut state = state();
        state.apply(Event::Conversations(vec![conversation("b", false)]));
        for (chat, ts) in [("a", 30), ("b", 20), ("g", 10)] {
            state.apply(Event::Message(message(chat, "Ann", ts, false)));
        }
        assert_eq!(listed(&state), ["a", "b", "g"]);
        assert!(matches!(state.toggle_pin(&id("g")), Some(Command::SetPins(p)) if p == [id("g")]));
        state.toggle_pin(&id("b"));
        assert_eq!(listed(&state), ["g", "b", "a"]);
        assert!(
            matches!(state.move_pin(&id("b"), 0), Some(Command::SetPins(p)) if p == [id("b"), id("g")])
        );
        assert_eq!(listed(&state), ["b", "g", "a"]);
        state.toggle_pin(&id("b"));
        assert_eq!(listed(&state), ["g", "a", "b"]);
        assert!(state.move_pin_by(&id("a"), -1).is_none(), "not pinned");
        state.toggle_pin(&id("a"));
        assert!(state.move_pin_by(&id("g"), -1).is_none(), "already first");
        state.move_pin_by(&id("g"), 1);
        assert_eq!(listed(&state), ["a", "g", "b"]);
        state.toggle_pin(&id("a"));
        assert!(state.select_pinned(0).is_some());
        assert_eq!(state.selected().unwrap().id, id("g"));
        assert!(state.select_pinned(1).is_none());
    }

    #[test]
    fn archived_chats_have_their_own_list() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        state.select(&id("a"));
        assert!(matches!(
            state.archive(&id("a"), true),
            Some(Command::Archive { archived: true, .. })
        ));
        assert!(state.selected().is_none());
        assert_eq!(listed(&state), ["g"]);
        assert_eq!(state.archived(), (1, 0));
        state.show_archived(true);
        assert_eq!(listed(&state), ["a"]);
    }

    #[test]
    fn muted_chats_neither_notify_nor_count() {
        let mut state = notifying();
        state.mute(&id("a"), Some(u64::MAX));
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        assert!(state.take_notices().is_empty());
        assert_eq!(state.unread_total(), 0);
        assert_eq!(
            state
                .conversations()
                .find(|c| c.conversation.id == id("a"))
                .unwrap()
                .unread,
            1
        );
        state.mute(&id("a"), None);
        assert_eq!(state.unread_total(), 1);
    }

    #[test]
    fn marked_unread_counts_as_one_until_opened() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        state.select(&id("a"));
        history_arrives(&mut state, "a");
        state.take_commands();
        assert!(matches!(
            state.mark_unread(&id("a"), true),
            Some(Command::MarkUnread(_))
        ));
        assert!(state.selected().is_none());
        assert_eq!(state.unread_total(), 1);
        state.select(&id("a"));
        history_arrives(&mut state, "a");
        assert!(
            matches!(&state.take_commands()[..], [Command::MarkRead { messages, .. }] if messages.is_empty())
        );
        assert_eq!(state.unread_total(), 0);
    }

    #[test]
    fn receipts_edits_and_deletions_change_messages_in_place() {
        let mut state = state();
        state.select(&id("a"));
        state.apply(Event::Message(message("a", "You", 10, true)));
        let update = state.apply(Event::Delivered {
            conversation: id("a"),
            timestamps: vec![10],
            delivery: Delivery::Read,
        });
        assert!(update.rows);
        // Never backwards.
        state.apply(Event::Delivered {
            conversation: id("a"),
            timestamps: vec![10],
            delivery: Delivery::Delivered,
        });
        let target = state.messages().next().unwrap().message.reference();
        assert_eq!(
            state.messages().next().unwrap().message.delivery,
            Delivery::Read
        );
        state.apply(Event::Edited {
            conversation: id("a"),
            target: target.clone(),
            message: ChatMessage {
                body: "better".into(),
                edited_at: Some(50),
                ..message("a", "You", 10, true)
            },
        });
        let edited = state.messages().next().unwrap().message;
        assert_eq!(edited.body, "better");
        assert_eq!(edited.delivery, Delivery::Read);
        assert_eq!(edited.edits, [(50, "You at 10\nsecond line".to_string())]);
        state.apply(Event::Deleted {
            conversation: id("a"),
            target: target.clone(),
        });
        let deleted = state.messages().next().unwrap().message;
        assert!(deleted.deleted && deleted.body.is_empty());
        let update = state.apply(Event::Expired {
            conversation: id("a"),
            targets: vec![target],
        });
        assert_eq!(update.messages, MessagesUpdate::Reset);
        assert_eq!(state.messages().len(), 0);
    }

    /// Once the message being edited is gone, the composer writes a new message again.
    #[test]
    fn an_edit_of_a_gone_message_is_a_new_message() {
        let mut state = state();
        state.select(&id("a"));
        state.apply(Event::Message(ChatMessage {
            timestamp: now_millis(),
            ..message("a", "You", 0, true)
        }));
        let ours = state.messages().position(|m| m.message.outgoing).unwrap();
        assert!(state.edit(ours).is_some());
        let target = state.editing().unwrap().reference();
        state.apply(Event::Expired {
            conversation: id("a"),
            targets: vec![target],
        });
        assert!(state.editing().is_none());
        assert!(matches!(state.compose("new").0, Some(Command::Send { .. })));
    }

    #[test]
    fn composing_formats_and_edits() {
        let mut state = state();
        state.select(&id("a"));
        assert!(matches!(
            state.compose("*hi* there").0,
            Some(Command::Send { body, styles, .. }) if body == "hi there" && styles.len() == 1
        ));
        state.apply(Event::Message(ChatMessage {
            timestamp: now_millis(),
            ..message("a", "You", 0, true)
        }));
        state.apply(Event::Message(message("a", "Ann", 1, false)));
        let ours = state.messages().position(|m| m.message.outgoing).unwrap();
        let theirs = state.messages().position(|m| !m.message.outgoing).unwrap();
        assert_eq!(state.edit(theirs), None);
        assert!(state.edit(ours).is_some());
        assert!(matches!(
            state.compose("fixed").0,
            Some(Command::Edit { body, .. }) if body == "fixed"
        ));
        // Editing ends with the edit.
        assert!(matches!(
            state.compose("next").0,
            Some(Command::Send { .. })
        ));
    }

    /// Editing starts from the message as typed: sent again, it keeps its spoiler (which
    /// would otherwise be revealed to everyone), its formatting and whom it mentions.
    #[test]
    fn an_edit_keeps_spoilers_formatting_and_mentions() {
        let mut state = state();
        state.select(&id("a"));
        let body = "@Ann the butler did it";
        state.apply(Event::Message(ChatMessage {
            body: body.into(),
            timestamp: now_millis(),
            styles: vec![
                StyleRange {
                    start: 9,
                    end: 15,
                    style: crate::Style::Spoiler,
                },
                StyleRange {
                    start: 16,
                    end: 22,
                    style: crate::Style::Bold,
                },
            ],
            mentions: vec![Mention {
                start: 0,
                end: 4,
                service_id: "ann".into(),
            }],
            ..message("a", "You", 0, true)
        }));
        let ours = state.messages().position(|m| m.message.outgoing).unwrap();
        let typed = state.edit(ours).unwrap();
        assert_eq!(typed, "@Ann the ||butler|| *did it*");
        let Some(Command::Edit {
            body: sent,
            styles,
            mentions,
            ..
        }) = state.compose(&typed.replace("did", "done")).0
        else {
            panic!("edits");
        };
        assert_eq!(sent, "@Ann the butler done it");
        assert!(
            styles
                .iter()
                .any(|s| s.style == crate::Style::Spoiler && &sent[s.start..s.end] == "butler")
        );
        assert_eq!(styles.len(), 2);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].service_id, "ann");
    }

    #[test]
    fn mentions_are_picked_from_the_group_and_sent_where_they_stand() {
        let mut state = state();
        state.apply(Event::Conversations(vec![Conversation {
            members: ["Ann Lee", "Bob"]
                .map(|name| Person {
                    service_id: name.to_lowercase(),
                    name: name.into(),
                })
                .into(),
            ..conversation("g", true)
        }]));
        state.select(&id("g"));
        assert!(state.mention_candidates("hi").is_empty());
        let names = |draft| -> Vec<String> {
            state
                .mention_candidates(draft)
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        assert_eq!(names("hi @"), ["Ann Lee", "Bob"]);
        assert_eq!(names("hi @le"), ["Ann Lee"]);
        let draft = state.pick_mention("hi @le", 0).unwrap();
        assert_eq!(draft, "hi @Ann Lee ");
        let Some(Command::Send { body, mentions, .. }) = state.compose(&format!("{draft}*look*")).0
        else {
            panic!("sends");
        };
        assert_eq!(body, "hi @Ann Lee look");
        assert_eq!(
            mentions,
            [Mention {
                start: 3,
                end: 11,
                service_id: "ann lee".into()
            }]
        );
        // Forgotten once sent.
        assert!(
            matches!(state.compose("@Ann Lee").0, Some(Command::Send { mentions, .. }) if mentions.is_empty())
        );
    }

    #[test]
    fn typing_is_announced_sparingly() {
        let mut state = state();
        state.select(&id("a"));
        assert!(matches!(
            state.draft_changed("h"),
            Some(Command::Typing { typing: true, .. })
        ));
        assert!(state.draft_changed("hi").is_none());
        assert!(matches!(
            state.draft_changed(""),
            Some(Command::Typing { typing: false, .. })
        ));
        assert!(state.draft_changed("").is_none());
    }

    #[test]
    fn typing_shows_until_their_message_arrives() {
        let mut state = state();
        state.apply(Event::Typing {
            conversation: id("a"),
            sender_id: "ann".into(),
            sender: "Ann".into(),
            typing: true,
        });
        assert_eq!(state.typing(&id("a")), ["Ann"]);
        assert!(state.typing_expires().is_some());
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        assert!(state.typing(&id("a")).is_empty());
    }

    #[test]
    fn deleting_here_removes_and_for_everyone_leaves_a_tombstone() {
        let mut state = state();
        state.select(&id("a"));
        let now = now_millis();
        for ts in [now - 2, now - 1] {
            state.apply(Event::Message(ChatMessage {
                timestamp: ts,
                ..message("a", "You", 0, true)
            }));
        }
        let (command, _) = state.delete(0, false);
        assert!(matches!(
            command,
            Some(Command::Delete {
                for_everyone: false,
                ..
            })
        ));
        assert_eq!(state.messages().len(), 1);
        let (command, _) = state.delete(0, true);
        assert!(matches!(
            command,
            Some(Command::Delete {
                for_everyone: true,
                ..
            })
        ));
        assert!(state.messages().next().unwrap().message.deleted);
        // Already deleted for everyone.
        assert!(state.delete(0, true).0.is_none());
    }

    #[test]
    fn older_pages_prepend_and_bring_their_reactions_home() {
        let mut state = state();
        let a = id("a");
        assert!(matches!(
            state.select(&a),
            Some(Command::LoadHistory { before: None, .. })
        ));
        let old = message("a", "Ann", 10, false);
        state.apply(Event::History {
            conversation: a.clone(),
            messages: vec![message("a", "Ann", 30, false)],
            // A reaction stored with the newer page, to a message in the older one.
            reactions: vec![(old.reference(), reaction("👍", "bob", false), false)],
            older: Some(30),
        });
        assert!(state.has_older());
        assert!(matches!(
            state.load_older(),
            Some(Command::LoadHistory {
                before: Some(30),
                ..
            })
        ));
        assert!(state.load_older().is_none(), "once at a time");
        let update = state.apply(Event::History {
            conversation: a,
            messages: vec![old, message("a", "Ann", 20, false)],
            reactions: Vec::new(),
            older: None,
        });
        assert_eq!(update.messages, MessagesUpdate::Prepended(2));
        assert!(!state.has_older());
        assert!(state.load_older().is_none());
        let first = state.messages().next().unwrap().message;
        assert_eq!((first.timestamp, first.reactions.len()), (10, 1));
    }

    #[test]
    fn forwarding_sends_a_copy_to_the_chosen_chat() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        state.apply(Event::Message(message("g", "Bob", 20, false)));
        state.select(&id("a"));
        assert!(state.start_forward(0));
        let titles: Vec<_> = state
            .forward_targets("")
            .iter()
            .map(|c| c.id.0.clone())
            .collect();
        assert_eq!(titles, ["g", "a"]);
        assert_eq!(state.forward_targets("G").len(), 1);
        assert_eq!(state.forwarded().as_deref(), Some("Ann at 10\nsecond line"));
        let Some(Command::Forward {
            conversation, body, ..
        }) = state.forward_to(&id("g"))
        else {
            panic!("forwards");
        };
        assert_eq!(
            (conversation, body),
            (id("g"), "Ann at 10\nsecond line".into())
        );
        assert!(!state.forwarding());
        assert!(state.forward_to(&id("g")).is_none(), "once");
        assert!(state.forwarded().is_none());
    }

    #[test]
    fn picked_messages_are_copied_forwarded_and_deleted_together() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        state.apply(Event::Message(message("a", "Me", 20, true)));
        state.apply(Event::Message(message("a", "Ann", 30, false)));
        state.select(&id("a"));
        assert!(state.toggle_selected(0).rows);
        state.select_through(2);
        assert!(
            state.messages().all(|v| v.selected),
            "the range from the anchor"
        );
        state.toggle_selected(1);
        assert_eq!(
            state.selected_text(),
            "Ann at 10\nsecond line\n\nAnn at 30\nsecond line"
        );

        assert!(state.start_forward_selection());
        assert_eq!(state.forwarded().as_deref(), Some("2 messages"));
        assert!(matches!(
            state.forward_to(&id("g")),
            Some(Command::Forward { body, .. }) if body == "Ann at 10\nsecond line"
        ));
        // Among what else followed (read receipts).
        let forwards: Vec<_> = state
            .take_commands()
            .into_iter()
            .filter(|c| matches!(c, Command::Forward { .. }))
            .collect();
        assert!(matches!(
            forwards.as_slice(),
            [Command::Forward { body, .. }] if body == "Ann at 30\nsecond line"
        ));
        assert!(state.selection().is_empty(), "done with them");

        state.toggle_selected(0);
        state.toggle_selected(2);
        let (command, update) = state.delete_selection(false);
        assert!(matches!(command, Some(Command::Delete { .. })));
        let deletes = state
            .take_commands()
            .into_iter()
            .filter(|c| matches!(c, Command::Delete { .. }))
            .count();
        assert_eq!(deletes, 1, "the second goes along");
        assert_eq!(update, Update::reset());
        assert_eq!(state.messages().len(), 1);
        assert!(state.selection().is_empty());

        state.toggle_selected(0);
        assert_eq!(state.selection().len(), 1);
        state.select(&id("g"));
        assert!(state.selection().is_empty(), "picks belong to the chat");
    }

    #[test]
    fn search_results_load_their_page_and_are_scrolled_to() {
        let mut state = state();
        let a = id("a");
        state.select(&a);
        state.apply(Event::History {
            conversation: a.clone(),
            messages: vec![message("a", "Ann", 30, false)],
            reactions: Vec::new(),
            older: Some(30),
        });
        state.take_commands();
        state.open_chat_search();
        assert!(matches!(
            state.search_chat(" moon "),
            Some(Command::SearchChat { query, .. }) if query == "moon"
        ));
        assert!(matches!(state.chat_search(), Some(("moon", None))));
        let old = message("a", "Ann", 10, false);
        state.apply(Event::SearchResults {
            conversation: a.clone(),
            query: "moon".into(),
            messages: vec![old.clone()],
        });
        assert_eq!(state.chat_search().unwrap().1.unwrap().len(), 1);
        state.show_result(0);
        assert!(state.chat_search().is_none());
        // Not loaded yet: the page before is asked for, then it is scrolled to.
        assert!(matches!(
            &state.take_commands()[..],
            [Command::LoadHistory {
                before: Some(30),
                ..
            }]
        ));
        assert_eq!(state.jump(), None);
        state.apply(Event::History {
            conversation: a,
            messages: vec![old, message("a", "Ann", 20, false)],
            reactions: Vec::new(),
            older: None,
        });
        assert_eq!(state.jump(), Some((0, 1)));
        assert!(state.messages().next().unwrap().highlighted);
    }

    #[test]
    fn snippets_show_the_match_in_context() {
        assert_eq!(
            snippet("see\nthe moon", "MOON"),
            ("see the moon".into(), Some(8..12))
        );
        let long = format!("{} needle {}", "a".repeat(100), "b".repeat(100));
        let (shown, found) = snippet(&long, "needle");
        assert!(shown.starts_with('…') && shown.ends_with('…'));
        assert_eq!(&shown[found.unwrap()], "needle");
        assert!(shown.chars().count() < 140);
        let (shown, found) = snippet("Grüße aus München", "münchen");
        assert_eq!(&shown[found.unwrap()], "München");
        assert_eq!(
            snippet("nothing here", "moon"),
            ("nothing here".into(), None)
        );
    }

    #[test]
    fn media_overview_sorts_attachments_and_links() {
        let mut state = state();
        state.select(&id("a"));
        assert!(matches!(state.open_media(), Some(Command::LoadMedia(i)) if i == id("a")));
        assert!(matches!(state.media(), Some(None)));
        let photo = with_attachment("a", 3, "p", AttachmentKind::Image);
        let file = with_attachment("a", 2, "f", AttachmentKind::File);
        let link = ChatMessage {
            body: "see https://example.org/x and http://b.c/d".into(),
            ..message("a", "Ann", 1, false)
        };
        state.apply(Event::Media {
            conversation: id("a"),
            messages: vec![photo, file, link],
        });
        let overview = state.media().unwrap().unwrap();
        assert_eq!(overview.media.len(), 1);
        assert_eq!(overview.files.len(), 1);
        let links: Vec<_> = overview.links.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(links, ["https://example.org/x", "http://b.c/d"]);
        // Older than the loaded history, yet it can be fetched.
        assert!(state.fetch_attachment(&AttachmentId("p".into())).is_some());
    }

    #[test]
    fn moving_between_chats() {
        let mut state = state();
        state.apply(Event::Conversations(vec![conversation("b", false)]));
        for (chat, ts) in [("a", 30), ("b", 20), ("g", 10)] {
            state.apply(Event::Message(message(chat, "Ann", ts, false)));
        }
        state.select_relative(1);
        assert_eq!(state.selected().unwrap().id, id("a"));
        state.select_relative(1);
        assert_eq!(state.selected().unwrap().id, id("b"));
        assert!(state.select_relative(5).is_none());
        state.select_relative(-1);
        assert_eq!(state.selected().unwrap().id, id("a"));
        // "a" and "b" were read by opening them; "g" is still unread.
        state.select_next_unread();
        assert_eq!(state.selected().unwrap().id, id("g"));
        assert!(state.select_next_unread().is_none());
    }

    #[test]
    fn message_requests_wait_for_an_answer() {
        let mut state = state();
        state.apply(Event::Message(message("a", "Ann", 10, false)));
        state.select(&id("a"));
        let chat = |state: &ChatState| state.conversation(&id("a")).unwrap().clone();
        // Unknown until the phone's settings were read.
        assert!(!state.is_request(&chat(&state)));
        state.apply(Event::StorageRead);
        assert!(state.is_request(&chat(&state)));
        assert!(!state.can_compose("hi"));
        state.apply(Event::Message(with_attachment(
            "a",
            11,
            "p",
            AttachmentKind::Image,
        )));
        let photo = AttachmentId("p".into());
        assert!(state.fetch_attachment(&photo).is_none(), "a request");
        assert!(matches!(
            state.respond(&id("a"), RequestResponse::Accept),
            Some(Command::Respond {
                response: RequestResponse::Accept,
                ..
            })
        ));
        assert!(!state.is_request(&chat(&state)));
        assert!(state.can_compose("hi"));
        assert!(state.fetch_attachment(&photo).is_some(), "accepted");
        // Blocked chats can't be written to; deleting clears the chat.
        state.block(&id("a"), true);
        assert!(!state.can_compose("hi"));
        state.apply(Event::Cleared(id("a")));
        assert!(state.selected().is_none());
        assert_eq!(listed(&state), ["g"]);
    }

    #[test]
    fn folders_show_their_chats() {
        let mut state = state();
        state.apply(Event::Conversations(vec![conversation("b", false)]));
        for (chat, ts) in [("a", 30), ("b", 20), ("g", 10)] {
            state.apply(Event::Message(message(chat, "Ann", ts, false)));
        }
        state.apply(Event::Folders(vec![
            crate::ChatFolder {
                name: "Friends".into(),
                included: vec![id("a")],
                all_groups: true,
                ..Default::default()
            },
            crate::ChatFolder {
                name: "Unread people".into(),
                all_individual: true,
                excluded: vec![id("b")],
                only_unread: true,
                show_muted: true,
                ..Default::default()
            },
        ]));
        state.show_folder(Some(0));
        assert_eq!(listed(&state), ["a", "g"]);
        assert_eq!(state.folder_unread(0), 2);
        state.show_folder(Some(1));
        assert_eq!(listed(&state), ["a"]);
        state.select(&id("a"));
        // Read now, yet the open chat stays listed.
        assert_eq!(listed(&state), ["a"]);
        state.show_folder(None);
        assert_eq!(listed(&state), ["a", "b", "g"]);
        // A folder that went away on the phone isn't shown any more.
        state.show_folder(Some(1));
        state.apply(Event::Folders(Vec::new()));
        assert_eq!(state.folder(), None);
    }

    #[test]
    fn strangers_asking_to_write_are_not_offered_for_groups() {
        let mut state = state();
        let mut ann = conversation("c:ann", false);
        ann.settings.accepted = true;
        state.apply(Event::Conversations(vec![
            ann,
            conversation("c:stranger", false),
        ]));
        state.apply(Event::StorageRead);
        let names: Vec<_> = state
            .group_candidates("")
            .iter()
            .map(|c| c.id.0.clone())
            .collect();
        assert_eq!(names, ["c:ann"]);
    }

    #[test]
    fn each_chat_keeps_its_draft() {
        let mut state = state();
        let sent = ChatMessage {
            timestamp: now_millis(),
            ..message("a", "Me", 0, true)
        };
        state.apply(Event::Message(sent));
        state.apply(Event::Message(message("g", "Bob", 20, false)));
        state.select(&id("a"));
        state.draft_changed("for Ann");
        state.select(&id("g"));
        assert_eq!(state.draft(), "", "not Ann's");
        state.draft_changed("for the group");
        state.select(&id("a"));
        assert_eq!(state.draft(), "for Ann");
        // An edit is written over it, and leaves it be.
        assert!(state.edit_last().is_some());
        state.draft_changed("corrected");
        state.cancel_edit();
        assert_eq!(state.draft(), "for Ann");
        // Sent, it is gone.
        assert!(state.compose("for Ann").0.is_some());
        assert_eq!(state.draft(), "");
        state.select(&id("g"));
        assert_eq!(state.draft(), "for the group");
    }

    #[test]
    fn forwarding_offers_only_chats_it_can_go_to() {
        let mut state = state();
        let mut ann = conversation("c:ann", false);
        ann.settings.accepted = true;
        let mut bob = conversation("c:bob", false);
        bob.settings.accepted = true;
        bob.settings.blocked = true;
        state.apply(Event::Conversations(vec![
            ann,
            bob,
            conversation("c:stranger", false),
        ]));
        state.apply(Event::StorageRead);
        for chat in ["c:ann", "c:bob", "c:stranger"] {
            state.apply(Event::Message(message(chat, "Someone", 10, false)));
        }
        let ids: Vec<_> = state
            .forward_targets("")
            .iter()
            .map(|c| c.id.0.clone())
            .collect();
        assert!(ids.iter().any(|id| id == "c:ann"));
        assert!(!ids.iter().any(|id| id == "c:bob" || id == "c:stranger"));
    }

    #[test]
    fn groups_are_made_and_changed() {
        let mut state = state();
        state.apply(Event::Conversations(vec![
            conversation("c:ann", false),
            conversation("c:bob", false),
        ]));
        let names: Vec<_> = state
            .group_candidates("")
            .iter()
            .map(|c| c.id.0.clone())
            .collect();
        assert_eq!(names, ["a", "c:ann", "c:bob"]);
        assert!(
            state.create_group("  ", &[id("c:ann")]).is_none(),
            "needs a name"
        );
        assert!(matches!(
            state.create_group("Climbing", &[id("c:ann")]),
            Some(Command::CreateGroup { title, members }) if title == "Climbing" && members == ["ann"]
        ));
        // The new group opens.
        state.apply(Event::Conversations(vec![conversation("g:new", true)]));
        state.apply(Event::GroupCreated(id("g:new")));
        assert_eq!(state.selected().unwrap().id, id("g:new"));
        assert!(
            state
                .edit_group(&id("g:new"), crate::GroupEdit::Title("g:new".into()))
                .is_none()
        );
        assert!(
            state
                .edit_group(&id("g:new"), crate::GroupEdit::Leave)
                .is_some()
        );
        // Once left, read only.
        state.apply(Event::Conversations(vec![Conversation {
            left: true,
            ..conversation("g:new", true)
        }]));
        assert!(!state.can_compose("hi"));
        assert!(
            state
                .edit_group(&id("g:new"), crate::GroupEdit::Leave)
                .is_none()
        );
    }

    #[test]
    fn a_group_is_joined_through_its_link() {
        let mut state = state();
        let link = "https://signal.group/#abc";
        assert!(matches!(
            state.open_join(Some(" https://signal.group/#abc ")),
            Some(Command::PreviewGroupLink(typed)) if typed == link
        ));
        let (typed, preview, busy) = state.join().unwrap();
        assert_eq!((typed, preview, busy), (link, None, true));
        assert!(state.join_group().is_none(), "nothing looked up yet");
        // A failed lookup leaves the dialog open for another try.
        state.apply(Event::Error("no such group".into()));
        assert!(!state.join().unwrap().2);
        let preview = crate::GroupPreview {
            link: link.into(),
            title: "Climbing".into(),
            members: 3,
            approval: true,
            requested: false,
        };
        state.apply(Event::GroupPreview(crate::GroupPreview {
            link: "https://signal.group/#other".into(),
            ..preview.clone()
        }));
        assert_eq!(state.join().unwrap().1, None, "an answer for another link");
        state.apply(Event::GroupPreview(preview.clone()));
        assert_eq!(state.join().unwrap().1, Some(&preview));
        assert!(matches!(state.join_group(), Some(Command::JoinGroup(typed)) if typed == link));
        assert!(state.join_group().is_none(), "already joining");
        state.apply(Event::Conversations(vec![Conversation {
            requested: true,
            left: true,
            ..conversation("g:climbing", true)
        }]));
        state.apply(Event::GroupJoined(id("g:climbing")));
        assert!(state.join().is_none(), "done");
        assert_eq!(state.selected().unwrap().id, id("g:climbing"));
        // Waiting for an admin: the request can be taken back, nothing else.
        assert!(
            state
                .edit_group(&id("g:climbing"), crate::GroupEdit::CancelJoinRequest)
                .is_some()
        );
        assert!(
            state
                .edit_group(&id("g:climbing"), crate::GroupEdit::Leave)
                .is_none()
        );
    }

    #[test]
    fn people_are_chosen_for_a_group() {
        let mut state = state();
        state.apply(Event::Conversations(vec![
            conversation("c:ann", false),
            conversation("c:bob", false),
            Conversation {
                members: vec![Person {
                    service_id: "ann".into(),
                    name: "Ann".into(),
                }],
                ..conversation("g:club", true)
            },
        ]));
        state.open_group_draft(None);
        state.toggle_group_member(&id("c:ann"));
        state.toggle_group_member(&id("c:bob"));
        state.toggle_group_member(&id("c:bob"));
        assert!(matches!(state.group_draft(), Some((true, [chosen])) if *chosen == id("c:ann")));
        assert!(matches!(
            state.finish_group_draft("Club"),
            Some(Command::CreateGroup { members, .. }) if members == ["ann"]
        ));
        assert!(state.group_draft().is_none());
        // Adding to a group: its members aren't offered again.
        state.open_group_draft(Some(id("g:club")));
        let offered: Vec<_> = state
            .group_candidates("")
            .iter()
            .map(|c| c.id.0.clone())
            .collect();
        assert_eq!(offered, ["a", "c:bob"]);
        state.toggle_group_member(&id("c:bob"));
        assert!(matches!(
            state.finish_group_draft(""),
            Some(Command::EditGroup { edit: crate::GroupEdit::AddMembers(m), .. }) if m == ["bob"]
        ));
    }

    #[test]
    fn stories_are_watched_in_order() {
        let mut state = state();
        let story = |who: &str, chat: &str, ts: u64, viewed| crate::Story {
            author_id: who.into(),
            author: who.into(),
            conversation: id(chat),
            timestamp: ts,
            content: crate::StoryContent::Text {
                text: format!("{who} {ts}"),
                foreground: 0,
                background: 0,
            },
            viewed,
            outgoing: false,
        };
        state.apply(Event::Stories(vec![
            story("ann", "a", 10, true),
            story("ann", "a", 20, false),
            story("bob", "g", 30, true),
            // A stranger's: no chat with them, so not listed (watching would tell them).
            story("eve", "c:eve", 25, false),
        ]));
        assert!(state.open_story(&id("c:eve")).is_none());
        let rings: Vec<_> = state
            .story_rings()
            .iter()
            .map(|r| (r.conversation.0.clone(), r.unviewed))
            .collect();
        assert_eq!(
            rings,
            [("a".into(), true), ("g".into(), false)],
            "unseen first"
        );
        assert_eq!(state.unviewed_stories(), 1);
        // Opens at the first unseen, and marks it seen.
        assert!(
            matches!(state.open_story(&id("a")), Some(Command::ViewStory(r)) if r.timestamp == 20)
        );
        assert_eq!(
            state.current_story().map(|(_, at, count)| (at, count)),
            Some((1, 2))
        );
        assert_eq!(state.unviewed_stories(), 0);
        assert!(state.step_story(-1).is_none(), "already seen");
        assert_eq!(state.current_story().unwrap().1, 0);
        state.step_story(1);
        state.step_story(1);
        assert!(state.current_story().is_none(), "closes after the last");
        assert!(
            state
                .send_story(&id("g"), crate::NewStory::Text("hi".into()))
                .is_some()
        );
        assert!(
            state
                .send_story(&id("a"), crate::NewStory::Text("hi".into()))
                .is_none(),
            "groups only"
        );
        // When an older story expires, the one shown stays shown.
        state.open_story(&id("a"));
        state.step_story(1);
        assert_eq!(state.current_story().unwrap().0.timestamp, 20);
        state.apply(Event::Stories(vec![
            story("ann", "a", 20, true),
            story("bob", "g", 30, true),
        ]));
        assert_eq!(
            state
                .current_story()
                .map(|(s, at, count)| (s.timestamp, at, count)),
            Some((20, 0, 1))
        );
        // Once it expires too, the viewer closes.
        state.apply(Event::Stories(vec![story("bob", "g", 30, true)]));
        assert!(state.current_story().is_none());
    }
}

#[cfg(test)]
mod edit_props {
    use super::*;
    use crate::{ConversationKind, Person, Style};
    use proptest::prelude::*;

    fn group_state() -> ChatState {
        let mut chat = ChatState::default();
        chat.apply(Event::Linked {
            account: "me".into(),
        });
        chat.apply(Event::Synced);
        chat.apply(Event::Conversations(vec![Conversation {
            id: ConversationId("g".into()),
            title: "Group".into(),
            kind: ConversationKind::Group,
            members: vec![Person {
                service_id: "ann-id".into(),
                name: "Ann".into(),
            }],
            settings: ChatSettings {
                accepted: true,
                ..Default::default()
            },
            ..Default::default()
        }]));
        chat.select(&ConversationId("g".into()));
        chat.apply(Event::History {
            conversation: ConversationId("g".into()),
            messages: Vec::new(),
            reactions: Vec::new(),
            older: None,
        });
        chat
    }

    /// Sends `typed` (picking `@Ann` from the candidates first if it mentions her), lets the
    /// sent message arrive, then edits it without changing anything: returns what was sent and
    /// what the edit sends.
    /// What went out: the text, its formatting and how many it mentions.
    type Sent = (String, Vec<StyleRange>, usize);

    /// What a message typed as `typed` sends, and then an unchanged edit of it.
    fn send_then_edit(typed: &str) -> (Sent, Sent) {
        let mut chat = group_state();
        if typed.contains("@Ann") {
            // As the composer does: "@A" typed, then the candidate picked.
            chat.pick_mention("@A", 0).expect("Ann is a candidate");
        }
        let (command, _) = chat.compose(typed);
        let Some(Command::Send {
            body,
            styles,
            mentions,
            timestamp,
            conversation,
            ..
        }) = command
        else {
            panic!("not sent: {typed:?}");
        };
        chat.apply(Event::Message(ChatMessage {
            conversation,
            sender_id: "me".into(),
            sender: "You".into(),
            body: body.clone(),
            styles: styles.clone(),
            // As the backend reads the message back: with whom it mentions.
            mentions: mentions.clone(),
            timestamp,
            outgoing: true,
            ..Default::default()
        }));
        let index = chat.messages().len() - 1;
        let text = chat.edit(index).expect("ours, and recent");
        let (command, _) = chat.compose(&text);
        let Some(Command::Edit {
            body: edited,
            styles: edited_styles,
            mentions: edited_mentions,
            ..
        }) = command
        else {
            panic!("not an edit");
        };
        (
            (body, styles, mentions.len()),
            (edited, edited_styles, edited_mentions.len()),
        )
    }

    #[test]
    fn editing_without_changes_keeps_formatting_and_mentions() {
        for typed in [
            "hi @Ann",
            "*bold* and ||the butler did it||",
            "`code` ~gone~ _it_",
        ] {
            let (sent, edit) = send_then_edit(typed);
            println!("{typed:?}: sent {sent:?}\n  unchanged edit sends {edit:?}");
            assert_eq!(edit, sent, "{typed:?}");
        }
    }

    fn spoiler_message(body: &str, secret: std::ops::Range<usize>) -> ChatMessage {
        ChatMessage {
            conversation: ConversationId("g".into()),
            sender_id: "ann-id".into(),
            sender: "Ann".into(),
            body: body.into(),
            styles: vec![StyleRange {
                start: secret.start,
                end: secret.end,
                style: Style::Spoiler,
            }],
            timestamp: now_millis() - 1000,
            ..Default::default()
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, failure_persistence: None, ..ProptestConfig::default() })]

        /// A spoiler stays hidden in what is shown around its message: the chat list's preview,
        /// the notification, and the quote of a reply.
        #[test]
        fn spoilers_stay_hidden_around_the_message(before in "[a-z ]{0,8}", secret in "[A-Z]{4,8}", after in "[a-z ]{0,8}") {
            let body = format!("{before}{secret}{after}");
            let at = before.len()..before.len() + secret.len();
            let mut chat = group_state();
            chat.set_notifications(true, crate::settings::NotificationContent::NameAndMessage);
            chat.set_window_focused(false);
            chat.take_notices();
            chat.apply(Event::Message(spoiler_message(&body, at)));
            let preview = chat.conversations().next().unwrap().preview();
            let notices = chat.take_notices();
            let last = chat.messages().len() - 1;
            chat.reply_to(last);
            let quote = chat.replying().map(|q| q.text.clone()).unwrap_or_default();
            let leaks: Vec<String> = [
                ("chat list preview", preview.clone()),
                ("notification", format!("{notices:?}")),
                ("reply quote", quote.clone()),
            ]
            .into_iter()
            .filter(|(_, shown)| shown.contains(&secret))
            .map(|(what, shown)| format!("{what}: {shown}"))
            .collect();
            prop_assert!(leaks.is_empty(), "spoiler {secret:?} of {body:?} shown in {leaks:#?}");
        }
    }
}
#[cfg(test)]
mod read_props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, failure_persistence: None, ..ProptestConfig::default() })]

        /// With the chat open in a focused window, every incoming message that arrives is read:
        /// it goes into a MarkRead (a read receipt), and a disappearing one's timer is started.
        #[test]
        fn every_message_seen_in_an_open_chat_is_marked_read(arrivals in prop::collection::vec((0u64..20, any::<bool>()), 1..8)) {
            let base = now_millis() - 3_600_000;
            let id = ConversationId("a".into());
            let mut chat = ChatState::default();
            chat.apply(Event::Linked { account: "me".into() });
            chat.apply(Event::Synced);
            chat.apply(Event::Conversations(vec![Conversation {
                id: id.clone(),
                title: "Alice".into(),
                settings: ChatSettings { accepted: true, ..Default::default() },
                ..Default::default()
            }]));
            chat.select(&id);
            chat.apply(Event::History { conversation: id.clone(), messages: Vec::new(), reactions: Vec::new(), older: None });
            chat.set_window_focused(true);
            let mut read = Vec::new();
            let mut expiring = Vec::new();
            let mut sent = Vec::new();
            for (t, disappearing) in arrivals {
                let message = ChatMessage {
                    conversation: id.clone(),
                    sender_id: "alice".into(),
                    sender: "Alice".into(),
                    body: format!("at {t}"),
                    timestamp: base + t * 1000,
                    expire_timer: disappearing.then_some(3600),
                    ..Default::default()
                };
                if !sent.contains(&message.timestamp) {
                    sent.push(message.timestamp);
                }
                chat.apply(Event::Message(message));
                for command in chat.take_commands() {
                    if let Command::MarkRead { messages, expiring: e, .. } = command {
                        read.extend(messages.into_iter().map(|m| m.timestamp));
                        expiring.extend(e.into_iter().map(|(m, _)| m.timestamp));
                    }
                }
            }
            let unread: Vec<u64> = sent.iter().copied().filter(|t| !read.contains(t)).map(|t| (t - base) / 1000).collect();
            prop_assert!(unread.is_empty(), "never marked read (no receipt, timer never started): {unread:?} of {:?}", sent.iter().map(|t| (t - base) / 1000).collect::<Vec<_>>());
        }
    }
}

#[cfg(test)]
mod model_tests {
    //! The Slint UI keeps its message rows in sync only through the `Update`s that `apply` and the
    //! actions return (b33p-slint/src/ui.rs `render`), merged per batch of events (main.rs). This
    //! replays that bookkeeping on a plain Vec and checks it against `messages()`.
    use super::*;
    use crate::{AfterFinding, ConversationKind, Delivery, Reaction};
    use proptest::prelude::*;
    use std::sync::OnceLock;

    const CONVS: [&str; 2] = ["a", "g"];
    const SENDERS: [&str; 3] = ["alice", "bob", "me"];

    fn base() -> u64 {
        static BASE: OnceLock<u64> = OnceLock::new();
        // An hour ago, on the minute: recent enough to edit and delete for everyone.
        *BASE.get_or_init(|| (now_millis() - 3_600_000) / 60_000 * 60_000)
    }

    fn ts(t: u8) -> u64 {
        base() + u64::from(t) * 1000
    }

    fn cid(c: usize) -> ConversationId {
        ConversationId(CONVS[c].into())
    }

    fn msg(c: usize, sender: usize, t: u8) -> ChatMessage {
        ChatMessage {
            conversation: cid(c),
            sender_id: SENDERS[sender].into(),
            sender: SENDERS[sender].into(),
            body: format!("{}@{t}", SENDERS[sender]),
            timestamp: ts(t),
            outgoing: sender == 2,
            ..Default::default()
        }
    }

    fn target(sender: usize, t: u8) -> MessageRef {
        MessageRef {
            author_id: SENDERS[sender].into(),
            timestamp: ts(t),
        }
    }

    #[derive(Clone, Debug)]
    enum Ev {
        Msg(usize, usize, u8),
        Latest(usize, usize, u8),
        History {
            conv: usize,
            msgs: Vec<(usize, u8)>,
            older: Option<u8>,
        },
        Edited(usize, usize, u8),
        Deleted(usize, usize, u8),
        Expiring(usize, usize, u8),
        Expired(usize, Vec<(usize, u8)>),
        Cleared(usize),
        Delivered(usize, Vec<u8>),
        Reaction {
            conv: usize,
            sender: usize,
            t: u8,
            from: usize,
            remove: bool,
        },
        Sent(usize),
        SendFailed(usize),
        Found(usize),
    }

    #[derive(Clone, Debug)]
    enum Op {
        Batch(Vec<Ev>),
        Select(usize),
        Deselect,
        Compose,
        LoadOlder,
        Delete(usize, bool),
        Edit(usize),
        ReplyTo(usize),
        Reveal(usize),
        NotificationReply(usize),
        Archive,
    }

    fn ev() -> impl Strategy<Value = Ev> {
        let conv = 0usize..2;
        let sender = 0usize..3;
        let t = 0u8..12;
        prop_oneof![
            6 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Msg(c, s, t)),
            1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Latest(c, s, t)),
            4 => (conv.clone(), prop::collection::vec((sender.clone(), t.clone()), 0..5), prop::option::of(t.clone()))
                .prop_map(|(conv, msgs, older)| Ev::History { conv, msgs, older }),
            1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Edited(c, s, t)),
            1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Deleted(c, s, t)),
            1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Expiring(c, s, t)),
            1 => (conv.clone(), prop::collection::vec((sender.clone(), t.clone()), 0..3)).prop_map(|(c, v)| Ev::Expired(c, v)),
            1 => conv.clone().prop_map(Ev::Cleared),
            1 => (conv.clone(), prop::collection::vec(t.clone(), 0..3)).prop_map(|(c, v)| Ev::Delivered(c, v)),
            1 => (conv.clone(), sender.clone(), t.clone(), sender.clone(), any::<bool>())
                .prop_map(|(conv, sender, t, from, remove)| Ev::Reaction { conv, sender, t, from, remove }),
            2 => (0usize..4).prop_map(Ev::Sent),
            1 => (0usize..4).prop_map(Ev::SendFailed),
            1 => conv.prop_map(Ev::Found),
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            8 => prop::collection::vec(ev(), 1..5).prop_map(Op::Batch),
            3 => (0usize..2).prop_map(Op::Select),
            1 => Just(Op::Deselect),
            2 => Just(Op::Compose),
            2 => Just(Op::LoadOlder),
            2 => (0usize..8, any::<bool>()).prop_map(|(i, e)| Op::Delete(i, e)),
            1 => (0usize..8).prop_map(Op::Edit),
            1 => (0usize..8).prop_map(Op::ReplyTo),
            1 => (0usize..8).prop_map(Op::Reveal),
            1 => (0usize..2).prop_map(Op::NotificationReply),
            1 => Just(Op::Archive),
        ]
    }

    /// What a row of the UI's model shows, as far as telling messages apart goes.
    #[derive(Clone, Debug, PartialEq)]
    struct Row {
        timestamp: u64,
        sender_id: String,
        body: String,
        unsent: Option<Unsent>,
        delivery: Delivery,
        deleted: bool,
        reactions: usize,
        expires_at: Option<u64>,
    }

    fn row(view: &MessageView) -> Row {
        let m = view.message;
        Row {
            timestamp: m.timestamp,
            sender_id: m.sender_id.clone(),
            body: view.body.to_owned(),
            unsent: m.unsent,
            delivery: m.delivery,
            deleted: m.deleted,
            reactions: m.reactions.len(),
            expires_at: m.expires_at,
        }
    }

    /// ui.rs `render`, for the message rows: a Slint `VecModel` ignores `set_row_data` past its
    /// end and panics on `insert` past it.
    fn render(model: &mut Vec<Row>, chat: &ChatState, update: Update) {
        match update.messages {
            MessagesUpdate::Unchanged => {}
            MessagesUpdate::Appended(n) => model.extend(chat.last_messages(n).map(|m| row(&m))),
            MessagesUpdate::Prepended(n) => {
                let items: Vec<Row> = chat.messages().take(n + 1).map(|m| row(&m)).collect();
                for (r, item) in items.into_iter().enumerate() {
                    if r < n {
                        model.insert(r, item);
                    } else if r < model.len() {
                        model[r] = item;
                    }
                }
            }
            MessagesUpdate::Reset => *model = chat.messages().map(|m| row(&m)).collect(),
        }
        if update.rows && update.messages != MessagesUpdate::Reset {
            for (r, m) in chat.messages().enumerate() {
                if r < model.len() {
                    model[r] = row(&m);
                }
            }
        }
    }

    struct Sim {
        chat: ChatState,
        model: Vec<Row>,
        /// Sends shown while sending: conversation and timestamp.
        sending: Vec<(ConversationId, u64)>,
        log: Vec<String>,
    }

    impl Sim {
        fn new() -> Self {
            let mut chat = ChatState::default();
            chat.apply(Event::Linked {
                account: "me".into(),
            });
            chat.apply(Event::Synced);
            chat.apply(Event::Conversations(vec![
                Conversation {
                    id: cid(0),
                    title: "Alice".into(),
                    settings: ChatSettings {
                        accepted: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Conversation {
                    id: cid(1),
                    title: "Group".into(),
                    kind: ConversationKind::Group,
                    settings: ChatSettings {
                        accepted: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ]));
            chat.set_window_focused(true);
            Sim {
                chat,
                model: Vec::new(),
                sending: Vec::new(),
                log: Vec::new(),
            }
        }

        fn event(&mut self, e: &Ev) -> Option<Event> {
            Some(match e.clone() {
                Ev::Msg(c, s, t) => Event::Message(msg(c, s, t)),
                Ev::Latest(c, s, t) => Event::Latest(msg(c, s, t)),
                Ev::History { conv, msgs, older } => Event::History {
                    conversation: cid(conv),
                    messages: {
                        let mut m: Vec<ChatMessage> =
                            msgs.iter().map(|&(s, t)| msg(conv, s, t)).collect();
                        m.sort_by_key(|m| (m.timestamp, m.sender_id.clone()));
                        m
                    },
                    reactions: Vec::new(),
                    older: older.map(ts),
                },
                Ev::Edited(c, s, t) => Event::Edited {
                    conversation: cid(c),
                    target: target(s, t),
                    message: ChatMessage {
                        body: "edited".into(),
                        ..msg(c, s, t)
                    },
                },
                Ev::Deleted(c, s, t) => Event::Deleted {
                    conversation: cid(c),
                    target: target(s, t),
                },
                Ev::Expiring(c, s, t) => Event::Expiring {
                    conversation: cid(c),
                    expiries: vec![(target(s, t), now_millis() + 600_000)],
                },
                Ev::Expired(c, v) => Event::Expired {
                    conversation: cid(c),
                    targets: v.iter().map(|&(s, t)| target(s, t)).collect(),
                },
                Ev::Cleared(c) => Event::Cleared(cid(c)),
                Ev::Delivered(c, v) => Event::Delivered {
                    conversation: cid(c),
                    timestamps: v.iter().map(|&t| ts(t)).collect(),
                    delivery: Delivery::Read,
                },
                Ev::Reaction {
                    conv,
                    sender,
                    t,
                    from,
                    remove,
                } => Event::Reaction {
                    conversation: cid(conv),
                    target: target(sender, t),
                    reaction: Reaction {
                        emoji: "👍".into(),
                        sender_id: SENDERS[from].into(),
                        sender: SENDERS[from].into(),
                        outgoing: from == 2,
                    },
                    remove,
                },
                Ev::Sent(i) => {
                    if self.sending.is_empty() {
                        return None;
                    }
                    let (conversation, timestamp) = self.sending.remove(i % self.sending.len());
                    Event::Message(ChatMessage {
                        conversation,
                        sender_id: "me".into(),
                        sender: "me".into(),
                        body: "sent".into(),
                        timestamp,
                        outgoing: true,
                        ..Default::default()
                    })
                }
                Ev::SendFailed(i) => {
                    if self.sending.is_empty() {
                        return None;
                    }
                    let (conversation, timestamp) = self.sending[i % self.sending.len()].clone();
                    Event::SendFailed {
                        conversation,
                        timestamp,
                    }
                }
                Ev::Found(c) => Event::Found {
                    conversation: cid(c),
                    then: AfterFinding::Open,
                },
            })
        }

        fn note_send(&mut self, command: &Option<Command>) {
            if let Some(Command::Send {
                conversation,
                timestamp,
                ..
            }) = command
            {
                self.sending.push((conversation.clone(), *timestamp));
            }
        }

        fn run(&mut self, op: &Op) {
            let update = match op {
                Op::Batch(events) => {
                    self.chat.set_window_focused(true);
                    let mut update = Update::default();
                    for e in events {
                        if let Some(event) = self.event(e) {
                            let u = self.chat.apply(event.clone());
                            self.log
                                .push(format!("  apply {event:?} -> {:?}", u.messages));
                            update = update.merge(u);
                        }
                    }
                    update
                }
                Op::Select(c) => {
                    self.chat.select(&cid(*c));
                    Update::reset()
                }
                Op::Deselect => self.chat.deselect(),
                Op::Compose => {
                    let (command, update) = self.chat.compose("hello");
                    self.note_send(&command);
                    update
                }
                Op::LoadOlder => {
                    self.chat.load_older();
                    Update::default()
                }
                Op::Delete(i, everyone) => self.chat.delete(*i, *everyone).1,
                Op::Edit(i) => {
                    self.chat.edit(*i);
                    let (command, update) = self.chat.compose("changed");
                    self.note_send(&command);
                    update
                }
                Op::ReplyTo(i) => self.chat.reply_to(*i),
                Op::Reveal(i) => self.chat.reveal(*i),
                Op::NotificationReply(c) => {
                    let (command, update) = self.chat.reply(&cid(*c), "thanks");
                    self.note_send(&command);
                    update.merge(Update {
                        conversations: true,
                        ..Update::default()
                    })
                }
                Op::Archive => match self.chat.selected.clone() {
                    Some(id) => {
                        self.chat.archive(&id, true);
                        // Unarchive right away so the chat can be selected again.
                        self.chat.archive(&id, false);
                        Update::reset()
                    }
                    None => Update::default(),
                },
            };
            self.chat.take_commands();
            self.log.push(format!("{op:?} -> {update:?}"));
            render(&mut self.model, &self.chat, update);
        }

        fn shown(&self) -> Vec<(u64, String)> {
            self.model
                .iter()
                .map(|r| (r.timestamp, r.sender_id.clone()))
                .collect()
        }

        fn truth(&self) -> Vec<(u64, String)> {
            self.chat
                .messages()
                .map(|m| (m.message.timestamp, m.message.sender_id.clone()))
                .collect()
        }
    }

    fn rel(rows: &[(u64, String)]) -> Vec<String> {
        rows.iter()
            .map(|(t, s)| format!("{s}@{}", (t.saturating_sub(base())) / 1000))
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, max_shrink_iters: 20000, failure_persistence: None, ..ProptestConfig::default() })]

        /// After every batch of events and every action, the UI's rows are the messages of
        /// `messages()`, in order: same count, same message in each row.
        #[test]
        fn ui_rows_follow_messages(ops in prop::collection::vec(op(), 1..30)) {
            let mut sim = Sim::new();
            for op in &ops {
                sim.run(op);
                let (shown, truth) = (sim.shown(), sim.truth());
                prop_assert!(
                    shown == truth,
                    "after {:?}\nUI rows:  {:?}\nmessages: {:?}\nlog:\n{}",
                    op, rel(&shown), rel(&truth), sim.log.join("\n")
                );
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 3000, max_shrink_iters: 20000, failure_persistence: None, ..ProptestConfig::default() })]

        /// Weaker: rows hold the right messages *and* show their current state (body, delivery,
        /// deletion, reactions, expiry).
        #[test]
        fn ui_rows_show_current_state(ops in prop::collection::vec(op(), 1..30)) {
            let mut sim = Sim::new();
            for op in &ops {
                sim.run(op);
                if sim.shown() != sim.truth() {
                    // Covered by `ui_rows_follow_messages`.
                    return Ok(());
                }
                let truth: Vec<Row> = sim.chat.messages().map(|m| row(&m)).collect();
                prop_assert!(
                    sim.model == truth,
                    "after {:?}\nUI rows:  {:?}\nmessages: {:?}\nlog:\n{}",
                    op, sim.model, truth, sim.log.join("\n")
                );
            }
        }
    }

    /// The realistic shape of what `ui_rows_follow_messages` finds: two messages arrive late
    /// (sent before the oldest loaded one), then the older page that has a message between
    /// them is loaded.
    #[test]
    fn late_messages_then_an_older_page() {
        let mut sim = Sim::new();
        let a = cid(0);
        sim.run(&Op::Select(0));
        // The newest page: messages at 10 and 11; older ones before 10 are still stored.
        sim.run(&Op::Batch(vec![Ev::History {
            conv: 0,
            msgs: vec![(0, 10), (2, 11)],
            older: Some(10),
        }]));
        // Two messages Alice sent earlier arrive only now (her phone was offline).
        sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 3)]));
        sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 5)]));
        // Scrolled to the top: the page before 10 comes, with what was stored between them.
        sim.run(&Op::LoadOlder);
        let update = sim.chat.apply(Event::History {
            conversation: a.clone(),
            messages: vec![
                msg(0, 1, 2),
                msg(0, 0, 3),
                msg(0, 1, 4),
                msg(0, 0, 5),
                msg(0, 1, 6),
            ],
            reactions: Vec::new(),
            older: None,
        });
        render(&mut sim.model, &sim.chat, update);
        println!(
            "update {update:?}\nUI rows:  {:?}\nmessages: {:?}",
            rel(&sim.shown()),
            rel(&sim.truth())
        );
        assert_eq!(rel(&sim.shown()), rel(&sim.truth()));
    }

    /// The same, reached by picking a search result in the older page: the UI scrolls to the
    /// result's row, which shows another message, and the result itself isn't in the list.
    #[test]
    fn search_result_in_an_older_page() {
        let mut sim = Sim::new();
        let a = cid(0);
        sim.run(&Op::Select(0));
        sim.run(&Op::Batch(vec![Ev::History {
            conv: 0,
            msgs: vec![(0, 10), (2, 11)],
            older: Some(10),
        }]));
        sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 3)]));
        sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 5)]));
        sim.chat.open_chat_search();
        sim.chat.search_chat("bob");
        sim.chat.apply(Event::SearchResults {
            conversation: a.clone(),
            query: "bob".into(),
            messages: vec![msg(0, 1, 6)],
        });
        let update = sim.chat.show_result(0);
        render(&mut sim.model, &sim.chat, update);
        let commands = sim.chat.take_commands();
        assert!(
            matches!(
                commands.as_slice(),
                [Command::LoadHistory {
                    before: Some(_),
                    ..
                }]
            ),
            "{commands:?}"
        );
        let update = sim.chat.apply(Event::History {
            conversation: a.clone(),
            messages: vec![
                msg(0, 1, 2),
                msg(0, 0, 3),
                msg(0, 1, 4),
                msg(0, 0, 5),
                msg(0, 1, 6),
            ],
            reactions: Vec::new(),
            older: None,
        });
        render(&mut sim.model, &sim.chat, update);
        let (row, _) = sim.chat.jump().expect("scrolls to the result");
        let at_row = &sim.model[row];
        println!(
            "jump row {row}: UI shows {}@{} there; result bob@6 shown at all: {}",
            at_row.sender_id,
            (at_row.timestamp - base()) / 1000,
            sim.model
                .iter()
                .any(|r| r.sender_id == "bob" && r.timestamp == ts(6))
        );
        assert_eq!(
            (at_row.sender_id.as_str(), at_row.timestamp),
            ("bob", ts(6))
        );
    }
}

#[cfg(test)]
mod helper_props {
    use super::*;
    use proptest::prelude::*;

    fn piece() -> impl Strategy<Value = String> {
        prop_oneof![
            6 => prop::sample::select(vec![
                "a", "B", "c", "x", " ", "  ", "\n", "\t", "é", "É", "🦀", "e\u{0301}", "İ", "ı", "ẞ", "ß",
                "Ω", "ω", "\u{212A}", "k", "K", "Σ", "σ", "ς", "\u{00A0}", "\u{3000}", "ǅ", "ﬃ", "ŉ",
                "Ⅻ", "ǰ", "ΐ", "@", ".", "*", "<", "#",
            ]).prop_map(str::to_string),
            1 => any::<char>().prop_map(|c| c.to_string()),
        ]
    }

    fn text() -> impl Strategy<Value = String> {
        prop::collection::vec(piece(), 0..40).prop_map(|v| v.concat())
    }

    /// Lowercase by char, with both sigmas the same (snippet compares per char).
    fn fold(c: char) -> String {
        let s: String = c.to_lowercase().collect();
        if s == "ς" { "σ".into() } else { s }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 20000, failure_persistence: None, ..ProptestConfig::default() })]

        #[test]
        fn snippet_highlights_a_match(body in text(), query in text()) {
            let (text, found) = snippet(&body, &query);
            if let Some(found) = found.clone() {
                let part = text.get(found.clone());
                prop_assert!(part.is_some_and(|p| !p.is_empty()), "{found:?} in {text:?}");
                let shown: Vec<String> = part.unwrap().chars().map(fold).collect();
                let wanted: Vec<String> = query.trim().to_lowercase().chars().map(fold).collect();
                prop_assert_eq!(shown, wanted, "{:?} {:?} -> {:?} {:?}", body, query, text, found);
            }
        }

        /// When the flattened body contains the query (char by char, ignoring case), it is found.
        #[test]
        fn snippet_finds_what_is_there(prefix in text(), needle in "[a-zA-Z]{1,5}", suffix in text()) {
            let body = format!("{prefix}{needle}{suffix}");
            let (_, found) = snippet(&body, &needle);
            let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
            let same_len = flat.to_lowercase().chars().count() == flat.chars().count();
            prop_assert!(found.is_some() || !same_len, "{body:?} / {needle:?}");
        }

        #[test]
        fn initials_are_at_most_two(title in text()) {
            let initials = initials(&title);
            prop_assert!(!initials.is_empty());
            prop_assert!(initials.chars().count() <= 2, "{title:?} -> {initials:?}");
        }

        #[test]
        fn formatting_never_panics(t in any::<u64>(), now in any::<u64>(), len in any::<usize>(), id in ".*") {
            let _ = format_size(t);
            let _ = format_remaining(t, now);
            let _ = format_list_time(t);
            let _ = format_time(t);
            let _ = format_clock(t);
            let _ = looks_like_number(&id);
            let index = avatar_index(&ConversationId(id), len);
            prop_assert!(index < len.max(1));
        }

        #[test]
        fn list_time_and_day_labels_never_panic(t in any::<u64>(), n in any::<i64>()) {
            let now = jiff::Timestamp::from_millisecond(n.rem_euclid(253_402_207_200_000 - 86_400_000))
                .unwrap()
                .to_zoned(jiff::tz::TimeZone::system());
            let _ = format_list_time_at(t, &now);
            let _ = day_label(t, &now);
        }

        #[test]
        fn timers_read_back(seconds in prop_oneof![1u32..100_000, any::<u32>()]) {
            let text = crate::format_timer(seconds);
            let (n, unit) = text.split_once(' ').unwrap();
            let n: u64 = n.parse().unwrap();
            let size = match unit.trim_end_matches('s') {
                "week" => 604_800u64,
                "day" => 86_400,
                "hour" => 3_600,
                "minute" => 60,
                "second" => 1,
                other => panic!("{other}"),
            };
            prop_assert_eq!(n * size, u64::from(seconds), "{}", text);
            prop_assert_eq!(n == 1, !unit.ends_with('s'), "{}", text);
            let _ = crate::timer_notice("You", seconds);
        }

        #[test]
        fn mentions_are_where_they_say(body in text(), names in prop::collection::vec("[a-zA-Z é🦀]{0,5}", 0..4)) {
            let people: Vec<Person> = names
                .iter()
                .enumerate()
                .map(|(i, n)| Person { service_id: format!("id{i}"), name: n.clone() })
                .collect();
            let mentions = mentions_in(&body, &people);
            let mut last_end = 0;
            for m in &mentions {
                prop_assert!(m.start < m.end && m.end <= body.len());
                prop_assert!(body.is_char_boundary(m.start) && body.is_char_boundary(m.end));
                prop_assert!(m.start >= last_end, "overlap {mentions:?}");
                last_end = m.end;
                let person = people.iter().find(|p| p.service_id == m.service_id).unwrap();
                prop_assert_eq!(&body[m.start..m.end], format!("@{}", person.name));
            }
            if let Some(query) = mention_query(&body) {
                prop_assert!(body.ends_with(query));
                prop_assert!(body[..body.len() - query.len()].ends_with('@'));
            }
        }
    }
}
