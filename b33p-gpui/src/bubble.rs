//! One row of the open chat's messages: a bubble, a notice, and the day and "new messages"
//! lines between them.

use std::rc::Rc;

use b33p_core::state::{self, MessageView};
use b33p_core::{ConversationId, Delivery, Unsent};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, FontWeight, SharedString, div, px};

use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{avatar, avatar_color, icon};

/// The row of message `index` of the open chat.
pub fn row(index: usize, shared: &Rc<Shared>, cx: &mut App) -> AnyElement {
    let t = theme::tokens(cx);
    let chat = shared.chat.borrow();
    let Some(view) = chat.message_view(index) else {
        return div().into_any_element();
    };
    let in_group = chat.selected().is_some_and(|c| c.is_group());
    let mut column = div().w_full().flex().flex_col();
    if let Some(day) = &view.day {
        column = column.child(
            div().flex().justify_center().py(px(theme::SPACE_S)).child(
                div()
                    .px(px(10.))
                    .py(px(3.))
                    .rounded_full()
                    .bg(t.surface_raised)
                    .text_size(px(theme::BODY * theme::SMALL))
                    .text_color(t.text_secondary)
                    .child(SharedString::from(day.clone())),
            ),
        );
    }
    if view.first_unread {
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .py(px(theme::SPACE_S))
                .text_size(px(theme::BODY * theme::SMALL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.accent)
                .child(div().flex_1().h(px(1.)).bg(t.accent.opacity(0.4)))
                .child("New messages")
                .child(div().flex_1().h(px(1.)).bg(t.accent.opacity(0.4))),
        );
    }
    column
        .child(message(&view, in_group, shared, &t))
        .into_any_element()
}

fn message(view: &MessageView, in_group: bool, shared: &Rc<Shared>, t: &Tokens) -> AnyElement {
    let m = view.message;
    if m.notice {
        return div()
            .flex()
            .justify_center()
            .py(px(2.))
            .child(
                div()
                    .text_center()
                    .text_size(px(theme::BODY * theme::SMALL))
                    .text_color(t.text_tertiary)
                    .child(SharedString::from(view.body.to_owned())),
            )
            .into_any_element();
    }
    let outgoing = m.outgoing;
    let show_avatar = in_group && !outgoing;
    let sender = ConversationId::contact(&m.sender_id);
    let (background, foreground) = if outgoing {
        (t.bubble_outgoing, t.bubble_outgoing_text)
    } else {
        (t.bubble_incoming, t.bubble_incoming_text)
    };
    let radius = px(theme::BUBBLE_RADIUS);
    let tight = px(theme::BUBBLE_RADIUS_TIGHT);
    let delivery = match (outgoing, m.unsent, m.delivery) {
        (false, ..) => None,
        (true, Some(Unsent::Sending), _) => Some("clock"),
        (true, Some(Unsent::Failed | Unsent::Refused), _) => Some("triangle-alert"),
        (true, None, Delivery::Sent) => Some("check"),
        (true, None, Delivery::Delivered | Delivery::Read) => Some("check-check"),
    };
    let body = if m.deleted {
        state::DELETED.to_owned()
    } else {
        view.body.to_owned()
    };
    let bubble = div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .max_w(gpui_kit::relative(0.7))
        .px(px(12.))
        .py(px(8.))
        .bg(background)
        .text_color(foreground)
        .rounded_tl(if outgoing || view.first_of_run { radius } else { tight })
        .rounded_tr(if !outgoing || view.first_of_run { radius } else { tight })
        .rounded_bl(if outgoing || view.last_of_run { radius } else { tight })
        .rounded_br(if !outgoing || view.last_of_run { radius } else { tight })
        .when(view.selected, |b| b.border_2().border_color(t.accent))
        .when(view.show_sender, |b| {
            b.child(
                div()
                    .text_size(px(theme::BODY * theme::BODY_SMALL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(avatar_color(&sender))
                    .child(SharedString::from(m.sender.clone())),
            )
        })
        .children(m.quote.as_ref().map(|quote| {
            div()
                .flex()
                .flex_col()
                .pl(px(8.))
                .border_l_2()
                .border_color(foreground.opacity(0.5))
                .text_size(px(theme::BODY * theme::BODY_SMALL))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(quote.author.clone())),
                )
                .child(div().opacity(0.8).child(SharedString::from(quote.text.clone())))
        }))
        .when(!body.is_empty(), |b| {
            b.child(
                div()
                    .when(m.deleted, |d| d.italic().opacity(0.7))
                    .child(SharedString::from(body)),
            )
        })
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .items_center()
                .gap(px(4.))
                .text_size(px(theme::BODY * theme::CAPTION))
                .opacity(0.75)
                .when(m.edited_at.is_some(), |d| d.child("edited"))
                .child(state::format_clock(m.timestamp))
                .children(delivery.map(|name| icon(name).size(px(12.)))),
        );
    let reactions = reaction_chips(&m.reactions, t);
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_end()
        .gap(px(theme::SPACE_S))
        .when(outgoing, |row| row.justify_end())
        .children(show_avatar.then(|| {
            let picture = shared.images.avatar(&format!("person:{}", m.sender_id), view.sender_avatar);
            div().w(px(28.)).flex_none().child(avatar(
                28.,
                &state::initials(&m.sender),
                avatar_color(&sender),
                picture,
                t,
            ))
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .when(outgoing, |c| c.items_end())
                .max_w(gpui_kit::relative(0.75))
                .child(bubble)
                .children(reactions),
        )
        .into_any_element()
}

/// One chip per emoji, in the order they first came, counting everyone who chose it.
fn reaction_chips(reactions: &[b33p_core::Reaction], t: &Tokens) -> Option<AnyElement> {
    if reactions.is_empty() {
        return None;
    }
    let mut chips: Vec<(String, u32, bool)> = Vec::new();
    for reaction in reactions {
        match chips.iter_mut().find(|(emoji, ..)| *emoji == reaction.emoji) {
            Some(chip) => {
                chip.1 += 1;
                chip.2 |= reaction.outgoing;
            }
            None => chips.push((reaction.emoji.clone(), 1, reaction.outgoing)),
        }
    }
    Some(
        div()
            .flex()
            .flex_row()
            .gap(px(4.))
            .children(chips.into_iter().map(|(emoji, count, mine)| {
                div()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded_full()
                    .text_size(px(theme::BODY * theme::BODY_SMALL))
                    .bg(if mine { t.selected } else { t.surface_raised })
                    .child(if count > 1 {
                        format!("{emoji} {count}")
                    } else {
                        emoji
                    })
            }))
            .into_any_element(),
    )
}
