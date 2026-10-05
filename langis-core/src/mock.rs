use std::{sync::Arc, time::Duration};

use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, ChatMessage, Command,
    Conversation, ConversationId, Event, Events, HistoryTransfer, Inbox, Picture, QrMatrix, Quote,
    Reaction, now_millis,
};

const CONVERSATIONS: &[(&str, &str, bool)] = &[
    ("alice", "Alice", false),
    ("rust", "Rustaceans 🦀", true),
    ("bob", "Bob", false),
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
            } else {
                "+49 151 23456789".into()
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
            Ok(Command::Unlock(_) | Command::SkipHistory) => {}
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
            Ok(Command::LoadHistory(id)) => {
                let messages = history(&id, started);
                let _ = events.send(Event::History {
                    conversation: id,
                    messages,
                });
            }
            // Nothing to open: mock attachments have no content.
            Ok(Command::OpenAttachment(_)) => {}
            Ok(Command::Send {
                conversation,
                body,
                quote,
                files,
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
                let _ = events.send(Event::Message(ChatMessage {
                    quote,
                    attachments,
                    ..message(&conversation.0, "You", &body, now_millis(), true)
                }));
                std::thread::sleep(Duration::from_millis(600));
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
                ..message("alice", "You", "", ago(29), true)
            },
        ],
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
        "bob" => years_of_bob(started),
        _ => Vec::new(),
    }
}

/// About four years with Bob, busy at times and silent for one: a long history for the
/// activity strip. Made up the same way every time.
fn years_of_bob(started: u64) -> Vec<ChatMessage> {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    // (from days ago, until days ago, messages a day)
    const SEASONS: &[(u64, u64, f32)] = &[
        (1460, 1300, 6.0),
        (1300, 1100, 0.5),
        (1100, 1000, 15.0),
        (1000, 620, 0.0),
        (620, 400, 3.0),
        (400, 380, 25.0),
        (380, 60, 1.0),
        (60, 1, 8.0),
    ];
    const LINES: &[&str] = &[
        "Morning!",
        "Did you see that?",
        "Haha, yes",
        "On my way",
        "Let's do Friday",
        "Can't tonight, sorry",
        "Look at this",
        "😂",
        "Sounds good",
        "How was it?",
    ];
    // A small LCG: enough to look irregular, and the same on every run.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut random = move |below: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % below.max(1)
    };
    let today = started - started % DAY;
    let mut messages = Vec::new();
    for &(from, until, rate) in SEASONS {
        for days_ago in (until..from).rev() {
            let n = if rate >= 1.0 {
                random(2 * rate as u64 + 1)
            } else {
                u64::from(random(100) < (rate * 100.0) as u64)
            };
            for _ in 0..n {
                let timestamp = today - days_ago * DAY + (8 * 60 + random(15 * 60)) * 60_000;
                let outgoing = random(2) == 0;
                let (sender, line) = if outgoing {
                    ("You", true)
                } else {
                    ("Bob", false)
                };
                let mut m = message(
                    "bob",
                    sender,
                    LINES[random(LINES.len() as u64) as usize],
                    timestamp,
                    line,
                );
                let id = format!("bob-{}", messages.len());
                match random(60) {
                    0..4 => m.attachments.push(attachment(
                        &id,
                        AttachmentKind::Image,
                        "image/jpeg",
                        None,
                        (800, 600),
                    )),
                    4 => m.attachments.push(attachment(
                        &id,
                        AttachmentKind::File,
                        "application/pdf",
                        Some("tickets.pdf"),
                        (0, 0),
                    )),
                    _ => {}
                }
                messages.push(m);
            }
        }
    }
    messages.sort_by_key(|m| m.timestamp);
    messages
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
