//! The info of one of our messages: who it went to, and when each of them got it and read it,
//! as Signal's apps show it ("Message details"). Opened from the message's menu ("Info"),
//! closed with Esc, a click outside, or "Close"; the state holds what is shown
//! (`ChatState::message_info`), so the harness opens and closes it too.

use std::rc::Rc;

use b33p_core::state;
use b33p_core::{ConversationId, Receipt};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, FocusHandle, Focusable, FontWeight, IntoElement, Render, Role, SharedString,
    Window, div, px,
};

use crate::dialog;
use crate::pins::pin_text;
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{Speaks as _, avatar, avatar_color, icon, modal};

/// What a recipient's row says of the message: when they read it, else when they got it, else
/// that it has not reached them yet. `time` says a time of day or date.
pub fn status(receipt: &Receipt, time: impl Fn(u64) -> String) -> String {
    match (receipt.read_at, receipt.delivered_at) {
        (Some(at), _) => format!("Read {}", time(at)),
        (None, Some(at)) => format!("Delivered {}", time(at)),
        (None, None) => "Not delivered yet".to_owned(),
    }
}

/// Closes the info.
pub fn close(shared: &Rc<Shared>, cx: &mut App) {
    shared.edit(cx, |chat| chat.close_message_info());
}

pub struct MessageInfoView {
    shared: Rc<Shared>,
    focus: FocusHandle,
}

impl MessageInfoView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self { shared, focus }
    }
}

impl Focusable for MessageInfoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MessageInfoView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        // What is shown, taken out of the state so that it isn't borrowed while drawn.
        let (summary, sent, recipients) = {
            let chat = shared.chat.borrow();
            let Some((message, info)) = chat.message_info() else {
                return div().into_any_element();
            };
            let time = state::format_time(message.timestamp);
            let recipients = info.map(|info| {
                info.recipients
                    .iter()
                    .map(|receipt| {
                        let id = ConversationId::contact(&receipt.person.service_id);
                        let picture = shared.images.avatar(
                            &format!("person:{}", receipt.person.service_id),
                            chat.avatar(&id),
                        );
                        (
                            receipt.person.name.clone(),
                            status(receipt, state::format_time),
                            receipt.read_at.is_some(),
                            avatar_color(&id),
                            picture,
                        )
                    })
                    .collect::<Vec<_>>()
            });
            (pin_text(message), time, recipients)
        };
        let rows: Vec<_> = recipients
            .iter()
            .flatten()
            .enumerate()
            .map(|(at, (name, status, read, color, picture))| {
                div()
                    .id(("recipient", at))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_M))
                    .py(px(6.))
                    .speaks_as(
                        Role::ListItem,
                        format!("{name}, {}", status.to_lowercase()),
                        format!("Recipient {at}"),
                    )
                    .child(avatar(
                        36.,
                        &state::initials(name),
                        *color,
                        picture.clone(),
                        &t,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(theme::BODY * theme::SMALL))
                                    .text_color(t.text_secondary)
                                    .child(SharedString::from(status.clone())),
                            ),
                    )
                    .child(
                        icon(if *read { "check-check" } else { "check" })
                            .size(px(theme::ICON))
                            .text_color(if *read { t.accent } else { t.text_tertiary }),
                    )
            })
            .collect();
        let close_shared = shared.clone();
        let content = div()
            .track_focus(&self.focus)
            .key_context("MessageInfo")
            .debug_selector(|| "Message info".into())
            .on_key_down({
                let shared = shared.clone();
                move |event, _, cx| {
                    if event.keystroke.key == "enter" {
                        close(&shared, cx);
                        cx.stop_propagation();
                    }
                }
            })
            .child(
                dialog::layout("Message info")
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().line_clamp(2).child(SharedString::from(summary)))
                            .child(dialog::hint(&t, format!("Sent {sent}"))),
                    )
                    .child(match &recipients {
                        None => dialog::hint(&t, "Finding out who received it…")
                            .id("message-info-gathering")
                            .debug_selector(|| "Gathering".into())
                            .into_any_element(),
                        Some(list) if list.is_empty() => {
                            dialog::hint(&t, "Nobody to show.").into_any_element()
                        }
                        Some(_) => div()
                            .id("message-info-recipients")
                            .flex()
                            .flex_col()
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .children(rows)
                            .into_any_element(),
                    })
                    .child(
                        dialog::buttons().child(
                            Button::new("message-info-close")
                                .primary()
                                .label("Close")
                                .debug_selector(|| "Close".into())
                                .on_click(move |_, _, cx| close(&close_shared, cx)),
                        ),
                    ),
            );
        modal(&t, 380., content, move |_, cx| close(&shared, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use b33p_core::Person;

    fn receipt(delivered: Option<u64>, read: Option<u64>) -> Receipt {
        Receipt {
            person: Person::default(),
            delivered_at: delivered,
            read_at: read,
        }
    }

    #[test]
    fn a_recipient_is_read_delivered_or_not_reached() {
        let time = |at: u64| format!("t{at}");
        assert_eq!(status(&receipt(Some(1), Some(2)), time), "Read t2");
        assert_eq!(status(&receipt(Some(1), None), time), "Delivered t1");
        // A read receipt can come without a delivery receipt (they arrive out of order).
        assert_eq!(status(&receipt(None, Some(2)), time), "Read t2");
        assert_eq!(status(&receipt(None, None), time), "Not delivered yet");
    }
}
