//! The screens before and around the chats: unlocking, linking, failures, unlinking.

use super::*;

impl ChatState {
    /// Validates the password form. On success returns the unlock command and shows the
    /// connecting screen; otherwise puts the reason on the password screen.
    pub fn unlock(&mut self, password: &str, confirmation: &str) -> Option<Command> {
        let Screen::Password { new, error } = &mut self.screen else {
            return None;
        };
        let problem = if password.is_empty() {
            Some("Enter a password.".to_string())
        } else if *new && let Some(problem) = password_problem(password) {
            Some(problem)
        } else if *new && password != confirmation {
            Some("The passwords don't match.".to_string())
        } else {
            None
        };
        if problem.is_some() {
            *error = problem;
            return None;
        }
        self.screen = Screen::Syncing(SyncStage::Starting);
        Some(Command::Unlock(Password::new(password)))
    }

    pub fn dismiss_error(&mut self) {
        self.error = None;
        self.error_retry = None;
    }

    /// Shows `error`, offering to try `command` again if that may work and is worth offering
    /// (see [`offers_retry`]).
    pub(super) fn show_error(&mut self, error: crate::Error, command: Option<Command>) {
        if let Some(join) = &mut self.join {
            join.busy = false;
        }
        self.error_retry = command.filter(|command| error.retryable() && offers_retry(command));
        self.error = Some(error);
    }

    /// Whether the error shown comes with a way to try again; see [`ChatState::retry_error`].
    pub fn can_retry_error(&self) -> bool {
        self.error_retry.is_some()
    }

    /// Tries the command that failed again, and stops showing its error; `None` if the error
    /// shown has nothing to try again.
    pub fn retry_error(&mut self) -> Option<Command> {
        let command = self.error_retry.take()?;
        self.error = None;
        if let Command::JoinGroup(_) | Command::PreviewGroupLink(_) = &command
            && let Some(join) = &mut self.join
        {
            join.busy = true;
        }
        Some(command)
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// After a failure: tries again.
    pub fn retry(&mut self) -> Option<Command> {
        let Screen::Failed { failure, .. } = self.screen else {
            return None;
        };
        if !failure.can_retry() {
            return None;
        }
        self.screen = Screen::Syncing(SyncStage::Starting);
        Some(Command::Retry)
    }

    /// Whether [`ChatState::start_over`] would: after some failures, or for a forgotten
    /// password.
    pub fn can_start_over(&self) -> bool {
        match &self.screen {
            Screen::Failed { failure, .. } => failure.can_start_over(),
            Screen::Password { new, .. } => !new,
            _ => false,
        }
    }

    /// Deletes everything stored here and starts again with linking. The UI asks first.
    pub fn start_over(&mut self) -> Option<Command> {
        if !self.can_start_over() {
            return None;
        }
        self.forget_account();
        Some(Command::StartOver)
    }

    /// After [`Failure::KeyringLocked`]: protects the data with a password instead.
    pub fn use_password(&mut self) -> Option<Command> {
        let Screen::Failed { failure, .. } = &self.screen else {
            return None;
        };
        if !failure.can_use_password() {
            return None;
        }
        self.screen = Screen::Syncing(SyncStage::Starting);
        Some(Command::UsePassword)
    }

    /// Removes this device from the account and deletes what is stored here, then starts
    /// again with linking. Only while linked; the UI asks first.
    pub fn unlink(&mut self) -> Option<Command> {
        if self.account.is_empty() || !matches!(self.screen, Screen::Chats) {
            return None;
        }
        self.forget_account();
        Some(Command::Unlink)
    }

    /// Back to how a fresh start looks, for data that is being deleted. What the UI set (the
    /// notification policy, whether the window has focus) stays.
    pub(super) fn forget_account(&mut self) {
        let notifications = std::mem::take(&mut self.notifications);
        *self = Self {
            notifications: Notifications {
                enabled: notifications.enabled,
                content: notifications.content,
                window_focused: notifications.window_focused,
                ..Notifications::default()
            },
            ..Self::default()
        };
    }

    /// While waiting for the phone, gives up on the history transfer.
    pub fn skip_history(&mut self) -> Option<Command> {
        (self.transfer == Some(HistoryTransfer::Waiting)).then_some(Command::SkipHistory)
    }

    /// The message history transfer in progress, if any.
    pub fn transfer(&self) -> Option<HistoryTransfer> {
        self.transfer
    }

    pub fn synced(&self) -> bool {
        self.synced
    }

    /// How the connection to Signal is doing, for the chat list.
    pub fn connection(&self) -> Connection {
        match (self.synced, self.connected) {
            (true, _) => Connection::Online,
            (false, true) => Connection::CatchingUp,
            (false, false) => Connection::Offline,
        }
    }

    /// Whether the start-up screen may be left for the chats already: it is catching up on
    /// what came meanwhile, which can go on while the chats show (the chat list says so).
    pub fn can_start(&self) -> bool {
        matches!(self.screen, Screen::Syncing(SyncStage::CatchingUp { .. }))
    }

    /// Leaves the start-up screen for the chats while catching up goes on; see
    /// [`ChatState::can_start`].
    pub fn start(&mut self) -> Update {
        if !self.can_start() {
            return Update::default();
        }
        self.screen = Screen::Chats;
        Update {
            screen: true,
            conversations: true,
            ..Update::default()
        }
    }

    /// What the start-up screen waits for, while it shows.
    pub fn sync_stage(&self) -> Option<SyncStage> {
        match self.screen {
            Screen::Syncing(stage) => Some(stage),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_ref().map(|error| error.message.as_str())
    }
}

/// Whether the error banner offers to try `command` again when it failed for a reason that
/// may pass: what the user asked for and waits on. Not sending a message (its bubble has its
/// own "Retry"), downloads (the picture's place has), nor what happens by itself or again
/// anyway (typing, reading, settings, the session's own steps).
fn offers_retry(command: &Command) -> bool {
    match command {
        Command::LoadHistory { .. }
        | Command::SearchChat { .. }
        | Command::ViewStory(_)
        | Command::SendStory { .. }
        | Command::CreateGroup { .. }
        | Command::EditGroup { .. }
        | Command::PreviewGroupLink(_)
        | Command::JoinGroup(_)
        | Command::LoadStickers
        | Command::SendSticker { .. }
        | Command::Block { .. }
        | Command::Respond { .. }
        | Command::LoadDetails(_)
        | Command::FindNumber { .. }
        | Command::SetVerified { .. }
        | Command::LoadMedia(_)
        | Command::Forward { .. }
        | Command::MarkUnread(_)
        | Command::Edit { .. }
        | Command::Delete { .. }
        | Command::SetPins(_)
        | Command::Mute { .. }
        | Command::Archive { .. }
        | Command::SetTimer { .. }
        | Command::React { .. } => true,
        Command::Unlock(_)
        | Command::Retry
        | Command::StartOver
        | Command::UsePassword
        | Command::Unlink
        | Command::SetDeviceName(_)
        | Command::SkipHistory
        | Command::FetchAttachment(_)
        | Command::SaveAttachment(_)
        | Command::OpenAttachment(_)
        | Command::Send { .. }
        | Command::Typing { .. }
        | Command::MarkRead { .. }
        | Command::Preferences(_) => false,
    }
}
