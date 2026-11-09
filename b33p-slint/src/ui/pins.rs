//! The bar of messages pinned in the open chat, and pinning from a message's menu.

use super::*;

/// The open chat's pins, for the bar and the messages' menus.
pub(super) fn render_pins(window: &AppWindow, chat: &ChatState) {
    let open = window.global::<OpenChat>();
    // Its models, made here the first time (beside the others of `set_up_models`).
    if open
        .get_pins()
        .as_any()
        .downcast_ref::<VecModel<PinItem>>()
        .is_none()
    {
        open.set_pins(ModelRc::new(VecModel::<PinItem>::default()));
        open.set_pinned_keys(ModelRc::new(VecModel::<SharedString>::default()));
    }
    let pinned = chat.pinned_messages();
    let pins: Vec<PinItem> = pinned
        .iter()
        .map(|pin| {
            let message = &pin.message;
            PinItem {
                key: format!("{}:{}", message.timestamp, message.sender_id).into(),
                author: message.sender.as_str().into(),
                text: pin_text(message).into(),
            }
        })
        .collect();
    let keys: Vec<SharedString> = pins.iter().map(|pin| pin.key.clone()).collect();
    update_list(&open.get_pins(), pins);
    update_list(&open.get_pinned_keys(), keys);
    open.set_can_pin(chat.can_pin_messages());
    open.set_pins_full(chat.pins_full());
}

/// What a pinned message says in the bar: its first line, or what it is.
fn pin_text(message: &b33p_core::ChatMessage) -> String {
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

pub(super) fn wire_pins(window: &AppWindow, shared: &Rc<Shared>) {
    let open = window.global::<OpenChat>();
    open.on_pin({
        let shared = shared.clone();
        move |key, seconds| {
            let seconds = u32::try_from(seconds).ok().filter(|&s| s > 0);
            shared.command(|chat| row(chat, &key).and_then(|row| chat.pin_message(row, seconds)));
        }
    });
    open.on_unpin({
        let shared = shared.clone();
        move |key| shared.command(|chat| row(chat, &key).and_then(|row| chat.unpin_message(row)))
    });
    open.on_show_pinned({
        let shared = shared.clone();
        move |index| shared.redraw(|chat| chat.show_pinned(index as usize))
    });
    open.on_check_pins({
        let shared = shared.clone();
        move || {
            shared.redraw(|_| Update {
                pins: true,
                ..Update::default()
            })
        }
    });
}
