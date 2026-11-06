// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::rc::Rc;
use std::time::Duration;

use b33p_core::settings::Settings;
use b33p_core::state::{ChatState, Update};
use b33p_core::{Backend, Config, ConversationId};
use b33p_notify::{Action, AppInfo, Notifier};
use b33p_platform::{clipboard, desktop, instance, logging, motion};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::{
    App, AppContext as _, AsyncApp, Bounds, TitlebarOptions, WindowBounds, WindowDecorations,
    WindowOptions, px, size,
};

#[cfg(all(feature = "harness", target_family = "unix"))]
mod agent;
// The harness drives the whole app over a socket: test builds only, never a release.
#[cfg(all(feature = "harness", not(debug_assertions)))]
compile_error!("the `harness` feature is for test builds: build without `--release`");
mod actions;
mod activity;
mod app;
mod assets;
mod bubble;
mod chat_list;
mod composer;
mod confirm;
mod conversation;
mod dialog;
mod forward;
mod group_draft;
mod emoji;
mod images;
mod join;
mod details;
mod media;
mod overlays;
mod panel;
mod panels;
mod search;
mod settings;
mod shared;
mod shutdown;
mod start;
mod stories;
mod stickers;
mod theme;
mod widgets;

use shared::Shared;

const APP: AppInfo = AppInfo {
    id: "io.github.barafael.B33p",
    name: "b33p",
    #[cfg(windows)]
    icon: include_bytes!("../../b33p-slint/assets/b33p.ico"),
    #[cfg(not(windows))]
    icon: &[],
};

/// What `--version` and `--help` print, answered before anything starts. (A Windows release
/// has no console to print to.)
fn command_line_answer() -> Option<String> {
    match std::env::args().nth(1)?.as_str() {
        "--version" | "-V" => Some(format!("b33p {}", env!("CARGO_PKG_VERSION"))),
        "--unregister" => {
            if let Err(error) = b33p_platform::autostart::set(false) {
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

fn main() {
    if let Some(answer) = command_line_answer() {
        println!("{answer}");
        return;
    }
    let settings = Settings::load();
    logging::init();

    // Before the backend opens the database.
    let instance = match instance::claim(Backend::mock_requested()) {
        instance::Claim::First(primary) => Some(primary),
        instance::Claim::Second => {
            tracing::info!("b33p is already running; showing it instead");
            return;
        }
        instance::Claim::Unchecked => None,
    };
    let (shutdown, quit_asked) = shutdown::watch();
    clipboard::clean();
    // Not joined at the end: a dialog may still be open when b33p quits.
    let (desktop_requests, desktop_queue) = async_channel::unbounded();
    std::thread::spawn(move || {
        desktop::Desktop.event_loop(desktop_queue);
    });

    let config = Config {
        device_name: settings.device_name(),
        preferences: settings.preferences,
    };
    let backend = Backend::from_env(config);
    let (notifier, notifier_actions) = Notifier::new(&APP);
    let events = backend.events();
    let commands = backend.commands();
    let activations = instance.as_ref().map(|instance| instance.activations.clone());

    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            actions::bind(cx);
            theme::apply(settings.theme, None, cx);
            let mut chat = ChatState::default();
            chat.set_notifications(settings.notifications, settings.notification_content);
            let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
            let shared = Shared::new(
                chat,
                notifier,
                commands,
                settings.clone(),
                desktop_requests,
                scroller,
            );
            if let Err(error) = open_window(&shared, cx) {
                tracing::error!(error = %b33p_core::scrub::scrubbed(&error), "can't open the window");
                cx.quit();
                return;
            }
            // Closing the window ends b33p, until there is a tray to keep it in.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            shared.refresh(shared::everything(), cx);
            #[cfg(all(feature = "harness", target_family = "unix"))]
            agent::start(&shared, cx);
            spawn_pumps(&shared, events, notifier_actions, activations, quit_asked, cx);
        });
    // Waits for the backend to close the database, which must happen before the process exits.
    drop(backend);
    // Pasted pictures have been sent or given up by now.
    clipboard::clean();
    shutdown.closed();
    // Only now may another b33p start.
    drop(instance);
}

/// Opens the window, with the views that draw the shared state.
fn open_window(shared: &Rc<Shared>, cx: &mut App) -> anyhow_free::Result<()> {
    let title = if b33p_core::Backend::mock_requested() {
        "b33p · MOCK"
    } else {
        "b33p"
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1000.), px(680.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(360.), px(480.))),
        app_id: Some(APP.id.into()),
        window_decorations: Some(WindowDecorations::Server),
        ..WindowOptions::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| app::AppView::new(shared.clone(), window, cx))
    })?;
    cx.activate(true);
    Ok(())
}

/// Result without a dependency on anyhow, which GPUI's errors come in.
mod anyhow_free {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
}

/// Everything that arrives from outside while the window is up: the backend's events, what the
/// user did with a notification, b33p started again, the desktop's reduced-motion preference,
/// and the system's request to quit.
fn spawn_pumps(
    shared: &Rc<Shared>,
    events: async_channel::Receiver<b33p_core::Event>,
    notifier_actions: async_channel::Receiver<Action>,
    activations: Option<async_channel::Receiver<Option<String>>>,
    quit_asked: async_channel::Receiver<()>,
    cx: &mut App,
) {
    // Await backend events on the UI thread and apply each burst (e.g. the initial sync) as
    // one batch, so the window is drawn once per batch. Nothing polls: idle means idle. A long
    // burst (catching up after days away) is cut into batches of a few milliseconds, with the
    // window drawn between them, so it doesn't freeze meanwhile.
    cx.spawn({
        let shared = shared.clone();
        async move |cx: &mut AsyncApp| {
            const BATCH_TIME: Duration = Duration::from_millis(8);
            while let Ok(first) = events.recv().await {
                let started = std::time::Instant::now();
                cx.update(|cx| {
                    let batch = std::iter::once(first).chain(std::iter::from_fn(|| {
                        (started.elapsed() < BATCH_TIME)
                            .then(|| events.try_recv().ok())
                            .flatten()
                    }));
                    shared.apply_events(batch, cx);
                });
                if !events.is_empty() {
                    // The window is drawn between batches.
                    cx.background_executor().timer(Duration::from_millis(1)).await;
                }
            }
            // The channel only closes early if the backend thread died without reporting why
            // (a panic).
            cx.update(|cx| shared.backend_gone(cx));
        }
    })
    .detach();

    // What the user did with a notification: open its chat, or send the answer typed into it.
    cx.spawn({
        let shared = shared.clone();
        async move |cx: &mut AsyncApp| {
            while let Ok(action) = notifier_actions.recv().await {
                cx.update(|cx| match action {
                    Action::Open { key, .. } => {
                        if let Some(key) = key {
                            shared.open_chat(key, cx);
                        }
                        cx.activate(true);
                    }
                    Action::Reply { key, text } => {
                        shared.act(cx, |chat| {
                            let (command, update) = chat.reply(&ConversationId(key), &text);
                            let read = Update {
                                conversations: true,
                                ..Update::default()
                            };
                            (command, update.merge(read))
                        });
                    }
                });
            }
        }
    })
    .detach();

    // Starting b33p again shows this one.
    if let Some(activations) = activations {
        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Ok(token) = activations.recv().await {
                tracing::info!(token = token.is_some(), "started again; showing the window");
                cx.update(|cx| cx.activate(true));
            }
        })
        .detach();
    }

    // The desktop's reduced-motion preference, as it is and as it changes.
    let system_motion = motion::watch();
    cx.spawn({
        let shared = shared.clone();
        async move |cx: &mut AsyncApp| {
            // Kept for as long as it is listened to.
            let _watching = &system_motion;
            while let Ok(reduced) = system_motion.reduced.recv().await {
                tracing::debug!(reduced, "the desktop's reduced-motion preference");
                shared.system_reduces_motion.set(reduced);
                cx.update(|cx| cx.refresh_windows());
            }
        }
    })
    .detach();

    // The system asks b33p to quit: logout, `kill`, Ctrl+C in the terminal.
    cx.spawn(async move |cx: &mut AsyncApp| {
        if quit_asked.recv().await.is_ok() {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}
