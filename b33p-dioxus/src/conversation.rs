//! An open chat: its messages, and the composer.

use b33p_core::state::format_clock;
use b33p_core::{AttachmentKind, ConversationId, Delivery, RequestResponse, Unsent};
use dioxus::prelude::*;

use crate::store::Store;
use crate::widgets::Avatar;

/// What one message shows, copied out of the state so that drawing doesn't hold it.
#[derive(Clone, PartialEq)]
struct Row {
    /// Where it is among the messages, which is how the state names a message to act on.
    index: usize,
    key: String,
    day: Option<String>,
    first_unread: bool,
    notice: bool,
    outgoing: bool,
    sender: Option<String>,
    first_of_run: bool,
    last_of_run: bool,
    quote: Option<(String, String)>,
    attachments: Vec<String>,
    body: String,
    deleted: bool,
    edited: bool,
    time: String,
    delivery: Delivery,
    unsent: Option<Unsent>,
    reactions: Vec<(String, usize)>,
}

fn attachment_label(kind: AttachmentKind, file_name: Option<&str>) -> String {
    match kind {
        AttachmentKind::Image => "🖼 Photo".into(),
        AttachmentKind::Video => "🎞 Video".into(),
        AttachmentKind::Audio => "🎵 Audio".into(),
        AttachmentKind::VoiceNote => "🎤 Voice message".into(),
        AttachmentKind::Sticker => "Sticker".into(),
        AttachmentKind::LongText => "📄 Long message".into(),
        AttachmentKind::ViewOnce => "👁 View once: open it on your phone".into(),
        AttachmentKind::File => format!("📎 {}", file_name.unwrap_or("File")),
    }
}

/// Reactions as Signal's apps show them: each emoji once, with how many chose it.
fn tally(reactions: &[b33p_core::Reaction]) -> Vec<(String, usize)> {
    let mut tally: Vec<(String, usize)> = Vec::new();
    for reaction in reactions {
        match tally.iter_mut().find(|(emoji, _)| *emoji == reaction.emoji) {
            Some((_, count)) => *count += 1,
            None => tally.push((reaction.emoji.clone(), 1)),
        }
    }
    tally
}

#[component]
pub fn Conversation() -> Element {
    let store = use_context::<Store>();
    // What the textarea starts with: the draft kept for this chat. After that it is the
    // textarea's own, and `draft` only follows it (see `sent`).
    let initial_draft = use_hook(|| store.chat.peek().draft().to_owned());
    let mut draft = use_signal(|| initial_draft.clone());
    // Raised on every send, so that the textarea is made anew and empty.
    let mut sent = use_signal(|| 0u32);

    let (id, title, subtitle, typing, request, can_write, rows) = {
        let chat = store.chat.read();
        let Some(conversation) = chat.selected() else {
            return rsx! {};
        };
        let typing = chat.typing(&conversation.id).join(", ");
        let rows: Vec<Row> = chat
            .messages()
            .enumerate()
            .map(|(index, view)| {
                let message = view.message;
                Row {
                    index,
                    key: format!("{}-{}", message.timestamp, message.sender_id),
                    day: view.day.clone(),
                    first_unread: view.first_unread,
                    notice: message.notice,
                    outgoing: message.outgoing,
                    sender: view.show_sender.then(|| message.sender.clone()),
                    first_of_run: view.first_of_run,
                    last_of_run: view.last_of_run,
                    quote: message
                        .quote
                        .as_ref()
                        .map(|quote| (quote.author.clone(), quote.text.clone())),
                    attachments: message
                        .attachments
                        .iter()
                        .map(|a| attachment_label(a.kind, a.file_name.as_deref()))
                        .collect(),
                    body: view.body.to_owned(),
                    deleted: message.deleted,
                    edited: message.edited_at.is_some(),
                    time: format_clock(message.timestamp),
                    delivery: message.delivery,
                    unsent: message.unsent,
                    reactions: tally(&message.reactions),
                }
            })
            .collect();
        (
            conversation.id.clone(),
            conversation.title.clone(),
            conversation.subtitle.clone(),
            typing,
            chat.is_request(conversation),
            chat.can_write_to(conversation),
            rows,
        )
    };

    // Android's Back button goes back in the WebView's history, when it has some, and leaves the
    // app when it has none. An open chat is one step of that history: Back closes it, as the
    // arrow in the bar does.
    use_hook(move || {
        spawn(async move {
            let mut navigation = document::eval(
                r#"
                history.pushState({ b33pChat: true }, "");
                window.addEventListener("popstate", () => dioxus.send("back"), { once: true });
                "#,
            );
            if navigation.recv::<String>().await.is_ok() {
                store.act(|chat| chat.deselect());
            }
        });
    });

    // The keyboard opening shrinks the page, which leaves the newest messages behind it, unless
    // the page was at the bottom: then it stays there. Someone reading further up isn't moved.
    // An effect, so that it runs once the page is there to find.
    use_effect(|| {
        document::eval(
            r#"
            const messages = document.getElementById("messages");
            if (messages) {
                let atBottom = true;
                messages.addEventListener("scroll", () => {
                    atBottom = messages.scrollHeight - messages.scrollTop - messages.clientHeight < 80;
                });
                if (window.b33pKeepBottom) window.removeEventListener("resize", window.b33pKeepBottom);
                window.b33pKeepBottom = () => {
                    if (atBottom) messages.scrollTop = messages.scrollHeight;
                };
                window.addEventListener("resize", window.b33pKeepBottom);
            }
            "#,
        );
    });

    // Show the newest message when a chat opens and when one arrives.
    let count = rows.len();
    use_effect(use_reactive!(|count| {
        if count > 0 {
            document::eval(
                "const m = document.getElementById('messages'); if (m) m.scrollTop = m.scrollHeight;",
            );
        }
    }));

    // The textarea made anew by a send has no focus: give it back, so that the keyboard stays.
    let sends = sent();
    use_effect(use_reactive!(|sends| {
        if sends > 0 {
            document::eval("document.getElementById('draft')?.focus();");
        }
    }));

    let can_compose = store.chat.read().can_compose(&draft.read());
    let mut send = move || {
        let text = draft.read().clone();
        if store.chat.peek().can_compose(&text) {
            store.command(|chat| chat.compose(&text).0);
            draft.set(String::new());
            sent += 1;
        }
    };

    rsx! {
        header { class: "bar",
            button {
                class: "icon back",
                "aria-label": "Back to chats",
                onclick: move |_| {
                    // The history entry the chat made goes with it.
                    document::eval("history.back();");
                    store.act(|chat| chat.deselect());
                },
                "‹"
            }
            Avatar { name: title.clone(), small: true }
            div { class: "bar-title",
                div { class: "title", "{title}" }
                div { class: "subtitle",
                    if typing.is_empty() {
                        "{subtitle}"
                    } else {
                        em { "{typing} typing…" }
                    }
                }
            }
        }
        main { id: "messages", class: "messages",
            for row in rows {
                Message { key: "{row.key}", row }
            }
        }
        if request {
            RequestBar { id: id.clone() }
        } else if can_write {
            form {
                class: "composer",
                onsubmit: move |event| {
                    event.prevent_default();
                    send();
                },
                // Uncontrolled: a value written back into the field races with the keystrokes
                // typed meanwhile (and with the keyboard's composition), and drops them.
                textarea {
                    key: "{sent}",
                    id: "draft",
                    rows: "1",
                    placeholder: "Message",
                    oninput: move |event| {
                        let text = event.value();
                        store.command(|chat| chat.draft_changed(&text));
                        draft.set(text);
                    },
                    "{initial_draft}"
                }
                button {
                    r#type: "submit",
                    class: "send",
                    "aria-label": "Send",
                    disabled: !can_compose,
                    "➤"
                }
            }
        } else {
            p { class: "readonly", "You can't send messages to this chat." }
        }
    }
}

#[component]
fn Message(row: Row) -> Element {
    let store = use_context::<Store>();
    if row.notice {
        return rsx! {
            if let Some(day) = &row.day {
                div { class: "day", "{day}" }
            }
            div { class: "notice", "{row.body}" }
        };
    }
    let mut class = String::from("bubble");
    class.push_str(if row.outgoing { " out" } else { " in" });
    if row.first_of_run {
        class.push_str(" first");
    }
    if row.last_of_run {
        class.push_str(" last");
    }
    if row.deleted {
        class.push_str(" deleted");
    }
    let index = row.index;
    rsx! {
        if let Some(day) = &row.day {
            div { class: "day", "{day}" }
        }
        if row.first_unread {
            div { class: "day unread-marker", "Unread messages" }
        }
        div { class: if row.outgoing { "line out" } else { "line in" },
            div { class,
                if let Some(sender) = &row.sender {
                    div { class: "sender", "{sender}" }
                }
                if let Some((author, text)) = &row.quote {
                    div { class: "quote",
                        div { class: "quote-author", "{author}" }
                        div { class: "quote-text", "{text}" }
                    }
                }
                for label in &row.attachments {
                    div { class: "attachment", "{label}" }
                }
                if !row.body.is_empty() {
                    div { class: "text", "{row.body}" }
                }
                div { class: "meta",
                    if row.edited {
                        span { "edited" }
                    }
                    span { "{row.time}" }
                    if row.outgoing {
                        match row.unsent {
                            Some(Unsent::Sending) => rsx! { span { class: "tick", "…" } },
                            Some(Unsent::Failed) => rsx! {
                                button {
                                    class: "retry",
                                    onclick: move |_| store.command(|chat| chat.retry_send(index)),
                                    "Not sent. Tap to try again"
                                }
                            },
                            Some(Unsent::Refused) => rsx! {
                                span { class: "retry", "Signal refused this message" }
                            },
                            None => rsx! {
                                span {
                                    class: if row.delivery == Delivery::Read { "tick read" } else { "tick" },
                                    match row.delivery {
                                        Delivery::Sent => "✓",
                                        Delivery::Delivered | Delivery::Read => "✓✓",
                                    }
                                }
                            },
                        }
                    }
                }
                if !row.reactions.is_empty() {
                    div { class: "reactions",
                        for (emoji , count) in &row.reactions {
                            span { class: "reaction",
                                "{emoji}"
                                if *count > 1 {
                                    " {count}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A chat from someone not in our contacts: it waits for an answer before anything is sent.
#[component]
fn RequestBar(id: ConversationId) -> Element {
    let store = use_context::<Store>();
    let respond = move |response: RequestResponse| {
        let id = id.clone();
        move |_| store.command(|chat| chat.respond(&id, response))
    };
    rsx! {
        div { class: "request",
            p { "This person isn't in your contacts. Accepting shares your name and picture with them." }
            div { class: "actions",
                button { class: "primary", onclick: respond(RequestResponse::Accept), "Accept" }
                button { class: "secondary", onclick: respond(RequestResponse::Block), "Block" }
                button { class: "danger", onclick: respond(RequestResponse::Delete), "Delete" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use b33p_core::Reaction;

    fn reaction(emoji: &str, sender: &str) -> Reaction {
        Reaction {
            emoji: emoji.into(),
            sender_id: sender.into(),
            sender: sender.into(),
            outgoing: false,
        }
    }

    #[test]
    fn reactions_are_counted_per_emoji_in_the_order_they_came() {
        let reactions = [
            reaction("👍", "a"),
            reaction("❤️", "b"),
            reaction("👍", "c"),
            reaction("👍", "d"),
        ];
        assert_eq!(
            tally(&reactions),
            vec![("👍".to_owned(), 3), ("❤️".to_owned(), 1)]
        );
        assert!(tally(&[]).is_empty());
    }

    #[test]
    fn attachments_are_named_by_what_they_are() {
        assert_eq!(attachment_label(AttachmentKind::Image, None), "🖼 Photo");
        assert_eq!(
            attachment_label(AttachmentKind::File, Some("notes.pdf")),
            "📎 notes.pdf"
        );
        // A file with no name still says it is a file.
        assert_eq!(attachment_label(AttachmentKind::File, None), "📎 File");
    }
}
