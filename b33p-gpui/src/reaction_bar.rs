//! The quick reactions that come over a message once the pointer has rested on it a moment, as
//! `ReactionPicker` in `b33p-slint/ui/bubble.slint` does: a bar of six emoji and "+" (every
//! other emoji), straddling the bubble's top edge so that the pointer can move up onto it
//! without ever being off both. It goes when the pointer leaves the message and the bar.
//!
//! The bar is drawn for one message at a time and made only while it shows. The only thing that
//! ticks is the dwell timer, once, when the pointer arrives on a message; nothing here redraws
//! the window at rest.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::{Anchor, AnyElement, App, FontWeight, Role, SharedString, Window, div, px};

use crate::message_menu::{self, QUICK};
use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, icon};

/// How long the pointer rests on a message before the bar comes (not while it merely crosses
/// the list): Slint's 350 ms.
pub const DWELL: Duration = Duration::from_millis(350);

/// How tall the bar is, and how far it reaches over the bubble's top edge.
const HEIGHT: f32 = 40.;
const OVERLAP: f32 = 6.;

/// What the bar knows: which message it is shown for, and whether the pointer is on it.
#[derive(Default)]
pub struct State {
    /// The message whose bar is shown.
    shown: Cell<Option<usize>>,
    /// The pointer is on the bar, which is outside the message's own row and so is not the row
    /// being hovered.
    over_bar: Cell<bool>,
    /// The picker of every emoji, opened from the bar's "+", is open for this message: it
    /// belongs to the bar, which stays as long as it does.
    full: Cell<Option<usize>>,
    /// Counts the arrivals of the pointer on a message, so that a dwell timer started by an
    /// earlier one knows it is not the latest.
    arrivals: Cell<u64>,
}

impl State {
    /// Rows have other numbers after this (a chat opened, older messages put in above).
    pub fn reset(&self) {
        self.shown.set(None);
        self.over_bar.set(false);
        self.full.set(None);
        self.arrivals.set(self.arrivals.get() + 1);
    }

    pub fn shown_for(&self, index: usize) -> bool {
        self.shown.get() == Some(index)
    }
}

/// Whether the bar of `index` stays: the pointer is on the message or the bar, or the picker
/// opened from the bar is open.
fn stays(shared: &Shared, index: usize) -> bool {
    shared.hovered_row.get() == Some(index)
        || shared.reaction_bar.over_bar.get()
        || shared.reaction_bar.full.get() == Some(index)
}

/// The pointer came onto the message `index`, or left it. Called by the message's row, which
/// says whether a reaction can be made to it at all.
pub fn hover(
    shared: &Rc<Shared>,
    index: usize,
    hovered: bool,
    can_react: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let state = &shared.reaction_bar;
    // Whatever timer ran for an earlier arrival is no longer wanted.
    let arrival = state.arrivals.get() + 1;
    state.arrivals.set(arrival);
    if hovered {
        if !can_react || state.shown_for(index) {
            return;
        }
        let shared = shared.clone();
        window
            .spawn(cx, async move |cx| {
                cx.background_executor().timer(DWELL).await;
                cx.update(|window, _| {
                    let state = &shared.reaction_bar;
                    if state.arrivals.get() == arrival && shared.hovered_row.get() == Some(index) {
                        state.shown.set(Some(index));
                        window.refresh();
                    }
                })
                .ok();
            })
            .detach();
    } else {
        settle(shared, index, window, cx);
    }
}

/// Takes the bar away if the pointer is off the message, the bar and the picker. Done once the
/// event that moved the pointer is over: it can have left the row and reached the bar in one
/// move, and the row learns of the first before the bar learns of the second.
fn settle(shared: &Rc<Shared>, index: usize, window: &mut Window, cx: &mut App) {
    let shared = shared.clone();
    window.defer(cx, move |window, _| {
        if shared.reaction_bar.shown_for(index) && !stays(&shared, index) {
            shared.reaction_bar.shown.set(None);
            window.refresh();
        }
    });
}

/// A reaction was chosen: the bar is gone with the pointer still on it, so it won't say it left.
fn done(shared: &Shared, window: &mut Window) {
    shared.reaction_bar.reset();
    window.refresh();
}

/// The bar over the bubble of message `index`, if it is shown for it. `outgoing` messages sit at
/// the right, and the bar with them. Put it in the box that wraps the bubble, which must be
/// positioned (`relative`).
pub fn bar(
    shared: &Rc<Shared>,
    index: usize,
    outgoing: bool,
    can_react: bool,
    t: &Tokens,
) -> Option<AnyElement> {
    if !can_react || !shared.reaction_bar.shown_for(index) {
        return None;
    }
    let cell = |emoji: &'static str| {
        let shared = shared.clone();
        div()
            .id(SharedString::from(format!("bar-{index}-{emoji}")))
            .flex_none()
            .size(px(theme::BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .hover(move |s| s.bg(t.pressed))
            .speaks(Role::Button, format!("React with {emoji}"))
            .text_size(px(theme::BODY * theme::EMOJI))
            .font_weight(FontWeight::NORMAL)
            .child(SharedString::from(emoji))
            .on_click(move |_, window, cx| {
                message_menu::react(&shared, index, emoji, cx);
                done(&shared, window);
            })
    };
    let more = {
        let (opened, closed) = (shared.clone(), shared.clone());
        Popover::new(SharedString::from(format!("bar-more-{index}")))
            // Opens away from the bubble, towards the middle of the window.
            .anchor(if outgoing {
                Anchor::BottomRight
            } else {
                Anchor::BottomLeft
            })
            .trigger(
                Button::new(SharedString::from(format!("bar-more-button-{index}")))
                    .ghost()
                    .small()
                    .rounded_full()
                    .icon(icon("plus"))
                    .tooltip("More reactions")
                    .debug_selector(|| "More reactions".into()),
            )
            .on_open_change(move |open, window, cx| {
                let state = &opened.reaction_bar;
                state.full.set(open.then_some(index));
                if !*open {
                    // A reaction picked in it has already taken the bar away.
                    settle(&closed, index, window, cx);
                }
            })
            .content({
                let shared = shared.clone();
                move |_, window, cx| message_menu::picker(&shared, index, true, window, cx)
            })
    };
    let over = shared.clone();
    Some(
        div()
            .id(SharedString::from(format!("reaction-bar-{index}")))
            .debug_selector(|| "Reactions".into())
            .absolute()
            .top(px(OVERLAP - HEIGHT))
            .when(outgoing, |bar| bar.right_0())
            .when(!outgoing, |bar| bar.left_0())
            .h(px(HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .p(px(theme::SPACE_XS))
            .rounded_full()
            .bg(t.surface_raised)
            .border_1()
            .border_color(t.divider)
            .shadow_md()
            // Over the message above: nothing under the bar answers the pointer.
            .occlude()
            .on_hover(move |hovered, window, cx| {
                over.reaction_bar.over_bar.set(*hovered);
                if !*hovered {
                    settle(&over, index, window, cx);
                }
            })
            .children(QUICK.iter().map(|emoji| cell(emoji)))
            .child(more)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use b33p_core::state::ChatState;
    use b33p_core::{ChatMessage, Command, ConversationId, Event};
    use gpui::TestAppContext;

    use super::DWELL;
    use crate::testing::{self, Ui};

    fn chat_with(messages: &[&str]) -> ChatState {
        let mut chat = testing::chats(&["Ann"]);
        let ann = ConversationId::contact("ann");
        chat.select(&ann);
        chat.apply(Event::History {
            conversation: ann,
            messages: messages
                .iter()
                .enumerate()
                .map(|(n, body)| ChatMessage {
                    conversation: ConversationId::contact("ann"),
                    sender_id: "ann".into(),
                    sender: "Ann".into(),
                    body: (*body).into(),
                    timestamp: 1_700_000_000_000 + n as u64 * 1000,
                    ..ChatMessage::default()
                })
                .collect(),
            reactions: Vec::new(),
            older: None,
        });
        chat
    }

    /// The quick reactions come only once the pointer has rested on a message, go with the
    /// pointer, and a click on one reacts.
    #[gpui::test]
    async fn the_reaction_bar_comes_after_a_dwell(tc: &mut TestAppContext) {
        let mut ui = Ui::new(tc, chat_with(&["one", "two", "three"]));
        assert!(!ui.shown("Reactions"));
        ui.hover("Message row 1");
        assert!(!ui.shown("Reactions"), "not at once");
        ui.wait(DWELL / 2);
        assert!(!ui.shown("Reactions"), "not before the dwell is over");
        ui.wait(DWELL);
        assert!(ui.shown("Reactions"), "after it");
        assert!(ui.shown("React with \u{1F44D}"));
        assert!(ui.shown("More reactions"));

        // Onto the bar, which is over the message above: it stays.
        let bar = ui.bounds("Reactions").expect("the bar is drawn");
        ui.cx
            .simulate_mouse_move(bar.center(), None, gpui::Modifiers::none());
        ui.wait(DWELL);
        assert!(ui.shown("Reactions"), "the pointer is on it");

        ui.click("React with \u{1F44D}");
        assert!(!ui.shown("Reactions"), "a reaction closes it");
        assert!(
            ui.sent().iter().any(|command| matches!(
                command,
                Command::React { emoji, .. } if emoji == "\u{1F44D}"
            )),
            "and is sent"
        );
    }

    /// A pointer that only crosses the list brings nothing, and one that leaves takes it away.
    #[gpui::test]
    async fn the_reaction_bar_goes_with_the_pointer(tc: &mut TestAppContext) {
        let mut ui = Ui::new(tc, chat_with(&["one", "two", "three"]));
        ui.hover("Message row 2");
        ui.wait(DWELL / 3);
        ui.hover("Message row 1");
        ui.wait(DWELL / 3);
        ui.hover("Message row 0");
        ui.wait(DWELL / 3);
        assert!(
            !ui.shown("Reactions"),
            "a pointer crossing messages brings nothing"
        );
        ui.wait(DWELL);
        assert!(ui.shown("Reactions"), "it rests on one");
        ui.cx.simulate_mouse_move(
            gpui::point(gpui::px(100.), gpui::px(300.)),
            None,
            gpui::Modifiers::none(),
        );
        ui.settle();
        assert!(!ui.shown("Reactions"), "gone once the pointer is away");
    }
}
