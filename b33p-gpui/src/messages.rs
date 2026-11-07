//! The open chat's messages: the virtual list of rows, and the keys that are the messages' own.
//!
//! A view of its own, drawn cached: GPUI draws the whole window for any change, and runs every
//! view's `render` again unless the view is cached and wasn't notified. The messages are the
//! costliest to build (every row on screen is laid out and shaped again), and typing in the
//! composer, the caret's blink or a hover elsewhere change none of them.
//!
//! What a cached view shows is replayed from the last frame until it is notified, so it has to
//! be notified by everything that changes what it reads that GPUI doesn't tell it about: the
//! scrolling ([`Shared::scroller`]) and the text selected ([`TextSelection`]). The chat state
//! changes through [`Shared::flush`], which refreshes the window, and a refresh ignores the
//! caches.

use std::rc::Rc;

use gpui_kit::component::message_scroller::MessageScroller;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, FocusHandle, IntoElement, MouseButton, Render, StyleRefinement,
    Subscription, Window, div, px,
};

use crate::actions::{CloseFront, CopyMessages};
use crate::bubble;
use crate::message_menu;
use crate::shared::Shared;
use crate::text::TextSelection;

pub struct MessagesView {
    shared: Rc<Shared>,
    /// Has the keys while text of a message is selected, so that Ctrl+C copies it.
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl MessagesView {
    pub fn new(shared: Rc<Shared>, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        cx.default_global::<TextSelection>().focus = Some(focus.clone());
        let subscriptions = vec![
            // The scrolling moves the rows, which the cached view would otherwise not redraw.
            cx.observe(&shared.scroller, |_, _, cx| cx.notify()),
            // Selecting text, or dropping the selection, repaints the text.
            cx.observe_global::<TextSelection>(|_, cx| cx.notify()),
        ];
        Self {
            shared,
            focus,
            _subscriptions: subscriptions,
        }
    }

    /// How the view is laid out when it is not drawn anew: the whole space it is given.
    pub fn layout() -> StyleRefinement {
        StyleRefinement::default().size_full()
    }
}

impl Render for MessagesView {
    /// The messages, a list that draws only the rows on screen.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shared = self.shared.clone();
        // Messages that came while scrolled away from the end are counted for the button that
        // goes there; scrolled to it again, they are seen.
        if !shared.scroller.read(cx).is_scrolled_up() {
            shared.unseen.set(0);
        }
        let unseen = shared.unseen.get();
        let list = MessageScroller::new("messages", shared.scroller.clone(), {
            let shared = shared.clone();
            move |index, _window, cx| bubble::row(index, &shared, cx)
        })
        // The rows carry their own spacing: tight within a run, a little more between runs.
        .with_row_style(StyleRefinement::default().pb(px(0.)).px(px(0.)))
        .with_jump_button_label(if unseen > 0 {
            format!("Newest messages, {unseen} new")
        } else {
            "Newest messages".to_owned()
        })
        .with_jump_button_renderer(move |button| {
            if unseen > 0 {
                button.label(unseen.to_string())
            } else {
                button
            }
        });
        div()
            .id("messages-area")
            .debug_selector(|| "Messages".into())
            .size_full()
            .key_context("Messages")
            .track_focus(&self.focus)
            // Keys that are the messages' own while they have the focus: Ctrl+C copies the text
            // selected, or else the picked messages; Esc drops the selection of text, and
            // otherwise is for whatever is open.
            .on_action(cx.listener(|this, _: &CopyMessages, _, cx| {
                if TextSelection::copy(cx) {
                    return;
                }
                let text = this.shared.chat.borrow().selected_text();
                if text.is_empty() {
                    cx.propagate();
                } else {
                    message_menu::copy(text, cx);
                    this.shared.redraw(cx, |chat| chat.clear_selection());
                }
            }))
            .on_action(cx.listener(|_, _: &CloseFront, _, cx| {
                if TextSelection::clear(cx) {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            // A click on the messages that isn't on text drops the selection of text, and
            // leaves the composer its focus.
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                if TextSelection::clear(cx) {
                    window.refresh();
                }
            })
            .child(list)
    }
}

/// The messages as a child of the conversation: cached, see the module's documentation.
pub fn element(view: &gpui_kit::Entity<MessagesView>) -> AnyElement {
    view.clone()
        .cached(MessagesView::layout())
        .into_any_element()
}
