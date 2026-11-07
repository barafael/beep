//! Who has received and read our messages: a message's info, and who read up to where.

use super::*;

impl ChatState {
    /// Shows the info of the open chat's `index`th message, if it is ours and sent: who it
    /// went to, and when each received and read it. Returns the command that gathers that.
    pub fn open_message_info(&mut self, index: usize) -> Option<Command> {
        let conversation = self.selected.clone()?;
        let message = self.selected_history().get(index)?;
        if !message.outgoing || message.notice || message.unsent.is_some() {
            return None;
        }
        let target = message.reference();
        self.message_info = Some((conversation.clone(), target.clone(), None));
        Some(Command::LoadMessageInfo {
            conversation,
            target,
        })
    }

    pub fn close_message_info(&mut self) {
        self.message_info = None;
    }

    /// The message whose info is shown, and the info once gathered.
    pub fn message_info(&self) -> Option<(&ChatMessage, Option<&MessageInfo>)> {
        let (conversation, target, info) = self.message_info.as_ref()?;
        if self.selected.as_ref() != Some(conversation) {
            return None;
        }
        let history = self.history.get(conversation)?;
        let message = &history[find(history, ref_key(target))?];
        Some((message, info.as_ref()))
    }

    /// In a group, the members whose newest message of ours read is the open chat's `index`th,
    /// as Signal's apps put their pictures under it. Empty for others' messages and for a chat
    /// of two, where the ticks say it.
    pub fn readers_of(&self, index: usize) -> Vec<&Person> {
        let Some(conversation) = self.selected().filter(|c| c.is_group()) else {
            return Vec::new();
        };
        let Some(message) = self.selected_history().get(index).filter(|m| m.outgoing) else {
            return Vec::new();
        };
        let Some(newest) = self.readers.get(&conversation.id) else {
            return Vec::new();
        };
        conversation
            .members
            .iter()
            .filter(|member| newest.get(&member.service_id) == Some(&message.timestamp))
            .collect()
    }
}
