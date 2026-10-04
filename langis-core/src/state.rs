//! Toolkit-independent view model: everything a chat UI needs to know, derived from [`Event`]s.
//! Frontends render from this and translate user actions into [`Command`]s through it.

use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{Avatar, ChatMessage, Command, Conversation, ConversationId, Event, Password, QrMatrix};

/// Minimum length for a newly chosen database password.
pub const MIN_PASSWORD_LEN: usize = 8;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    Connecting,
    /// See [`Event::PasswordRequired`].
    Password { new: bool, error: Option<String> },
    Link(QrMatrix),
    Chats,
}

/// What a call to [`ChatState::apply`] changed, so UIs can update incrementally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub screen: bool,
    pub conversations: bool,
    pub messages: MessagesUpdate,
}

impl Update {
    /// Combines updates from several events applied in a row.
    pub fn merge(self, other: Self) -> Self {
        Self {
            screen: self.screen || other.screen,
            conversations: self.conversations || other.conversations,
            messages: self.messages.merge(other.messages),
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
    pub avatar: Option<&'a Arc<Avatar>>,
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
        let body = message.body.lines().next().unwrap_or_default();
        if message.outgoing {
            format!("You: {body}")
        } else {
            body.to_string()
        }
    }
}

pub struct MessageView<'a> {
    pub message: &'a ChatMessage,
    /// Show the sender's name above the message (incoming group messages, first of a run).
    pub show_sender: bool,
}

#[derive(Default)]
pub struct ChatState {
    screen: Screen,
    account: String,
    synced: bool,
    error: Option<String>,
    /// Most recently active first; see [`ChatState::sort_conversations`].
    conversations: Vec<Conversation>,
    unread: HashMap<ConversationId, u32>,
    /// Per conversation, sorted and unique by [`message_key`].
    history: HashMap<ConversationId, Vec<ChatMessage>>,
    avatars: HashMap<ConversationId, Arc<Avatar>>,
    /// Conversations whose stored history has arrived.
    loaded: HashSet<ConversationId>,
    selected: Option<ConversationId>,
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
            Event::Synced => self.synced = true,
            Event::Reconnecting => self.synced = false,
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
            Event::Avatar { conversation, avatar } => {
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
            Event::Message(message) => {
                let id = message.conversation.clone();
                let outgoing = message.outgoing;
                let history = self.history.entry(id.clone()).or_default();
                let Some(pos) = insert(history, message) else {
                    return update;
                };
                let appended = pos + 1 == history.len();
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
        self.selected = Some(id.clone());
        (!self.loaded.contains(id)).then(|| Command::LoadHistory(id.clone()))
    }

    /// Validates the password form. On success returns the unlock command and shows the
    /// connecting screen; otherwise puts the reason on the password screen.
    pub fn unlock(&mut self, password: &str, confirmation: &str) -> Option<Command> {
        let Screen::Password { new, error } = &mut self.screen else { return None };
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
        self.synced && self.selected.is_some() && !text.trim().is_empty()
    }

    /// Turns composer text into a send command for the selected conversation.
    pub fn compose(&self, text: &str) -> Option<Command> {
        if !self.can_compose(text) {
            return None;
        }
        let conversation = self.selected.clone()?;
        Some(Command::Send {
            conversation,
            body: text.trim().to_string(),
        })
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

    pub fn synced(&self) -> bool {
        self.synced
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn conversations(&self) -> impl Iterator<Item = ConversationView<'_>> {
        self.conversations.iter().map(|c| ConversationView {
            conversation: c,
            avatar: self.avatars.get(&c.id),
            last_message: self.last_of(&c.id),
            unread: self.unread.get(&c.id).copied().unwrap_or_default(),
            selected: self.selected.as_ref() == Some(&c.id),
        })
    }

    pub fn selected(&self) -> Option<&Conversation> {
        self.selected_index().map(|i| &self.conversations[i])
    }

    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        self.conversations.iter().position(|c| &c.id == id)
    }

    /// Messages of the selected conversation, oldest first.
    pub fn messages(&self) -> impl ExactSizeIterator<Item = MessageView<'_>> {
        let group = self.selected().is_some_and(|c| c.is_group);
        let history = self
            .selected
            .as_ref()
            .and_then(|id| self.history.get(id))
            .map_or(&[][..], Vec::as_slice);
        history
            .iter()
            .enumerate()
            .map(move |(i, message)| MessageView {
                message,
                show_sender: group
                    && !message.outgoing
                    && (i == 0 || history[i - 1].sender_id != message.sender_id),
            })
    }

    /// The `n` most recent messages of the selected conversation, oldest first, for
    /// [`MessagesUpdate::Appended`].
    pub fn last_messages(&self, n: usize) -> impl Iterator<Item = MessageView<'_>> {
        let messages = self.messages();
        let skip = messages.len().saturating_sub(n);
        messages.skip(skip)
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

/// "14:05" for today, "Mar  3, 14:05" otherwise, in local time.
pub fn format_time(timestamp: u64) -> String {
    let Ok(time) = jiff::Timestamp::from_millisecond(timestamp as i64) else {
        return String::new();
    };
    let time = time.to_zoned(jiff::tz::TimeZone::system());
    let format = if time.date() == jiff::Zoned::now().date() { "%H:%M" } else { "%b %e, %H:%M" };
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
        state.apply(Event::History { conversation: id.clone(), messages: Vec::new() });
        assert!(state.select(&id).is_none());
        assert!(state.select(&ConversationId("nope".into())).is_none());
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
        // A partial update never drops conversations, and renames apply in place.
        let mut renamed = conversation("a", false);
        renamed.title = "Alice".into();
        state.apply(Event::Conversations(vec![
            conversation("n", false),
            renamed,
        ]));
        let titles: Vec<_> = state
            .conversations()
            .map(|c| c.conversation.title.as_str())
            .collect();
        assert_eq!(titles, ["g", "Alice", "n"]);
    }

    #[test]
    fn inactive_conversations_list_names_before_numbers() {
        let mut state = ChatState::default();
        let titled = |id: &str, title: &str| Conversation { title: title.into(), ..conversation(id, false) };
        state.apply(Event::Conversations(vec![
            titled("1", "+49 151 1234567"),
            titled("2", "bob"),
            titled("3", "Alice"),
        ]));
        let titles: Vec<_> = state.conversations().map(|c| c.conversation.title.as_str()).collect();
        assert_eq!(titles, ["Alice", "bob", "+49 151 1234567"]);
    }

    #[test]
    fn appends_in_one_batch_are_all_reported() {
        let mut state = state();
        state.select(&ConversationId("a".into()));
        let update = [1, 2, 3].into_iter().fold(Update::default(), |update, ts| {
            update.merge(state.apply(Event::Message(message("a", "Alice", ts, false))))
        });
        assert_eq!(update.messages, MessagesUpdate::Appended(3));
        let stamps: Vec<_> = state.last_messages(2).map(|m| m.message.timestamp).collect();
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
        assert!(state.unlock("whatever", "whatever").is_none(), "no password requested yet");
        state.apply(Event::PasswordRequired { new: true, error: None });
        assert!(state.unlock("short", "short").is_none());
        assert!(matches!(state.screen(), Screen::Password { error: Some(e), .. } if e.contains("8")));
        assert!(state.unlock("long enough", "long enougj").is_none());
        let command = state.unlock("long enough", "long enough");
        assert!(matches!(command, Some(Command::Unlock(p)) if p.expose() == "long enough"));
        assert_eq!(state.screen(), &Screen::Connecting);
    }

    #[test]
    fn existing_password_needs_no_confirmation() {
        let mut state = ChatState::default();
        state.apply(Event::PasswordRequired { new: false, error: Some("Wrong password.".into()) });
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
