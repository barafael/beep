//! The chat list: which chats show, in what order, and their settings.

use super::*;

impl ChatState {
    /// Selects a conversation; until its stored history has arrived, returns the command
    /// that fetches it (afterwards live events keep it current). Asking again after a failed
    /// load retries it; a duplicate answer is harmless because inserts are deduplicated.
    pub fn select(&mut self, id: &ConversationId) -> Option<Command> {
        if !self.conversation_at.contains_key(id) {
            return None;
        }
        self.read_seen();
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
            self.jump_to_unread = true;
            self.forget_changed_rows();
        }
        self.selected = Some(id.clone());
        self.continue_unread_jump();
        self.mark_seen();
        // Picking a search result ends the search; the chosen chat stays on the list.
        self.filter.clear();
        self.notifications
            .pending
            .push(Notice::Withdraw(id.clone()));
        (!self.is_loaded(id)).then(|| Command::LoadHistory {
            conversation: id.clone(),
            before: None,
        })
    }

    /// A conversation's picture, if known.
    pub fn avatar(&self, id: &ConversationId) -> Option<&Arc<Picture>> {
        match id.contact_id() {
            Some(person) => self.people_avatars.get(person),
            None => self.avatars.get(id),
        }
    }

    pub fn conversation(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversation_at
            .get(id)
            .map(|&at| &self.conversations[at])
    }

    pub(super) fn conversation_mut(&mut self, id: &ConversationId) -> Option<&mut Conversation> {
        let at = *self.conversation_at.get(id)?;
        Some(&mut self.conversations[at])
    }

    /// Checks [`ChatState::conversation_at`] against the list.
    #[cfg(test)]
    pub(super) fn assert_indexed(&self) {
        assert_eq!(self.conversation_at.len(), self.conversations.len());
        for (at, c) in self.conversations.iter().enumerate() {
            assert_eq!(self.conversation_at.get(&c.id), Some(&at), "{:?}", c.id);
        }
    }

    /// Whether the conversation's newest page of history has come.
    pub(super) fn is_loaded(&self, id: &ConversationId) -> bool {
        self.older.contains_key(id)
    }

    /// Back to the chat list, e.g. on a narrow window that shows one or the other.
    pub fn deselect(&mut self) -> Update {
        self.read_seen();
        self.keep_draft_mentions();
        self.selected = None;
        self.replying = None;
        self.editing = None;
        self.selection.clear();
        self.selection_anchor = None;
        self.read_mark = None;
        self.draft_mentions.clear();
        self.staged.clear();
        self.forget_changed_rows();
        Update::reset()
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
            && !self.written_to.contains(&conversation.id)
    }

    /// Answers the message request in `id`.
    pub fn respond(&mut self, id: &ConversationId, response: RequestResponse) -> Option<Command> {
        let conversation = self.conversation_mut(id)?;
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

    /// Looks up who a typed phone number belongs to, then opens their chat or chooses them for
    /// the group being made; nothing for text that isn't a phone number.
    pub fn find_number(&self, text: &str, then: crate::AfterFinding) -> Option<Command> {
        (self.synced && looks_like_number(text)).then(|| Command::FindNumber {
            number: text.trim().to_owned(),
            then,
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

    /// Changes a conversation's settings here at once; the command makes it last.
    pub(super) fn change_settings(
        &mut self,
        id: &ConversationId,
        change: impl FnOnce(&mut ChatSettings),
    ) -> Option<()> {
        let conversation = self.conversation_mut(id)?;
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

    pub(super) fn set_pins(&mut self, pinned: Vec<ConversationId>) -> Command {
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
        let conversation = self.conversation_mut(id)?;
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

    /// Names of who is typing in a conversation.
    pub fn typing(&self, id: &ConversationId) -> Vec<&str> {
        let now = self.now();
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
        let now = self.now();
        self.typing
            .values()
            .flatten()
            .map(|t| t.until)
            .filter(|&until| until > now)
            .min()
    }

    /// Unread messages over all conversations, e.g. for the tray icon.
    /// Muted conversations don't count; ones marked unread by hand count as one.
    pub fn unread_total(&self) -> u32 {
        let now = self.now();
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

    pub(super) fn view<'a>(&'a self, c: &'a Conversation) -> ConversationView<'a> {
        ConversationView {
            conversation: c,
            avatar: self.avatar(&c.id),
            last_message: self.last_of(&c.id),
            unread: self.unread.get(&c.id).copied().unwrap_or_default(),
            selected: self.selected.as_ref() == Some(&c.id),
            typing: self.typing(&c.id),
            now: self.now(),
        }
    }

    pub fn selected(&self) -> Option<&Conversation> {
        self.conversation(self.selected.as_ref()?)
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

    pub(super) fn listed(&self) -> impl Iterator<Item = &Conversation> {
        let filter = search_term(&self.filter);
        let now = self.now();
        self.conversations.iter().filter(move |c| {
            if filter.is_empty() {
                c.settings.archived == self.show_archived
                    && (c.is_group()
                        || c.note_to_self
                        || c.settings.pinned.is_some()
                        || self.last_of(&c.id).is_some()
                        || self.selected.as_ref() == Some(&c.id))
                    && (self.show_archived
                        || self.folder.is_none_or(|at| self.in_folder(c, at, now)))
            } else {
                title_matches(c, &filter)
            }
        })
    }

    /// Whether the `at`th folder shows a conversation, by Signal's rules. The open one always
    /// stays.
    pub(super) fn in_folder(&self, c: &Conversation, at: usize, now: u64) -> bool {
        if self.selected.as_ref() == Some(&c.id) {
            return true;
        }
        let (folder, chats) = (&self.folders[at], &self.folder_chats[at]);
        let member = chats.included.contains(&c.id)
            || (folder.all_individual && !c.is_group())
            || (folder.all_groups && c.is_group());
        member
            && !chats.excluded.contains(&c.id)
            && (folder.show_muted || !c.settings.muted(now))
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
        if at >= self.folders.len() {
            return 0;
        }
        let now = self.now();
        // Only chats with unread messages count: fewer than there are chats, mostly.
        self.unread
            .iter()
            .filter_map(|(id, &unread)| Some((self.conversation(id)?, unread)))
            .filter(|(c, _)| !c.settings.archived && self.in_folder(c, at, now))
            .map(|(_, unread)| unread)
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

    /// The chats whose rows in [`ChatState::conversations`] events changed since the last call,
    /// for the UI to redraw just those (and the rows that moved); the open chat's always,
    /// since reading it changes its row at the end of a batch. `None` means every row: when
    /// none were noted, and when an event changed more than its own chat's row.
    ///
    /// Only events note chats. What the UI does itself ([`Update`]s that actions return) isn't
    /// noted, so a redraw that includes one must redraw every row whatever this says.
    ///
    /// To be called at every redraw of the list, so that what was noted doesn't pile up.
    pub fn take_changed_chats(&self) -> Option<Vec<ConversationId>> {
        let ChangedChats { mut chats, all } = self.changed_chats.take();
        if all || chats.is_empty() {
            return None;
        }
        if let Some(open) = &self.selected
            && !chats.contains(open)
        {
            chats.push(open.clone());
        }
        Some(chats)
    }

    /// The chats [`ChatState::conversations`] lists, in its order, without the rest of their
    /// rows: for telling which rows moved.
    pub fn listed_ids(&self) -> impl Iterator<Item = &ConversationId> {
        self.listed().map(|c| &c.id)
    }

    /// A chat as [`ChatState::conversations`] shows it, whether listed or not.
    pub fn conversation_view(&self, id: &ConversationId) -> Option<ConversationView<'_>> {
        self.conversation(id).map(|c| self.view(c))
    }

    pub(super) fn last_of(&self, id: &ConversationId) -> Option<&ChatMessage> {
        self.history.get(id).and_then(|h| h.last())
    }

    /// Pinned ones first, in their order; then newest activity first; conversations without messages alphabetically after those,
    /// names before bare phone numbers.
    pub(super) fn sort_conversations(&mut self) {
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
        for (at, c) in self.conversations.iter().enumerate() {
            if let Some(indexed) = self.conversation_at.get_mut(&c.id) {
                *indexed = at;
            }
        }
    }
}
