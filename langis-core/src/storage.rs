//! Reading the storage service, where the phone keeps the account's settings and those of each
//! contact and group: which chats are pinned, muted, archived or marked unread.
//!
//! langis doesn't write there yet, so its own changes must survive a sync: a setting is taken
//! from the phone only if it changed there since the last sync (what the phone said then is kept
//! in `langis_remote`). Pins work the same way, and pins made only in langis stay after the
//! phone's (Signal allows the phone four).

use std::collections::{HashMap, HashSet};

use presage::{
    libsignal_service::{StorageService, protocol::ServiceId},
    proto::{
        account_record::pinned_conversation::Identifier as Pinned, manifest_record::identifier,
        storage_record::Record,
    },
};
use tracing::{debug, info};

use crate::{ChatSettings, ConversationId, Event, Events, local::LocalStore, signal::Mgr};

/// Items read per request.
const BATCH: usize = 500;

/// Takes over what changed on the phone since the last time; tells the UI what changed here.
pub(crate) async fn sync(
    manager: &Mgr,
    tables: &LocalStore,
    events: &Events,
) -> Result<(), String> {
    let Some(service) = manager
        .storage_service()
        .await
        .map_err(|e| format!("failed to reach the storage service: {e}"))?
    else {
        debug!("no storage key yet; the phone hasn't sent it");
        return Ok(());
    };
    let known = tables.storage_version().await.map_err(db)?;
    let Some(manifest) = service
        .manifest_if_changed(known)
        .await
        .map_err(|e| format!("failed to read the storage manifest: {e}"))?
    else {
        return Ok(());
    };
    let wanted: Vec<Vec<u8>> = manifest
        .identifiers
        .iter()
        .filter(|id| {
            matches!(
                id.r#type(),
                identifier::Type::Contact | identifier::Type::Groupv2 | identifier::Type::Account
            )
        })
        .map(|id| id.raw.clone())
        .collect();
    let ikm = (!manifest.record_ikm.is_empty()).then_some(&manifest.record_ikm[..]);
    let mut remote = HashMap::new();
    let mut pins = Vec::new();
    for keys in wanted.chunks(BATCH) {
        let records = read(&service, keys.to_vec(), ikm).await?;
        for record in records {
            match record {
                Record::Contact(contact) => {
                    let aci = ServiceId::parse_from_service_id_string(&contact.aci)
                        .or_else(|| ServiceId::parse_from_service_id_binary(&contact.aci_binary));
                    if let Some(aci) = aci {
                        remote.insert(
                            ConversationId::contact(&aci.service_id_string()),
                            settings(
                                contact.archived,
                                contact.marked_unread,
                                contact.muted_until_timestamp,
                            ),
                        );
                    }
                }
                Record::GroupV2(group) => {
                    remote.insert(
                        ConversationId(format!("g:{}", hex::encode(&group.master_key))),
                        settings(
                            group.archived,
                            group.marked_unread,
                            group.muted_until_timestamp,
                        ),
                    );
                }
                Record::Account(account) => {
                    pins = account
                        .pinned_conversations
                        .iter()
                        .filter_map(|pinned| match pinned.identifier.as_ref()? {
                            Pinned::Contact(contact) => {
                                let who =
                                    ServiceId::parse_from_service_id_string(&contact.service_id)
                                        .or_else(|| {
                                            ServiceId::parse_from_service_id_binary(
                                                &contact.service_id_binary,
                                            )
                                        })?;
                                Some(ConversationId::contact(&who.service_id_string()))
                            }
                            Pinned::GroupMasterKey(key) => {
                                Some(ConversationId(format!("g:{}", hex::encode(key))))
                            }
                            _ => None,
                        })
                        .collect();
                }
                _ => {}
            }
        }
    }
    for (position, id) in pins.iter().enumerate() {
        remote.entry(id.clone()).or_default().pinned = Some(position as u32);
    }
    let local = tables.conversations().await.map_err(db)?;
    let before = tables.remote().await.map_err(db)?;
    let changed = merge(&local, &before, &remote);
    tables.save_settings(&changed).await.map_err(db)?;
    tables
        .set_remote(&remote, manifest.version)
        .await
        .map_err(db)?;
    info!(
        version = manifest.version,
        changed = changed.len(),
        "read the storage service"
    );
    for (conversation, settings) in changed {
        if !events.send(Event::Settings {
            conversation,
            settings,
        }) {
            break;
        }
    }
    Ok(())
}

async fn read(
    service: &StorageService,
    keys: Vec<Vec<u8>>,
    ikm: Option<&[u8]>,
) -> Result<Vec<Record>, String> {
    let records = service
        .read_items(keys, ikm)
        .await
        .map_err(|e| format!("failed to read storage records: {e}"))?;
    Ok(records.into_iter().filter_map(|r| r.record).collect())
}

fn settings(archived: bool, marked_unread: bool, muted_until: u64) -> ChatSettings {
    ChatSettings {
        archived,
        marked_unread,
        muted_until: (muted_until > 0).then_some(muted_until),
        ..ChatSettings::default()
    }
}

fn db(error: sqlx::Error) -> String {
    format!("failed to save the storage service's settings: {error}")
}

/// The local settings to change: those the phone changed since it said `before` (or that it
/// says for the first time), with `now` being what it says now. Read positions are kept.
fn merge(
    local: &HashMap<ConversationId, ChatSettings>,
    before: &HashMap<ConversationId, ChatSettings>,
    now: &HashMap<ConversationId, ChatSettings>,
) -> Vec<(ConversationId, ChatSettings)> {
    let mut merged: HashMap<ConversationId, ChatSettings> = local.clone();
    let unknown = ChatSettings::default();
    for (id, remote) in now {
        let old = before.get(id);
        let settings = merged.entry(id.clone()).or_default();
        if old.is_none_or(|old| old.archived != remote.archived) {
            settings.archived = remote.archived;
        }
        if old.is_none_or(|old| old.marked_unread != remote.marked_unread) {
            settings.marked_unread = remote.marked_unread;
        }
        if old.is_none_or(|old| old.muted_until != remote.muted_until) {
            settings.muted_until = remote.muted_until;
        }
    }
    // Pins: the phone's in its order if they changed there, then those only pinned here.
    let pinned = |map: &HashMap<ConversationId, ChatSettings>| {
        let mut pins: Vec<(u32, ConversationId)> = map
            .iter()
            .filter_map(|(id, s)| Some((s.pinned?, id.clone())))
            .collect();
        pins.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.0.cmp(&b.1.0)));
        pins.into_iter().map(|(_, id)| id).collect::<Vec<_>>()
    };
    let (before_pins, now_pins) = (pinned(before), pinned(now));
    if before_pins != now_pins {
        let theirs: HashSet<&ConversationId> = before_pins.iter().chain(&now_pins).collect();
        let mut order = now_pins.clone();
        order.extend(pinned(local).into_iter().filter(|id| !theirs.contains(id)));
        for (id, settings) in merged.iter_mut() {
            settings.set_pinned(order.iter().position(|p| p == id).map(|at| at as u32));
        }
        for (position, id) in order.iter().enumerate() {
            merged
                .entry(id.clone())
                .or_default()
                .set_pinned(Some(position as u32));
        }
    }
    let mut changed: Vec<_> = merged
        .into_iter()
        .filter(|(id, settings)| local.get(id).unwrap_or(&unknown) != settings)
        .collect();
    changed.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> ConversationId {
        ConversationId(s.into())
    }

    fn map(entries: &[(&str, ChatSettings)]) -> HashMap<ConversationId, ChatSettings> {
        entries.iter().map(|(k, v)| (id(k), v.clone())).collect()
    }

    fn pinned(at: u32) -> ChatSettings {
        ChatSettings {
            pinned: Some(at),
            ..ChatSettings::default()
        }
    }

    #[test]
    fn only_what_changed_on_the_phone_is_taken() {
        let archived = ChatSettings {
            archived: true,
            ..ChatSettings::default()
        };
        // Unarchived here, still archived on the phone as before: stays unarchived.
        let local = map(&[("a", ChatSettings::default())]);
        let before = map(&[("a", archived.clone())]);
        assert!(merge(&local, &before, &before).is_empty());
        // Archived on the phone since: taken.
        let changed = merge(&local, &map(&[("a", ChatSettings::default())]), &before);
        assert_eq!(changed, [(id("a"), archived.clone())]);
        // First sync: everything the phone says is taken, read positions kept.
        let local = map(&[(
            "b",
            ChatSettings {
                last_read: 7,
                ..ChatSettings::default()
            },
        )]);
        let changed = merge(&local, &HashMap::new(), &map(&[("b", archived)]));
        assert_eq!(changed[0].1.last_read, 7);
        assert!(changed[0].1.archived);
    }

    #[test]
    fn pins_from_the_phone_come_first_and_local_ones_stay() {
        // Pinned here: "x"; the phone pinned "a" then "b".
        let local = map(&[("x", pinned(0))]);
        let now = map(&[("a", pinned(0)), ("b", pinned(1))]);
        let changed: HashMap<_, _> = merge(&local, &HashMap::new(), &now).into_iter().collect();
        assert_eq!(changed[&id("a")].pinned, Some(0));
        assert_eq!(changed[&id("b")].pinned, Some(1));
        assert_eq!(changed[&id("x")].pinned, Some(2));
        // Unchanged on the phone: local reordering stays.
        let local = map(&[("b", pinned(0)), ("a", pinned(1))]);
        assert!(merge(&local, &now, &now).is_empty());
        // Unpinned on the phone: unpinned here too.
        let later = map(&[("b", pinned(0))]);
        let changed: HashMap<_, _> = merge(&local, &now, &later).into_iter().collect();
        assert_eq!(changed[&id("a")].pinned, None);
    }
}
