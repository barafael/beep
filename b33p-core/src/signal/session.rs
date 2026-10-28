//! A linked session's actor, [`Session`]: it takes the commands from the UI and what
//! [`Receiver`] receives, deletes disappearing messages when due, and takes stories off the
//! list once their day is over, one at a time. Beside it
//! run the workers it hands work to: the outbox, the commands taken in order, the jobs of
//! [`Background`](super::Background), the avatars; and the storage service's follower.

use std::{cell::RefCell, panic::AssertUnwindSafe, rc::Rc, time::Duration};

use futures::{FutureExt as _, pin_mut};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use tracing::{info, warn};

use crate::{Command, Event, attachments, scrub};

use super::{
    Ctx, Mgr,
    avatars::AvatarFetcher,
    commands::{command_failed, handle, outgoing},
    history::sweep_expired,
    lifecycle::{CommandRx, Notice, unlink},
    receive::{Incoming, Kept, Receiver, Repairs, handle_incoming},
    stories::{send_stories, stories_over},
};

/// How [`Session::event_loop`] ended.
pub(super) enum Ended {
    /// The UI went away.
    Gone,
    /// Signal removed this device; it has to be linked again.
    Removed,
    /// [`Command::Unlink`]: removed from the account here; the data is to be deleted.
    Unlinked,
}

pub(super) struct Session {
    manager: Mgr,
    ctx: Ctx,
    downloads: Rc<attachments::Downloads>,
    avatars: AvatarFetcher,
    /// What handling what is received keeps from one message to the next.
    kept: Kept,
    /// Kept across restarts of receiving: the limits it keeps must outlive one.
    repairs: Repairs,
    /// Where commands go that are taken in order; see [`outgoing`].
    in_order: mpsc::UnboundedSender<Command>,
}

impl Session {
    pub(super) fn new(
        manager: Mgr,
        ctx: Ctx,
        downloads: Rc<attachments::Downloads>,
        avatars: AvatarFetcher,
        in_order: mpsc::UnboundedSender<Command>,
    ) -> Self {
        Self {
            manager,
            ctx,
            downloads,
            avatars,
            kept: Kept::default(),
            repairs: Repairs::default(),
            in_order,
        }
    }

    /// Takes `pending` first, then `commands` as they come and what is received, until the UI
    /// goes away, this device is unlinked, or Signal removes it. A new device name is put in
    /// `device_name`, for the next link. `story_over` is when the first story is over, as
    /// [`send_stories`] said.
    pub(super) async fn event_loop(
        mut self,
        pending: Vec<Command>,
        commands: &mut CommandRx,
        device_name: &RefCell<String>,
        story_over: Option<u64>,
    ) -> (Self, Ended) {
        let (mut incoming, mut receiving) = self.start_receiving(Duration::ZERO);
        let mut failures = 0u32;
        // Disappearing messages are deleted when due, and stories come off the list after
        // their day, in between what else the session does.
        let sweep = tokio::time::sleep(Duration::ZERO);
        pin_mut!(sweep);
        let stories_due = tokio::time::sleep_until(stories_over(story_over));
        pin_mut!(stories_due);

        let mut pending = pending.into_iter();
        let ended = loop {
            let command = match pending.next() {
                Some(command) => command,
                None => tokio::select! {
                    // Commands first, then what is due: neither is held up by catching up.
                    biased;
                    command = commands.recv() => match command {
                        Some(command) => command,
                        None => break Ended::Gone,
                    },
                    () = &mut sweep => match sweep_expired(&self.ctx).await {
                        Some(wait) => {
                            sweep.as_mut().reset(Instant::now() + wait);
                            continue;
                        }
                        None => break Ended::Gone,
                    },
                    () = self.ctx.expiring.notified() => {
                        sweep.as_mut().reset(Instant::now());
                        continue;
                    }
                    () = &mut stories_due => {
                        let next = send_stories(&self.manager, &self.ctx).await;
                        stories_due.as_mut().reset(stories_over(next));
                        continue;
                    }
                    // It may be over before the first one known, or be the first one.
                    () = self.ctx.story_kept.notified() => {
                        stories_due.as_mut().reset(Instant::now());
                        continue;
                    }
                    arrived = incoming.recv() => match arrived {
                        Some(Incoming::Removed) => break Ended::Removed,
                        Some(arrived) => {
                            if !self.on_incoming(arrived).await {
                                break Ended::Gone;
                            }
                            continue;
                        }
                        // `Receiver` ends on its own only after `Removed`, else it failed.
                        None => {
                            let error = match (&mut receiving).await {
                                Ok(_) => break Ended::Removed,
                                Err(error) => error,
                            };
                            // Start over, rather than look connected while nothing arrives;
                            // later and later should it keep failing.
                            warn!(error = %scrub::chain(&error), "receiving failed");
                            self.receiving_failed();
                            failures += 1;
                            let after = Duration::from_secs(5 << failures.min(6));
                            (incoming, receiving) = self.start_receiving(after);
                            continue;
                        }
                    },
                },
            };
            if let Command::Unlink = command {
                info!("unlinking this device");
                // Receiving stops with its channel closed.
                incoming.close();
                unlink(self.manager.clone()).await;
                break Ended::Unlinked;
            }
            if let Command::SetDeviceName(name) = command {
                // For the next link; the account keeps the name it was linked with.
                *device_name.borrow_mut() = name;
                continue;
            }
            self.on_command(command).await;
        };
        // Receiving stops with its channel closed.
        incoming.close();
        (self, ended)
    }

    async fn on_command(&mut self, command: Command) {
        if outgoing(&command) {
            // Kept from here until it went out: should b33p stop before, it shows as failed.
            if let Err(error) = self.ctx.tables.keep_unsent(&command).await {
                warn!(error = %scrub::chain(&error), "failed to keep a message until it is sent");
            }
            // It fails only if the task taking them in order panicked: then say the message
            // failed rather than show it as on its way. It is kept, so it shows as failed at
            // the next start too.
            if let Err(mpsc::error::SendError(command)) = self.in_order.send(command) {
                if let Command::Send {
                    conversation,
                    timestamp,
                    ..
                } = &command
                {
                    self.ctx.events.send(Event::SendFailed {
                        conversation: conversation.clone(),
                        timestamp: *timestamp,
                        retryable: false,
                    });
                }
                command_failed(
                    &self.ctx.events,
                    Notice::OrderedSendingStopped.into(),
                    command,
                );
            }
            return;
        }
        handle(&mut self.manager, &self.ctx, &self.downloads, command).await;
    }

    /// Returns `false` once the UI has gone away.
    async fn on_incoming(&mut self, arrived: Incoming) -> bool {
        let handling = handle_incoming(
            &self.manager,
            &self.ctx,
            &self.avatars,
            &mut self.kept,
            &mut self.repairs,
            arrived,
        );
        // Something it couldn't handle stops only this, as it did when it was handled where
        // it was received.
        match AssertUnwindSafe(handling).catch_unwind().await {
            Ok(handled) => handled,
            Err(_) => {
                warn!("handling what was received failed");
                self.receiving_failed();
                true
            }
        }
    }

    fn receiving_failed(&self) {
        self.ctx
            .events
            .send(Event::Error(Notice::ReceivingFailed.into()));
    }

    /// Starts a [`Receiver`], after `after`.
    fn start_receiving(&self, after: Duration) -> (mpsc::Receiver<Incoming>, JoinHandle<Receiver>) {
        let (incoming, arrived) = mpsc::channel(1);
        let receiver = Receiver::new(self.manager.clone()).event_loop(incoming, after);
        (arrived, tokio::task::spawn_local(receiver))
    }
}
