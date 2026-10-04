//! Toolkit-independent view model: everything a chat UI needs to know, derived from [`Event`]s.
//! Frontends render from this and translate user actions into [`Command`]s through it.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::settings::NotificationContent;
use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, ChatMessage, Command,
    Conversation, ConversationId, Event, HistoryTransfer, Password, Picture, QrMatrix, Quote,
    apply_reaction,
};

/// Minimum length for a newly chosen database password.
pub const MIN_PASSWORD_LEN: usize = 8;

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
}

impl Update {
    /// Combines updates from several events applied in a row.
    pub fn merge(self, other: Self) -> Self {
        Self {
            screen: self.screen || other.screen,
            conversations: self.conversations || other.conversations,
            messages: self.messages.merge(other.messages),
            rows: self.rows || other.rows,
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
            (Unchanged, Unchanged) => Unchanged,
        }
    }
}

pub struct ConversationView<'a> {
    pub conversation: &'a Conversation,
    /// Shared, so UIs can cache what they convert it into and tell when it changed.
    pub avatar: Option<&'a Arc<Picture>>,
    pub last_message: Option<&'a ChatMessage>,
    pub unread: u32,
    pub selected: bool,
}

impl ConversationView<'_> {
    /// First line of the newest message, prefixed with "You: " if we sent it.
    pub fn preview(&self) -> String {
        let Some(message) = self.last_message else {
            return String::new();
        };
        let body = match message.body.lines().next() {
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
    /// The message text: its full text if it is long and that has been fetched.
    pub body: &'a str,
    /// "Today", "Yesterday", "Monday", "Oct 2": set on the first message of each day.
    pub day: Option<String>,
    /// Messages by the same person within a few minutes form a run, drawn close together.
    pub first_of_run: bool,
    pub last_of_run: bool,
    /// The sender's picture, if known (for incoming group messages).
    pub sender_avatar: Option<&'a Arc<Picture>>,
}

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
    /// Conversations whose stored history has arrived.
    loaded: HashSet<ConversationId>,
    selected: Option<ConversationId>,
    notifications: Notifications,
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
                // A reaction to a message not loaded yet is folded in when its history loads.
                if let Some(history) = self.history.get_mut(&conversation)
                    && apply_reaction(history, &target, reaction, remove)
                    && self.selected.as_ref() == Some(&conversation)
                {
                    update.rows = true;
                }
            }
            Event::Avatar {
                conversation,
                avatar,
            } => {
                self.avatars.insert(conversation, avatar);
                update.conversations = true;
            }
            Event::History {
                conversation,
                messages,
            } => {
                self.loaded.insert(conversation.clone());
                let history = self.history.entry(conversation.clone()).or_default();
                let mut added = false;
                for message in messages {
                    added |= insert(history, message).is_some();
                }
                if added {
                    self.sort_conversations();
                    update.conversations = true;
                    if self.selected.as_ref() == Some(&conversation) {
                        update.messages = MessagesUpdate::Reset;
                    }
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
            Event::Message(message) => {
                let id = message.conversation.clone();
                let outgoing = message.outgoing;
                let history = self.history.entry(id.clone()).or_default();
                let Some(pos) = insert(history, message) else {
                    return update;
                };
                let appended = pos + 1 == history.len();
                if !outgoing {
                    let message = &history[pos];
                    // A photo or sticker alone says what it is.
                    let body = match (message.body.is_empty(), message.attachments.first()) {
                        (true, Some(attachment)) => describe_kind(attachment.kind).to_string(),
                        _ => message.body.clone(),
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
                } else if !outgoing {
                    *self.unread.entry(id).or_default() += 1;
                }
                self.sort_conversations();
                update.conversations = true;
            }
            Event::Error(error) => self.error = Some(error),
        }
        update
    }

    /// Selects a conversation; until its stored history has arrived, returns the command
    /// that fetches it (afterwards live events keep it current). Asking again after a failed
    /// load retries it; a duplicate answer is harmless because inserts are deduplicated.
    pub fn select(&mut self, id: &ConversationId) -> Option<Command> {
        if !self.conversations.iter().any(|c| &c.id == id) {
            return None;
        }
        self.unread.remove(id);
        if self.selected.as_ref() != Some(id) {
            self.replying = None;
            self.staged.clear();
        }
        self.selected = Some(id.clone());
        // Picking a search result ends the search; the chosen chat stays on the list.
        self.filter.clear();
        self.notifications
            .pending
            .push(Notice::Withdraw(id.clone()));
        (!self.loaded.contains(id)).then(|| Command::LoadHistory(id.clone()))
    }

    /// Answers a conversation that needn't be the selected one, e.g. from a notification.
    /// Counts as reading it.
    pub fn reply(&mut self, id: &ConversationId, text: &str) -> Option<Command> {
        let body = text.trim();
        if body.is_empty() || self.conversation(id).is_none() {
            return None;
        }
        self.unread.remove(id);
        self.notifications
            .pending
            .push(Notice::Withdraw(id.clone()));
        Some(Command::Send {
            files: Vec::new(),
            conversation: id.clone(),
            body: body.to_string(),
            quote: None,
        })
    }

    pub fn set_notifications(&mut self, enabled: bool, content: NotificationContent) {
        self.notifications.enabled = enabled;
        self.notifications.content = content;
    }

    /// Whether the window has the user's attention; messages for the open conversation of a
    /// focused window don't notify.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.notifications.window_focused = focused;
        if let (true, Some(id)) = (focused, &self.selected) {
            self.notifications
                .pending
                .push(Notice::Withdraw(id.clone()));
        }
    }

    /// Notifications to show or withdraw since the last call.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notifications.pending)
    }

    pub fn conversation(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| &c.id == id)
    }

    fn notify_message(&mut self, id: &ConversationId, sender: String, body: String) {
        let n = &mut self.notifications;
        if !n.enabled || (n.window_focused && self.selected.as_ref() == Some(id)) {
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
            .filter(|c| c.is_group);
        let name = match group {
            Some(group) => format!("{sender} in {}", group.title),
            None => sender,
        };
        let (title, body) = match n.content {
            NotificationContent::NameAndMessage => (name, body),
            NotificationContent::NameOnly => (name, "New message".into()),
            NotificationContent::Nothing => ("langis".into(), "New message".into()),
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
            title: "langis".into(),
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

    /// Whether [`ChatState::compose`] would send `text`.
    pub fn can_compose(&self, text: &str) -> bool {
        self.synced
            && self.selected.is_some()
            && (!text.trim().is_empty() || !self.staged.is_empty())
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
        self.selected = None;
        self.replying = None;
        self.staged.clear();
        Update {
            conversations: true,
            messages: MessagesUpdate::Reset,
            ..Update::default()
        }
    }

    /// Turns composer text into a send command for the selected conversation.
    /// Sends `text` to the selected conversation, as a reply if one is being written.
    pub fn compose(&mut self, text: &str) -> Option<Command> {
        if !self.can_compose(text) {
            return None;
        }
        let conversation = self.selected.clone()?;
        Some(Command::Send {
            conversation,
            body: text.trim().to_string(),
            quote: self.replying.take(),
            files: std::mem::take(&mut self.staged),
        })
    }

    /// Reacts to the `index`th message of [`ChatState::messages`] with `emoji`; reacting with
    /// the emoji we already put there takes it back.
    pub fn react(&self, index: usize, emoji: &str) -> Option<Command> {
        let conversation = self.selected.clone()?;
        let message = self.messages().nth(index)?.message;
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
        let quote = self.messages().nth(index).map(|view| Quote {
            author: view.message.sender.clone(),
            text: view.body.lines().next().unwrap_or_default().to_owned(),
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
    /// on its way. After a failure, asking again retries.
    pub fn fetch_attachment(&mut self, id: &AttachmentId) -> Option<Command> {
        if matches!(
            self.attachments.get(id),
            Some(AttachmentState::Loading | AttachmentState::Ready(_))
        ) {
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
        self.history
            .get(self.selected.as_ref()?)?
            .iter()
            .flat_map(|m| &m.attachments)
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

    /// The chat list: groups and conversations that have messages, most recently active
    /// first. Synced contacts never written to stay out of it.
    pub fn conversations(&self) -> impl Iterator<Item = ConversationView<'_>> {
        self.listed().map(|c| ConversationView {
            conversation: c,
            avatar: self.avatars.get(&c.id),
            last_message: self.last_of(&c.id),
            unread: self.unread.get(&c.id).copied().unwrap_or_default(),
            selected: self.selected.as_ref() == Some(&c.id),
        })
    }

    pub fn selected(&self) -> Option<&Conversation> {
        let id = self.selected.as_ref()?;
        self.conversations.iter().find(|c| &c.id == id)
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
                c.is_group || self.last_of(&c.id).is_some() || self.selected.as_ref() == Some(&c.id)
            } else {
                c.title.to_lowercase().contains(&filter)
            }
        })
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
        let group = self.selected().is_some_and(|c| c.is_group);
        let history = self
            .selected
            .as_ref()
            .and_then(|id| self.history.get(id))
            .map_or(&[][..], Vec::as_slice);
        let now = jiff::Zoned::now();
        let same_run = |a: &ChatMessage, b: &ChatMessage| {
            a.sender_id == b.sender_id
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
                body: self.full_text(message).unwrap_or(&message.body),
                day: new_day.then(|| day_label(message.timestamp, &now)),
                first_of_run,
                last_of_run: next.is_none_or(|n| !same_run(message, n)),
                sender_avatar: if group && !message.outgoing {
                    self.avatars
                        .get(&ConversationId::contact(&message.sender_id))
                } else {
                    None
                },
            }
        })
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

    /// Newest activity first; conversations without messages alphabetically after those,
    /// names before bare phone numbers.
    fn sort_conversations(&mut self) {
        let mut conversations = std::mem::take(&mut self.conversations);
        conversations.sort_by_cached_key(|c| {
            (
                Reverse(self.last_of(&c.id).map(|m| m.timestamp)),
                !c.title.starts_with(char::is_alphabetic),
                c.title.to_lowercase(),
            )
        });
        self.conversations = conversations;
    }
}

/// Signal identifies a message by its author and sent timestamp.
fn message_key(message: &ChatMessage) -> (u64, &str) {
    (message.timestamp, &message.sender_id)
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

    fn conversation(id: &str, is_group: bool) -> Conversation {
        Conversation {
            id: ConversationId(id.into()),
            title: id.into(),
            is_group,
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
            [(None, "langis", "3 new messages in 2 chats")]
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
            [(Some("a"), "langis", "New message")]
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
        let reply = state.reply(&a, " thanks ");
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
        assert!(state.reply(&a, "  ").is_none());
        assert!(state.reply(&ConversationId("nope".into()), "hi").is_none());
        state.select(&a);
        assert_eq!(state.take_notices(), [Notice::Withdraw(a)]);
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
        assert!(matches!(state.select(&id), Some(Command::LoadHistory(i)) if i == id));
        assert_eq!(
            state.conversations().find(|c| c.selected).unwrap().unread,
            0
        );
        // Not loaded yet (in flight, or the load failed): asking again retries.
        assert!(state.select(&id).is_some());
        state.apply(Event::History {
            conversation: id.clone(),
            messages: Vec::new(),
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
        assert!(matches!(state.select(&id), Some(Command::LoadHistory(i)) if i == id));
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
        let sent = state.compose("sure");
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
    fn mentions_become_names_counting_utf16() {
        let body = "Hi \u{FFFC}, see you";
        assert_eq!(
            crate::resolve_mentions(body, &[(3, 1, "Alice".into())]),
            "Hi @Alice, see you"
        );
        // An emoji before the mention takes two UTF-16 units.
        let body = "🎉 \u{FFFC}!";
        assert_eq!(
            crate::resolve_mentions(body, &[(3, 1, "Bob".into())]),
            "🎉 @Bob!"
        );
        assert_eq!(crate::resolve_mentions("plain", &[]), "plain");
        assert_eq!(crate::resolve_mentions("hi \u{FFFC}", &[]), "hi @…");
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
        let sent = state.compose("");
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
        });
        assert_eq!(state.messages().count(), 1);
        // Same timestamp from a different author is a different message.
        state.apply(Event::History {
            conversation: id,
            messages: vec![message("a", "Bob", 5, false)],
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
        assert!(state.compose("hi").is_none());
        state.select(&ConversationId("a".into()));
        assert!(state.compose("   ").is_none());
        assert!(matches!(state.compose(" hi "), Some(Command::Send { body, .. }) if body == "hi"));
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
}
