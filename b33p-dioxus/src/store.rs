//! The chat state, shared by every component.
//!
//! `b33p-core` keeps all of what a chat UI shows in one [`ChatState`]: events go in, what to
//! draw comes out, and what the user does comes back as [`Command`]s for the backend. Here it
//! lives in one signal, so that every component that reads it draws again when it changes.

use b33p_core::settings::NotificationContent;
use b33p_core::state::ChatState;
use b33p_core::{Command, Commands, Event};
use dioxus::prelude::*;

#[derive(Clone, Copy)]
pub struct Store {
    /// Read it in a component to draw from it; change it only through [`Store::act`].
    pub chat: Signal<ChatState>,
    commands: Signal<Commands>,
}

impl Store {
    /// Must be called while a component runs: the signals belong to it.
    pub fn new(commands: Commands) -> Self {
        let mut chat = ChatState::default();
        // Notifications are Android's to show, from a service that this app doesn't have yet:
        // the state need not make them up.
        chat.set_notifications(false, NotificationContent::Nothing);
        // The window has the user's attention whenever there is a window to draw into; what
        // happens while the app is in the background is for the Android lifecycle to say.
        chat.set_window_focused(true);
        Self {
            chat: Signal::new(chat),
            commands: Signal::new(commands),
        }
    }

    /// Changes the state, then sends the backend what follows from it (read receipts and the
    /// like).
    pub fn act<R>(&self, action: impl FnOnce(&mut ChatState) -> R) -> R {
        let mut signal = self.chat;
        let mut chat = signal.write();
        let result = action(&mut chat);
        for command in chat.take_commands() {
            self.commands.peek().send(command);
        }
        result
    }

    /// Changes the state, and sends the command it answers with, if any.
    pub fn command(&self, action: impl FnOnce(&mut ChatState) -> Option<Command>) {
        if let Some(command) = self.act(action) {
            self.commands.peek().send(command);
        }
    }

    /// Applies what the backend sent, a burst of events as one change.
    pub fn apply(&self, events: Vec<Event>) {
        self.act(|chat| {
            chat.apply_all(events);
            // Nothing shows them here yet; unread, they would pile up.
            drop(chat.take_notices());
        });
    }

    /// The backend's channel closed without a word: it died. Say so instead of freezing.
    pub fn backend_gone(&self) {
        use b33p_core::state::Screen;
        use b33p_core::{Event, Failure};
        self.act(|chat| {
            if !matches!(chat.screen(), Screen::Failed { .. }) {
                chat.apply(Event::Failed {
                    failure: Failure::Stopped,
                    message: "b33p stopped working unexpectedly. Close it and open it again to \
                              try again."
                        .into(),
                });
            }
        });
    }
}
