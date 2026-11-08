//! Small pieces more than one screen draws.

use dioxus::prelude::*;

/// A round badge with the name's first letter, in a colour the name always gets. (Contacts'
/// pictures arrive as raw pixels, which want converting for a WebView; that comes later.)
#[component]
pub fn Avatar(name: String, #[props(default = false)] small: bool) -> Element {
    let initial = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect());
    // A stable hash, so that a name's colour doesn't change between runs.
    let hue = name.bytes().fold(7u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    }) % 360;
    let class = if small { "avatar small" } else { "avatar" };
    rsx! {
        div {
            class,
            style: "background: hsl({hue} 45% 42%);",
            aria_hidden: "true",
            "{initial}"
        }
    }
}
