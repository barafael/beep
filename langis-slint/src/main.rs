// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use langis_core::settings::{NotificationContent, Settings, TextSize, Theme};
use langis_core::state::{self, AttachmentState, ChatState, ConversationView, MessageView, MessagesUpdate, Notice, SaveState, Update};
use langis_core::{Attachment, AttachmentContent, AttachmentId, AttachmentKind, Picture, Backend, Command, Commands, Config, ConversationId, Event, HistoryTransfer, QrMatrix};
use langis_notify::{Action, AppInfo, Notification, Notifier};
use slint::{Color, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

const APP: AppInfo = AppInfo { id: "org.langis.Langis", name: "langis" };

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let window = AppWindow::new()?;
    let settings = Settings::load();
    show_settings(&window, &settings);
    let config = Config::default();
    window.set_device_name(config.device_name.as_str().into());
    let backend = Backend::from_env(config);
    let chat = Rc::new(RefCell::new(ChatState::default()));
    chat.borrow_mut().set_notifications(settings.notifications, settings.notification_content);
    let (notifier, notifier_actions) = Notifier::new(&APP);

    // The window owns the models; `vec_model` gets them back for updates.
    window.set_conversations(ModelRc::new(VecModel::<ConversationItem>::default()));
    window.set_messages(ModelRc::new(VecModel::<MessageItem>::default()));

    let images = Rc::new(Images::default());
    images.avatars_enabled.set(settings.show_avatars);
    let app = App {
        window: window.as_weak(),
        chat: chat.clone(),
        images: images.clone(),
        notifier: Rc::new(notifier),
        commands: backend.commands(),
    };
    window.on_select({
        let app = app.clone();
        move |id| {
            app.act(|chat, _| {
                let command = chat.select(&ConversationId(id.into()));
                (command, Update { conversations: true, messages: MessagesUpdate::Reset, ..Update::default() })
            })
        }
    });
    window.on_send({
        let app = app.clone();
        move |text| {
            app.act(|chat, window| {
                let command = chat.compose(&text);
                if command.is_some() {
                    window.set_draft(SharedString::new());
                }
                (command, Update::default())
            })
        }
    });
    window.on_unlock({
        let app = app.clone();
        move |password, confirmation| {
            app.act(|chat, _| (chat.unlock(&password, &confirmation), Update { screen: true, ..Update::default() }))
        }
    });
    window.on_draft_edited({
        let app = app.clone();
        move |_| app.act(|_, _| (None, Update::default()))
    });
    window.on_settings_changed({
        let app = app.clone();
        move || {
            let Some(window) = app.window.upgrade() else { return };
            let settings = read_settings(&window);
            window.set_font_scale(settings.text_size.scale());
            if let Err(error) = settings.save() {
                tracing::warn!(%error, "failed to save settings");
            }
            // The theme applies in the UI itself; pictures need the list rebuilt.
            let rebuild = app.images.avatars_enabled.replace(settings.show_avatars) != settings.show_avatars;
            app.act(|chat, _| {
                chat.set_notifications(settings.notifications, settings.notification_content);
                (None, Update { conversations: rebuild, ..Update::default() })
            });
        }
    });
    window.on_fetch_attachment({
        let app = app.clone();
        move |id| {
            app.act(|chat, _| (chat.fetch_attachment(&AttachmentId(id.into())), Update { rows: true, ..Update::default() }))
        }
    });
    window.on_save_attachment({
        let app = app.clone();
        move |id| {
            app.act(|chat, _| (chat.save_attachment(&AttachmentId(id.into())), Update { rows: true, ..Update::default() }))
        }
    });
    window.on_react({
        let app = app.clone();
        move |index, emoji| app.act(|chat, _| (chat.react(index as usize, &emoji), Update::default()))
    });
    window.on_reply({
        let app = app.clone();
        move |index| app.act(|chat, _| (None, chat.reply_to(index as usize)))
    });
    window.on_cancel_reply({
        let app = app.clone();
        move || {
            app.act(|chat, _| {
                chat.cancel_reply();
                (None, Update::default())
            })
        }
    });
    window.on_search({
        let app = app.clone();
        move |text| app.act(|chat, _| (None, chat.search(&text)))
    });
    window.on_skip_history({
        let app = app.clone();
        move || app.act(|chat, _| (chat.skip_history(), Update::default()))
    });
    window.on_dismiss_error({
        let app = app.clone();
        move || {
        app.act(|chat, _| {
            chat.dismiss_error();
            (None, Update::default())
        })
        }
    });

    // What the user did with a notification: open its chat, or send the answer typed into it.
    let notifier = app.notifier.clone();
    slint::spawn_local(async move {
        while let Ok(action) = notifier_actions.recv().await {
            match action {
                Action::Open { key } => {
                    if let Some(key) = key {
                        app.act(|chat, _| {
                            let command = chat.select(&ConversationId(key));
                            (command, Update { conversations: true, messages: MessagesUpdate::Reset, ..Update::default() })
                        });
                    }
                    app.raise();
                }
                Action::Reply { key, text } => app.act(|chat, _| {
                    (chat.reply(&ConversationId(key), &text), Update { conversations: true, ..Update::default() })
                }),
            }
        }
    })
    .expect("the event loop is set up");

    // Await backend events on the UI thread and apply each burst (e.g. the initial sync) as
    // one batch, so the UI refreshes once per batch. Nothing polls: idle means idle.
    let events = backend.events();
    let weak = window.as_weak();
    slint::spawn_local(async move {
        while let Ok(first) = events.recv().await {
            let batch = std::iter::once(first).chain(std::iter::from_fn(|| events.try_recv().ok()));
            let Some(window) = weak.upgrade() else { break };
            let mut chat = chat.borrow_mut();
            chat.set_window_focused(window_active(&window));
            let update = batch.fold(Update::default(), |update, event| {
                if let Event::Error(error) = &event {
                    tracing::error!("{error}");
                }
                update.merge(chat.apply(event))
            });
            render(&window, &chat, &images, update);
            deliver_notices(&notifier, &mut chat);
        }
        // The channel only closes early if the backend thread died without reporting why
        // (a panic); say so instead of leaving a frozen window.
        if let Some(window) = weak.upgrade() {
            let mut chat = chat.borrow_mut();
            let update = chat.apply(Event::Error("The Signal backend stopped unexpectedly.".into()));
            render(&window, &chat, &images, update);
        }
    })
    .expect("the event loop is set up");

    let result = window.run();
    // Waits for the backend to close the database, which must happen before the process exits.
    drop(backend);
    result
}

/// What UI callbacks need: they ask `ChatState` what to do, send the resulting command and
/// re-render whatever changed.
#[derive(Clone)]
struct App {
    window: slint::Weak<AppWindow>,
    chat: Rc<RefCell<ChatState>>,
    images: Rc<Images>,
    notifier: Rc<Notifier>,
    commands: Commands,
}

impl App {
    fn act(&self, action: impl FnOnce(&mut ChatState, &AppWindow) -> (Option<Command>, Update)) {
        let Some(window) = self.window.upgrade() else { return };
        let mut chat = self.chat.borrow_mut();
        let (command, update) = action(&mut chat, &window);
        if let Some(command) = command {
            self.commands.send(command);
        }
        render(&window, &chat, &self.images, update);
        deliver_notices(&self.notifier, &mut chat);
    }

    /// Brings the window to the front, e.g. after a notification was clicked. (On Wayland the
    /// compositor may only highlight it: raising needs an activation token Slint can't take yet.)
    fn raise(&self) {
        if let Some(window) = self.window.upgrade() {
            window.window().set_minimized(false);
            if let Err(error) = window.show() {
                tracing::warn!(%error, "failed to show the window");
            }
        }
    }
}

fn deliver_notices(notifier: &Notifier, chat: &mut ChatState) {
    for notice in chat.take_notices() {
        match notice {
            Notice::Show { conversation, title, body } => {
                notifier.show(Notification { key: conversation.map(|id| id.0), title, body })
            }
            Notice::Withdraw(id) => notifier.withdraw(&id.0),
        }
    }
}

/// Whether the window is the active one. Slint's public API doesn't offer this yet, but both
/// the Qt and the winit backend keep it up to date internally.
fn window_active(window: &AppWindow) -> bool {
    i_slint_core::window::WindowInner::from_pub(window.window()).active()
}

/// Pictures converted to Slint images once, so re-rendering lists neither copies pixels again
/// nor makes unchanged rows look changed.
#[derive(Default)]
struct Images {
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
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&picture.rgba, picture.width, picture.height);
        let image = Image::from_rgba8(buffer);
        images.insert(key.clone(), (picture.clone(), image.clone()));
        image
    }
}

/// The settings page shows choices by index, in the order of these enums.
fn show_settings(window: &AppWindow, settings: &Settings) {
    window.set_theme(match settings.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    window.set_text_size(match settings.text_size {
        TextSize::Small => 0,
        TextSize::Default => 1,
        TextSize::Large => 2,
    });
    window.set_show_avatars(settings.show_avatars);
    window.set_notifications(settings.notifications);
    window.set_notification_content(match settings.notification_content {
        NotificationContent::NameAndMessage => 0,
        NotificationContent::NameOnly => 1,
        NotificationContent::Nothing => 2,
    });
    window.set_font_scale(settings.text_size.scale());
}

fn read_settings(window: &AppWindow) -> Settings {
    Settings {
        theme: match window.get_theme() {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        },
        text_size: match window.get_text_size() {
            0 => TextSize::Small,
            2 => TextSize::Large,
            _ => TextSize::Default,
        },
        show_avatars: window.get_show_avatars(),
        notifications: window.get_notifications(),
        notification_content: match window.get_notification_content() {
            1 => NotificationContent::NameOnly,
            2 => NotificationContent::Nothing,
            _ => NotificationContent::NameAndMessage,
        },
    }
}

/// Brings the window in line with `chat`, touching only what `update` says changed (plus the
/// cheap status properties).
fn render(window: &AppWindow, chat: &ChatState, images: &Images, update: Update) {
    if update.screen {
        match chat.screen() {
            state::Screen::Connecting => window.set_screen(Screen::Connecting),
            state::Screen::Password { new, error } => {
                window.set_password_new(*new);
                window.set_password_error(error.as_deref().unwrap_or_default().into());
                window.set_screen(Screen::Password);
            }
            state::Screen::Link(qr) => {
                window.set_link_qr(qr_image(qr));
                window.set_screen(Screen::Link);
            }
            state::Screen::Chats => window.set_screen(Screen::Chats),
        }
        window.set_account(chat.account().into());
    }
    window.set_synced(chat.synced());
    window.set_history_status(
        match chat.transfer() {
            None | Some(HistoryTransfer::Finished) => String::new(),
            Some(HistoryTransfer::Waiting) => "Waiting for your phone to send your message history…".into(),
            Some(HistoryTransfer::Downloading) => "Downloading your message history…".into(),
            Some(HistoryTransfer::Importing { messages }) => format!("Importing your message history… {messages} messages"),
        }
        .into(),
    );
    window.set_history_skippable(chat.transfer() == Some(HistoryTransfer::Waiting));
    window.set_error(chat.error().unwrap_or_default().into());
    window.set_can_send(chat.can_compose(&window.get_draft()));
    let replying = chat.replying();
    window.set_reply_author(replying.map_or("", |q| q.author.as_str()).into());
    window.set_reply_text(replying.map_or("", |q| q.text.as_str()).into());

    let messages = window.get_messages();
    let messages = vec_model(&messages);
    match update.messages {
        MessagesUpdate::Unchanged => {}
        MessagesUpdate::Appended(n) => {
            for message in chat.last_messages(n) {
                messages.push(message_item(&message, chat, images));
            }
        }
        MessagesUpdate::Reset => {
            messages.set_vec(chat.messages().map(|m| message_item(&m, chat, images)).collect::<Vec<_>>())
        }
    }
    // Messages changed in place (downloads, reactions): redraw only rows that differ.
    if update.rows && update.messages != MessagesUpdate::Reset {
        for (row, message) in chat.messages().enumerate() {
            let item = message_item(&message, chat, images);
            if !messages.row_data(row).is_some_and(|old| same_message(&old, &item)) {
                messages.set_row_data(row, item);
            }
        }
    }

    if update.conversations {
        let items: Vec<_> = chat.conversations().map(|c| conversation_item(&c, images)).collect();
        let conversations = window.get_conversations();
        let conversations = vec_model(&conversations);
        // Update in place where possible so the list keeps its scroll position.
        if conversations.row_count() == items.len() {
            for (row, item) in items.into_iter().enumerate() {
                if conversations.row_data(row).as_ref() != Some(&item) {
                    conversations.set_row_data(row, item);
                }
            }
        } else {
            conversations.set_vec(items);
        }
        window.set_selected(chat.selected_index().map_or(-1, |i| i as i32));
        if !chat.searching() {
            window.set_search_text(SharedString::new());
        }
    }
}

/// The `VecModel` behind a model set up in `main`.
fn vec_model<T: Clone + 'static>(model: &ModelRc<T>) -> &VecModel<T> {
    model.as_any().downcast_ref().expect("models are VecModels, set up in main")
}

fn conversation_item(view: &ConversationView, images: &Images) -> ConversationItem {
    // Muted hues in the spirit of Signal's avatar palette; all readable with white text.
    const PALETTE: [u32; 8] = [
        0x3a76f0, 0x7d4fc9, 0xc4314b, 0xd36c1c, 0x2b8a60, 0x1e8a9e, 0x9a5b3c, 0x5c6bc0,
    ];
    let c = view.conversation;
    let rgb = PALETTE[state::avatar_index(&c.id, PALETTE.len())];
    ConversationItem {
        id: c.id.0.as_str().into(),
        title: c.title.as_str().into(),
        initials: state::initials(&c.title).into(),
        avatar_color: Color::from_argb_encoded(0xff00_0000 | rgb),
        avatar: view.avatar.map(|avatar| images.avatar(&c.id, avatar)).unwrap_or_default(),
        preview: view.preview().into(),
        unread: view.unread as i32,
    }
}

fn message_item(view: &MessageView, chat: &ChatState, images: &Images) -> MessageItem {
    let m = view.message;
    let attachments: Vec<_> =
        m.attachments.iter().map(|a| attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images)).collect();
    MessageItem {
        sender: m.sender.as_str().into(),
        body: view.body.into(),
        time: state::format_time(m.timestamp).into(),
        outgoing: m.outgoing,
        show_sender: view.show_sender,
        quote_author: m.quote.as_ref().map_or("", |q| q.author.as_str()).into(),
        quote_text: m.quote.as_ref().map_or("", |q| q.text.as_str()).into(),
        attachments: ModelRc::new(VecModel::from(attachments)),
        reactions: ModelRc::new(VecModel::from(reaction_items(&m.reactions))),
    }
}

/// One chip per emoji, in the order they first came, counting everyone who chose it.
fn reaction_items(reactions: &[langis_core::Reaction]) -> Vec<ReactionItem> {
    let mut items: Vec<ReactionItem> = Vec::new();
    for reaction in reactions {
        match items.iter_mut().find(|item| item.emoji.as_str() == reaction.emoji) {
            Some(item) => {
                item.count += 1;
                item.mine |= reaction.outgoing;
            }
            None => items.push(ReactionItem { emoji: reaction.emoji.as_str().into(), count: 1, mine: reaction.outgoing }),
        }
    }
    items
}

/// Like `==`, but comparing attachment lists by content: each item gets a new model, which
/// would never compare equal and so redraw every row with an attachment.
fn same_message(a: &MessageItem, b: &MessageItem) -> bool {
    fn same<T: PartialEq>(a: &ModelRc<T>, b: &ModelRc<T>) -> bool {
        a.row_count() == b.row_count() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
    }
    same(&a.attachments, &b.attachments)
        && same(&a.reactions, &b.reactions)
        && MessageItem { attachments: b.attachments.clone(), reactions: b.reactions.clone(), ..a.clone() } == *b
}

fn attachment_item(
    attachment: &Attachment,
    download: Option<&AttachmentState>,
    save: Option<&SaveState>,
    images: &Images,
) -> AttachmentItem {
    let kind = attachment.kind;
    let inline = matches!(kind, AttachmentKind::Image | AttachmentKind::Sticker);
    let picture = match download {
        Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => Some(picture),
        _ => None,
    };
    // Fit into the bubble, keeping the shape the sender reported (or the picture has).
    let (max, fallback) = if kind == AttachmentKind::Sticker { (128.0, (128.0, 128.0)) } else { (280.0, (240.0, 180.0)) };
    let (w, h) = match picture {
        Some(p) => (p.width as f32, p.height as f32),
        None if attachment.width > 0 && attachment.height > 0 => (attachment.width as f32, attachment.height as f32),
        None => fallback,
    };
    let scale = (max / w.max(h)).min(1.0);
    let size = state::format_size(attachment.size);
    let label = match (kind, &attachment.file_name) {
        (AttachmentKind::ViewOnce, _) => "View-once media: open it on your phone".to_string(),
        (AttachmentKind::File, Some(name)) => format!("📎 {name} · {size}"),
        _ => format!("{} · {size}", state::describe_kind(kind)),
    };
    let failed = match download {
        Some(AttachmentState::Failed(error)) => Some(error.clone()),
        _ => None,
    };
    // Saving says how it went; otherwise a failed download says why (click to retry).
    let status = match save {
        Some(SaveState::Saving) => "Saving…".into(),
        Some(SaveState::Saved(path)) => {
            format!("Saved to {}", path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("disk"))
        }
        Some(SaveState::Failed(error)) => format!("Couldn't save: {error}"),
        None => failed.clone().map(|error| format!("{error} Click to retry.")).unwrap_or_default(),
    };
    AttachmentItem {
        id: attachment.id.0.as_str().into(),
        inline,
        hidden: kind == AttachmentKind::LongText,
        fetch: download.is_none() && state::shown_inline(kind),
        picture: picture.map(|p| images.attachments.get(&attachment.id, p)).unwrap_or_default(),
        width: w * scale,
        height: h * scale,
        label: label.into(),
        status: status.into(),
        can_retry: failed.is_some() && state::shown_inline(kind),
        can_save: !matches!(kind, AttachmentKind::ViewOnce | AttachmentKind::LongText | AttachmentKind::Sticker),
    }
}

fn qr_image(qr: &QrMatrix) -> Image {
    // One pixel per module; the UI scales it up with `image-rendering: pixelated`.
    let (side, pixels) = qr.to_rgba(1);
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, side as u32, side as u32);
    Image::from_rgba8(buffer)
}
