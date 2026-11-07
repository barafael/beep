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
    let fail = |chat: &mut ChatState, text: &str, retryable: bool| {
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
        messages: (0..count)
            .map(|n| theirs(&format!("Message {n}"), n as u64))
            .collect(),
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
    assert!(
        top <= 10 && 10 - top < 15,
        "row 10 is in view from row {top}"
    );
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
        !ui.sent()
            .iter()
            .any(|c| matches!(c, Command::Forward { .. })),
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

/// A notice (here, a new safety number) is drawn as a line of text of its own, between
/// messages, not as one of the contact's messages: centred, where a message starts at the left.
#[gpui::test]
async fn a_notice_is_drawn_as_a_line_not_a_message(tc: &mut TestAppContext) {
    let notice = ChatMessage {
        notice: true,
        body: "Your safety number with Ann has changed.".into(),
        ..theirs("", 2)
    };
    let mut chat = testing::chats(&["Ann"]);
    let ann = ConversationId::contact("ann");
    chat.select(&ann);
    chat.apply(Event::History {
        conversation: ann,
        messages: vec![theirs("Message 0", 1), notice, theirs("After", 3)],
        reactions: Vec::new(),
        older: None,
    });
    let mut ui = Ui::new(tc, chat);
    let notice = ui.bounds("Notice").expect("the notice is shown");
    let after = ui.bounds("Message 2").expect("so is the next message");
    let before = ui.bounds("Message 0").expect("and the one before");
    assert!(before.origin.y < notice.origin.y);
    assert!(notice.origin.y < after.origin.y);
    let list = ui.bounds("Messages").expect("a chat is open");
    let (left, width) = (list.origin.x, list.size.width);
    let offset = |b: gpui::Bounds<gpui::Pixels>| b.origin.x - left;
    let centre = offset(notice) + notice.size.width / 2.;
    assert!(
        (centre - width / 2.).abs() < gpui::px(20.),
        "centred at {centre:?} in {width:?}"
    );
    assert!(
        offset(after) < width * 0.2,
        "a message starts at the left, at {:?}",
        offset(after)
    );
    assert!(
        offset(notice) > offset(after) + gpui::px(20.),
        "the notice is indented to the middle"
    );
}

/// A long error wraps in the banner instead of being cut off after a line.
#[gpui::test]
async fn a_long_error_wraps(tc: &mut TestAppContext) {
    let mut ui = Ui::sized(tc, many(1, None), 400., 500.);
    let long = "This device was removed from your Signal account. Scan the code to link it \
                again; the messages here are kept.";
    ui.act(|chat| {
        chat.apply(Event::Error(testing::error(
            b33p_core::ErrorKind::Refused,
            long,
        )));
        Update::default()
    });
    let text = ui.bounds("The error").expect("the banner shows it");
    assert!(
        text.size.height > gpui::px(30.),
        "one line only: {:?}",
        text.size
    );
}

/// The error banner offers to try again only when that may work.
#[gpui::test]
async fn the_error_offers_to_try_again_when_that_may_work(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(1, None));
    ui.act(|chat| {
        chat.apply(Event::Error(testing::error(
            b33p_core::ErrorKind::Unreachable,
            "Couldn't reach Signal.",
        )));
        Update::default()
    });
    assert!(ui.shown("Dismiss"), "the banner shows");
    assert!(!ui.shown("Try again"), "nothing to try again");

    ui.act(|chat| {
        chat.apply(Event::CommandFailed {
            error: testing::error(
                b33p_core::ErrorKind::Unreachable,
                "Couldn't look the group up.",
            ),
            command: Command::PreviewGroupLink("https://signal.group/#x".into()),
        });
        Update::default()
    });
    // The banner slides open first.
    ui.wait(crate::animation::NORMAL * 2);
    ui.click("Try again");
    assert!(
        ui.sent()
            .iter()
            .any(|command| matches!(command, Command::PreviewGroupLink(_))),
        "the lookup goes out again"
    );
}

/// Screen readers hear a chat row as one item that says what matters, and can open it.
#[gpui::test]
async fn chat_rows_speak_for_themselves(tc: &mut TestAppContext) {
    let mut chat = testing::chats(&["Ann"]);
    let bob = ConversationId::contact("bob");
    chat.apply(Event::Conversations(vec![Conversation {
        id: bob.clone(),
        title: "Bob".into(),
        settings: ChatSettings {
            accepted: true,
            muted_until: Some(u64::MAX),
            ..ChatSettings::default()
        },
        ..Conversation::default()
    }]));
    chat.apply(Event::Latest(ChatMessage {
        conversation: bob.clone(),
        sender_id: "bob".into(),
        ..theirs("See you", 4)
    }));
    chat.apply(Event::Unread(vec![(bob.clone(), 2)]));
    let mut ui = Ui::new(tc, chat);
    assert!(
        ui.shown("Bob, 2 unread, muted, See you"),
        "one item says it all"
    );
    ui.click("Bob, 2 unread, muted, See you");
    assert_eq!(
        ui.booted
            .shared
            .chat
            .borrow()
            .selected()
            .map(|c| c.id.clone()),
        Some(bob)
    );
}

/// The chat list follows events: a chat with a new message moves to the top with its new
/// preview, and the rows it passed move down.
#[gpui::test]
async fn the_chat_list_follows_events(tc: &mut TestAppContext) {
    let mut chat = testing::chats(&["ann", "bob", "cid"]);
    let from = |name: &str, body: &str, number: u64| {
        Event::Message(ChatMessage {
            conversation: ConversationId::contact(name),
            sender_id: name.into(),
            sender: name.into(),
            ..theirs(body, number)
        })
    };
    chat.apply_all([
        from("ann", "a", 3),
        from("bob", "b", 2),
        from("cid", "c", 1),
    ]);
    let mut ui = Ui::new(tc, chat);
    let y = |ui: &mut Ui, label: &str| ui.bounds(label).map(|b| b.origin.y);
    assert!(y(&mut ui, "ann, 1 unread, a").is_some());
    ui.act(|chat| chat.apply(from("cid", "newest", 4)));
    let cid = y(&mut ui, "cid, 2 unread, newest").expect("its new preview");
    let ann = y(&mut ui, "ann, 1 unread, a").expect("ann is still listed");
    let bob = y(&mut ui, "bob, 1 unread, b").expect("so is bob");
    assert!(cid < ann && ann < bob, "{cid:?} {ann:?} {bob:?}");
}

/// Settings lead to the issue tracker.
#[gpui::test]
async fn settings_say_where_to_report_a_problem(tc: &mut TestAppContext) {
    // Tall enough for the whole page: what is scrolled away isn't there to find.
    let mut ui = Ui::sized(tc, many(1, None), 800., 4000.);
    ui.booted.shared.ui.borrow_mut().settings_open = true;
    ui.settle();
    ui.click("Report…");
    let opened = crate::web::OPENED.with(|opened| opened.borrow().clone());
    assert!(
        opened
            .last()
            .is_some_and(|url| url.ends_with("/issues/new")),
        "{opened:?}"
    );
}

/// The settings page shrinks with the window, next to the chat list or in its place: none of
/// its controls runs out of the window (which it did, with a page too wide to shrink).
#[gpui::test]
async fn settings_fit_a_narrow_window(tc: &mut TestAppContext) {
    for width in [400., 640., 800.] {
        let mut ui = Ui::sized(tc, many(1, None), width, 4000.);
        ui.booted.shared.ui.borrow_mut().settings_open = true;
        ui.settle();
        let report = ui.bounds("Report…").expect("the page is shown");
        let right: f32 = (report.origin.x + report.size.width).into();
        assert!(right <= width, "at {width}: the button ends at {right}");
    }
}

/// Shortcuts work wherever the focus is, also in the composer, which takes the keys that are
/// not shortcuts as text.
#[gpui::test]
async fn shortcuts_work_from_the_composer(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(3, None));
    ui.click("Message");
    ui.press("ctrl-k");
    // Ctrl+K went to the chat list's search, which has the caret now.
    ui.type_keys("zzz");
    assert_eq!(ui.composer_text(), "", "what was typed went to the search");
    ui.click("Message");
    ui.type_keys("x");
    assert_eq!(
        ui.composer_text(),
        "x",
        "not a shortcut: it reaches the composer"
    );
}

/// The sticker button opens the picker and asks for the packs.
#[gpui::test]
async fn the_sticker_button_asks_for_the_packs(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(3, None));
    ui.click("Stickers");
    assert_eq!(
        ui.booted.shared.ui.borrow().composer_popup,
        crate::composer::Popup::Stickers,
        "the picker is open"
    );
    assert!(
        ui.sent()
            .iter()
            .any(|command| matches!(command, Command::LoadStickers)),
        "the packs are asked for"
    );
}

/// A button that is only an icon says what it does when the pointer rests on it.
#[gpui::test]
async fn icon_buttons_have_tooltips(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(1, None));
    let before = ui.painted();
    ui.hover("Search this chat");
    // Resting there, moving a little, as a hand does.
    ui.wait(std::time::Duration::from_millis(100));
    ui.wait(std::time::Duration::from_secs(1));
    assert!(ui.painted() > before, "a tooltip shows");
}

/// An emoji goes where the caret is, in place of what is selected, and the caret after it.
#[gpui::test]
async fn an_emoji_goes_where_the_caret_is(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(1, None));
    let put = |ui: &mut Ui, text: &str, range: std::ops::Range<usize>| {
        ui.type_in_composer(text);
        ui.insert_in_composer("👋", range);
        ui.composer_text()
    };
    assert_eq!(put(&mut ui, "Hi you", 2..2), "Hi👋 you");
    assert_eq!(put(&mut ui, "Hi you", 3..6), "Hi 👋");
    assert_eq!(put(&mut ui, "", 0..0), "👋");
    assert_eq!(put(&mut ui, "ab", 2..2), "ab👋");
}

/// A message's text is selected by dragging across it and copied with Ctrl+C; selecting in
/// another message drops the first one's selection, so Ctrl+C copies only the newer one.
#[gpui::test]
async fn ctrl_c_copies_the_text_selected_in_one_message(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, many(3, None));
    let drag_across = |ui: &mut Ui, label: &str| {
        let bubble = ui.bounds(label).expect("shown");
        let y = bubble.center().y;
        // Inside the bubble's padding, from the first letter to past the last.
        ui.drag(
            gpui::point(bubble.origin.x + gpui::px(14.), y),
            gpui::point(bubble.origin.x + bubble.size.width - gpui::px(2.), y),
        );
    };
    drag_across(&mut ui, "Message 1");
    ui.press("ctrl-c");
    assert_eq!(ui.clipboard().as_deref(), Some("Message 1"));

    drag_across(&mut ui, "Message 1");
    drag_across(&mut ui, "Message 2");
    ui.press("ctrl-c");
    assert_eq!(ui.clipboard().as_deref(), Some("Message 2"));
}

/// What a screen reader is told of things that are not words on a button: the code to scan,
/// the timer of a chat and of a message, the folders, the archive.
#[gpui::test]
async fn things_say_what_they_are_to_screen_readers(tc: &mut TestAppContext) {
    // The code to scan, where there is no chat yet.
    let mut waiting = ChatState::default();
    waiting.apply(Event::LinkQr(
        b33p_core::QrMatrix::encode("sgnl://linkdevice?uuid=x").expect("a code"),
    ));
    let mut ui = Ui::new(tc, waiting);
    assert!(ui.shown("The code to scan"));

    // A chat with a timer and a message that disappears, a folder and an archived chat.
    let mut chat = open_with(ann(|c| c.expire_timer = 3600));
    chat.apply(Event::Folders(vec![b33p_core::ChatFolder {
        name: "Work".into(),
        all_individual: true,
        ..Default::default()
    }]));
    chat.apply(Event::Conversations(vec![Conversation {
        id: ConversationId::contact("old"),
        title: "Old".into(),
        settings: ChatSettings {
            accepted: true,
            archived: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    }]));
    let ann_id = ConversationId::contact("ann");
    chat.apply(Event::History {
        conversation: ann_id,
        messages: vec![ChatMessage {
            expires_at: Some(b33p_core::now_millis() + 3_600_000),
            ..theirs("soon gone", 1)
        }],
        reactions: Vec::new(),
        older: None,
    });
    let mut ui = Ui::new(tc, chat);
    assert!(
        ui.shown("Disappearing messages: 1 hour"),
        "the chat's timer"
    );
    assert!(ui.shown("All chats"), "the folder that has everything");
    assert!(ui.shown("Work"), "the others by name");
    assert!(ui.shown("Archived chats, 1"), "the archive");
    assert!(ui.shown("Disappears 0"), "the message that disappears");
}

/// Jumping to a message lights it up and lets it settle, unless motion is off: then it is only
/// highlighted.
#[gpui::test]
async fn a_message_jumped_to_flashes_unless_motion_is_off(tc: &mut TestAppContext) {
    let jump = |ui: &mut Ui| {
        ui.act(|chat| {
            chat.open_chat_search();
            chat.search_chat("Message 2");
            chat.apply(Event::SearchResults {
                conversation: ConversationId::contact("ann"),
                query: "Message 2".into(),
                messages: vec![theirs("Message 2", 2)],
            });
            chat.show_result(0)
        });
    };
    let settles = |tc: &mut TestAppContext, animations: Option<b33p_core::settings::Animations>| {
        let mut ui = Ui::new(tc, many(5, None));
        if let Some(animations) = animations {
            let mut settings = ui.booted.shared.settings.borrow().clone();
            settings.animations = animations;
            let shared = ui.booted.shared.clone();
            ui.cx.update(|_, cx| shared.apply_settings(settings, cx));
            ui.settle();
        }
        jump(&mut ui);
        let at_first = ui.backgrounds();
        ui.wait(std::time::Duration::from_secs(2));
        at_first != ui.backgrounds()
    };
    use b33p_core::settings::Animations;
    assert!(
        settles(tc, Some(Animations::On)),
        "it flashes, then settles"
    );
    assert!(
        !settles(tc, Some(Animations::Off)),
        "with motion off it is lit as it stays"
    );
}

/// The "Animations" setting reaches everything that moves: GPUI and the component library ask
/// the app's reduced-motion flag before they animate, and the setting (or the desktop's wish,
/// for "As the desktop says") decides it.
#[gpui::test]
async fn the_animations_setting_decides_whether_things_move(tc: &mut TestAppContext) {
    use b33p_core::settings::Animations;
    let mut ui = Ui::new(tc, many(1, None));
    let shared = ui.booted.shared.clone();
    let reduced = |ui: &mut Ui, animations: Animations, desktop_reduces: bool| {
        let mut settings = shared.settings.borrow().clone();
        settings.animations = animations;
        shared.system_reduces_motion.set(desktop_reduces);
        ui.cx.update(|_, cx| {
            shared.apply_settings(settings, cx);
            cx.reduce_motion()
        })
    };
    assert!(
        !reduced(&mut ui, Animations::System, false),
        "the desktop doesn't mind"
    );
    assert!(
        reduced(&mut ui, Animations::System, true),
        "the desktop asks for less"
    );
    assert!(
        !reduced(&mut ui, Animations::On, true),
        "on, whatever the desktop says"
    );
    assert!(
        reduced(&mut ui, Animations::Off, false),
        "off, whatever the desktop says"
    );
}

/// Alt+↓ and Alt+↑ go through the chats, Alt+1 opens the first pinned one, Esc closes the
/// innermost of what is open: the same keys as the Slint window's.
#[gpui::test]
async fn the_shortcuts_go_through_chats_and_close_what_is_open(tc: &mut TestAppContext) {
    let mut chat = testing::chats(&["Ann", "Bob", "Cid"]);
    for (name, number) in [("ann", 3), ("bob", 2), ("cid", 1)] {
        chat.apply(Event::Latest(ChatMessage {
            conversation: ConversationId::contact(name),
            sender_id: name.into(),
            ..theirs("hello", number)
        }));
    }
    let mut ui = Ui::new(tc, chat);
    let open = |ui: &Ui| {
        ui.booted
            .shared
            .chat
            .borrow()
            .selected()
            .map(|c| c.title.clone())
    };
    assert_eq!(open(&ui), None);
    ui.press("alt-down");
    assert_eq!(open(&ui).as_deref(), Some("Ann"));
    ui.press("alt-down");
    assert_eq!(open(&ui).as_deref(), Some("Bob"));
    ui.press("alt-up");
    assert_eq!(open(&ui).as_deref(), Some("Ann"));

    // Esc closes what is in front, innermost first: here the settings, over the chat that had
    // the focus (in its composer), which is no longer there to hear the key.
    ui.booted.shared.ui.borrow_mut().settings_open = true;
    ui.settle();
    ui.press("escape");
    assert!(
        !ui.booted.shared.ui.borrow().settings_open,
        "the settings close"
    );
    assert_eq!(open(&ui).as_deref(), Some("Ann"), "the chat stays");
    ui.act(|chat| {
        chat.open_chat_search();
        Update::default()
    });
    assert!(ui.booted.shared.chat.borrow().chat_search().is_some());
    ui.press("escape");
    assert!(
        ui.booted.shared.chat.borrow().chat_search().is_none(),
        "the search closes"
    );
}
