// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use langis_core::settings::{Settings, WidgetStyle};
use langis_core::state::{AttachmentState, ChatState, MessagesUpdate, Notice, Screen, Update};
use langis_core::{
    AttachmentContent, AttachmentId, Backend, Command, Commands, Config, ConversationId, Event,
    Picture, QrMatrix, Style, StyleRange, format::find_url,
};
use langis_notify::{Action, AppInfo, Notification, Notifier};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

mod audio;
mod autostart;
mod clipboard;
mod instance;
mod logging;
mod shutdown;
mod tray;
#[cfg(test)]
mod ui_tests;

/// The UI, compiled once per Slint widget style (see `build.rs`). Slint fixes the style when it
/// compiles, so switching styles means switching between these: each includes `ui.rs`.
mod styles {
    pub mod native {
        include!(concat!(env!("OUT_DIR"), "/app_native.rs"));
        include!("ui.rs");
    }
    pub mod fluent {
        include!(concat!(env!("OUT_DIR"), "/app_fluent.rs"));
        include!("ui.rs");
    }
    pub mod material {
        include!(concat!(env!("OUT_DIR"), "/app_material.rs"));
        include!("ui.rs");
    }
    pub mod cupertino {
        include!(concat!(env!("OUT_DIR"), "/app_cupertino.rs"));
        include!("ui.rs");
    }
    pub mod cosmic {
        include!(concat!(env!("OUT_DIR"), "/app_cosmic.rs"));
        include!("ui.rs");
    }

    use std::rc::Rc;

    use langis_core::settings::WidgetStyle;

    /// A window in `style`, not shown yet. Fusion is the native (Qt) UI in Qt's Fusion style.
    pub fn create(
        style: WidgetStyle,
        shared: &Rc<super::Shared>,
    ) -> Result<Rc<dyn super::Ui>, slint::PlatformError> {
        match style {
            WidgetStyle::Native | WidgetStyle::Fusion => native::create(shared),
            WidgetStyle::Fluent => fluent::create(shared),
            WidgetStyle::Material => material::create(shared),
            WidgetStyle::Cupertino => cupertino::create(shared),
            WidgetStyle::Cosmic => cosmic::create(shared),
        }
    }
}

const APP: AppInfo = AppInfo {
    id: "org.langis.Langis",
    name: "langis",
};

/// A window in one of the styles; see `ui.rs`.
pub trait Ui {
    /// Brings the window in line with `chat`, touching only what `update` says changed.
    fn render(&self, chat: &ChatState, images: &Images, update: Update);
    fn show(&self) -> Result<(), slint::PlatformError>;
    fn hide(&self);
    /// Shows the window and brings it to the front, e.g. after a notification or the tray icon
    /// was clicked. See [`Shared::activate`].
    fn raise(&self);
    fn visible(&self) -> bool;
    /// Whether the window is the active one.
    fn active(&self) -> bool;
    fn geometry(&self) -> (slint::PhysicalPosition, slint::PhysicalSize);
    fn take_geometry(&self, geometry: (slint::PhysicalPosition, slint::PhysicalSize));
    fn open_settings(&self);
    /// What plays: the attachment (empty for nothing), paused, progress 0…1, and the time.
    fn show_playback(&self, id: &str, paused: bool, progress: f32, time: &str);
}

/// Everything that outlives a window: the chat state, the way to the backend, settings and
/// caches. UI callbacks ask `ChatState` what to do through [`Shared::act`].
pub struct Shared {
    chat: RefCell<ChatState>,
    images: Images,
    notifier: Notifier,
    commands: Commands,
    settings: RefCell<Settings>,
    device_name: String,
    /// The Qt style this process started with. Qt picks it once, so changing between the
    /// system style and Fusion waits for a restart.
    qt_style_at_start: QtStyle,
    ui: RefCell<Option<Rc<dyn Ui>>>,
    tray: RefCell<Option<tray::Tray>>,
    /// Started for the tray (`--hidden`): the window stays closed unless the user is needed,
    /// e.g. for the password, until the chats are there.
    start_hidden: Cell<bool>,
    /// Redraws when someone typing next counts as stopped, since no event says so.
    typing_timer: slint::Timer,
    /// Audio and voice messages playing here.
    player: RefCell<audio::Player>,
    /// Asked to play once downloaded.
    pending_play: RefCell<Option<AttachmentId>>,
    /// Moves the progress of what plays along.
    playback_timer: slint::Timer,
    /// For callbacks that outlive a call, like `typing_timer`'s.
    me: std::rc::Weak<Shared>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QtStyle {
    System,
    Fusion,
}

impl QtStyle {
    fn of(style: WidgetStyle) -> Option<Self> {
        match style {
            WidgetStyle::Native => Some(Self::System),
            WidgetStyle::Fusion => Some(Self::Fusion),
            _ => None,
        }
    }
}

impl Shared {
    fn ui(&self) -> Option<Rc<dyn Ui>> {
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

    fn refresh(&self, update: Update) {
        let Some(ui) = self.ui() else { return };
        let mut chat = self.chat.borrow_mut();
        ui.render(&chat, &self.images, update);
        if self.start_hidden.get() {
            let screen = chat.screen();
            if matches!(screen, Screen::Password { .. } | Screen::Link(_)) || chat.error().is_some()
            {
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
                let wait = at.saturating_sub(langis_core::now_millis()) + 50;
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

    fn start_playing(&self, id: AttachmentId, audio: Arc<[u8]>) {
        if let Err(error) = self.player.borrow_mut().play(id, audio) {
            tracing::warn!(%error, "can't play audio");
            let update = self.chat.borrow_mut().apply(Event::Error(error));
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

    /// Closing the window leaves langis running in the tray, to keep receiving messages, unless
    /// that is turned off or nothing shows tray icons.
    fn close_requested(&self) -> slint::CloseRequestResponse {
        if !self.in_tray() {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    }

    /// Shows the window and brings it to the front. `token` is a Wayland activation token from
    /// whatever asked (a launcher, a notification), which lets the compositor give langis focus.
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
            // langis reads this variable, and the moment is short.
            unsafe { std::env::set_var("XDG_ACTIVATION_TOKEN", token) };
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        let _ = token;
        ui.raise();
    }

    /// Opens the window of a start into the tray after all.
    fn stop_hiding(&self) {
        self.start_hidden.set(false);
        if let Some(ui) = self.ui()
            && let Err(error) = ui.show()
        {
            tracing::warn!(%error, "failed to show the window");
        }
    }

    fn in_tray(&self) -> bool {
        self.settings.borrow().close_to_tray && tray::host_present()
    }

    /// Why the chosen style isn't showing yet, if it isn't.
    fn style_note(&self) -> String {
        match QtStyle::of(self.settings.borrow().style) {
            Some(qt) if qt != self.qt_style_at_start => "Takes effect when langis restarts.".into(),
            _ => String::new(),
        }
    }

    fn apply_settings(self: &Rc<Self>, settings: Settings) {
        if let Err(error) = settings.save() {
            tracing::warn!(%error, "failed to save settings");
        }
        let previous = self.settings.replace(settings.clone());
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
        // Native and Fusion are the same window; Qt switches between them on restart.
        let window_of = |style| {
            if QtStyle::of(style).is_some() {
                WidgetStyle::Native
            } else {
                style
            }
        };
        if window_of(previous.style) != window_of(settings.style) {
            // Not from inside the old window's own callback, which is running right now.
            let this = self.clone();
            let _ = slint::spawn_local(async move { this.switch_style(settings.style) });
        }
    }

    /// Replaces the window with one in `style`, in the same place and showing the same things.
    fn switch_style(self: &Rc<Self>, style: WidgetStyle) {
        let new = match styles::create(style, self) {
            Ok(new) => new,
            Err(error) => {
                tracing::warn!(%error, "failed to switch the style");
                return;
            }
        };
        let old = self.ui.replace(Some(new.clone()));
        if let Some(old) = &old {
            new.take_geometry(old.geometry());
        }
        new.open_settings();
        self.refresh(everything());
        if let Err(error) = new.show() {
            tracing::warn!(%error, "failed to show the window");
        }
        if let Some(old) = old {
            old.hide();
        }
    }
}

/// An update that redraws the whole window, e.g. a new one.
fn everything() -> Update {
    Update {
        screen: true,
        conversations: true,
        messages: MessagesUpdate::Reset,
        rows: true,
    }
}

fn main() -> Result<(), slint::PlatformError> {
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

    tracing_subscriber::fmt()
        .with_env_filter(logging::from_env())
        .init();

    // Before the backend opens the database.
    let instance = match instance::claim(Backend::mock_requested()) {
        instance::Claim::First(primary) => Some(primary),
        instance::Claim::Second => {
            tracing::info!("langis is already running; showing it instead");
            return Ok(());
        }
        instance::Claim::Unchecked => None,
    };
    let shutdown = shutdown::watch();
    clipboard::clean();

    let config = Config::default();
    let device_name = config.device_name.clone();
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
        device_name,
        qt_style_at_start,
        ui: RefCell::default(),
        tray: RefCell::default(),
        start_hidden: Cell::new(false),
        typing_timer: slint::Timer::default(),
        player: RefCell::default(),
        pending_play: RefCell::default(),
        playback_timer: slint::Timer::default(),
        me: me.clone(),
    });
    shared
        .commands
        .send(Command::Preferences(settings.preferences));

    let ui = styles::create(settings.style, &shared)?;
    shared.ui.replace(Some(ui.clone()));
    shared.tray.replace(Some(tray::create(&shared)?));
    let hidden = std::env::args().any(|arg| arg == autostart::HIDDEN) && settings.close_to_tray;
    shared.start_hidden.set(hidden);
    shared.refresh(everything());
    if !hidden {
        ui.show()?;
    } else if !tray::host_present() {
        tray::open_unless_host_appears(&shared, Duration::from_secs(10));
    }

    // Starting langis again shows this one.
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
                            (
                                chat.reply(&ConversationId(key), &text),
                                Update {
                                    conversations: true,
                                    ..Update::default()
                                },
                            )
                        });
                    }
                }
            }
        }
    })
    .expect("the event loop is set up");

    // Await backend events on the UI thread and apply each burst (e.g. the initial sync) as
    // one batch, so the UI refreshes once per batch. Nothing polls: idle means idle.
    let events = backend.events();
    slint::spawn_local({
        let shared = shared.clone();
        async move {
            while let Ok(first) = events.recv().await {
                let batch =
                    std::iter::once(first).chain(std::iter::from_fn(|| events.try_recv().ok()));
                // Hidden, the window may still count as active to the toolkit.
                let focused = shared.ui().is_some_and(|ui| ui.visible() && ui.active());
                let update = {
                    let mut chat = shared.chat.borrow_mut();
                    chat.set_window_focused(focused);
                    batch.fold(Update::default(), |update, event| {
                        if let Event::Error(error) = &event {
                            tracing::error!("{error}");
                        }
                        update.merge(chat.apply(event))
                    })
                };
                shared.refresh(update);
            }
            // The channel only closes early if the backend thread died without reporting why
            // (a panic); say so instead of leaving a frozen window.
            let update = shared.chat.borrow_mut().apply(Event::Error(
                "The Signal backend stopped unexpectedly.".into(),
            ));
            shared.refresh(update);
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
    // Only now may another langis start.
    drop(instance);
    result
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
/// nor makes unchanged rows look changed. Shared by all styles' windows.
#[derive(Default)]
pub struct Images {
    avatars: ImageCache<ConversationId>,
    attachments: ImageCache<AttachmentId>,
    /// Decoded BlurHashes, by hash.
    blurs: RefCell<HashMap<String, Image>>,
    /// The "Profile pictures" setting.
    avatars_enabled: Cell<bool>,
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

    /// Empty when pictures are turned off.
    fn avatar(&self, id: &ConversationId, avatar: &Arc<Picture>) -> Image {
        if !self.avatars_enabled.get() {
            return Image::default();
        }
        self.avatars.get(id, avatar)
    }
}

struct ImageCache<K>(RefCell<HashMap<K, (Arc<Picture>, Image)>>);

impl<K> Default for ImageCache<K> {
    fn default() -> Self {
        Self(RefCell::default())
    }
}

impl<K: std::hash::Hash + Eq + Clone> ImageCache<K> {
    fn get(&self, key: &K, picture: &Arc<Picture>) -> Image {
        let mut images = self.0.borrow_mut();
        if let Some((cached, image)) = images.get(key)
            && Arc::ptr_eq(cached, picture)
        {
            return image.clone();
        }
        let image = picture_image(picture);
        images.insert(key.clone(), (picture.clone(), image.clone()));
        image
    }
}

/// A picture from the backend, for Slint.
fn picture_image(picture: &Picture) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &picture.rgba,
        picture.width,
        picture.height,
    ))
}

/// Message text as Slint styled text: everything escaped, so nothing a sender types is taken as
/// formatting, except web addresses, which become links.
fn rich_text(body: &str, styles: &[StyleRange], revealed: bool) -> slint::StyledText {
    slint::StyledText::from_markdown(&markdown(body, styles, revealed))
        .unwrap_or_else(|_| slint::StyledText::from_plain_text(body))
}

/// See [`rich_text`].
fn markdown(body: &str, styles: &[StyleRange], revealed: bool) -> String {
    /// Between markers and text, so emphasis opens and closes whatever the neighbouring
    /// characters are (CommonMark's flanking rules); invisible.
    const JOIN: char = '\u{2060}';
    fn escape(text: &str, out: &mut String) {
        for c in text.chars() {
            if c.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(c);
        }
    }
    fn with_links(text: &str, out: &mut String) {
        let mut rest = text;
        while let Some((start, end)) = find_url(rest) {
            escape(&rest[..start], out);
            let url = &rest[start..end];
            out.push('[');
            escape(url, out);
            out.push_str("](<");
            out.push_str(url);
            out.push_str(">)");
            rest = &rest[end..];
        }
        escape(rest, out);
    }
    let valid = |s: &&StyleRange| {
        s.start < s.end
            && s.end <= body.len()
            && body.is_char_boundary(s.start)
            && body.is_char_boundary(s.end)
    };
    // Pieces with the same formatting throughout; lines apart, for the breaks between them.
    let mut cuts: Vec<usize> = vec![0, body.len()];
    cuts.extend(styles.iter().filter(valid).flat_map(|s| [s.start, s.end]));
    cuts.extend(body.match_indices('\n').flat_map(|(i, _)| [i, i + 1]));
    cuts.sort_unstable();
    cuts.dedup();
    let mut markdown = String::with_capacity(body.len() + 16);
    for piece in cuts.windows(2) {
        let (start, end) = (piece[0], piece[1]);
        let text = &body[start..end];
        if text == "\n" {
            // A hard line break, as the sender typed it.
            markdown.push_str("\\\n");
            continue;
        }
        let has = |style| {
            styles
                .iter()
                .filter(valid)
                .any(|s| s.style == style && s.start <= start && end <= s.end)
        };
        // Leading spaces would start a code block; keep them as non-breaking spaces.
        let line_start = start == 0 || body.as_bytes()[start - 1] == b'\n';
        let text = if line_start {
            let trimmed = text.trim_start_matches(' ');
            markdown.extend(std::iter::repeat_n('\u{a0}', text.len() - trimmed.len()));
            trimmed
        } else {
            text
        };
        if text.is_empty() {
            continue;
        }
        if has(Style::Spoiler) && !revealed {
            // Hidden behind a link that reveals it; see `Bubble` in app.slint.
            let hidden: String = text
                .chars()
                .map(|c| if c.is_whitespace() { c } else { '▒' })
                .collect();
            markdown.push('[');
            markdown.push_str(&hidden);
            markdown.push_str("](spoiler:)");
            continue;
        }
        let markers: Vec<&str> = [
            (Style::Bold, "**"),
            (Style::Italic, "*"),
            (Style::Strikethrough, "~~"),
        ]
        .into_iter()
        .filter(|(style, _)| has(*style))
        .map(|(_, marker)| marker)
        .collect();
        // Apart from the previous piece's closing markers, which would otherwise run into these.
        if !markers.is_empty() {
            markdown.push(JOIN);
        }
        for marker in &markers {
            markdown.push_str(marker);
            markdown.push(JOIN);
        }
        if has(Style::Monospace) {
            // Code spans take no escapes; a longer fence than any backtick run inside.
            let longest = text
                .split(|c| c != '`')
                .map(str::len)
                .max()
                .unwrap_or_default();
            let fence = "`".repeat(longest + 1);
            markdown.push_str(&fence);
            markdown.push(' ');
            markdown.push_str(text);
            markdown.push(' ');
            markdown.push_str(&fence);
        } else {
            with_links(text, &mut markdown);
        }
        for marker in markers.iter().rev() {
            markdown.push(JOIN);
            markdown.push_str(marker);
        }
    }
    markdown
}

/// Opens a link from a message in the browser. Only web addresses: links come from message
/// text, so nothing else (files, other apps' schemes) is opened.
fn open_link(link: &str) {
    if !(link.starts_with("https://") || link.starts_with("http://")) {
        return;
    }
    if let Err(error) = open::that_detached(link) {
        tracing::warn!(%error, "couldn't open a link");
    }
}

fn qr_image(qr: &QrMatrix) -> Image {
    // One pixel per module; the UI scales it up with `image-rendering: pixelated`.
    let (side, pixels) = qr.to_rgba(1);
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, side as u32, side as u32);
    Image::from_rgba8(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(text: &slint::StyledText) -> String {
        i_slint_core::styled_text::get_raw_text(text).into_owned()
    }

    #[test]
    fn finds_web_addresses_without_trailing_punctuation() {
        let text = "see https://example.org/a?b=1. and (http://x.y/z)";
        let (start, end) = find_url(text).unwrap();
        assert_eq!(&text[start..end], "https://example.org/a?b=1");
        let rest = &text[end..];
        let (start, end) = find_url(rest).unwrap();
        assert_eq!(&rest[start..end], "http://x.y/z");
        assert_eq!(find_url("no links here, nor in https:// alone"), None);
    }

    /// What people type is shown as typed: no markdown, no HTML, line breaks kept.
    #[test]
    fn message_text_is_never_formatting() {
        for text in [
            "*not bold* _nor this_ `code` <b>tag</b> # heading",
            "1. not a list",
            "    indented",
            "two\nlines",
        ] {
            let shown = raw(&rich_text(text, &[], false));
            assert_eq!(shown.replace('\u{a0}', " "), text, "{text:?}");
        }
    }

    #[test]
    fn links_keep_their_text() {
        let shown = raw(&rich_text("look: https://example.org/x_y*z ok", &[], false));
        assert_eq!(shown, "look: https://example.org/x_y*z ok");
    }

    /// Formatting applies whatever surrounds it, and the text stays as it was.
    #[test]
    fn formatting_keeps_the_text() {
        let range = |start, end, style| StyleRange { start, end, style };
        let text = "a\"b\" (c) `d` e";
        let styles = [
            range(1, 4, Style::Bold),
            range(5, 8, Style::Italic),
            range(9, 12, Style::Monospace),
            range(0, 14, Style::Strikethrough),
        ];
        // Parsed, not the plain-text fallback: the markers are gone.
        let parse = |styles: &[StyleRange], revealed| {
            let markdown = markdown(text, styles, revealed);
            raw(&slint::StyledText::from_markdown(&markdown).expect("valid markdown"))
                .replace('\u{2060}', "")
        };
        assert_eq!(parse(&styles, false), text);
        let spoiler = [range(5, 8, Style::Spoiler)];
        assert_eq!(parse(&spoiler, false), "a\"b\" ▒▒▒ `d` e");
        assert_eq!(parse(&spoiler, true), text);
    }
}
