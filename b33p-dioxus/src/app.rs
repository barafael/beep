//! The root: starts the backend, feeds its events to the chat state, and shows the screen the
//! state says is the current one.

use std::rc::Rc;
use std::time::{Duration, Instant};

use b33p_core::state::Screen;
use b33p_core::{Backend, Config, Event};
use dioxus::prelude::*;

use crate::chats::Chats;
use crate::conversation::Conversation;
use crate::screens::{FailedScreen, LinkScreen, PasswordScreen, SyncingScreen};
use crate::store::Store;

const STYLE: &str = include_str!("style.css");

/// The backend this app talks to: Signal, or the offline mock for a build without `signal`.
fn start_backend() -> Backend {
    #[cfg(feature = "signal")]
    {
        Backend::start(Config {
            device_name: "b33p on Android".into(),
            ..Config::default()
        })
    }
    #[cfg(not(feature = "signal"))]
    {
        drop(Config::default());
        Backend::mock()
    }
}

/// Gives the screen its turn: the future is pending once, and wakes itself at once.
async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}

pub fn app() -> Element {
    // Dropped with the root, which stops the backend and waits until it has closed its database.
    let backend = use_hook(|| Rc::new(start_backend()));
    let store = use_context_provider({
        let backend = backend.clone();
        move || Store::new(backend.commands())
    });

    use_future({
        let backend = backend.clone();
        move || {
            let events = backend.events();
            async move {
                // A burst (the first sync brings an event per chat) is one change and one
                // drawing, but a long one is cut so that the screen doesn't freeze meanwhile.
                const BATCH_TIME: Duration = Duration::from_millis(8);
                while let Ok(first) = events.recv().await {
                    let started = Instant::now();
                    let mut batch: Vec<Event> = vec![first];
                    while started.elapsed() < BATCH_TIME
                        && let Ok(event) = events.try_recv()
                    {
                        batch.push(event);
                    }
                    store.apply(batch);
                    if !events.is_empty() {
                        // Let the screen be drawn between batches.
                        yield_now().await;
                    }
                }
                // The channel only closes early if the backend thread died without saying why.
                store.backend_gone();
            }
        }
    });

    let (screen, open) = {
        let chat = store.chat.read();
        (chat.screen().clone(), chat.selected().is_some())
    };
    rsx! {
        style { dangerous_inner_html: STYLE }
        div { class: "app",
            match screen {
                Screen::Syncing(stage) => rsx! { SyncingScreen { stage } },
                Screen::Password { new, error } => rsx! { PasswordScreen { new, error } },
                Screen::Link(qr) => rsx! { LinkScreen { qr } },
                Screen::Failed { failure, message } => rsx! { FailedScreen { failure, message } },
                // A phone shows one thing at a time: the list, or the chat opened from it.
                Screen::Chats if open => rsx! { Conversation {} },
                Screen::Chats => rsx! { Chats {} },
            }
        }
    }
}
