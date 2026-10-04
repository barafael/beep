use std::{sync::Arc, time::Duration};

use crate::{Attachment, AttachmentContent, AttachmentId, AttachmentKind, Picture, ChatMessage, Quote, HistoryTransfer, Command, Conversation, ConversationId, Event, Events, Inbox, QrMatrix, now_millis};

const CONVERSATIONS: &[(&str, &str, bool)] = &[
    ("alice", "Alice", false),
    ("rust", "Rustaceans 🦀", true),
    ("bob", "Bob", false),
];

pub fn run(commands: Inbox, events: Events) {
    // `LANGIS_MOCK_PASSWORD=1` simulates a desktop without a keyring; the password is "password".
    if std::env::var_os("LANGIS_MOCK_PASSWORD").is_some() {
        let mut request = Event::PasswordRequired { new: false, error: None };
        loop {
            let _ = events.send(request);
            match commands.iter().find_map(|c| if let Command::Unlock(p) = c { Some(p) } else { None }) {
                Some(p) if p.expose() == "password" => break,
                Some(_) => request = Event::PasswordRequired { new: false, error: Some("Wrong password.".into()) },
                None => return,
            }
        }
    }

    let url = "sgnl://linkdevice?uuid=mock&pub_key=mock";
    let _ = events.send(Event::LinkQr(QrMatrix::encode(url).unwrap()));
    std::thread::sleep(Duration::from_secs(2));
    let _ = events.send(Event::Linked { account: "+49 151 0000000 (mock)".into() });
    // `LANGIS_MOCK_TRANSFER=1` walks through a message history transfer; the wait for the
    // phone takes 6 seconds and can be skipped.
    // Commands that arrive meanwhile are handled afterwards, as in the real backend.
    let mut queued = std::collections::VecDeque::new();
    if std::env::var_os("LANGIS_MOCK_TRANSFER").is_some() {
        let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Waiting));
        let until = std::time::Instant::now() + Duration::from_secs(6);
        let mut skipped = false;
        while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
            match commands.recv_timeout(left) {
                Ok(Command::SkipHistory) => {
                    skipped = true;
                    break;
                }
                Ok(command) => queued.push_back(command),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        }
        if !skipped {
            for stage in [
                HistoryTransfer::Downloading,
                HistoryTransfer::Importing { messages: 500 },
                HistoryTransfer::Importing { messages: 1000 },
            ] {
                let _ = events.send(Event::HistoryTransfer(stage));
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Finished));
    }
    let conversations = CONVERSATIONS
        .iter()
        .map(|&(id, title, is_group)| Conversation {
            id: ConversationId(id.into()),
            title: title.into(),
            is_group,
        })
        .collect();
    let _ = events.send(Event::Conversations(conversations));
    let _ = events.send(Event::Avatar { conversation: ConversationId("alice".into()), avatar: Arc::new(gradient(96)) });
    let _ = events.send(Event::Synced);

    // Fixed at startup so repeated history loads return identical messages.
    let started = now_millis();
    // Like the real backend: each conversation's newest message, for sorting and previews.
    for &(id, _, _) in CONVERSATIONS {
        if let Some(latest) = history(&ConversationId(id.into()), started).pop() {
            let _ = events.send(Event::Latest(latest));
        }
    }

    loop {
        let next = match queued.pop_front() {
            Some(command) => Ok(command),
            None => commands.recv_timeout(Duration::from_secs(20)),
        };
        match next {
            Ok(Command::Unlock(_) | Command::SkipHistory) => {}
            // Every mock attachment is the same picture; saving pretends to succeed.
            Ok(Command::FetchAttachment(attachment)) => {
                std::thread::sleep(Duration::from_millis(400));
                let content = match attachment.kind {
                    AttachmentKind::LongText => Ok(AttachmentContent::Text(format!("{} (and the rest)", "A long message ".repeat(20)))),
                    _ => Ok(AttachmentContent::Picture(Arc::new(gradient(320)))),
                };
                let _ = events.send(Event::Attachment { id: attachment.id, content });
            }
            Ok(Command::SaveAttachment(attachment)) => {
                let path = Ok(std::env::temp_dir().join(attachment.file_name.unwrap_or_else(|| "attachment".into())));
                let _ = events.send(Event::AttachmentSaved { id: attachment.id, path });
            }
            Ok(Command::LoadHistory(id)) => {
                let messages = history(&id, started);
                let _ = events.send(Event::History { conversation: id, messages });
            }
            Ok(Command::Send { conversation, body }) => {
                let echo = format!("You said: {body}");
                let _ = events.send(Event::Message(message(&conversation.0, "You", &body, now_millis(), true)));
                std::thread::sleep(Duration::from_millis(600));
                let _ = events.send(Event::Message(message(&conversation.0, "Echo", &echo, now_millis(), false)));
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let _ = events.send(Event::Message(message("bob", "Bob", "ping", now_millis(), false)));
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// A diagonal gradient, standing in for a profile picture.
fn gradient(side: u32) -> Picture {
    let rgba = (0..side * side)
        .flat_map(|i| {
            let (x, y) = (i % side, i / side);
            let t = ((x + y) * 255 / (2 * side - 2)) as u8;
            [255 - t / 2, 120 + t / 3, t, 255]
        })
        .collect();
    Picture { width: side, height: side, rgba }
}

fn message(conversation: &str, sender: &str, body: &str, timestamp: u64, outgoing: bool) -> ChatMessage {
    ChatMessage {
        conversation: ConversationId(conversation.into()),
        sender_id: sender.to_lowercase(),
        sender: sender.into(),
        body: body.into(),
        timestamp,
        outgoing,
        ..Default::default()
    }
}

/// The stored history of a mock conversation, relative to `started`.
fn history(id: &ConversationId, started: u64) -> Vec<ChatMessage> {
    let ago = |minutes: u64| started - minutes * 60_000;
    match id.0.as_str() {
        "alice" => vec![
            message("alice", "Alice", "Hey! Did you try the new client?", ago(42), false),
            message("alice", "You", "Building it right now, in Rust.", ago(40), true),
            ChatMessage {
                quote: Some(Quote { author: "You".into(), text: "Building it right now, in Rust.".into() }),
                ..message("alice", "Alice", "Of course you are 😄", ago(39), false)
            },
            ChatMessage {
                attachments: vec![attachment("photo", AttachmentKind::Image, "image/jpeg", None, (800, 600))],
                ..message("alice", "Alice", "The view from the hut", ago(30), false)
            },
            ChatMessage {
                attachments: vec![attachment("notes", AttachmentKind::File, "application/pdf", Some("route-notes.pdf"), (0, 0))],
                ..message("alice", "You", "", ago(29), true)
            },
        ],
        "rust" => vec![
            message("rust", "Ferris", "Slint or iced?", ago(120), false),
            message("rust", "Carol", "Slint, if you want it to look native.", ago(118), false),
            ChatMessage {
                attachments: vec![attachment("long", AttachmentKind::LongText, "text/x-signal-plain", None, (0, 0))],
                ..message("rust", "Ferris", "A long message A long message A long…", ago(117), false)
            },
            ChatMessage {
                attachments: vec![attachment("sticker", AttachmentKind::Sticker, "image/webp", None, (512, 512))],
                ..message("rust", "Carol", "", ago(116), false)
            },
        ],
        _ => Vec::new(),
    }
}

fn attachment(id: &str, kind: AttachmentKind, content_type: &str, file_name: Option<&str>, (width, height): (u32, u32)) -> Attachment {
    Attachment {
        id: AttachmentId(id.into()),
        kind,
        content_type: content_type.into(),
        file_name: file_name.map(Into::into),
        size: 245_760,
        width,
        height,
        pointer: Arc::from(Vec::new()),
    }
}
