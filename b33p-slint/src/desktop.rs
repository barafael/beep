//! The desktop's file dialogs and clipboard. Both block until they are done, the dialogs until
//! the user is, so they run on a thread of their own: one actor, [`Desktop`], takes them in
//! turn, while the window goes on. Each request carries where its answer goes.

use std::path::PathBuf;

use futures_channel::oneshot;

/// What a file dialog may pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Any files, to attach.
    Files,
    /// One picture.
    Picture,
}

pub enum Request {
    /// Opens a file dialog titled `title`; answers with what was picked, nothing if cancelled.
    Pick {
        title: &'static str,
        pick: Pick,
        reply: oneshot::Sender<Vec<PathBuf>>,
    },
    /// Answers with the files on the clipboard (a picture saved as one), `None` if it holds
    /// neither; see [`crate::clipboard::pasted`].
    Paste {
        reply: oneshot::Sender<Option<Vec<PathBuf>>>,
    },
}

pub type Requests = async_channel::Sender<Request>;

/// Where pictures may come from.
const PICTURES: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

pub struct Desktop;

impl Desktop {
    /// Until every [`Requests`] is dropped and the queue is empty. Blocks: run it on a thread
    /// of its own.
    pub fn event_loop(mut self, requests: async_channel::Receiver<Request>) -> Self {
        while let Ok(request) = requests.recv_blocking() {
            self.handle(request);
        }
        self
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Pick { title, pick, reply } => {
                // Whoever asked may have gone meanwhile, e.g. with the window.
                let _ = reply.send(pick_files(title, pick));
            }
            // The window may own what was copied, and is free meanwhile to hand it over.
            Request::Paste { reply } => {
                let _ = reply.send(crate::clipboard::pasted());
            }
        }
    }
}

fn pick_files(title: &str, pick: Pick) -> Vec<PathBuf> {
    let dialog = rfd::FileDialog::new().set_title(title);
    match pick {
        Pick::Files => dialog.pick_files().unwrap_or_default(),
        Pick::Picture => dialog
            .add_filter("Pictures", PICTURES)
            .pick_file()
            .into_iter()
            .collect(),
    }
}

/// Asks `desktop` to pick, and hands what was picked to `then` on the UI thread. Nothing
/// happens if the user cancelled, or the desktop actor is gone.
pub fn pick(
    desktop: &Requests,
    title: &'static str,
    pick: Pick,
    then: impl FnOnce(Vec<PathBuf>) + 'static,
) {
    let (reply, picked) = oneshot::channel();
    if desktop
        .try_send(Request::Pick { title, pick, reply })
        .is_err()
    {
        return;
    }
    answered(picked, |files| {
        if !files.is_empty() {
            then(files);
        }
    });
}

/// Asks `desktop` for what the clipboard holds to attach, and hands it to `then` on the UI
/// thread.
pub fn paste(desktop: &Requests, then: impl FnOnce(Option<Vec<PathBuf>>) + 'static) {
    let (reply, pasted) = oneshot::channel();
    if desktop.try_send(Request::Paste { reply }).is_err() {
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop ends once every sender is gone, after what was queued.
    #[test]
    fn ends_when_nobody_can_ask() {
        let (requests, queue) = async_channel::unbounded();
        drop(requests);
        Desktop.event_loop(queue);
    }
}
