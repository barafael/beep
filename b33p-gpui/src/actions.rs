//! The keyboard shortcuts that work wherever the focus is.

use gpui_kit::{App, KeyBinding, actions};

actions!(
    b33p,
    [
        FocusSearch,
        SearchInChat,
        NextChat,
        PreviousChat,
        NextUnreadChat,
        CloseFront,
        CopyMessages,
    ]
);

/// `Pinned(n)`: the n-th pinned chat (Alt+1 to Alt+9).
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = b33p, no_json)]
pub struct Pinned(pub usize);

pub fn bind(cx: &mut App) {
    // Command on macOS, Control elsewhere; Option and Alt are the same key.
    let primary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    // Kept for as long as the process runs: GPUI wants the keystrokes as `'static` text.
    let keys = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    let mut bindings = vec![
        KeyBinding::new(keys(format!("{primary}-k")), FocusSearch, None),
        KeyBinding::new(keys(format!("{primary}-f")), SearchInChat, None),
        KeyBinding::new("alt-down", NextChat, None),
        KeyBinding::new("alt-up", PreviousChat, None),
        KeyBinding::new("alt-shift-down", NextUnreadChat, None),
        KeyBinding::new("escape", CloseFront, Some("App")),
    ];
    for n in 1..=9 {
        bindings.push(KeyBinding::new(keys(format!("alt-{n}")), Pinned(n - 1), None));
    }
    cx.bind_keys(bindings);
}
