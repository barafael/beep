//! Keeping chat settings and verifications in step with the phone's storage service.

use std::time::Duration;

use presage::proto::{
    SyncMessage,
    sync_message::{self, Content as SyncContent},
};
use tracing::warn;

use crate::{Event, Preferences, local::AccountPreferences};

use super::{
    Ctx, ForSession, Mgr, Ports,
    send::send_sync,
    trust::{Verifier, remember_verification},
};

/// Takes over chat settings from the phone's storage service now, and each time the phone
/// says they changed, for the rest of the session. The verifications it reads go to the session
/// to take over.
pub(super) async fn follow_storage(mut manager: Mgr, ctx: Ports) {
    const RETRY: Duration = Duration::from_secs(5 * 60);
    // Changes made here come in bursts (pinning, reordering); write them together.
    const SETTLE: Duration = Duration::from_secs(2);
    // The most a run of changes holds a read back.
    const LONGEST: Duration = Duration::from_secs(10);
    let mut unplaced = crate::storage::Unplaced::default();
    loop {
        match crate::storage::sync(&manager, &ctx.tables, &ctx.events).await {
            Ok(synced) => {
                let known = synced.known;
                retitle(&ctx, synced.names_changed);
                hand_over(&ctx, synced.verifications);
                take_preferences(&ctx, synced.preferences);
                if known && !ctx.events.sent(Event::StorageRead) {
                    return;
                }
                if known {
                    ctx.storage_read.send_replace(true);
                }
                if known && ctx.preferences.get().sync_to_phone {
                    write_storage(&mut manager, &ctx, &mut unplaced).await;
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
                settle(&ctx.storage_changed, SETTLE, LONGEST).await;
            }
            // E.g. offline at the start; try again later.
            Err(error) => {
                warn!(error = %crate::scrub::chain(&error), "failed to read the storage service");
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => {}
                    () = ctx.storage_changed.notified() => {}
                }
            }
        }
    }
}

/// Waits until nothing has said the storage service changed for `quiet`, but no longer than
/// `longest` in all, so that a run of changes (the phone writes a record for each chat it
/// touches, one after the other) is read once when it is over, not once for each of them. A
/// change that comes after leaves a wake for the next round, as `Notify` keeps one.
async fn settle(changed: &tokio::sync::Notify, quiet: Duration, longest: Duration) {
    let end = tokio::time::Instant::now() + longest;
    loop {
        let wait = quiet.min(end.saturating_duration_since(tokio::time::Instant::now()));
        if wait.is_zero() {
            return;
        }
        tokio::select! {
            () = changed.notified() => {}
            () = tokio::time::sleep(wait) => return,
        }
    }
}

/// Takes over the account's settings the phone changed, for the UI to keep. Link previews only
/// turn off from there: Signal's apps have them on by default, and b33p only if they were turned
/// on here, since the linked site sees our address.
fn take_preferences(ctx: &Ports, account: Option<AccountPreferences>) {
    let Some(account) = account else {
        return;
    };
    let before = ctx.preferences.get();
    let now = taken(before, account);
    if now != before {
        ctx.preferences.set(now);
        ctx.events.send(Event::Preferences(now));
    }
}

fn taken(before: Preferences, account: AccountPreferences) -> Preferences {
    Preferences {
        read_receipts: account.read_receipts,
        typing_indicators: account.typing_indicators,
        link_previews: before.link_previews && account.link_previews,
        sync_to_phone: before.sync_to_phone,
        high_quality_pictures: before.high_quality_pictures,
    }
}

/// Hands the verifications read to the session, which takes them over.
/// Titles the chats anew when the phone's names for contacts changed.
fn retitle(ctx: &Ports, names_changed: bool) {
    if names_changed {
        ctx.session.send(ForSession::Conversations).ok();
    }
}

fn hand_over(ctx: &Ports, verifications: Option<crate::storage::Verifications>) {
    if let Some(verifications) = verifications {
        ctx.session
            .send(ForSession::Verifications(verifications))
            .ok();
    }
}

/// Takes over the verifications the storage service says the phone changed (quietly on the
/// first read, which brings what was set before), then keeps what it said.
pub(super) async fn take_verifications(
    manager: &Mgr,
    ctx: &Ctx,
    verifications: crate::storage::Verifications,
) {
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
            warn!(error = %crate::scrub::chain(&error), "failed to keep a verification from the phone");
        }
    }
    if let Err(error) = ctx
        .tables
        .set_remote_verifications(&verifications.now)
        .await
    {
        warn!(error = %crate::scrub::chain(&error), "failed to keep what the phone says about verifications");
    }
}

/// Writes what changed here to the storage service, reading first when the phone changed
/// something meanwhile; then tells our other devices to fetch it.
async fn write_storage(manager: &mut Mgr, ctx: &Ports, unplaced: &mut crate::storage::Unplaced) {
    use crate::storage::Written;
    for _ in 0..3 {
        match crate::storage::push(manager, &ctx.tables, unplaced).await {
            Ok(Written::Stale) => {
                match crate::storage::sync(manager, &ctx.tables, &ctx.events).await {
                    Ok(synced) => {
                        retitle(ctx, synced.names_changed);
                        hand_over(ctx, synced.verifications);
                        take_preferences(ctx, synced.preferences);
                    }
                    Err(error) => {
                        warn!(error = %crate::scrub::chain(&error), "failed to read the storage service");
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
                warn!(error = %crate::scrub::chain(&error), "failed to write to the storage service");
                return;
            }
        }
    }
    warn!("the storage service kept changing; trying again later");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Instant;

    /// Time stands still for a test except where it sleeps, so these take no time and can't be
    /// late.
    async fn settled_after(changes: &[Duration], quiet: Duration, longest: Duration) -> Duration {
        let changed = tokio::sync::Notify::new();
        let start = Instant::now();
        let waiting = settle(&changed, quiet, longest);
        let making = async {
            for at in changes {
                tokio::time::sleep_until(start + *at).await;
                changed.notify_one();
            }
            std::future::pending::<()>().await;
        };
        tokio::select! {
            () = waiting => start.elapsed(),
            () = making => unreachable!("it never ends"),
        }
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[tokio::test(start_paused = true)]
    async fn a_quiet_moment_ends_the_wait() {
        let took = settled_after(&[], 2 * SECOND, 10 * SECOND).await;
        assert_eq!(took, 2 * SECOND);
    }

    /// The phone writes a record for each chat it touches, a second or so apart: one read after,
    /// not one for each.
    #[tokio::test(start_paused = true)]
    async fn a_run_of_changes_is_waited_out_and_read_once() {
        let changes = [SECOND / 2, SECOND, 2 * SECOND, 3 * SECOND];
        let took = settled_after(&changes, 2 * SECOND, 10 * SECOND).await;
        assert_eq!(took, 5 * SECOND);
    }

    #[tokio::test(start_paused = true)]
    async fn changes_that_never_stop_hold_the_read_back_only_so_long() {
        let changes: Vec<Duration> = (1..60).map(|n| n * SECOND).collect();
        let took = settled_after(&changes, 2 * SECOND, 10 * SECOND).await;
        assert_eq!(took, 10 * SECOND);
    }

    #[test]
    fn the_phone_turns_link_previews_off_but_not_on() {
        let account = |link_previews| AccountPreferences {
            read_receipts: false,
            typing_indicators: true,
            link_previews,
        };
        let before = |link_previews| Preferences {
            read_receipts: true,
            typing_indicators: false,
            link_previews,
            sync_to_phone: true,
            // b33p's own: the phone's settings don't touch it.
            high_quality_pictures: true,
        };
        for (here, phone, now) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert_eq!(
                taken(before(here), account(phone)),
                Preferences {
                    read_receipts: false,
                    typing_indicators: true,
                    link_previews: now,
                    sync_to_phone: true,
                    high_quality_pictures: true,
                }
            );
        }
    }
}
