//! The bars between the messages and the composer that answer something about the chat:
//! an invitation to a group, our own request to join one, and a safety number that changed.

use std::rc::Rc;

use b33p_core::state::Update;
use b33p_core::{GroupEdit, Trust};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Div, SharedString, div, px};

use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::icon;

/// The bars the open chat needs, top to bottom.
pub fn bars(shared: &Rc<Shared>, t: &Tokens) -> Vec<AnyElement> {
    let (id, title, invited, requested, changed) = {
        let chat = shared.chat.borrow();
        let Some(c) = chat.selected() else {
            return Vec::new();
        };
        (
            c.id.clone(),
            c.title.clone(),
            c.invited,
            c.requested,
            c.trust == Trust::Changed,
        )
    };
    let mut bars = Vec::new();
    if invited {
        let answer = |accept: bool| {
            let shared = shared.clone();
            let id = id.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
                let edit = if accept {
                    GroupEdit::AcceptInvitation
                } else {
                    GroupEdit::DeclineInvitation
                };
                shared.command(cx, |chat| chat.edit_group(&id, edit));
            }
        };
        bars.push(
            banner(
                t,
                "users",
                "You're invited to join this group. Its members see you once you join.".into(),
            )
            .child(
                Button::new("invite-decline")
                    .label("Decline")
                    .on_click(answer(false)),
            )
            .child(
                Button::new("invite-accept")
                    .primary()
                    .label("Accept")
                    .on_click(answer(true)),
            )
            .into_any_element(),
        );
    }
    if requested {
        let shared = shared.clone();
        bars.push(
            banner(
                t,
                "hourglass",
                "You asked to join this group. An admin has to let you in.".into(),
            )
            .child(
                Button::new("request-take-back")
                    .label("Take request back")
                    .on_click(move |_, _, cx| {
                        shared.act(cx, |chat| {
                            let id = chat.selected().map(|c| c.id.clone());
                            (
                                id.and_then(|id| {
                                    chat.edit_group(&id, GroupEdit::CancelJoinRequest)
                                }),
                                Update::default(),
                            )
                        });
                    }),
            )
            .into_any_element(),
        );
    }
    if changed {
        let (show, keep) = (shared.clone(), shared.clone());
        bars.push(
            banner(
                t,
                "shield-alert",
                format!(
                    "Your safety number with {title} changed since you verified it. Compare it \
                     again, or continue without."
                ),
            )
            .child(
                Button::new("safety-show")
                    .label("Show safety number")
                    .on_click(move |_, _, cx| show.command(cx, |chat| chat.open_details())),
            )
            .child(
                Button::new("safety-continue")
                    .label("Continue")
                    .on_click(move |_, _, cx| {
                        keep.command(cx, |chat| chat.confirm_safety_number(&id))
                    }),
            )
            .into_any_element(),
        );
    }
    bars
}

/// An icon and a line of text, with room for buttons after.
fn banner(t: &Tokens, name: &'static str, text: String) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_M))
        .pl(px(theme::SPACE_L))
        .pr(px(theme::SPACE_M))
        .py(px(theme::SPACE_M))
        .bg(t.surface_raised)
        .border_t_1()
        .border_color(t.divider)
        .child(icon(name).size(px(theme::ICON)))
        .child(div().flex_1().min_w_0().child(SharedString::from(text)))
}
