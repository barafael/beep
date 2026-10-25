use super::*;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, failure_persistence: None, ..ProptestConfig::default() })]

    /// With the chat open in a focused window, every incoming message that arrives is read:
    /// it goes into a MarkRead (a read receipt), and a disappearing one's timer is started.
    #[test]
    fn every_message_seen_in_an_open_chat_is_marked_read(arrivals in prop::collection::vec((0u64..20, any::<bool>()), 1..8)) {
        let base = now_millis() - 3_600_000;
        let id = ConversationId("a".into());
        let mut chat = ChatState::default();
        chat.apply(Event::Linked { account: "me".into() });
        chat.apply(Event::Synced);
        chat.apply(Event::Conversations(vec![Conversation {
            id: id.clone(),
            title: "Alice".into(),
            settings: ChatSettings { accepted: true, ..Default::default() },
            ..Default::default()
        }]));
        chat.select(&id);
        chat.apply(Event::History { conversation: id.clone(), messages: Vec::new(), reactions: Vec::new(), older: None });
        chat.set_window_focused(true);
        let mut read = Vec::new();
        let mut expiring = Vec::new();
        let mut sent = Vec::new();
        for (t, disappearing) in arrivals {
            let message = ChatMessage {
                conversation: id.clone(),
                sender_id: "alice".into(),
                sender: "Alice".into(),
                body: format!("at {t}"),
                timestamp: base + t * 1000,
                expire_timer: disappearing.then_some(3600),
                ..Default::default()
            };
            if !sent.contains(&message.timestamp) {
                sent.push(message.timestamp);
            }
            chat.apply(Event::Message(message));
            for command in chat.take_commands() {
                if let Command::MarkRead { messages, expiring: e, .. } = command {
                    read.extend(messages.into_iter().map(|m| m.timestamp));
                    expiring.extend(e.into_iter().map(|(m, _)| m.timestamp));
                }
            }
        }
        let unread: Vec<u64> = sent.iter().copied().filter(|t| !read.contains(t)).map(|t| (t - base) / 1000).collect();
        prop_assert!(unread.is_empty(), "never marked read (no receipt, timer never started): {unread:?} of {:?}", sent.iter().map(|t| (t - base) / 1000).collect::<Vec<_>>());
    }

    /// The same for messages arriving in batches, as when catching up: each is marked read
    /// once, and a batch makes at most one receipt.
    #[test]
    fn every_message_seen_in_a_batch_is_marked_read_once(batches in prop::collection::vec(prop::collection::vec((0u64..20, any::<bool>()), 1..6), 1..5)) {
        let base = now_millis() - 3_600_000;
        let id = ConversationId("a".into());
        let mut chat = ChatState::default();
        chat.apply(Event::Linked { account: "me".into() });
        chat.apply(Event::Synced);
        chat.apply(Event::Conversations(vec![Conversation {
            id: id.clone(),
            title: "Alice".into(),
            settings: ChatSettings { accepted: true, ..Default::default() },
            ..Default::default()
        }]));
        chat.select(&id);
        chat.apply(Event::History { conversation: id.clone(), messages: Vec::new(), reactions: Vec::new(), older: None });
        chat.set_window_focused(true);
        let mut read = Vec::new();
        let mut sent = Vec::new();
        for batch in batches {
            let events: Vec<Event> = batch.into_iter().map(|(t, outgoing)| {
                let timestamp = base + t * 1000;
                if !outgoing && !sent.contains(&timestamp) {
                    sent.push(timestamp);
                }
                Event::Message(ChatMessage {
                    conversation: id.clone(),
                    sender_id: if outgoing { "me" } else { "alice" }.into(),
                    sender: "Alice".into(),
                    timestamp,
                    outgoing,
                    ..Default::default()
                })
            }).collect();
            chat.apply_all(events);
            let receipts: Vec<Vec<u64>> = chat.take_commands().into_iter().filter_map(|command| match command {
                Command::MarkRead { messages, .. } => Some(messages.into_iter().map(|m| m.timestamp).collect()),
                _ => None,
            }).collect();
            prop_assert!(receipts.len() <= 1, "{receipts:?}");
            read.extend(receipts.into_iter().flatten());
        }
        read.sort_unstable();
        sent.sort_unstable();
        prop_assert_eq!(read, sent);
    }
}
