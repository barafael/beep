//! The open chat's messages: loading them, the rows, jumping to one, deleting.

use super::*;

impl ChatState {
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

    /// Scrolls to the pending search result if it is loaded, or asks for the page before.
    pub(super) fn continue_jump(&mut self) {
        let Some(target) = self.pending_jump.clone() else {
            return;
        };
        let id = self.selected.clone();
        let found = id
            .as_ref()
            .and_then(|id| self.history.get(id))
            .and_then(|history| find(history, ref_key(&target)));
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

    /// Scrolls the chat just opened to its first unread message, once its newest page is
    /// there. Not when the new ones are few: they show at the end anyway, which keeps following
    /// what comes next.
    pub(super) fn continue_unread_jump(&mut self) {
        /// At most this many new messages show at the end without a jump.
        const FEW: usize = 4;
        let Some(id) = self.selected.as_ref() else {
            return;
        };
        if !self.jump_to_unread || !self.is_loaded(id) {
            return;
        }
        self.jump_to_unread = false;
        let history = self.selected_history();
        let first = self
            .read_mark
            .filter(|&mark| mark > 0)
            .and_then(|mark| first_unread(history, mark));
        if let Some(row) = first
            && history.len() - row > FEW
        {
            let count = self.jump.map_or(1, |(_, count)| count + 1);
            self.jump = Some((row, count));
        }
    }

    /// The row the UI should scroll to, with a number that changes with each request, so the
    /// UI scrolls once per request.
    pub fn jump(&self) -> Option<(usize, u64)> {
        self.jump
    }

    /// Deletes the `index`th message of [`ChatState::messages`] here, or for everyone.
    pub fn delete(&mut self, index: usize, for_everyone: bool) -> (Option<Command>, Update) {
        let Some(message) = self.selected_history().get(index) else {
            return (None, Update::default());
        };
        if message.notice || for_everyone && !self.can_edit(message) {
            return (None, Update::default());
        }
        let target = message.reference();
        let conversation = message.conversation.clone();
        if message.unsent.is_some() {
            // Never went out (or is still going): nothing to tell anyone, only not to show it
            // as failed again. A send still under way finishes regardless, and the sent
            // message shows up.
            let timestamp = message.timestamp;
            self.pending_sends
                .remove(&(conversation.clone(), timestamp));
            let history = self.history.get_mut(&conversation).expect("shown");
            history.retain(|m| !is_unsent_at(m, timestamp));
            self.forget_changed_rows();
            let command = Command::DropUnsent {
                conversation,
                timestamp,
            };
            return (Some(command), Update::reset());
        }
        if self.editing.as_ref() == Some(&target) {
            self.editing = None;
        }
        let history = self.history.get_mut(&conversation).expect("shown");
        if let Some(at) = find(history, ref_key(&target)) {
            if for_everyone {
                tombstone(&mut history[at]);
            } else {
                history.remove(at);
            }
        }
        let command = Command::Delete {
            conversation,
            target,
            for_everyone,
        };
        self.forget_changed_rows();
        let update = Update::reset();
        (Some(command), update)
    }

    /// Shows the spoilers of the `index`th message of [`ChatState::messages`].
    pub fn reveal(&mut self, index: usize) -> Update {
        let reference = self
            .selected_history()
            .get(index)
            .map(ChatMessage::reference);
        if let Some(reference) = reference {
            self.revealed.insert(ref_key(&reference));
            self.changed_rows.get_mut().note([reference]);
        }
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// Whether the open chat shows messages that disappear, whose time left is counted down.
    pub fn shows_expiring(&self) -> bool {
        self.selected_history()
            .iter()
            .any(|m| m.expires_at.is_some() && !m.deleted)
    }

    /// Whether the open chat has older messages that aren't being fetched already.
    pub fn can_load_older(&self) -> bool {
        self.selected.as_ref().is_some_and(|id| {
            matches!(self.older.get(id), Some(Some(_))) && !self.loading_older.contains(id)
        })
    }

    /// Messages of the selected conversation, oldest first.
    pub fn messages(&self) -> impl ExactSizeIterator<Item = MessageView<'_>> {
        let rows = Rows::new(self);
        let all = 0..rows.history.len();
        Messages::new(rows, all)
    }

    /// The `index`th row of [`ChatState::messages`], the same as it would be there, without
    /// working out the rows before it.
    pub fn message_view(&self, index: usize) -> Option<MessageView<'_>> {
        let rows = Rows::new(self);
        (index < rows.history.len()).then(|| rows.view(index))
    }

    /// The open chat's messages that changed in place since the last call, for the UI to
    /// redraw just their rows and those beside them (whose runs end and start with theirs);
    /// see [`ChatState::row_of`]. `None` means every row: when too many changed, when a change
    /// isn't one message's (a sender's picture), and when none were noted, so that an
    /// [`Update::rows`] made without naming its messages still redraws them all.
    ///
    /// Every change to what a row shows notes its message here, also those asked for through
    /// the UI (a download, a send tried again): one that didn't would go unseen when it comes
    /// in the same redraw as one that did.
    ///
    /// To be called at every redraw, so that what was noted doesn't pile up.
    pub fn take_changed_rows(&self) -> Option<Vec<MessageRef>> {
        let changed = self.changed_rows.take();
        (!changed.all && !changed.messages.is_empty()).then_some(changed.messages)
    }

    /// Which row of [`ChatState::messages`] a message of the open chat is; `None` if it isn't
    /// loaded.
    pub fn row_of(&self, target: &MessageRef) -> Option<usize> {
        find(self.selected_history(), ref_key(target))
    }

    /// How many changed messages are noted, and whether all are; without taking them.
    #[cfg(test)]
    pub(super) fn changed_rows_noted(&self) -> (usize, bool) {
        let changed = self.changed_rows.take();
        let noted = (changed.messages.len(), changed.all);
        self.changed_rows.set(changed);
        noted
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

    pub(super) fn selected_history(&self) -> &[ChatMessage] {
        self.selected
            .as_ref()
            .and_then(|id| self.history.get(id))
            .map_or(&[][..], Vec::as_slice)
    }

    /// The `n` most recent messages of the selected conversation, oldest first, for
    /// [`MessagesUpdate::Appended`].
    pub fn last_messages(&self, n: usize) -> impl Iterator<Item = MessageView<'_>> {
        let rows = Rows::new(self);
        let len = rows.history.len();
        Messages::new(rows, len.saturating_sub(n)..len)
    }

    /// The fetched full text of a long message.
    pub(super) fn full_text(&self, message: &ChatMessage) -> Option<&str> {
        let long = message
            .attachments
            .iter()
            .find(|a| a.kind == AttachmentKind::LongText)?;
        match self.attachments.get(&long.id)? {
            AttachmentState::Ready(AttachmentContent::Text(text)) => Some(text),
            _ => None,
        }
    }
}

/// See [`ChatState::can_edit`].
pub(super) fn editable(message: &ChatMessage, now: u64) -> bool {
    message.outgoing
        && !message.deleted
        && !message.notice
        && message.unsent.is_none()
        && now.saturating_sub(message.timestamp) < DELETE_FOR_EVERYONE_MS
}

/// What is left of a message deleted for everyone.
pub(super) fn tombstone(message: &mut ChatMessage) {
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
pub(super) fn is_unsent_at(message: &ChatMessage, timestamp: u64) -> bool {
    message.unsent.is_some() && message.timestamp == timestamp
}

pub(super) fn message_key(message: &ChatMessage) -> (u64, &str) {
    (message.timestamp, &message.sender_id)
}

/// A [`MessageRef`] as a [`message_key`].
pub(super) fn ref_key(target: &MessageRef) -> (u64, &str) {
    (target.timestamp, &target.author_id)
}

/// Where the message with this [`message_key`] is in a history (sorted and unique by it).
pub(super) fn find(history: &[ChatMessage], key: (u64, &str)) -> Option<usize> {
    history.binary_search_by(|m| message_key(m).cmp(&key)).ok()
}

/// Where the messages sent at `timestamp` are in a history: together, as it is sorted by time
/// first.
pub(super) fn at_time(history: &[ChatMessage], timestamp: u64) -> Range<usize> {
    let start = history.partition_point(|m| m.timestamp < timestamp);
    let end = start + history[start..].partition_point(|m| m.timestamp == timestamp);
    start..end
}

/// Where the last of ours shown before it went out, sent at `timestamp`, is.
pub(super) fn unsent_at(history: &[ChatMessage], timestamp: u64) -> Option<usize> {
    let sent = at_time(history, timestamp);
    let last = history[sent.clone()]
        .iter()
        .rposition(|m| m.unsent.is_some())?;
    Some(sent.start + last)
}

/// Incoming messages after `last_read`.
pub(super) fn unread_after(
    history: &[ChatMessage],
    last_read: u64,
) -> impl Iterator<Item = &ChatMessage> {
    history
        .iter()
        .filter(move |m| !m.outgoing && !m.notice && m.timestamp > last_read)
}

/// Inserts a page of messages in order, each replacing the one already there with its
/// [`message_key`] (the last of several in the page wins); returns how many were new and how
/// many replaced one. An older page goes before the history at once, anything else is merged
/// in one pass.
pub(super) fn upsert_all(
    history: &mut Vec<ChatMessage>,
    mut page: Vec<ChatMessage>,
) -> (usize, usize) {
    let sorted = page
        .windows(2)
        .all(|w| message_key(&w[0]) < message_key(&w[1]));
    let older = page
        .last()
        .zip(history.first())
        .is_none_or(|(last, first)| message_key(last) < message_key(first));
    if sorted && older {
        let added = page.len();
        history.splice(0..0, page);
        return (added, 0);
    }
    // Of several with the same key, the last upserted is what stays: reversed, it comes first
    // among them after a stable sort, and dedup keeps the first.
    page.reverse();
    page.sort_by(|a, b| message_key(a).cmp(&message_key(b)));
    let before = page.len();
    page.dedup_by(|a, b| message_key(a) == message_key(b));
    let mut replaced = before - page.len();
    let had = history.len();
    let mut merged = Vec::with_capacity(had + page.len());
    let mut old = std::mem::take(history).into_iter().peekable();
    for message in page {
        let key = message_key(&message);
        while let Some(m) = old.next_if(|m| message_key(m) < key) {
            merged.push(m);
        }
        if old.next_if(|m| message_key(m) == key).is_some() {
            replaced += 1;
        }
        merged.push(message);
    }
    merged.extend(old);
    *history = merged;
    (history.len() - had, replaced)
}

/// Inserts in order unless already present; returns the new position.
pub(super) fn insert(history: &mut Vec<ChatMessage>, message: ChatMessage) -> Option<usize> {
    let pos = history
        .binary_search_by(|m| message_key(m).cmp(&message_key(&message)))
        .err()?;
    history.insert(pos, message);
    Some(pos)
}
