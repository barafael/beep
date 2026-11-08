// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]
// Without a renderer nothing launches the app, and all of it is unused; that build exists only
// so that `cargo build --workspace` needs no WebView libraries.
#![cfg_attr(not(any(feature = "mobile", feature = "desktop")), allow(dead_code))]

//! b33p for Android: the same Signal client as `b33p-slint` and `b33p-gpui`, with its window
//! drawn by Dioxus in the system's WebView. Everything that isn't the window is `b33p-core`.

mod android;
mod app;
mod chats;
mod conversation;
mod logging;
mod screens;
mod store;
mod widgets;

fn main() {
    android::init();
    logging::init();
    #[cfg(any(feature = "mobile", feature = "desktop"))]
    dioxus::launch(app::app);
    #[cfg(not(any(feature = "mobile", feature = "desktop")))]
    eprintln!("b33p-dioxus was built without a renderer: use --features mobile or desktop");
}
