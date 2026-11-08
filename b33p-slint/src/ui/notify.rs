//! How the open chat notifies, beside muting it, on its details page.

use b33p_core::ChatNotify as Notify;

use super::*;

/// The open chat's notification settings, for its details page.
pub(super) fn render_notify(window: &AppWindow, chat: &ChatState) {
    let notify = chat
        .selected()
        .map(|c| c.settings.notify)
        .unwrap_or_default();
    let global = window.global::<ChatNotify>();
    global.set_notify_for(i32::from(notify.mentions_only));
    global.set_show_text(!notify.hide_text);
    global.set_sound(!notify.silent);
}

pub(super) fn wire_notify(window: &AppWindow, shared: &Rc<Shared>) {
    let global = window.global::<ChatNotify>();
    global.on_changed({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let global = window.global::<ChatNotify>();
            let notify = Notify {
                mentions_only: global.get_notify_for() == 1,
                hide_text: !global.get_show_text(),
                silent: !global.get_sound(),
            };
            shared.command(|chat| {
                let id = chat.selected()?.id.clone();
                chat.set_chat_notify(&id, notify)
            });
        }
    });
}
