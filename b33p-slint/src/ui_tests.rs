//! The window's own behaviour, on Slint's headless testing backend: what the Slint code does
//! with the properties Rust sets and the input it gets. (The fluent style, since the native one
//! needs Qt.)

use std::cell::Cell;
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::generated::{
    Account, AppWindow, ChatList, Composer, ConversationItem, DeliveryState, Groups, MessageItem,
    OpenChat, Panels, Prefs, Screen, StoryItem,
};
use crate::rows::Rows;

fn window_with_messages(count: usize) -> (AppWindow, Rc<Rows<MessageItem>>) {
    i_slint_backend_testing::init_no_event_loop();
    let window = AppWindow::new().unwrap();
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 500.));
    window.global::<Account>().set_screen(Screen::Chats);
    let ann = ConversationItem {
        id: "a".into(),
        title: "Ann".into(),
        ..Default::default()
    };
    window
        .global::<ChatList>()
        .set_conversations(ModelRc::new(VecModel::from(vec![ann.clone()])));
    window.global::<ChatList>().set_selected(0);
    window.global::<ChatList>().set_selected_id("a".into());
    window.global::<ChatList>().set_current(ann);
    let messages = Rc::new(Rows::default());
    messages.set_vec(
        (0..count)
            .map(|n| message(&format!("Message {n}")))
            .collect(),
    );
    window
        .global::<OpenChat>()
        .set_messages(messages.clone().into());
    window.show().unwrap();
    settle(&window);
    (window, messages)
}

fn message(text: &str) -> MessageItem {
    MessageItem {
        body: slint::StyledText::from_plain_text(text),
        has_body: true,
        text: text.into(),
        first: true,
        last: true,
        ..Default::default()
    }
}

/// Lets layouts, change handlers and timers catch up. Nothing renders here, so the tree is
/// visited each round instead, which makes lists create the rows they show.
fn settle(window: &AppWindow) {
    for _ in 0..20 {
        let _ = ElementHandle::find_by_accessible_label(window, "").count();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
        slint::platform::update_timers_and_animations();
    }
}

fn find(window: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(window, label).next()
}

/// Scrolls the message list by `pixels` (positive is towards older messages).
fn scroll(window: &AppWindow, pixels: f32) {
    ElementHandle::find_by_element_id(window, "AppWindow::message-list")
        .next()
        .expect("a chat is open")
        .scroll(0., pixels);
    settle(window);
}

/// The newest of "Message 0" … "Message {count - 1}" on screen.
fn newest_shown(window: &AppWindow, count: usize) -> Option<(usize, ElementHandle)> {
    (0..count)
        .rev()
        .find_map(|n| Some((n, find(window, &format!("Message {n}"))?)))
}

/// A notice (here, a new safety number) is drawn as a line of text of its own, between
/// messages, not as one of the contact's messages.
#[test]
fn a_notice_is_drawn_as_a_line_not_a_message() {
    let (window, messages) = window_with_messages(2);
    let text = "Your safety number with Ann has changed.";
    messages.extend([
        MessageItem {
            notice: true,
            ..message(text)
        },
        message("After"),
    ]);
    settle(&window);
    let notice = find(&window, text).expect("the notice is shown");
    let after = find(&window, "After").expect("so is the next message");
    let before = find(&window, "Message 1").expect("and the one before");
    assert!(before.absolute_position().y < notice.absolute_position().y);
    assert!(notice.absolute_position().y < after.absolute_position().y);
    // Centred in the list, unlike an incoming message, which starts at its left edge.
    let list = ElementHandle::find_by_element_id(&window, "AppWindow::message-list")
        .next()
        .expect("a chat is open");
    let (left, width) = (list.absolute_position().x, list.size().width);
    let offset = |e: &ElementHandle| e.absolute_position().x - left;
    let centre = offset(&notice) + notice.size().width / 2.;
    assert!(
        (centre - width / 2.).abs() < 20.,
        "centred at {centre} in {width}"
    );
    assert!(
        offset(&after) < width * 0.2,
        "a message starts at the left, at {}",
        offset(&after)
    );
    assert!(
        offset(&notice) > offset(&after) + 20.,
        "the notice is indented to the middle"
    );
}

/// The details page offers to mark the safety number verified, or to clear that, and says
/// when it is.
/// A text story's box takes its size from the viewer. The viewer must not take its size from the
/// box in turn (the text's height follows its width), or any click in the viewer, which lays it
/// out, dies of the binding loop.
#[test]
fn a_text_story_can_be_clicked_through() {
    let (window, _messages) = window_with_messages(0);
    let stepped = Rc::new(Cell::new(0));
    window.global::<Panels>().on_step_story({
        let stepped = stepped.clone();
        move |by| stepped.set(stepped.get() + by)
    });
    window.global::<Panels>().set_stories_open(true);
    window.global::<Panels>().set_story(StoryItem {
        shown: true,
        author: "Ferris".into(),
        time: "11:42".into(),
        position: "1 / 1".into(),
        text_story: true,
        text: "cargo build --release 🦀".into(),
        foreground: slint::Color::from_rgb_u8(255, 255, 255),
        background: slint::Color::from_rgb_u8(43, 138, 96),
        ..Default::default()
    });
    settle(&window);
    find(&window, "Next story")
        .expect("the viewer shows")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(stepped.get(), 1);
}

/// A message that failed to go out offers to be sent again, from its bubble; one Signal
/// refused doesn't, since sending it again would be refused too.
#[test]
fn a_failed_message_offers_a_retry() {
    let (window, messages) = window_with_messages(3);
    messages.set_row_data(
        1,
        MessageItem {
            key: "2:me".into(),
            outgoing: true,
            delivery: DeliveryState::Failed,
            can_send_again: true,
            ..message("never went out")
        },
    );
    messages.set_row_data(
        2,
        MessageItem {
            key: "3:me".into(),
            outgoing: true,
            delivery: DeliveryState::Failed,
            ..message("refused")
        },
    );
    let retried = Rc::new(std::cell::RefCell::new(None));
    window.global::<OpenChat>().on_retry_send({
        let retried = retried.clone();
        move |key| *retried.borrow_mut() = Some(key)
    });
    settle(&window);
    assert_eq!(
        find(&window, "Send again").into_iter().count(),
        1,
        "only the failed message offers it"
    );
    find(&window, "Send again")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(retried.borrow().as_deref(), Some("2:me"));
}

/// Not a test: a fixed workload for callgrind (`cargo test ... bench_message_list -- --ignored`
/// under `valgrind --tool=callgrind --toggle-collect='*phase_*'`), so that changes to the
/// message list can be compared by instruction count.
#[test]
#[ignore]
fn bench_message_list() {
    let (window, messages) = window_with_messages(300);
    for row in (0..300).step_by(7) {
        let mut item = messages.row_data(row).unwrap();
        item.outgoing = true;
        messages.set_row_data(row, item);
    }
    phase_scroll(&window);
    phase_switch(&window, &messages);
}

/// Scrolls up through the history and back down, which makes and drops rows.
#[inline(never)]
fn phase_scroll(window: &AppWindow) {
    for _ in 0..12 {
        scroll(window, 400.);
    }
    for _ in 0..12 {
        scroll(window, -400.);
    }
}

/// Switches chats twice: every row goes and comes again.
#[inline(never)]
fn phase_switch(window: &AppWindow, messages: &Rc<Rows<MessageItem>>) {
    for round in 0..2 {
        let items: Vec<_> = (0..200)
            .map(|n| message(&format!("Round {round} message {n}")))
            .collect();
        messages.set_vec(items);
        settle(window);
    }
}

#[test]
fn the_safety_number_can_be_marked_verified() {
    let (window, _messages) = window_with_messages(1);
    let asked = Rc::new(Cell::new(None));
    window.global::<Groups>().on_set_verified({
        let asked = asked.clone();
        move |verified| asked.set(Some(verified))
    });
    // Tall enough for the whole details page: the panel makes only what shows.
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 1200.));
    window.global::<Groups>().set_details_open(true);
    window
        .global::<Groups>()
        .set_details_safety_number("12345 67890".into());
    settle(&window);
    assert!(
        find(&window, "Copy").is_some(),
        "the safety number's buttons show"
    );
    assert!(find(&window, "Verified: you compared it with theirs.").is_none());
    find(&window, "Mark as verified")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(asked.get(), Some(true));

    window.global::<Groups>().set_details_verified(true);
    settle(&window);
    assert!(find(&window, "Verified: you compared it with theirs.").is_some());
    find(&window, "Clear verification")
        .expect("offered once verified")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(asked.get(), Some(false));
}

/// A chat whose verified safety number changed says so where the composer was, and offers to
/// continue (confirming it) or to see the safety number.
#[test]
fn a_changed_safety_number_holds_the_composer() {
    let (window, _messages) = window_with_messages(1);
    let confirmed = Rc::new(std::cell::RefCell::new(None));
    window.global::<Groups>().on_confirm_safety_number({
        let confirmed = confirmed.clone();
        move |id| *confirmed.borrow_mut() = Some(id)
    });
    let opened = Rc::new(Cell::new(false));
    window.global::<Groups>().on_open_details({
        let opened = opened.clone();
        move || opened.set(true)
    });
    let notice = "Your safety number with Ann changed since you verified it. Compare it again, or continue without.";
    assert!(find(&window, notice).is_none());
    let changed = ConversationItem {
        id: "a".into(),
        title: "Ann".into(),
        safety_changed: true,
        ..Default::default()
    };
    window
        .global::<ChatList>()
        .set_conversations(ModelRc::new(VecModel::from(vec![changed.clone()])));
    window.global::<ChatList>().set_current(changed);
    settle(&window);
    assert!(find(&window, notice).is_some());
    find(&window, "Show safety number")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert!(opened.get());
    find(&window, "Continue")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(confirmed.borrow().as_deref(), Some("a"));
}

/// A group we're invited to says so where the composer would be, and answers the invitation.
#[test]
fn an_invitation_is_answered_in_the_chat() {
    let (window, _messages) = window_with_messages(1);
    let answers = Rc::new(std::cell::RefCell::new(Vec::new()));
    window.global::<Groups>().on_answer_invitation({
        let answers = answers.clone();
        move |id, accept| answers.borrow_mut().push((id.to_string(), accept))
    });
    let text = "You're invited to join this group. Its members see you once you join.";
    assert!(find(&window, text).is_none());
    let club = ConversationItem {
        id: "club".into(),
        title: "Book club".into(),
        group: true,
        left: true,
        invited: true,
        ..Default::default()
    };
    window
        .global::<ChatList>()
        .set_conversations(ModelRc::new(VecModel::from(vec![club.clone()])));
    window.global::<ChatList>().set_selected_id(club.id.clone());
    window.global::<ChatList>().set_current(club);
    settle(&window);
    assert!(find(&window, text).is_some());
    for label in ["Decline", "Accept"] {
        find(&window, label)
            .expect("offered")
            .mock_single_click(PointerEventButton::Left);
    }
    assert_eq!(
        *answers.borrow(),
        [("club".to_string(), false), ("club".to_string(), true)]
    );
}

/// A phone number typed in the chat search offers to message it, which looks it up.
#[test]
fn a_typed_number_can_be_messaged() {
    let (window, _messages) = window_with_messages(1);
    let found = Rc::new(std::cell::RefCell::new(None));
    window.global::<ChatList>().on_find_number({
        let found = found.clone();
        move |number, open| *found.borrow_mut() = Some((number.to_string(), open))
    });
    window
        .global::<ChatList>()
        .set_search_text("+49 151 23456789".into());
    settle(&window);
    assert!(
        find(&window, "Message +49 151 23456789").is_none(),
        "only once it is a number"
    );
    window.global::<ChatList>().set_search_is_number(true);
    settle(&window);
    find(&window, "Message +49 151 23456789")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(
        *found.borrow(),
        Some(("+49 151 23456789".to_string(), true))
    );
}

#[test]
fn scrolling_to_the_top_asks_for_older_messages() {
    let (window, _messages) = window_with_messages(60);
    window.global::<OpenChat>().set_can_load_older(true);
    let asked = Rc::new(Cell::new(0));
    window.global::<OpenChat>().on_load_older({
        let asked = asked.clone();
        move || asked.set(asked.get() + 1)
    });
    // Opened at the newest messages; nothing older is asked for yet.
    let (newest, _) = newest_shown(&window, 60).expect("messages are shown");
    assert!(newest >= 55, "opened at message {newest}");
    assert_eq!(asked.get(), 0);
    for _ in 0..40 {
        scroll(&window, 400.);
    }
    assert!(find(&window, "Message 0").is_some(), "scrolled to the top");
    assert!(asked.get() > 0);
}

#[test]
fn older_messages_put_in_above_leave_the_view_in_place() {
    let (window, messages) = window_with_messages(60);
    // Away from the bottom, so the list no longer follows new messages.
    for _ in 0..6 {
        scroll(&window, 300.);
    }
    let label = (20..60)
        .map(|n| format!("Message {n}"))
        .find(|label| find(&window, label).is_some())
        .expect("something is shown");
    let before = find(&window, &label).unwrap().absolute_position().y;
    messages.prepend((0..10).map(|n| message(&format!("Older {n}"))).collect());
    settle(&window);
    let after = find(&window, &label)
        .expect("still shown")
        .absolute_position()
        .y;
    assert!(
        (after - before).abs() < 2.,
        "moved from {before} to {after}"
    );
}

#[test]
fn clicking_a_mention_candidate_picks_it() {
    let (window, _messages) = window_with_messages(3);
    let picked = Rc::new(Cell::new(None));
    window.global::<Composer>().on_pick_mention({
        let picked = picked.clone();
        move |index| picked.set(Some(index))
    });
    let names: Vec<SharedString> = vec!["Ann".into(), "Andrea".into()];
    window
        .global::<Composer>()
        .set_mention_candidates(ModelRc::new(VecModel::from(names)));
    settle(&window);
    find(&window, "@Andrea")
        .expect("listed")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(picked.get(), Some(1));
    assert_eq!(
        window
            .global::<Composer>()
            .get_mention_candidates()
            .row_count(),
        2
    );
}

#[test]
fn forwarding_goes_to_the_clicked_chat_once_confirmed() {
    let (window, _messages) = window_with_messages(3);
    let to = Rc::new(std::cell::RefCell::new(None));
    window.global::<Panels>().on_forward_to({
        let to = to.clone();
        move |id| *to.borrow_mut() = Some(id)
    });
    let target = |id: &str, title: &str| ConversationItem {
        id: id.into(),
        title: title.into(),
        ..Default::default()
    };
    window
        .global::<Panels>()
        .set_forward_targets(ModelRc::new(VecModel::from(vec![
            target("b", "Bob"),
            target("c", "Carol"),
        ])));
    window.global::<Panels>().set_forwarding(true);
    settle(&window);
    find(&window, "Carol")
        .expect("listed")
        .mock_single_click(PointerEventButton::Left);
    settle(&window);
    assert_eq!(*to.borrow(), None, "asks first");
    assert!(find(&window, "Forward to Carol?").is_some());
    find(&window, "Forward")
        .expect("confirmable")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(to.borrow().as_deref(), Some("c"));
}

#[test]
fn picked_messages_get_a_bar_to_act_on_them() {
    let (window, _messages) = window_with_messages(3);
    let cleared = Rc::new(Cell::new(false));
    window.global::<OpenChat>().on_clear_selection({
        let cleared = cleared.clone();
        move || cleared.set(true)
    });
    assert!(find(&window, "Clear selection").is_none());
    window.global::<OpenChat>().set_selected_count(2);
    settle(&window);
    assert!(find(&window, "2 messages selected").is_some());
    assert!(
        find(&window, "Forward selected").is_none(),
        "not until all can go"
    );
    window.global::<OpenChat>().set_can_forward_selection(true);
    settle(&window);
    assert!(find(&window, "Forward selected").is_some());
    find(&window, "Clear selection")
        .expect("a way out")
        .mock_single_click(PointerEventButton::Left);
    assert!(cleared.get());
}

#[test]
fn jumping_brings_the_row_into_view() {
    let (window, _messages) = window_with_messages(60);
    assert!(find(&window, "Message 10").is_none());
    window.global::<OpenChat>().set_jump_row(10);
    window.global::<OpenChat>().set_jump_serial(1);
    settle(&window);
    assert!(find(&window, "Message 10").is_some());
}

#[test]
fn clicking_a_search_result_shows_it() {
    let (window, _messages) = window_with_messages(3);
    let shown = Rc::new(Cell::new(None));
    window.global::<Panels>().on_show_result({
        let shown = shown.clone();
        move |index| shown.set(Some(index))
    });
    window.global::<Panels>().set_chat_search_open(true);
    window
        .global::<Panels>()
        .set_chat_search_text("moon".into());
    let result = |text: &str| crate::generated::SearchResult {
        sender: "Ann".into(),
        when: "Oct 2".into(),
        text: text.into(),
        snippet: slint::StyledText::from_plain_text(text),
    };
    window
        .global::<Panels>()
        .set_search_results(ModelRc::new(VecModel::from(vec![
            result("the moon"),
            result("a moon rising"),
        ])));
    settle(&window);
    find(&window, "a moon rising")
        .expect("listed")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(shown.get(), Some(1));
}

#[test]
fn shortcuts_reach_rust_from_the_composer() {
    use slint::platform::{Key, WindowEvent};
    let (window, _messages) = window_with_messages(3);
    let pressed = Rc::new(std::cell::RefCell::new(Vec::new()));
    window.on_shortcut({
        let pressed = pressed.clone();
        move |key, control, alt, shift| {
            pressed
                .borrow_mut()
                .push((key.to_string(), control, alt, shift));
            key == "k"
        }
    });
    // Typing in the composer.
    ElementHandle::find_by_element_id(&window, "AppWindow::composer")
        .next()
        .expect("composer")
        .mock_single_click(PointerEventButton::Left);
    settle(&window);
    let key = |text: SharedString| {
        window
            .window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window
            .window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    };
    let control = SharedString::from(char::from(Key::Control));
    window.window().dispatch_event(WindowEvent::KeyPressed {
        text: control.clone(),
    });
    key("k".into());
    window
        .window()
        .dispatch_event(WindowEvent::KeyReleased { text: control });
    key("x".into());
    settle(&window);
    let pressed = pressed.borrow();
    assert!(pressed.contains(&("k".into(), true, false, false)));
    // Not a shortcut: it reaches the composer.
    assert!(pressed.contains(&("x".into(), false, false, false)));
    assert_eq!(window.global::<Composer>().get_draft(), "x");
}

/// Only the opening: the testing backend doesn't lay out a popup's contents (that happens when
/// it is drawn), so the stickers in it can't be found.
#[test]
fn the_sticker_button_asks_for_the_packs() {
    let (window, _messages) = window_with_messages(3);
    let opened = Rc::new(Cell::new(false));
    window.global::<Composer>().on_open_stickers({
        let opened = opened.clone();
        move || opened.set(true)
    });
    find(&window, "Stickers")
        .expect("sticker button")
        .mock_single_click(PointerEventButton::Left);
    settle(&window);
    assert!(opened.get());
    assert_eq!(
        i_slint_core::window::WindowInner::from_pub(window.window())
            .active_popups()
            .len(),
        1,
        "the picker is open"
    );
}

/// Screen readers hear a chat row as one item that says what matters, and can open it.
#[test]
fn chat_rows_speak_for_themselves() {
    let (window, _messages) = window_with_messages(3);
    let selected = Rc::new(std::cell::RefCell::new(None));
    window.global::<ChatList>().on_select({
        let selected = selected.clone();
        move |id| *selected.borrow_mut() = Some(id)
    });
    window.global::<ChatList>().set_selected(-1);
    window
        .global::<ChatList>()
        .set_selected_id(SharedString::new());
    window
        .global::<ChatList>()
        .set_conversations(ModelRc::new(VecModel::from(vec![ConversationItem {
            id: "b".into(),
            title: "Bob".into(),
            unread: 2,
            muted: true,
            preview: "See you".into(),
            ..Default::default()
        }])));
    settle(&window);
    let row = find(&window, "Bob, 2 unread, muted, See you").expect("one item says it all");
    row.invoke_accessible_default_action();
    assert_eq!(selected.borrow().as_deref(), Some("b"));
}

/// What ui.rs makes of the chat state: here, how far our messages got, and that every one
/// that failed says so, not only a run's last.
#[test]
fn the_window_shows_the_chat_state() {
    use b33p_core::state::{ChatState, Update};
    use b33p_core::{ChatMessage, ChatSettings, Conversation, ConversationId, Event, Unsent};

    i_slint_backend_testing::init_no_event_loop();
    let window = AppWindow::new().unwrap();
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 500.));
    crate::ui::set_up_models(&window);
    let ann = ConversationId::contact("ann");
    let mut chat = ChatState::default();
    chat.apply(Event::Linked {
        account: "me".into(),
    });
    chat.apply(Event::Synced);
    chat.apply(Event::Conversations(vec![Conversation {
        id: ann.clone(),
        title: "Ann".into(),
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    }]));
    chat.select(&ann);
    let ours = |body: &str, timestamp, unsent| ChatMessage {
        conversation: ann.clone(),
        sender_id: "me".into(),
        sender: "You".into(),
        body: body.into(),
        timestamp,
        outgoing: true,
        unsent,
        ..ChatMessage::default()
    };
    let now = b33p_core::now_millis();
    chat.apply(Event::History {
        conversation: ann.clone(),
        messages: vec![
            ours("first try", now - 3000, Some(Unsent::Failed)),
            ours("second try", now - 2000, Some(Unsent::Failed)),
            ours("went out", now - 1000, None),
        ],
        reactions: Vec::new(),
        older: None,
    });
    crate::ui::render(
        &window,
        &chat,
        &crate::Images::default(),
        Update {
            screen: true,
            ..Update::reset()
        },
        true,
    );
    window.show().unwrap();
    settle(&window);

    let delivery: Vec<_> = window
        .global::<OpenChat>()
        .get_messages()
        .iter()
        .map(|m| m.delivery)
        .collect();
    assert_eq!(
        delivery,
        [
            DeliveryState::Failed,
            DeliveryState::Failed,
            DeliveryState::Sent
        ]
    );
    assert_eq!(
        ElementHandle::find_by_accessible_label(&window, "Send again").count(),
        2,
        "each failed message offers a retry"
    );
}

/// The chat list follows events row by row: a chat with a new message moves to the top with
/// its new preview, and the rows it passed move down.
#[test]
fn the_chat_list_follows_events() {
    use b33p_core::state::{ChatState, Update};
    use b33p_core::{ChatMessage, ChatSettings, Conversation, ConversationId, Event};

    i_slint_backend_testing::init_no_event_loop();
    let window = AppWindow::new().unwrap();
    crate::ui::set_up_models(&window);
    let mut chat = ChatState::default();
    chat.apply(Event::Linked {
        account: "me".into(),
    });
    chat.apply(Event::Synced);
    let person = |name: &str| Conversation {
        id: ConversationId::contact(name),
        title: name.into(),
        settings: ChatSettings {
            accepted: true,
            ..ChatSettings::default()
        },
        ..Conversation::default()
    };
    chat.apply(Event::Conversations(vec![
        person("ann"),
        person("bob"),
        person("cid"),
    ]));
    let from = |name: &str, body: &str, timestamp| {
        Event::Message(ChatMessage {
            conversation: ConversationId::contact(name),
            sender_id: name.into(),
            sender: name.into(),
            body: body.into(),
            timestamp,
            ..ChatMessage::default()
        })
    };
    chat.apply_all([
        from("ann", "a", 3),
        from("bob", "b", 2),
        from("cid", "c", 1),
    ]);
    let images = crate::Images::default();
    crate::ui::render(&window, &chat, &images, Update::reset(), true);
    let rows = || -> Vec<(String, String)> {
        window
            .global::<ChatList>()
            .get_conversations()
            .iter()
            .map(|c| (c.title.to_string(), c.preview.to_string()))
            .collect()
    };
    assert_eq!(rows()[0].0, "ann");

    let update = chat.apply(from("cid", "newest", 4));
    crate::ui::render(&window, &chat, &images, update, false);
    let titles: Vec<_> = rows().into_iter().map(|(title, _)| title).collect();
    assert_eq!(titles, ["cid", "ann", "bob"]);
    assert!(rows()[0].1.contains("newest"), "{:?}", rows()[0]);
}

/// A failure has its own screen, with what can be done about it; deleting the data asks first.
#[test]
fn a_failure_offers_a_way_out() {
    use b33p_core::state::{ChatState, Update};
    use b33p_core::{Event, Failure};

    i_slint_backend_testing::init_no_event_loop();
    let window = AppWindow::new().unwrap();
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 500.));
    crate::ui::set_up_models(&window);
    let mut chat = ChatState::default();
    chat.apply(Event::Failed {
        failure: Failure::KeyMissing,
        message: "The key is missing.".into(),
    });
    crate::ui::render(
        &window,
        &chat,
        &crate::Images::default(),
        Update {
            screen: true,
            ..Update::reset()
        },
        true,
    );
    let retried = Rc::new(Cell::new(false));
    window.global::<Account>().on_retry({
        let retried = retried.clone();
        move || retried.set(true)
    });
    let started_over = Rc::new(Cell::new(false));
    window.global::<Account>().on_start_over({
        let started_over = started_over.clone();
        move || started_over.set(true)
    });
    window.show().unwrap();
    settle(&window);

    assert_eq!(window.global::<Account>().get_screen(), Screen::Failed);
    assert!(find(&window, "The key is missing.").is_some());
    assert!(
        find(&window, "Use a password instead").is_none(),
        "only for a locked keyring"
    );
    find(&window, "Try again")
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert!(retried.get());

    find(&window, "Start over…")
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    settle(&window);
    assert!(!started_over.get(), "not before it is confirmed");
    find(&window, "Delete and start over")
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    settle(&window);
    assert!(started_over.get());
    assert!(find(&window, "Delete and start over").is_none());
}

/// A long error wraps in the banner instead of being cut off after a line.
#[test]
fn a_long_error_wraps() {
    let (window, _) = window_with_messages(1);
    let long = "This device was removed from your Signal account. Scan the code to link it \
                again; the messages here are kept.";
    window
        .window()
        .set_size(slint::LogicalSize::new(400., 500.));
    window.global::<Account>().set_error(long.into());
    settle(&window);
    let text = find(&window, long).expect("the banner shows it");
    assert!(text.size().height > 30., "one line only: {:?}", text.size());
}

/// The error banner offers to try again only when that may work.
#[test]
fn the_error_offers_to_try_again_when_that_may_work() {
    let (window, _) = window_with_messages(1);
    let account = window.global::<Account>();
    account.set_error("Couldn't reach Signal.".into());
    settle(&window);
    assert!(find(&window, "Dismiss").is_some(), "the banner shows");
    assert!(find(&window, "Try again").is_none());

    account.set_can_retry_error(true);
    let retried = Rc::new(Cell::new(false));
    account.on_retry_error({
        let retried = retried.clone();
        move || retried.set(true)
    });
    settle(&window);
    find(&window, "Try again")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert!(retried.get());
}

/// Settings lead to the issue tracker.
#[test]
fn settings_say_where_to_report_a_problem() {
    let (window, _) = window_with_messages(1);
    // Tall enough for the whole page: what is scrolled away isn't there to find.
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 4000.));
    window.global::<Prefs>().set_settings_open(true);
    settle(&window);
    let opened = Rc::new(std::cell::RefCell::new(String::new()));
    window.global::<Account>().on_open_url({
        let opened = opened.clone();
        move |url| *opened.borrow_mut() = url.into()
    });
    find(&window, "Report…")
        .expect("a button to report a problem")
        .invoke_accessible_default_action();
    assert!(
        opened.borrow().ends_with("/issues/new"),
        "{}",
        opened.borrow()
    );
}

/// A button that is only an icon says what it does when the pointer rests on it.
#[test]
fn icon_buttons_have_tooltips() {
    use slint::platform::WindowEvent;
    let (window, _) = window_with_messages(1);
    let shown = |window: &AppWindow| {
        i_slint_core::window::WindowInner::from_pub(window.window())
            .active_popups
            .borrow()
            .len()
    };
    let button = find(&window, "Search this chat").expect("the chat header's button");
    let at = button.absolute_position();
    let size = button.size();
    let centre = slint::LogicalPosition::new(at.x + size.width / 2., at.y + size.height / 2.);
    assert_eq!(shown(&window), 0);
    window
        .window()
        .dispatch_event(WindowEvent::PointerMoved { position: centre });
    settle(&window);
    // Resting there, moving a little, as a hand does.
    window.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(centre.x + 1., centre.y),
    });
    for _ in 0..10 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(200));
        slint::platform::update_timers_and_animations();
    }
    assert!(shown(&window) > 0, "a tooltip shows");
}

/// An emoji goes where the caret is, in place of what is selected, and the caret after it;
/// offsets inside a character or past the end don't split or overrun the draft.
#[test]
fn an_emoji_goes_where_the_caret_is() {
    use crate::ui::splice;
    assert_eq!(splice("Hi you", "👋", 2, 2), ("Hi👋 you".into(), 6));
    assert_eq!(splice("Hi you", "👋", 3, 6), ("Hi 👋".into(), 7));
    assert_eq!(splice("Hi you", "👋", 6, 3), ("Hi 👋".into(), 7));
    assert_eq!(splice("", "👋", 0, 0), ("👋".into(), 4));
    assert_eq!(splice("é", "x", 1, 1), ("xé".into(), 1));
    assert_eq!(splice("ab", "x", 99, 99), ("abx".into(), 3));
    assert_eq!(splice("ab", "x", -5, 1), ("xb".into(), 1));
}
