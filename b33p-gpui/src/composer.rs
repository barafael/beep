//! Where the message is written.

use std::rc::Rc;

use b33p_core::ConversationId;
use b33p_core::state::Update;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, Window, div, px};

use crate::shared::Shared;
use crate::theme;
use crate::widgets::icon;

pub struct ComposerView {
    shared: Rc<Shared>,
    pub input: Entity<TextareaState>,
    /// The chat whose draft the field shows, and whether it was an edit that it shows.
    showing: Option<(ConversationId, bool)>,
    _subscriptions: Vec<Subscription>,
}

impl ComposerView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .placeholder("Message")
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe_in(&input, window, Self::input_event);
        *shared.composer.borrow_mut() = Some(cx.weak_entity());
        Self {
            shared,
            input,
            showing: None,
            _subscriptions: vec![subscription],
        }
    }

    fn input_event(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let text = input.read(cx).value().to_string();
                let command = self.shared.chat.borrow_mut().draft_changed(&text);
                if let Some(command) = command {
                    self.shared.commands.send(command);
                }
                cx.notify();
            }
            InputEvent::PressEnter { shift: false, .. } => {
                self.send(window, cx);
            }
            _ => {}
        }
    }

    /// Puts `text` in the field.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Sends what is written: during an edit that is the edit, which the state keeps out of
    /// the chat's draft. The field is then empty, or after an edit holds what was being written
    /// before it.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).value().to_string();
        let sent = self.shared.act(cx, |chat| chat.compose(&text));
        if sent {
            let draft = self.shared.chat.borrow().draft().to_owned();
            self.input
                .update(cx, |input, cx| input.set_value(draft, window, cx));
            self.shared.refresh(Update::default(), cx);
        }
        sent
    }
}

impl Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        // Another chat's draft, or an edit that ended without being sent (its message was
        // deleted, here or elsewhere): the field follows the state.
        let (now, text) = {
            let chat = self.shared.chat.borrow();
            let now = chat
                .selected()
                .map(|c| (c.id.clone(), chat.editing().is_some()));
            (now, chat.draft().to_owned())
        };
        if now != self.showing {
            let editing_ended = matches!(
                (&self.showing, &now),
                (Some((a, true)), Some((b, false))) if a == b
            );
            let other_chat = self.showing.as_ref().map(|s| &s.0) != now.as_ref().map(|n| &n.0);
            if other_chat || editing_ended {
                self.input
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            }
            self.showing = now;
        }
        let can_send = {
            let chat = self.shared.chat.borrow();
            chat.can_compose(&self.input.read(cx).value())
        };
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_end()
            .gap(px(theme::SPACE_S))
            .p(px(theme::SPACE_M))
            .border_t_1()
            .border_color(t.divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(theme::SPACE_M))
                    .py(px(theme::SPACE_S))
                    .rounded(px(theme::RADIUS_L))
                    .bg(t.field)
                    .child(Textarea::new(&self.input)),
            )
            .child(
                Button::new("send")
                    .primary()
                    .icon(icon("send"))
                    .tooltip("Send")
                    .disabled(!can_send)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.send(window, cx);
                    })),
            )
    }
}
