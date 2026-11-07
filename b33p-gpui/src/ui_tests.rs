//! The window's own behaviour, on GPUI's test platform: what the views do with the state they
//! are given and the input they get. The same checks as `b33p-slint/src/ui_tests.rs`, where
//! Slint has a behaviour of its own and GPUI the same one.

use b33p_core::state::{ChatState, Screen, Update};
use b33p_core::{
    ChatMessage, ChatSettings, Command, Conversation, ConversationId, ConversationKind, Event,
    Failure, Person, Trust, Unsent,
};
use gpui::TestAppContext;

use crate::testing::{self, Ui};

/// The state of a chat open with Ann, with `messages` in it (oldest first), as the harness's
/// `Select` and history leave it.
fn with_messages(messages: Vec<ChatMessage>) -> ChatState {
    let mut chat = testing::chats(&["Ann"]);
    let ann = ConversationId::contact("ann");
    chat.select(&ann);
    chat.apply(Event::History {
        conversation: ann,
        messages,
        reactions: Vec::new(),
        older: None,
    });
    chat
}

fn ours(body: &str, timestamp: u64, unsent: Option<Unsent>) -> ChatMessage {
    ChatMessage {
        conversation: ConversationId::contact("ann"),
        sender_id: "me".into(),
        sender: "You".into(),
        body: body.into(),
        timestamp,
        outgoing: true,
        unsent,
        ..ChatMessage::default()
    }
}

/// A failure has its own screen, with what can be done about it; deleting the data asks first.
#[gpui::test]
async fn a_failure_offers_a_way_out(tc: &mut TestAppContext) {
    let mut chat = ChatState::default();
    chat.apply(Event::Failed {
        failure: Failure::KeyMissing,
        message: "The key is missing.".into(),
    });
    let mut ui = Ui::new(tc, chat);
    assert!(ui.shown("Try again"));
    assert!(
        !ui.shown("Use a password instead"),
        "only for a locked keyring"
    );
    assert!(!ui.shown("Confirm"), "nothing is asked yet");

    ui.click("Start over…");
    assert!(ui.shown("Confirm"), "it asks first");
    let failed = |ui: &Ui| {
        matches!(
            ui.booted.shared.chat.borrow().screen(),
            Screen::Failed { .. }
        )
    };
    assert!(failed(&ui), "not before it is confirmed");
    ui.click("Confirm");
    assert!(!failed(&ui), "started over once confirmed");
    assert!(!ui.shown("Confirm"));
}

/// How far our messages got, and that every one that failed says so, not only a run's last.
#[gpui::test]
async fn the_window_shows_the_chat_state(tc: &mut TestAppContext) {
    let now = b33p_core::now_millis();
    let chat = with_messages(vec![
        ours("first try", now - 3000, Some(Unsent::Failed)),
        ours("second try", now - 2000, Some(Unsent::Failed)),
        ours("went out", now - 1000, None),
    ]);
    let mut ui = Ui::new(tc, chat);
    // Both failed ones, not only a run's last, offer to be sent again.
    assert!(ui.shown("Send again 0"), "the first failed message says so");
    assert!(ui.shown("Send again 1"), "and so does the second");
    assert!(!ui.shown("Not sent 2") && !ui.shown("Send again 2"));
}

/// The state with `conversation` listed and open, and nothing said in it.
fn open_with(conversation: Conversation) -> ChatState {
    let id = conversation.id.clone();
    let mut chat = testing::chats(&[]);
    chat.apply(Event::Conversations(vec![conversation]));
    chat.select(&id);
    chat
}

fn ann(settings: impl FnOnce(&mut Conversation)) -> Conversation {
    let mut conversation = Conversation {
        id: ConversationId::contact("ann"),
        title: "Ann".into(),
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    };
    settings(&mut conversation);
    conversation
}

/// A text story's box takes its size from the viewer, and clicking through it works.
#[gpui::test]
async fn a_text_story_can_be_clicked_through(tc: &mut TestAppContext) {
    let mut chat = testing::chats(&["Ann"]);
    chat.apply(Event::Stories(vec![b33p_core::Story {
        author_id: "ann".into(),
        author: "Ann".into(),
        conversation: ConversationId::contact("ann"),
        timestamp: 10,
        content: b33p_core::StoryContent::Text {
            text: "cargo build --release".into(),
            foreground: 0xffff_ffff,
            background: 0xff2b_8a60,
        },
        viewed: false,
        outgoing: false,
    }]));
    chat.open_story(&ConversationId::contact("ann"));
    let mut ui = Ui::new(tc, chat);
    ui.booted.shared.ui.borrow_mut().stories_open = true;
    ui.settle();
    assert!(ui.booted.shared.chat.borrow().current_story().is_some());
    ui.click("Next story");
    assert!(
        ui.booted.shared.chat.borrow().current_story().is_none(),
        "past the last, the viewer closes"
    );
}

/// A message that failed to go out offers to be sent again, from its bubble; one Signal
/// refused doesn't, since sending it again would be refused too.
#[gpui::test]
async fn a_failed_message_offers_a_retry(tc: &mut TestAppContext) {
    let mut chat = open_with(ann(|_| {}));
    let mut fail = |chat: &mut ChatState, text: &str, retryable: bool| {
        let Some(Command::Send { timestamp, .. }) = chat.compose(text).0 else {
            panic!("{text:?} is sent");
        };
        chat.apply(Event::SendFailed {
            conversation: ConversationId::contact("ann"),
            timestamp,
            retryable,
        });
    };
    fail(&mut chat, "never went out", true);
    fail(&mut chat, "refused", false);
    let mut ui = Ui::new(tc, chat);
    assert!(ui.shown("Send again 0"), "the failed message offers it");
    assert!(!ui.shown("Send again 1"), "the refused one does not");
    assert!(ui.shown("Not sent 1"), "but says it is not sent");
    ui.click("Send again 0");
    assert!(
        ui.sent()
            .iter()
            .any(|command| matches!(command, Command::Send { .. })),
        "it goes out again"
    );
    assert!(!ui.shown("Send again 0"), "and is sending");
}

/// The details page offers to mark the safety number verified, or to clear that, and says when
/// it is.
#[gpui::test]
async fn the_safety_number_can_be_marked_verified(tc: &mut TestAppContext) {
    let mut chat = open_with(ann(|_| {}));
    chat.open_details();
    chat.apply(Event::Details {
        conversation: ConversationId::contact("ann"),
        details: b33p_core::Details {
            safety_number: Some("12345 67890".into()),
            ..Default::default()
        },
    });
    // Tall enough for the whole details page.
    let mut ui = Ui::sized(tc, chat, 800., 1200.);
    assert!(ui.shown("Copy"), "the safety number's buttons show");
    assert!(!ui.shown("Clear verification"));
    ui.click("Mark as verified");
    assert!(
        ui.sent()
            .iter()
            .any(|command| matches!(command, Command::SetVerified { verified: true, .. })),
        "asked to verify"
    );
    ui.act(|chat| {
        chat.apply(Event::Details {
            conversation: ConversationId::contact("ann"),
            details: b33p_core::Details {
                safety_number: Some("12345 67890".into()),
                trust: Trust::Verified,
                ..Default::default()
            },
        })
    });
    assert!(!ui.shown("Mark as verified"));
    ui.click("Clear verification");
    assert!(
        ui.sent().iter().any(|command| matches!(
            command,
            Command::SetVerified {
                verified: false,
                ..
            }
        )),
        "asked to clear it"
    );
}

/// A chat whose verified safety number changed says so where the composer was, and offers to
/// continue (confirming it) or to see the safety number.
#[gpui::test]
async fn a_changed_safety_number_holds_the_composer(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, open_with(ann(|c| c.trust = Trust::Changed)));
    assert!(ui.shown("Show safety number"));
    assert!(!ui.shown("Message input"), "no field to write in");
    ui.click("Show safety number");
    assert!(
        ui.booted.shared.chat.borrow().details().is_some(),
        "the details page opens"
    );
    ui.click("Continue");
    assert!(
        ui.sent().iter().any(|command| matches!(
            command,
            Command::SetVerified {
                verified: false,
                ..
            }
        )),
        "continuing accepts the new number"
    );
}

/// A group we're invited to says so where the composer would be, and answers the invitation.
#[gpui::test]
async fn an_invitation_is_answered_in_the_chat(tc: &mut TestAppContext) {
    let club = Conversation {
        id: ConversationId("club".into()),
        title: "Book club".into(),
        kind: ConversationKind::Group,
        left: true,
        invited: true,
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    };
    let mut ui = Ui::new(tc, open_with(club));
    ui.click("Decline");
    ui.click("Accept");
    let answers: Vec<_> = ui
        .sent()
        .into_iter()
        .filter_map(|command| match command {
            Command::EditGroup { edit, .. } => Some(edit),
            _ => None,
        })
        .collect();
    assert_eq!(
        answers,
        [
            b33p_core::GroupEdit::DeclineInvitation,
            b33p_core::GroupEdit::AcceptInvitation
        ]
    );
}

/// A phone number typed in the chat search offers to message it, which looks it up.
#[gpui::test]
async fn a_typed_number_can_be_messaged(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, testing::chats(&["Ann"]));
    assert!(!ui.shown("Message +49 151 23456789"));
    ui.focus_search();
    ui.type_keys("+49 151 23456789");
    ui.click("Message +49 151 23456789");
    assert!(
        ui.sent()
            .iter()
            .any(|command| matches!(command, Command::FindNumber { .. })),
        "the number is looked up"
    );
}

/// Incoming messages "Message 0" to "Message {count - 1}" in a chat open with Ann, with older
/// ones still to load if `older`.
fn many(count: usize, older: Option<u64>) -> ChatState {
    let mut chat = testing::chats(&["Ann"]);
    let ann = ConversationId::contact("ann");
    chat.select(&ann);
    chat.apply(Event::History {
        conversation: ann,
        messages: (0..count).map(|n| theirs(&format!("Message {n}"), n as u64)).collect(),
        reactions: Vec::new(),
        older,
    });
    chat
}

fn theirs(body: &str, number: u64) -> ChatMessage {
    ChatMessage {
        conversation: ConversationId::contact("ann"),
        sender_id: "ann".into(),
        sender: "Ann".into(),
        body: body.into(),
        timestamp: 1_700_000_000_000 + number * 1000,
        ..ChatMessage::default()
    }
}

#[gpui::test]
async fn scrolling_to_the_top_asks_for_older_messages(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(60, Some(1_699_999_999_000)));
    // Opened at the newest messages; nothing older is asked for yet.
    let (top, _) = ui.top_row();
    assert!(top >= 40, "opened at row {top}");
    ui.settle();
    let loads = |ui: &Ui| {
        ui.sent()
            .iter()
            .filter(|command| matches!(command, Command::LoadHistory { .. }))
            .count()
    };
    assert_eq!(loads(&ui), 0, "nothing older is asked for yet");
    for _ in 0..40 {
        ui.scroll(400.);
    }
    assert_eq!(ui.top_row().0, 0, "scrolled to the top");
    assert_eq!(loads(&ui), 1, "older messages are asked for, once");
}

#[gpui::test]
async fn older_messages_put_in_above_leave_the_view_in_place(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(60, Some(1_699_999_990_000)));
    // Away from the bottom, so the list no longer follows new messages.
    for _ in 0..2 {
        ui.scroll(300.);
    }
    let (row, offset) = ui.top_row();
    assert!(row > 10 && row < 55, "scrolled to row {row}");
    ui.act(|chat| {
        chat.apply(Event::History {
            conversation: ConversationId::contact("ann"),
            messages: (0..10)
                .map(|n| ChatMessage {
                    timestamp: 1_699_999_990_000 + n * 100,
                    ..theirs(&format!("Older {n}"), 0)
                })
                .collect(),
            reactions: Vec::new(),
            older: None,
        })
    });
    let (after_row, after_offset) = ui.top_row();
    assert_eq!(after_row, row + 10, "the same message is on top");
    assert!(
        (after_offset - offset).abs() < 2.,
        "moved from {offset} to {after_offset}"
    );
}

#[gpui::test]
async fn jumping_brings_the_row_into_view(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(60, None));
    assert!(ui.top_row().0 > 20, "opened at the newest");
    // As a click on a search result does.
    ui.act(|chat| {
        chat.open_chat_search();
        chat.search_chat("Message 10");
        chat.apply(Event::SearchResults {
            conversation: ConversationId::contact("ann"),
            query: "Message 10".into(),
            messages: vec![theirs("Message 10", 10)],
        });
        chat.show_result(0)
    });
    let (top, _) = ui.top_row();
    assert!(top <= 10 && 10 - top < 15, "row 10 is in view from row {top}");
}

/// The chat search lists what it found, and showing a result closes it and marks the message.
#[gpui::test]
async fn clicking_a_search_result_shows_it(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(3, None));
    ui.act(|chat| {
        chat.open_chat_search();
        chat.search_chat("moon");
        chat.apply(Event::SearchResults {
            conversation: ConversationId::contact("ann"),
            query: "moon".into(),
            messages: vec![theirs("the moon", 1), theirs("a moon rising", 2)],
        });
        Update::default()
    });
    assert!(ui.shown("Search result 1"), "the results are listed");
    ui.click("Search result 1");
    assert!(
        ui.booted.shared.chat.borrow().chat_search().is_none(),
        "showing a result closes the search"
    );
}

#[gpui::test]
async fn clicking_a_mention_candidate_picks_it(tc: &mut TestAppContext) {
    let person = |name: &str| Person {
        service_id: name.to_lowercase(),
        name: name.into(),
    };
    let group = Conversation {
        id: ConversationId("g".into()),
        title: "Rust".into(),
        kind: ConversationKind::Group,
        members: vec![person("Ann"), person("Andrea"), person("Bob")],
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    };
    let mut ui = Ui::new(tc, open_with(group));
    ui.type_in_composer("hi @An");
    assert!(ui.shown("Mention Ann"), "listed");
    assert!(!ui.shown("Mention Bob"), "only who matches");
    ui.click("Mention Andrea");
    assert_eq!(ui.composer_text(), "hi @Andrea ");
}

#[gpui::test]
async fn forwarding_goes_to_the_clicked_chat_once_confirmed(tc: &mut TestAppContext) {
    let mut chat = testing::chats(&["Ann", "Bob", "Carol"]);
    for (name, number) in [("bob", 1), ("carol", 2)] {
        chat.apply(Event::Latest(ChatMessage {
            conversation: ConversationId::contact(name),
            sender_id: name.into(),
            ..theirs("hello", number)
        }));
    }
    let ann = ConversationId::contact("ann");
    chat.select(&ann);
    chat.apply(Event::History {
        conversation: ann,
        messages: vec![theirs("Message 0", 0)],
        reactions: Vec::new(),
        older: None,
    });
    chat.start_forward(0);
    let mut ui = Ui::new(tc, chat);
    assert!(ui.booted.shared.chat.borrow().forwarding());
    ui.click("Forward to Carol");
    assert!(
        !ui.sent().iter().any(|c| matches!(c, Command::Forward { .. })),
        "asks first"
    );
    assert!(ui.shown("Back"), "and says where it goes");
    ui.click("Forward");
    let sent = ui.sent();
    assert!(
        sent.iter().any(|c| matches!(
            c,
            Command::Forward { conversation, .. } if *conversation == ConversationId::contact("carol")
        )),
        "sent to Carol"
    );
}

#[gpui::test]
async fn picked_messages_get_a_bar_to_act_on_them(tc: &mut TestAppContext) {
    let mut chat = many(3, None);
    chat.apply(Event::Message(ChatMessage {
        deleted: true,
        ..theirs("gone", 5)
    }));
    let mut ui = Ui::new(tc, chat);
    assert!(!ui.shown("Clear selection"));
    ui.act(|chat| chat.toggle_selected(0));
    assert!(ui.shown("Clear selection"), "the bar shows");
    assert!(ui.shown("Forward selected"), "while all can go");
    ui.act(|chat| chat.toggle_selected(3));
    assert!(
        !ui.shown("Forward selected"),
        "not once one that was deleted is picked"
    );
    ui.click("Clear selection");
    assert!(!ui.booted.shared.chat.borrow().has_selection());
}
