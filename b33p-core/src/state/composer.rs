//! Writing: the draft, replies, edits, mentions, files and stickers, and sending.

use super::*;

impl ChatState {
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
            timestamp: self.send_timestamp(),
            body,
            quote: None,
            styles,
            mentions: Vec::new(),
        };
        let update = self.show_sending(&command);
        (Some(command), update)
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

    /// The selected chat's draft, as left there; empty if none.
    pub fn draft(&self) -> &str {
        self.selected
            .as_ref()
            .and_then(|id| self.drafts.get(id))
            .map_or("", |(text, _)| text)
    }

    /// Before leaving the selected chat: its draft keeps whom it mentions.
    pub(super) fn keep_draft_mentions(&mut self) {
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
    pub(super) fn keep_draft(&mut self, text: &str) {
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
            timestamp: self.send_timestamp(),
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

    /// The time to send a new message at: now, or just after the last one sent, since Signal
    /// knows messages by sender and timestamp and two sends can fall in one millisecond.
    fn send_timestamp(&mut self) -> u64 {
        self.last_send = self.now().max(self.last_send + 1);
        self.last_send
    }

    /// Shows the message `send` sends, as sending, until it is sent or fails.
    pub(super) fn show_sending(&mut self, send: &Command) -> Update {
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
        self.pending_sends
            .insert((conversation.clone(), *timestamp), send.clone());
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
        let now = self.now();
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
        let view = self.message_view(index)?;
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
        editable(message, self.now())
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    /// The message whose text the composer replaces, if any.
    pub fn editing(&self) -> Option<&ChatMessage> {
        let target = self.editing.as_ref()?;
        let history = self.history.get(self.selected.as_ref()?)?;
        history.get(find(history, ref_key(target))?)
    }

    /// Starts editing our newest message in the selected chat, if it can still be edited;
    /// returns its text for the composer.
    pub fn edit_last(&mut self) -> Option<String> {
        let index = self
            .selected_history()
            .iter()
            .rposition(|m| m.outgoing && !m.notice)?;
        self.edit(index)
    }

    /// Reacts to the `index`th message of [`ChatState::messages`] with `emoji`; reacting with
    /// the emoji we already put there takes it back.
    /// Sends the `index`th message of [`ChatState::messages`] again, if it failed to go out.
    pub fn retry_send(&mut self, index: usize) -> Option<Command> {
        let conversation = self.selected()?;
        if !self.can_write_to(conversation) {
            return None;
        }
        let conversation = conversation.id.clone();
        let message = self.selected_history().get(index)?;
        if !(message.outgoing && message.unsent == Some(Unsent::Failed)) {
            return None;
        }
        let timestamp = message.timestamp;
        let command = self
            .pending_sends
            .get(&(conversation.clone(), timestamp))?
            .clone();
        let history = self.history.get_mut(&conversation)?;
        let sent = at_time(history, timestamp);
        let changed = self.changed_rows.get_mut();
        for shown in history[sent].iter_mut().filter(|m| m.unsent.is_some()) {
            shown.unsent = Some(Unsent::Sending);
            changed.note([shown.reference()]);
        }
        Some(command)
    }

    pub fn react(&self, index: usize, emoji: &str) -> Option<Command> {
        let conversation = self.selected()?;
        if !self.can_write_to(conversation) {
            return None;
        }
        let conversation = conversation.id.clone();
        let message = self.selected_history().get(index)?;
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
            .message_view(index)
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
}
