use std::{sync::Arc, time::Duration};

use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, ChatMessage, ChatSettings,
    Command, Conversation, ConversationId, Delivery, Event, Events, HistoryTransfer, Inbox, Person,
    Picture, QrMatrix, Quote, Reaction, Style, StyleRange, now_millis,
};

const CONVERSATIONS: &[(&str, &str, bool)] = &[
    ("alice", "Alice", false),
    ("rust", "Rustaceans 🦀", true),
    ("bob", "Bob", false),
    ("dana", "Dana", false),
    ("me", "Note to Self", false),
];

pub fn run(commands: Inbox, events: Events) {
    // `LANGIS_MOCK_PASSWORD=1` simulates a desktop without a keyring; the password is "password".
    if cfg!(debug_assertions) && std::env::var_os("LANGIS_MOCK_PASSWORD").is_some() {
        let mut request = Event::PasswordRequired {
            new: false,
            error: None,
        };
        loop {
            let _ = events.send(request);
            match commands.iter().find_map(|c| {
                if let Command::Unlock(p) = c {
                    Some(p)
                } else {
                    None
                }
            }) {
                Some(p) if p.expose() == "password" => break,
                Some(_) => {
                    request = Event::PasswordRequired {
                        new: false,
                        error: Some("Wrong password.".into()),
                    }
                }
                None => return,
            }
        }
    }

    let url = "sgnl://linkdevice?uuid=mock&pub_key=mock";
    let _ = events.send(Event::LinkQr(QrMatrix::encode(url).unwrap()));
    std::thread::sleep(Duration::from_secs(2));
    let _ = events.send(Event::Linked {
        account: "+49 151 0000000 (mock)".into(),
    });
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
            subtitle: if is_group {
                "4 members".into()
            } else if id == "me" {
                "Messages to yourself".into()
            } else {
                "+49 151 23456789".into()
            },
            settings: ChatSettings {
                pinned: (id == "rust").then_some(0),
                archived: id == "dana",
                muted_until: (id == "bob").then_some(u64::MAX),
                ..ChatSettings::default()
            },
            expire_timer: if id == "alice" { 3600 } else { 0 },
            note_to_self: id == "me",
            members: if is_group {
                ["Carol", "Dana", "Eve", "Ferris"]
                    .map(|name| Person {
                        service_id: name.to_lowercase(),
                        name: name.into(),
                    })
                    .into()
            } else {
                Vec::new()
            },
        })
        .collect();
    let _ = events.send(Event::Conversations(conversations));
    let _ = events.send(Event::Avatar {
        conversation: ConversationId("alice".into()),
        avatar: Arc::new(gradient(96)),
    });
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
            // The state changes these settings itself; nothing to keep here.
            Ok(
                Command::Unlock(_)
                | Command::SkipHistory
                | Command::Typing { .. }
                | Command::MarkRead { .. }
                | Command::MarkUnread(_)
                | Command::SetPins(_)
                | Command::Mute { .. }
                | Command::Archive { .. }
                | Command::SetTimer { .. }
                | Command::Preferences(_),
            ) => {}
            Ok(Command::Edit {
                conversation,
                target,
                body,
                styles,
                mentions: _,
            }) => {
                let message = ChatMessage {
                    styles,
                    edited_at: Some(now_millis()),
                    ..message(&conversation.0, "You", &body, target.timestamp, true)
                };
                let _ = events.send(Event::Edited {
                    conversation,
                    target,
                    message,
                });
            }
            Ok(Command::Delete {
                conversation,
                target,
                for_everyone: true,
            }) => {
                let _ = events.send(Event::Deleted {
                    conversation,
                    target,
                });
            }
            Ok(Command::Delete { .. }) => {}
            Ok(Command::React {
                conversation,
                target,
                emoji,
                remove,
            }) => {
                let reaction = Reaction {
                    emoji,
                    sender_id: "you".into(),
                    sender: "You".into(),
                    outgoing: true,
                };
                let _ = events.send(Event::Reaction {
                    conversation,
                    target,
                    reaction,
                    remove,
                });
            }
            // Every mock attachment is the same picture; saving pretends to succeed.
            Ok(Command::FetchAttachment(attachment)) => {
                std::thread::sleep(Duration::from_millis(400));
                let content = match attachment.kind {
                    AttachmentKind::LongText => Ok(AttachmentContent::Text(format!(
                        "{} (and the rest)",
                        "A long message ".repeat(20)
                    ))),
                    _ => Ok(AttachmentContent::Picture(Arc::new(gradient(320)))),
                };
                let _ = events.send(Event::Attachment {
                    id: attachment.id,
                    content,
                });
            }
            Ok(Command::SaveAttachment(attachment)) => {
                let path = Ok(std::env::temp_dir()
                    .join(attachment.file_name.unwrap_or_else(|| "attachment".into())));
                let _ = events.send(Event::AttachmentSaved {
                    id: attachment.id,
                    path,
                });
            }
            // In pages, like the real backend.
            Ok(Command::LoadHistory {
                conversation,
                before,
            }) => {
                const PAGE: usize = 60;
                let mut messages = history(&conversation, started);
                messages.retain(|m| before.is_none_or(|before| m.timestamp < before));
                let page = messages.split_off(messages.len().saturating_sub(PAGE));
                // More before this page?
                let older = (!messages.is_empty())
                    .then(|| page.first().map(|m| m.timestamp))
                    .flatten();
                let _ = events.send(Event::History {
                    conversation,
                    messages: page,
                    reactions: Vec::new(),
                    older,
                });
            }
            // Nothing to open: mock attachments have no content.
            Ok(Command::OpenAttachment(_)) => {}
            Ok(Command::LoadDetails(conversation)) => {
                let details = match conversation.0.as_str() {
                    "rust" => crate::Details {
                        description: Some("All things Rust 🦀, mostly GUIs".into()),
                        members: ["Carol", "Dana", "Eve", "Ferris"]
                            .iter()
                            .map(|name| {
                                let person = Person {
                                    service_id: name.to_lowercase(),
                                    name: (*name).into(),
                                };
                                (person, *name == "Ferris")
                            })
                            .collect(),
                        ..Default::default()
                    },
                    _ => crate::Details {
                        about: Some("🏔 Out climbing".into()),
                        phone: Some("+49 151 23456789".into()),
                        safety_number: Some(
                            "05236 77412 59001 83312 41875 22604 \
                             93141 00552 36988 14205 77310 54128"
                                .into(),
                        ),
                        ..Default::default()
                    },
                };
                let _ = events.send(Event::Details {
                    conversation,
                    details,
                });
            }
            Ok(Command::LoadMedia(conversation)) => {
                let mut messages: Vec<_> = history(&conversation, started)
                    .into_iter()
                    .filter(|m| {
                        m.attachments
                            .iter()
                            .any(|a| a.kind != AttachmentKind::LongText)
                            || m.body.contains("https://")
                    })
                    .collect();
                messages.reverse();
                let _ = events.send(Event::Media {
                    conversation,
                    messages,
                });
            }
            Ok(Command::SearchChat {
                conversation,
                query,
            }) => {
                let wanted = query.to_lowercase();
                let mut messages: Vec<_> = history(&conversation, started)
                    .into_iter()
                    .filter(|m| !wanted.is_empty() && m.body.to_lowercase().contains(&wanted))
                    .collect();
                messages.reverse();
                let _ = events.send(Event::SearchResults {
                    conversation,
                    query,
                    messages,
                });
            }
            Ok(Command::Forward {
                conversation,
                body,
                styles,
                attachments,
            }) => {
                let _ = events.send(Event::Message(ChatMessage {
                    attachments,
                    styles,
                    ..message(&conversation.0, "You", &body, now_millis(), true)
                }));
            }
            Ok(Command::Send {
                conversation,
                body,
                quote,
                files,
                styles,
                mentions: _,
            }) => {
                let attachments = files
                    .iter()
                    .map(|path| {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                        attachment(
                            name,
                            AttachmentKind::File,
                            "application/octet-stream",
                            Some(name),
                            (0, 0),
                        )
                    })
                    .collect();
                let echo = format!("You said: {body}");
                let sent = now_millis();
                let _ = events.send(Event::Message(ChatMessage {
                    quote,
                    attachments,
                    styles,
                    ..message(&conversation.0, "You", &body, sent, true)
                }));
                let typing = |typing| Event::Typing {
                    conversation: conversation.clone(),
                    sender_id: "echo".into(),
                    sender: "Echo".into(),
                    typing,
                };
                for (delivery, wait) in [(Delivery::Delivered, 300), (Delivery::Read, 300)] {
                    std::thread::sleep(Duration::from_millis(wait));
                    let _ = events.send(Event::Delivered {
                        conversation: conversation.clone(),
                        timestamps: vec![sent],
                        delivery,
                    });
                }
                let _ = events.send(typing(true));
                std::thread::sleep(Duration::from_millis(1500));
                let _ = events.send(Event::Message(message(
                    &conversation.0,
                    "Echo",
                    &echo,
                    now_millis(),
                    false,
                )));
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let _ = events.send(Event::Message(message(
                    "bob",
                    "Bob",
                    "ping",
                    now_millis(),
                    false,
                )));
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
    Picture {
        width: side,
        height: side,
        rgba,
    }
}

fn message(
    conversation: &str,
    sender: &str,
    body: &str,
    timestamp: u64,
    outgoing: bool,
) -> ChatMessage {
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
            message(
                "alice",
                "Alice",
                "Hey! Did you try the new client?",
                ago(42),
                false,
            ),
            message(
                "alice",
                "You",
                "Building it right now, in Rust.",
                ago(40),
                true,
            ),
            ChatMessage {
                quote: Some(Quote {
                    author: "You".into(),
                    text: "Building it right now, in Rust.".into(),
                    ..Default::default()
                }),
                ..message("alice", "Alice", "Of course you are 😄", ago(39), false)
            },
            ChatMessage {
                attachments: vec![attachment(
                    "photo",
                    AttachmentKind::Image,
                    "image/jpeg",
                    None,
                    (800, 600),
                )],
                reactions: vec![Reaction {
                    emoji: "😮".into(),
                    sender_id: "you".into(),
                    sender: "You".into(),
                    outgoing: true,
                }],
                ..message("alice", "Alice", "The view from the hut", ago(30), false)
            },
            ChatMessage {
                attachments: vec![attachment(
                    "notes",
                    AttachmentKind::File,
                    "application/pdf",
                    Some("route-notes.pdf"),
                    (0, 0),
                )],
                delivery: Delivery::Read,
                ..message("alice", "You", "", ago(29), true)
            },
            ChatMessage {
                styles: vec![
                    StyleRange {
                        start: 8,
                        end: 12,
                        style: Style::Bold,
                    },
                    StyleRange {
                        start: 13,
                        end: 19,
                        style: Style::Italic,
                    },
                ],
                edited_at: Some(ago(5)),
                edits: vec![(ago(5), "Look at the stars tonight".into())],
                delivery: Delivery::Delivered,
                ..message("alice", "You", "See the moon rising tonight", ago(6), true)
            },
            ChatMessage {
                deleted: true,
                ..message("alice", "Alice", "", ago(3), false)
            },
            ChatMessage {
                expire_timer: Some(3600),
                expires_at: Some(started + 50 * 60_000),
                ..message(
                    "alice",
                    "Alice",
                    "This one disappears in an hour",
                    ago(2),
                    false,
                )
            },
        ],
        // A long history, to page through.
        "bob" => (0..250u64)
            .map(|n| {
                let (sender, outgoing) = if n % 3 == 0 {
                    ("You", true)
                } else {
                    ("Bob", false)
                };
                message(
                    "bob",
                    sender,
                    &format!("Message {n}"),
                    ago((250 - n) * 30),
                    outgoing,
                )
            })
            .collect(),
        "dana" => vec![message(
            "dana",
            "Dana",
            "Old news",
            ago(60 * 24 * 40),
            false,
        )],
        "rust" => vec![
            message("rust", "Ferris", "Slint or iced?", ago(120), false),
            ChatMessage {
                reactions: ["ferris", "dana", "eve"]
                    .iter()
                    .map(|who| Reaction {
                        emoji: "👍".into(),
                        sender_id: (*who).into(),
                        sender: (*who).into(),
                        outgoing: false,
                    })
                    .chain([Reaction {
                        emoji: "❤️".into(),
                        sender_id: "you".into(),
                        sender: "You".into(),
                        outgoing: true,
                    }])
                    .collect(),
                ..message(
                    "rust",
                    "Carol",
                    "Slint, if you want it to look native.",
                    ago(118),
                    false,
                )
            },
            ChatMessage {
                attachments: vec![attachment(
                    "long",
                    AttachmentKind::LongText,
                    "text/x-signal-plain",
                    None,
                    (0, 0),
                )],
                ..message(
                    "rust",
                    "Ferris",
                    "A long message A long message A long…",
                    ago(117),
                    false,
                )
            },
            ChatMessage {
                attachments: vec![attachment(
                    "sticker",
                    AttachmentKind::Sticker,
                    "image/webp",
                    None,
                    (512, 512),
                )],
                ..message("rust", "Carol", "", ago(116), false)
            },
        ],
        _ => Vec::new(),
    }
}

fn attachment(
    id: &str,
    kind: AttachmentKind,
    content_type: &str,
    file_name: Option<&str>,
    (width, height): (u32, u32),
) -> Attachment {
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
