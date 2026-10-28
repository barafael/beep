use super::*;
use crate::{ConversationKind, Delivery};

fn conversation(id: &str, is_group: bool) -> Conversation {
    Conversation {
        id: ConversationId(id.into()),
        title: id.into(),
        kind: if is_group {
            ConversationKind::Group
        } else {
            ConversationKind::Contact
        },
        ..Conversation::default()
    }
}

fn message(conversation: &str, sender: &str, timestamp: u64, outgoing: bool) -> ChatMessage {
    ChatMessage {
        conversation: ConversationId(conversation.into()),
        sender_id: sender.to_lowercase(),
        sender: sender.into(),
        body: format!("{sender} at {timestamp}\nsecond line"),
        timestamp,
        outgoing,
        ..Default::default()
    }
}

fn state() -> ChatState {
    let mut state = ChatState::default();
    state.apply(Event::Linked {
        account: "me".into(),
    });
    state.apply(Event::Synced);
    state.apply(Event::Conversations(vec![
        conversation("a", false),
        conversation("g", true),
    ]));
    state
}

fn notifying() -> ChatState {
    let mut state = state();
    state.set_notifications(true, NotificationContent::NameAndMessage);
    state.take_notices();
    state
}

fn shown(notices: &[Notice]) -> Vec<(Option<&str>, &str, &str)> {
    notices
        .iter()
        .filter_map(|n| match n {
            Notice::Show {
                conversation,
                title,
                body,
            } => Some((
                conversation.as_ref().map(|c| c.0.as_str()),
                title.as_str(),
                body.as_str(),
            )),
            Notice::Withdraw(_) => None,
        })
        .collect()
}

#[test]
fn incoming_messages_notify_unless_their_chat_is_in_front() {
    let mut state = notifying();
    state.apply(Event::Message(message("a", "Alice", 1, false)));
    assert_eq!(
        shown(&state.take_notices()),
        [(Some("a"), "Alice", "Alice at 1\nsecond line")]
    );

    state.select(&ConversationId("a".into()));
    state.set_window_focused(true);
    state.take_notices();
    state.apply(Event::Message(message("a", "Alice", 2, false)));
    assert!(
        shown(&state.take_notices()).is_empty(),
        "open chat in a focused window"
    );
    state.apply(Event::Message(message("g", "Carol", 3, false)));
    assert_eq!(
        shown(&state.take_notices()),
        [(Some("g"), "Carol in g", "Carol at 3\nsecond line")]
    );

    state.set_window_focused(false);
    state.apply(Event::Message(message("a", "Alice", 4, false)));
    assert_eq!(
        shown(&state.take_notices()).len(),
        1,
        "open chat, but window in the background"
    );
}

#[test]
fn outgoing_duplicate_and_disabled_dont_notify() {
    let mut state = notifying();
    state.apply(Event::Message(message("a", "You", 1, true)));
    state.apply(Event::Message(message("a", "Alice", 2, false)));
    state.take_notices();
    state.apply(Event::Message(message("a", "Alice", 2, false)));
    state.apply(Event::Latest(message("a", "Alice", 3, false)));
    assert!(state.take_notices().is_empty());
    state.set_notifications(false, NotificationContent::NameAndMessage);
    state.apply(Event::Message(message("a", "Alice", 4, false)));
    assert!(state.take_notices().is_empty());
}

#[test]
fn catching_up_is_summarized_once_synced() {
    let mut state = notifying();
    state.apply(Event::Reconnecting);
    for (chat, ts) in [("a", 1), ("a", 2), ("g", 3)] {
        state.apply(Event::Message(message(chat, "Alice", ts, false)));
    }
    assert!(state.take_notices().is_empty());
    state.apply(Event::Synced);
    assert_eq!(
        shown(&state.take_notices()),
        [(None, "b33p", "3 new messages in 2 chats")]
    );
    state.apply(Event::Synced);
    assert!(state.take_notices().is_empty());
}

#[test]
fn content_setting_controls_what_is_revealed() {
    let mut state = notifying();
    state.set_notifications(true, NotificationContent::NameOnly);
    state.apply(Event::Message(message("a", "Alice", 1, false)));
    assert_eq!(
        shown(&state.take_notices()),
        [(Some("a"), "Alice", "New message")]
    );
    state.set_notifications(true, NotificationContent::Nothing);
    state.apply(Event::Message(message("a", "Alice", 2, false)));
    assert_eq!(
        shown(&state.take_notices()),
        [(Some("a"), "b33p", "New message")]
    );
}

#[test]
fn attachment_only_messages_say_what_they_are() {
    let mut state = notifying();
    state.apply(Event::Message(ChatMessage {
        body: String::new(),
        ..with_attachment("a", 9, "p", AttachmentKind::Image)
    }));
    assert_eq!(
        shown(&state.take_notices()),
        [(Some("a"), "Alice", "📷 Photo")]
    );
}

#[test]
fn reading_or_replying_withdraws_the_notification() {
    let mut state = notifying();
    let a = ConversationId("a".into());
    state.apply(Event::Message(message("a", "Alice", 1, false)));
    state.take_notices();
    let reply = state.reply(&a, " thanks ").0;
    assert!(
        matches!(reply, Some(Command::Send { conversation, body, quote: None, .. }) if conversation == a && body == "thanks")
    );
    assert_eq!(state.take_notices(), [Notice::Withdraw(a.clone())]);
    assert_eq!(
        state
            .conversations()
            .find(|c| c.conversation.id == a)
            .unwrap()
            .unread,
        0
    );
    assert!(state.reply(&a, "  ").0.is_none());
    assert!(
        state
            .reply(&ConversationId("nope".into()), "hi")
            .0
            .is_none()
    );
    state.select(&a);
    assert_eq!(state.take_notices(), [Notice::Withdraw(a)]);
}

/// A reply from a notification goes by the composer's rules: it would otherwise accept a
/// message request (and share our profile key), write to a blocked chat, or be attempted
/// while catching up.
#[test]
fn replying_follows_the_composers_rules() {
    let mut state = notifying();
    let a = ConversationId("a".into());
    state.apply(Event::Message(message("a", "Alice", 1, false)));
    // An unanswered request, once the phone's settings are known.
    state.apply(Event::StorageRead);
    assert!(state.reply(&a, "hi").0.is_none());
    state.apply(Event::Settings {
        conversation: a.clone(),
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
    });
    assert!(state.reply(&a, "hi").0.is_some());
    state.apply(Event::Settings {
        conversation: a.clone(),
        settings: ChatSettings {
            accepted: true,
            blocked: true,
            ..ChatSettings::default()
        },
    });
    assert!(state.reply(&a, "hi").0.is_none());
    // Reconnecting: nothing goes out until caught up again.
    let mut state = notifying();
    state.apply(Event::Message(message("a", "Alice", 1, false)));
    state.apply(Event::Reconnecting);
    assert!(state.reply(&a, "hi").0.is_none());
    state.apply(Event::Synced);
    assert!(state.reply(&a, "hi").0.is_some());
    // A group we left.
    let mut state = notifying();
    let g = ConversationId("g".into());
    state.apply(Event::Conversations(vec![Conversation {
        left: true,
        ..conversation("g", true)
    }]));
    assert!(state.reply(&g, "hi").0.is_none());
    state.select(&g);
    assert!(state.react(0, "👍").is_none());
}

#[test]
fn incoming_message_moves_conversation_up_and_counts_unread() {
    let mut state = state();
    let update = state.apply(Event::Message(message("g", "Carol", 1, false)));
    assert_eq!(
        update,
        Update {
            conversations: true,
            ..Update::default()
        }
    );
    let views: Vec<_> = state.conversations().collect();
    assert_eq!(views[0].conversation.id.0, "g");
    assert_eq!(views[0].unread, 1);
    assert_eq!(views[0].preview(), "Carol at 1");
    assert_eq!(views[0].preview_kind(), None, "words only");
}

/// With a clock set by hand, what runs out runs out when it should, without waiting.
#[test]
fn typing_and_mutes_run_out_by_the_clock() {
    let start = 1_700_000_000_000;
    let clock = Clock::fixed(start);
    let mut state = state();
    state.set_clock(clock.clone());
    state.apply(Event::Typing {
        conversation: id("g"),
        sender_id: "bob".into(),
        sender: "Bob".into(),
        typing: true,
    });
    assert_eq!(state.typing(&id("g")), ["Bob"]);
    assert_eq!(state.typing_expires(), Some(start + TYPING_TIMEOUT_MS));
    clock.set(start + TYPING_TIMEOUT_MS + 1);
    assert!(state.typing(&id("g")).is_empty());
    assert_eq!(state.typing_expires(), None);

    let mut settings = state.conversation(&id("g")).unwrap().settings.clone();
    settings.muted_until = Some(start + 2 * TYPING_TIMEOUT_MS);
    state.apply(Event::Settings {
        conversation: id("g"),
        settings,
    });
    assert!(state.conversation_view(&id("g")).unwrap().muted());
    clock.set(start + 3 * TYPING_TIMEOUT_MS);
    assert!(!state.conversation_view(&id("g")).unwrap().muted());
}

/// A message without words is previewed by what it carries, in words with an icon (the
/// emoji are for notifications); so is a change of the timer.
#[test]
fn previews_say_what_a_message_without_words_is() {
    let mut state = state();
    state.apply(Event::Message(ChatMessage {
        body: String::new(),
        ..with_attachment("g", 1, "p", AttachmentKind::Image)
    }));
    let view = state.conversations().next().unwrap();
    assert_eq!(view.preview(), "Photo");
    assert_eq!(
        view.preview_kind(),
        Some(PreviewKind::Attachment(AttachmentKind::Image))
    );
    state.apply(Event::Message(ChatMessage {
        body: crate::timer_notice("Carol", 3600),
        notice: true,
        timer_change: Some(3600),
        ..message("g", "Carol", 2, false)
    }));
    let view = state.conversations().next().unwrap();
    assert_eq!(view.preview(), "Carol set disappearing messages to 1 hour.");
    assert_eq!(view.preview_kind(), Some(PreviewKind::Timer));
}

#[test]
fn selecting_requests_history_until_it_arrives() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Alice", 1, false)));
    let id = ConversationId("a".into());
    assert!(
        matches!(state.select(&id), Some(Command::LoadHistory { conversation: i, before: None }) if i == id)
    );
    assert_eq!(
        state.conversations().find(|c| c.selected).unwrap().unread,
        0
    );
    // Not loaded yet (in flight, or the load failed): asking again retries.
    assert!(state.select(&id).is_some());
    state.apply(Event::History {
        conversation: id.clone(),
        messages: Vec::new(),
        reactions: Vec::new(),
        older: None,
    });
    assert!(state.select(&id).is_none());
    assert!(state.select(&ConversationId("nope".into())).is_none());
}

#[test]
fn latest_message_previews_without_counting_as_loaded() {
    let mut state = state();
    let id = ConversationId("a".into());
    state.apply(Event::Latest(message("a", "Alice", 5, false)));
    let view = state
        .conversations()
        .find(|c| c.conversation.id == id)
        .unwrap();
    assert_eq!(view.last_message.unwrap().timestamp, 5);
    assert_eq!(view.unread, 0);
    // Opening it still fetches the full history.
    assert!(
        matches!(state.select(&id), Some(Command::LoadHistory { conversation: i, before: None }) if i == id)
    );
}

/// Signal's edits change the text; a backend reporting only the text doesn't lose the
/// photo (or quote) the message was sent with.
#[test]
fn an_edit_keeps_the_attachments_and_quote() {
    let mut state = state();
    state.select(&id("a"));
    state.apply(Event::History {
        conversation: id("a"),
        messages: vec![ChatMessage {
            outgoing: true,
            quote: Some(Quote {
                author: "Alice".into(),
                text: "hello".into(),
                author_id: "alice".into(),
                timestamp: 5,
            }),
            ..with_attachment("a", 10, "p", AttachmentKind::Image)
        }],
        reactions: Vec::new(),
        older: None,
    });
    let target = state.messages().next().unwrap().message.reference();
    // Its sender's edit: nobody else's is taken.
    let sender_id = target.author_id.clone();
    state.apply(Event::Edited {
        conversation: id("a"),
        target,
        message: ChatMessage {
            body: "better".into(),
            sender_id,
            ..message("a", "You", 10, true)
        },
    });
    let edited = state.messages().next().unwrap().message;
    assert_eq!(edited.body, "better");
    assert_eq!(edited.attachments.len(), 1);
    assert_eq!(
        edited.quote.as_ref().map(|q| q.text.as_str()),
        Some("hello")
    );
}

/// An edit from anyone but the sender is dropped: it would move the message in the
/// history, which is sorted by time and sender.
#[test]
fn only_the_sender_edits_a_message() {
    let mut state = state();
    state.select(&id("a"));
    state.apply(Event::History {
        conversation: id("a"),
        messages: vec![message("a", "Alice", 10, false)],
        reactions: Vec::new(),
        older: None,
    });
    let target = state.messages().next().unwrap().message.reference();
    state.apply(Event::Edited {
        conversation: id("a"),
        target,
        message: ChatMessage {
            body: "not yours".into(),
            sender_id: "mallory".into(),
            ..message("a", "Mallory", 10, false)
        },
    });
    let shown = state.messages().next().unwrap().message;
    assert_ne!(shown.body, "not yours");
    assert_eq!(shown.edited_at, None);
}

/// Downloads beyond the bound go, those that came first first, but never what shows: the
/// open chat's.
#[test]
fn downloads_are_kept_within_bounds() {
    let mut state = state();
    state.kept_downloads = Some(250);
    state.select(&id("a"));
    state.apply(Event::History {
        conversation: id("a"),
        messages: vec![with_attachment("a", 1, "shown", AttachmentKind::Image)],
        reactions: Vec::new(),
        older: None,
    });
    let picture = |n: usize| {
        Ok(AttachmentContent::Picture(Arc::new(Picture {
            width: 1,
            height: 1,
            rgba: vec![0; n],
        })))
    };
    let download = |state: &mut ChatState, name: &str| {
        state.apply(Event::Attachment {
            id: AttachmentId(name.into()),
            content: picture(100),
        });
    };
    download(&mut state, "shown");
    download(&mut state, "first");
    assert_eq!(state.downloads_forgotten(), 0);
    download(&mut state, "second");
    // 300 bytes: the first that came and doesn't show goes.
    assert_eq!(state.downloads_forgotten(), 1);
    let there = |state: &ChatState, name: &str| {
        matches!(
            state.attachment(&AttachmentId(name.into())),
            Some(AttachmentState::Ready(_))
        )
    };
    assert!(there(&state, "shown"));
    assert!(!there(&state, "first"));
    assert!(there(&state, "second"));
    download(&mut state, "third");
    assert!(there(&state, "shown"), "what shows stays");
    assert!(!there(&state, "second") && there(&state, "third"));
    assert_eq!(state.downloaded_bytes, 200);
}

fn with_attachment(
    conversation: &str,
    timestamp: u64,
    id: &str,
    kind: AttachmentKind,
) -> ChatMessage {
    let attachment = Attachment {
        id: AttachmentId(id.into()),
        kind,
        content_type: String::new(),
        file_name: None,
        size: 1,
        width: 0,
        height: 0,
        blurhash: None,
        caption: None,
        gif: false,
        pointer: Arc::from(Vec::new()),
    };
    ChatMessage {
        attachments: vec![attachment],
        ..message(conversation, "Alice", timestamp, false)
    }
}

#[test]
fn attachments_are_fetched_once_and_only_for_the_selected_chat() {
    let mut state = state();
    let photo = AttachmentId("photo".into());
    state.apply(Event::Message(with_attachment(
        "a",
        1,
        "photo",
        AttachmentKind::Image,
    )));
    assert!(state.fetch_attachment(&photo).is_none(), "not selected");
    state.select(&ConversationId("a".into()));
    assert!(
        matches!(state.fetch_attachment(&photo), Some(Command::FetchAttachment(a)) if a.id == photo)
    );
    assert!(state.fetch_attachment(&photo).is_none(), "already asked");
    let update = state.apply(Event::Attachment {
        id: photo.clone(),
        content: Err(crate::Error::unreachable("gone")),
    });
    assert!(update.rows);
    assert_eq!(
        state.attachment(&photo),
        Some(&AttachmentState::Failed(crate::Error::unreachable("gone")))
    );
}

#[test]
fn long_text_replaces_the_body_once_fetched() {
    let mut state = state();
    state.apply(Event::Message(with_attachment(
        "a",
        1,
        "long",
        AttachmentKind::LongText,
    )));
    state.select(&ConversationId("a".into()));
    assert_ne!(state.messages().next().unwrap().body, "the whole story");
    state.apply(Event::Attachment {
        id: AttachmentId("long".into()),
        content: Ok(AttachmentContent::Text("the whole story".into())),
    });
    assert_eq!(state.messages().next().unwrap().body, "the whole story");
}

#[test]
fn view_once_media_is_not_saved() {
    let mut state = state();
    state.apply(Event::Message(with_attachment(
        "a",
        1,
        "once",
        AttachmentKind::ViewOnce,
    )));
    state.apply(Event::Message(with_attachment(
        "a",
        2,
        "file",
        AttachmentKind::File,
    )));
    state.select(&ConversationId("a".into()));
    assert!(
        state
            .save_attachment(&AttachmentId("once".into()))
            .is_none()
    );
    assert!(
        state
            .save_attachment(&AttachmentId("file".into()))
            .is_some()
    );
    assert!(
        state
            .save_attachment(&AttachmentId("file".into()))
            .is_none(),
        "already saving"
    );
}

#[test]
fn saving_keeps_the_shown_picture_and_failures_can_be_retried() {
    let mut state = state();
    let photo = AttachmentId("photo".into());
    state.apply(Event::Message(with_attachment(
        "a",
        1,
        "photo",
        AttachmentKind::Image,
    )));
    state.select(&ConversationId("a".into()));
    assert!(state.fetch_attachment(&photo).is_some());
    state.apply(Event::Attachment {
        id: photo.clone(),
        content: Err(crate::Error::unreachable("offline")),
    });
    assert!(
        state.fetch_attachment(&photo).is_some(),
        "retry after failure"
    );
    let picture = AttachmentContent::Picture(Arc::new(Picture {
        width: 1,
        height: 1,
        rgba: vec![0; 4],
    }));
    state.apply(Event::Attachment {
        id: photo.clone(),
        content: Ok(picture.clone()),
    });
    assert!(state.save_attachment(&photo).is_some());
    state.apply(Event::AttachmentSaved {
        id: photo.clone(),
        path: Ok("/tmp/photo.jpg".into()),
    });
    assert_eq!(
        state.attachment(&photo),
        Some(&AttachmentState::Ready(picture))
    );
    assert_eq!(
        state.saved(&photo),
        Some(&SaveState::Saved("/tmp/photo.jpg".into()))
    );
    assert!(state.fetch_attachment(&photo).is_none(), "already shown");
}

#[test]
fn search_finds_contacts_never_written_to() {
    let mut state = state();
    // "a" is a contact without messages, so it isn't listed until searched for.
    assert!(state.conversations().all(|c| c.conversation.id.0 != "a"));
    state.search("A");
    let found: Vec<_> = state
        .conversations()
        .map(|c| c.conversation.id.0.clone())
        .collect();
    assert_eq!(found, ["a"]);
    state.select(&ConversationId("a".into()));
    assert!(!state.searching());
    assert!(
        state.conversations().any(|c| c.conversation.id.0 == "a"),
        "selected stays listed"
    );
}

#[test]
fn replies_quote_the_chosen_message_and_reset_with_the_chat() {
    let mut state = state();
    state.apply(Event::Synced);
    state.apply(Event::Message(message("a", "Alice", 7, false)));
    state.select(&ConversationId("a".into()));
    state.reply_to(0);
    let quote = state.replying().unwrap().clone();
    assert_eq!(
        (
            quote.author.as_str(),
            quote.author_id.as_str(),
            quote.timestamp
        ),
        ("Alice", "alice", 7)
    );
    let sent = state.compose("sure").0;
    assert!(matches!(sent, Some(Command::Send { quote: Some(q), .. }) if q == quote));
    assert!(state.replying().is_none(), "sending uses it up");

    state.reply_to(0);
    state.select(&ConversationId("g".into()));
    assert!(state.replying().is_none(), "another chat drops it");
}

fn reaction(emoji: &str, sender: &str, outgoing: bool) -> crate::Reaction {
    crate::Reaction {
        emoji: emoji.into(),
        sender_id: sender.into(),
        sender: sender.into(),
        outgoing,
    }
}

#[test]
fn one_reaction_per_person_and_ours_toggles() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Alice", 7, false)));
    state.select(&ConversationId("a".into()));
    let target = state.messages().next().unwrap().message.reference();
    let react = |state: &mut ChatState, r, remove| {
        state.apply(Event::Reaction {
            conversation: ConversationId("a".into()),
            target: target.clone(),
            reaction: r,
            remove,
        })
    };
    assert!(react(&mut state, reaction("👍", "bob", false), false).rows);
    react(&mut state, reaction("❤️", "bob", false), false);
    react(&mut state, reaction("😂", "me", true), false);
    let emojis: Vec<_> = state
        .messages()
        .next()
        .unwrap()
        .message
        .reactions
        .iter()
        .map(|r| r.emoji.clone())
        .collect();
    assert_eq!(
        emojis,
        ["❤️", "😂"],
        "Bob's second reaction replaced his first"
    );

    // Reacting with our current emoji takes it back; another one replaces it.
    assert!(matches!(
        state.react(0, "😂"),
        Some(Command::React { remove: true, .. })
    ));
    assert!(matches!(
        state.react(0, "👍"),
        Some(Command::React { remove: false, .. })
    ));
    react(&mut state, reaction("😂", "me", true), true);
    assert_eq!(state.messages().next().unwrap().message.reactions.len(), 1);
}

#[test]
fn list_times_and_day_labels_get_coarser_with_age() {
    let now: jiff::Zoned = "2026-10-05T12:00:00[Europe/Berlin]".parse().unwrap();
    let at = |s: &str| {
        let t: jiff::Zoned = format!("{s}[Europe/Berlin]").parse().unwrap();
        t.timestamp().as_millisecond() as u64
    };
    // Formatting uses the system time zone; compare against what it says for the same instant.
    let local = |s: &str, f: &str| zoned(at(s)).unwrap().strftime(f).to_string();
    assert_eq!(
        format_list_time_at(at("2026-10-05T09:30:00"), &now),
        local("2026-10-05T09:30:00", "%H:%M")
    );
    assert_eq!(
        format_list_time_at(at("2026-10-01T09:30:00"), &now),
        local("2026-10-01T09:30:00", "%a")
    );
    assert_eq!(
        format_list_time_at(at("2026-03-02T09:30:00"), &now),
        local("2026-03-02T09:30:00", "%b %-d")
    );
    assert!(format_list_time_at(at("2024-03-02T09:30:00"), &now).ends_with("2024"));
    assert_eq!(day_label(at("2026-10-05T09:30:00"), &now), "Today");
    assert_eq!(day_label(at("2026-10-04T09:30:00"), &now), "Yesterday");
    assert_eq!(
        day_label(at("2026-03-02T09:30:00"), &now),
        local("2026-03-02T09:30:00", "%B %-d")
    );
}

/// The chat list is drawn again when its times change: when the next day starts, also
/// when the clocks change on the way.
#[test]
fn midnight_is_when_tomorrow_starts() {
    let until = |s: &str| {
        let now: jiff::Zoned = format!("{s}[Europe/Berlin]").parse().unwrap();
        until_midnight_after(&now).as_secs()
    };
    assert_eq!(until("2026-10-24T23:30:00"), 30 * 60);
    assert_eq!(until("2026-10-24T00:00:00"), 24 * 3600);
    // Back an hour at 3 o'clock.
    assert_eq!(until("2026-10-25T00:00:00"), 25 * 3600);
    assert_eq!(until("2026-03-29T00:00:00"), 23 * 3600);
    let wait = until_midnight();
    assert!(!wait.is_zero() && wait.as_secs() <= 25 * 3600, "{wait:?}");
}

#[test]
fn runs_break_on_author_gap_and_day() {
    let mut state = state();
    let minute = 60_000;
    let base = 1_790_000_000_000; // some afternoon
    for (sender, t) in [("Alice", 0), ("Alice", 1), ("Bob", 2), ("Bob", 20)] {
        state.apply(Event::Message(message(
            "g",
            sender,
            base + t * minute,
            false,
        )));
    }
    state.select(&ConversationId("g".into()));
    let runs: Vec<_> = state
        .messages()
        .map(|m| (m.first_of_run, m.last_of_run, m.show_sender))
        .collect();
    assert_eq!(
        runs,
        [
            (true, false, true),
            (false, true, false),
            (true, true, true),
            (true, true, true)
        ]
    );
    assert!(state.messages().next().unwrap().day.is_some());
    assert!(state.messages().nth(1).unwrap().day.is_none());
}

/// What a row shows, to compare rows worked out in different ways.
#[derive(Debug, PartialEq)]
struct Shown {
    message: (u64, String),
    show_sender: bool,
    body: String,
    styles: Vec<StyleRange>,
    day: Option<String>,
    first_of_run: bool,
    last_of_run: bool,
    sender_avatar: Option<*const Picture>,
    first_unread: bool,
    revealed: bool,
    highlighted: bool,
    selected: bool,
}

fn shown_row(view: &MessageView) -> Shown {
    Shown {
        message: (view.message.timestamp, view.message.sender_id.clone()),
        show_sender: view.show_sender,
        body: view.body.to_owned(),
        styles: view.styles.to_vec(),
        day: view.day.clone(),
        first_of_run: view.first_of_run,
        last_of_run: view.last_of_run,
        sender_avatar: view.sender_avatar.map(Arc::as_ptr),
        first_unread: view.first_unread,
        revealed: view.revealed,
        highlighted: view.highlighted,
        selected: view.selected,
    }
}

/// The rows as `messages()` first worked them out, plainly, one after the other: however
/// they are worked out now, they must come out the same.
fn rows_from_scratch(state: &ChatState) -> Vec<Shown> {
    let local_date = |timestamp| zoned(timestamp).map(|time| time.date());
    let group = state.selected().is_some_and(|c| c.is_group());
    let history = state.selected_history();
    let now = jiff::Zoned::now();
    let read_mark = state.read_mark.filter(|&mark| mark > 0);
    let first_unread = read_mark.and_then(|mark| {
        let first = unread_after(history, mark).next()?;
        history.iter().position(|m| std::ptr::eq(m, first))
    });
    let picked = state.selection();
    let same_run = |a: &ChatMessage, b: &ChatMessage| {
        !a.notice
            && !b.notice
            && a.sender_id == b.sender_id
            && b.timestamp.saturating_sub(a.timestamp) <= RUN_GAP_MS
            && local_date(a.timestamp) == local_date(b.timestamp)
    };
    history
        .iter()
        .enumerate()
        .map(|(i, message)| {
            let previous = i.checked_sub(1).map(|i| &history[i]);
            let next = history.get(i + 1);
            let new_day =
                previous.is_none_or(|p| local_date(p.timestamp) != local_date(message.timestamp));
            let first_of_run = previous.is_none_or(|p| !same_run(p, message));
            let full_text = state.full_text(message);
            Shown {
                message: (message.timestamp, message.sender_id.clone()),
                show_sender: group && !message.outgoing && first_of_run,
                body: match (message.deleted, full_text) {
                    (true, _) => DELETED,
                    (false, Some(text)) => text,
                    (false, None) => &message.body,
                }
                .to_owned(),
                styles: if message.deleted || full_text.is_some() {
                    Vec::new()
                } else {
                    message.styles.clone()
                },
                day: new_day.then(|| day_label(message.timestamp, &now)),
                first_of_run,
                last_of_run: next.is_none_or(|n| !same_run(message, n)),
                first_unread: first_unread == Some(i),
                revealed: state.revealed.contains(message_key(message)),
                highlighted: state.highlight.as_ref().is_some_and(|h| message.is(h)),
                selected: picked.iter().any(|p| std::ptr::eq(*p, message)),
                sender_avatar: (group && !message.outgoing)
                    .then(|| state.avatar(&ConversationId::contact(&message.sender_id)))
                    .flatten()
                    .map(Arc::as_ptr),
            }
        })
        .collect()
}

/// A group's history with a bit of everything rows show: runs and what breaks them, days
/// (some messages either side of a midnight), notices, our own messages, two at the same
/// moment, a fetched long text, a deleted message, spoilers revealed, a search result,
/// picked messages, the "new messages" line and senders' pictures.
fn varied_group() -> ChatState {
    let mut state = state();
    let minute = 60_000;
    let midnight = jiff::Zoned::now()
        .date()
        .checked_sub(jiff::Span::new().days(3))
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::system())
        .unwrap()
        .timestamp()
        .as_millisecond() as u64;
    let senders = ["Ann", "Bob", "Cy", "You"];
    let mut messages = Vec::new();
    let mut t = midnight - 90 * minute;
    for i in 0..60usize {
        t += match i % 9 {
            0 => 6 * minute,
            4 => 3 * 60 * minute,
            7 => 26 * 60 * minute,
            _ => minute,
        };
        let sender = senders[(i * 7 / 5) % senders.len()];
        let mut m = message("g", sender, t, sender == "You");
        match i % 13 {
            3 => m.notice = true,
            5 => tombstone(&mut m),
            8 => {
                let long = with_attachment("g", t, &format!("long{i}"), AttachmentKind::LongText);
                m.attachments = long.attachments;
            }
            10 => {
                m.styles = vec![StyleRange {
                    start: 0,
                    end: 3,
                    style: crate::Style::Spoiler,
                }]
            }
            _ => {}
        }
        messages.push(m);
    }
    messages.push(message("g", "Ann", t, false));
    messages.push(message("g", "Bob", t, false));
    state.apply(Event::Settings {
        conversation: id("g"),
        settings: ChatSettings {
            last_read: messages[40].timestamp,
            accepted: true,
            ..ChatSettings::default()
        },
    });
    for sender in ["ann", "bob"] {
        state.apply(Event::Avatar {
            conversation: ConversationId::contact(sender),
            avatar: Some(Arc::new(Picture {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            })),
        });
    }
    state.select(&id("g"));
    state.apply(Event::History {
        conversation: id("g"),
        messages: messages.clone(),
        reactions: Vec::new(),
        older: None,
    });
    for i in [8, 21, 34] {
        state.apply(Event::Attachment {
            id: AttachmentId(format!("long{i}")),
            content: Ok(AttachmentContent::Text("the whole story".into())),
        });
    }
    state.reveal(10);
    state.open_chat_search();
    state.search_chat("at");
    state.apply(Event::SearchResults {
        conversation: id("g"),
        query: "at".into(),
        messages: vec![messages[20].clone()],
    });
    state.show_result(0);
    state.toggle_selected(2);
    state.toggle_selected(15);
    state.select_through(18);
    state
}

#[test]
fn any_row_is_the_same_alone_as_among_the_others() {
    let state = varied_group();
    let all: Vec<Shown> = state.messages().map(|v| shown_row(&v)).collect();
    assert_eq!(all, rows_from_scratch(&state));
    for (what, count) in [
        (
            "new messages line",
            all.iter().filter(|r| r.first_unread).count(),
        ),
        ("revealed", all.iter().filter(|r| r.revealed).count()),
        ("highlighted", all.iter().filter(|r| r.highlighted).count()),
        ("picked", all.iter().filter(|r| r.selected).count()),
        (
            "sender pictures",
            all.iter().filter(|r| r.sender_avatar.is_some()).count(),
        ),
        ("days", all.iter().filter(|r| r.day.is_some()).count()),
        (
            "senders shown",
            all.iter().filter(|r| r.show_sender).count(),
        ),
        (
            "long text",
            all.iter().filter(|r| r.body == "the whole story").count(),
        ),
        ("deleted", all.iter().filter(|r| r.body == DELETED).count()),
    ] {
        assert!(count > 0, "no {what} to compare");
    }
    for (i, row) in all.iter().enumerate() {
        assert_eq!(&shown_row(&state.message_view(i).unwrap()), row, "row {i}");
        assert_eq!(
            &shown_row(&state.messages().nth(i).unwrap()),
            row,
            "row {i}"
        );
    }
    assert!(state.message_view(all.len()).is_none());
    assert!(state.messages().nth(all.len()).is_none());
    for n in 0..all.len() + 2 {
        let last: Vec<Shown> = state.last_messages(n).map(|v| shown_row(&v)).collect();
        assert_eq!(last, all[all.len().saturating_sub(n)..], "last {n}");
    }
    let skipped: Vec<Shown> = state.messages().skip(7).map(|v| shown_row(&v)).collect();
    assert_eq!(skipped, all[7..]);
    assert_eq!(state.messages().len(), all.len());
    assert_eq!(state.messages().count(), all.len());
    assert_eq!(
        state.messages().last().map(|v| shown_row(&v)).as_ref(),
        all.last()
    );
}

#[test]
fn staged_files_go_with_the_next_message() {
    let mut state = state();
    state.apply(Event::Synced);
    state.select(&ConversationId("a".into()));
    assert!(!state.can_compose(""));
    state.stage_files(["/tmp/a.pdf".into(), "/tmp/b.png".into()]);
    state.unstage(0);
    assert!(state.can_compose(""), "files alone can be sent");
    let sent = state.compose("").0;
    assert!(
        matches!(sent, Some(Command::Send { files, .. }) if files == [std::path::PathBuf::from("/tmp/b.png")])
    );
    assert!(state.staged().is_empty());
}

#[test]
fn sizes_read_naturally() {
    assert_eq!(format_size(999), "999 B");
    assert_eq!(format_size(245_760), "245 KB");
    assert_eq!(format_size(1_250_000), "1.2 MB");
}

#[test]
fn history_can_be_skipped_only_while_waiting() {
    let mut state = state();
    assert!(state.skip_history().is_none());
    state.apply(Event::HistoryTransfer(HistoryTransfer::Waiting));
    assert!(matches!(state.skip_history(), Some(Command::SkipHistory)));
    state.apply(Event::HistoryTransfer(HistoryTransfer::Importing {
        messages: 3,
    }));
    assert!(state.skip_history().is_none());
    state.apply(Event::HistoryTransfer(HistoryTransfer::Finished));
    assert_eq!(state.transfer(), None);
}

#[test]
fn history_merges_without_duplicates_in_order() {
    let mut state = state();
    let id = ConversationId("a".into());
    state.select(&id);
    state.apply(Event::Message(message("a", "Alice", 3, false)));
    let update = state.apply(Event::History {
        conversation: id,
        messages: vec![
            message("a", "Alice", 3, false),
            message("a", "You", 1, true),
        ],
        reactions: Vec::new(),
        older: None,
    });
    assert_eq!(update.messages, MessagesUpdate::Reset);
    let stamps: Vec<_> = state.messages().map(|m| m.message.timestamp).collect();
    assert_eq!(stamps, [1, 3]);
}

#[test]
fn history_dedupe_ignores_display_name_changes() {
    let mut state = state();
    let id = ConversationId("a".into());
    state.select(&id);
    // Received live before contacts synced, so only the raw id was known as a name.
    let mut live = message("a", "Alice", 5, false);
    live.sender = "3f2a…".into();
    state.apply(Event::Message(live));
    state.apply(Event::History {
        conversation: id.clone(),
        messages: vec![message("a", "Alice", 5, false)],
        reactions: Vec::new(),
        older: None,
    });
    assert_eq!(state.messages().count(), 1);
    // Same timestamp from a different author is a different message.
    state.apply(Event::History {
        conversation: id,
        messages: vec![message("a", "Bob", 5, false)],
        reactions: Vec::new(),
        older: None,
    });
    assert_eq!(state.messages().count(), 2);
}

#[test]
fn conversations_upsert_in_activity_order() {
    let mut state = state();
    state.apply(Event::Message(message("g", "Carol", 1, false)));
    state.apply(Event::Message(message("a", "Alice", 2, false)));
    // A partial update never drops conversations, and renames apply in place.
    let mut renamed = conversation("a", false);
    renamed.title = "Alice".into();
    state.apply(Event::Conversations(vec![conversation("n", true), renamed]));
    let titles: Vec<_> = state
        .conversations()
        .map(|c| c.conversation.title.as_str())
        .collect();
    assert_eq!(titles, ["Alice", "g", "n"]);
}

#[test]
fn list_holds_groups_and_active_chats_names_before_numbers() {
    let mut state = ChatState::default();
    let titled = |id: &str, title: &str, is_group| Conversation {
        title: title.into(),
        ..conversation(id, is_group)
    };
    state.apply(Event::Conversations(vec![
        titled("1", "+49 151 1234567", true),
        titled("2", "bob", true),
        titled("3", "Alice", true),
        // A synced contact never written to.
        titled("4", "Zoe", false),
    ]));
    let titles: Vec<_> = state
        .conversations()
        .map(|c| c.conversation.title.as_str())
        .collect();
    assert_eq!(titles, ["Alice", "bob", "+49 151 1234567"]);
    // Writing to them puts them on the list, at the top.
    state.apply(Event::Message(message("4", "Zoe", 1, false)));
    assert_eq!(
        state.conversations().next().unwrap().conversation.title,
        "Zoe"
    );
    assert_eq!(state.selected_index(), None);
    state.select(&ConversationId("4".into()));
    assert_eq!(state.selected_index(), Some(0));
}

#[test]
fn appends_in_one_batch_are_all_reported() {
    let mut state = state();
    state.select(&ConversationId("a".into()));
    let update = [1, 2, 3].into_iter().fold(Update::default(), |update, ts| {
        update.merge(state.apply(Event::Message(message("a", "Alice", ts, false))))
    });
    assert_eq!(update.messages, MessagesUpdate::Appended(3));
    let stamps: Vec<_> = state
        .last_messages(2)
        .map(|m| m.message.timestamp)
        .collect();
    assert_eq!(stamps, [2, 3]);
    // An out-of-order message in the same batch forces a full re-read.
    let update = update.merge(state.apply(Event::Message(message("a", "Bob", 0, false))));
    assert_eq!(update.messages, MessagesUpdate::Reset);
}

#[test]
fn the_start_screen_shows_until_caught_up() {
    let mut state = ChatState::default();
    let stage = |state: &ChatState| state.sync_stage().expect("the start screen");
    assert_eq!(stage(&state), SyncStage::Starting);
    state.apply(Event::Linked {
        account: "me".into(),
    });
    assert_eq!(stage(&state), SyncStage::Connecting);
    // Freshly linked: the phone's history comes first.
    state.apply(Event::HistoryTransfer(HistoryTransfer::Waiting));
    assert_eq!(stage(&state), SyncStage::Transfer(HistoryTransfer::Waiting));
    assert!(state.skip_history().is_some());
    state.apply(Event::HistoryTransfer(HistoryTransfer::Finished));
    assert_eq!(stage(&state), SyncStage::Connecting);
    assert!(state.skip_history().is_none());
    state.apply(Event::Connected);
    assert_eq!(stage(&state), SyncStage::CatchingUp { messages: 0 });
    let update = state.apply(Event::Message(message("a", "Ann", 5, false)));
    assert!(update.screen, "the count on the screen changed");
    assert_eq!(stage(&state), SyncStage::CatchingUp { messages: 1 });
    let update = state.apply(Event::Synced);
    assert!(update.screen);
    assert_eq!(state.screen(), &Screen::Chats);
    // Later drops don't bring the start screen back.
    state.apply(Event::Reconnecting);
    state.apply(Event::Connected);
    assert_eq!(state.screen(), &Screen::Chats);
    assert!(state.sync_stage().is_none());
}

/// Opening a chat with many unread messages shows the first of them; with a few, the end
/// (where they show anyway).
#[test]
fn a_chat_opens_at_its_first_unread_message() {
    let open = |unread: u64| {
        let mut state = state();
        state.apply(Event::Settings {
            conversation: id("a"),
            settings: ChatSettings {
                last_read: 10,
                ..Default::default()
            },
        });
        state.select(&id("a"));
        assert_eq!(state.jump(), None, "not before the history is there");
        let read = (1..=10).map(|t| message("a", "Ann", t, false));
        let new = (11..=10 + unread).map(|t| message("a", "Ann", t, false));
        state.apply(Event::History {
            conversation: id("a"),
            messages: read.chain(new).collect(),
            reactions: Vec::new(),
            older: None,
        });
        state
    };
    let many = open(20);
    assert_eq!(many.jump().map(|(row, _)| row), Some(10));
    assert!(many.messages().nth(10).unwrap().first_unread);
    assert_eq!(open(3).jump(), None);
    // Opened again (all read by now), it doesn't jump again.
    let mut again = open(20);
    let jumped = again.jump();
    again.select(&id("g"));
    again.select(&id("a"));
    assert_eq!(again.jump(), jumped);
}

/// Catching up, the chats can be opened already; the rest comes in as they show.
#[test]
fn the_chats_can_be_started_while_catching_up() {
    let mut state = ChatState::default();
    assert!(!state.can_start());
    assert_eq!(state.start(), Update::default());
    state.apply(Event::Linked {
        account: "me".into(),
    });
    assert!(!state.can_start(), "not before connecting");
    state.apply(Event::Connected);
    state.apply(Event::Message(message("a", "Ann", 5, false)));
    assert!(state.can_start());
    let update = state.start();
    assert!(update.screen && update.conversations);
    assert_eq!(state.screen(), &Screen::Chats);
    assert_eq!(state.connection(), Connection::CatchingUp);
    assert!(!state.can_start());
    state.apply(Event::Message(message("a", "Ann", 6, false)));
    state.apply(Event::Synced);
    assert_eq!(state.screen(), &Screen::Chats);
    assert_eq!(state.connection(), Connection::Online);
}

#[test]
fn an_unreachable_signal_still_shows_the_chats() {
    let mut state = ChatState::default();
    state.apply(Event::Linked {
        account: "me".into(),
    });
    let update = state.apply(Event::Reconnecting);
    assert!(update.screen);
    assert_eq!(state.screen(), &Screen::Chats);
    assert!(!state.synced());
}

#[test]
fn reconnecting_disables_sending_until_synced() {
    let mut state = state();
    state.select(&ConversationId("a".into()));
    state.apply(Event::Reconnecting);
    assert!(!state.can_compose("hi"));
    state.apply(Event::Synced);
    assert!(state.can_compose("hi"));
}

#[test]
fn live_messages_dedupe_and_insert_in_order() {
    let mut state = state();
    state.select(&ConversationId("a".into()));
    assert_eq!(
        state
            .apply(Event::Message(message("a", "Alice", 5, false)))
            .messages,
        MessagesUpdate::Appended(1)
    );
    assert_eq!(
        state.apply(Event::Message(message("a", "Alice", 5, false))),
        Update::default()
    );
    assert_eq!(
        state
            .apply(Event::Message(message("a", "Bob", 2, false)))
            .messages,
        MessagesUpdate::Reset
    );
    let stamps: Vec<_> = state.messages().map(|m| m.message.timestamp).collect();
    assert_eq!(stamps, [2, 5]);
}

#[test]
fn group_sender_shown_once_per_run() {
    let mut state = state();
    state.select(&ConversationId("g".into()));
    for (sender, ts) in [("Carol", 1), ("Carol", 2), ("Dave", 3)] {
        state.apply(Event::Message(message("g", sender, ts, false)));
    }
    state.apply(Event::Message(message("g", "You", 4, true)));
    let shown: Vec<_> = state.messages().map(|m| m.show_sender).collect();
    assert_eq!(shown, [true, false, true, false]);
    assert!(state.last_messages(1).next().unwrap().message.outgoing);
}

#[test]
fn compose_requires_selection_sync_and_text() {
    let mut state = state();
    assert!(state.compose("hi").0.is_none());
    state.select(&ConversationId("a".into()));
    assert!(state.compose("   ").0.is_none());
    assert!(matches!(state.compose(" hi ").0, Some(Command::Send { body, .. }) if body == "hi"));
}

/// The timestamp `command` sends with; panics for anything but a send.
fn sent_at(command: Option<Command>) -> u64 {
    match command {
        Some(Command::Send { timestamp, .. }) => timestamp,
        other => panic!("not a send: {other:?}"),
    }
}

#[test]
fn a_new_message_shows_while_sending_and_the_sent_one_takes_its_place() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Alice", 7, false)));
    state.select(&ConversationId("a".into()));
    let (command, update) = state.compose("hi");
    let timestamp = sent_at(command);
    assert_eq!(update.messages, MessagesUpdate::Appended(1));
    assert!(update.scroll_to_end, "sending shows the end of the chat");
    let shown = &state.last_messages(1).next().unwrap().message;
    assert_eq!(
        (shown.body.as_str(), shown.unsent),
        ("hi", Some(Unsent::Sending))
    );

    let update = state.apply(Event::Message(ChatMessage {
        body: "hi".into(),
        ..message("a", "Me", timestamp, true)
    }));
    assert!(update.rows, "redrawn in place");
    assert_eq!(update.messages, MessagesUpdate::Unchanged);
    let bodies: Vec<_> = state.messages().map(|m| m.message.unsent).collect();
    assert_eq!(bodies, [None, None], "replaced, not added");
    assert_eq!(
        state.last_messages(1).next().unwrap().message.sender_id,
        "me"
    );
}

#[test]
fn sends_in_one_millisecond_get_their_own_timestamps() {
    let mut state = state();
    state.set_clock(Clock::fixed(1_000));
    state.select(&ConversationId("a".into()));
    let first = sent_at(state.compose("one").0);
    let second = sent_at(state.compose("two").0);
    let reply = sent_at(state.reply(&ConversationId("a".into()), "three").0);
    assert_eq!([first, second, reply], [1_000, 1_001, 1_002]);
    let shown: Vec<_> = state.messages().map(|m| m.message.body.as_str()).collect();
    assert_eq!(shown, ["one", "two", "three"], "none replaces another");
}

#[test]
fn a_failed_send_stays_shown_as_failed() {
    let mut state = state();
    state.select(&ConversationId("a".into()));
    let timestamp = sent_at(state.compose("hi").0);
    let update = state.apply(Event::SendFailed {
        conversation: ConversationId("a".into()),
        timestamp,
        retryable: true,
    });
    assert!(update.rows);
    let shown = state.last_messages(1).next().unwrap().message.clone();
    assert_eq!(shown.unsent, Some(Unsent::Failed));

    // Nothing points at a message that never went out; deleting it removes it here, and
    // tells the backend only not to show it as failed again.
    assert!(state.react(0, "👍").is_none());
    state.reply_to(0);
    assert!(state.replying().is_none());
    assert!(!state.can_edit(&shown));
    assert!(!ChatState::can_forward(&shown));
    let (command, _) = state.delete(0, false);
    assert!(matches!(
        command,
        Some(Command::DropUnsent { timestamp: t, .. }) if t == timestamp
    ));
    assert_eq!(state.messages().count(), 0);
}

#[test]
fn a_failed_send_can_be_sent_again() {
    let mut state = state();
    let a = ConversationId("a".into());
    state.select(&a);
    let (first, _) = state.compose("hi");
    let timestamp = sent_at(first.clone());
    assert!(state.retry_send(0).is_none(), "still sending");
    state.apply(Event::SendFailed {
        conversation: a.clone(),
        timestamp,
        retryable: true,
    });
    let again = state.retry_send(0).expect("sent again");
    assert_eq!(sent_at(Some(again.clone())), timestamp, "the same message");
    assert!(matches!(
        (first.unwrap(), again),
        (Command::Send { body: b1, .. }, Command::Send { body: b2, .. }) if b1 == b2
    ));
    let shown = state.last_messages(1).next().unwrap().message.clone();
    assert_eq!(shown.unsent, Some(Unsent::Sending));
    // Once sent, there is nothing to send again.
    state.apply(Event::Message(ChatMessage {
        body: "hi".into(),
        ..message("a", "Me", timestamp, true)
    }));
    assert!(state.retry_send(0).is_none());
    // Nor is someone else's message.
    state.apply(Event::Message(message("a", "Bob", timestamp + 1, false)));
    assert!(state.retry_send(1).is_none());
}

/// A message b33p didn't get to send before it stopped shows as failed when it starts again,
/// once, and goes out as it was when sent again.
#[test]
fn an_interrupted_send_shows_as_failed() {
    let mut state = state();
    let a = ConversationId("a".into());
    let send = Command::Send {
        conversation: a.clone(),
        timestamp: 1_000,
        body: "hi".into(),
        quote: None,
        files: Vec::new(),
        styles: Vec::new(),
        mentions: Vec::new(),
    };
    state.apply(Event::Interrupted(vec![send.clone()]));
    // Again, as when the backend's session starts over: still once.
    state.apply(Event::Interrupted(vec![send.clone()]));
    state.select(&a);
    let shown: Vec<_> = state.messages().map(|m| m.message.clone()).collect();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].body, "hi");
    assert_eq!(shown[0].unsent, Some(Unsent::Failed));
    let again = state.retry_send(0).expect("sent again");
    assert_eq!(format!("{again:?}"), format!("{send:?}"));
}

/// The banner says in words that a message couldn't go, not the library's error, and goes
/// once the message is sent again; another error stays.
#[test]
fn a_failed_sends_error_goes_with_its_retry() {
    let mut state = state();
    let a = ConversationId("a".into());
    state.select(&a);
    let (command, _) = state.compose("hi");
    let command = command.unwrap();
    let timestamp = sent_at(Some(command.clone()));
    state.apply(Event::SendFailed {
        conversation: a.clone(),
        timestamp,
        retryable: true,
    });
    let raw = "failed to send: libsignal-service error: websocket request failed";
    state.apply(Event::CommandFailed {
        error: crate::Error::unreachable(raw),
        command: command.clone(),
    });
    assert_eq!(state.error(), Some(super::session::UNSENT));
    assert!(!state.can_retry_error(), "the bubble has the retry");
    assert!(state.retry_send(0).is_some());
    assert_eq!(state.error(), None);

    // What Signal refused says why, and another error isn't taken away by a retry.
    let (refused, _) = state.compose("ho");
    let refused = refused.unwrap();
    state.apply(Event::CommandFailed {
        error: crate::Error::refused("failed to send: recipient is not registered"),
        command: refused,
    });
    assert_eq!(
        state.error(),
        Some("failed to send: recipient is not registered")
    );
    state.apply(Event::SendFailed {
        conversation: a.clone(),
        timestamp,
        retryable: true,
    });
    state.apply(Event::Error(crate::Error::unreachable(
        "Receiving messages failed; trying again.",
    )));
    assert!(state.retry_send(0).is_some());
    assert_eq!(
        state.error(),
        Some("Receiving messages failed; trying again.")
    );
}

/// A message Signal refused (the recipient left Signal, say) shows as not sent, without a way
/// to send it again: as it is, it won't go.
#[test]
fn a_refused_send_isnt_offered_again() {
    let mut state = state();
    let a = ConversationId("a".into());
    state.select(&a);
    let timestamp = sent_at(state.compose("hi").0);
    state.apply(Event::SendFailed {
        conversation: a,
        timestamp,
        retryable: false,
    });
    let shown = state.last_messages(1).next().unwrap().message.clone();
    assert_eq!(shown.unsent, Some(Unsent::Refused));
    assert!(state.retry_send(0).is_none());
}

/// What failed for a reason that may pass is offered again from the error, once; what was
/// refused, and sends (whose bubble has its own retry), aren't.
#[test]
fn failed_commands_are_offered_again_when_that_may_work() {
    let mut state = state();
    let react = Command::React {
        conversation: id("a"),
        target: MessageRef {
            author_id: "ann".into(),
            timestamp: 1,
        },
        emoji: "👍".into(),
        remove: false,
    };
    state.apply(Event::CommandFailed {
        error: crate::Error::unreachable("failed to send: timed out"),
        command: react,
    });
    assert_eq!(state.error(), Some("failed to send: timed out"));
    assert!(state.can_retry_error());
    assert!(matches!(state.retry_error(), Some(Command::React { .. })));
    assert_eq!(state.error(), None, "trying again takes the error away");
    assert!(state.retry_error().is_none(), "once");

    state.apply(Event::CommandFailed {
        error: crate::Error::refused("not a member"),
        command: Command::LoadMedia(id("g")),
    });
    assert!(state.error().is_some());
    assert!(!state.can_retry_error(), "refused");

    state.select(&id("a"));
    let send = state.compose("hi").0.expect("a send");
    state.apply(Event::CommandFailed {
        error: crate::Error::unreachable("offline"),
        command: send,
    });
    assert!(!state.can_retry_error(), "the bubble offers it");

    state.apply(Event::CommandFailed {
        error: crate::Error::unreachable("offline"),
        command: Command::LoadMedia(id("g")),
    });
    state.dismiss_error();
    assert!(!state.can_retry_error(), "dismissed with its error");
}

/// A picture gone from Signal's servers isn't asked for again; one that failed to come over
/// the network is.
#[test]
fn only_downloads_that_may_work_are_tried_again() {
    let mut state = state();
    for (name, at) in [("gone", 1), ("offline", 2)] {
        state.apply(Event::Message(with_attachment(
            "a",
            at,
            name,
            AttachmentKind::Image,
        )));
    }
    state.select(&id("a"));
    let gone = AttachmentId("gone".into());
    let offline = AttachmentId("offline".into());
    assert!(state.fetch_attachment(&gone).is_some());
    assert!(state.fetch_attachment(&offline).is_some());
    state.apply(Event::Attachment {
        id: gone.clone(),
        content: Err(crate::Error::refused("no longer available")),
    });
    state.apply(Event::Attachment {
        id: offline.clone(),
        content: Err(crate::Error::unreachable("timed out")),
    });
    assert!(state.fetch_attachment(&gone).is_none());
    assert!(state.fetch_attachment(&offline).is_some());
}

#[test]
fn files_show_by_name_until_sent() {
    let mut state = state();
    state.select(&ConversationId("a".into()));
    state.stage_files(vec![std::path::PathBuf::from("/tmp/b.png")]);
    state.compose("look");
    let shown = &state.last_messages(1).next().unwrap().message;
    assert_eq!(shown.body, "look\n📎 b.png");
}

#[test]
fn a_reply_from_a_notification_shows_while_sending() {
    let mut state = state();
    let a = ConversationId("a".into());
    state.select(&a);
    let (command, update) = state.reply(&a, "thanks");
    sent_at(command);
    assert_eq!(update.messages, MessagesUpdate::Appended(1));
    let shown = &state.last_messages(1).next().unwrap().message;
    assert_eq!(shown.unsent, Some(Unsent::Sending));
}

#[test]
fn new_password_is_validated_before_unlocking() {
    let mut state = ChatState::default();
    assert!(
        state.unlock("whatever", "whatever").is_none(),
        "no password requested yet"
    );
    state.apply(Event::PasswordRequired {
        new: true,
        error: None,
    });
    assert!(state.unlock("short", "short").is_none());
    assert!(matches!(state.screen(), Screen::Password { error: Some(e), .. } if e.contains("8")));
    assert!(state.unlock("long enough", "long enougj").is_none());
    let command = state.unlock("long enough", "long enough");
    assert!(matches!(command, Some(Command::Unlock(p)) if p.expose() == "long enough"));
    assert_eq!(state.screen(), &Screen::Syncing(SyncStage::Starting));
}

#[test]
fn a_failure_waits_for_an_answer() {
    let mut state = ChatState::default();
    assert!(state.retry().is_none(), "nothing failed");
    state.apply(Event::Failed {
        failure: Failure::KeyringLocked,
        message: "locked".into(),
    });
    assert!(matches!(state.screen(), Screen::Failed { message, .. } if message == "locked"));
    assert!(
        !state.can_start_over(),
        "waiting may help; nothing to delete"
    );
    assert!(state.start_over().is_none());
    assert!(matches!(state.use_password(), Some(Command::UsePassword)));
    assert_eq!(state.screen(), &Screen::Syncing(SyncStage::Starting));

    state.apply(Event::Failed {
        failure: Failure::KeyMissing,
        message: "missing".into(),
    });
    assert!(state.use_password().is_none());
    assert!(matches!(state.retry(), Some(Command::Retry)));
    state.apply(Event::Failed {
        failure: Failure::KeyMissing,
        message: "missing".into(),
    });
    assert!(matches!(state.start_over(), Some(Command::StartOver)));
    assert_eq!(state.screen(), &Screen::Syncing(SyncStage::Starting));

    state.apply(Event::Failed {
        failure: Failure::Stopped,
        message: "gone".into(),
    });
    assert!(state.retry().is_none(), "nobody is there to try again");
    assert!(!state.can_start_over());
}

#[test]
fn the_connection_is_followed() {
    let mut state = ChatState::default();
    state.apply(Event::Linked {
        account: "me".into(),
    });
    assert_eq!(state.connection(), Connection::Offline);
    state.apply(Event::Connected);
    assert_eq!(state.connection(), Connection::CatchingUp);
    state.apply(Event::Synced);
    assert_eq!(state.connection(), Connection::Online);
    assert!(
        state.apply(Event::Reconnecting).screen,
        "the status shows anew"
    );
    assert_eq!(state.connection(), Connection::Offline);
    state.apply(Event::Connected);
    state.apply(Event::Synced);
    assert_eq!(state.connection(), Connection::Online);
}

#[test]
fn a_forgotten_password_starts_over() {
    let mut state = ChatState::default();
    state.apply(Event::PasswordRequired {
        new: true,
        error: None,
    });
    assert!(state.start_over().is_none(), "nothing to forget yet");
    state.apply(Event::PasswordRequired {
        new: false,
        error: None,
    });
    assert!(matches!(state.start_over(), Some(Command::StartOver)));
}

#[test]
fn unlinking_forgets_the_account() {
    let mut state = ChatState::default();
    assert!(state.unlink().is_none(), "not linked");
    state.set_notifications(true, NotificationContent::default());
    state.apply(Event::Linked {
        account: "+1 555".into(),
    });
    state.apply(Event::Synced);
    let a = conversation("a", false);
    state.apply(Event::Conversations(vec![a.clone()]));
    assert!(state.conversation(&a.id).is_some());
    assert!(matches!(state.unlink(), Some(Command::Unlink)));
    assert_eq!(state.account(), "");
    assert!(state.conversation(&a.id).is_none());
    assert_eq!(state.screen(), &Screen::Syncing(SyncStage::Starting));
    assert!(state.notifications.enabled, "the UI's settings stay");
}

#[test]
fn existing_password_needs_no_confirmation() {
    let mut state = ChatState::default();
    state.apply(Event::PasswordRequired {
        new: false,
        error: Some("Wrong password.".into()),
    });
    assert!(state.unlock("", "").is_none());
    assert!(state.unlock("x", "").is_some());
}

#[test]
fn time_shows_date_only_for_other_days() {
    let now = jiff::Timestamp::now().as_millisecond() as u64;
    assert_eq!(format_time(now).len(), "14:05".len());
    assert!(format_time(86_400_000).contains(", "));
}

#[test]
fn initials_handle_symbols_and_empty() {
    assert_eq!(initials("Rustaceans 🦀"), "R");
    assert_eq!(initials("ada lovelace"), "AL");
    assert_eq!(initials(""), "?");
}

fn id(s: &str) -> ConversationId {
    ConversationId(s.into())
}

fn listed(state: &ChatState) -> Vec<String> {
    state
        .conversations()
        .map(|c| c.conversation.id.0.clone())
        .collect()
}

/// Typed like a phone number (as people write them) is looked up; names, short numbers and
/// text with letters aren't.
#[test]
fn phone_numbers_are_told_from_names() {
    for number in [
        "+49 151 23456789",
        "0151/2345678",
        "(030) 123-4567",
        "+4915123456789",
    ] {
        assert!(looks_like_number(number), "{number}");
    }
    for text in [
        "Ann",
        "12345",
        "call 0151 2345678",
        "+49 151 2345678 9999999",
        "",
    ] {
        assert!(!looks_like_number(text), "{text}");
    }
}

/// Looking a number up opens the chat with them, or chooses them for the group being made.
#[test]
fn someone_found_by_number_is_opened_or_chosen() {
    use crate::AfterFinding::{Choose, Open};
    let mut state = state();
    assert!(state.find_number("Ann", Open).is_none(), "not a number");
    assert!(matches!(
        state.find_number(" +49 151 23456789 ", Open),
        Some(Command::FindNumber { number, then: Open }) if number == "+49 151 23456789"
    ));
    state.apply(Event::Found {
        conversation: id("a"),
        then: Open,
    });
    assert_eq!(state.selected().map(|c| c.id.clone()), Some(id("a")));

    state.open_group_draft(Some(id("g")));
    state.apply(Event::Found {
        conversation: id("a"),
        then: Choose,
    });
    assert!(matches!(state.group_draft(), Some((false, [chosen])) if *chosen == id("a")));
    state.apply(Event::Found {
        conversation: id("a"),
        then: Choose,
    });
    assert!(
        matches!(state.group_draft(), Some((false, [_]))),
        "found again: still chosen, not unchosen"
    );
}

/// An invitation is answered while we're invited, not in the group yet; other changes need
/// being in it.
#[test]
fn invitations_are_answered_before_joining() {
    use crate::GroupEdit::{AcceptInvitation, DeclineInvitation, Title};
    let mut state = state();
    let invited = Conversation {
        left: true,
        invited: true,
        ..conversation("club", true)
    };
    state.apply(Event::Conversations(vec![invited]));
    let club = id("club");
    assert!(matches!(
        state.edit_group(&club, AcceptInvitation),
        Some(Command::EditGroup {
            edit: AcceptInvitation,
            ..
        })
    ));
    assert!(state.edit_group(&club, DeclineInvitation).is_some());
    assert!(
        state.edit_group(&club, Title("Ours".into())).is_none(),
        "not in it yet"
    );
    // A group we're in has nothing to accept.
    assert!(state.edit_group(&id("g"), AcceptInvitation).is_none());
    assert!(state.edit_group(&id("g"), Title("Ours".into())).is_some());
}

/// Verifying acts on the open details page, and only with a safety number to verify; the
/// answer updates what it shows.
#[test]
fn verifying_from_the_details_page() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 1, false)));
    assert!(state.set_verified(true).is_none(), "no details page open");
    state.select(&id("a"));
    assert!(state.open_details().is_some());
    assert!(state.set_verified(true).is_none(), "details not loaded yet");
    state.apply(Event::Details {
        conversation: id("a"),
        details: crate::Details::default(),
    });
    assert!(state.set_verified(true).is_none(), "no safety number yet");
    state.apply(Event::Details {
        conversation: id("a"),
        details: crate::Details {
            safety_number: Some("12345".into()),
            ..Default::default()
        },
    });
    assert!(matches!(
        state.set_verified(true),
        Some(Command::SetVerified { conversation, verified: true }) if conversation == id("a")
    ));
    state.apply(Event::Trust {
        conversation: id("b"),
        trust: crate::Trust::Verified,
    });
    assert_eq!(
        state.details().unwrap().unwrap().trust,
        crate::Trust::Unverified,
        "another chat's"
    );
    state.apply(Event::Trust {
        conversation: id("a"),
        trust: crate::Trust::Verified,
    });
    assert_eq!(
        state.details().unwrap().unwrap().trust,
        crate::Trust::Verified
    );
}

/// When the safety number of a contact we verified changes, nothing is sent to them until
/// that is confirmed (or it is verified again), as Signal asks.
#[test]
fn a_changed_verified_safety_number_stops_sending_until_confirmed() {
    let mut state = state();
    state.apply(Event::Message(message("a", "a", 1, false)));
    state.select(&id("a"));
    assert!(state.can_compose("hi"), "writable before");
    assert!(
        state.confirm_safety_number(&id("a")).is_none(),
        "nothing to confirm"
    );

    state.apply(Event::Trust {
        conversation: id("a"),
        trust: crate::Trust::Changed,
    });
    assert!(!state.can_compose("hi"));
    assert!(state.compose("hi").0.is_none(), "not sent");
    assert!(state.react(0, "👍").is_none(), "no reaction either");
    assert!(matches!(
        state.confirm_safety_number(&id("a")),
        Some(Command::SetVerified { conversation, verified: false }) if conversation == id("a")
    ));

    state.apply(Event::Trust {
        conversation: id("a"),
        trust: crate::Trust::Unverified,
    });
    assert!(state.can_compose("hi"), "writable once confirmed");
}

/// Our own notices (a timer we set, a change we made to a group) aren't messages we wrote:
/// nothing to edit or delete for everyone, and "edit the last one" passes over them.
#[test]
fn our_notices_cannot_be_edited() {
    let mut state = state();
    let now = now_millis();
    state.apply(Event::Message(message("a", "You", now - 2, true)));
    state.apply(Event::Message(ChatMessage {
        notice: true,
        body: "You set disappearing messages to 1 hour.".into(),
        ..message("a", "You", now - 1, true)
    }));
    state.select(&id("a"));
    let at = state.messages().position(|v| v.message.notice).unwrap();
    let notice = state.messages().nth(at).unwrap().message;
    assert!(
        !state.can_edit(notice),
        "nothing to edit or delete for everyone"
    );
    assert!(state.edit_last().is_some(), "the message before it");
    assert_eq!(state.editing().unwrap().timestamp, now - 2);
}

/// A notice (e.g. a changed safety number) is shown, but nobody wrote it: no notification,
/// not unread, and no read receipt for it.
#[test]
fn notices_are_shown_and_nothing_more() {
    let notice = |timestamp| ChatMessage {
        notice: true,
        ..message("a", "Ann", timestamp, false)
    };
    let mut state = notifying();
    state.apply(Event::Message(notice(10)));
    assert!(shown(&state.take_notices()).is_empty(), "no notification");
    assert_eq!(state.unread_total(), 0, "not unread");

    state.apply(Event::Message(message("a", "Ann", 20, false)));
    state.take_notices();
    assert_eq!(state.unread_total(), 1, "only the real message");
    state.set_window_focused(true);
    state.select(&id("a"));
    history_arrives(&mut state, "a");
    assert!(
        matches!(
            &state.take_commands()[..],
            [Command::MarkRead { messages, .. }]
                if messages.len() == 1 && messages[0].timestamp == 20
        ),
        "a receipt for the message only"
    );

    // Nothing to answer, react to, forward or delete; and it stands apart from Ann's run.
    let at = state.messages().position(|v| v.message.notice).unwrap();
    state.reply_to(at);
    assert!(state.replying().is_none(), "no reply");
    assert!(state.react(at, "👍").is_none(), "no reaction");
    assert!(!state.start_forward(at), "no forwarding");
    assert!(state.delete(at, false).0.is_none(), "no deleting");
    let view = state.messages().nth(at).unwrap();
    assert!(view.first_of_run && view.last_of_run, "a run of its own");
}

/// The chat's stored history, as far as this state has it (nothing more).
fn history_arrives(state: &mut ChatState, chat: &str) {
    state.apply(Event::History {
        conversation: id(chat),
        messages: Vec::new(),
        reactions: Vec::new(),
        older: None,
    });
}

/// What arrived while b33p was closed is read with the rest once its page is here, not
/// only what was known when the chat was opened (the newest, from `Event::Latest`), and
/// so is a message that came late, dated before what was read. A message dated in the
/// future doesn't make everything until then count as read.
#[test]
fn reading_covers_everything_unread() {
    let mut state = state();
    state.set_window_focused(true);
    state.apply(Event::Latest(message("a", "Ann", 50, false)));
    state.select(&id("a"));
    state.apply(Event::History {
        conversation: id("a"),
        messages: [10, 20, 30, 40, 50]
            .map(|ts| message("a", "Ann", ts, false))
            .into(),
        reactions: Vec::new(),
        older: None,
    });
    let read = |state: &mut ChatState| -> Vec<u64> {
        state
            .take_commands()
            .into_iter()
            .flat_map(|command| match command {
                Command::MarkRead { messages, .. } => messages,
                _ => Vec::new(),
            })
            .map(|m| m.timestamp)
            .collect()
    };
    let mut all = read(&mut state);
    all.sort_unstable();
    assert_eq!(all, [10, 20, 30, 40, 50]);

    // Late, while another chat is open.
    state.select(&id("g"));
    state.apply(Event::Message(message("a", "Ann", 35, false)));
    assert_eq!(state.unread_total(), 1);
    state.select(&id("a"));
    assert_eq!(read(&mut state), [35]);

    // From the future: the mark stays in the present, and the message is read once.
    let future = now_millis() + 365 * 24 * 3600 * 1000;
    state.apply(Event::Message(message("a", "Ann", future, false)));
    assert_eq!(read(&mut state), [future]);
    assert!(state.conversation(&id("a")).unwrap().settings.last_read <= now_millis());
    state.set_window_focused(false);
    state.set_window_focused(true);
    assert!(read(&mut state).is_empty());
}

#[test]
fn viewing_a_chat_reads_it_and_queues_receipts_once() {
    let mut state = state();
    state.set_window_focused(true);
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.apply(Event::Message(message("a", "Ann", 20, false)));
    assert_eq!(state.unread_total(), 2);
    state.select(&id("a"));
    // Read once what is stored has arrived: until then, more may be unread than is known.
    assert!(state.take_commands().is_empty());
    assert_eq!(state.unread_total(), 0, "the badge goes at once");
    history_arrives(&mut state, "a");
    let commands = state.take_commands();
    assert!(matches!(
        &commands[..],
        [Command::MarkRead { conversation, messages, .. }]
            if *conversation == id("a") && messages.len() == 2
    ));
    assert_eq!(state.unread_total(), 0);
    assert_eq!(state.conversation(&id("a")).unwrap().settings.last_read, 20);
    // Seen as it arrives while the window has focus; nothing twice.
    state.apply(Event::Message(message("a", "Ann", 30, false)));
    let commands = state.take_commands();
    assert!(matches!(
        &commands[..],
        [Command::MarkRead { messages, .. }] if messages.len() == 1 && messages[0].timestamp == 30
    ));
    state.set_window_focused(false);
    state.apply(Event::Message(message("a", "Ann", 40, false)));
    assert!(state.take_commands().is_empty());
    assert_eq!(state.unread_total(), 1);
    state.set_window_focused(true);
    assert_eq!(state.take_commands().len(), 1);
    assert_eq!(state.unread_total(), 0);
}

/// As in Signal's apps, an edit of a message read before is read anew, so its sender hears
/// of it; its disappearing timer runs already.
#[test]
fn an_edit_is_read_anew() {
    let mut state = state();
    state.set_window_focused(true);
    state.select(&id("a"));
    history_arrives(&mut state, "a");
    state.apply(Event::Message(ChatMessage {
        expire_timer: Some(60),
        ..message("a", "Ann", 10, false)
    }));
    assert_eq!(state.take_commands().len(), 1);
    let target = state.messages().next().unwrap().message.reference();
    let edit = |state: &mut ChatState, at| {
        state.apply(Event::Edited {
            conversation: id("a"),
            target: target.clone(),
            message: ChatMessage {
                edited_at: Some(at),
                ..message("a", "Ann", 10, false)
            },
        })
    };
    edit(&mut state, 20);
    let commands = state.take_commands();
    assert!(
        matches!(
            &commands[..],
            [Command::MarkRead { messages, expiring, .. }]
                if *messages == [target.clone()] && expiring.is_empty()
        ),
        "{commands:?}"
    );
    // Not while the chat isn't looked at; then once.
    state.set_window_focused(false);
    edit(&mut state, 30);
    edit(&mut state, 40);
    assert!(state.take_commands().is_empty());
    state.set_window_focused(true);
    let commands = state.take_commands();
    assert!(
        matches!(&commands[..], [Command::MarkRead { messages, .. }] if *messages == [target.clone()]),
        "{commands:?}"
    );
    // Our own edits aren't read.
    state.apply(Event::Message(message("a", "You", 50, true)));
    let ours = state.last_messages(1).next().unwrap().message.reference();
    state.apply(Event::Edited {
        conversation: id("a"),
        target: ours,
        message: ChatMessage {
            edited_at: Some(60),
            ..message("a", "You", 50, true)
        },
    });
    assert!(state.take_commands().is_empty());
}

/// Catching up with the chat open: what arrives in one batch is read with one receipt, in
/// order, a late one (dated before what was read) too. What was seen is read even if
/// another chat opens, or the message goes, before the batch ends.
#[test]
fn a_batch_seen_is_read_with_one_receipt() {
    let mut state = state();
    state.set_window_focused(true);
    state.select(&id("a"));
    history_arrives(&mut state, "a");
    state.apply(Event::Message(message("a", "Ann", 100, false)));
    state.take_commands();
    let read = |state: &mut ChatState| -> Vec<(ConversationId, Vec<u64>)> {
        state
            .take_commands()
            .into_iter()
            .filter_map(|command| match command {
                Command::MarkRead {
                    conversation,
                    messages,
                    ..
                } => Some((conversation, messages.iter().map(|m| m.timestamp).collect())),
                _ => None,
            })
            .collect()
    };
    let mut batch: Vec<Event> = (200..250)
        .map(|ts| Event::Message(message("a", "Ann", ts, false)))
        .collect();
    batch.push(Event::Message(message("a", "Bob", 50, false)));
    batch.push(Event::Message(message("a", "Me", 300, true)));
    state.apply_all(batch);
    let expected: Vec<u64> = std::iter::once(50).chain(200..250).collect();
    assert_eq!(read(&mut state), [(id("a"), expected)]);
    assert_eq!(state.unread_total(), 0);
    assert_eq!(
        state.conversation(&id("a")).unwrap().settings.last_read,
        249
    );

    state.apply_all([
        Event::Message(message("a", "Ann", 400, false)),
        Event::Found {
            conversation: id("g"),
            then: crate::AfterFinding::Open,
        },
        Event::Message(message("a", "Ann", 401, false)),
    ]);
    assert_eq!(read(&mut state), [(id("a"), vec![400])]);
    assert_eq!(state.unread_total(), 1, "the one that came with g open");

    state.select(&id("a"));
    assert_eq!(read(&mut state), [(id("a"), vec![401])]);
    let gone = message("a", "Ann", 500, false);
    state.apply_all([
        Event::Message(gone.clone()),
        Event::Expired {
            conversation: id("a"),
            targets: vec![gone.reference()],
        },
    ]);
    assert_eq!(read(&mut state), [(id("a"), vec![500])]);
}

#[test]
fn new_messages_line_stays_where_reading_began() {
    let mut state = state();
    state.apply(Event::History {
        conversation: id("a"),
        messages: vec![
            message("a", "Ann", 10, false),
            message("a", "Ann", 20, false),
        ],
        reactions: Vec::new(),
        older: None,
    });
    state.apply(Event::Settings {
        conversation: id("a"),
        settings: ChatSettings {
            last_read: 10,
            ..ChatSettings::default()
        },
    });
    state.select(&id("a"));
    let firsts: Vec<bool> = state.messages().map(|m| m.first_unread).collect();
    assert_eq!(firsts, [false, true]);
}

#[test]
fn read_elsewhere_clears_what_was_read_there() {
    let mut state = state();
    for ts in [10, 20, 30] {
        state.apply(Event::Message(message("a", "Ann", ts, false)));
    }
    state.take_notices();
    state.apply(Event::Settings {
        conversation: id("a"),
        settings: ChatSettings {
            last_read: 20,
            ..ChatSettings::default()
        },
    });
    assert_eq!(state.unread_total(), 1);
    state.apply(Event::Settings {
        conversation: id("a"),
        settings: ChatSettings {
            last_read: 30,
            ..ChatSettings::default()
        },
    });
    assert_eq!(state.unread_total(), 0);
    assert_eq!(state.take_notices(), [Notice::Withdraw(id("a"))]);
}

/// Chats are found by id wherever adding, renaming, sorting and starting over put them.
#[test]
fn chats_are_found_wherever_they_moved() {
    let mut state = state();
    state.assert_indexed();
    state.apply_all([
        Event::Conversations(vec![
            conversation("b", false),
            conversation("c", true),
            conversation("b", false),
        ]),
        Event::Message(message("c", "Cy", 10, false)),
        Event::Conversations(vec![Conversation {
            title: "Bea".into(),
            ..conversation("b", false)
        }]),
    ]);
    state.assert_indexed();
    assert_eq!(state.conversation(&id("b")).unwrap().title, "Bea");
    for (chat, ts) in [("a", 20), ("b", 30), ("g", 5)] {
        state.apply(Event::Message(message(chat, "Ann", ts, false)));
        state.assert_indexed();
    }
    assert_eq!(listed(&state), ["b", "a", "c", "g"]);
    state.toggle_pin(&id("g"));
    state.assert_indexed();
    state.archive(&id("b"), true);
    state.mute(&id("a"), Some(u64::MAX));
    state.assert_indexed();
    assert_eq!(listed(&state), ["g", "a", "c"]);
    for chat in ["a", "b", "c", "g"] {
        assert_eq!(state.conversation(&id(chat)).unwrap().id, id(chat));
    }
    assert!(state.conversation(&id("nope")).is_none());
    state.select(&id("c"));
    assert_eq!(state.selected().map(|c| &c.id), Some(&id("c")));
    assert!(state.unlink().is_some());
    state.assert_indexed();
    assert!(state.conversation(&id("a")).is_none());
}

#[test]
fn pinned_chats_come_first_in_their_order() {
    let mut state = state();
    state.apply(Event::Conversations(vec![conversation("b", false)]));
    for (chat, ts) in [("a", 30), ("b", 20), ("g", 10)] {
        state.apply(Event::Message(message(chat, "Ann", ts, false)));
    }
    assert_eq!(listed(&state), ["a", "b", "g"]);
    assert!(matches!(state.toggle_pin(&id("g")), Some(Command::SetPins(p)) if p == [id("g")]));
    state.toggle_pin(&id("b"));
    assert_eq!(listed(&state), ["g", "b", "a"]);
    assert!(
        matches!(state.move_pin(&id("b"), 0), Some(Command::SetPins(p)) if p == [id("b"), id("g")])
    );
    assert_eq!(listed(&state), ["b", "g", "a"]);
    state.toggle_pin(&id("b"));
    assert_eq!(listed(&state), ["g", "a", "b"]);
    assert!(state.move_pin_by(&id("a"), -1).is_none(), "not pinned");
    state.toggle_pin(&id("a"));
    assert!(state.move_pin_by(&id("g"), -1).is_none(), "already first");
    state.move_pin_by(&id("g"), 1);
    assert_eq!(listed(&state), ["a", "g", "b"]);
    state.toggle_pin(&id("a"));
    assert!(state.select_pinned(0).is_some());
    assert_eq!(state.selected().unwrap().id, id("g"));
    assert!(state.select_pinned(1).is_none());
}

#[test]
fn archived_chats_have_their_own_list() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.select(&id("a"));
    assert!(matches!(
        state.archive(&id("a"), true),
        Some(Command::Archive { archived: true, .. })
    ));
    assert!(state.selected().is_none());
    assert_eq!(listed(&state), ["g"]);
    assert_eq!(state.archived(), (1, 0));
    state.show_archived(true);
    assert_eq!(listed(&state), ["a"]);
}

#[test]
fn muted_chats_neither_notify_nor_count() {
    let mut state = notifying();
    state.mute(&id("a"), Some(u64::MAX));
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    assert!(state.take_notices().is_empty());
    assert_eq!(state.unread_total(), 0);
    assert_eq!(
        state
            .conversations()
            .find(|c| c.conversation.id == id("a"))
            .unwrap()
            .unread,
        1
    );
    state.mute(&id("a"), None);
    assert_eq!(state.unread_total(), 1);
}

#[test]
fn marked_unread_counts_as_one_until_opened() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.select(&id("a"));
    history_arrives(&mut state, "a");
    state.take_commands();
    assert!(matches!(
        state.mark_unread(&id("a"), true),
        Some(Command::MarkUnread(_))
    ));
    assert!(state.selected().is_none());
    assert_eq!(state.unread_total(), 1);
    state.select(&id("a"));
    history_arrives(&mut state, "a");
    assert!(
        matches!(&state.take_commands()[..], [Command::MarkRead { messages, .. }] if messages.is_empty())
    );
    assert_eq!(state.unread_total(), 0);
}

#[test]
fn receipts_edits_and_deletions_change_messages_in_place() {
    let mut state = state();
    state.select(&id("a"));
    state.apply(Event::Message(message("a", "You", 10, true)));
    let update = state.apply(Event::Delivered {
        conversation: id("a"),
        timestamps: vec![10],
        delivery: Delivery::Read,
    });
    assert!(update.rows);
    // Never backwards.
    state.apply(Event::Delivered {
        conversation: id("a"),
        timestamps: vec![10],
        delivery: Delivery::Delivered,
    });
    let target = state.messages().next().unwrap().message.reference();
    assert_eq!(
        state.messages().next().unwrap().message.delivery,
        Delivery::Read
    );
    state.apply(Event::Edited {
        conversation: id("a"),
        target: target.clone(),
        message: ChatMessage {
            body: "better".into(),
            edited_at: Some(50),
            ..message("a", "You", 10, true)
        },
    });
    let edited = state.messages().next().unwrap().message;
    assert_eq!(edited.body, "better");
    // As in Signal's apps: the edit hasn't been read yet, until a receipt for it comes (the
    // backend tells it by the message's own time).
    assert_eq!(edited.delivery, Delivery::Sent);
    assert_eq!(edited.edits, [(50, "You at 10\nsecond line".to_string())]);
    state.apply(Event::Delivered {
        conversation: id("a"),
        timestamps: vec![10],
        delivery: Delivery::Read,
    });
    assert_eq!(
        state.messages().next().unwrap().message.delivery,
        Delivery::Read
    );
    state.apply(Event::Deleted {
        conversation: id("a"),
        target: target.clone(),
    });
    let deleted = state.messages().next().unwrap().message;
    assert!(deleted.deleted && deleted.body.is_empty());
    let update = state.apply(Event::Expired {
        conversation: id("a"),
        targets: vec![target],
    });
    assert_eq!(update.messages, MessagesUpdate::Reset);
    assert_eq!(state.messages().len(), 0);
}

/// Once the message being edited is gone, the composer writes a new message again.
#[test]
fn an_edit_of_a_gone_message_is_a_new_message() {
    let mut state = state();
    state.select(&id("a"));
    state.apply(Event::Message(ChatMessage {
        timestamp: now_millis(),
        ..message("a", "You", 0, true)
    }));
    let ours = state.messages().position(|m| m.message.outgoing).unwrap();
    assert!(state.edit(ours).is_some());
    let target = state.editing().unwrap().reference();
    state.apply(Event::Expired {
        conversation: id("a"),
        targets: vec![target],
    });
    assert!(state.editing().is_none());
    assert!(matches!(state.compose("new").0, Some(Command::Send { .. })));
}

#[test]
fn composing_formats_and_edits() {
    let mut state = state();
    state.select(&id("a"));
    assert!(matches!(
        state.compose("*hi* there").0,
        Some(Command::Send { body, styles, .. }) if body == "hi there" && styles.len() == 1
    ));
    state.apply(Event::Message(ChatMessage {
        timestamp: now_millis(),
        ..message("a", "You", 0, true)
    }));
    state.apply(Event::Message(message("a", "Ann", 1, false)));
    // The sent one: in another millisecond than the composed message, it doesn't replace
    // that one's echo, which is still sending and can't be edited.
    let ours = state
        .messages()
        .position(|m| m.message.outgoing && m.message.unsent.is_none())
        .unwrap();
    let theirs = state.messages().position(|m| !m.message.outgoing).unwrap();
    assert_eq!(state.edit(theirs), None);
    assert!(state.edit(ours).is_some());
    assert!(matches!(
        state.compose("fixed").0,
        Some(Command::Edit { body, .. }) if body == "fixed"
    ));
    // Editing ends with the edit.
    assert!(matches!(
        state.compose("next").0,
        Some(Command::Send { .. })
    ));
}

/// Editing starts from the message as typed: sent again, it keeps its spoiler (which
/// would otherwise be revealed to everyone), its formatting and whom it mentions.
#[test]
fn an_edit_keeps_spoilers_formatting_and_mentions() {
    let mut state = state();
    state.select(&id("a"));
    let body = "@Ann the butler did it";
    state.apply(Event::Message(ChatMessage {
        body: body.into(),
        timestamp: now_millis(),
        styles: vec![
            StyleRange {
                start: 9,
                end: 15,
                style: crate::Style::Spoiler,
            },
            StyleRange {
                start: 16,
                end: 22,
                style: crate::Style::Bold,
            },
        ],
        mentions: vec![Mention {
            start: 0,
            end: 4,
            service_id: "ann".into(),
        }],
        ..message("a", "You", 0, true)
    }));
    let ours = state.messages().position(|m| m.message.outgoing).unwrap();
    let typed = state.edit(ours).unwrap();
    assert_eq!(typed, "@Ann the ||butler|| *did it*");
    let Some(Command::Edit {
        body: sent,
        styles,
        mentions,
        ..
    }) = state.compose(&typed.replace("did", "done")).0
    else {
        panic!("edits");
    };
    assert_eq!(sent, "@Ann the butler done it");
    assert!(
        styles
            .iter()
            .any(|s| s.style == crate::Style::Spoiler && &sent[s.start..s.end] == "butler")
    );
    assert_eq!(styles.len(), 2);
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].service_id, "ann");
}

#[test]
fn mentions_are_picked_from_the_group_and_sent_where_they_stand() {
    let mut state = state();
    state.apply(Event::Conversations(vec![Conversation {
        members: ["Ann Lee", "Bob"]
            .map(|name| Person {
                service_id: name.to_lowercase(),
                name: name.into(),
            })
            .into(),
        ..conversation("g", true)
    }]));
    state.select(&id("g"));
    assert!(state.mention_candidates("hi").is_empty());
    let names = |draft| -> Vec<String> {
        state
            .mention_candidates(draft)
            .iter()
            .map(|p| p.name.clone())
            .collect()
    };
    assert_eq!(names("hi @"), ["Ann Lee", "Bob"]);
    assert_eq!(names("hi @le"), ["Ann Lee"]);
    let draft = state.pick_mention("hi @le", 0).unwrap();
    assert_eq!(draft, "hi @Ann Lee ");
    let Some(Command::Send { body, mentions, .. }) = state.compose(&format!("{draft}*look*")).0
    else {
        panic!("sends");
    };
    assert_eq!(body, "hi @Ann Lee look");
    assert_eq!(
        mentions,
        [Mention {
            start: 3,
            end: 11,
            service_id: "ann lee".into()
        }]
    );
    // Forgotten once sent.
    assert!(
        matches!(state.compose("@Ann Lee").0, Some(Command::Send { mentions, .. }) if mentions.is_empty())
    );
}

#[test]
fn typing_is_announced_sparingly() {
    let mut state = state();
    state.select(&id("a"));
    assert!(matches!(
        state.draft_changed("h"),
        Some(Command::Typing { typing: true, .. })
    ));
    assert!(state.draft_changed("hi").is_none());
    assert!(matches!(
        state.draft_changed(""),
        Some(Command::Typing { typing: false, .. })
    ));
    assert!(state.draft_changed("").is_none());
}

#[test]
fn typing_shows_until_their_message_arrives() {
    let mut state = state();
    state.apply(Event::Typing {
        conversation: id("a"),
        sender_id: "ann".into(),
        sender: "Ann".into(),
        typing: true,
    });
    assert_eq!(state.typing(&id("a")), ["Ann"]);
    assert!(state.typing_expires().is_some());
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    assert!(state.typing(&id("a")).is_empty());
}

#[test]
fn deleting_here_removes_and_for_everyone_leaves_a_tombstone() {
    let mut state = state();
    state.select(&id("a"));
    let now = now_millis();
    for ts in [now - 2, now - 1] {
        state.apply(Event::Message(ChatMessage {
            timestamp: ts,
            ..message("a", "You", 0, true)
        }));
    }
    let (command, _) = state.delete(0, false);
    assert!(matches!(
        command,
        Some(Command::Delete {
            for_everyone: false,
            ..
        })
    ));
    assert_eq!(state.messages().len(), 1);
    let (command, _) = state.delete(0, true);
    assert!(matches!(
        command,
        Some(Command::Delete {
            for_everyone: true,
            ..
        })
    ));
    assert!(state.messages().next().unwrap().message.deleted);
    // Already deleted for everyone.
    assert!(state.delete(0, true).0.is_none());
}

#[test]
fn older_pages_prepend_and_bring_their_reactions_home() {
    let mut state = state();
    let a = id("a");
    assert!(matches!(
        state.select(&a),
        Some(Command::LoadHistory { before: None, .. })
    ));
    let old = message("a", "Ann", 10, false);
    state.apply(Event::History {
        conversation: a.clone(),
        messages: vec![message("a", "Ann", 30, false)],
        // A reaction stored with the newer page, to a message in the older one.
        reactions: vec![(old.reference(), reaction("👍", "bob", false), false)],
        older: Some(30),
    });
    assert!(state.has_older());
    assert!(matches!(
        state.load_older(),
        Some(Command::LoadHistory {
            before: Some(30),
            ..
        })
    ));
    assert!(state.load_older().is_none(), "once at a time");
    let update = state.apply(Event::History {
        conversation: a,
        messages: vec![old, message("a", "Ann", 20, false)],
        reactions: Vec::new(),
        older: None,
    });
    assert_eq!(update.messages, MessagesUpdate::Prepended(2));
    assert!(!state.has_older());
    assert!(state.load_older().is_none());
    let first = state.messages().next().unwrap().message;
    assert_eq!((first.timestamp, first.reactions.len()), (10, 1));
}

#[test]
fn forwarding_sends_a_copy_to_the_chosen_chat() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.apply(Event::Message(message("g", "Bob", 20, false)));
    state.select(&id("a"));
    assert!(state.start_forward(0));
    let titles: Vec<_> = state
        .forward_targets("")
        .iter()
        .map(|c| c.id.0.clone())
        .collect();
    assert_eq!(titles, ["g", "a"]);
    assert_eq!(state.forward_targets("G").len(), 1);
    assert_eq!(state.forwarded().as_deref(), Some("Ann at 10\nsecond line"));
    let Some(Command::Forward {
        conversation, body, ..
    }) = state.forward_to(&id("g"))
    else {
        panic!("forwards");
    };
    assert_eq!(
        (conversation, body),
        (id("g"), "Ann at 10\nsecond line".into())
    );
    assert!(!state.forwarding());
    assert!(state.forward_to(&id("g")).is_none(), "once");
    assert!(state.forwarded().is_none());
}

#[test]
fn picked_messages_are_copied_forwarded_and_deleted_together() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.apply(Event::Message(message("a", "Me", 20, true)));
    state.apply(Event::Message(message("a", "Ann", 30, false)));
    state.select(&id("a"));
    assert!(state.toggle_selected(0).rows);
    state.select_through(2);
    assert!(
        state.messages().all(|v| v.selected),
        "the range from the anchor"
    );
    state.toggle_selected(1);
    assert_eq!(
        state.selected_text(),
        "Ann at 10\nsecond line\n\nAnn at 30\nsecond line"
    );

    assert!(state.start_forward_selection());
    assert_eq!(state.forwarded().as_deref(), Some("2 messages"));
    assert!(matches!(
        state.forward_to(&id("g")),
        Some(Command::Forward { body, .. }) if body == "Ann at 10\nsecond line"
    ));
    // Among what else followed (read receipts).
    let forwards: Vec<_> = state
        .take_commands()
        .into_iter()
        .filter(|c| matches!(c, Command::Forward { .. }))
        .collect();
    assert!(matches!(
        forwards.as_slice(),
        [Command::Forward { body, .. }] if body == "Ann at 30\nsecond line"
    ));
    assert!(state.selection().is_empty(), "done with them");

    state.toggle_selected(0);
    state.toggle_selected(2);
    let (command, update) = state.delete_selection(false);
    assert!(matches!(command, Some(Command::Delete { .. })));
    let deletes = state
        .take_commands()
        .into_iter()
        .filter(|c| matches!(c, Command::Delete { .. }))
        .count();
    assert_eq!(deletes, 1, "the second goes along");
    assert_eq!(update, Update::reset());
    assert_eq!(state.messages().len(), 1);
    assert!(state.selection().is_empty());

    state.toggle_selected(0);
    assert_eq!(state.selection().len(), 1);
    state.select(&id("g"));
    assert!(state.selection().is_empty(), "picks belong to the chat");
}

/// A chat to pick messages in: theirs and ours, one of ours too old to delete for
/// everyone, a notice, one still sending and one that failed to.
fn picking_state(now: u64) -> ChatState {
    let mut state = state();
    state.set_window_focused(true);
    let mut messages: Vec<ChatMessage> = [
        ("Ann", false),
        ("Me", true),
        ("Ann", false),
        ("Me", true),
        ("Me", true),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (sender, outgoing))| message("a", sender, now - 60_000 + i as u64 * 1_000, outgoing))
    .collect();
    messages.insert(0, message("a", "Me", 1_000, true));
    messages.push(ChatMessage {
        notice: true,
        ..message("a", "Ann", now - 30_000, false)
    });
    state.select(&id("a"));
    state.apply(Event::History {
        conversation: id("a"),
        messages,
        reactions: Vec::new(),
        older: None,
    });
    for timestamp in [now - 20_000, now - 10_000] {
        let send = Command::Send {
            conversation: id("a"),
            timestamp,
            body: "hi".into(),
            quote: None,
            files: Vec::new(),
            styles: Vec::new(),
            mentions: Vec::new(),
        };
        state.show_sending(&send);
    }
    state.apply(Event::SendFailed {
        conversation: id("a"),
        timestamp: now - 10_000,
        retryable: true,
    });
    state.take_commands();
    state
}

/// Deleting the picked messages together does what deleting each in turn did.
#[test]
fn deleting_picked_messages_deletes_each() {
    let now = now_millis();
    for for_everyone in [false, true] {
        let mut each = picking_state(now);
        let mut together = picking_state(now);
        for state in [&mut each, &mut together] {
            let last = state.messages().len() - 1;
            state.toggle_selected(0);
            state.select_through(last);
            assert!(state.edit(4).is_some());
        }
        let targets: Vec<MessageRef> = each.selection().iter().map(|m| m.reference()).collect();
        assert_eq!(targets.len(), 8, "all but the notice");
        let mut commands = Vec::new();
        let mut update = Update::default();
        for target in targets {
            let index = each.messages().position(|v| v.message.is(&target)).unwrap();
            let (command, done) = each.delete(index, for_everyone);
            commands.extend(command);
            update = update.merge(done);
        }

        let (first, done) = together.delete_selection(for_everyone);
        let all: Vec<Command> = first.into_iter().chain(together.take_commands()).collect();
        assert_eq!(format!("{all:?}"), format!("{commands:?}"));
        // Deleted here, the two that didn't go out are dropped too.
        assert_eq!(all.len(), if for_everyone { 3 } else { 8 });
        assert_eq!(done, update);
        let rows = |state: &ChatState| -> Vec<(u64, String, bool)> {
            state
                .messages()
                .map(|v| {
                    (
                        v.message.timestamp,
                        v.message.sender_id.clone(),
                        v.message.deleted,
                    )
                })
                .collect()
        };
        assert_eq!(rows(&together), rows(&each));
        let sends = |state: &ChatState| -> Vec<u64> {
            let mut sends: Vec<u64> = state.pending_sends.keys().map(|(_, t)| *t).collect();
            sends.sort_unstable();
            sends
        };
        assert_eq!(sends(&together), sends(&each));
        assert_eq!(together.editing, each.editing);
        assert!(!together.has_selection());
    }
}

/// Picks of messages that left the chat since (disappeared, deleted elsewhere) don't count.
#[test]
fn picks_count_while_their_messages_are_there() {
    let mut state = picking_state(now_millis());
    let counts = |state: &ChatState| {
        let len = state.selection_len();
        assert_eq!(len, state.selection().len());
        assert_eq!(state.has_selection(), len > 0);
        len
    };
    assert_eq!(counts(&state), 0);
    assert!(state.selected_text().is_empty());
    assert!(!state.can_forward_selection());
    assert!(!state.can_delete_selection_for_everyone());
    state.toggle_selected(1);
    state.toggle_selected(2);
    assert_eq!(counts(&state), 2);
    assert!(!state.can_delete_selection_for_everyone(), "one is theirs");
    let gone = state.messages().nth(1).unwrap().message.reference();
    state.apply(Event::Expired {
        conversation: id("a"),
        targets: vec![gone],
    });
    assert_eq!(counts(&state), 1);
    assert!(state.can_delete_selection_for_everyone());
    assert_eq!(state.selected_text(), state.selection()[0].body);
    // What was the third is the second now.
    state.toggle_selected(1);
    assert_eq!(counts(&state), 0);
    assert!(state.selected_text().is_empty());
    assert!(!state.can_forward_selection());
}

#[test]
fn search_results_load_their_page_and_are_scrolled_to() {
    let mut state = state();
    let a = id("a");
    state.select(&a);
    state.apply(Event::History {
        conversation: a.clone(),
        messages: vec![message("a", "Ann", 30, false)],
        reactions: Vec::new(),
        older: Some(30),
    });
    state.take_commands();
    state.open_chat_search();
    assert!(matches!(
        state.search_chat(" moon "),
        Some(Command::SearchChat { query, .. }) if query == "moon"
    ));
    assert!(matches!(state.chat_search(), Some(("moon", None))));
    let old = message("a", "Ann", 10, false);
    state.apply(Event::SearchResults {
        conversation: a.clone(),
        query: "moon".into(),
        messages: vec![old.clone()],
    });
    assert_eq!(state.chat_search().unwrap().1.unwrap().len(), 1);
    state.show_result(0);
    assert!(state.chat_search().is_none());
    // Not loaded yet: the page before is asked for, then it is scrolled to.
    assert!(matches!(
        &state.take_commands()[..],
        [Command::LoadHistory {
            before: Some(30),
            ..
        }]
    ));
    assert_eq!(state.jump(), None);
    state.apply(Event::History {
        conversation: a,
        messages: vec![old, message("a", "Ann", 20, false)],
        reactions: Vec::new(),
        older: None,
    });
    assert_eq!(state.jump(), Some((0, 1)));
    assert!(state.messages().next().unwrap().highlighted);
}

#[test]
fn snippets_show_the_match_in_context() {
    assert_eq!(
        snippet("see\nthe moon", "MOON"),
        ("see the moon".into(), Some(8..12))
    );
    let long = format!("{} needle {}", "a".repeat(100), "b".repeat(100));
    let (shown, found) = snippet(&long, "needle");
    assert!(shown.starts_with('…') && shown.ends_with('…'));
    assert_eq!(&shown[found.unwrap()], "needle");
    assert!(shown.chars().count() < 140);
    let (shown, found) = snippet("Grüße aus München", "münchen");
    assert_eq!(&shown[found.unwrap()], "München");
    assert_eq!(
        snippet("nothing here", "moon"),
        ("nothing here".into(), None)
    );
}

#[test]
fn media_overview_sorts_attachments_and_links() {
    let mut state = state();
    state.select(&id("a"));
    assert!(matches!(state.open_media(), Some(Command::LoadMedia(i)) if i == id("a")));
    assert!(matches!(state.media(), Some(None)));
    let photo = with_attachment("a", 3, "p", AttachmentKind::Image);
    let file = with_attachment("a", 2, "f", AttachmentKind::File);
    let link = ChatMessage {
        body: "see https://example.org/x and http://b.c/d".into(),
        ..message("a", "Ann", 1, false)
    };
    let gone = ChatMessage {
        body: "https://gone.example".into(),
        deleted: true,
        ..message("a", "Ann", 5, false)
    };
    let another = ChatMessage {
        body: "and https://example.net".into(),
        ..message("a", "Bob", 4, false)
    };
    state.apply(Event::Media {
        conversation: id("a"),
        messages: vec![photo, link, gone, file, another],
    });
    let overview = state.media().unwrap().unwrap();
    assert_eq!(overview.media.len(), 1);
    assert_eq!(overview.files.len(), 1);
    let links: Vec<_> = overview
        .links
        .iter()
        .map(|(m, l)| (m.timestamp, l.as_str()))
        .collect();
    assert_eq!(
        links,
        [
            (1, "https://example.org/x"),
            (1, "http://b.c/d"),
            (4, "https://example.net")
        ]
    );
    // Older than the loaded history, yet it can be fetched.
    assert!(state.fetch_attachment(&AttachmentId("p".into())).is_some());
}

#[test]
fn moving_between_chats() {
    let mut state = state();
    state.apply(Event::Conversations(vec![conversation("b", false)]));
    for (chat, ts) in [("a", 30), ("b", 20), ("g", 10)] {
        state.apply(Event::Message(message(chat, "Ann", ts, false)));
    }
    state.select_relative(1);
    assert_eq!(state.selected().unwrap().id, id("a"));
    state.select_relative(1);
    assert_eq!(state.selected().unwrap().id, id("b"));
    assert!(state.select_relative(5).is_none());
    state.select_relative(-1);
    assert_eq!(state.selected().unwrap().id, id("a"));
    // "a" and "b" were read by opening them; "g" is still unread.
    state.select_next_unread();
    assert_eq!(state.selected().unwrap().id, id("g"));
    assert!(state.select_next_unread().is_none());
}

#[test]
fn message_requests_wait_for_an_answer() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.select(&id("a"));
    let chat = |state: &ChatState| state.conversation(&id("a")).unwrap().clone();
    // Unknown until the phone's settings were read.
    assert!(!state.is_request(&chat(&state)));
    state.apply(Event::StorageRead);
    assert!(state.is_request(&chat(&state)));
    assert!(!state.can_compose("hi"));
    state.apply(Event::Message(with_attachment(
        "a",
        11,
        "p",
        AttachmentKind::Image,
    )));
    let photo = AttachmentId("p".into());
    assert!(state.fetch_attachment(&photo).is_none(), "a request");
    assert!(matches!(
        state.respond(&id("a"), RequestResponse::Accept),
        Some(Command::Respond {
            response: RequestResponse::Accept,
            ..
        })
    ));
    assert!(!state.is_request(&chat(&state)));
    assert!(state.can_compose("hi"));
    assert!(state.fetch_attachment(&photo).is_some(), "accepted");
    // Blocked chats can't be written to; deleting clears the chat.
    state.block(&id("a"), true);
    assert!(!state.can_compose("hi"));
    state.apply(Event::Cleared(id("a")));
    assert!(state.selected().is_none());
    assert_eq!(listed(&state), ["g"]);
}

/// The backend lists chats again with what its table had (a contact saved, a group changed),
/// which can be read before an answer to a request is written there: the answer stays, and
/// only the backend's own settings changes (`Event::Settings`) change them.
#[test]
fn listing_chats_again_keeps_an_answer() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.apply(Event::StorageRead);
    let chat = |state: &ChatState| state.conversation(&id("a")).unwrap().clone();
    assert!(state.is_request(&chat(&state)));
    state.respond(&id("a"), RequestResponse::Accept);
    // Read from the table before the answer was written there.
    state.apply(Event::Conversations(vec![conversation("a", false)]));
    assert!(!state.is_request(&chat(&state)), "the answer taken back");
    state.select(&id("a"));
    assert!(state.can_compose("hi"));
    // What the backend changes itself still shows.
    state.apply(Event::Settings {
        conversation: id("a"),
        settings: ChatSettings {
            accepted: true,
            archived: true,
            ..Default::default()
        },
    });
    assert!(chat(&state).settings.archived);
    // A chat new here takes the settings it comes with.
    let mut new = conversation("n", false);
    new.settings.accepted = true;
    state.apply(Event::Conversations(vec![new]));
    assert!(state.conversation(&id("n")).unwrap().settings.accepted);
}

#[test]
fn folders_show_their_chats() {
    let mut state = state();
    state.apply(Event::Conversations(vec![conversation("b", false)]));
    for (chat, ts) in [("a", 30), ("b", 20), ("g", 10)] {
        state.apply(Event::Message(message(chat, "Ann", ts, false)));
    }
    state.apply(Event::Folders(vec![
        crate::ChatFolder {
            name: "Friends".into(),
            included: vec![id("a")],
            all_groups: true,
            ..Default::default()
        },
        crate::ChatFolder {
            name: "Unread people".into(),
            all_individual: true,
            excluded: vec![id("b")],
            only_unread: true,
            show_muted: true,
            ..Default::default()
        },
    ]));
    state.show_folder(Some(0));
    assert_eq!(listed(&state), ["a", "g"]);
    assert_eq!(state.folder_unread(0), 2);
    state.show_folder(Some(1));
    assert_eq!(listed(&state), ["a"]);
    state.select(&id("a"));
    // Read now, yet the open chat stays listed.
    assert_eq!(listed(&state), ["a"]);
    state.show_folder(None);
    assert_eq!(listed(&state), ["a", "b", "g"]);
    // Counts are of listed chats only: not archived ones, nor muted ones where the folder
    // hides them, nor chats that aren't there.
    state.apply(Event::Unread(vec![(id("b"), 4), (id("nope"), 7)]));
    state.apply(Event::Message(message("g", "Ann", 40, false)));
    assert_eq!(state.folder_unread(0), 2);
    assert_eq!(state.folder_unread(1), 0, "b is excluded, a is read");
    state.apply(Event::Folders(vec![crate::ChatFolder {
        name: "Everyone".into(),
        all_individual: true,
        all_groups: true,
        ..Default::default()
    }]));
    assert_eq!(state.folder_unread(0), 6);
    state.mute(&id("g"), Some(u64::MAX));
    assert_eq!(state.folder_unread(0), 4);
    state.archive(&id("b"), true);
    assert_eq!(state.folder_unread(0), 0);
    assert_eq!(state.folder_unread(1), 0, "no such folder");
    // A folder that went away on the phone isn't shown any more.
    state.show_folder(Some(0));
    state.apply(Event::Folders(Vec::new()));
    assert_eq!(state.folder(), None);
}

/// Someone's new picture shows beside what they wrote in the open group at once.
#[test]
fn senders_pictures_are_redrawn_when_they_change() {
    let mut state = state();
    state.apply(Event::Message(message("g", "Bob", 10, false)));
    state.apply(Event::Message(message("a", "Ann", 20, false)));
    let picture = |of: &str| Event::Avatar {
        conversation: ConversationId::contact(of),
        avatar: Some(Arc::new(Picture {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        })),
    };
    state.select(&id("g"));
    let update = state.apply(picture("bob"));
    assert!(update.rows && update.conversations);
    assert!(state.messages().next().unwrap().sender_avatar.is_some());
    assert!(!state.apply(picture("ann")).rows, "not in this chat");
    let gone = state.apply(Event::Avatar {
        conversation: ConversationId::contact("bob"),
        avatar: None,
    });
    assert!(gone.rows);
    assert!(state.messages().next().unwrap().sender_avatar.is_none());
    // Pictures beside messages are a group's.
    state.select(&id("a"));
    assert!(!state.apply(picture("ann")).rows);
    assert!(!state.apply(picture("bob")).rows);
}

/// Messages and typing name their chat for the list, so that the UI redraws just its row;
/// what changes more names none, and every row is redrawn.
#[test]
fn changed_chats_are_named_for_their_rows() {
    let mut state = state();
    assert_eq!(state.take_changed_chats(), None, "none noted");
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.apply(Event::Message(message("g", "Bob", 20, false)));
    state.apply(Event::Message(message("a", "Ann", 30, false)));
    assert_eq!(state.take_changed_chats(), Some(vec![id("a"), id("g")]));
    assert_eq!(state.take_changed_chats(), None, "taken");

    state.select(&id("a"));
    let typing = Event::Typing {
        conversation: id("g"),
        sender_id: "bob".into(),
        sender: "Bob".into(),
        typing: true,
    };
    state.apply(typing);
    assert_eq!(
        state.take_changed_chats(),
        Some(vec![id("g"), id("a")]),
        "and the open one, which reading changes"
    );

    state.apply(Event::Message(message("g", "Bob", 40, false)));
    state.apply(Event::Folders(Vec::new()));
    assert_eq!(state.take_changed_chats(), None, "folders change every row");

    let listed: Vec<_> = state
        .conversations()
        .map(|c| c.conversation.id.clone())
        .collect();
    assert_eq!(state.listed_ids().cloned().collect::<Vec<_>>(), listed);
    let view = state.conversation_view(&id("g")).expect("known");
    assert_eq!(view.last_message.map(|m| m.timestamp), Some(40));
}

fn ann(timestamp: u64) -> MessageRef {
    MessageRef {
        author_id: "ann".into(),
        timestamp,
    }
}

/// What changes in place names its messages of the open chat, once, so that the UI
/// redraws just their rows; those of other chats aren't shown.
#[test]
fn changed_messages_are_named_for_their_rows() {
    let mut state = state();
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.apply(Event::Message(message("a", "Ann", 20, false)));
    let photo = AttachmentId("photo".into());
    state.apply(Event::Message(ChatMessage {
        sender_id: "ann".into(),
        ..with_attachment("a", 30, "photo", AttachmentKind::Image)
    }));
    state.apply(Event::Message(message("g", "Bob", 5, false)));
    state.select(&id("a"));
    history_arrives(&mut state, "a");
    assert_eq!(state.take_changed_rows(), None, "none noted");
    assert_eq!(state.row_of(&ann(20)), Some(1));
    assert_eq!(state.row_of(&ann(15)), None);

    let update = state.apply(Event::Reaction {
        conversation: id("a"),
        target: ann(20),
        reaction: reaction("👍", "bob", false),
        remove: false,
    });
    assert!(update.rows);
    assert_eq!(state.take_changed_rows(), Some(vec![ann(20)]));
    assert_eq!(state.take_changed_rows(), None, "taken");

    let bob = ChatMessage::reference(&message("g", "Bob", 5, false));
    let update = state.apply(Event::Deleted {
        conversation: id("g"),
        target: bob,
    });
    assert!(!update.rows);
    assert_eq!(state.take_changed_rows(), None, "not shown");

    // Also what the UI asks for: a download, picks, spoilers, a search result.
    assert!(state.fetch_attachment(&photo).is_some());
    state.apply(Event::Attachment {
        id: photo.clone(),
        content: Err(crate::Error::unreachable("gone")),
    });
    state.toggle_selected(0);
    state.select_through(1);
    state.reveal(1);
    assert_eq!(
        state.take_changed_rows(),
        Some(vec![ann(30), ann(30), ann(10), ann(20), ann(20)])
    );
    state.open_chat_search();
    state.search_chat("Ann");
    state.apply(Event::SearchResults {
        conversation: id("a"),
        query: "Ann".into(),
        messages: vec![
            message("a", "Ann", 30, false),
            message("a", "Ann", 10, false),
        ],
    });
    state.show_result(0);
    state.open_chat_search();
    state.search_chat("Ann");
    state.apply(Event::SearchResults {
        conversation: id("a"),
        query: "Ann".into(),
        messages: vec![message("a", "Ann", 10, false)],
    });
    state.show_result(0);
    assert_eq!(
        state.take_changed_rows(),
        Some(vec![ann(30), ann(30), ann(10)]),
        "the result marked, then the mark moved"
    );

    // Ours, while it goes out.
    let timestamp = sent_at(state.compose("hi").0);
    let unsent = MessageRef {
        author_id: String::new(),
        timestamp,
    };
    state.apply(Event::SendFailed {
        conversation: id("a"),
        timestamp,
        retryable: true,
    });
    assert!(state.retry_send(3).is_some());
    state.apply(Event::Message(ChatMessage {
        sender_id: "me".into(),
        ..message("a", "You", timestamp, true)
    }));
    let sent = MessageRef {
        author_id: "me".into(),
        timestamp,
    };
    assert_eq!(state.row_of(&sent), Some(3));
    assert_eq!(
        state.take_changed_rows(),
        Some(vec![unsent.clone(), unsent, sent])
    );
}

/// What can't be named, or is too much to, redraws every row; so does nothing noted. Made
/// anew, the rows have nothing left to redraw.
#[test]
fn changed_rows_are_all_or_forgotten() {
    let mut state = state();
    for timestamp in [10, 20, 30] {
        state.apply(Event::Message(message("g", "Ann", timestamp, false)));
    }
    state.select(&id("g"));
    history_arrives(&mut state, "g");
    state.toggle_selected(0);
    assert!(state.clear_selection().rows);
    assert_eq!(state.take_changed_rows(), None, "every pick is gone");
    state.reveal(0);
    let update = state.apply(Event::Avatar {
        conversation: ConversationId::contact("ann"),
        avatar: None,
    });
    assert!(update.rows);
    assert_eq!(state.take_changed_rows(), None, "beside each of hers");
    for _ in 0..=CHANGED_ROWS_NAMED {
        state.reveal(0);
    }
    assert_eq!(state.take_changed_rows(), None, "too many");
    state.reveal(0);
    assert!(state.take_changed_rows().is_some(), "named again");

    state.reveal(0);
    state.select(&id("a"));
    state.select(&id("g"));
    assert_eq!(state.take_changed_rows(), None, "another chat was shown");
    state.reveal(0);
    state.deselect();
    assert_eq!(state.take_changed_rows(), None);
    state.select(&id("g"));
    state.reveal(0);
    let update = state.apply(Event::Message(message("g", "Ann", 15, false)));
    assert_eq!(update.messages, MessagesUpdate::Reset);
    assert_eq!(state.take_changed_rows(), None, "made anew");
}

#[test]
fn formatters_say_what_one_off_formatting_says() {
    let (list_time, time) = (list_time_formatter(), time_formatter());
    let now = now_millis();
    for t in [
        0,
        86_400_000,
        now - 400 * 86_400_000,
        now - 3 * 86_400_000,
        now,
        u64::MAX,
    ] {
        assert_eq!(list_time(t), format_list_time(t), "{t}");
        assert_eq!(time(t), format_time(t), "{t}");
    }
}

#[test]
fn strangers_asking_to_write_are_not_offered_for_groups() {
    let mut state = state();
    let mut ann = conversation("c:ann", false);
    ann.settings.accepted = true;
    state.apply(Event::Conversations(vec![
        ann,
        conversation("c:stranger", false),
    ]));
    state.apply(Event::StorageRead);
    let names: Vec<_> = state
        .group_candidates("")
        .iter()
        .map(|c| c.id.0.clone())
        .collect();
    assert_eq!(names, ["c:ann"]);
}

#[test]
fn each_chat_keeps_its_draft() {
    let mut state = state();
    let sent = ChatMessage {
        timestamp: now_millis(),
        ..message("a", "Me", 0, true)
    };
    state.apply(Event::Message(sent));
    state.apply(Event::Message(message("g", "Bob", 20, false)));
    state.select(&id("a"));
    state.draft_changed("for Ann");
    state.select(&id("g"));
    assert_eq!(state.draft(), "", "not Ann's");
    state.draft_changed("for the group");
    state.select(&id("a"));
    assert_eq!(state.draft(), "for Ann");
    // An edit is written over it, and leaves it be.
    assert!(state.edit_last().is_some());
    state.draft_changed("corrected");
    state.cancel_edit();
    assert_eq!(state.draft(), "for Ann");
    // Sent, it is gone.
    assert!(state.compose("for Ann").0.is_some());
    assert_eq!(state.draft(), "");
    state.select(&id("g"));
    assert_eq!(state.draft(), "for the group");
}

#[test]
fn forwarding_offers_only_chats_it_can_go_to() {
    let mut state = state();
    let mut ann = conversation("c:ann", false);
    ann.settings.accepted = true;
    let mut bob = conversation("c:bob", false);
    bob.settings.accepted = true;
    bob.settings.blocked = true;
    state.apply(Event::Conversations(vec![
        ann,
        bob,
        conversation("c:stranger", false),
    ]));
    state.apply(Event::StorageRead);
    for chat in ["c:ann", "c:bob", "c:stranger"] {
        state.apply(Event::Message(message(chat, "Someone", 10, false)));
    }
    let ids: Vec<_> = state
        .forward_targets("")
        .iter()
        .map(|c| c.id.0.clone())
        .collect();
    assert!(ids.iter().any(|id| id == "c:ann"));
    assert!(!ids.iter().any(|id| id == "c:bob" || id == "c:stranger"));
}

#[test]
fn groups_are_made_and_changed() {
    let mut state = state();
    state.apply(Event::Conversations(vec![
        conversation("c:ann", false),
        conversation("c:bob", false),
    ]));
    let names: Vec<_> = state
        .group_candidates("")
        .iter()
        .map(|c| c.id.0.clone())
        .collect();
    assert_eq!(names, ["a", "c:ann", "c:bob"]);
    assert!(
        state.create_group("  ", &[id("c:ann")]).is_none(),
        "needs a name"
    );
    assert!(matches!(
        state.create_group("Climbing", &[id("c:ann")]),
        Some(Command::CreateGroup { title, members }) if title == "Climbing" && members == ["ann"]
    ));
    // The new group opens.
    state.apply(Event::Conversations(vec![conversation("g:new", true)]));
    state.apply(Event::GroupCreated(id("g:new")));
    assert_eq!(state.selected().unwrap().id, id("g:new"));
    assert!(
        state
            .edit_group(&id("g:new"), crate::GroupEdit::Title("g:new".into()))
            .is_none()
    );
    assert!(
        state
            .edit_group(&id("g:new"), crate::GroupEdit::Leave)
            .is_some()
    );
    // Once left, read only.
    state.apply(Event::Conversations(vec![Conversation {
        left: true,
        ..conversation("g:new", true)
    }]));
    assert!(!state.can_compose("hi"));
    assert!(
        state
            .edit_group(&id("g:new"), crate::GroupEdit::Leave)
            .is_none()
    );
}

#[test]
fn a_group_is_joined_through_its_link() {
    let mut state = state();
    let link = "https://signal.group/#abc";
    assert!(matches!(
        state.open_join(Some(" https://signal.group/#abc ")),
        Some(Command::PreviewGroupLink(typed)) if typed == link
    ));
    let (typed, preview, busy) = state.join().unwrap();
    assert_eq!((typed, preview, busy), (link, None, true));
    assert!(state.join_group().is_none(), "nothing looked up yet");
    // A failed lookup leaves the dialog open for another try.
    state.apply(Event::Error(crate::Error::refused("no such group")));
    assert!(!state.join().unwrap().2);
    let preview = crate::GroupPreview {
        link: link.into(),
        title: "Climbing".into(),
        members: 3,
        approval: true,
        requested: false,
    };
    state.apply(Event::GroupPreview(crate::GroupPreview {
        link: "https://signal.group/#other".into(),
        ..preview.clone()
    }));
    assert_eq!(state.join().unwrap().1, None, "an answer for another link");
    state.apply(Event::GroupPreview(preview.clone()));
    assert_eq!(state.join().unwrap().1, Some(&preview));
    assert!(matches!(state.join_group(), Some(Command::JoinGroup(typed)) if typed == link));
    assert!(state.join_group().is_none(), "already joining");
    state.apply(Event::Conversations(vec![Conversation {
        requested: true,
        left: true,
        ..conversation("g:climbing", true)
    }]));
    state.apply(Event::GroupJoined(id("g:climbing")));
    assert!(state.join().is_none(), "done");
    assert_eq!(state.selected().unwrap().id, id("g:climbing"));
    // Waiting for an admin: the request can be taken back, nothing else.
    assert!(
        state
            .edit_group(&id("g:climbing"), crate::GroupEdit::CancelJoinRequest)
            .is_some()
    );
    assert!(
        state
            .edit_group(&id("g:climbing"), crate::GroupEdit::Leave)
            .is_none()
    );
}

#[test]
fn people_are_chosen_for_a_group() {
    let mut state = state();
    state.apply(Event::Conversations(vec![
        conversation("c:ann", false),
        conversation("c:bob", false),
        Conversation {
            members: vec![Person {
                service_id: "ann".into(),
                name: "Ann".into(),
            }],
            ..conversation("g:club", true)
        },
    ]));
    state.open_group_draft(None);
    state.toggle_group_member(&id("c:ann"));
    state.toggle_group_member(&id("c:bob"));
    state.toggle_group_member(&id("c:bob"));
    assert!(matches!(state.group_draft(), Some((true, [chosen])) if *chosen == id("c:ann")));
    assert!(matches!(
        state.finish_group_draft("Club"),
        Some(Command::CreateGroup { members, .. }) if members == ["ann"]
    ));
    assert!(state.group_draft().is_none());
    // Adding to a group: its members aren't offered again.
    state.open_group_draft(Some(id("g:club")));
    let offered: Vec<_> = state
        .group_candidates("")
        .iter()
        .map(|c| c.id.0.clone())
        .collect();
    assert_eq!(offered, ["a", "c:bob"]);
    state.toggle_group_member(&id("c:bob"));
    assert!(matches!(
        state.finish_group_draft(""),
        Some(Command::EditGroup { edit: crate::GroupEdit::AddMembers(m), .. }) if m == ["bob"]
    ));
}

#[test]
fn stories_are_watched_in_order() {
    let mut state = state();
    let story = |who: &str, chat: &str, ts: u64, viewed| crate::Story {
        author_id: who.into(),
        author: who.into(),
        conversation: id(chat),
        timestamp: ts,
        content: crate::StoryContent::Text {
            text: format!("{who} {ts}"),
            foreground: 0,
            background: 0,
        },
        viewed,
        outgoing: false,
    };
    state.apply(Event::Stories(vec![
        story("ann", "a", 10, true),
        story("ann", "a", 20, false),
        story("bob", "g", 30, true),
        // A stranger's: no chat with them, so not listed (watching would tell them).
        story("eve", "c:eve", 25, false),
    ]));
    assert!(state.open_story(&id("c:eve")).is_none());
    let rings: Vec<_> = state
        .story_rings()
        .iter()
        .map(|r| (r.conversation.0.clone(), r.unviewed))
        .collect();
    assert_eq!(
        rings,
        [("a".into(), true), ("g".into(), false)],
        "unseen first"
    );
    assert_eq!(state.unviewed_stories(), 1);
    // Opens at the first unseen, and marks it seen.
    assert!(matches!(state.open_story(&id("a")), Some(Command::ViewStory(r)) if r.timestamp == 20));
    assert_eq!(
        state.current_story().map(|(_, at, count)| (at, count)),
        Some((1, 2))
    );
    assert_eq!(state.unviewed_stories(), 0);
    assert!(state.step_story(-1).is_none(), "already seen");
    assert_eq!(state.current_story().unwrap().1, 0);
    state.step_story(1);
    state.step_story(1);
    assert!(state.current_story().is_none(), "closes after the last");
    assert!(
        state
            .send_story(&id("g"), crate::NewStory::Text("hi".into()))
            .is_some()
    );
    assert!(
        state
            .send_story(&id("a"), crate::NewStory::Text("hi".into()))
            .is_none(),
        "groups only"
    );
    // When an older story expires, the one shown stays shown.
    state.open_story(&id("a"));
    state.step_story(1);
    assert_eq!(state.current_story().unwrap().0.timestamp, 20);
    state.apply(Event::Stories(vec![
        story("ann", "a", 20, true),
        story("bob", "g", 30, true),
    ]));
    assert_eq!(
        state
            .current_story()
            .map(|(s, at, count)| (s.timestamp, at, count)),
        Some((20, 0, 1))
    );
    // Once it expires too, the viewer closes.
    state.apply(Event::Stories(vec![story("bob", "g", 30, true)]));
    assert!(state.current_story().is_none());
}

/// Blocking shows at once, while the backend takes the block in turn, after the commands ahead
/// of it: a message it receives in between still comes, and isn't shown (pending/tour.md,
/// finding 5). What we send from another device still is, as the backend has it.
#[test]
fn a_message_after_blocking_isnt_shown() {
    let mut state = state();
    state.select(&id("a"));
    state.apply(Event::Message(message("a", "Ann", 10, false)));
    state.block(&id("a"), true);
    state.apply(Event::Message(message("a", "Ann", 20, false)));
    state.apply(Event::Message(message("a", "Me", 30, true)));
    let stamps =
        |state: &ChatState| -> Vec<u64> { state.messages().map(|m| m.message.timestamp).collect() };
    assert_eq!(stamps(&state), [10, 30]);
    state.block(&id("a"), false);
    state.apply(Event::Message(message("a", "Ann", 40, false)));
    assert_eq!(stamps(&state), [10, 30, 40]);
}

/// A group's open details are read again when a change to it comes (its notice), so that a
/// member leaving shows without opening them again (pending/tour.md, finding 3).
#[test]
fn a_change_to_a_group_reloads_its_open_details() {
    let reloads = |state: &mut ChatState| {
        state
            .take_commands()
            .into_iter()
            .filter(|command| matches!(command, Command::LoadDetails(_)))
            .count()
    };
    let mut state = state();
    state.select(&id("g"));
    assert!(matches!(
        state.open_details(),
        Some(Command::LoadDetails(_))
    ));
    let left = ChatMessage {
        notice: true,
        body: "Ann left the group.".into(),
        ..message("g", "Ann", 10, false)
    };
    state.apply(Event::Message(left.clone()));
    assert_eq!(reloads(&mut state), 1);
    // Not for what someone wrote.
    state.apply(Event::Message(message("g", "Ann", 20, false)));
    assert_eq!(reloads(&mut state), 0);
    // Nor once the details are closed.
    state.close_details();
    state.apply(Event::Message(ChatMessage {
        timestamp: 30,
        ..left
    }));
    assert_eq!(reloads(&mut state), 0);
}
