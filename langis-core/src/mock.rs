use std::{sync::Arc, time::Duration};

use crate::{Avatar, ChatMessage, HistoryTransfer, Command, Conversation, ConversationId, Event, Events, Inbox, QrMatrix, now_millis};

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
    // `LANGIS_MOCK_TRANSFER=1` walks through a message history transfer.
    if std::env::var_os("LANGIS_MOCK_TRANSFER").is_some() {
        for stage in [
            HistoryTransfer::Waiting,
            HistoryTransfer::Downloading,
            HistoryTransfer::Importing { messages: 500 },
            HistoryTransfer::Importing { messages: 1000 },
            HistoryTransfer::Finished,
        ] {
            let _ = events.send(Event::HistoryTransfer(stage));
            std::thread::sleep(Duration::from_secs(2));
        }
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

    let message = |conversation: &str, sender: &str, body: &str, timestamp, outgoing| ChatMessage {
        conversation: ConversationId(conversation.into()),
        sender_id: sender.to_lowercase(),
        sender: sender.into(),
        body: body.into(),
        timestamp,
        outgoing,
    };
    // Fixed at startup so repeated history loads return identical messages.
    let started = now_millis();
    let ago = |minutes: u64| started - minutes * 60_000;

    loop {
        match commands.recv_timeout(Duration::from_secs(20)) {
            Ok(Command::Unlock(_)) => {}
            Ok(Command::LoadHistory(id)) => {
                let messages = match id.0.as_str() {
                    "alice" => vec![
                        message("alice", "Alice", "Hey! Did you try the new client?", ago(42), false),
                        message("alice", "You", "Building it right now, in Rust.", ago(40), true),
                        message("alice", "Alice", "Of course you are 😄", ago(39), false),
                    ],
                    "rust" => vec![
                        message("rust", "Ferris", "Slint or iced?", ago(120), false),
                        message("rust", "Carol", "Slint, if you want it to look native.", ago(118), false),
                    ],
                    _ => Vec::new(),
                };
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
fn gradient(side: u32) -> Avatar {
    let rgba = (0..side * side)
        .flat_map(|i| {
            let (x, y) = (i % side, i / side);
            let t = ((x + y) * 255 / (2 * side - 2)) as u8;
            [255 - t / 2, 120 + t / 3, t, 255]
        })
        .collect();
    Avatar { side, rgba }
}
