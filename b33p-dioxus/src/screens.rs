//! What shows before the chats do, and when something goes wrong: starting up, the password,
//! the code to link with, and a failure with ways out of it.

use b33p_core::state::MIN_PASSWORD_LEN;
use b33p_core::{Failure, HistoryTransfer, QrMatrix, SyncStage};
use dioxus::prelude::*;

use crate::store::Store;

fn stage_text(stage: SyncStage) -> String {
    match stage {
        SyncStage::Starting => "Opening your messages…".into(),
        SyncStage::Connecting => "Connecting to Signal…".into(),
        SyncStage::Transfer(HistoryTransfer::Waiting) => {
            "Waiting for your phone to send your message history…".into()
        }
        SyncStage::Transfer(HistoryTransfer::Downloading) => "Downloading your history…".into(),
        SyncStage::Transfer(HistoryTransfer::Importing { messages }) => {
            format!("Importing your history: {messages} messages so far…")
        }
        SyncStage::Transfer(HistoryTransfer::Finished) => "Finishing up…".into(),
        SyncStage::CatchingUp { messages } => {
            format!("Catching up on what came meanwhile: {messages} messages…")
        }
    }
}

#[component]
pub fn SyncingScreen(stage: SyncStage) -> Element {
    let store = use_context::<Store>();
    let can_start = store.chat.read().can_start();
    rsx! {
        div { class: "centered",
            h1 { class: "logo", "b33p" }
            div { class: "spinner", role: "progressbar" }
            p { class: "status", "{stage_text(stage)}" }
            if can_start {
                button {
                    class: "secondary",
                    onclick: move |_| {
                        store.act(|chat| chat.start());
                    },
                    "Show my chats"
                }
            }
        }
    }
}

#[component]
pub fn PasswordScreen(new: bool, error: Option<String>) -> Element {
    let store = use_context::<Store>();
    let mut password = use_signal(String::new);
    let mut confirmation = use_signal(String::new);
    let can_start_over = store.chat.read().can_start_over();
    let asking_to_start_over = use_signal(|| false);
    let submit = move || {
        store.command(|chat| chat.unlock(&password.read(), &confirmation.read()));
    };
    rsx! {
        form {
            class: "centered",
            onsubmit: move |event| {
                event.prevent_default();
                submit();
            },
            h1 { class: "logo", "b33p" }
            if new {
                p { class: "status",
                    "Choose a password for the messages stored on this phone. "
                    "You enter it each time b33p starts; nobody can recover it for you."
                }
            } else {
                p { class: "status", "Enter your password to open your messages." }
            }
            input {
                r#type: "password",
                autocomplete: "off",
                placeholder: "Password",
                // Not bound to the signal: a value written back into the field races with the
                // keystrokes typed meanwhile, and drops them.
                oninput: move |event| password.set(event.value()),
            }
            if new {
                input {
                    r#type: "password",
                    autocomplete: "off",
                    placeholder: "Password again",
                    oninput: move |event| confirmation.set(event.value()),
                }
                p { class: "hint", "At least {MIN_PASSWORD_LEN} characters." }
            }
            if let Some(error) = error {
                p { class: "error", role: "alert", "{error}" }
            }
            button { r#type: "submit", class: "primary", if new { "Set password" } else { "Unlock" } }
            if can_start_over {
                StartOver { asking: asking_to_start_over }
            }
        }
    }
}

/// "Forgot password" and its kin: deleting everything stored here to link again, asked twice.
#[component]
fn StartOver(asking: Signal<bool>) -> Element {
    let store = use_context::<Store>();
    rsx! {
        if asking() {
            div { class: "confirm",
                p { class: "error",
                    "This deletes every message stored on this phone, and links it to Signal again."
                }
                button {
                    r#type: "button",
                    class: "danger",
                    onclick: move |_| store.command(|chat| chat.start_over()),
                    "Delete and start over"
                }
                button {
                    r#type: "button",
                    class: "secondary",
                    onclick: move |_| asking.set(false),
                    "Cancel"
                }
            }
        } else {
            button {
                r#type: "button",
                class: "link",
                onclick: move |_| asking.set(true),
                "Forgot the password? Start over"
            }
        }
    }
}

/// The code Signal on the phone scans to link this device, as an SVG: crisp at any size.
fn qr_path(qr: &QrMatrix) -> String {
    use std::fmt::Write as _;
    const QUIET: usize = 4;
    let mut path = String::new();
    for (i, _) in qr.modules.iter().enumerate().filter(|(_, dark)| **dark) {
        let (x, y) = (i % qr.width + QUIET, i / qr.width + QUIET);
        // Writing to a `String` doesn't fail.
        write!(path, "M{x} {y}h1v1h-1z").ok();
    }
    path
}

#[component]
pub fn LinkScreen(qr: QrMatrix) -> Element {
    let side = qr.width + 8;
    let path = qr_path(&qr);
    rsx! {
        div { class: "centered",
            h1 { class: "logo", "b33p" }
            p { class: "status", "Link b33p to your Signal account" }
            svg {
                class: "qr",
                view_box: "0 0 {side} {side}",
                shape_rendering: "crispEdges",
                role: "img",
                "aria-label": "The code to scan with Signal on your phone",
                rect { width: "{side}", height: "{side}", fill: "#fff" }
                path { d: "{path}", fill: "#000" }
            }
            ol { class: "steps",
                li { "On the phone that has Signal, open Signal." }
                li { "Go to Settings → Linked devices → Link new device." }
                li { "Scan this code." }
            }
            p { class: "hint", "The code stays good for a few minutes; a new one appears after that." }
        }
    }
}

#[component]
pub fn FailedScreen(failure: Failure, message: String) -> Element {
    let store = use_context::<Store>();
    let asking_to_start_over = use_signal(|| false);
    let (can_retry, can_start_over, can_use_password) = {
        let chat = store.chat.read();
        (
            failure.can_retry(),
            chat.can_start_over(),
            failure.can_use_password(),
        )
    };
    rsx! {
        div { class: "centered",
            h1 { class: "logo", "b33p" }
            p { class: "error", role: "alert", "{message}" }
            if can_retry {
                button {
                    class: "primary",
                    onclick: move |_| store.command(|chat| chat.retry()),
                    "Try again"
                }
            }
            if can_use_password {
                button {
                    class: "secondary",
                    onclick: move |_| store.command(|chat| chat.use_password()),
                    "Use a password instead"
                }
            }
            if can_start_over {
                StartOver { asking: asking_to_start_over }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_path_draws_each_dark_module_inside_the_quiet_zone() {
        // A 2x2 code with the top-left and bottom-right modules dark.
        let qr = QrMatrix {
            width: 2,
            modules: vec![true, false, false, true],
            encoded_text: String::new(),
        };
        // Four modules of quiet zone on every side: the first module is at (4, 4).
        assert_eq!(qr_path(&qr), "M4 4h1v1h-1zM5 5h1v1h-1z");
    }

    #[test]
    fn a_light_code_has_nothing_to_draw() {
        let qr = QrMatrix {
            width: 1,
            modules: vec![false],
            encoded_text: String::new(),
        };
        assert_eq!(qr_path(&qr), "");
    }

    #[test]
    fn the_real_link_code_draws_as_many_modules_as_it_has_dark_ones() {
        let qr = QrMatrix::encode("sgnl://linkdevice?uuid=test").unwrap();
        let dark = qr.modules.iter().filter(|dark| **dark).count();
        assert_eq!(qr_path(&qr).matches('M').count(), dark);
    }

    #[test]
    fn every_stage_of_starting_up_says_something_different() {
        use HistoryTransfer::*;
        let stages = [
            SyncStage::Starting,
            SyncStage::Connecting,
            SyncStage::Transfer(Waiting),
            SyncStage::Transfer(Downloading),
            SyncStage::Transfer(Importing { messages: 12 }),
            SyncStage::Transfer(Finished),
            SyncStage::CatchingUp { messages: 7 },
        ];
        let texts: Vec<String> = stages.into_iter().map(stage_text).collect();
        for (i, text) in texts.iter().enumerate() {
            assert!(!text.is_empty());
            assert!(!texts[..i].contains(text), "{text:?} comes twice");
        }
        assert!(texts[4].contains("12"), "{}", texts[4]);
        assert!(texts[6].contains('7'), "{}", texts[6]);
    }
}
