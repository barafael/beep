//! The account's sends, one at a time.
//!
//! A send loads the recipient's session from the store, encrypts with its next counter, and
//! saves it, with awaits in between; two at once (a typing indicator in the background, the
//! message right after it) encrypt with the same counter, and the recipient refuses the second
//! as "message with old counter". So every send of b33p's goes through one actor, [`Sending`],
//! which owns a manager and works through its [`Outbox`] in order.
//!
//! This covers b33p's own sends only: presage also sends from tasks of its own while receiving,
//! and decrypting changes sessions too (#51); that needs the same in presage.

use futures::channel::oneshot;
use presage::libsignal_service::protocol::ServiceId;
use presage::{
    libsignal_service::{content::ContentBody, groups_v2::GroupInviteLink, protocol::Aci},
    manager::{GroupEdit, GroupJoin},
};
use tokio::sync::mpsc;
use tracing::debug;

use crate::scrub::scrubbed;
use crate::signal::Mgr;

type PresageError = presage::Error<presage_store_sqlite::SqliteStoreError>;

/// Where sends are queued for [`Sending`]. Bounded: a full queue holds up whoever adds to it.
pub(crate) type Outbox = mpsc::Sender<Send>;

/// How many sends may wait. Each is small, and a chat's worth of read receipts fits.
pub(crate) const CAPACITY: usize = 64;

/// Why a send didn't happen.
#[derive(Debug)]
pub(crate) enum SendFailure {
    /// The session ended first: [`Sending`] is gone.
    Stopped,
    Presage(Box<PresageError>),
}

impl std::fmt::Display for SendFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendFailure::Stopped => f.write_str("the session ended before it was sent"),
            SendFailure::Presage(error) => error.fmt(f),
        }
    }
}

/// What becomes of a send's outcome.
pub(crate) enum Reply<T> {
    /// To the one who asked.
    To(oneshot::Sender<Result<T, PresageError>>),
    /// Nobody waits: a failure is only logged, as failing to send `what`.
    Log(&'static str),
}

/// One thing for [`Sending`] to do; each sends, to someone or to our own devices.
pub(crate) enum Send {
    Message {
        to: ServiceId,
        body: ContentBody,
        timestamp: u64,
        reply: Reply<()>,
    },
    GroupMessage {
        master_key: [u8; 32],
        body: ContentBody,
        timestamp: u64,
        reply: Reply<()>,
    },
    /// Asks the primary device for the contacts.
    RequestContacts { reply: Reply<()> },
    SessionReset {
        to: ServiceId,
        timestamp: u64,
        reply: Reply<()>,
    },
    /// Group changes are sent to the members.
    CreateGroup {
        title: String,
        members: Vec<Aci>,
        reply: Reply<([u8; 32], u64)>,
    },
    ChangeGroup {
        master_key: [u8; 32],
        edit: GroupEdit,
        reply: Reply<u64>,
    },
    JoinGroup {
        link: GroupInviteLink,
        reply: Reply<GroupJoin>,
    },
}

/// Does the sends of an [`Outbox`], one after the other.
pub(crate) struct Sending {
    manager: Mgr,
}

impl Sending {
    pub(crate) fn new(manager: Mgr) -> Self {
        Self { manager }
    }

    /// Until every [`Outbox`] is dropped and the queue is empty.
    pub(crate) async fn event_loop(mut self, mut outbox: mpsc::Receiver<Send>) -> Self {
        while let Some(send) = outbox.recv().await {
            self.send(send).await;
        }
        self
    }

    async fn send(&mut self, send: Send) {
        let manager = &mut self.manager;
        match send {
            Send::Message {
                to,
                body,
                timestamp,
                reply,
            } => answer(reply, manager.send_message(to, body, timestamp).await),
            Send::GroupMessage {
                master_key,
                body,
                timestamp,
                reply,
            } => answer(
                reply,
                manager
                    .send_message_to_group(&master_key, body, timestamp)
                    .await,
            ),
            Send::RequestContacts { reply } => answer(reply, manager.request_contacts().await),
            Send::SessionReset {
                to,
                timestamp,
                reply,
            } => answer(reply, manager.send_session_reset(&to, timestamp).await),
            Send::CreateGroup {
                title,
                members,
                reply,
            } => answer(reply, manager.create_group(&title, &members).await),
            Send::ChangeGroup {
                master_key,
                edit,
                reply,
            } => answer(reply, manager.change_group(&master_key, edit).await),
            Send::JoinGroup { link, reply } => answer(reply, manager.join_group(&link).await),
        }
    }
}

fn answer<T>(reply: Reply<T>, result: Result<T, PresageError>) {
    match reply {
        // Whoever asked may have stopped waiting; the send happened all the same.
        Reply::To(asker) => drop(asker.send(result)),
        Reply::Log(what) => {
            if let Err(error) = result {
                debug!(error = %scrubbed(&error), "failed to send {what}");
            }
        }
    }
}

/// Queues `make`'s send and waits for its outcome.
async fn ask<T>(outbox: &Outbox, make: impl FnOnce(Reply<T>) -> Send) -> Result<T, SendFailure> {
    let (asker, answer) = oneshot::channel();
    outbox
        .send(make(Reply::To(asker)))
        .await
        .map_err(|_| SendFailure::Stopped)?;
    answer
        .await
        .map_err(|_| SendFailure::Stopped)?
        .map_err(|error| SendFailure::Presage(Box::new(error)))
}

/// Queues `send` without waiting for it to go out.
async fn post(outbox: &Outbox, send: Send) {
    if outbox.send(send).await.is_err() {
        debug!("the session ended before a send was queued");
    }
}

/// The sends, through an [`Outbox`]. Each waits its turn behind the ones queued before it.
pub(crate) trait Sends {
    /// Sends `body` to `to` and waits until it went out.
    async fn message(
        &self,
        to: ServiceId,
        body: impl Into<ContentBody>,
        timestamp: u64,
    ) -> Result<(), SendFailure>;

    /// Sends `body` to `to` in the background; `what` names it if it fails.
    async fn post_message(
        &self,
        to: ServiceId,
        body: impl Into<ContentBody>,
        timestamp: u64,
        what: &'static str,
    );

    async fn group_message(
        &self,
        master_key: [u8; 32],
        body: impl Into<ContentBody>,
        timestamp: u64,
    ) -> Result<(), SendFailure>;

    async fn post_group_message(
        &self,
        master_key: [u8; 32],
        body: impl Into<ContentBody>,
        timestamp: u64,
        what: &'static str,
    );

    async fn request_contacts(&self) -> Result<(), SendFailure>;

    async fn session_reset(&self, to: ServiceId, timestamp: u64) -> Result<(), SendFailure>;

    async fn create_group(
        &self,
        title: String,
        members: Vec<Aci>,
    ) -> Result<([u8; 32], u64), SendFailure>;

    async fn change_group(&self, master_key: [u8; 32], edit: GroupEdit)
    -> Result<u64, SendFailure>;

    async fn join_group(&self, link: GroupInviteLink) -> Result<GroupJoin, SendFailure>;
}

impl Sends for Outbox {
    async fn message(
        &self,
        to: ServiceId,
        body: impl Into<ContentBody>,
        timestamp: u64,
    ) -> Result<(), SendFailure> {
        let body = body.into();
        ask(self, |reply| Send::Message {
            to,
            body,
            timestamp,
            reply,
        })
        .await
    }

    async fn post_message(
        &self,
        to: ServiceId,
        body: impl Into<ContentBody>,
        timestamp: u64,
        what: &'static str,
    ) {
        let send = Send::Message {
            to,
            body: body.into(),
            timestamp,
            reply: Reply::Log(what),
        };
        post(self, send).await;
    }

    async fn group_message(
        &self,
        master_key: [u8; 32],
        body: impl Into<ContentBody>,
        timestamp: u64,
    ) -> Result<(), SendFailure> {
        let body = body.into();
        ask(self, |reply| Send::GroupMessage {
            master_key,
            body,
            timestamp,
            reply,
        })
        .await
    }

    async fn post_group_message(
        &self,
        master_key: [u8; 32],
        body: impl Into<ContentBody>,
        timestamp: u64,
        what: &'static str,
    ) {
        let send = Send::GroupMessage {
            master_key,
            body: body.into(),
            timestamp,
            reply: Reply::Log(what),
        };
        post(self, send).await;
    }

    async fn request_contacts(&self) -> Result<(), SendFailure> {
        ask(self, |reply| Send::RequestContacts { reply }).await
    }

    async fn session_reset(&self, to: ServiceId, timestamp: u64) -> Result<(), SendFailure> {
        ask(self, |reply| Send::SessionReset {
            to,
            timestamp,
            reply,
        })
        .await
    }

    async fn create_group(
        &self,
        title: String,
        members: Vec<Aci>,
    ) -> Result<([u8; 32], u64), SendFailure> {
        ask(self, |reply| Send::CreateGroup {
            title,
            members,
            reply,
        })
        .await
    }

    async fn change_group(
        &self,
        master_key: [u8; 32],
        edit: GroupEdit,
    ) -> Result<u64, SendFailure> {
        ask(self, |reply| Send::ChangeGroup {
            master_key,
            edit,
            reply,
        })
        .await
    }

    async fn join_group(&self, link: GroupInviteLink) -> Result<GroupJoin, SendFailure> {
        ask(self, |reply| Send::JoinGroup { link, reply }).await
    }
}
