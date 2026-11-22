//! Small pieces more than one screen draws.

use b33p_core::ConversationId;
use b33p_core::state::{avatar_color, initials};
use dioxus::prelude::*;

/// A round badge with the chat's initials, in the colour the desktop gives it too. (Contacts'
/// pictures arrive as raw pixels, which want converting for a WebView; that comes later.)
#[component]
pub fn Avatar(id: ConversationId, title: String, #[props(default = false)] small: bool) -> Element {
    let color = avatar_color(&id);
    let class = if small { "avatar small" } else { "avatar" };
    rsx! {
        div {
            class,
            style: "background: #{color:06x};",
            aria_hidden: "true",
            "{initials(&title)}"
        }
    }
}
