//! What each backend event does to the state.

use super::*;

impl ChatState {
    /// Applies a batch of events, sorting the chat list once at the end rather than per
    /// event (a sync brings one event per chat).
    pub fn apply_all(&mut self, events: impl IntoIterator<Item = Event>) -> Update {
        let update = events.into_iter().fold(Update::default(), |update, event| {
            update.merge(self.apply_one(event))
        });
        self.finish_batch();
        update
    }

    /// Takes the time from `clock` from now on; see [`Clock`].
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    pub(super) fn now(&self) -> u64 {
        self.clock.now()
    }

    pub fn apply(&mut self, event: Event) -> Update {
        let update = self.apply_one(event);
        self.finish_batch();
        update
    }

    /// What waits for the end of a batch of events.
    pub(super) fn finish_batch(&mut self) {
        if std::mem::take(&mut self.sort_pending) {
            self.sort_conversations();
        }
        self.read_seen();
    }

    pub(super) fn apply_one(&mut self, event: Event) -> Update {
        let row = row_changed_by(&event);
        let update = self.apply_event(event);
        if update.conversations {
            self.changed_chats.get_mut().note(row);
        }
        update
    }

    pub(super) fn apply_event(&mut self, event: Event) -> Update {
        let mut update = Update::default();
        // Reading what was seen waits for the end of the batch, unless this changes what
        // reading it would do: its chat's settings change, or messages go.
        if let Some(seen) = &self.seen_pending
            && match &event {
                Event::Settings { conversation, .. }
                | Event::Cleared(conversation)
                | Event::Expired { conversation, .. } => conversation == seen,
                Event::Conversations(list) => list.iter().any(|c| &c.id == seen),
                _ => false,
            }
        {
            self.read_seen();
        }
        if let (Event::Message(_), Screen::Syncing(SyncStage::CatchingUp { messages })) =
            (&event, &mut self.screen)
        {
            *messages += 1;
            update.screen = true;
        }
        match event {
            Event::Failed { failure, message } => {
                self.screen = Screen::Failed { failure, message };
                update.screen = true;
            }
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
                // A sticker load cut off by relinking never answers; ask again next time.
                if matches!(self.sticker_packs, Some(None)) {
                    self.sticker_packs = None;
                }
                self.screen = Screen::Syncing(SyncStage::Connecting);
                update.screen = true;
            }
            Event::Connected => {
                self.connected = true;
                update.screen = true;
                if let Screen::Syncing(stage) = &mut self.screen {
                    *stage = SyncStage::CatchingUp { messages: 0 };
                    update.screen = true;
                }
            }
            Event::Synced => {
                self.synced = true;
                update.screen = true;
                self.announce_caught_up();
                if matches!(self.screen, Screen::Syncing(_)) {
                    self.screen = Screen::Chats;
                    update.screen = true;
                }
            }
            Event::Reconnecting => {
                self.synced = false;
                self.connected = false;
                update.screen = true;
                // Signal out of reach at the start: the chats are still there to read.
                if matches!(self.screen, Screen::Syncing(_)) {
                    self.screen = Screen::Chats;
                    update.screen = true;
                }
            }
            Event::HistoryTransfer(stage) => {
                self.transfer = (stage != HistoryTransfer::Finished).then_some(stage);
                if let Screen::Syncing(now) = &mut self.screen {
                    *now = match stage {
                        HistoryTransfer::Finished => SyncStage::Connecting,
                        stage => SyncStage::Transfer(stage),
                    };
                    update.screen = true;
                }
            }
            Event::Conversations(list) => {
                for conversation in list {
                    match self.conversation_at.get(&conversation.id) {
                        // A chat listed again keeps its settings: the backend read them from its
                        // table, maybe before an answer or a pin made here was written there.
                        // What it changes itself comes as `Event::Settings`.
                        Some(&i) => {
                            let settings = std::mem::take(&mut self.conversations[i].settings);
                            self.conversations[i] = conversation;
                            self.conversations[i].settings = settings;
                        }
                        None => {
                            self.conversation_at
                                .insert(conversation.id.clone(), self.conversations.len());
                            self.conversations.push(conversation);
                        }
                    }
                }
                self.sort_pending = true;
                update.conversations = true;
            }
            Event::Attachment { id, content } => {
                let shown = self.attachment_rows(&id);
                let state = match content {
                    Ok(content) => {
                        self.downloaded_bytes += download_size(&content);
                        self.downloaded.push_back(id.clone());
                        AttachmentState::Ready(content)
                    }
                    Err(error) => AttachmentState::Failed(error),
                };
                if let Some(AttachmentState::Ready(old)) = self.attachments.insert(id, state) {
                    self.downloaded_bytes -= download_size(&old);
                }
                self.keep_downloads_in_bounds();
                update.rows = !shown.is_empty();
                self.changed_rows.get_mut().note(shown);
            }
            Event::AttachmentSaved { id, path } => {
                let state = match path {
                    Ok(path) => SaveState::Saved(path),
                    Err(error) => SaveState::Failed(error),
                };
                let shown = self.attachment_rows(&id);
                update.rows = !shown.is_empty();
                self.changed_rows.get_mut().note(shown);
                self.saves.insert(id, state);
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
                if let Some(at) = find(history, ref_key(&target)) {
                    apply_reaction(&mut history[at..=at], &target, reaction, remove);
                    update.rows = self.rows_changed(&conversation, [target]);
                } else if self.is_loaded(&conversation) {
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
                // Someone's picture also shows beside what they wrote in the open group.
                update.rows = conversation.contact_id().is_some_and(|person| {
                    self.selected().is_some_and(Conversation::is_group)
                        && self
                            .selected_history()
                            .iter()
                            .any(|m| !m.outgoing && m.sender_id == person)
                });
                if update.rows {
                    self.changed_rows.get_mut().note_all();
                }
                match (conversation.contact_id(), avatar) {
                    (Some(person), Some(avatar)) => {
                        self.people_avatars.insert(person.to_owned(), avatar);
                    }
                    (Some(person), None) => {
                        self.people_avatars.remove(person);
                    }
                    (None, Some(avatar)) => {
                        self.avatars.insert(conversation, avatar);
                    }
                    (None, None) => {
                        self.avatars.remove(&conversation);
                    }
                }
                update.conversations = true;
            }
            Event::History {
                conversation,
                messages,
                reactions,
                older,
            } => {
                let newest_page = self.older.insert(conversation.clone(), older).is_none();
                self.loading_older.remove(&conversation);
                let history = self.history.entry(conversation.clone()).or_default();
                let first = history.first().map(|m| (m.timestamp, m.sender_id.clone()));
                if messages.iter().any(|m| m.outgoing) {
                    self.written_to.insert(conversation.clone());
                }
                // The stored history is complete (delivery, edits, …); it replaces what came
                // before it, e.g. the bare newest message from `Event::Latest`.
                let (added, mut replaced) = upsert_all(history, messages);
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
                    match find(history, ref_key(&target)) {
                        Some(at) => {
                            apply_reaction(&mut history[at..=at], &target, reaction, remove);
                            replaced += 1;
                        }
                        None => waiting.push((target, reaction, remove)),
                    }
                }
                if newest_page || added > 0 {
                    self.sort_pending = true;
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
                    if replaced > 0 {
                        // Which ones isn't known.
                        update.rows = true;
                        self.changed_rows.get_mut().note_all();
                    }
                    if newest_page {
                        self.continue_unread_jump();
                        self.mark_seen_later();
                    }
                    self.continue_jump();
                }
            }
            Event::Latest(message) => {
                let id = message.conversation.clone();
                if message.outgoing {
                    self.written_to.insert(id.clone());
                }
                let history = self.history.entry(id.clone()).or_default();
                if insert(history, message).is_some() {
                    self.sort_pending = true;
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
                        .is_some_and(|history| unsent_at(history, message.timestamp).is_some()) =>
            {
                // Sent: it takes the place of the message shown while sending.
                let id = message.conversation.clone();
                self.pending_sends.remove(&(id.clone(), message.timestamp));
                self.written_to.insert(id.clone());
                let history = self.history.get_mut(&id).expect("checked above");
                let shown = unsent_at(history, message.timestamp).expect("checked above");
                history.remove(shown);
                let pos = insert(history, message);
                if self.selected.as_ref() == Some(&id) {
                    if pos == Some(shown) {
                        // Its sender is known now: the rows beside it may join its run.
                        let sent = history[shown].reference();
                        update.rows = self.rows_changed(&id, [sent]);
                    } else {
                        update.messages = MessagesUpdate::Reset;
                    }
                }
                update.conversations = true;
            }
            Event::Interrupted(sends) => {
                for send in sends {
                    let Command::Send {
                        conversation,
                        timestamp,
                        ..
                    } = &send
                    else {
                        continue;
                    };
                    let failed = Event::SendFailed {
                        conversation: conversation.clone(),
                        timestamp: *timestamp,
                        retryable: true,
                    };
                    update = update
                        .merge(self.show_sending(&send))
                        .merge(self.apply_event(failed));
                }
            }
            Event::SendFailed {
                conversation,
                timestamp,
                retryable,
            } => {
                if let Some(history) = self.history.get_mut(&conversation)
                    && let Some(at) = unsent_at(history, timestamp)
                {
                    let message = &mut history[at];
                    message.unsent = Some(if retryable {
                        Unsent::Failed
                    } else {
                        Unsent::Refused
                    });
                    let failed = message.reference();
                    update.rows = self.rows_changed(&conversation, [failed]);
                }
            }
            Event::Message(message) => {
                let id = message.conversation.clone();
                let outgoing = message.outgoing;
                // Blocking shows here at once; the backend drops a blocked chat's messages
                // once it takes the block, after the commands ahead of it. What it received in
                // between isn't shown either. What we send from another device still is.
                if !outgoing
                    && self
                        .conversation(&id)
                        .is_some_and(|chat| chat.settings.blocked)
                {
                    return update;
                }
                if outgoing {
                    self.written_to.insert(id.clone());
                }
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
                        apply_reaction(&mut history[pos..=pos], &target, reaction, remove);
                    }
                }
                let appended = pos + 1 == history.len();
                if let Some(timer) = history[pos].timer_change
                    && let Some(&at) = self.conversation_at.get(&id)
                {
                    self.conversations[at].expire_timer = timer;
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
                        (true, Some(attachment)) => describe_kind_for_notice(attachment.kind),
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
                // A change to a group whose details are open: they are read again. Its notice
                // comes once the group is stored as changed.
                if notice
                    && self.details.as_ref().is_some_and(|(shown, _)| *shown == id)
                    && self.conversation(&id).is_some_and(Conversation::is_group)
                {
                    self.queued.push(Command::LoadDetails(id.clone()));
                }
                if looking {
                    self.mark_seen_later();
                } else if !outgoing && !notice {
                    *self.unread.entry(id).or_default() += 1;
                }
                self.sort_pending = true;
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
                let mut changed = Vec::new();
                for timestamp in timestamps {
                    let sent = at_time(history, timestamp);
                    for message in history[sent]
                        .iter_mut()
                        .filter(|m| m.outgoing && m.delivery < delivery)
                    {
                        message.delivery = delivery;
                        changed.push(message.reference());
                    }
                }
                update.rows = !changed.is_empty() && self.rows_changed(&conversation, changed);
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
                        until: self.clock.now() + TYPING_TIMEOUT_MS,
                    });
                }
                update.conversations = true;
            }
            Event::Edited {
                conversation,
                target,
                mut message,
            } => {
                // Only a message's sender edits it; the history is sorted by (time, sender),
                // which an edit by anyone else would break.
                if message.sender_id == target.author_id
                    && let Some(old) = self.find_message(&conversation, &target)
                {
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
                    // Ours counts as sent again until a receipt names the edit, as in
                    // Signal's apps.
                    message.delivery = if message.outgoing {
                        crate::Delivery::Sent
                    } else {
                        old.delivery
                    };
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
                    let incoming = !message.outgoing;
                    *old = message;
                    if incoming {
                        self.unread_edits
                            .entry(conversation.clone())
                            .or_default()
                            .push(target.clone());
                        if self.selected.as_ref() == Some(&conversation)
                            && self.notifications.window_focused
                        {
                            self.mark_seen_later();
                        }
                    }
                    update.rows = self.rows_changed(&conversation, [target]);
                    update.conversations = true;
                }
            }
            Event::Deleted {
                conversation,
                target,
            } => {
                if let Some(message) = self.find_message(&conversation, &target) {
                    tombstone(message);
                    update.rows = self.rows_changed(&conversation, [target]);
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
                        update.rows |= self.rows_changed(&conversation, [target]);
                    }
                }
            }
            Event::Expired {
                conversation,
                targets,
            } => {
                if let Some(history) = self.history.get_mut(&conversation) {
                    let before = history.len();
                    let mut keep = vec![true; history.len()];
                    for at in targets.iter().filter_map(|t| find(history, ref_key(t))) {
                        keep[at] = false;
                    }
                    let mut at = 0;
                    history.retain(|_| {
                        at += 1;
                        keep[at - 1]
                    });
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
                let Some(existing) = self.conversation_mut(&conversation) else {
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
                self.sort_pending = true;
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
                self.folder_chats = folders.iter().map(FolderChats::of).collect();
                self.folders = folders;
                self.folder = self.folder.filter(|&at| at < self.folders.len());
                update.conversations = true;
            }
            Event::StickerPacks(packs) => {
                self.sticker_packs = Some(Some(packs));
            }
            // Gathered again only once asked for (the picker shown): those shown stay until the
            // answer replaces them, and an answer already on its way may be from before.
            Event::StickerPacksChanged => {
                if self.sticker_packs.is_some() {
                    self.queued.push(Command::LoadStickers);
                }
            }
            Event::StorageRead => {
                self.requests_known = true;
                update.conversations = true;
            }
            // Kept by the UI, with its other settings.
            Event::Preferences(_) => {}
            Event::Cleared(conversation) => {
                self.history.remove(&conversation);
                self.written_to.remove(&conversation);
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
                if let Some(listed) = self.conversation_mut(&conversation) {
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
                    *media = Some(Gathered::new(messages));
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
            Event::Error(error) => self.show_error(error, None),
            Event::CommandFailed { error, command } => self.show_error(error, Some(command)),
        }
        if update.messages == MessagesUpdate::Reset {
            self.forget_changed_rows();
        }
        update
    }

    pub(super) fn find_message(
        &mut self,
        conversation: &ConversationId,
        target: &MessageRef,
    ) -> Option<&mut ChatMessage> {
        let history = self.history.get_mut(conversation)?;
        let at = find(history, ref_key(target))?;
        Some(&mut history[at])
    }

    /// Notes that these messages of `conversation` show differently now, so that the UI
    /// redraws just their rows; returns whether they show at all (it is the open chat).
    pub(super) fn rows_changed(
        &mut self,
        conversation: &ConversationId,
        messages: impl IntoIterator<Item = MessageRef>,
    ) -> bool {
        let shown = self.selected.as_ref() == Some(conversation);
        if shown {
            self.changed_rows.get_mut().note(messages);
        }
        shown
    }

    /// The open chat's rows are made anew, or another chat's are shown: what changed in the
    /// old ones no longer matters.
    pub(super) fn forget_changed_rows(&mut self) {
        *self.changed_rows.get_mut() = ChangedRows::default();
    }
}

/// The one chat whose row in the list `event` can change, if its changes stop there: those
/// that come often (messages, typing, pictures). Any other event that changes the list has
/// every row redrawn. Not a contact's new name, say: group rows show names too; nor chat
/// settings: the number of pinned chats goes into every row.
pub(super) fn row_changed_by(event: &Event) -> Option<ConversationId> {
    match event {
        Event::Message(ChatMessage { conversation, .. })
        | Event::Latest(ChatMessage { conversation, .. })
        | Event::Typing { conversation, .. }
        | Event::Edited { conversation, .. }
        | Event::Deleted { conversation, .. }
        | Event::Expired { conversation, .. }
        | Event::Reaction { conversation, .. }
        | Event::Delivered { conversation, .. }
        | Event::History { conversation, .. }
        | Event::Avatar { conversation, .. } => Some(conversation.clone()),
        _ => None,
    }
}
