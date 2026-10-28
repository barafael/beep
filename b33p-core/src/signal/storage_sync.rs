//! Keeping chat settings and verifications in step with the phone's storage service.

use std::time::Duration;

use presage::proto::{
    SyncMessage,
    sync_message::{self, Content as SyncContent},
};
use tracing::warn;

use crate::{Event, scrub::scrubbed};

use super::{
    Ctx, Mgr,
    send::send_sync,
    trust::{Verifier, remember_verification},
};

/// Takes over chat settings from the phone's storage service now, and each time the phone
/// says they changed, for the rest of the session.
pub(super) async fn follow_storage(mut manager: Mgr, ctx: Ctx) {
    const RETRY: Duration = Duration::from_secs(5 * 60);
    // Changes made here come in bursts (pinning, reordering); write them together.
    const SETTLE: Duration = Duration::from_secs(2);
    loop {
        match crate::storage::sync(&manager, &ctx.tables, &ctx.events).await {
            Ok((known, verifications)) => {
                take_verifications(&manager, &ctx, verifications).await;
                if known && !ctx.events.sent(Event::StorageRead) {
                    return;
                }
                if known {
                    ctx.storage_read.send_replace(true);
                }
                if known && ctx.preferences.get().sync_to_phone {
                    write_storage(&mut manager, &ctx).await;
                }
                if known {
                    ctx.storage_changed.notified().await;
                } else {
                    // The phone sends the key right after linking (`handle_content` wakes
                    // this when it comes); if that was missed, look again later rather than
                    // never, since without it message requests can't be told apart.
                    tokio::select! {
                        () = tokio::time::sleep(RETRY) => {}
                        () = ctx.storage_changed.notified() => {}
                    }
                }
                tokio::time::sleep(SETTLE).await;
            }
            // E.g. offline at the start; try again later.
            Err(error) => {
                warn!(error = %scrubbed(&error), "failed to read the storage service");
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => {}
                    () = ctx.storage_changed.notified() => {}
                }
            }
        }
    }
}

/// Takes over the verifications the storage service says the phone changed (quietly on the
/// first read, which brings what was set before), then keeps what it said.
async fn take_verifications(
    manager: &Mgr,
    ctx: &Ctx,
    verifications: Option<crate::storage::Verifications>,
) {
    let Some(verifications) = verifications else {
        return;
    };
    for (who, verification) in &verifications.changed {
        let Ok(key) =
            presage::libsignal_service::protocol::IdentityKey::decode(&verification.identity_key)
        else {
            continue;
        };
        let verifier = Verifier::Elsewhere {
            quiet: verifications.first,
        };
        // One failing doesn't hold back the others: the storage service won't be read for them
        // again until the phone changes something.
        if let Err(error) =
            remember_verification(manager, ctx, *who, &key, verification.state, verifier).await
        {
            warn!(error = %scrubbed(&error), "failed to keep a verification from the phone");
        }
    }
    if let Err(error) = ctx
        .tables
        .set_remote_verifications(&verifications.now)
        .await
    {
        warn!(error = %scrubbed(&error), "failed to keep what the phone says about verifications");
    }
}

/// Writes what changed here to the storage service, reading first when the phone changed
/// something meanwhile; then tells our other devices to fetch it.
async fn write_storage(manager: &mut Mgr, ctx: &Ctx) {
    use crate::storage::Written;
    for _ in 0..3 {
        match crate::storage::push(manager, &ctx.tables).await {
            Ok(Written::Stale) => {
                match crate::storage::sync(manager, &ctx.tables, &ctx.events).await {
                    Ok((_, verifications)) => take_verifications(manager, ctx, verifications).await,
                    Err(error) => {
                        warn!(error = %scrubbed(&error), "failed to read the storage service");
                        return;
                    }
                }
            }
            Ok(Written::Records(_)) => {
                use sync_message::fetch_latest::Type;
                let latest = sync_message::FetchLatest {
                    r#type: Some(Type::StorageManifest as i32),
                };
                let sync = SyncMessage {
                    content: Some(SyncContent::FetchLatest(latest)),
                    ..Default::default()
                };
                send_sync(manager, &ctx.outbox, sync).await;
                return;
            }
            Ok(Written::Nothing | Written::NoKey) => return,
            Err(error) => {
                warn!(error = %scrubbed(&error), "failed to write to the storage service");
                return;
            }
        }
    }
    warn!("the storage service kept changing; trying again later");
}
