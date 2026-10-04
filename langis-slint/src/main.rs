// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::rc::Rc;

use langis_core::state::{self, ChatState, ConversationView, MessageView, MessagesUpdate, Update};
use langis_core::{Backend, Command, Config, ConversationId, Event, QrMatrix};
use slint::{Color, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let window = AppWindow::new()?;
    let backend = Rc::new(Backend::from_env(Config::default()));
    let chat = Rc::new(RefCell::new(ChatState::default()));

    // The window owns the models; `vec_model` gets them back for updates.
    window.set_conversations(ModelRc::new(VecModel::<ConversationItem>::default()));
    window.set_messages(ModelRc::new(VecModel::<MessageItem>::default()));

    let app = App { window: window.as_weak(), chat: chat.clone(), backend: backend.clone() };
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
    window.on_dismiss_error(move || {
        app.act(|chat, _| {
            chat.dismiss_error();
            (None, Update::default())
        })
    });

    // Await backend events on the UI thread and apply each burst (e.g. the initial sync) as
    // one batch, so the UI refreshes once per batch. Nothing polls: idle means idle.
    let events = backend.events();
    let weak = window.as_weak();
    slint::spawn_local(async move {
        while let Ok(first) = events.recv().await {
            let batch = std::iter::once(first).chain(std::iter::from_fn(|| events.try_recv().ok()));
            let Some(window) = weak.upgrade() else { break };
            let mut chat = chat.borrow_mut();
            let update = batch.fold(Update::default(), |update, event| {
                if let Event::Error(error) = &event {
                    tracing::error!("{error}");
                }
                update.merge(chat.apply(event))
            });
            render(&window, &chat, update);
        }
    })
    .expect("the event loop is set up");

    window.run()
}

/// What UI callbacks need: they ask `ChatState` what to do, send the resulting command and
/// re-render whatever changed.
#[derive(Clone)]
struct App {
    window: slint::Weak<AppWindow>,
    chat: Rc<RefCell<ChatState>>,
    backend: Rc<Backend>,
}

impl App {
    fn act(&self, action: impl FnOnce(&mut ChatState, &AppWindow) -> (Option<Command>, Update)) {
        let Some(window) = self.window.upgrade() else { return };
        let mut chat = self.chat.borrow_mut();
        let (command, update) = action(&mut chat, &window);
        if let Some(command) = command {
            self.backend.send(command);
        }
        render(&window, &chat, update);
    }
}

/// Brings the window in line with `chat`, touching only what `update` says changed (plus the
/// cheap status properties).
fn render(window: &AppWindow, chat: &ChatState, update: Update) {
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
    window.set_error(chat.error().unwrap_or_default().into());
    window.set_can_send(chat.can_compose(&window.get_draft()));

    let messages = window.get_messages();
    let messages = vec_model(&messages);
    match update.messages {
        MessagesUpdate::Unchanged => {}
        MessagesUpdate::Appended => {
            if let Some(last) = chat.last_message() {
                messages.push(message_item(&last));
            }
        }
        MessagesUpdate::Reset => messages.set_vec(chat.messages().map(|m| message_item(&m)).collect::<Vec<_>>()),
    }

    if update.conversations {
        let items: Vec<_> = chat.conversations().map(|c| conversation_item(&c)).collect();
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
    }
}

/// The `VecModel` behind a model set up in `main`.
fn vec_model<T: Clone + 'static>(model: &ModelRc<T>) -> &VecModel<T> {
    model.as_any().downcast_ref().expect("models are VecModels, set up in main")
}

fn conversation_item(view: &ConversationView) -> ConversationItem {
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
        preview: view.preview().into(),
        unread: view.unread as i32,
    }
}

fn message_item(view: &MessageView) -> MessageItem {
    let m = view.message;
    MessageItem {
        sender: m.sender.as_str().into(),
        body: m.body.as_str().into(),
        time: state::format_time(m.timestamp).into(),
        outgoing: m.outgoing,
        show_sender: view.show_sender,
    }
}

fn qr_image(qr: &QrMatrix) -> Image {
    // One pixel per module; the UI scales it up with `image-rendering: pixelated`.
    let (side, pixels) = qr.to_rgba(1);
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, side as u32, side as u32);
    Image::from_rgba8(buffer)
}
