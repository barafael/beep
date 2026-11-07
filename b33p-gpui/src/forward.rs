//! Choosing where to forward a message to, over everything else; then confirming it, since it
//! goes out at once.

use std::rc::Rc;

use b33p_core::ConversationId;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription,
    UniformListScrollHandle, Window, div, px,
};

use crate::dialog::{self, Person};
use crate::shared::Shared;
use crate::theme;
use crate::widgets::modal;

pub struct ForwardView {
    shared: Rc<Shared>,
    filter: Entity<InputState>,
    scroll: UniformListScrollHandle,
    /// Where it is to go, once picked: asked about before it does.
    picked: Option<(ConversationId, String)>,
    /// Takes Enter while confirming.
    confirm_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ForwardView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        filter.update(cx, |filter, cx| filter.focus(window, cx));
        let subscription = cx.subscribe_in(
            &filter,
            window,
            |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => cx.notify(),
                    // Enter picks the first chat found.
                    InputEvent::PressEnter { .. } => {
                        let text = input.read(cx).value().to_string();
                        let first = this
                            .shared
                            .chat
                            .borrow()
                            .forward_targets(&text)
                            .first()
                            .map(|c| (c.id.clone(), c.title.clone()));
                        if let Some((id, title)) = first {
                            this.pick(id, title, window, cx);
                        }
                    }
                    _ => {}
                }
            },
        );
        let confirm_focus = cx.focus_handle();
        Self {
            shared,
            filter,
            scroll: UniformListScrollHandle::new(),
            picked: None,
            confirm_focus,
            _subscriptions: vec![subscription],
        }
    }

    fn pick(
        &mut self,
        id: ConversationId,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picked = Some((id, title));
        self.confirm_focus.focus(window, cx);
        cx.notify();
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picked = None;
        self.filter
            .update(cx, |filter, cx| filter.focus(window, cx));
        cx.notify();
    }

    fn forward(&mut self, cx: &mut Context<Self>) {
        if let Some((id, _)) = self.picked.take() {
            self.shared.command(cx, |chat| chat.forward_to(&id));
        }
    }
}

impl Focusable for ForwardView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.confirm_focus.clone()
    }
}

impl Render for ForwardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let cancel = {
            let shared = shared.clone();
            move |_: &mut Window, cx: &mut App| shared.edit(cx, |chat| chat.cancel_forward())
        };
        let content = match self.picked.clone() {
            None => {
                let filter = self.filter.read(cx).value().to_string();
                let rows: Vec<Person> = {
                    let chat = shared.chat.borrow();
                    chat.forward_targets(&filter)
                        .into_iter()
                        .map(|c| Person::of(c, &chat, &shared, None))
                        .collect()
                };
                let view = cx.entity().downgrade();
                let list =
                    dialog::people("forward-targets", rows, &self.scroll, &t, "Forward to", {
                        let shared = shared.clone();
                        move |id: &ConversationId, window, cx| {
                            let title = shared
                                .chat
                                .borrow()
                                .conversation(id)
                                .map(|c| c.title.clone());
                            if let Some(title) = title {
                                let id = id.clone();
                                if view
                                    .update(cx, |this, cx| this.pick(id, title, window, cx))
                                    .is_err()
                                {
                                    tracing::warn!(
                                        "the forward dialog closed before a click on it"
                                    );
                                }
                            }
                        }
                    });
                dialog::layout("Forward to…")
                    .h(px(dialog::height(window, 520.)))
                    .child(Input::new(&self.filter).cleanable(true))
                    .child(list)
                    .child(
                        dialog::buttons().child(
                            Button::new("forward-cancel")
                                .label("Cancel")
                                .debug_selector(|| "Cancel".into())
                                .on_click({
                                    let cancel = cancel.clone();
                                    move |_, window, cx| cancel(window, cx)
                                }),
                        ),
                    )
                    .into_any_element()
            }
            Some((_, title)) => {
                let preview = shared.chat.borrow().forwarded().unwrap_or_default();
                div()
                    .track_focus(&self.confirm_focus)
                    .key_context("ForwardConfirm")
                    // Enter forwards, as the primary button does.
                    .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                        if event.keystroke.key == "enter" {
                            this.forward(cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        dialog::layout(format!("Forward to {title}?"))
                            .child(quote(&t, preview))
                            .child(
                                dialog::buttons()
                                    .child(
                                        Button::new("forward-back")
                                            .label("Back")
                                            .debug_selector(|| "Back".into())
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.back(window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("forward-send")
                                            .primary()
                                            .label("Forward")
                                            .debug_selector(|| "Forward".into())
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.forward(cx)),
                                            ),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        };
        modal(&t, 420., content, cancel)
    }
}

/// The message being forwarded, as a quote.
fn quote(t: &theme::Tokens, text: String) -> gpui_kit::Div {
    div()
        .flex()
        .flex_row()
        .rounded(px(theme::RADIUS_S))
        .bg(t.hover)
        .overflow_hidden()
        .child(div().flex_none().w(px(3.)).bg(t.accent))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .max_h(px(theme::BODY * 7.))
                .overflow_hidden()
                .py(px(10.))
                .pl(px(10.))
                .pr(px(10.))
                .text_color(t.text_secondary)
                .child(text),
        )
}
