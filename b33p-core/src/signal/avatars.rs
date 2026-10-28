//! Profile and group pictures for the UI: fetched from presage, cropped and scaled down.

use std::{cell::Cell, rc::Rc, sync::Arc};

use presage::{
    Error as PresageError,
    libsignal_service::{content::GroupContextV2, protocol::ServiceId, push_service::ServiceError},
    store::{ContentsStore, Thread},
};
use tracing::{debug, warn};

use crate::{Event, Events, Picture, attachments, scrub::scrubbed};

use super::{
    Ctx, Mgr, Threads,
    conversations::{conversation_id, tag},
};

/// Sends a group's picture as presage has it now (see [`fetch_avatars`]), or that it has none.
pub(super) async fn send_group_avatar(mut manager: Mgr, ctx: &Ctx, key: [u8; 32]) {
    let revision = manager
        .store()
        .group(key)
        .await
        .ok()
        .flatten()
        .map_or(0, |group| group.revision);
    let context = GroupContextV2 {
        master_key: Some(key.to_vec()),
        revision: Some(revision),
        ..Default::default()
    };
    let avatar = match manager.retrieve_group_avatar(context).await {
        Ok(Some(bytes)) => decode_avatar(bytes).await.map(Arc::new),
        Ok(None) => None,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to fetch the group's picture");
            return;
        }
    };
    ctx.events.send(Event::Avatar {
        conversation: conversation_id(&Thread::Group(key)),
        avatar,
    });
}

/// Side of decoded avatars: a 40 px circle at up to 3x scaling.
const AVATAR_SIDE: u32 = 128;

/// Runs [`fetch_avatars`] one pass at a time: asking during a pass schedules one more after it,
/// instead of a second pass sending the same requests alongside.
#[derive(Clone)]
pub(super) struct AvatarFetcher {
    run: Rc<Cell<AvatarRun>>,
    manager: Mgr,
    threads: Threads,
    events: Events,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AvatarRun {
    Idle,
    Running,
    Again,
}

impl AvatarFetcher {
    pub(super) fn new(manager: Mgr, threads: Threads, events: Events) -> Self {
        Self {
            run: Rc::new(Cell::new(AvatarRun::Idle)),
            manager,
            threads,
            events,
        }
    }

    pub(super) fn request(&self) {
        match self.run.get() {
            AvatarRun::Idle => {
                self.run.set(AvatarRun::Running);
                let this = self.clone();
                tokio::task::spawn_local(async move {
                    loop {
                        fetch_avatars(
                            this.manager.clone(),
                            this.threads.clone(),
                            this.events.clone(),
                        )
                        .await;
                        if this.run.get() == AvatarRun::Again {
                            this.run.set(AvatarRun::Running);
                        } else {
                            this.run.set(AvatarRun::Idle);
                            break;
                        }
                    }
                });
            }
            AvatarRun::Running => self.run.set(AvatarRun::Again),
            AvatarRun::Again => {}
        }
    }
}

/// Sends each conversation's profile or group picture. presage caches them in the store, so
/// only the first run downloads them, one at a time to stay clear of rate limits.
///
/// A profile picture needs the contact's profile key, which only arrives with their messages
/// (the contact sync no longer carries it). Until then the picture from the contact sync, i.e.
/// what the phone shows, stands in.
async fn fetch_avatars(mut manager: Mgr, threads: Threads, events: Events) {
    let threads: Vec<_> = threads
        .borrow()
        .iter()
        .map(|(id, thread)| (id.clone(), thread.clone()))
        .collect();
    let (mut found, mut none, mut failed) = (0, 0, 0);
    for (id, thread) in threads {
        let picture = match thread {
            Thread::Contact(ServiceId::Aci(aci)) => {
                let id = ServiceId::Aci(aci);
                let profile = match manager.store().profile_key(&id).await {
                    Ok(Some(key)) => {
                        manager
                            .retrieve_profile_avatar_by_uuid(aci.into(), key)
                            .await
                    }
                    _ => Ok(None),
                };
                match profile {
                    Ok(None) => {
                        let contact = manager.store().contact_by_id(&id).await.ok().flatten();
                        Ok(contact
                            .and_then(|c| c.avatar)
                            .map(|avatar| avatar.reader.to_vec()))
                    }
                    profile => profile,
                }
            }
            Thread::Contact(_) => continue,
            Thread::Group(key) => {
                // presage only refetches the group if this revision is newer than the cached one.
                let revision = manager
                    .store()
                    .group(key)
                    .await
                    .ok()
                    .flatten()
                    .map_or(0, |group| group.revision);
                let context = GroupContextV2 {
                    master_key: Some(key.to_vec()),
                    revision: Some(revision),
                    ..Default::default()
                };
                manager.retrieve_group_avatar(context).await
            }
        };
        match picture {
            Ok(Some(bytes)) => match decode_avatar(bytes).await {
                Some(avatar) => {
                    found += 1;
                    if !events.sent(Event::Avatar {
                        conversation: id,
                        avatar: Some(Arc::new(avatar)),
                    }) {
                        return;
                    }
                }
                None => {
                    failed += 1;
                    debug!(conversation = %tag(&id), "could not decode avatar");
                }
            },
            Ok(None) => none += 1,
            Err(PresageError::ServiceError(ServiceError::RateLimitExceeded { .. })) => {
                warn!("rate limited while fetching avatars; the rest are fetched next time");
                return;
            }
            Err(error) => {
                failed += 1;
                debug!(conversation = %tag(&id), error = %scrubbed(&error), "failed to fetch avatar");
            }
        }
    }
    debug!(found, none, failed, "fetched avatars");
}

/// Crops to a square and scales to [`AVATAR_SIDE`], off the runtime's thread: decoding and
/// resizing take long enough to stall everything else on it.
async fn decode_avatar(bytes: Vec<u8>) -> Option<Picture> {
    tokio::task::spawn_blocking(move || {
        let image = attachments::decode_limited(&bytes)?;
        let square = image.resize_to_fill(
            AVATAR_SIDE,
            AVATAR_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
        Some(Picture {
            width: AVATAR_SIDE,
            height: AVATAR_SIDE,
            rgba: square.into_rgba8().into_raw(),
        })
    })
    .await
    .ok()
    .flatten()
}
