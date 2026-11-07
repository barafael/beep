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
                // Fails only if whoever asked has gone meanwhile, e.g. with the window.
                reply.send(pick_files(title, pick)).ok();
            }
            // The window may own what was copied, and is free meanwhile to hand it over.
            Request::Paste { reply } => {
                // Fails only if whoever asked has gone meanwhile.
                reply.send(crate::clipboard::pasted()).ok();
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

/// Asks `desktop` to pick. The answer comes on the returned receiver: what was picked, empty
/// if the user cancelled; `None` if the desktop actor is gone.
pub fn ask_pick(
    desktop: &Requests,
    title: &'static str,
    pick: Pick,
) -> Option<oneshot::Receiver<Vec<PathBuf>>> {
    let (reply, picked) = oneshot::channel();
    desktop
        .try_send(Request::Pick { title, pick, reply })
        .ok()?;
    Some(picked)
}

/// Asks `desktop` for what the clipboard holds to attach; `None` if the actor is gone.
pub fn ask_paste(desktop: &Requests) -> Option<oneshot::Receiver<Option<Vec<PathBuf>>>> {
    let (reply, pasted) = oneshot::channel();
    desktop.try_send(Request::Paste { reply }).ok()?;
    Some(pasted)
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
