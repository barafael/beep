//! Choosing people for a new group, or to add to the open one.

use std::rc::Rc;

use b33p_core::state;
use b33p_core::{AfterFinding, ConversationId};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, IntoElement, Render, Subscription, UniformListScrollHandle, Window, div, px,
};

use crate::dialog::{self, Person};
use crate::shared::Shared;
use crate::theme;
use crate::widgets::modal_holding;

pub struct GroupDraftView {
    shared: Rc<Shared>,
    name: Entity<InputState>,
    filter: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl GroupDraftView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let new_group = shared
            .chat
            .borrow()
            .group_draft()
            .is_some_and(|(new_group, _)| new_group);
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search contacts, or type a phone number")
        });
        if new_group {
            name.update(cx, |name, cx| name.focus(window, cx));
        } else {
            filter.update(cx, |filter, cx| filter.focus(window, cx));
        }
        let subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.finish(cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &filter,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.look_up_if_number(cx),
                    _ => {}
                },
            ),
        ];
        Self {
            shared,
            name,
            filter,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Whether "Create" or "Add" can be pressed: a new group needs a name, adding needs someone.
    fn can_finish(&self, cx: &gpui_kit::App) -> bool {
        match self.shared.chat.borrow().group_draft() {
            Some((true, _)) => !self.name.read(cx).value().trim().is_empty(),
            Some((false, chosen)) => !chosen.is_empty(),
            None => false,
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if !self.can_finish(cx) {
            return;
        }
        let name = self.name.read(cx).value().to_string();
        self.shared
            .command(cx, |chat| chat.finish_group_draft(&name));
    }

    /// A typed phone number that no contact matches is looked up on Signal, to choose whoever it
    /// belongs to.
    fn look_up_if_number(&mut self, cx: &mut Context<Self>) {
        let text = self.filter.read(cx).value().to_string();
        let nobody = self.shared.chat.borrow().group_candidates(&text).is_empty();
        if nobody && state::looks_like_number(&text) {
            self.find_number(&text, cx);
        }
    }

    fn find_number(&self, number: &str, cx: &mut Context<Self>) {
        self.shared
            .command(cx, |chat| chat.find_number(number, AfterFinding::Choose));
    }
}

impl Render for GroupDraftView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let filter_text = self.filter.read(cx).value().to_string();
        let is_number = state::looks_like_number(&filter_text);
        let (new_group, rows) = {
            let chat = shared.chat.borrow();
            let Some((new_group, chosen)) = chat.group_draft() else {
                return div().into_any_element();
            };
            let rows: Vec<Person> = chat
                .group_candidates(&filter_text)
                .into_iter()
                .map(|c| Person::of(c, &chat, &shared, Some(chosen.contains(&c.id))))
                .collect();
            (new_group, rows)
        };
        let can_finish = self.can_finish(cx);
        let list = dialog::people("candidates", rows, &self.scroll, &t, "Choose", {
            let shared = shared.clone();
            move |id: &ConversationId, _, cx| {
                shared.edit(cx, |chat| chat.toggle_group_member(id));
            }
        });
        let height = dialog::height(window, 560.);
        let content = dialog::layout(if new_group {
            "New group"
        } else {
            "Add members"
        })
        .h(px(height))
        .when(new_group, |layout| layout.child(Input::new(&self.name)))
        .child(Input::new(&self.filter).cleanable(true))
        .when(is_number, |layout| {
            layout.child(
                Button::new("find-number")
                    .label(format!("Find {filter_text} on Signal"))
                    .on_click(cx.listener({
                        let number = filter_text.clone();
                        move |this, _, _, cx| this.find_number(&number, cx)
                    })),
            )
        })
        .child(list)
        .child(
            dialog::buttons()
                .child(
                    Button::new("draft-cancel")
                        .label("Cancel")
                        .debug_selector(|| "Cancel".into())
                        .on_click({
                            let shared = shared.clone();
                            move |_, _, cx| shared.edit(cx, |chat| chat.close_group_draft())
                        }),
                )
                .child(
                    Button::new("draft-finish")
                        .primary()
                        .label(if new_group { "Create" } else { "Add" })
                        .disabled(!can_finish)
                        .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                ),
        );
        modal_holding(&t, 420., content)
    }
}
