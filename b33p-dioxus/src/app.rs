//! The root: starts the backend, feeds its events to the chat state, and shows the screen the
//! state says is the current one.

use std::rc::Rc;
use std::time::{Duration, Instant};

use b33p_core::state::{ChatState, Screen, until_midnight};
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

/// When what the state shows next changes with the clock alone (ms since the epoch, by the
/// state's clock): someone typing counts as stopped, a "5 min" in the list needs counting on,
/// or midnight turns "14:05" into "Mon" and "Today" into "Yesterday".
/// `to_midnight` is [`until_midnight`], given for tests.
fn next_change(chat: &ChatState, to_midnight: Duration) -> u64 {
    let now = chat.now();
    // A second past it, to be sure the day has turned.
    let midnight = now.saturating_add(to_midnight.as_millis() as u64 + 1_000);
    let recent = chat.shows_recent().then(|| now.saturating_add(30_000));
    // A little after, to be sure it is over.
    let typing = chat.typing_expires().map(|at| at.saturating_add(50));
    [Some(midnight), recent, typing]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(midnight)
}

/// Redraws whatever reads the state when [`next_change`] comes. Runs anew whenever the state
/// changes, so that the time is counted from what it says now.
fn use_clock(store: Store) {
    use_resource(move || async move {
        // Read, not peeked: a change to the state starts this anew.
        let due = next_change(&store.chat.read(), until_midnight());
        // In steps of at most a minute, by the wall clock: a timer's clock stops while the
        // phone sleeps, and would wake far past midnight.
        loop {
            let now = store.chat.peek().now();
            if now >= due {
                break;
            }
            tokio::time::sleep(Duration::from_millis((due - now).min(60_000))).await;
        }
        // Nothing to change, only to draw again: a write tells every reader so.
        let mut chat = store.chat;
        drop(chat.write());
    });
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

    use_clock(store);
    // What a screen reader calls the window is the page's title, which Dioxus's own page sets
    // to "Dioxus app"; `document::Title` only names the native window.
    use_hook(|| {
        document::eval("document.title = 'b33p';");
    });

    let (screen, open) = {
        let chat = store.chat.read();
        (chat.screen().clone(), chat.selected().is_some())
    };
    rsx! {
        // The desktop's title bar ("Dioxus App" otherwise); the page's own title is set below.
        document::Title { "b33p" }
        style { dangerous_inner_html: STYLE }
        div { class: "app",
            match screen {
                Screen::Syncing(stage) => rsx! {
                    SyncingScreen { stage }
                },
                Screen::Password { new, error } => rsx! {
                    PasswordScreen { new, error }
                },
                Screen::Link(qr) => rsx! {
                    LinkScreen { qr }
                },
                Screen::Failed { failure, message } => rsx! {
                    FailedScreen { failure, message }
                },
                Screen::Chats if open => rsx! {
                    // A phone shows one thing at a time: the list, or the chat opened from it.
                    ErrorBanner {}
                    Conversation {}
                },
                Screen::Chats => rsx! {
                    ErrorBanner {}
                    Chats {}
                },
            }
        }
    }
}

/// What went wrong with something asked of the backend (a send, a download, joining a group),
/// until it is dismissed or tried again.
#[component]
fn ErrorBanner() -> Element {
    let store = use_context::<Store>();
    let (error, can_retry) = {
        let chat = store.chat.read();
        (chat.error(), chat.can_retry_error())
    };
    let Some(error) = error else {
        return rsx! {};
    };
    rsx! {
        div { class: "error-banner", role: "alert",
            span { class: "error-text", "{error}" }
            if can_retry {
                button {
                    class: "link",
                    onclick: move |_| store.command(|chat| chat.retry_error()),
                    "Try again"
                }
            }
            button {
                class: "icon dismiss",
                "aria-label": "Dismiss",
                onclick: move |_| store.act(|chat| chat.dismiss_error()),
                "×"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use b33p_core::state::Clock;
    use b33p_core::{ChatMessage, Conversation, ConversationId};

    const NOW: u64 = 1_800_000_000_000;
    const HOUR: Duration = Duration::from_secs(3600);

    fn chat() -> ChatState {
        let mut chat = ChatState::default();
        chat.set_clock(Clock::fixed(NOW));
        chat.apply(Event::Linked {
            account: "me".into(),
            service_id: "me".into(),
        });
        chat.apply(Event::Synced);
        chat.apply(Event::Conversations(vec![Conversation {
            id: ConversationId("a".into()),
            title: "Ann".into(),
            ..Conversation::default()
        }]));
        chat
    }

    #[test]
    fn with_nothing_counting_down_the_next_change_is_midnight() {
        assert_eq!(next_change(&chat(), HOUR), NOW + 3_600_000 + 1_000);
    }

    #[test]
    fn someone_typing_is_redrawn_when_it_runs_out() {
        let mut chat = chat();
        chat.apply(Event::Typing {
            conversation: ConversationId("a".into()),
            sender_id: "ann".into(),
            sender: "Ann".into(),
            typing: true,
        });
        let expires = chat.typing_expires().expect("Ann is typing");
        assert_eq!(next_change(&chat, HOUR), expires + 50);
        // Unless midnight comes first.
        assert_eq!(next_change(&chat, Duration::ZERO), NOW + 1_000);
    }

    #[test]
    fn a_time_from_the_last_hour_is_counted_on_every_half_minute() {
        let mut chat = chat();
        chat.apply(Event::Message(ChatMessage {
            conversation: ConversationId("a".into()),
            sender_id: "ann".into(),
            sender: "Ann".into(),
            body: "hi".into(),
            timestamp: NOW - 120_000,
            ..ChatMessage::default()
        }));
        assert!(chat.shows_recent());
        assert_eq!(next_change(&chat, HOUR), NOW + 30_000);
    }
}
