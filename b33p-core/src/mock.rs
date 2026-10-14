use std::{sync::Arc, time::Duration};

use crate::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, ChatMessage, ChatSettings,
    Command, Conversation, ConversationId, ConversationKind, Delivery, Event, Events,
    HistoryTransfer, Inbox, Person, Picture, QrMatrix, Quote, Reaction, Style, StyleRange,
    now_millis,
};

const CONVERSATIONS: &[(&str, &str, bool)] = &[
    ("alice", "Alice", false),
    ("rust", "Rustaceans 🦀", true),
    ("bob", "Bob", false),
    ("dana", "Dana", false),
    // A group we're invited to, not in yet.
    ("club", "Book club", true),
    ("me", "Note to Self", false),
    // Not accepted: a message request.
    ("stranger", "+44 7700 900123", false),
];

/// A mock conversation's id, made as the real backend makes them ("c:" and the person's id,
/// "g:" and the group's key), so the chat state treats it alike.
fn chat_id(name: &str) -> ConversationId {
    let group = name.starts_with("group-")
        || CONVERSATIONS
            .iter()
            .any(|&(id, _, is_group)| id == name && is_group);
    ConversationId(format!("{}:{name}", if group { "g" } else { "c" }))
}

/// What a mock conversation's id names: the person or group.
fn chat_name(id: &ConversationId) -> &str {
    id.0.split_once(':').map_or(id.0.as_str(), |(_, name)| name)
}

/// A mock conversation as it starts out.
fn conversation(id: &str, title: &str, is_group: bool) -> Conversation {
    Conversation {
        id: chat_id(id),
        title: title.into(),
        kind: if is_group {
            ConversationKind::Group
        } else {
            ConversationKind::Contact
        },
        subtitle: if id == "club" {
            "You're invited to this group.".into()
        } else if is_group {
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
            accepted: id != "stranger",
            ..ChatSettings::default()
        },
        expire_timer: if id == "alice" { 3600 } else { 0 },
        note_to_self: id == "me",
        admin: is_group && id != "club",
        left: id == "club",
        invited: id == "club",
        // Verified once; her key changed since, so writing to her waits for a confirmation.
        trust: if id == "dana" {
            crate::Trust::Changed
        } else {
            crate::Trust::Unverified
        },
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
    }
}

pub fn run(commands: Inbox, events: Events) {
    // `B33P_MOCK_PASSWORD=1` simulates a desktop without a keyring; the password is "password".
    if cfg!(debug_assertions) && std::env::var_os("B33P_MOCK_PASSWORD").is_some() {
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
    // `B33P_MOCK_TRANSFER=1` walks through a message history transfer; the wait for the
    // phone takes 6 seconds and can be skipped.
    // Commands that arrive meanwhile are handled afterwards, as in the real backend.
    let mut queued = std::collections::VecDeque::new();
    if std::env::var_os("B33P_MOCK_TRANSFER").is_some() {
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
        .map(|&(id, title, is_group)| conversation(id, title, is_group))
        .collect();
    let _ = events.send(Event::Conversations(conversations));
    let _ = events.send(Event::Avatar {
        conversation: chat_id("alice"),
        avatar: Some(Arc::new(gradient(96))),
    });
    let _ = events.send(Event::Synced);
    let _ = events.send(Event::StorageRead);
    // Two stories from Alice (a picture, then text) and one in the group.
    let mut stories = vec![
        crate::Story {
            author_id: "alice".into(),
            author: "Alice".into(),
            conversation: chat_id("alice"),
            timestamp: now_millis() - 3 * 3_600_000,
            content: crate::StoryContent::Picture(attachment(
                "story-hut",
                AttachmentKind::Image,
                "image/jpeg",
                None,
                (1080, 1920),
            )),
            viewed: false,
            outgoing: false,
        },
        crate::Story {
            author_id: "alice".into(),
            author: "Alice".into(),
            conversation: chat_id("alice"),
            timestamp: now_millis() - 3_600_000,
            content: crate::StoryContent::Text {
                text: "Summit tomorrow! ⛰".into(),
                foreground: 0xffff_ffff,
                background: 0xffd3_6c1c,
            },
            viewed: false,
            outgoing: false,
        },
        crate::Story {
            author_id: "ferris".into(),
            author: "Ferris".into(),
            conversation: chat_id("rust"),
            timestamp: now_millis() - 7_200_000,
            content: crate::StoryContent::Text {
                text: "cargo build --release 🦀".into(),
                foreground: 0xffff_ffff,
                background: 0xff2b_8a60,
            },
            viewed: true,
            outgoing: false,
        },
    ];
    let _ = events.send(Event::Stories(stories.clone()));
    let _ = events.send(Event::Folders(vec![
        crate::ChatFolder {
            name: "Friends".into(),
            included: vec![chat_id("alice"), chat_id("bob")],
            ..Default::default()
        },
        crate::ChatFolder {
            name: "Groups".into(),
            all_groups: true,
            show_muted: true,
            ..Default::default()
        },
    ]));

    // Fixed at startup so repeated history loads return identical messages.
    let started = now_millis();
    // Like the real backend: each conversation's newest message, for sorting and previews.
    for &(id, _, _) in CONVERSATIONS {
        if let Some(latest) = history(&chat_id(id), started).pop() {
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
                    ..message(
                        chat_name(&conversation),
                        "You",
                        &body,
                        target.timestamp,
                        true,
                    )
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
                    AttachmentKind::Audio | AttachmentKind::VoiceNote => {
                        Ok(AttachmentContent::Audio(tone_wav().into()))
                    }
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
            Ok(Command::Block { .. }) => {}
            Ok(Command::ViewStory(story)) => {
                for s in &mut stories {
                    if s.author_id == story.author_id && s.timestamp == story.timestamp {
                        s.viewed = true;
                    }
                }
                let _ = events.send(Event::Stories(stories.clone()));
            }
            Ok(Command::SendStory {
                conversation,
                story,
            }) => {
                let content = match story {
                    crate::NewStory::Text(text) => crate::StoryContent::Text {
                        text,
                        foreground: 0xffff_ffff,
                        background: 0xff3a_76f0,
                    },
                    crate::NewStory::Picture(_) => crate::StoryContent::Picture(attachment(
                        &format!("story-{}", now_millis()),
                        AttachmentKind::Image,
                        "image/jpeg",
                        None,
                        (1080, 1920),
                    )),
                };
                stories.push(crate::Story {
                    author_id: "you".into(),
                    author: "You".into(),
                    conversation,
                    timestamp: now_millis(),
                    content,
                    viewed: true,
                    outgoing: true,
                });
                let _ = events.send(Event::Stories(stories.clone()));
            }
            Ok(Command::CreateGroup { title, members }) => {
                let id = chat_id(&format!("group-{}", now_millis()));
                let conversation = Conversation {
                    id: id.clone(),
                    title,
                    kind: ConversationKind::Group,
                    subtitle: format!("{} members", members.len() + 1),
                    members: members
                        .iter()
                        .map(|m| Person {
                            service_id: m.clone(),
                            name: m.clone(),
                        })
                        .collect(),
                    admin: true,
                    settings: ChatSettings {
                        accepted: true,
                        ..ChatSettings::default()
                    },
                    ..Conversation::default()
                };
                let _ = events.send(Event::Conversations(vec![conversation]));
                let _ = events.send(Event::GroupCreated(id.clone()));
                let _ = events.send(Event::Message(ChatMessage {
                    notice: true,
                    ..message(
                        chat_name(&id),
                        "You",
                        "You created the group.",
                        now_millis(),
                        true,
                    )
                }));
            }
            Ok(Command::EditGroup { conversation, edit }) => {
                let notice = match edit {
                    crate::GroupEdit::Title(title) => format!("You named the group “{title}”."),
                    crate::GroupEdit::Description(_) => {
                        "You changed the group's description.".into()
                    }
                    crate::GroupEdit::SetPicture(_) => "You changed the group's picture.".into(),
                    crate::GroupEdit::RemovePicture => "You removed the group's picture.".into(),
                    crate::GroupEdit::AddMembers(members) => {
                        format!("You added {}.", members.join(", "))
                    }
                    crate::GroupEdit::RemoveMember(member) => format!("You removed {member}."),
                    crate::GroupEdit::SetAdmin(member, true) => {
                        format!("You made {member} an admin.")
                    }
                    crate::GroupEdit::SetAdmin(member, false) => {
                        format!("You took {member}'s admin rights.")
                    }
                    crate::GroupEdit::Leave => "You left the group.".into(),
                    crate::GroupEdit::AcceptInvitation | crate::GroupEdit::DeclineInvitation => {
                        let accept = edit == crate::GroupEdit::AcceptInvitation;
                        let (_, title, _) = CONVERSATIONS
                            .iter()
                            .find(|(id, _, _)| *id == chat_name(&conversation))
                            .copied()
                            .unwrap_or(("", "", true));
                        let mut now = self::conversation(chat_name(&conversation), title, true);
                        now.invited = false;
                        now.left = !accept;
                        now.subtitle = if accept {
                            "4 members".into()
                        } else {
                            "You aren't a member of this group.".into()
                        };
                        let _ = events.send(Event::Conversations(vec![now]));
                        if accept {
                            "You joined the group.".into()
                        } else {
                            "You declined the invitation.".into()
                        }
                    }
                };
                let _ = events.send(Event::Message(ChatMessage {
                    notice: true,
                    ..message(chat_name(&conversation), "You", &notice, now_millis(), true)
                }));
            }
            Ok(Command::LoadStickers) => {
                // One pack of gradients standing in for stickers.
                let stickers = ["😀", "🦀", "👍", "🎉", "❤️", "🙈"]
                    .iter()
                    .enumerate()
                    .map(|(id, emoji)| crate::StickerItem {
                        id: id as u32,
                        emoji: (*emoji).into(),
                        picture: Arc::new(gradient(64)),
                    })
                    .collect();
                let _ = events.send(Event::StickerPacks(vec![crate::StickerPack {
                    id: "mock".into(),
                    title: "Gradients".into(),
                    stickers,
                }]));
            }
            Ok(Command::SendSticker { conversation, .. }) => {
                let _ = events.send(Event::Message(ChatMessage {
                    attachments: vec![attachment(
                        &format!("sticker-{}", now_millis()),
                        AttachmentKind::Sticker,
                        "image/webp",
                        None,
                        (512, 512),
                    )],
                    ..message(chat_name(&conversation), "You", "", now_millis(), true)
                }));
            }
            Ok(Command::Respond {
                conversation,
                response,
            }) => {
                if response == crate::RequestResponse::Delete {
                    let _ = events.send(Event::Cleared(conversation));
                }
            }
            Ok(Command::SetVerified {
                conversation,
                verified,
            }) => {
                let _ = events.send(Event::Trust {
                    conversation: conversation.clone(),
                    trust: if verified {
                        crate::Trust::Verified
                    } else {
                        crate::Trust::Unverified
                    },
                });
                let name = CONVERSATIONS
                    .iter()
                    .find(|(id, _, _)| *id == chat_name(&conversation))
                    .map_or("them", |(_, name, _)| *name);
                let state = if verified { "verified" } else { "unverified" };
                let _ = events.send(Event::Message(ChatMessage {
                    notice: true,
                    ..message(
                        chat_name(&conversation),
                        name,
                        &format!("You marked your safety number with {name} as {state}."),
                        now_millis(),
                        false,
                    )
                }));
            }
            // The stranger's number is on "Signal"; no other.
            Ok(Command::FindNumber { number, then }) => {
                let digits: String = number.chars().filter(char::is_ascii_digit).collect();
                if digits.ends_with("7700900123") {
                    let _ = events.send(Event::Found {
                        conversation: chat_id("stranger"),
                        then,
                    });
                } else {
                    let _ = events.send(Event::Error(format!(
                        "Nobody on Signal has the number {number}."
                    )));
                }
            }
            Ok(Command::LoadDetails(conversation)) => {
                let details = match chat_name(&conversation) {
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
                    ..message(chat_name(&conversation), "You", &body, now_millis(), true)
                }));
            }
            Ok(Command::Send {
                conversation,
                timestamp,
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
                // Sending takes a moment, and `/fail` fails, to see both in the UI.
                std::thread::sleep(Duration::from_millis(1200));
                if body == "/fail" {
                    let _ = events.send(Event::SendFailed {
                        conversation,
                        timestamp,
                    });
                    let _ = events.send(Event::Error("failed to send: mock failure".into()));
                    continue;
                }
                let echo = format!("You said: {body}");
                let sent = timestamp;
                let _ = events.send(Event::Message(ChatMessage {
                    quote,
                    attachments,
                    styles,
                    ..message(chat_name(&conversation), "You", &body, sent, true)
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
                    chat_name(&conversation),
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

/// Two seconds of a quiet 440 Hz tone as a WAV file, standing in for a voice message.
pub fn tone_wav() -> Vec<u8> {
    const RATE: u32 = 8000;
    let samples: Vec<i16> = (0..RATE * 2)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            ((t * 440. * std::f32::consts::TAU).sin() * 3000.) as i16
        })
        .collect();
    let data_len = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
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
        conversation: chat_id(conversation),
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
    match chat_name(id) {
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
            // She set up a new phone; what follows came with her new safety number.
            ChatMessage {
                notice: true,
                ..message(
                    "alice",
                    "Alice",
                    "Your safety number with Alice has changed.",
                    ago(31),
                    false,
                )
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
                preview: Some(crate::LinkPreview {
                    url: "https://www.rust-lang.org/".into(),
                    title: "Rust Programming Language".into(),
                    description:
                        "A language empowering everyone to build reliable and efficient software."
                            .into(),
                    image: Some(attachment(
                        "preview",
                        AttachmentKind::Image,
                        "image/png",
                        None,
                        (1200, 630),
                    )),
                }),
                ..message(
                    "alice",
                    "Alice",
                    "Have you seen https://www.rust-lang.org/ lately?",
                    ago(4),
                    false,
                )
            },
            ChatMessage {
                attachments: vec![attachment(
                    "voice",
                    AttachmentKind::VoiceNote,
                    "audio/aac",
                    None,
                    (0, 0),
                )],
                ..message("alice", "Alice", "", ago(1), false)
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
        // A long history, to page through, after years of it for the activity strip.
        "bob" => {
            let mut messages = years_of_bob(started);
            messages.extend((0..250u64).map(|n| {
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
            }));
            messages.sort_by_key(|m| m.timestamp);
            messages
        }
        "stranger" => vec![message(
            "stranger",
            "+44 7700 900123",
            "Hi! Is this still the number for the flat viewing?",
            ago(15),
            false,
        )],
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
                attachments: vec![Attachment {
                    caption: Some("Ferris at the meetup".into()),
                    ..attachment(
                        "video",
                        AttachmentKind::Video,
                        "video/mp4",
                        None,
                        (640, 360),
                    )
                }],
                ..message("rust", "Dana", "", ago(117), false)
            },
            ChatMessage {
                attachments: vec![Attachment {
                    gif: true,
                    ..attachment("gif", AttachmentKind::Video, "video/mp4", None, (300, 300))
                }],
                ..message("rust", "Eve", "", ago(117), false)
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
        // A BlurHash of a sunset, for every picture and video.
        blurhash: matches!(kind, AttachmentKind::Image | AttachmentKind::Video)
            .then(|| "LKO2?U%2Tw=w]~RBVZRi};RPxuwH".into()),
        caption: None,
        gif: false,
        pointer: Arc::from(Vec::new()),
    }
}
