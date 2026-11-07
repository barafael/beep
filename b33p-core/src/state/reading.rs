//! Reading: what was seen, telling senders and other devices so, and notifications.

use super::*;

impl ChatState {
    /// The selected conversation is in front of the user: its messages count as read. Not
    /// before its stored history is here, though: only part of what is unread is known until
    /// then, and the rest would be passed over for good. The history's arrival marks it.
    pub(super) fn mark_seen(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.is_loaded(&id) {
            if let Some(command) = self.mark_read(id) {
                self.queued.push(command);
            }
        } else {
            self.unread.remove(&id);
            self.opened_unloaded.insert(id);
        }
    }

    /// [`ChatState::mark_seen`] once the events being applied are: messages that come in a
    /// batch (catching up) are read together, with one receipt.
    pub(super) fn mark_seen_later(&mut self) {
        match &self.selected {
            Some(id) if self.is_loaded(id) => self.seen_pending = Some(id.clone()),
            _ => self.mark_seen(),
        }
    }

    /// Reads what [`ChatState::mark_seen_later`] left for later.
    pub(super) fn read_seen(&mut self) {
        if let Some(id) = self.seen_pending.take()
            && let Some(command) = self.mark_read(id)
        {
            self.queued.push(command);
        }
    }

    /// Counts a conversation's messages as read; returns the [`Command::MarkRead`] for those
    /// that weren't.
    pub(super) fn mark_read(&mut self, id: ConversationId) -> Option<Command> {
        self.unread.remove(&id);
        let at = *self.conversation_at.get(&id)?;
        let settings = &mut self.conversations[at].settings;
        let history = self.history.get(&id).map_or(&[][..], Vec::as_slice);
        // The history is sorted by time: what came after `last_read` is at its end, and those
        // that came late, before it, are looked up.
        let after = history.partition_point(|m| m.timestamp <= settings.last_read);
        let mut late: Vec<usize> = self
            .late
            .remove(&id)
            .unwrap_or_default()
            .iter()
            .filter_map(|target| find(history, ref_key(target)))
            .filter(|&i| i < after)
            .collect();
        late.sort_unstable();
        late.dedup();
        let unread: Vec<&ChatMessage> = late
            .into_iter()
            .map(|i| &history[i])
            .chain(&history[after..])
            .filter(|m| !m.outgoing && !m.notice)
            .filter(|m| !self.read.contains(message_key(m)))
            .collect();
        // Edits of messages read before; their disappearing timers run already.
        let edits: Vec<MessageRef> = self
            .unread_edits
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .filter(|target| find(history, ref_key(target)).is_some())
            .filter(|target| !unread.iter().any(|m| m.reference() == *target))
            .collect();
        if unread.is_empty() && edits.is_empty() && !settings.marked_unread {
            return None;
        }
        settings.marked_unread = false;
        // Senders choose their messages' dates: one from the future mustn't make everything
        // until then count as read.
        if let Some(newest) = unread.iter().map(|m| m.timestamp).max() {
            settings.last_read = settings.last_read.max(newest.min(self.clock.now()));
        }
        for m in &unread {
            self.read.insert(message_key(m));
        }
        let expiring = unread
            .iter()
            .filter_map(|m| Some((m.reference(), m.expire_timer?)))
            .collect();
        let mut messages: Vec<MessageRef> = unread.iter().map(|m| m.reference()).collect();
        for edit in edits {
            if !messages.contains(&edit) {
                messages.push(edit);
            }
        }
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

    /// Notifies about an incoming message, as the app's settings and its chat's say. `for_me`:
    /// it mentions us or replies to one of ours (see [`addresses`]).
    pub(super) fn notify_message(
        &mut self,
        id: &ConversationId,
        sender: String,
        body: String,
        for_me: bool,
    ) {
        let conversation = self
            .conversation_at
            .get(id)
            .map(|&at| &self.conversations[at]);
        let muted = conversation.is_some_and(|c| c.settings.muted(self.now()));
        let chat = conversation.map(|c| c.settings.notify).unwrap_or_default();
        let n = &mut self.notifications;
        if !n.enabled || muted || (n.window_focused && self.selected.as_ref() == Some(id)) {
            return;
        }
        if chat.mentions_only && !for_me {
            return;
        }
        if !self.synced {
            *n.caught_up.entry(id.clone()).or_default() += 1;
            return;
        }
        let group = conversation.filter(|c| c.is_group());
        let name = match group {
            Some(group) => format!("{sender} in {}", group.title),
            None => sender,
        };
        // The chat's setting only ever hides more than the app's.
        let content = match n.content {
            NotificationContent::NameAndMessage if chat.hide_text => NotificationContent::NameOnly,
            content => content,
        };
        let (title, body) = match content {
            NotificationContent::NameAndMessage => (name, body),
            NotificationContent::NameOnly => (name, "New message".into()),
            NotificationContent::Nothing => ("b33p".into(), "New message".into()),
        };
        n.pending.push(Notice::Show {
            conversation: Some(id.clone()),
            title,
            body,
            silent: chat.silent,
        });
    }

    pub(super) fn announce_caught_up(&mut self) {
        // Quiet if every chat in it is.
        let silent = self.notifications.caught_up.keys().all(|id| {
            self.conversation(id)
                .is_some_and(|c| c.settings.notify.silent)
        });
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
            silent,
        });
    }
}

/// Whether an incoming message is for us in particular: it mentions us (`own`, our service ID),
/// or replies to one of our messages. The quote names its author; one that doesn't is looked
/// for in `history`, its conversation's.
pub(super) fn addresses(message: &ChatMessage, own: &str, history: &[ChatMessage]) -> bool {
    let mentioned = !own.is_empty() && message.mentions.iter().any(|m| m.service_id == own);
    let replied = message.quote.as_ref().is_some_and(|quote| {
        if quote.author_id.is_empty() {
            quote.timestamp != 0
                && history
                    .iter()
                    .any(|m| m.outgoing && m.timestamp == quote.timestamp)
        } else {
            quote.author_id == own
        }
    });
    mentioned || replied
}
