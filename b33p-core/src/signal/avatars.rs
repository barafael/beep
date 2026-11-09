//! Profile and group pictures for the UI: fetched from presage, cropped and scaled down.

use std::sync::Arc;

use presage::{
    Error as PresageError,
    libsignal_service::{
        content::GroupContextV2,
        protocol::{Aci, ServiceId},
        push_service::ServiceError,
    },
    store::{ContentsStore, Thread},
};
use presage_store_sqlite::SqliteStoreError;
use tracing::{debug, warn};

use tokio::sync::mpsc;

use crate::{ConversationId, Event, Events, Picture, attachments};

use super::{
    Ctx, Mgr,
    conversations::{conversation_id, tag},
};

/// Sends a group's picture as presage has it now (see [`fetch_avatars`]), or that it has none.
pub(super) async fn send_group_avatar(mut manager: Mgr, ctx: &Ctx, key: [u8; 32]) {
    let avatar = match group_picture(&mut manager, key).await {
        Ok(Some(bytes)) => decode_avatar(bytes).await.map(Arc::new),
        Ok(None) => None,
        Err(error) => {
            warn!(error = %crate::scrub::chain(&error), "failed to fetch the group's picture");
            return;
        }
    };
    ctx.events.send(Event::Avatar {
        conversation: conversation_id(&Thread::Group(key)),
        avatar,
    });
}

/// Sends a contact's picture as presage has it now (see [`contact_picture`]), or that they have
/// none.
pub(super) async fn send_contact_avatar(mut manager: Mgr, ctx: &Ctx, aci: Aci) {
    let avatar = match contact_picture(&mut manager, aci).await {
        Ok(Some(bytes)) => decode_avatar(bytes).await.map(Arc::new),
        Ok(None) => None,
        Err(error) => {
            warn!(error = %crate::scrub::chain(&error), "failed to fetch a contact's picture");
            return;
        }
    };
    ctx.events.send(Event::Avatar {
        conversation: conversation_id(&Thread::Contact(ServiceId::Aci(aci))),
        avatar,
    });
}

/// The group's picture as presage has it now, if it has one.
async fn group_picture(
    manager: &mut Mgr,
    key: [u8; 32],
) -> Result<Option<Vec<u8>>, PresageError<SqliteStoreError>> {
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

/// A contact's profile picture. It needs their profile key, which only arrives with their
/// messages (the contact sync no longer carries it); until then, or without one, the picture
/// from the contact sync, i.e. what the phone shows, stands in.
async fn contact_picture(
    manager: &mut Mgr,
    aci: Aci,
) -> Result<Option<Vec<u8>>, PresageError<SqliteStoreError>> {
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

/// Side of decoded avatars: a 40 px circle at up to 3x scaling.
const AVATAR_SIDE: u32 = 128;

/// Where the session asks [`Avatars`] for the pictures of the threads it has now.
pub(super) type AvatarRequests = mpsc::UnboundedSender<Vec<(ConversationId, Thread)>>;

/// Runs [`fetch_avatars`] one pass at a time: what is asked during a pass, the latest of it,
/// makes one more pass after it, instead of a second pass sending the same requests alongside.
pub(super) struct Avatars {
    manager: Mgr,
    events: Events,
}

impl Avatars {
    pub(super) fn new(manager: Mgr, events: Events) -> Self {
        Self { manager, events }
    }

    /// Until every [`AvatarRequests`] is dropped and the last pass is done.
    pub(super) async fn event_loop(
        self,
        mut asked: mpsc::UnboundedReceiver<Vec<(ConversationId, Thread)>>,
    ) -> Self {
        while let Some(mut threads) = asked.recv().await {
            while let Ok(newer) = asked.try_recv() {
                threads = newer;
            }
            fetch_avatars(self.manager.clone(), threads, self.events.clone()).await;
        }
        self
    }
}

/// Sends each conversation's profile or group picture. presage caches them in the store, so
/// only the first run downloads them, one at a time to stay clear of rate limits; a contact's
/// is fetched again when their details are opened (`Command::LoadDetails`).
async fn fetch_avatars(mut manager: Mgr, threads: Vec<(ConversationId, Thread)>, events: Events) {
    let (mut found, mut none, mut failed) = (0, 0, 0);
    for (id, thread) in threads {
        let picture = match thread {
            Thread::Contact(ServiceId::Aci(aci)) => contact_picture(&mut manager, aci).await,
            Thread::Contact(_) => continue,
            Thread::Group(key) => group_picture(&mut manager, key).await,
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
                debug!(conversation = %tag(&id), error = %crate::scrub::chain(&error), "failed to fetch avatar");
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
