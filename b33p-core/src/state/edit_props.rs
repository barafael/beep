use super::*;
use crate::{ConversationKind, Person, Style};
use proptest::prelude::*;

fn group_state() -> ChatState {
    let mut chat = ChatState::default();
    chat.apply(Event::Linked {
        account: "me".into(),
    });
    chat.apply(Event::Synced);
    chat.apply(Event::Conversations(vec![Conversation {
        id: ConversationId("g".into()),
        title: "Group".into(),
        kind: ConversationKind::Group,
        members: vec![Person {
            service_id: "ann-id".into(),
            name: "Ann".into(),
        }],
        settings: ChatSettings {
            accepted: true,
            ..Default::default()
        },
        ..Default::default()
    }]));
    chat.select(&ConversationId("g".into()));
    chat.apply(Event::History {
        conversation: ConversationId("g".into()),
        messages: Vec::new(),
        reactions: Vec::new(),
        older: None,
    });
    chat
}

/// Sends `typed` (picking `@Ann` from the candidates first if it mentions her), lets the
/// sent message arrive, then edits it without changing anything: returns what was sent and
/// what the edit sends.
/// What went out: the text, its formatting and how many it mentions.
type Sent = (String, Vec<StyleRange>, usize);

/// What a message typed as `typed` sends, and then an unchanged edit of it.
fn send_then_edit(typed: &str) -> (Sent, Sent) {
    let mut chat = group_state();
    if typed.contains("@Ann") {
        // As the composer does: "@A" typed, then the candidate picked.
        chat.pick_mention("@A", 0).expect("Ann is a candidate");
    }
    let (command, _) = chat.compose(typed);
    let Some(Command::Send {
        body,
        styles,
        mentions,
        timestamp,
        conversation,
        ..
    }) = command
    else {
        panic!("not sent: {typed:?}");
    };
    chat.apply(Event::Message(ChatMessage {
        conversation,
        sender_id: "me".into(),
        sender: "You".into(),
        body: body.clone(),
        styles: styles.clone(),
        // As the backend reads the message back: with whom it mentions.
        mentions: mentions.clone(),
        timestamp,
        outgoing: true,
        ..Default::default()
    }));
    let index = chat.messages().len() - 1;
    let text = chat.edit(index).expect("ours, and recent");
    let (command, _) = chat.compose(&text);
    let Some(Command::Edit {
        body: edited,
        styles: edited_styles,
        mentions: edited_mentions,
        ..
    }) = command
    else {
        panic!("not an edit");
    };
    (
        (body, styles, mentions.len()),
        (edited, edited_styles, edited_mentions.len()),
    )
}

#[test]
fn editing_without_changes_keeps_formatting_and_mentions() {
    for typed in [
        "hi @Ann",
        "*bold* and ||the butler did it||",
        "`code` ~gone~ _it_",
    ] {
        let (sent, edit) = send_then_edit(typed);
        println!("{typed:?}: sent {sent:?}\n  unchanged edit sends {edit:?}");
        assert_eq!(edit, sent, "{typed:?}");
    }
}

fn spoiler_message(body: &str, secret: std::ops::Range<usize>) -> ChatMessage {
    ChatMessage {
        conversation: ConversationId("g".into()),
        sender_id: "ann-id".into(),
        sender: "Ann".into(),
        body: body.into(),
        styles: vec![StyleRange {
            start: secret.start,
            end: secret.end,
            style: Style::Spoiler,
        }],
        timestamp: now_millis() - 1000,
        ..Default::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, failure_persistence: None, ..ProptestConfig::default() })]

    /// A spoiler stays hidden in what is shown around its message: the chat list's preview,
    /// the notification, and the quote of a reply.
    #[test]
    fn spoilers_stay_hidden_around_the_message(before in "[a-z ]{0,8}", secret in "[A-Z]{4,8}", after in "[a-z ]{0,8}") {
        let body = format!("{before}{secret}{after}");
        let at = before.len()..before.len() + secret.len();
        let mut chat = group_state();
        chat.set_notifications(true, crate::settings::NotificationContent::NameAndMessage);
        chat.set_window_focused(false);
        chat.take_notices();
        chat.apply(Event::Message(spoiler_message(&body, at)));
        let preview = chat.conversations().next().unwrap().preview();
        let notices = chat.take_notices();
        let last = chat.messages().len() - 1;
        chat.reply_to(last);
        let quote = chat.replying().map(|q| q.text.clone()).unwrap_or_default();
        let leaks: Vec<String> = [
            ("chat list preview", preview.clone()),
            ("notification", format!("{notices:?}")),
            ("reply quote", quote.clone()),
        ]
        .into_iter()
        .filter(|(_, shown)| shown.contains(&secret))
        .map(|(what, shown)| format!("{what}: {shown}"))
        .collect();
        prop_assert!(leaks.is_empty(), "spoiler {secret:?} of {body:?} shown in {leaks:#?}");
    }
}
