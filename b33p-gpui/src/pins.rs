//! What is pinned in the open chat, as Signal's apps show it: one pin at a time in a bar over the
//! messages, the newest first; a click shows it in the chat and turns to the one pinned before
//! it. Ported from `b33p-slint/ui/pins.slint` and `src/ui/pins.rs`.

use std::rc::Rc;

use b33p_core::state::{self, ChatState};
use b33p_core::{ChatMessage, Command, PinnedMessage};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, FontWeight, IntoElement, Render, Role, SharedString, Window};
use gpui_kit::{div, px};

use crate::shared::Shared;
use crate::theme;
use crate::widgets::{Speaks as _, divider, icon, icon_button};

/// The pins a chat keeps, and which of them the bar is on.
pub struct PinBar {
    shared: Rc<Shared>,
    /// How many pins back from the newest the bar is.
    back: usize,
    /// The pins the bar was last drawn for (each message's key), so that it starts over at the
    /// newest when they change.
    seen: Vec<String>,
}

impl PinBar {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self {
            shared,
            back: 0,
            seen: Vec::new(),
        }
    }

    /// The bar's content, or nothing while the open chat has no pins.
    pub fn shown(this: &gpui_kit::Entity<Self>, shared: &Shared) -> Option<AnyElement> {
        (!shared.chat.borrow().pinned_messages().is_empty())
            .then(|| this.clone().into_any_element())
    }
}

/// The pin the bar shows, as an index into the pins (oldest first), after `back` clicks; the
/// newest to begin with, and round to it again after the oldest.
pub fn shown_pin(count: usize, back: usize) -> usize {
    count.saturating_sub(1) - back % count.max(1)
}

/// What a pinned message says in the bar: its first line, or what it is.
pub fn pin_text(message: &ChatMessage) -> String {
    let line = b33p_core::format::hide_spoilers(&message.body, &message.styles)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    match message.attachments.first() {
        Some(attachment) if line.is_empty() => state::describe_kind(attachment.kind).to_owned(),
        _ => line,
    }
}

/// What a screen reader says for the bar.
pub fn bar_label(count: usize, shown: usize, author: &str, text: &str) -> String {
    if count > 1 {
        format!("Pinned message {} of {count}: {author}: {text}", shown + 1)
    } else {
        format!("Pinned message: {author}: {text}")
    }
}

/// The command that unpins `pin`, which the bar may show though its message is not among the
/// ones loaded (older history is fetched as the chat is scrolled), so it goes by the pin.
fn unpin(chat: &ChatState, pin: &PinnedMessage) -> Option<Command> {
    if !chat.can_pin_messages() {
        return None;
    }
    Some(Command::UnpinMessage {
        conversation: chat.selected()?.id.clone(),
        target: pin.message.reference(),
    })
}

fn key(message: &ChatMessage) -> String {
    format!("{}:{}", message.timestamp, message.sender_id)
}

impl Render for PinBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let (pins, can_unpin) = {
            let chat = shared.chat.borrow();
            let pins: Vec<(String, String, String)> = chat
                .pinned_messages()
                .iter()
                .map(|pin| {
                    (
                        key(&pin.message),
                        pin.message.sender.clone(),
                        pin_text(&pin.message),
                    )
                })
                .collect();
            (pins, chat.can_pin_messages())
        };
        let keys: Vec<String> = pins.iter().map(|(key, ..)| key.clone()).collect();
        if keys != self.seen {
            self.seen = keys;
            self.back = 0;
        }
        if pins.is_empty() {
            return div().into_any_element();
        }
        let count = pins.len();
        let shown = shown_pin(count, self.back);
        let (_, author, text) = pins[shown].clone();
        let label = bar_label(count, shown, &author, &text);
        div()
            .id("pin-bar")
            .flex_none()
            .h(px(theme::PANEL_HEADER))
            .flex()
            .flex_col()
            .cursor_pointer()
            .bg(t.surface)
            .hover(move |bar| bar.bg(t.hover))
            .speaks(Role::Button, label)
            .on_click(cx.listener(move |bar, _, _, cx| {
                bar.shared.redraw(cx, |chat| chat.show_pinned(shown));
                bar.back += 1;
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_S))
                    .pl(px(theme::SPACE_M))
                    .pr(px(theme::SPACE_S))
                    // A mark for each pin, the one shown in the accent colour, when there are
                    // several.
                    .when(count > 1, |row| {
                        let height =
                            (theme::PANEL_HEADER - 2. * theme::SPACE_S - 2. * (count as f32 - 1.))
                                / count as f32;
                        row.child(div().flex().flex_col().gap(px(2.)).children((0..count).map(
                            |at| {
                                div()
                                    .w(px(3.))
                                    .h(px(height))
                                    .rounded_full()
                                    .bg(if at == shown { t.accent } else { t.divider })
                            },
                        )))
                    })
                    .child(icon("pin").size(px(theme::ICON)).text_color(t.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(theme::BODY * theme::SMALL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.accent)
                                    .child(SharedString::from(author)),
                            )
                            .child(div().truncate().child(SharedString::from(text))),
                    )
                    .when(can_unpin, |row| {
                        let pin_shared = shared.clone();
                        let pin_key = pins[shown].0.clone();
                        row.child(
                            icon_button("unpin", "pin-off", "Unpin", move |_, _, cx| {
                                // The bar's own click is not this one's.
                                cx.stop_propagation();
                                // By its key: the pins may have changed since the bar was drawn.
                                pin_shared.command(cx, |chat| {
                                    let pins = chat.pinned_messages();
                                    let pin =
                                        pins.iter().find(|pin| key(&pin.message) == pin_key)?;
                                    unpin(chat, pin)
                                });
                            })
                            .small(),
                        )
                    }),
            )
            .child(divider(&t))
            .into_any_element()
    }
}
