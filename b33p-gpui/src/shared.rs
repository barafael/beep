//! Everything that outlives a window: the chat state, the way to the backend, settings and
//! caches. Views read the state when they are drawn and ask [`ChatState`] what to do through
//! [`Shared::act`], which sends the command it answers with and draws what it changed.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use b33p_core::settings::Settings;
use b33p_core::state::{AttachmentState, ChatState, MessagesUpdate, Notice, Screen, Update};
use b33p_core::{
    AttachmentContent, AttachmentId, Command, Commands, ConversationId, Event, Failure,
};
use b33p_notify::{Notification, Notifier};
use b33p_platform::audio;
use b33p_platform::desktop::Requests as DesktopRequests;
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::{App, AsyncApp, Entity, Task};

use crate::images::Images;

/// What only the window keeps open or chosen; everything else the chat state knows.
#[derive(Default)]
pub struct UiState {
    pub settings_open: bool,
    pub calendar_open: bool,
    pub stories_open: bool,
    pub confirm: Confirm,
    /// A dialog's text, e.g. the message a confirmation is about.
    pub confirm_message: String,
    /// The window is narrower than a list and a chat side by side: one pane at a time.
    pub narrow: bool,
    pub sidebar_width: f32,
    /// What the harness says about the window being focused (`SetWindowFocused`).
    pub window_active: bool,
    pub window_visible: bool,
    /// The emoji or sticker picker over the composer, if one is open.
    pub composer_popup: crate::composer::Popup,
}

impl UiState {
    /// Opens (or closes) what only the window keeps open, as its buttons do: "settings",
    /// "stories", "calendar", or the dialogs that ask first: "unlink", "start_over",
    /// "leave_group". The name back if there is none like it.
    pub fn show_part(&mut self, what: &str, open: bool) -> Result<(), String> {
        match what {
            "settings" => self.settings_open = open,
            "stories" => self.stories_open = open,
            "calendar" => self.calendar_open = open,
            "unlink" | "start_over" | "leave_group" => {
                self.confirm = match (what, open) {
                    (_, false) => Confirm::None,
                    ("unlink", true) => Confirm::Unlink,
                    ("leave_group", true) => Confirm::LeaveGroup,
                    _ => Confirm::StartOver,
                }
            }
            _ => return Err(what.to_owned()),
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Confirm {
    #[default]
    None,
    StartOver,
    Unlink,
    LeaveGroup,
    BlockRequest,
    DeleteRequest,
    DeleteForEveryone,
    DeleteSelectionForEveryone,
}

pub struct Shared {
    pub chat: RefCell<ChatState>,
    pub images: Images,
    pub notifier: Notifier,
    pub commands: Commands,
    pub settings: RefCell<Settings>,
    pub ui: RefCell<UiState>,
    /// The desktop's file dialogs and clipboard; see `b33p_platform::desktop`.
    pub desktop: DesktopRequests,
    /// The open chat's messages' scrolling, which follows [`Update::messages`].
    pub scroller: Entity<MessageScrollerState>,
    /// The desktop asks for reduced motion.
    pub system_reduces_motion: Cell<bool>,
    /// What changed since the window was last brought in line with the state; handled once
    /// per turn of the event loop, however many actions and events a turn brings.
    pending: RefCell<Option<Update>>,
    /// Whether the window counts as focused, as the harness says (`SetWindowFocused`), in place
    /// of the window's own: in the test box no window manager ever activates it.
    pub focus_override: Cell<Option<bool>>,
    /// Redraws when someone typing next counts as stopped, since no event says so.
    typing_timer: RefCell<Option<Task<()>>>,
    /// Redraws the chat list at midnight, when its times change ("14:05" becomes "Mon").
    midnight_timer: RefCell<Option<Task<()>>>,
    /// Redraws the open chat's messages once a minute while some of them disappear.
    expiry_timer: RefCell<Option<Task<()>>>,
    /// Audio and voice messages playing here.
    pub player: RefCell<audio::Player>,
    /// Asked to play once downloaded.
    pending_play: RefCell<Option<AttachmentId>>,
    /// Moves the progress of what plays along.
    playback_timer: RefCell<Option<Task<()>>>,
    /// How far what plays got: its id, whether it is paused, 0..1, and the time shown.
    pub playback: RefCell<Playback>,
    /// The scroll request of [`ChatState::jump`] last carried out.
    jump_serial: Cell<u64>,
    /// The composer, which holds the text being written: the harness types into it.
    pub composer: RefCell<Option<gpui_kit::WeakEntity<crate::composer::ComposerView>>>,
    /// For tasks that outlive a call.
    pub me: Weak<Shared>,
    /// Answers the agent harness's socket (`--features harness`).
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub agent: crate::agent::Handle,
}

/// See [`Shared::playback`].
#[derive(Clone, Default, PartialEq)]
pub struct Playback {
    pub id: String,
    pub paused: bool,
    pub progress: f32,
    pub time: String,
}

impl Shared {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chat: ChatState,
        notifier: Notifier,
        commands: Commands,
        settings: Settings,
        desktop: DesktopRequests,
        scroller: Entity<MessageScrollerState>,
    ) -> Rc<Self> {
        let images = Images::default();
        images.avatars_enabled.set(settings.show_avatars);
        Rc::new_cyclic(|me| Self {
            chat: RefCell::new(chat),
            images,
            notifier,
            commands,
            settings: RefCell::new(settings),
            ui: RefCell::new(UiState {
                sidebar_width: 300.,
                window_visible: true,
                ..UiState::default()
            }),
            desktop,
            scroller,
            system_reduces_motion: Cell::new(false),
            pending: RefCell::default(),
            focus_override: Cell::new(None),
            typing_timer: RefCell::default(),
            midnight_timer: RefCell::default(),
            expiry_timer: RefCell::default(),
            player: RefCell::default(),
            pending_play: RefCell::default(),
            playback_timer: RefCell::default(),
            playback: RefCell::default(),
            jump_serial: Cell::new(0),
            composer: RefCell::default(),
            me: me.clone(),
            #[cfg(all(feature = "harness", target_family = "unix"))]
            agent: crate::agent::Handle::default(),
        })
    }

    /// Runs a UI action against the chat state, sends the command it produces and draws what
    /// it changed. Returns whether a command was sent.
    pub fn act(
        &self,
        cx: &mut App,
        action: impl FnOnce(&mut ChatState) -> (Option<Command>, Update),
    ) -> bool {
        let (command, update) = action(&mut self.chat.borrow_mut());
        let sent = command.is_some();
        if let Some(command) = command {
            self.commands.send(command);
        }
        self.refresh(update, cx);
        sent
    }

    /// Changes the state; what shows follows on its own.
    pub fn edit(&self, cx: &mut App, action: impl FnOnce(&mut ChatState)) {
        self.act(cx, |chat| {
            action(chat);
            (None, Update::default())
        });
    }

    /// Sends the command the state answers with, if any.
    pub fn command(&self, cx: &mut App, action: impl FnOnce(&mut ChatState) -> Option<Command>) {
        self.act(cx, |chat| (action(chat), Update::default()));
    }

    /// Draws what the state says changed.
    pub fn redraw(&self, cx: &mut App, action: impl FnOnce(&mut ChatState) -> Update) {
        self.act(cx, |chat| (None, action(chat)));
    }

    /// Brings the window in line with the state, at the next turn of the event loop: a key is
    /// typed, a batch of events comes, and each alone drew before.
    pub fn refresh(&self, update: Update, cx: &mut App) {
        let first = {
            let mut pending = self.pending.borrow_mut();
            match pending.take() {
                Some(earlier) => {
                    *pending = Some(earlier.merge(update));
                    false
                }
                None => {
                    *pending = Some(update);
                    true
                }
            }
        };
        if first {
            let me = self.me.clone();
            cx.defer(move |cx| {
                if let Some(shared) = me.upgrade() {
                    shared.flush(cx);
                }
            });
        }
    }

    /// Draws now what is pending.
    pub fn flush(&self, cx: &mut App) {
        let Some(update) = self.pending.borrow_mut().take() else {
            return;
        };
        let mut chat = self.chat.borrow_mut();
        self.images.forget_gone(&chat);
        self.follow_messages(&chat, update, cx);
        #[cfg(all(feature = "harness", target_family = "unix"))]
        self.agent.publish(&chat, &self.settings.borrow(), self);
        deliver_notices(&self.notifier, &mut chat);
        for command in chat.take_commands() {
            self.commands.send(command);
        }
        match chat.typing_expires() {
            Some(at) => {
                let wait = at.saturating_sub(b33p_core::now_millis()) + 50;
                self.after(&self.typing_timer, Duration::from_millis(wait), cx, |shared, cx| {
                    shared.refresh(
                        Update {
                            conversations: true,
                            ..Update::default()
                        },
                        cx,
                    );
                });
            }
            None => {
                self.typing_timer.take();
            }
        }
        // The times in the list just drawn hold until midnight (a second more, to be past it),
        // and so do the days over the messages ("Today"). Counted anew whenever the list is
        // drawn: a timer's clock stops while the computer sleeps.
        if update.conversations {
            self.after(
                &self.midnight_timer,
                b33p_core::state::until_midnight() + Duration::from_secs(1),
                cx,
                |shared, cx| {
                    shared.refresh(
                        Update {
                            conversations: true,
                            rows: true,
                            ..Update::default()
                        },
                        cx,
                    );
                },
            );
        }
        // What disappears says how long it has left ("5m"): counted down once a minute, while
        // the open chat has any.
        if chat.shows_expiring() {
            if self.expiry_timer.borrow().is_none() {
                self.every(&self.expiry_timer, Duration::from_secs(60), cx, |shared, cx| {
                    shared.refresh(
                        Update {
                            rows: true,
                            ..Update::default()
                        },
                        cx,
                    );
                });
            }
        } else {
            self.expiry_timer.take();
        }
        // A download asked to play has arrived.
        let pending = self.pending_play.borrow().clone();
        if let Some(id) = pending {
            match chat.attachment(&id) {
                Some(AttachmentState::Ready(AttachmentContent::Audio(audio))) => {
                    let audio = audio.clone();
                    self.pending_play.replace(None);
                    drop(chat);
                    self.start_playing(id, audio, cx);
                }
                Some(AttachmentState::Failed(_)) => {
                    self.pending_play.replace(None);
                }
                _ => {}
            }
        }
        cx.refresh_windows();
    }

    /// Tells the message list what happened to the messages, so that it keeps what is on screen
    /// in place.
    fn follow_messages(&self, chat: &ChatState, update: Update, cx: &mut App) {
        let count = chat.messages().len();
        self.scroller.update(cx, |scroller, cx| {
            match update.messages {
                MessagesUpdate::Unchanged => {
                    if scroller.item_count() != count {
                        scroller.reset(count, cx);
                    }
                }
                MessagesUpdate::Appended(n) => {
                    if scroller.item_count() + n == count {
                        scroller.append(n, cx);
                    } else {
                        scroller.reset(count, cx);
                    }
                }
                MessagesUpdate::Prepended(n) => {
                    if scroller.item_count() + n == count {
                        scroller.prepend(n, cx);
                    } else {
                        scroller.reset(count, cx);
                    }
                }
                MessagesUpdate::Reset => scroller.reset(count, cx),
            }
            if update.rows || update.messages != MessagesUpdate::Unchanged {
                // Rows changed in place may change in height (a picture arrived).
                scroller.remeasure(cx);
            }
            if update.scroll_to_end {
                scroller.scroll_to_end(cx);
            }
            if let Some((row, serial)) = chat.jump()
                && serial != self.jump_serial.replace(serial)
            {
                scroller.scroll_to_item(row, cx);
            }
        });
    }

    /// Runs `then` once, after `wait`; replaces what the timer in `slot` waited for.
    fn after(
        &self,
        slot: &RefCell<Option<Task<()>>>,
        wait: Duration,
        cx: &mut App,
        then: impl FnOnce(&Rc<Shared>, &mut App) + 'static,
    ) {
        let me = self.me.clone();
        let task = cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(wait).await;
            cx.update(|cx| {
                if let Some(shared) = me.upgrade() {
                    then(&shared, cx);
                }
            });
        });
        slot.replace(Some(task));
    }

    /// Runs `then` every `period`, until the timer in `slot` is replaced or dropped.
    fn every(
        &self,
        slot: &RefCell<Option<Task<()>>>,
        period: Duration,
        cx: &mut App,
        then: impl Fn(&Rc<Shared>, &mut App) + 'static,
    ) {
        let me = self.me.clone();
        let task = cx.spawn(async move |cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(period).await;
                let alive = cx.update(|cx| match me.upgrade() {
                    Some(shared) => {
                        then(&shared, cx);
                        true
                    }
                    None => false,
                });
                if !alive {
                    break;
                }
            }
        });
        slot.replace(Some(task));
    }

    /// Plays an audio attachment, or pauses or carries on if it is the one playing.
    pub fn toggle_playback(&self, id: AttachmentId, cx: &mut App) {
        let current = self.player.borrow_mut().state().map(|s| s.id.clone());
        // Whatever was waiting to download is no longer wanted.
        self.pending_play.replace(None);
        if current.as_ref() == Some(&id) {
            self.player.borrow_mut().toggle();
            self.show_playback(cx);
            return;
        }
        let ready = match self.chat.borrow().attachment(&id) {
            Some(AttachmentState::Ready(AttachmentContent::Audio(audio))) => Some(audio.clone()),
            _ => None,
        };
        match ready {
            Some(audio) => self.start_playing(id, audio, cx),
            None => {
                let command = self.chat.borrow_mut().fetch_audio(&id);
                let loading = matches!(
                    self.chat.borrow().attachment(&id),
                    Some(AttachmentState::Loading)
                );
                if loading {
                    self.pending_play.replace(Some(id));
                }
                if let Some(command) = command {
                    self.commands.send(command);
                }
                cx.refresh_windows();
            }
        }
    }

    /// Goes to `fraction` of the audio playing, if `id` is what plays; otherwise starts it
    /// there.
    pub fn seek_playback(&self, id: AttachmentId, fraction: f32, cx: &mut App) {
        let playing = self
            .player
            .borrow_mut()
            .state()
            .is_some_and(|state| state.id == &id);
        if !playing {
            self.toggle_playback(id.clone(), cx);
        }
        let result = self.player.borrow_mut().seek(&id, fraction);
        if let Err(error) = result {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't seek in audio");
        }
        self.show_playback(cx);
    }

    fn start_playing(&self, id: AttachmentId, audio: Arc<[u8]>, cx: &mut App) {
        if let Err(error) = self.player.borrow_mut().play(id, audio) {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't play audio");
            // The audio itself, which playing again won't change.
            let update = self
                .chat
                .borrow_mut()
                .apply(Event::Error(error.into()));
            self.refresh(update, cx);
            return;
        }
        self.every(
            &self.playback_timer,
            Duration::from_millis(200),
            cx,
            |shared, cx| shared.show_playback(cx),
        );
        self.show_playback(cx);
    }

    /// Shows how far playing got; stops the timer once nothing plays.
    fn show_playback(&self, cx: &mut App) {
        let mut player = self.player.borrow_mut();
        let shown = match player.state() {
            Some(state) => {
                let progress = state.length.map_or(0., |length| {
                    (state.position.as_secs_f32() / length.as_secs_f32().max(0.001)).min(1.)
                });
                let time = match state.length {
                    Some(length) => format!(
                        "{} / {}",
                        audio::clock(state.position),
                        audio::clock(length)
                    ),
                    None => audio::clock(state.position),
                };
                Playback {
                    id: state.id.0.clone(),
                    paused: state.paused,
                    progress,
                    time,
                }
            }
            None => {
                self.playback_timer.take();
                Playback::default()
            }
        };
        drop(player);
        if *self.playback.borrow() != shown {
            self.playback.replace(shown);
            cx.refresh_windows();
        }
    }

    /// Whether the window counts as focused: as the harness says, or as the window knows.
    pub fn focused(&self) -> bool {
        self.focus_override.get().unwrap_or_else(|| {
            let ui = self.ui.borrow();
            ui.window_visible && ui.window_active
        })
    }

    /// The backend's, after the phone changed the account's settings: shown and saved.
    pub fn take_preferences(&self, preferences: b33p_core::Preferences, cx: &mut App) {
        let mut settings = self.settings.borrow().clone();
        settings.preferences = preferences;
        self.apply_settings(settings, cx);
    }

    /// Saves and applies the settings.
    pub fn apply_settings(&self, settings: Settings, cx: &mut App) {
        if let Err(error) = settings.save() {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to save settings");
        }
        let theme = settings.theme;
        self.settings.replace(settings.clone());
        crate::theme::apply(theme, None, cx);
        self.commands
            .send(Command::Preferences(settings.preferences));
        let rebuild =
            self.images.avatars_enabled.replace(settings.show_avatars) != settings.show_avatars;
        self.act(cx, |chat| {
            chat.set_notifications(settings.notifications, settings.notification_content);
            (
                None,
                Update {
                    conversations: rebuild,
                    ..Update::default()
                },
            )
        });
    }

    /// Names this computer anew, for the phone's list of linked devices; empty goes back to
    /// the default name. Returns whether the backend was told (the name changed).
    pub fn rename_device(&self, name: &str, cx: &mut App) -> bool {
        let mut settings = self.settings.borrow().clone();
        let name = b33p_core::settings::clean_device_name(name);
        settings.device_name = (!name.is_empty()
            && name != b33p_core::settings::default_device_name())
        .then_some(name);
        let name = settings.device_name();
        if settings == *self.settings.borrow() {
            cx.refresh_windows();
            return false;
        }
        if let Err(error) = settings.save() {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to save settings");
        }
        self.settings.replace(settings);
        self.commands.send(Command::SetDeviceName(name));
        cx.refresh_windows();
        true
    }

    /// Applies a burst of backend events as one batch, so the window is drawn once per batch.
    pub fn apply_events(&self, batch: impl Iterator<Item = Event>, cx: &mut App) {
        let focused = self.focused();
        let mut preferences = None;
        let update = {
            let mut chat = self.chat.borrow_mut();
            chat.set_window_focused(focused);
            chat.apply_all(batch.inspect(|event| match event {
                // Commands log their own failures in the backend.
                Event::Error(error) => {
                    let message = b33p_core::scrub::scrubbed(error);
                    match error.kind() {
                        b33p_core::ErrorKind::Unreachable => tracing::warn!("{message}"),
                        _ => tracing::error!("{message}"),
                    }
                }
                Event::Preferences(taken) => preferences = Some(*taken),
                _ => {}
            }))
        };
        self.refresh(update, cx);
        if let Some(preferences) = preferences {
            self.take_preferences(preferences, cx);
        }
    }

    /// The backend stopped without saying why: say so, instead of leaving a frozen window. Not
    /// over a failure it did report, which says more.
    pub fn backend_gone(&self, cx: &mut App) {
        let mut chat = self.chat.borrow_mut();
        if !matches!(chat.screen(), Screen::Failed { .. }) {
            let update = chat.apply(Event::Failed {
                failure: Failure::Stopped,
                message: "b33p stopped working unexpectedly. Restart it to try again; the \
                          log says what went wrong."
                    .into(),
            });
            drop(chat);
            self.refresh(update, cx);
        }
    }

    /// Opens the chat `key` and carries on from there, for a click on its notification.
    pub fn open_chat(&self, key: String, cx: &mut App) {
        self.act(cx, |chat| {
            let command = chat.select(&ConversationId(key));
            let update = Update {
                conversations: true,
                messages: MessagesUpdate::Reset,
                ..Update::default()
            };
            (command, update)
        });
    }
}

/// An update that draws the whole window, e.g. a new one.
pub fn everything() -> Update {
    Update {
        screen: true,
        rows: true,
        ..Update::reset()
    }
}

fn deliver_notices(notifier: &Notifier, chat: &mut ChatState) {
    for notice in chat.take_notices() {
        match notice {
            Notice::Show {
                conversation,
                title,
                body,
            } => notifier.show(Notification {
                key: conversation.map(|id| id.0),
                title,
                body,
            }),
            Notice::Withdraw(id) => notifier.withdraw(&id.0),
        }
    }
}

impl Shared {
    /// A change to the conversation `id` from the chat list's menu (pin, mute, archive, …): it
    /// changes the chat list, and the messages only when the open chat closed with it
    /// (archiving or marking it unread does).
    pub fn chat_action(
        &self,
        cx: &mut App,
        id: &ConversationId,
        action: impl FnOnce(&mut ChatState, &ConversationId) -> Option<Command>,
    ) {
        self.act(cx, |chat| {
            let open = chat.selected().map(|c| c.id.clone());
            let command = action(chat, id);
            let update = if chat.selected().map(|c| &c.id) == open.as_ref() {
                Update {
                    conversations: true,
                    ..Update::default()
                }
            } else {
                Update::reset()
            };
            (command, update)
        });
    }

    /// Picks the chat `id`.
    pub fn select(&self, cx: &mut App, id: &ConversationId) {
        self.ui.borrow_mut().settings_open = false;
        self.act(cx, |chat| (chat.select(id), Update::reset()));
    }

    /// Whether the window is narrow and a chat is open: one pane at a time.
    pub fn narrow(&self) -> bool {
        self.ui.borrow().narrow
    }
}

impl Shared {
    /// Runs `f` with the window, if there is one: for what the window alone can do (a size).
    pub fn with_window<R>(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut gpui_kit::Window, &mut App) -> R,
    ) -> Option<R> {
        let handle = cx.windows().into_iter().next()?;
        handle.update(cx, |_, window, cx| f(window, cx)).ok()
    }

    /// Puts `text` in the composer, as typed.
    pub fn set_composer_text(&self, text: &str, cx: &mut App) {
        let Some(composer) = self.composer.borrow().as_ref().and_then(|c| c.upgrade()) else {
            return;
        };
        let text = text.to_owned();
        self.with_window(cx, |window, cx| {
            composer.update(cx, |composer, cx| composer.set_text(&text, window, cx));
        });
    }

    /// Puts the caret in the composer: after a chat opens, a reply or an edit starts. Done once
    /// the current event is handled, so that it may be asked for from inside a click.
    pub fn focus_composer(&self, cx: &mut App) {
        let me = self.me.clone();
        cx.defer(move |cx| {
            let Some(shared) = me.upgrade() else {
                return;
            };
            let Some(composer) = shared.composer.borrow().as_ref().and_then(|c| c.upgrade()) else {
                return;
            };
            shared.with_window(cx, |window, cx| {
                composer.update(cx, |composer, cx| composer.focus(window, cx));
            });
        });
    }

    /// What the composer's send button does; whether a command went out.
    pub fn send_composer(&self, cx: &mut App) -> bool {
        let Some(composer) = self.composer.borrow().as_ref().and_then(|c| c.upgrade()) else {
            return false;
        };
        self.with_window(cx, |window, cx| {
            composer.update(cx, |composer, cx| composer.send(window, cx))
        })
        .unwrap_or(false)
    }
}
