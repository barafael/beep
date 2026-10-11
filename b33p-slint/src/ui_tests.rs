//! The window's own behaviour, on Slint's headless testing backend: what the Slint code does
//! with the properties Rust sets and the input it gets. (The fluent style, since the native one
//! needs Qt.)

use std::cell::Cell;
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::generated::{AppWindow, ConversationItem, MessageItem, Screen};

fn window_with_messages(count: usize) -> (AppWindow, Rc<VecModel<MessageItem>>) {
    i_slint_backend_testing::init_no_event_loop();
    let window = AppWindow::new().unwrap();
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 500.));
    window.set_screen(Screen::Chats);
    let ann = ConversationItem {
        id: "a".into(),
        title: "Ann".into(),
        ..Default::default()
    };
    window.set_conversations(ModelRc::new(VecModel::from(vec![ann.clone()])));
    window.set_selected(0);
    window.set_selected_id("a".into());
    window.set_current(ann);
    let messages = Rc::new(VecModel::from(
        (0..count)
            .map(|n| message(&format!("Message {n}")))
            .collect::<Vec<_>>(),
    ));
    window.set_messages(messages.clone().into());
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
    messages.push(MessageItem {
        notice: true,
        ..message(text)
    });
    messages.push(message("After"));
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
#[test]
fn the_safety_number_can_be_marked_verified() {
    let (window, _messages) = window_with_messages(1);
    let asked = Rc::new(Cell::new(None));
    window.on_set_verified({
        let asked = asked.clone();
        move |verified| asked.set(Some(verified))
    });
    // Tall enough for the whole details page: the panel makes only what shows.
    window
        .window()
        .set_size(slint::LogicalSize::new(800., 1200.));
    window.set_details_open(true);
    window.set_details_safety_number("12345 67890".into());
    settle(&window);
    assert!(
        find(&window, "Copy").is_some(),
        "the safety number's buttons show"
    );
    assert!(find(&window, "✓ Verified: you compared it with theirs.").is_none());
    find(&window, "Mark as verified")
        .expect("offered")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(asked.get(), Some(true));

    window.set_details_verified(true);
    settle(&window);
    assert!(find(&window, "✓ Verified: you compared it with theirs.").is_some());
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
    window.on_confirm_safety_number({
        let confirmed = confirmed.clone();
        move |id| *confirmed.borrow_mut() = Some(id)
    });
    let opened = Rc::new(Cell::new(false));
    window.on_open_details({
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
    window.set_conversations(ModelRc::new(VecModel::from(vec![changed.clone()])));
    window.set_current(changed);
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
    window.on_answer_invitation({
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
    window.set_conversations(ModelRc::new(VecModel::from(vec![club.clone()])));
    window.set_selected_id(club.id.clone());
    window.set_current(club);
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
    window.on_find_number({
        let found = found.clone();
        move |number, open| *found.borrow_mut() = Some((number.to_string(), open))
    });
    window.set_search_text("+49 151 23456789".into());
    settle(&window);
    assert!(
        find(&window, "Message +49 151 23456789").is_none(),
        "only once it is a number"
    );
    window.set_search_is_number(true);
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
    let asked = Rc::new(Cell::new(0));
    window.on_load_older({
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
    for n in 0..10 {
        messages.insert(n, message(&format!("Older {n}")));
    }
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
    window.on_pick_mention({
        let picked = picked.clone();
        move |index| picked.set(Some(index))
    });
    let names: Vec<SharedString> = vec!["Ann".into(), "Andrea".into()];
    window.set_mention_candidates(ModelRc::new(VecModel::from(names)));
    settle(&window);
    find(&window, "@Andrea")
        .expect("listed")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(picked.get(), Some(1));
    assert_eq!(window.get_mention_candidates().row_count(), 2);
}

#[test]
fn forwarding_goes_to_the_clicked_chat_once_confirmed() {
    let (window, _messages) = window_with_messages(3);
    let to = Rc::new(std::cell::RefCell::new(None));
    window.on_forward_to({
        let to = to.clone();
        move |id| *to.borrow_mut() = Some(id)
    });
    let target = |id: &str, title: &str| ConversationItem {
        id: id.into(),
        title: title.into(),
        ..Default::default()
    };
    window.set_forward_targets(ModelRc::new(VecModel::from(vec![
        target("b", "Bob"),
        target("c", "Carol"),
    ])));
    window.set_forwarding(true);
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
    window.on_clear_selection({
        let cleared = cleared.clone();
        move || cleared.set(true)
    });
    assert!(find(&window, "Clear selection").is_none());
    window.set_selected_count(2);
    settle(&window);
    assert!(find(&window, "2 messages selected").is_some());
    assert!(
        find(&window, "Forward selected").is_none(),
        "not until all can go"
    );
    window.set_can_forward_selection(true);
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
    window.set_jump_row(10);
    window.set_jump_serial(1);
    settle(&window);
    assert!(find(&window, "Message 10").is_some());
}

#[test]
fn clicking_a_search_result_shows_it() {
    let (window, _messages) = window_with_messages(3);
    let shown = Rc::new(Cell::new(None));
    window.on_show_result({
        let shown = shown.clone();
        move |index| shown.set(Some(index))
    });
    window.set_chat_search_open(true);
    window.set_chat_search_text("moon".into());
    let result = |text: &str| crate::generated::SearchResult {
        sender: "Ann".into(),
        when: "Oct 2".into(),
        text: text.into(),
        snippet: slint::StyledText::from_plain_text(text),
    };
    window.set_search_results(ModelRc::new(VecModel::from(vec![
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
    assert_eq!(window.get_draft(), "x");
}

/// Only the opening: the testing backend doesn't lay out a popup's contents (that happens when
/// it is drawn), so the stickers in it can't be found.
#[test]
fn the_sticker_button_asks_for_the_packs() {
    let (window, _messages) = window_with_messages(3);
    let opened = Rc::new(Cell::new(false));
    window.on_open_stickers({
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
    window.on_select({
        let selected = selected.clone();
        move |id| *selected.borrow_mut() = Some(id)
    });
    window.set_selected(-1);
    window.set_selected_id(SharedString::new());
    window.set_conversations(ModelRc::new(VecModel::from(vec![ConversationItem {
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
