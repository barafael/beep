//! Picking messages, and forwarding them.

use super::*;

impl ChatState {
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
        let message = self.selected_history().get(index).cloned();
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
        let filter = search_term(filter);
        self.conversations
            .iter()
            .filter(|c| self.can_write_to(c))
            .filter(|c| {
                if filter.is_empty() {
                    // As in the chat list, without the archive split.
                    c.is_group() || c.note_to_self || self.last_of(&c.id).is_some()
                } else {
                    title_matches(c, &filter)
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
                .zip(&selection)
                .all(|(m, s)| message_key(m) == message_key(s))
        {
            let unpicked: Vec<MessageRef> = selection.iter().map(|m| m.reference()).collect();
            self.selection.clear();
            self.selection_anchor = None;
            self.changed_rows.get_mut().note(unpicked);
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

    /// Picks the `index`th message of [`ChatState::messages`], or unpicks it. Picked messages
    /// are copied, forwarded or deleted together; see [`ChatState::selection`]. Notices can't
    /// be picked: there is nothing to do with them.
    pub fn toggle_selected(&mut self, index: usize) -> Update {
        let Some(message) = self.selected_history().get(index) else {
            return Update::default();
        };
        if message.notice {
            return Update::default();
        }
        let target = message.reference();
        if !self.selection.remove(ref_key(&target)) {
            self.selection.insert(ref_key(&target));
        }
        self.changed_rows.get_mut().note([target.clone()]);
        self.selection_anchor = Some(target);
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// Picks every message from the one last picked (or unpicked) through the `index`th; just
    /// the `index`th if none was.
    pub fn select_through(&mut self, index: usize) -> Update {
        let history = self
            .selected
            .as_ref()
            .and_then(|id| self.history.get(id))
            .map_or(&[][..], Vec::as_slice);
        let anchor = self
            .selection_anchor
            .as_ref()
            .and_then(|anchor| find(history, ref_key(anchor)));
        let (Some(from), true) = (anchor, index < history.len()) else {
            return self.toggle_selected(index);
        };
        let changed = self.changed_rows.get_mut();
        for message in &history[from.min(index)..=from.max(index)] {
            if !message.notice && self.selection.insert(message_key(message)) {
                changed.note([message.reference()]);
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
        self.changed_rows.get_mut().note_all();
        Update {
            rows: true,
            ..Update::default()
        }
    }

    /// The picked messages still in the selected chat, in its order.
    pub fn selection(&self) -> Vec<&ChatMessage> {
        let history = self.selected_history();
        self.picked_rows()
            .into_iter()
            .map(|i| &history[i])
            .collect()
    }

    /// How many messages [`ChatState::selection`] has, without gathering them.
    pub fn selection_len(&self) -> usize {
        let history = self.selected_history();
        self.selection
            .iter()
            .filter(|&key| find(history, key).is_some())
            .count()
    }

    /// Whether [`ChatState::selection`] has any message, without gathering them.
    pub fn has_selection(&self) -> bool {
        let history = self.selected_history();
        self.selection
            .iter()
            .any(|key| find(history, key).is_some())
    }

    /// Where the picked messages still in the selected chat are in its history, in order.
    pub(super) fn picked_rows(&self) -> Vec<usize> {
        let history = self.selected_history();
        let mut rows: Vec<usize> = self
            .selection
            .iter()
            .filter_map(|key| find(history, key))
            .collect();
        rows.sort_unstable();
        rows
    }

    /// What the picked messages say, one after the other, for copying: each its text, or what
    /// is attached when it has none.
    pub fn selected_text(&self) -> String {
        let mut text = String::new();
        for m in self.selection() {
            let said = if m.deleted {
                DELETED
            } else {
                match self.full_text(m).unwrap_or(&m.body) {
                    "" => m.attachments.first().map_or("", |a| describe_kind(a.kind)),
                    body => body,
                }
            };
            if said.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(said);
        }
        text
    }

    /// Whether every picked message can be forwarded; see [`ChatState::can_forward`].
    pub fn can_forward_selection(&self) -> bool {
        self.all_picked(Self::can_forward)
    }

    /// Whether every picked message can still be deleted for everyone.
    pub fn can_delete_selection_for_everyone(&self) -> bool {
        let now = self.now();
        self.all_picked(|m| editable(m, now))
    }

    /// Whether something is picked, and `test` holds for every picked message: asked at every
    /// redraw, so in no particular order (sorting them is what [`ChatState::selection`] costs).
    pub(super) fn all_picked(&self, mut test: impl FnMut(&ChatMessage) -> bool) -> bool {
        let history = self.selected_history();
        let mut any = false;
        self.selection
            .iter()
            .filter_map(|key| find(history, key))
            .all(|at| {
                any = true;
                test(&history[at])
            })
            && any
    }

    /// Deletes the picked messages, each as [`ChatState::delete`] would: the command for the
    /// first one; those for the others come from [`ChatState::take_commands`].
    pub fn delete_selection(&mut self, for_everyone: bool) -> (Option<Command>, Update) {
        let rows = self.picked_rows();
        self.selection.clear();
        self.selection_anchor = None;
        let Some(history) = self
            .selected
            .as_ref()
            .and_then(|id| self.history.get_mut(id))
        else {
            return (None, Update::default());
        };
        let now = self.clock.now();
        let mut gone = vec![false; history.len()];
        let mut unsent = Vec::new();
        let mut commands = Vec::new();
        for i in rows {
            let message = &mut history[i];
            if message.notice || for_everyone && !editable(message, now) {
                continue;
            }
            if message.unsent.is_some() {
                // Never went out (or is still going): nothing to tell anyone, only not to show
                // it as failed again. A send still under way finishes regardless, and the sent
                // message shows up.
                unsent.push((message.conversation.clone(), message.timestamp));
                commands.push(Command::DropUnsent {
                    conversation: message.conversation.clone(),
                    timestamp: message.timestamp,
                });
                continue;
            }
            let target = message.reference();
            if self.editing.as_ref() == Some(&target) {
                self.editing = None;
            }
            if for_everyone {
                tombstone(message);
            } else {
                gone[i] = true;
            }
            commands.push(Command::Delete {
                conversation: message.conversation.clone(),
                target,
                for_everyone,
            });
        }
        let mut i = 0;
        history.retain(|m| {
            let keep = !gone[i]
                && !unsent
                    .iter()
                    .any(|(_, timestamp)| is_unsent_at(m, *timestamp));
            i += 1;
            keep
        });
        let update = if commands.is_empty() {
            Update::default()
        } else {
            self.forget_changed_rows();
            Update::reset()
        };
        for send in unsent {
            self.pending_sends.remove(&send);
        }
        let mut commands = commands.into_iter();
        let first = commands.next();
        self.queued.extend(commands);
        (first, update)
    }
}
