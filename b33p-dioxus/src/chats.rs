//! The chat list.

use b33p_core::ConversationId;
use b33p_core::state::{Connection, format_list_time};
use dioxus::prelude::*;

use crate::store::Store;
use crate::widgets::Avatar;

/// What one row of the list shows, copied out of the state so that drawing doesn't hold it.
#[derive(Clone, PartialEq)]
struct Row {
    id: ConversationId,
    title: String,
    preview: String,
    time: String,
    unread: u32,
    marked_unread: bool,
    pinned: bool,
    muted: bool,
    typing: bool,
    request: bool,
}

#[component]
pub fn Chats() -> Element {
    let store = use_context::<Store>();
    let (rows, connection) = {
        let chat = store.chat.read();
        let rows: Vec<Row> = chat
            .conversations()
            .map(|view| Row {
                id: view.conversation.id.clone(),
                title: view.conversation.title.clone(),
                preview: view.preview(),
                time: view
                    .last_message
                    .map(|message| format_list_time(message.timestamp))
                    .unwrap_or_default(),
                unread: view.unread,
                marked_unread: view.marked_unread(),
                pinned: view.pinned(),
                muted: view.muted(),
                typing: !view.typing.is_empty(),
                request: chat.is_request(view.conversation),
            })
            .collect();
        (rows, chat.connection())
    };

    rsx! {
        header { class: "bar",
            h1 { "b33p" }
        }
        match connection {
            Connection::Online => rsx! {},
            Connection::CatchingUp => rsx! { div { class: "banner", "Catching up…" } },
            Connection::Offline => rsx! {
                div { class: "banner warning", "Offline. Trying to connect…" }
            },
        }
        main { class: "list",
            if rows.is_empty() {
                p { class: "empty", "No chats yet." }
            }
            for row in rows {
                ChatRow { key: "{row.id.0}", row }
            }
        }
    }
}

#[component]
fn ChatRow(row: Row) -> Element {
    let store = use_context::<Store>();
    let id = row.id.clone();
    let unread = row.unread > 0 || row.marked_unread;
    let class = if unread { "row unread" } else { "row" };
    rsx! {
        button {
            class,
            onclick: move |_| store.command(|chat| chat.select(&id)),
            Avatar { name: row.title.clone() }
            div { class: "row-text",
                div { class: "row-top",
                    span { class: "title", "{row.title}" }
                    span { class: "time", "{row.time}" }
                }
                div { class: "row-bottom",
                    span { class: "preview",
                        if row.typing {
                            em { "typing…" }
                        } else if row.request {
                            em { "Message request" }
                        } else {
                            "{row.preview}"
                        }
                    }
                    if row.pinned {
                        span { class: "mark", title: "Pinned", "📌" }
                    }
                    if row.muted {
                        span { class: "mark", title: "Muted", "🔕" }
                    }
                    if row.unread > 0 {
                        span { class: "badge", "{row.unread}" }
                    } else if row.marked_unread {
                        span { class: "badge dot" }
                    }
                }
            }
        }
    }
}
