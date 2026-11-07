//! Messages pinned in the open chat: the bar above it, and jumping to one.

use super::*;

impl ChatState {
    /// What is pinned in the open chat, oldest pin first, without pins that ran out.
    pub fn pinned_messages(&self) -> Vec<&PinnedMessage> {
        let now = self.now();
        self.selected
            .as_ref()
            .and_then(|id| self.pins.get(id))
            .into_iter()
            .flatten()
            .filter(|pin| pin.expires_at.is_none_or(|at| at > now))
            .collect()
    }

    /// When the next of the open chat's pins runs out (ms since the epoch), for the window
    /// to look again then.
    pub fn next_pin_expiry(&self) -> Option<u64> {
        let now = self.now();
        self.pinned_messages()
            .iter()
            .filter_map(|pin| pin.expires_at)
            .filter(|&at| at > now)
            .min()
    }

    /// Shows the `index`th of [`ChatState::pinned_messages`] in the chat, as a click on it in the bar
    /// does: marks it and scrolls to it, loading older history first if need be.
    pub fn show_pinned(&mut self, index: usize) -> Update {
        let target = self
            .pinned_messages()
            .get(index)
            .map(|pin| pin.message.reference());
        match target {
            Some(target) => self.jump_to_message(target),
            None => Update::default(),
        }
    }

    /// Messages gone from `conversation` (deleted for everyone, disappeared) are no longer
    /// pinned, as the backend's table has them already. Whether the open chat's pins changed.
    pub(super) fn unpin_gone(
        &mut self,
        conversation: &ConversationId,
        gone: &[MessageRef],
    ) -> bool {
        let Some(pins) = self.pins.get_mut(conversation) else {
            return false;
        };
        let before = pins.len();
        pins.retain(|pin| !gone.iter().any(|target| pin.message.is(target)));
        pins.len() != before && self.selected.as_ref() == Some(conversation)
    }

    /// A pinned message was edited: the bar shows its new text. Whether the open chat's pins
    /// changed.
    pub(super) fn pinned_edited(
        &mut self,
        conversation: &ConversationId,
        target: &MessageRef,
        edited: &ChatMessage,
    ) -> bool {
        let Some(pin) = self
            .pins
            .get_mut(conversation)
            .and_then(|pins| pins.iter_mut().find(|pin| pin.message.is(target)))
        else {
            return false;
        };
        // Only its sender edits a message, as for the chat itself.
        if edited.sender_id != target.author_id {
            return false;
        }
        pin.message = ChatMessage {
            timestamp: target.timestamp,
            ..edited.clone()
        };
        self.selected.as_ref() == Some(conversation)
    }

    /// Whether messages can be pinned in the open chat, as Signal-Desktop's `canPinMessages`:
    /// a chat with one person, or a group we may change the details of.
    pub fn can_pin_messages(&self) -> bool {
        self.selected().is_some_and(|conversation| {
            self.can_write_to(conversation)
                && (!conversation.is_group()
                    || conversation.admin
                    || conversation.members_edit_details)
        })
    }

    /// Whether the open chat's `index`th message is pinned.
    pub fn is_pinned(&self, index: usize) -> bool {
        self.selected_history().get(index).is_some_and(|message| {
            self.pinned_messages()
                .iter()
                .any(|pin| pin.message.is(&message.reference()))
        })
    }

    /// Pins the open chat's `index`th message for `seconds`, or until unpinned with `None`.
    /// Not a notice, a deleted message, or one still being sent.
    pub fn pin_message(&self, index: usize, seconds: Option<u32>) -> Option<Command> {
        if !self.can_pin_messages() || self.is_pinned(index) {
            return None;
        }
        let message = self.selected_history().get(index)?;
        if message.notice || message.deleted || message.unsent.is_some() {
            return None;
        }
        Some(Command::PinMessage {
            conversation: self.selected()?.id.clone(),
            target: message.reference(),
            seconds,
        })
    }

    /// Unpins the open chat's `index`th message.
    pub fn unpin_message(&self, index: usize) -> Option<Command> {
        if !self.can_pin_messages() || !self.is_pinned(index) {
            return None;
        }
        let message = self.selected_history().get(index)?;
        Some(Command::UnpinMessage {
            conversation: self.selected()?.id.clone(),
            target: message.reference(),
        })
    }

    /// Whether pinning one more message in the open chat replaces its oldest pin, which
    /// Signal-Desktop asks about first.
    pub fn pins_full(&self) -> bool {
        self.pinned_messages().len() >= PIN_LIMIT
    }
}

/// The pins a chat keeps, as Signal's apps by default: a fourth replaces the oldest.
pub const PIN_LIMIT: usize = 3;
