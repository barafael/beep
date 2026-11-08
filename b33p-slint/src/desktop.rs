//! The desktop's file dialogs and clipboard, answered on Slint's event loop; the actor that
//! runs them is `b33p_platform::desktop`.

use std::path::PathBuf;

pub use b33p_platform::desktop::{Desktop, Pick, Requests};
use futures_channel::oneshot;

/// Asks `desktop` to pick, and hands what was picked to `then` on the UI thread. Nothing
/// happens if the user cancelled, or the desktop actor is gone.
pub fn pick(
    desktop: &Requests,
    title: &'static str,
    pick: Pick,
    then: impl FnOnce(Vec<PathBuf>) + 'static,
) {
    let Some(picked) = b33p_platform::desktop::ask_pick(desktop, title, pick) else {
        return;
    };
    answered(picked, |files| {
        if !files.is_empty() {
            then(files);
        }
    });
}

/// Asks `desktop` for what the clipboard holds to attach, and hands it to `then` on the UI
/// thread.
pub fn paste(desktop: &Requests, then: impl FnOnce(Option<Vec<PathBuf>>) + 'static) {
    let Some(pasted) = b33p_platform::desktop::ask_paste(desktop) else {
        return;
    };
    answered(pasted, then);
}

/// Runs `then` on the UI thread once `answer` comes.
fn answered<T: 'static>(answer: oneshot::Receiver<T>, then: impl FnOnce(T) + 'static) {
    let waiting = slint::spawn_local(async move {
        if let Ok(answer) = answer.await {
            then(answer);
        }
    });
    if let Err(error) = waiting {
        tracing::warn!(%error, "can't wait for the desktop's answer");
    }
}
