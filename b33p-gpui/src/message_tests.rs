//! What a message offers beyond its text, on GPUI's test platform: a click on its quote, the bar
//! of pinned messages and the menu's pinning, a long message folded to "Read more", and the
//! info of a message of ours. The same checks as the Slint window's
//! (`b33p-slint/src/ui_tests.rs`, `tests/harness.rs`), and for the info and the readers, which
//! Slint has no window for yet, the ones that say what this one does.

use b33p_core::state::{ChatState, Clock, Update};
use b33p_core::{
    ChatMessage, ChatSettings, Command, Conversation, ConversationId, ConversationKind, Event,
    MessageInfo, Person, PinnedMessage, Quote, Receipt,
};
use gpui::TestAppContext;

use crate::message_menu::{self, PIN_CHOICES, PinOffer};
use crate::testing::{self, Ui};

const START: u64 = 1_700_000_000_000;

fn person(name: &str) -> Person {
    Person {
        service_id: name.to_lowercase(),
        name: name.into(),
    }
}

/// A message by Ann (or by us, `ours`), the `number`th second after the start.
fn said(body: &str, number: u64, ours: bool) -> ChatMessage {
    ChatMessage {
        conversation: ConversationId::contact("ann"),
        sender_id: if ours { "me" } else { "ann" }.into(),
        sender: if ours { "You" } else { "Ann" }.into(),
        body: body.into(),
        timestamp: START + number * 1000,
        outgoing: ours,
        ..ChatMessage::default()
    }
}

/// Opens a chat with Ann holding `messages`, with `older` still to load if given.
fn chat_with(messages: Vec<ChatMessage>, older: Option<u64>) -> ChatState {
    let mut chat = testing::chats(&["Ann"]);
    let ann = ConversationId::contact("ann");
    chat.select(&ann);
    chat.apply(Event::History {
        conversation: ann,
        messages,
        reactions: Vec::new(),
        older,
    });
    chat
}

/// Sixty messages from Ann, "Message 0" to "Message 59", the last of which answers `quoted`.
fn thread(quoted: u64) -> Vec<ChatMessage> {
    let mut messages: Vec<_> = (0..60)
        .map(|n| said(&format!("Message {n}"), n, false))
        .collect();
    messages[59].quote = Some(Quote {
        author: "Ann".into(),
        text: "Message 5".into(),
        author_id: "ann".into(),
        timestamp: quoted,
    });
    messages
}

fn pinned(message: ChatMessage) -> PinnedMessage {
    PinnedMessage {
        message,
        expires_at: None,
    }
}

fn highlighted(ui: &Ui) -> Vec<usize> {
    ui.booted
        .shared
        .chat
        .borrow()
        .messages()
        .enumerate()
        .filter(|(_, view)| view.highlighted)
        .map(|(at, _)| at)
        .collect()
}

/// A click on the quote in a bubble shows the message it quotes: scrolls there and marks it. The
/// quote says what it does, and answers the pointer.
#[gpui::test]
async fn a_click_on_a_quote_shows_the_quoted_message(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, chat_with(thread(START + 5000), None));
    assert!(ui.top_row().0 > 20, "opened at the newest");
    assert!(ui.shown("Quote 59"), "the quote is a button of its own");
    let resting = ui.backgrounds();
    ui.hover("Quote 59");
    assert_ne!(ui.backgrounds(), resting, "the quote answers the pointer");

    ui.click("Quote 59");
    let (top, _) = ui.top_row();
    assert!(top <= 5 && 5 - top < 15, "row 5 is in view from row {top}");
    assert_eq!(highlighted(&ui), [5], "and marked");
    assert!(
        !ui.sent()
            .iter()
            .any(|command| matches!(command, Command::LoadHistory { .. })),
        "it was loaded already"
    );
}

/// When the quoted message is in older history, that is loaded first, and the view goes there
/// once it has arrived.
#[gpui::test]
async fn a_quote_of_an_older_message_loads_it_first(tc: &mut TestAppContext) {
    let mut messages = thread(START - 5000);
    messages[59].quote.as_mut().unwrap().text = "Before all this".into();
    let mut ui = Ui::new(tc, chat_with(messages, Some(START)));
    ui.sent();
    ui.click("Quote 59");
    let loads = ui
        .sent()
        .into_iter()
        .filter(|command| matches!(command, Command::LoadHistory { before: Some(at), .. } if *at == START))
        .count();
    assert_eq!(loads, 1, "the page before the loaded ones is asked for");
    assert!(highlighted(&ui).is_empty(), "nothing is there to mark yet");

    ui.act(|chat| {
        chat.apply(Event::History {
            conversation: ConversationId::contact("ann"),
            messages: vec![said("Before all this", 0, false).tap(|m| m.timestamp = START - 5000)],
            reactions: Vec::new(),
            older: None,
        })
    });
    assert_eq!(highlighted(&ui), [0], "the old message is marked");
    assert_eq!(ui.top_row().0, 0, "and shown");
}

trait Tap: Sized {
    fn tap(mut self, change: impl FnOnce(&mut Self)) -> Self {
        change(&mut self);
        self
    }
}
impl Tap for ChatMessage {}

/// A group we administer (or not), with messages from Ann and by us.
fn group(admin: bool) -> ChatState {
    let id = ConversationId("g".into());
    let mut chat = testing::chats(&[]);
    chat.apply(Event::Conversations(vec![Conversation {
        id: id.clone(),
        title: "Rust".into(),
        kind: ConversationKind::Group,
        admin,
        members: vec![person("Ann"), person("Bob"), person("Cid")],
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    }]));
    chat.select(&id);
    let mut messages: Vec<_> = (0..6)
        .map(|n| ChatMessage {
            conversation: id.clone(),
            ..said(&format!("Message {n}"), n, n % 2 == 1)
        })
        .collect();
    messages[1].delivery = b33p_core::Delivery::Delivered;
    chat.apply(Event::History {
        conversation: id,
        messages,
        reactions: Vec::new(),
        older: None,
    });
    chat
}

fn pins_of(chat: &ChatState, at: &[u64]) -> Event {
    let id = chat.selected().unwrap().id.clone();
    Event::PinnedMessages {
        conversation: id.clone(),
        pins: at
            .iter()
            .map(|&n| {
                pinned(ChatMessage {
                    conversation: id.clone(),
                    ..said(&format!("Message {n}\nsecond line"), n, n % 2 == 1)
                })
            })
            .collect(),
    }
}

/// The bar of pins above the chat: one pin at a time, the newest first; a click shows it in the
/// chat and turns to the one pinned before; a screen reader hears which it is.
#[gpui::test]
async fn the_pin_bar_shows_one_pin_at_a_time(tc: &mut TestAppContext) {
    let mut ui = Ui::new(tc, group(true));
    assert!(
        !ui.shown("Pinned message: Ann: Message 0"),
        "no pins, no bar"
    );
    ui.act(|chat| {
        let event = pins_of(chat, &[0]);
        chat.apply(event);
        Update {
            pins: true,
            ..Update::default()
        }
    });
    assert!(
        ui.shown("Pinned message: Ann: Message 0"),
        "one pin: its author and first line"
    );
    ui.click("Pinned message: Ann: Message 0");
    assert_eq!(highlighted(&ui), [0], "a click shows the pin in the chat");

    // Three: the newest first, each click turns to the one before it, round to the newest.
    ui.act(|chat| {
        let event = pins_of(chat, &[0, 2, 4]);
        chat.apply(event);
        Update {
            pins: true,
            ..Update::default()
        }
    });
    let labels = [
        "Pinned message 3 of 3: Ann: Message 4",
        "Pinned message 2 of 3: Ann: Message 2",
        "Pinned message 1 of 3: Ann: Message 0",
        "Pinned message 3 of 3: Ann: Message 4",
    ];
    for pair in labels.windows(2) {
        assert!(ui.shown(pair[0]), "{:?} is shown", pair[0]);
        ui.click(pair[0]);
        assert!(ui.shown(pair[1]), "then {:?}", pair[1]);
    }
    assert_eq!(
        highlighted(&ui),
        [0],
        "the last click was on the oldest, row 0"
    );

    // Other pins start the bar over at the newest.
    ui.click(labels[0]);
    ui.act(|chat| {
        let event = pins_of(chat, &[2, 4]);
        chat.apply(event);
        Update {
            pins: true,
            ..Update::default()
        }
    });
    assert!(ui.shown("Pinned message 2 of 2: Ann: Message 4"));
}

/// The bar's button unpins the pin shown, for those who may pin; others don't get it.
#[gpui::test]
async fn the_pin_bar_unpins_for_those_who_may(tc: &mut TestAppContext) {
    for (admin, offered) in [(true, true), (false, false)] {
        let mut ui = Ui::new(tc, group(admin));
        ui.act(|chat| {
            let event = pins_of(chat, &[0, 2]);
            chat.apply(event);
            Update {
                pins: true,
                ..Update::default()
            }
        });
        assert!(ui.shown("Pinned message 2 of 2: Ann: Message 2"));
        assert_eq!(ui.shown("Unpin"), offered, "unpin offered to an admin only");
        if offered {
            ui.sent();
            ui.click("Unpin");
            let sent = ui.sent();
            assert!(
                matches!(&sent[..], [Command::UnpinMessage { target, .. }] if target.timestamp == START + 2000),
                "the shown pin, the newest, is unpinned: {sent:?}"
            );
            assert!(
                highlighted(&ui).is_empty(),
                "the click was the button's, not the bar's"
            );
        }
    }
}

/// Pins that run out go from the bar when they do, with no event to say so.
#[gpui::test]
async fn a_pin_leaves_the_bar_when_it_runs_out(tc: &mut TestAppContext) {
    let now = b33p_core::now_millis();
    let mut chat = group(true);
    let clock = Clock::fixed(now);
    chat.set_clock(clock.clone());
    let id = chat.selected().unwrap().id.clone();
    chat.apply(Event::PinnedMessages {
        conversation: id.clone(),
        pins: vec![PinnedMessage {
            message: ChatMessage {
                conversation: id,
                ..said("Message 0", 0, false)
            },
            expires_at: Some(now + 3000),
        }],
    });
    let mut ui = Ui::new(tc, chat);
    assert!(ui.shown("Pinned message: Ann: Message 0"));
    ui.wait(std::time::Duration::from_secs(1));
    assert!(ui.shown("Pinned message: Ann: Message 0"), "not yet");
    clock.set(now + 3001);
    ui.wait(std::time::Duration::from_secs(3));
    assert!(
        !ui.shown("Pinned message: Ann: Message 0"),
        "gone when its time came"
    );
}

/// What the menu offers about pins: pinning (with its choices) or unpinning where messages can be
/// pinned, never for a notice, a deleted message or one not sent, and a note that the oldest pin
/// would go when three are pinned.
#[gpui::test]
async fn the_menu_offers_pinning_where_it_is_allowed(tc: &mut TestAppContext) {
    let offers = |chat: &ChatState| -> Vec<PinOffer> {
        (0..chat.messages().len())
            .map(|at| {
                let view = chat.message_view(at).unwrap();
                crate::bubble::pin_offer(chat, at, view.message)
            })
            .collect()
    };
    let mut chat = group(true);
    let in_group = chat.selected().unwrap().id.clone();
    chat.apply(Event::Message(ChatMessage {
        conversation: in_group.clone(),
        notice: true,
        ..said("Safety number", 10, false)
    }));
    chat.apply(Event::Message(ChatMessage {
        conversation: in_group,
        deleted: true,
        ..said("gone", 11, false)
    }));
    let event = pins_of(&chat, &[0]);
    chat.apply(event);
    assert_eq!(
        offers(&chat),
        [
            PinOffer::Unpin,
            PinOffer::Pin,
            PinOffer::Pin,
            PinOffer::Pin,
            PinOffer::Pin,
            PinOffer::Pin,
            PinOffer::None,
            PinOffer::None,
        ]
    );
    assert!(!chat.pins_full());
    let event = pins_of(&chat, &[0, 1, 2]);
    chat.apply(event);
    assert!(chat.pins_full(), "a fourth would replace the oldest");

    // Someone who may not pin gets nothing.
    let mut member = group(false);
    let event = pins_of(&member, &[0]);
    member.apply(event);
    assert!(offers(&member).iter().all(|offer| *offer == PinOffer::None));

    // The choices, as Slint's menu has them, and what each asks for.
    let labels: Vec<_> = PIN_CHOICES.iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels,
        [
            "For 24 hours",
            "For 7 days",
            "For 30 days",
            "Until unpinned"
        ]
    );
    let mut ui = Ui::new(tc, group(true));
    ui.sent();
    for (_, seconds) in PIN_CHOICES {
        ui.cx.update(|_, cx| {
            message_menu::pin(&ui.booted.shared, 2, seconds, cx);
        });
        let sent = ui.sent();
        assert!(
            matches!(&sent[..], [Command::PinMessage { seconds: s, target, .. }]
                if *s == seconds && target.timestamp == START + 2000),
            "{sent:?}"
        );
    }
}

/// A message of more than 800 characters shows ten lines and "Read more"; the rest comes on a
/// click, for that message only, and folds again when the chat is shown anew.
#[gpui::test]
async fn a_long_message_folds_until_read_more(tc: &mut TestAppContext) {
    let long = (0..150).map(|n| format!("line {n}\n")).collect::<String>();
    assert!(long.chars().count() > 800);
    let mut ui = Ui::new(
        tc,
        chat_with(vec![said(&long, 0, false), said("Short.", 1, false)], None),
    );
    assert!(ui.shown("Read more 0"), "the long one is folded");
    assert!(!ui.shown("Read more 1"), "the short one is not");
    let folded = ui.bounds("Message 0").unwrap().size.height;

    ui.click("Read more 0");
    assert!(!ui.shown("Read more 0"), "unfolded");
    let unfolded = ui.bounds("Message 0").unwrap().size.height;
    assert!(
        unfolded > folded * 3.,
        "the whole text is taller: {folded:?} then {unfolded:?}"
    );

    // Shown anew (another chat, and back), it is folded again.
    ui.show(Update::reset());
    assert!(ui.shown("Read more 0"));
}

/// The message-info dialog: for ours only, says it is gathering until the backend answers, then
/// lists each recipient; Esc and "Close" close it.
#[gpui::test]
async fn the_info_of_a_message_of_ours_lists_who_got_it(tc: &mut TestAppContext) {
    let mut ui = Ui::new(
        tc,
        chat_with(vec![said("Theirs", 0, false), said("Ours", 1, true)], None),
    );
    ui.sent();
    ui.cx
        .update(|_, cx| message_menu::open_info(&ui.booted.shared, 0, cx));
    ui.settle();
    assert!(!ui.shown("Message info"), "a message of theirs has none");
    assert!(ui.sent().is_empty());

    ui.cx
        .update(|_, cx| message_menu::open_info(&ui.booted.shared, 1, cx));
    ui.settle();
    assert!(
        matches!(&ui.sent()[..], [Command::LoadMessageInfo { target, .. }] if target.timestamp == START + 1000)
    );
    assert!(ui.shown("Message info"), "the dialog is open");
    assert!(ui.shown("Gathering"), "and says it is still gathering");
    assert!(!ui.shown("Recipient 0"));

    ui.act(|chat| {
        chat.apply(Event::MessageInfo {
            conversation: ConversationId::contact("ann"),
            target: said("Ours", 1, true).reference(),
            info: MessageInfo {
                recipients: vec![Receipt {
                    person: person("Ann"),
                    delivered_at: Some(START + 2000),
                    read_at: Some(START + 3000),
                }],
            },
        });
        Update::default()
    });
    assert!(!ui.shown("Gathering"));
    assert!(ui.shown("Recipient 0"), "Ann is listed");

    ui.press("escape");
    assert!(!ui.shown("Message info"), "Esc closes it");
    assert!(ui.booted.shared.chat.borrow().message_info().is_none());

    ui.cx
        .update(|_, cx| message_menu::open_info(&ui.booted.shared, 1, cx));
    ui.settle();
    assert!(ui.shown("Message info"));
    ui.click("Close");
    assert!(!ui.shown("Message info"), "Close closes it");
}

/// In a group, the members who read up to our newest message have their small pictures under it.
#[gpui::test]
async fn readers_show_under_the_newest_message_they_read(tc: &mut TestAppContext) {
    let mut chat = group(true);
    let id = chat.selected().unwrap().id.clone();
    chat.apply(Event::Readers {
        conversation: id,
        newest: [
            ("ann".to_owned(), START + 5000),
            ("bob".to_owned(), START + 3000),
        ]
        .into_iter()
        .collect(),
    });
    let mut ui = Ui::new(tc, chat);
    // Our messages are rows 1, 3 and 5; Ann read up to the last, Bob up to the middle one.
    assert!(ui.shown("Readers 5"), "under our newest");
    assert!(ui.shown("Readers 3"), "and Bob's");
    assert!(!ui.shown("Readers 1"), "nobody's newest");
    assert!(!ui.shown("Readers 2"), "never under theirs");
}
