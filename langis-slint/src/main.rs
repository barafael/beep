// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use langis_core::settings::{Settings, WidgetStyle};
use langis_core::state::{ChatState, MessagesUpdate, Notice, Update};
use langis_core::{
    AttachmentId, Backend, Command, Commands, Config, ConversationId, Event, Picture, QrMatrix,
};
use langis_notify::{Action, AppInfo, Notification, Notifier};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

mod logging;

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
    /// Brings the window to the front, e.g. after a notification was clicked.
    fn raise(&self);
    /// Whether the window is the active one.
    fn active(&self) -> bool;
    fn geometry(&self) -> (slint::PhysicalPosition, slint::PhysicalSize);
    fn take_geometry(&self, geometry: (slint::PhysicalPosition, slint::PhysicalSize));
    fn open_settings(&self);
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
        deliver_notices(&self.notifier, &mut chat);
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

    let config = Config::default();
    let device_name = config.device_name.clone();
    let backend = Backend::from_env(config);
    let (notifier, notifier_actions) = Notifier::new(&APP);
    let images = Images::default();
    images.avatars_enabled.set(settings.show_avatars);
    let mut chat = ChatState::default();
    chat.set_notifications(settings.notifications, settings.notification_content);
    let shared = Rc::new(Shared {
        chat: RefCell::new(chat),
        images,
        notifier,
        commands: backend.commands(),
        settings: RefCell::new(settings.clone()),
        device_name,
        qt_style_at_start,
        ui: RefCell::default(),
    });

    let ui = styles::create(settings.style, &shared)?;
    shared.ui.replace(Some(ui.clone()));
    shared.refresh(everything());
    ui.show()?;

    // What the user did with a notification: open its chat, or send the answer typed into it.
    slint::spawn_local({
        let shared = shared.clone();
        async move {
            while let Ok(action) = notifier_actions.recv().await {
                match action {
                    Action::Open { key } => {
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
                        if let Some(ui) = shared.ui() {
                            ui.raise();
                        }
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
                let focused = shared.ui().is_some_and(|ui| ui.active());
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

    // Until the last window closes; switching styles opens the new one before closing the old.
    let result = slint::run_event_loop();
    // Waits for the backend to close the database, which must happen before the process exits.
    drop(backend);
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
    /// The "Profile pictures" setting.
    avatars_enabled: Cell<bool>,
}

impl Images {
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
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &picture.rgba,
            picture.width,
            picture.height,
        );
        let image = Image::from_rgba8(buffer);
        images.insert(key.clone(), (picture.clone(), image.clone()));
        image
    }
}

/// Message text as Slint styled text: everything escaped, so nothing a sender types is taken as
/// formatting, except web addresses, which become links.
fn rich_text(body: &str) -> slint::StyledText {
    fn escape(text: &str, out: &mut String) {
        for c in text.chars() {
            if c.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(c);
        }
    }
    let mut markdown = String::with_capacity(body.len() + 16);
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            // A hard line break, as the sender typed it.
            markdown.push_str("\\\n");
        }
        // Leading spaces would start a code block; keep them as non-breaking spaces.
        let text = line.trim_start_matches(' ');
        markdown.extend(std::iter::repeat_n('\u{a0}', line.len() - text.len()));
        let mut rest = text;
        while let Some((start, end)) = find_url(rest) {
            escape(&rest[..start], &mut markdown);
            let url = &rest[start..end];
            markdown.push('[');
            escape(url, &mut markdown);
            markdown.push_str("](<");
            markdown.push_str(url);
            markdown.push_str(">)");
            rest = &rest[end..];
        }
        escape(rest, &mut markdown);
    }
    slint::StyledText::from_markdown(&markdown)
        .unwrap_or_else(|_| slint::StyledText::from_plain_text(body))
}

/// The first http(s) address in `text`, as a byte range; trailing punctuation is left out.
fn find_url(text: &str) -> Option<(usize, usize)> {
    let start = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| text.find(scheme))
        .min()?;
    let tail = &text[start..];
    let length = tail
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(tail.len());
    let url =
        tail[..length].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"']);
    // Just a scheme is no address.
    (url.len() > "https://".len()).then_some((start, start + url.len()))
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
            let shown = raw(&rich_text(text));
            assert_eq!(shown.replace('\u{a0}', " "), text, "{text:?}");
        }
    }

    #[test]
    fn links_keep_their_text() {
        let shown = raw(&rich_text("look: https://example.org/x_y*z ok"));
        assert_eq!(shown, "look: https://example.org/x_y*z ok");
    }
}
