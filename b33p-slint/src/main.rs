// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use b33p_core::settings::{Settings, WidgetStyle};
use b33p_core::state::{AttachmentState, ChatState, MessagesUpdate, Notice, Screen, Update};
use b33p_core::{
    AttachmentContent, AttachmentId, Backend, Command, Commands, Config, ConversationId, Event,
    Failure, Picture, QrMatrix,
};
use b33p_notify::{Action, AppInfo, Notification, Notifier};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

#[cfg(all(feature = "harness", target_family = "unix"))]
mod agent;
// The harness drives the whole app over a socket: test builds only, never a release.
#[cfg(all(feature = "harness", not(debug_assertions)))]
compile_error!("the `harness` feature is for test builds: build without `--release`");
mod audio;
mod autostart;
mod capture;
mod clipboard;
mod emoji;
mod instance;
mod logging;
mod markdown;
mod motion;
mod rows;
mod shutdown;
mod tray;
#[cfg(test)]
mod ui_tests;

use markdown::{highlighted, rich_text};

/// The UI Slint compiles from `ui/app.slint` (see `build.rs`): in the native style for the app,
/// in Fluent for the tests, which run without Qt. `ui.rs` builds on whichever it is.
// Slint's code, not ours, discards what it likes.
#[allow(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]
mod generated {
    #[cfg(not(test))]
    include!(concat!(env!("OUT_DIR"), "/app_native.rs"));
    #[cfg(test)]
    include!(concat!(env!("OUT_DIR"), "/app_fluent.rs"));
}
mod ui;

const APP: AppInfo = AppInfo {
    id: "io.github.barafael.B33p",
    name: "b33p",
    #[cfg(windows)]
    icon: include_bytes!("../assets/b33p.ico"),
    #[cfg(not(windows))]
    icon: &[],
};

/// Everything that outlives a window: the chat state, the way to the backend, settings and
/// caches. UI callbacks ask `ChatState` what to do through [`Shared::act`].
pub struct Shared {
    chat: RefCell<ChatState>,
    images: Images,
    notifier: Notifier,
    commands: Commands,
    settings: RefCell<Settings>,
    /// The Qt style this process started with. Qt picks it once, so changing between the
    /// system style and Fusion waits for a restart.
    qt_style_at_start: QtStyle,
    ui: RefCell<Option<Rc<ui::Window>>>,
    tray: RefCell<Option<tray::Tray>>,
    /// Started for the tray (`--hidden`): the window stays closed unless the user is needed,
    /// e.g. for the password, until the chats are there.
    start_hidden: Cell<bool>,
    /// The desktop asks for reduced motion; see `motion.rs`.
    system_reduces_motion: Cell<bool>,
    /// What changed since the window was last brought in line with the state; drawn once
    /// per turn of the event loop, however many actions and events a turn brings.
    pending: RefCell<Option<Update>>,
    /// Whether something other than events (an action, a timer) changed the chat list since
    /// it was last drawn: then every row is drawn, since only events name the chats they
    /// change (`ChatState::take_changed_chats`).
    whole_list: Cell<bool>,
    /// Whether the window counts as focused, as the harness says (`SetWindowFocused`), in
    /// place of the window's own: in the test box no window manager ever activates it.
    focus_override: Cell<Option<bool>>,
    /// Redraws when someone typing next counts as stopped, since no event says so.
    typing_timer: slint::Timer,
    /// Redraws the chat list at midnight, when its times change ("14:05" becomes "Mon").
    midnight_timer: slint::Timer,
    /// Redraws the open chat's messages once a minute while some of them disappear.
    expiry_timer: slint::Timer,
    /// Audio and voice messages playing here.
    player: RefCell<audio::Player>,
    /// Asked to play once downloaded.
    pending_play: RefCell<Option<AttachmentId>>,
    /// Moves the progress of what plays along.
    playback_timer: slint::Timer,
    /// For callbacks that outlive a call, like `typing_timer`'s.
    me: std::rc::Weak<Shared>,
    /// Answers the agent harness's socket (`--features harness`).
    #[cfg(all(feature = "harness", target_family = "unix"))]
    agent: agent::Handle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QtStyle {
    System,
    Fusion,
}

impl QtStyle {
    fn of(style: WidgetStyle) -> Self {
        match style {
            WidgetStyle::Native => Self::System,
            WidgetStyle::Fusion => Self::Fusion,
        }
    }
}

impl Shared {
    fn ui(&self) -> Option<Rc<ui::Window>> {
        self.ui.borrow().clone()
    }

    /// Runs a UI action against the chat state, sends the command it produces and redraws what
    /// it changed. Returns whether a command was sent.
    fn act(&self, action: impl FnOnce(&mut ChatState) -> (Option<Command>, Update)) -> bool {
        let (command, update) = action(&mut self.chat.borrow_mut());
        let sent = command.is_some();
        if let Some(command) = command {
            self.commands.send(command);
        }
        self.refresh(update);
        sent
    }

    /// Changes the state; what shows follows on its own.
    fn edit(&self, action: impl FnOnce(&mut ChatState)) {
        self.act(|chat| {
            action(chat);
            (None, Update::default())
        });
    }

    /// Sends the command the state answers with, if any.
    fn command(&self, action: impl FnOnce(&mut ChatState) -> Option<Command>) {
        self.act(|chat| (action(chat), Update::default()));
    }

    /// Redraws what the state says changed.
    fn redraw(&self, action: impl FnOnce(&mut ChatState) -> Update) {
        self.act(|chat| (None, action(chat)));
    }

    /// Brings the window in line with the state, at the next turn of the event loop: a row
    /// being made asks for a picture, a key is typed, a batch of events comes, and each
    /// alone redrew before.
    fn refresh(&self, update: Update) {
        if update.conversations {
            self.whole_list.set(true);
        }
        self.refresh_after_events(update);
    }

    /// [`Shared::refresh`] for what events changed, which name the chats they change.
    fn refresh_after_events(&self, update: Update) {
        let mut pending = self.pending.borrow_mut();
        match pending.take() {
            Some(earlier) => {
                *pending = Some(earlier.merge(update));
            }
            None => {
                *pending = Some(update);
                let me = self.me.clone();
                slint::Timer::single_shot(Duration::ZERO, move || {
                    if let Some(shared) = me.upgrade() {
                        shared.flush();
                    }
                });
            }
        }
    }

    fn flush(&self) {
        let Some(update) = self.pending.borrow_mut().take() else {
            return;
        };
        let Some(ui) = self.ui() else { return };
        let mut chat = self.chat.borrow_mut();
        ui.render(&chat, &self.images, update, self.whole_list.take());
        #[cfg(all(feature = "harness", target_family = "unix"))]
        self.agent
            .publish(&chat, &self.settings.borrow(), self.ui().as_deref());
        if self.start_hidden.get() {
            let screen = chat.screen();
            // Not for a keyring that isn't unlocked yet: at login it usually is a moment later,
            // and it is tried again by itself. Opened from the tray, the window says why.
            let needs_user = match screen {
                Screen::Password { .. } | Screen::Link(_) => true,
                Screen::Failed { failure, .. } => *failure != Failure::KeyringUnavailable,
                Screen::Syncing(_) | Screen::Chats => false,
            };
            if needs_user || chat.error().is_some() {
                self.stop_hiding();
            } else if matches!(screen, Screen::Chats) {
                self.start_hidden.set(false);
            }
        }
        if let Some(tray) = &*self.tray.borrow() {
            tray.set_unread(chat.unread_total().try_into().unwrap_or(i32::MAX));
        }
        deliver_notices(&self.notifier, &mut chat);
        for command in chat.take_commands() {
            self.commands.send(command);
        }
        match chat.typing_expires() {
            Some(at) => {
                let wait = at.saturating_sub(b33p_core::now_millis()) + 50;
                let me = self.me.clone();
                self.typing_timer.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(wait),
                    move || {
                        if let Some(shared) = me.upgrade() {
                            shared.refresh(Update {
                                conversations: true,
                                ..Update::default()
                            });
                        }
                    },
                );
            }
            None => self.typing_timer.stop(),
        }
        // The times in the list just drawn hold until midnight (a second more, to be past it),
        // and so do the days over the messages ("Today"). Counted anew whenever the list is
        // drawn: the timer's clock stops while the computer sleeps.
        if update.conversations {
            let me = self.me.clone();
            self.midnight_timer.start(
                slint::TimerMode::SingleShot,
                b33p_core::state::until_midnight() + Duration::from_secs(1),
                move || {
                    if let Some(shared) = me.upgrade() {
                        shared.refresh(Update {
                            conversations: true,
                            rows: true,
                            ..Update::default()
                        });
                    }
                },
            );
        }
        // What disappears says how long it has left ("5m"): counted down once a minute, while
        // the open chat has any.
        if chat.shows_expiring() {
            if !self.expiry_timer.running() {
                let me = self.me.clone();
                self.expiry_timer.start(
                    slint::TimerMode::Repeated,
                    Duration::from_secs(60),
                    move || {
                        if let Some(shared) = me.upgrade() {
                            shared.refresh(Update {
                                rows: true,
                                ..Update::default()
                            });
                        }
                    },
                );
            }
        } else {
            self.expiry_timer.stop();
        }
        // A download asked to play has arrived.
        let pending = self.pending_play.borrow().clone();
        if let Some(id) = pending {
            match chat.attachment(&id) {
                Some(AttachmentState::Ready(AttachmentContent::Audio(audio))) => {
                    let audio = audio.clone();
                    self.pending_play.replace(None);
                    drop(chat);
                    self.start_playing(id, audio);
                }
                Some(AttachmentState::Failed(_)) => {
                    self.pending_play.replace(None);
                }
                _ => {}
            }
        }
    }

    /// Plays an audio attachment, or pauses or carries on if it is the one playing.
    fn toggle_playback(&self, id: AttachmentId) {
        let current = self.player.borrow_mut().state().map(|s| s.id.clone());
        // Whatever was waiting to download is no longer wanted.
        self.pending_play.replace(None);
        if current.as_ref() == Some(&id) {
            self.player.borrow_mut().toggle();
            self.show_playback();
            return;
        }
        let ready = match self.chat.borrow().attachment(&id) {
            Some(AttachmentState::Ready(AttachmentContent::Audio(audio))) => Some(audio.clone()),
            _ => None,
        };
        match ready {
            Some(audio) => self.start_playing(id, audio),
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
            }
        }
    }

    /// Goes to `fraction` of the audio playing, if `id` is what plays; otherwise starts it
    /// there.
    fn seek_playback(&self, id: AttachmentId, fraction: f32) {
        let playing = self
            .player
            .borrow_mut()
            .state()
            .is_some_and(|state| state.id == &id);
        if !playing {
            self.toggle_playback(id.clone());
        }
        let result = self.player.borrow_mut().seek(&id, fraction);
        if let Err(error) = result {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't seek in audio");
        }
        self.show_playback();
    }

    fn start_playing(&self, id: AttachmentId, audio: Arc<[u8]>) {
        if let Err(error) = self.player.borrow_mut().play(id, audio) {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't play audio");
            // The audio itself, which playing again won't change.
            let update = self
                .chat
                .borrow_mut()
                .apply(Event::Error(b33p_core::Error::refused(error)));
            self.refresh(update);
            return;
        }
        let me = self.me.clone();
        self.playback_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(200),
            move || {
                if let Some(shared) = me.upgrade() {
                    shared.show_playback();
                }
            },
        );
        self.show_playback();
    }

    /// Shows how far playing got; stops the timer once nothing plays.
    fn show_playback(&self) {
        let mut player = self.player.borrow_mut();
        let state = player.state();
        if state.is_none() {
            self.playback_timer.stop();
        }
        let Some(ui) = self.ui() else { return };
        match state {
            Some(state) => {
                let progress = state.length.map_or(0., |length| {
                    (state.position.as_secs_f32() / length.as_secs_f32().max(0.001)).min(1.)
                });
                let time = match state.length {
                    Some(length) => {
                        format!(
                            "{} / {}",
                            audio::clock(state.position),
                            audio::clock(length)
                        )
                    }
                    None => audio::clock(state.position),
                };
                ui.show_playback(&state.id.0, state.paused, progress, &time);
            }
            None => ui.show_playback("", false, 0., ""),
        }
    }

    /// Closing the window leaves b33p running in the tray, to keep receiving messages, unless
    /// that is turned off or nothing shows tray icons.
    fn close_requested(&self) -> slint::CloseRequestResponse {
        if !self.in_tray() {
            // Fails only once the event loop has stopped: quitting already.
            slint::quit_event_loop().ok();
        }
        slint::CloseRequestResponse::HideWindow
    }

    /// Shows the window and brings it to the front. `token` is a Wayland activation token from
    /// whatever asked (a launcher, a notification), which lets the compositor give b33p focus.
    fn activate(&self, token: Option<String>) {
        let Some(ui) = self.ui() else { return };
        #[cfg(all(unix, not(target_os = "macos")))]
        if token.is_none() && ui.visible() && !ui.active() {
            // Without a token, KWin (like other compositors) keeps an open window where it is,
            // but it does focus one that appears. So the window appears anew, where the
            // compositor places it.
            ui.hide();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(token) = token {
            // Qt's Wayland plugin takes the token from here when the window asks to be
            // activated, and clears it; KDE's KWindowSystem hands tokens to Qt the same way.
            // SAFETY: setting the environment races with C code reading it on another thread
            // (Rust's own readers are locked). Accepted, as in KWindowSystem: nothing else in
            // b33p reads this variable, and the moment is short.
            unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", token) };
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        drop(token);
        ui.raise();
    }

    /// Opens the window of a start into the tray after all.
    fn stop_hiding(&self) {
        self.start_hidden.set(false);
        if let Some(ui) = self.ui()
            && let Err(error) = ui.show()
        {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to show the window");
        }
    }

    fn in_tray(&self) -> bool {
        self.settings.borrow().close_to_tray && tray::host_present()
    }

    /// Why the chosen style isn't showing yet, if it isn't.
    fn style_note(&self) -> String {
        if QtStyle::of(self.settings.borrow().style) != self.qt_style_at_start {
            "Takes effect when b33p restarts.".into()
        } else {
            String::new()
        }
    }

    /// Animates, or not, as the settings and the desktop say.
    fn apply_motion(&self) {
        let enabled = self
            .settings
            .borrow()
            .animations
            .enabled(self.system_reduces_motion.get());
        if let Some(ui) = self.ui() {
            ui.set_animations(enabled);
        }
    }

    /// Names this computer anew, for the phone's list of linked devices; empty goes back to
    /// the default name. Returns whether the backend was told (the name changed).
    fn rename_device(&self, name: &str) -> bool {
        let mut settings = self.settings.borrow().clone();
        let name = b33p_core::settings::clean_device_name(name);
        settings.device_name = (!name.is_empty()
            && name != b33p_core::settings::default_device_name())
        .then_some(name);
        let name = settings.device_name();
        if let Some(ui) = self.ui() {
            ui.set_device_name(&name);
        }
        if settings == *self.settings.borrow() {
            return false;
        }
        if let Err(error) = settings.save() {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to save settings");
        }
        self.settings.replace(settings);
        self.commands.send(Command::SetDeviceName(name));
        true
    }

    /// The backend's, after the phone changed the account's settings: shown and saved.
    fn take_preferences(self: &Rc<Self>, preferences: b33p_core::Preferences) {
        let mut settings = self.settings.borrow().clone();
        settings.preferences = preferences;
        if let Some(window) = self.ui() {
            window.show_settings(&settings, &self.style_note());
        }
        self.apply_settings(settings);
    }

    fn apply_settings(self: &Rc<Self>, settings: Settings) {
        if let Err(error) = settings.save() {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to save settings");
        }
        self.settings.replace(settings.clone());
        self.apply_motion();
        self.commands
            .send(Command::Preferences(settings.preferences));
        if let Some(tray) = &*self.tray.borrow() {
            tray.set_shown(settings.close_to_tray);
        }
        let rebuild =
            self.images.avatars_enabled.replace(settings.show_avatars) != settings.show_avatars;
        self.act(|chat| {
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
}

/// An update that redraws the whole window, e.g. a new one.
fn everything() -> Update {
    Update {
        screen: true,
        rows: true,
        ..Update::reset()
    }
}

/// What `--version` and `--help` print, answered before anything starts. (A Windows release
/// has no console to print to.)
fn command_line_answer() -> Option<String> {
    match std::env::args().nth(1)?.as_str() {
        "--version" | "-V" => Some(format!("b33p {}", env!("CARGO_PKG_VERSION"))),
        "--unregister" => {
            if let Err(error) = autostart::set(false) {
                println!("Starting with the system wasn't turned off: {error}");
            }
            b33p_notify::unregister(&APP);
            Some(format!(
                "b33p no longer starts with the system. Its messages and keys stay in {}, \
                 its log in {}; delete those too to remove everything.",
                b33p_core::settings::data_dir()
                    .map_or_else(String::new, |dir| dir.display().to_string()),
                b33p_core::settings::log_dir()
                    .map_or_else(String::new, |dir| dir.display().to_string()),
            ))
        }
        "--help" | "-h" => Some(format!(
            "b33p {}: a Signal client for the desktop\n\n\
             Usage: b33p [--hidden]\n\n\
             \x20 --hidden      start in the tray, as when starting with the system\n\
             \x20 --version     print the version\n\
             \x20 --unregister  stop starting with the system, and forget the notification\n\
             \x20               registration (Windows), before deleting b33p\n\n\
             RUST_LOG raises how much is logged. Logs go to {}.\n\
             {}",
            env!("CARGO_PKG_VERSION"),
            b33p_core::settings::log_dir()
                .map_or_else(|| "stderr".into(), |dir| dir.display().to_string()),
            env!("CARGO_PKG_REPOSITORY"),
        )),
        _ => None,
    }
}

fn main() -> Result<(), slint::PlatformError> {
    if let Some(answer) = command_line_answer() {
        println!("{answer}");
        return Ok(());
    }
    let settings = Settings::load();
    // Qt reads its style once, when it starts, so this has to come first.
    let qt_style_at_start = if settings.style == WidgetStyle::Fusion
        && std::env::var_os("QT_STYLE_OVERRIDE").is_none()
    {
        // SAFETY: nothing else runs yet; the backend and Qt start below.
        unsafe { std::env::set_var("QT_STYLE_OVERRIDE", "Fusion") };
        QtStyle::Fusion
    } else {
        QtStyle::System
    };

    logging::init();

    // Before the backend opens the database.
    let instance = match instance::claim(Backend::mock_requested()) {
        instance::Claim::First(primary) => Some(primary),
        instance::Claim::Second => {
            tracing::info!("b33p is already running; showing it instead");
            return Ok(());
        }
        instance::Claim::Unchecked => None,
    };
    let shutdown = shutdown::watch();
    clipboard::clean();

    let config = Config {
        device_name: settings.device_name(),
        preferences: settings.preferences,
    };
    let backend = Backend::from_env(config);
    let (notifier, notifier_actions) = Notifier::new(&APP);
    let images = Images::default();
    images.avatars_enabled.set(settings.show_avatars);
    let mut chat = ChatState::default();
    chat.set_notifications(settings.notifications, settings.notification_content);
    let shared = Rc::new_cyclic(|me| Shared {
        chat: RefCell::new(chat),
        images,
        notifier,
        commands: backend.commands(),
        settings: RefCell::new(settings.clone()),
        qt_style_at_start,
        ui: RefCell::default(),
        tray: RefCell::default(),
        start_hidden: Cell::new(false),
        system_reduces_motion: Cell::new(false),
        pending: RefCell::default(),
        whole_list: Cell::new(false),
        focus_override: Cell::new(None),
        typing_timer: slint::Timer::default(),
        midnight_timer: slint::Timer::default(),
        expiry_timer: slint::Timer::default(),
        player: RefCell::default(),
        pending_play: RefCell::default(),
        playback_timer: slint::Timer::default(),
        me: me.clone(),
        #[cfg(all(feature = "harness", target_family = "unix"))]
        agent: agent::Handle::default(),
    });
    let ui = ui::create(&shared)?;
    shared.ui.replace(Some(ui.clone()));
    shared.apply_motion();
    // The desktop's reduced-motion preference, as it is and as it changes.
    let system_motion = motion::watch();
    slint::spawn_local({
        let shared = shared.clone();
        let reduced = system_motion.reduced.clone();
        async move {
            while let Ok(reduced) = reduced.recv().await {
                tracing::debug!(reduced, "the desktop's reduced-motion preference");
                shared.system_reduces_motion.set(reduced);
                shared.apply_motion();
            }
        }
    })
    .expect("the event loop is set up");
    tray::watch_host();
    shared.tray.replace(Some(tray::create(&shared)?));
    let hidden = std::env::args().any(|arg| arg == autostart::HIDDEN) && settings.close_to_tray;
    shared.start_hidden.set(hidden);
    shared.refresh(everything());
    #[cfg(all(feature = "harness", target_family = "unix"))]
    agent::start(&shared);
    if !hidden {
        ui.show()?;
    } else if !tray::host_present() {
        tray::open_unless_host_appears(&shared, Duration::from_secs(10));
    }

    // Starting b33p again shows this one.
    if let Some(instance) = &instance {
        let activations = instance.activations.clone();
        let shared = shared.clone();
        slint::spawn_local(async move {
            while let Ok(token) = activations.recv().await {
                tracing::info!(token = token.is_some(), "started again; showing the window");
                shared.activate(token);
            }
        })
        .expect("the event loop is set up");
    }

    // What the user did with a notification: open its chat, or send the answer typed into it.
    slint::spawn_local({
        let shared = shared.clone();
        async move {
            while let Ok(action) = notifier_actions.recv().await {
                match action {
                    Action::Open {
                        key,
                        activation_token,
                    } => {
                        if let Some(key) = key {
                            shared.act(|chat| {
                                let command = chat.select(&ConversationId(key));
                                let update = Update {
                                    conversations: true,
                                    messages: MessagesUpdate::Reset,
                                    ..Update::default()
                                };
                                (command, update)
                            });
                        }
                        shared.activate(activation_token);
                    }
                    Action::Reply { key, text } => {
                        shared.act(|chat| {
                            let (command, update) = chat.reply(&ConversationId(key), &text);
                            let read = Update {
                                conversations: true,
                                ..Update::default()
                            };
                            (command, update.merge(read))
                        });
                    }
                }
            }
        }
    })
    .expect("the event loop is set up");

    // Await backend events on the UI thread and apply each burst (e.g. the initial sync) as
    // one batch, so the UI refreshes once per batch. Nothing polls: idle means idle. A long
    // burst (catching up after days away) is cut into batches of a few milliseconds, with the
    // window drawn between them, so it doesn't freeze meanwhile.
    let events = backend.events();
    slint::spawn_local({
        let shared = shared.clone();
        async move {
            const BATCH_TIME: Duration = Duration::from_millis(8);
            while let Ok(first) = events.recv().await {
                let started = std::time::Instant::now();
                let batch = std::iter::once(first).chain(std::iter::from_fn(|| {
                    (started.elapsed() < BATCH_TIME)
                        .then(|| events.try_recv().ok())
                        .flatten()
                }));
                // Hidden, the window may still count as active to the toolkit.
                let focused = shared
                    .focus_override
                    .get()
                    .unwrap_or_else(|| shared.ui().is_some_and(|ui| ui.visible() && ui.active()));
                let mut preferences = None;
                let update = {
                    let mut chat = shared.chat.borrow_mut();
                    chat.set_window_focused(focused);
                    chat.apply_all(batch.inspect(|event| match event {
                        // Commands log their own failures in the backend.
                        Event::Error(error) => {
                            let message = b33p_core::scrub::scrub(&error.message);
                            match error.kind {
                                b33p_core::ErrorKind::Unreachable => tracing::warn!("{message}"),
                                _ => tracing::error!("{message}"),
                            }
                        }
                        Event::Preferences(taken) => preferences = Some(*taken),
                        _ => {}
                    }))
                };
                shared.refresh_after_events(update);
                if let Some(preferences) = preferences {
                    shared.take_preferences(preferences);
                }
                if !events.is_empty() {
                    next_turn().await;
                }
            }
            // The channel only closes early if the backend thread died without reporting why
            // (a panic); say so instead of leaving a frozen window. Not over a failure it did
            // report, which says more.
            let mut chat = shared.chat.borrow_mut();
            if !matches!(chat.screen(), Screen::Failed { .. }) {
                let update = chat.apply(Event::Failed {
                    failure: Failure::Stopped,
                    message: "b33p stopped working unexpectedly. Restart it to try again; the \
                              log says what went wrong."
                        .into(),
                });
                drop(chat);
                shared.refresh(update);
            }
        }
    })
    .expect("the event loop is set up");

    // Until "Quit" in the tray, or the window closes where there is no tray (`close_requested`).
    // Not just until no window is shown: the window may be hidden in the tray, or for a moment
    // while it is shown anew (`activate`).
    let result = slint::run_event_loop_until_quit();
    // Waits for the backend to close the database, which must happen before the process exits.
    drop(backend);
    // Pasted pictures have been sent or given up by now.
    clipboard::clean();
    shutdown.closed();
    // Only now may another b33p start.
    drop(instance);
    result
}

/// Waits for the event loop to come round once more: what it has queued meanwhile (the redraw
/// that `Shared::refresh` asked for) runs first.
async fn next_turn() {
    let (done, turned) = async_channel::bounded::<()>(1);
    slint::Timer::single_shot(Duration::ZERO, move || {
        // The only message: there is room for it, and we are waiting.
        done.try_send(()).ok();
    });
    // Fails only if the timer was dropped unrun, which is a turn too.
    turned.recv().await.ok();
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

/// Pictures converted to Slint images once, so re-rendering lists neither copies pixels again
/// nor makes unchanged rows look changed.
#[derive(Default)]
pub struct Images {
    avatars: ImageCache<ConversationId>,
    attachments: ImageCache<AttachmentId>,
    /// Decoded BlurHashes, by hash.
    blurs: RefCell<HashMap<String, Image>>,
    /// The "Profile pictures" setting.
    avatars_enabled: Cell<bool>,
    /// Message text as styled text, by what it says and how: making it means writing and
    /// parsing Markdown, and rows are made again whenever they are redrawn.
    texts: RefCell<HashMap<u64, slint::StyledText>>,
    /// [`ChatState::downloads_forgotten`] when `attachments` last let go of what it forgot.
    forgotten: Cell<u64>,
}

impl Images {
    /// A small blurred picture for a BlurHash, with the shape of `width` × `height`; empty if
    /// the hash is invalid.
    fn blur(&self, hash: &str, width: u32, height: u32) -> Image {
        if let Some(image) = self.blurs.borrow().get(hash) {
            return image.clone();
        }
        // Blurred anyway: a few dozen pixels are plenty, scaled up by the UI. The sizes are
        // the sender's, so they are taken wide enough not to overflow.
        let (width, height) = (u64::from(width), u64::from(height));
        let (w, h) = if width >= height {
            (32, (32 * height / width.max(1)).clamp(4, 32) as u32)
        } else {
            ((32 * width / height.max(1)).clamp(4, 32) as u32, 32)
        };
        // A hash is base-83 text, 6 to 166 characters long; nothing else is worth decoding.
        let plausible = hash.is_ascii() && (6..=166).contains(&hash.len());
        let image = match plausible.then(|| blurhash::decode(hash, w, h, 1.0)) {
            Some(Ok(rgba)) => Image::from_rgba8(SharedPixelBuffer::clone_from_slice(&rgba, w, h)),
            _ => Image::default(),
        };
        self.blurs
            .borrow_mut()
            .insert(hash.to_owned(), image.clone());
        image
    }

    /// Lets go of the pictures the state let go of (to keep memory in bounds), which would
    /// otherwise stay here.
    fn forget_gone(&self, chat: &ChatState) {
        if self.forgotten.replace(chat.downloads_forgotten()) == chat.downloads_forgotten() {
            return;
        }
        self.attachments.0.borrow_mut().retain(|id, (picture, _)| {
            matches!(
                chat.attachment(id),
                Some(AttachmentState::Ready(AttachmentContent::Picture(kept))) if Arc::ptr_eq(kept, picture)
            )
        });
    }

    /// `body` as styled text; see [`markdown::rich_text`].
    fn text(
        &self,
        body: &str,
        styles: &[b33p_core::StyleRange],
        mentions: &[b33p_core::Mention],
        revealed: bool,
    ) -> slint::StyledText {
        /// About a long chat's worth; beyond it, the cache starts again.
        const KEPT: usize = 4096;
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut hasher);
        revealed.hash(&mut hasher);
        for style in styles {
            (style.start, style.end, style.style).hash(&mut hasher);
        }
        for mention in mentions {
            (mention.start, mention.end, &mention.service_id).hash(&mut hasher);
        }
        let key = hasher.finish();
        if let Some(text) = self.texts.borrow().get(&key) {
            return text.clone();
        }
        let text = rich_text(body, styles, mentions, revealed);
        let mut texts = self.texts.borrow_mut();
        if texts.len() >= KEPT {
            texts.clear();
        }
        texts.insert(key, text.clone());
        text
    }

    /// Empty when pictures are turned off. Round: cheaper than clipping it at every draw.
    fn avatar(&self, id: &ConversationId, avatar: &Arc<Picture>) -> Image {
        if !self.avatars_enabled.get() {
            return Image::default();
        }
        self.avatars.get(id, avatar, round_image)
    }
}

struct ImageCache<K>(RefCell<HashMap<K, (Arc<Picture>, Image)>>);

impl<K> Default for ImageCache<K> {
    fn default() -> Self {
        Self(RefCell::default())
    }
}

impl<K: std::hash::Hash + Eq + Clone> ImageCache<K> {
    fn get(&self, key: &K, picture: &Arc<Picture>, convert: fn(&Picture) -> Image) -> Image {
        let mut images = self.0.borrow_mut();
        if let Some((cached, image)) = images.get(key)
            && Arc::ptr_eq(cached, picture)
        {
            return image.clone();
        }
        let image = convert(picture);
        images.insert(key.clone(), (picture.clone(), image.clone()));
        image
    }
}

/// A picture from the backend, for Slint.
pub(crate) fn picture_image(picture: &Picture) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &picture.rgba,
        picture.width,
        picture.height,
    ))
}

/// The picture inside the largest circle that fits it, the rest see-through.
fn round_image(picture: &Picture) -> Image {
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &picture.rgba,
        picture.width,
        picture.height,
    );
    let (width, height) = (picture.width as f32, picture.height as f32);
    let (cx, cy) = (width / 2.0, height / 2.0);
    let radius = width.min(height) / 2.0;
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (
            (i as u32 % picture.width) as f32 + 0.5,
            (i as u32 / picture.width) as f32 + 0.5,
        );
        let outside = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - radius;
        // A pixel-wide soft edge, as a clip would draw it.
        let cover = (0.5 - outside).clamp(0.0, 1.0);
        if cover < 1.0 {
            pixel.a = (pixel.a as f32 * cover) as u8;
        }
    }
    Image::from_rgba8(pixels)
}

/// Opens a link from a message in the browser. Only web addresses: links come from message
/// text, so nothing else (files, other apps' schemes) is opened.
fn open_link(link: &str) {
    if !(link.starts_with("https://") || link.starts_with("http://")) {
        return;
    }
    if let Err(error) = open::that_detached(link) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "couldn't open a link");
    }
}

fn qr_image(qr: &QrMatrix) -> Image {
    // One pixel per module; the UI scales it up with `image-rendering: pixelated`.
    let (side, pixels) = qr.to_rgba(1);
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, side as u32, side as u32);
    Image::from_rgba8(buffer)
}
