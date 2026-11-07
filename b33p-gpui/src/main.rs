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
use b33p_platform::{autostart, clipboard, desktop, instance, logging, motion};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::{App, AppContext as _, AsyncApp};

#[cfg(all(feature = "harness", target_family = "unix"))]
mod agent;
// The harness drives the whole app over a socket: test builds only, never a release.
#[cfg(all(feature = "harness", not(debug_assertions)))]
compile_error!("the `harness` feature is for test builds: build without `--release`");
mod actions;
mod activity;
mod animation;
mod app;
mod assets;
mod attachments;
mod bubble;
mod capture;
mod chat_list;
mod composer;
mod confirm;
mod conversation;
mod details;
mod dialog;
#[cfg(test)]
mod e2e_tests;
mod emoji;
mod forward;
mod group_draft;
mod images;
mod join;
mod media;
mod message_info;
mod message_menu;
#[cfg(test)]
mod message_tests;
mod messages;
mod overlays;
mod panel;
mod panels;
mod pins;
mod reaction_bar;
mod search;
mod settings;
mod shared;
mod shutdown;
mod start;
mod stickers;
mod stories;
#[cfg(test)]
mod testing;
mod text;
mod theme;
mod tray;
#[cfg(test)]
mod ui_tests;
mod web;
mod widgets;
mod window;

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
    let activations = instance
        .as_ref()
        .map(|instance| instance.activations.clone());

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
            window::apply_motion(&shared, cx);
            // `--hidden` starts in the tray, unless there is nothing to keep it in.
            let hidden =
                std::env::args().any(|arg| arg == autostart::HIDDEN) && settings.close_to_tray;
            window::start(&shared, activations, hidden, cx);
            shared.refresh(shared::everything(), cx);
            #[cfg(all(feature = "harness", target_family = "unix"))]
            agent::start(&shared, cx);
            spawn_pumps(&shared, events, notifier_actions, quit_asked, cx);
        });
    // Waits for the backend to close the database, which must happen before the process exits.
    drop(backend);
    // Pasted pictures have been sent or given up by now.
    clipboard::clean();
    shutdown.closed();
    // Only now may another b33p start.
    drop(instance);
}

/// Everything that arrives from outside while the window is up: the backend's events, what the
/// user did with a notification, b33p started again, the desktop's reduced-motion preference,
/// and the system's request to quit.
fn spawn_pumps(
    shared: &Rc<Shared>,
    events: async_channel::Receiver<b33p_core::Event>,
    notifier_actions: async_channel::Receiver<Action>,
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
                    cx.background_executor()
                        .timer(Duration::from_millis(1))
                        .await;
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
                    Action::Open {
                        key,
                        activation_token,
                    } => {
                        if let Some(key) = key {
                            shared.open_chat(key, cx);
                        }
                        window::activate(&shared, activation_token, cx);
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
                cx.update(|cx| window::apply_motion(&shared, cx));
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
