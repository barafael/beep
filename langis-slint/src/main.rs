// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use langis_core::settings::{Settings, TextSize, Theme};
use langis_core::state::{self, ChatState, ConversationView, MessageView, MessagesUpdate, Update};
use langis_core::{Avatar, Backend, Command, Commands, Config, ConversationId, Event, QrMatrix};
use slint::{Color, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

/// Targets that can hold message contents in memory, and so must never log at `debug`.
///
/// `Content` derives `Debug`, and that prints the message body in plaintext, so a single
/// `debug!(?msg, …)` anywhere in these crates writes the conversation to the log.
const PLAINTEXT_TARGETS: [&str; 2] = ["presage", "libsignal_service"];

/// Builds the log filter from `RUST_LOG`.
///
/// `RUST_LOG` still raises verbosity, but it cannot lift [`PLAINTEXT_TARGETS`] out of `info`.
/// There is no way to redact this at the call site: by the time a value reaches a log statement
/// it has already been formatted, so the filter has to be the thing that decides. Debugging
/// langis itself stays possible, since `langis_core` is not capped.
fn log_filter(rust_log: &str) -> tracing_subscriber::EnvFilter {
    use tracing::level_filters::LevelFilter;

    let mut filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .parse_lossy(&clamp_plaintext(rust_log));
    for target in PLAINTEXT_TARGETS {
        filter = filter.add_directive(
            format!("{target}=info")
                .parse()
                .expect("a target and a level are a valid directive"),
        );
    }
    filter
}

/// Rewrites `rust_log` so that no directive in it can enable `debug` or `trace` for a
/// [`PLAINTEXT_TARGETS`] crate.
///
/// The broad cap added in [`log_filter`] is not enough on its own: `EnvFilter` picks the *most
/// specific* matching directive, so `RUST_LOG=presage::manager::registered=trace` beats
/// `presage=info`. Rewriting the level is the only way to make the cap stick, and doing it on the
/// string keeps the decision visible rather than relying on precedence rules. Regex directives are
/// dropped, since they can name any target at all.
fn clamp_plaintext(rust_log: &str) -> String {
    use tracing::level_filters::LevelFilter;

    let mut kept: Vec<String> = Vec::new();
    for directive in rust_log.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        // A regex directive such as `/registered/=trace` can match anything, so there is no
        // telling what it would enable.
        if directive.starts_with('/') {
            continue;
        }
        let Some((target, level)) = directive.split_once('=') else {
            // A bare level such as `debug` applies to everything, but the caps are per-crate, so
            // they are the more specific match and win on their own.
            kept.push(directive.to_string());
            continue;
        };
        let below_info = level
            .parse::<LevelFilter>()
            .is_ok_and(|level| level > LevelFilter::INFO);
        // `target[span{field}]=level` is valid too; compare the module path on its own.
        let module = target.split_once('[').map_or(target, |(module, _)| module);
        let plaintext = PLAINTEXT_TARGETS
            .iter()
            .any(|name| module == *name || module.starts_with(&format!("{name}::")));
        kept.push(match (plaintext, below_info) {
            (true, true) => format!("{target}=info"),
            _ => directive.to_string(),
        });
    }
    kept.join(",")
}

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(log_filter(&std::env::var("RUST_LOG").unwrap_or_default()))
        .init();

    let window = AppWindow::new()?;
    let settings = Settings::load();
    show_settings(&window, &settings);
    let config = Config::default();
    window.set_device_name(config.device_name.as_str().into());
    if Backend::mock_requested() {
        // The mock sends nothing at all, so say so where it can't be missed.
        window.set_mock(true);
    }
    let backend = Backend::from_env(config);
    let chat = Rc::new(RefCell::new(ChatState::default()));

    // The window owns the models; `vec_model` gets them back for updates.
    window.set_conversations(ModelRc::new(VecModel::<ConversationItem>::default()));
    window.set_messages(ModelRc::new(VecModel::<MessageItem>::default()));

    let avatars = Rc::new(AvatarImages::default());
    avatars.enabled.set(settings.show_avatars);
    let app = App { window: window.as_weak(), chat: chat.clone(), avatars: avatars.clone(), commands: backend.commands() };
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
    window.on_load_older({
        let app = app.clone();
        move || app.act(|chat, _| (chat.load_older(), Update::default()))
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
            if app.avatars.enabled.replace(settings.show_avatars) != settings.show_avatars {
                app.act(|_, _| (None, Update { conversations: true, ..Update::default() }));
            }
        }
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
            render(&window, &chat, &avatars, update);
        }
        // The channel only closes early if the backend thread died without reporting why
        // (a panic); say so instead of leaving a frozen window.
        if let Some(window) = weak.upgrade() {
            let mut chat = chat.borrow_mut();
            let update = chat.apply(Event::Error("The Signal backend stopped unexpectedly.".into()));
            render(&window, &chat, &avatars, update);
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
    avatars: Rc<AvatarImages>,
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
        render(&window, &chat, &self.avatars, update);
    }
}

/// Avatars converted to Slint images, so re-rendering the list neither copies pixels again
/// nor makes unchanged rows look changed.
#[derive(Default)]
struct AvatarImages {
    images: RefCell<HashMap<ConversationId, (Arc<Avatar>, Image)>>,
    /// The "Profile pictures" setting.
    enabled: Cell<bool>,
}

impl AvatarImages {
    /// Empty when pictures are turned off.
    fn get(&self, id: &ConversationId, avatar: &Arc<Avatar>) -> Image {
        if !self.enabled.get() {
            return Image::default();
        }
        let mut images = self.images.borrow_mut();
        if let Some((cached, image)) = images.get(id)
            && Arc::ptr_eq(cached, avatar)
        {
            return image.clone();
        }
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&avatar.rgba, avatar.side, avatar.side);
        let image = Image::from_rgba8(buffer);
        images.insert(id.clone(), (avatar.clone(), image.clone()));
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
    }
}

/// Brings the window in line with `chat`, touching only what `update` says changed (plus the
/// cheap status properties).
fn render(window: &AppWindow, chat: &ChatState, avatars: &AvatarImages, update: Update) {
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
        MessagesUpdate::Appended(n) => {
            for message in chat.last_messages(n) {
                messages.push(message_item(&message));
            }
        }
        MessagesUpdate::Reset => messages.set_vec(chat.messages().map(|m| message_item(&m)).collect::<Vec<_>>()),
    }

    if update.conversations {
        let items: Vec<_> = chat.conversations().map(|c| conversation_item(&c, avatars)).collect();
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

fn conversation_item(view: &ConversationView, avatars: &AvatarImages) -> ConversationItem {
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
        avatar: view.avatar.map(|avatar| avatars.get(&c.id, avatar)).unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::{log_filter, PLAINTEXT_TARGETS};
    use std::sync::{Arc, Mutex};

    /// Captures formatted log output so a test can assert on what was written.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Sink;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Runs `emit` under the filter `log_filter` builds from `rust_log`, and returns what the
    /// subscriber wrote.
    fn logged(rust_log: &str, emit: impl FnOnce()) -> String {
        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_env_filter(log_filter(rust_log))
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    /// The site in presage that logs a whole `Content`, whose derived `Debug` prints the
    /// message body in plaintext.
    fn presage_debug() {
        tracing::debug!(target: "presage::manager::registered", answer = 42, "a message body");
    }

    #[test]
    fn rust_log_cannot_turn_on_presage_debug_logging() {
        for rust_log in [
            "debug",
            "trace",
            "presage=debug",
            "presage::manager::registered=trace",
            // Regex directives match anything, so they are dropped outright.
            "/registered/=trace",
            "/^presage.*/=debug",
        ] {
            let logged = logged(rust_log, presage_debug);
            assert!(!logged.contains("42"), "`{rust_log}` logged a body: {logged}");
        }
    }

    /// The other crate that can hold message contents is capped too, not only the one with the
    /// logging site found today: a future logging call must not undo this.
    #[test]
    fn every_plaintext_target_is_capped() {
        for target in PLAINTEXT_TARGETS {
            for rust_log in ["debug", "trace", &format!("{target}=debug")] {
                let logged = logged(rust_log, || {
                    // One callsite per capped crate, since the target must be a literal.
                    match target {
                        "presage" => tracing::debug!(
                            target: "presage::manager::registered",
                            answer = 42,
                            "a message body"
                        ),
                        _ => tracing::debug!(
                            target: "libsignal_service::cipher::decrypt",
                            answer = 42,
                            "a message body"
                        ),
                    }
                });
                assert!(!logged.contains("42"), "`{rust_log}` logged from {target}: {logged}");
            }
        }
    }

    /// Debugging langis itself must keep working, or nobody will ever use `RUST_LOG` again.
    #[test]
    fn rust_log_still_debugs_langis() {
        let debug = || tracing::debug!(target: "langis_core::signal", answer = 42, "a hint");
        assert!(logged("debug", debug).contains("42"));
        assert!(logged("langis_core=debug", debug).contains("42"));
        // Nothing is logged at debug when `RUST_LOG` is unset.
        assert!(!logged("", debug).contains("42"));
    }

    /// `info` still works, so this isn't just "silence everything".
    #[test]
    fn info_is_still_logged() {
        let info = || tracing::info!(target: "presage::manager::registered", answer = 42, "hello");
        assert!(logged("", info).contains("42"));
    }
}
