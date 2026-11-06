//! What b33p asks of the desktop, whichever toolkit draws its window: one copy of b33p, the
//! log, starting with the system, the clipboard, audio playback, the desktop's reduced-motion
//! preference, and its file dialogs. Both windows (b33p-slint, b33p-gpui) build on it.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

pub mod audio;
pub mod autostart;
pub mod clipboard;
pub mod desktop;
pub mod instance;
pub mod logging;
pub mod motion;
