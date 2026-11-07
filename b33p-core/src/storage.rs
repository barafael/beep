//! Reading the storage service, where the phone keeps the account's settings and those of each
//! contact and group: which chats are pinned, muted, archived or marked unread, and whose safety
//! number was verified.
//!
//! b33p writes there only when asked to sync to the phone, so its own changes must survive a
//! sync: a setting is taken from the phone only if it changed there since the last sync (what
//! the phone said then is kept in `b33p_remote`). Pins work the same way, and pins made only in
//! b33p stay after the phone's (Signal allows the phone four).

use std::collections::{HashMap, HashSet};

use presage::{
    libsignal_service::{StorageService, protocol::ServiceId},
    proto::{
        account_record::pinned_conversation::Identifier as Pinned, manifest_record::identifier,
        storage_record::Record,
    },
};
use tracing::{debug, info};

use crate::signal::errors::{http_kind, kind_of, service_kind, storage_service_kind};
use crate::{
    ChatSettings, ConversationId, ErrorKind, Event, Events,
    local::{AccountPreferences, LocalStore, Verification},
    signal::Mgr,
};

type PresageError = presage::Error<presage_store_sqlite::SqliteStoreError>;
type ServiceError = presage::libsignal_service::prelude::ServiceError;
type StorageServiceError = presage::libsignal_service::StorageServiceError;

/// What fails in reading the storage service or writing to it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StorageError {
    #[error("failed to reach the storage service")]
    Reach(#[source] PresageError),
    #[error("failed to read the storage manifest")]
    ReadManifest(#[source] StorageServiceError),
    #[error("failed to read storage records")]
    ReadRecords(#[source] StorageServiceError),
    #[error("the storage key has {len} bytes, not 32")]
    KeyLength { len: usize },
    #[error("failed to make a request to the storage service's {endpoint}")]
    Request {
        endpoint: &'static str,
        source: ServiceError,
    },
    #[error("the storage service's {endpoint} failed")]
    Http {
        endpoint: &'static str,
        source: reqwest::Error,
    },
    #[error("the storage service's {endpoint} answered with something other than {what}")]
    Decode {
        endpoint: &'static str,
        what: &'static str,
        source: prost::DecodeError,
    },
    #[error("a storage blob of {len} bytes doesn't decrypt")]
    Decrypt { len: usize },
    /// The manifest as read, which is to be changed to write records.
    #[error("the storage manifest doesn't decode")]
    InvalidManifest(#[source] prost::DecodeError),
    #[error("the storage manifest couldn't be changed")]
    ManifestUnchangeable,
    #[error("the changed storage manifest doesn't decode")]
    ChangedManifestInvalid(#[source] prost::DecodeError),
    #[error("the changed storage manifest doesn't add up; not writing it")]
    ChangedManifestWrong,
    #[error("failed to {step}")]
    Tables { step: Step, source: sqlx::Error },
}

/// What was being done with b33p's records of the storage service.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Step {
    ReadVersion,
    ReadVerificationsRead,
    ReadPreferences,
    KeepPreferences,
    KeepFolders,
    ReadNames,
    KeepNames,
    ReadVerifications,
    TakeSettings,
    ReadSettings,
    ReadRemote,
    KeepWritten,
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReadVersion => "read the storage manifest's version last read",
            Self::ReadVerificationsRead => "read whether the phone's verifications were read",
            Self::ReadPreferences => "read the account's settings as the phone said them last",
            Self::KeepPreferences => "keep the account's settings as the phone says them",
            Self::KeepFolders => "keep the chat folders",
            Self::ReadNames => "read the contacts' names as the phone said them last",
            Self::KeepNames => "keep the contacts' names as the phone says them",
            Self::ReadVerifications => "read the verifications as the phone said them last",
            Self::TakeSettings => "take over the chats' settings from the phone",
            Self::ReadSettings => "read the chats' settings",
            Self::ReadRemote => "read the chats' settings as the phone said them last",
            Self::KeepWritten => "keep what was written to the storage service",
        })
    }
}

impl crate::Classified for StorageError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Reach(error) => kind_of(error),
            Self::ReadManifest(error) | Self::ReadRecords(error) => storage_service_kind(error),
            Self::Request { source, .. } => service_kind(source),
            Self::Http { source, .. } => http_kind(source),
            Self::KeyLength { .. }
            | Self::Decode { .. }
            | Self::Decrypt { .. }
            | Self::InvalidManifest(_)
            | Self::ManifestUnchangeable
            | Self::ChangedManifestInvalid(_)
            | Self::ChangedManifestWrong => ErrorKind::Refused,
            Self::Tables { .. } => ErrorKind::Storage,
        }
    }
}

/// For `map_err` on b33p's records: failing at `step`.
fn at(step: Step) -> impl FnOnce(sqlx::Error) -> StorageError {
    move |source| StorageError::Tables { step, source }
}

/// Items read per request.
const BATCH: usize = 500;

/// Verifications a read of the storage service found: those the phone changed since the last
/// read, for the caller to take over (it notes them in the chats), and all it says now, to keep
/// once they are; see [`LocalStore::set_remote_verifications`].
pub(crate) struct Verifications {
    /// The first read of them: what was set before b33p read them, taken over quietly.
    pub first: bool,
    pub changed: Vec<(ServiceId, Verification)>,
    pub now: HashMap<String, Verification>,
}

/// What a read of the storage service found for the caller to take over.
#[derive(Default)]
pub(crate) struct Synced {
    /// Whether b33p knows what the phone says: not before the phone has sent the storage key.
    pub known: bool,
    pub verifications: Option<Verifications>,
    /// The account's settings, if they changed on the phone since the last read.
    pub preferences: Option<AccountPreferences>,
    /// Whether the names the phone gives contacts changed, so the chats are to be titled anew.
    pub names_changed: bool,
}

/// Takes over what changed on the phone since the last time; tells the UI what changed here.
pub(crate) async fn sync(
    manager: &Mgr,
    tables: &LocalStore,
    events: &Events,
) -> Result<Synced, StorageError> {
    let Some(service) = manager
        .storage_service()
        .await
        .map_err(StorageError::Reach)?
    else {
        debug!("no storage key yet; the phone hasn't sent it");
        return Ok(Synced::default());
    };
    // Verifications came later than the rest: read everything again once to get them.
    let first = !tables
        .remote_verifications_read()
        .await
        .map_err(at(Step::ReadVerificationsRead))?;
    // So did contacts' names.
    let names_unread = !tables
        .contact_names_read()
        .await
        .map_err(at(Step::ReadNames))?;
    let known = if first || names_unread {
        0
    } else {
        tables
            .storage_version()
            .await
            .map_err(at(Step::ReadVersion))?
    };
    let Some(manifest) = service
        .manifest_if_changed(known)
        .await
        .map_err(StorageError::ReadManifest)?
    else {
        return Ok(Synced {
            known: true,
            ..Synced::default()
        });
    };
    let wanted: Vec<Vec<u8>> = manifest
        .identifiers
        .iter()
        .filter(|id| {
            matches!(
                id.r#type(),
                identifier::Type::Contact
                    | identifier::Type::Groupv2
                    | identifier::Type::Account
                    | identifier::Type::ChatFolder
            )
        })
        .map(|id| id.raw.clone())
        .collect();
    let ikm = (!manifest.record_ikm.is_empty()).then_some(&manifest.record_ikm[..]);
    let mut remote = HashMap::new();
    let mut verifications = HashMap::new();
    let mut names = HashMap::new();
    let mut pins = Vec::new();
    let mut folders = Vec::new();
    let mut account_preferences = None;
    for keys in wanted.chunks(BATCH) {
        let records = read(&service, keys.to_vec(), ikm).await?;
        for record in records {
            match record {
                Record::Contact(contact) => {
                    let aci = ServiceId::parse_from_service_id_string(&contact.aci)
                        .or_else(|| ServiceId::parse_from_service_id_binary(&contact.aci_binary));
                    if let Some(aci) = aci
                        && let Some(verification) = verification(&contact)
                    {
                        verifications.insert(aci.service_id_string(), verification);
                    }
                    if let Some(aci) = aci
                        && let Some(name) = shown_name(&contact)
                    {
                        names.insert(aci.service_id_string(), name);
                    }
                    if let Some(aci) = aci {
                        remote.insert(
                            ConversationId::contact(&aci.service_id_string()),
                            settings(
                                contact.archived,
                                contact.marked_unread,
                                contact.muted_until_timestamp,
                                contact.blocked,
                                contact.whitelisted,
                            ),
                        );
                    }
                }
                Record::GroupV2(group) => {
                    remote.insert(
                        ConversationId::group(&group.master_key),
                        settings(
                            group.archived,
                            group.marked_unread,
                            group.muted_until_timestamp,
                            group.blocked,
                            group.whitelisted,
                        ),
                    );
                }
                Record::Account(account) => {
                    account_preferences = Some(AccountPreferences {
                        read_receipts: account.read_receipts,
                        typing_indicators: account.typing_indicators,
                        link_previews: account.link_previews,
                    });
                    pins = account
                        .pinned_conversations
                        .iter()
                        .filter_map(|pinned| match pinned.identifier.as_ref()? {
                            Pinned::Contact(contact) => contact_conversation(
                                &contact.service_id,
                                &contact.service_id_binary,
                            ),
                            Pinned::GroupMasterKey(key) => Some(ConversationId::group(key)),
                            _ => None,
                        })
                        .collect();
                }
                Record::ChatFolder(folder) => {
                    use presage::proto::chat_folder_record::FolderType;
                    if folder.folder_type() == FolderType::Custom
                        && folder.deleted_at_timestamp_ms == 0
                    {
                        folders.push((folder.position, chat_folder(folder)));
                    }
                }
                _ => {}
            }
        }
    }
    let preferences = match account_preferences {
        Some(now)
            if tables
                .remote_preferences()
                .await
                .map_err(at(Step::ReadPreferences))?
                != Some(now) =>
        {
            tables
                .set_remote_preferences(now)
                .await
                .map_err(at(Step::KeepPreferences))?;
            Some(now)
        }
        Some(_) | None => None,
    };
    folders.sort_by_key(|(position, _)| *position);
    let folders: Vec<_> = folders.into_iter().map(|(_, folder)| folder).collect();
    tables
        .set_folders(&folders)
        .await
        .map_err(at(Step::KeepFolders))?;
    events.send(Event::Folders(folders));
    // Verifications the phone changed since last time; ours stand otherwise. The caller takes
    // them over (with notes in the chats) and then keeps what the phone said.
    let verified_before = tables
        .remote_verifications()
        .await
        .map_err(at(Step::ReadVerifications))?;
    let verified_changed: Vec<(ServiceId, Verification)> =
        changed_verifications(&verified_before, &verifications)
            .into_iter()
            .filter_map(|(aci, v)| {
                Some((
                    ServiceId::parse_from_service_id_string(aci)?,
                    Verification::clone(v),
                ))
            })
            .collect();
    let names_before = tables.contact_names().await.map_err(at(Step::ReadNames))?;
    let names_changed = names != names_before;
    if names_changed || names_unread {
        tables
            .set_contact_names(&names)
            .await
            .map_err(at(Step::KeepNames))?;
    }
    set_pins(&mut remote, &pins);
    let changed = tables
        .take_remote(&remote, manifest.version, merge)
        .await
        .map_err(at(Step::TakeSettings))?;
    info!(
        version = manifest.version,
        changed = changed.len(),
        "read the storage service"
    );
    for (conversation, settings) in changed {
        if !events.sent(Event::Settings {
            conversation,
            settings,
        }) {
            break;
        }
    }
    Ok(Synced {
        known: true,
        verifications: Some(Verifications {
            first,
            changed: verified_changed,
            now: verifications,
        }),
        preferences,
        names_changed,
    })
}

/// What Signal's apps title a contact's chat by, as the phone's record of them says: the
/// nickname given them, their name in the phone's address book, their profile name, their
/// username; `None` with none of these, when the number is all there is. Signal-Desktop's
/// `getTitle` (`util/getTitle.ts`) goes in this order.
pub(crate) fn shown_name(contact: &presage::proto::ContactRecord) -> Option<String> {
    let join = |given: &str, family: &str| format!("{} {}", given.trim(), family.trim());
    let nickname = contact
        .nickname
        .as_ref()
        .map(|name| join(&name.given, &name.family))
        .unwrap_or_default();
    [
        nickname,
        join(&contact.system_given_name, &contact.system_family_name),
        join(&contact.given_name, &contact.family_name),
        contact.username.clone(),
    ]
    .into_iter()
    .map(|name| name.trim().to_owned())
    .find(|name| !name.is_empty())
}

/// Puts the account record's pins on the phone's chats, archived ones left out: Signal unpins a
/// chat it archives, but Signal-Android writes only that chat's record then (ThreadTable.kt
/// `setArchived`, `markNeedsSyncWithoutRefresh(recipientIds)`), so the account record can still
/// list it. Taken as pinned there, a pin of it made here would look unchanged and never be
/// written, and the phone would stay without it.
fn set_pins(remote: &mut HashMap<ConversationId, ChatSettings>, pins: &[ConversationId]) {
    let shown: Vec<&ConversationId> = pins
        .iter()
        .filter(|id| !remote.get(*id).is_some_and(|settings| settings.archived))
        .collect();
    for (position, id) in (0u32..).zip(shown) {
        remote.entry(id.clone()).or_default().pinned = Some(position);
    }
}

/// The verifications that changed on the phone since it said `before`; the rest are left as
/// they are here, which may be newer.
fn changed_verifications<'a>(
    before: &HashMap<String, Verification>,
    now: &'a HashMap<String, Verification>,
) -> Vec<(&'a String, &'a Verification)> {
    let mut changed: Vec<_> = now
        .iter()
        .filter(|(aci, verification)| before.get(*aci) != Some(*verification))
        .collect();
    changed.sort_by(|a, b| a.0.cmp(b.0));
    changed
}

/// Whether the phone verified a contact's safety number (or saw it change since), and for which
/// key of theirs.
fn verification(contact: &presage::proto::ContactRecord) -> Option<Verification> {
    use presage::{
        libsignal_service::protocol::IdentityKey,
        proto::{contact_record::IdentityState, verified::State},
    };
    let key = IdentityKey::decode(&contact.identity_key).ok()?;
    Some(Verification {
        identity_key: key.serialize().to_vec(),
        state: match contact.identity_state() {
            IdentityState::Default => State::Default,
            IdentityState::Verified => State::Verified,
            IdentityState::Unverified => State::Unverified,
        },
    })
}

async fn read(
    service: &StorageService,
    keys: Vec<Vec<u8>>,
    ikm: Option<&[u8]>,
) -> Result<Vec<Record>, StorageError> {
    let records = service
        .read_items(keys, ikm)
        .await
        .map_err(StorageError::ReadRecords)?;
    Ok(records.into_iter().filter_map(|r| r.record).collect())
}

/// What a contact or group record says: archived, marked unread, muted until, blocked, and
/// whitelisted (accepted, not a message request).
fn settings(
    archived: bool,
    marked_unread: bool,
    muted_until: u64,
    blocked: bool,
    whitelisted: bool,
) -> ChatSettings {
    ChatSettings {
        archived,
        marked_unread,
        muted_until: (muted_until > 0).then_some(muted_until),
        blocked,
        accepted: whitelisted,
        ..ChatSettings::default()
    }
}

/// The chat of a contact a storage record names by service ID, as a string or (older
/// records) in binary.
fn contact_conversation(service_id: &str, binary: &[u8]) -> Option<ConversationId> {
    let who = ServiceId::parse_from_service_id_string(service_id)
        .or_else(|| ServiceId::parse_from_service_id_binary(binary))?;
    Some(ConversationId::contact(&who.service_id_string()))
}

fn chat_folder(folder: presage::proto::ChatFolderRecord) -> crate::ChatFolder {
    use presage::proto::recipient::Identifier;
    let ids = |recipients: &[presage::proto::Recipient]| -> Vec<ConversationId> {
        recipients
            .iter()
            .filter_map(|r| match r.identifier.as_ref()? {
                Identifier::Contact(contact) => {
                    contact_conversation(&contact.service_id, &contact.service_id_binary)
                }
                Identifier::GroupMasterKey(key) => Some(ConversationId::group(key)),
                Identifier::LegacyGroupId(_) => None,
            })
            .collect()
    };
    crate::ChatFolder {
        name: folder.name.clone(),
        only_unread: folder.show_only_unread,
        show_muted: folder.show_muted_chats,
        all_individual: folder.include_all_individual_chats,
        all_groups: folder.include_all_group_chats,
        included: ids(&folder.included_recipients),
        excluded: ids(&folder.excluded_recipients),
    }
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
            // Archiving unpins, as it does here.
            settings.set_archived(remote.archived);
        }
        if old.is_none_or(|old| old.marked_unread != remote.marked_unread) {
            settings.marked_unread = remote.marked_unread;
        }
        if old.is_none_or(|old| old.muted_until != remote.muted_until) {
            settings.muted_until = remote.muted_until;
        }
        if old.is_none_or(|old| old.blocked != remote.blocked) {
            settings.blocked = remote.blocked;
        }
        // Accepted on the phone; un-accepting isn't a thing.
        settings.accepted |= remote.accepted;
    }
    // Pins: the phone's in its order if they changed there, then those only pinned here.
    let (before_pins, now_pins) = (pinned_order(before), pinned_order(now));
    if before_pins != now_pins {
        let theirs: HashSet<&ConversationId> = before_pins.iter().chain(&now_pins).collect();
        let mut order = now_pins.clone();
        order.extend(
            pinned_order(local)
                .into_iter()
                .filter(|id| !theirs.contains(id)),
        );
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

// -- Writing ---------------------------------------------------------------------------------

/// How a write to the storage service went.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Written {
    /// Nothing differed from what the phone has.
    Nothing,
    /// This many records were changed.
    Records(usize),
    /// The phone changed something since the last read: read, then try again.
    Stale,
    /// No storage key yet.
    NoKey,
}

/// Signal shows at most this many pinned chats.
const MAX_PINS: usize = 4;

/// Writes what was changed here (pins, mutes, archive, blocks, accepted requests) to the
/// storage service. Records are changed in their bytes, field by field, so fields b33p
/// doesn't know stay as the phone wrote them; and only on top of the version b33p last read.
pub(crate) async fn push(manager: &Mgr, tables: &LocalStore) -> Result<Written, StorageError> {
    use presage::libsignal_service::prelude::ProtobufMessage as _;
    let Some((service, key)) = manager
        .storage_access()
        .await
        .map_err(StorageError::Reach)?
    else {
        return Ok(Written::NoKey);
    };
    let key: [u8; 32] = Vec::from(key)
        .try_into()
        .map_err(|key: Vec<u8>| StorageError::KeyLength { len: key.len() })?;
    let client = Client::new(service).await?;
    let (manifest, manifest_bytes) = client.manifest(&key).await?;
    if manifest.version
        != tables
            .storage_version()
            .await
            .map_err(at(Step::ReadVersion))?
    {
        return Ok(Written::Stale);
    }
    let local = tables
        .conversations()
        .await
        .map_err(at(Step::ReadSettings))?;
    let before = tables.remote().await.map_err(at(Step::ReadRemote))?;
    let pins = wanted_pins(&local);
    let pins_changed = pins != pinned_order(&before);
    let changed: HashMap<&ConversationId, ChatSettings> = local
        .iter()
        .filter_map(|(id, settings)| {
            // Pins are written apart, to the account record.
            let remote = ChatSettings {
                pinned: None,
                ..before.get(id).cloned().unwrap_or_default()
            };
            let wanted = wanted(settings, &remote);
            (wanted != remote).then_some((id, wanted))
        })
        .collect();
    if changed.is_empty() && !pins_changed {
        return Ok(Written::Nothing);
    }

    let ikm = (!manifest.record_ikm.is_empty()).then_some(&manifest.record_ikm[..]);
    let keys: Vec<Vec<u8>> = manifest
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
    let mut replaced: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut inserted = Vec::new();
    let mut remote_after = before.clone();
    for batch in keys.chunks(BATCH) {
        for (raw_id, plaintext) in client.read_raw(&key, batch.to_vec(), ikm).await? {
            let Ok(record) = presage::proto::StorageRecord::decode(&*plaintext) else {
                continue;
            };
            let edited = match record.record {
                Some(Record::Contact(contact)) => {
                    let Some(id) = contact_conversation(&contact.aci, &contact.aci_binary) else {
                        continue;
                    };
                    changed.get(&id).and_then(|wanted| {
                        let edited = crate::wire::edit_message(&plaintext, 1, |c| {
                            set_flags(c, wanted, [9, 10, 11, 12, 13])
                        })?;
                        remote_after.insert(id.clone(), wanted.clone());
                        Some(edited)
                    })
                }
                Some(Record::GroupV2(group)) => {
                    let id = ConversationId::group(&group.master_key);
                    changed.get(&id).and_then(|wanted| {
                        let edited = crate::wire::edit_message(&plaintext, 3, |g| {
                            set_flags(g, wanted, [2, 3, 4, 5, 6])
                        })?;
                        remote_after.insert(id.clone(), wanted.clone());
                        Some(edited)
                    })
                }
                Some(Record::Account(_)) if pins_changed => {
                    let entries: Vec<Vec<u8>> = pins.iter().filter_map(pinned_entry).collect();
                    crate::wire::edit_message(&plaintext, 4, |account| {
                        crate::wire::set_repeated(account, 14, &entries)
                    })
                }
                _ => None,
            };
            let Some(edited) = edited else { continue };
            let new_id: [u8; 16] = rand::random();
            inserted.push(presage::proto::StorageItem {
                key: new_id.to_vec(),
                value: encrypt(&item_key(&key, &new_id, ikm), &edited),
            });
            replaced.push((raw_id, new_id.to_vec()));
        }
    }
    if replaced.is_empty() {
        // The changes concern chats the phone keeps no record of.
        return Ok(Written::Nothing);
    }
    for (id, settings) in remote_after.iter_mut() {
        settings.pinned = pins.iter().position(|p| p == id).map(|at| at as u32);
    }

    let version = manifest.version + 1;
    let device = u32::from(manager.device_id());
    let next = next_manifest(&manifest_bytes, version, device, &replaced)?;
    let operation = presage::proto::WriteOperation {
        manifest: Some(presage::proto::StorageManifest {
            version,
            value: encrypt(&manifest_key(&key, version), &next),
        }),
        insert_item: inserted,
        delete_key: replaced.iter().map(|(old, _)| old.clone()).collect(),
        clear_all: false,
    };
    if !client.write(&operation).await? {
        return Ok(Written::Stale);
    }
    tables
        .set_remote(&remote_after, version)
        .await
        .map_err(at(Step::KeepWritten))?;
    info!(
        records = replaced.len(),
        version, "wrote to the storage service"
    );
    Ok(Written::Records(replaced.len()))
}

/// The manifest after replacing records: changed in its bytes (version, source device, and the
/// replaced records' identifiers), so what b33p doesn't know of it stays; checked by reading
/// it back as the phone will. ManifestRecord: version 1, identifiers 2, sourceDevice 3;
/// Identifier: raw 1.
fn next_manifest(
    manifest: &[u8],
    version: u64,
    device: u32,
    replaced: &[(Vec<u8>, Vec<u8>)],
) -> Result<Vec<u8>, StorageError> {
    use presage::libsignal_service::prelude::ProtobufMessage as _;
    let before =
        presage::proto::ManifestRecord::decode(manifest).map_err(StorageError::InvalidManifest)?;
    let next = crate::wire::set_varint(manifest, 1, version)
        .and_then(|m| crate::wire::set_varint(&m, 3, u64::from(device)))
        .and_then(|m| {
            crate::wire::edit_each(&m, 2, |identifier| {
                let raw = presage::proto::manifest_record::Identifier::decode(identifier)
                    .ok()?
                    .raw;
                let (_, new) = replaced.iter().find(|(old, _)| *old == raw)?;
                crate::wire::set_bytes(identifier, 1, new)
            })
        })
        .ok_or(StorageError::ManifestUnchangeable)?;
    let check = presage::proto::ManifestRecord::decode(&*next)
        .map_err(StorageError::ChangedManifestInvalid)?;
    let written: Vec<&Vec<u8>> = check.identifiers.iter().map(|id| &id.raw).collect();
    let adds_up = check.version == version
        && check.source_device == device
        && check.record_ikm == before.record_ikm
        && check.identifiers.len() == before.identifiers.len()
        && replaced
            .iter()
            .all(|(old, new)| !written.contains(&old) && written.contains(&new));
    if !adds_up {
        return Err(StorageError::ChangedManifestWrong);
    }
    Ok(next)
}

/// What the storage service should say for a chat: what b33p says, except that b33p
/// never takes back what the phone accepted (`accepted` only adds).
fn wanted(local: &ChatSettings, remote: &ChatSettings) -> ChatSettings {
    ChatSettings {
        archived: local.archived,
        marked_unread: local.marked_unread,
        muted_until: local.muted_until,
        blocked: local.blocked,
        accepted: local.accepted || remote.accepted,
        ..ChatSettings::default()
    }
}

/// Sets blocked, whitelisted, archived, marked unread and muted-until, given their field
/// numbers in this kind of record.
fn set_flags(
    record: &[u8],
    wanted: &ChatSettings,
    [blocked, whitelisted, archived, unread, muted]: [u64; 5],
) -> Option<Vec<u8>> {
    let muted_until = wanted
        .muted_until
        .map_or(0, |until| until.min(i64::MAX as u64));
    let record = crate::wire::set_varint(record, blocked, u64::from(wanted.blocked))?;
    let record = crate::wire::set_varint(&record, whitelisted, u64::from(wanted.accepted))?;
    let record = crate::wire::set_varint(&record, archived, u64::from(wanted.archived))?;
    let record = crate::wire::set_varint(&record, unread, u64::from(wanted.marked_unread))?;
    crate::wire::set_varint(&record, muted, muted_until)
}

/// The chats pinned here, in order, as many as Signal shows.
fn wanted_pins(local: &HashMap<ConversationId, ChatSettings>) -> Vec<ConversationId> {
    let mut pins = pinned_order(local);
    pins.truncate(MAX_PINS);
    pins
}

fn pinned_order(map: &HashMap<ConversationId, ChatSettings>) -> Vec<ConversationId> {
    let mut pins: Vec<(u32, &ConversationId)> = map
        .iter()
        .filter_map(|(id, s)| Some((s.pinned?, id)))
        .collect();
    pins.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.0.cmp(&b.1.0)));
    pins.into_iter().map(|(_, id)| id.clone()).collect()
}

/// An account record's entry for a pinned chat, encoded.
fn pinned_entry(id: &ConversationId) -> Option<Vec<u8>> {
    use presage::libsignal_service::prelude::ProtobufMessage as _;
    use presage::proto::account_record::{PinnedConversation, pinned_conversation};
    let identifier = if let Some(service_id) = id.contact_id() {
        Pinned::Contact(pinned_conversation::Contact {
            service_id: service_id.to_owned(),
            ..Default::default()
        })
    } else {
        Pinned::GroupMasterKey(hex::decode(id.group_key()?).ok()?)
    };
    Some(
        PinnedConversation {
            identifier: Some(identifier),
        }
        .encode_to_vec(),
    )
}

/// Requests to the storage service with its own credentials.
struct Client {
    service: presage::libsignal_service::push_service::PushService,
    auth: presage::libsignal_service::push_service::HttpAuth,
}

impl Client {
    async fn new(
        service: presage::libsignal_service::push_service::PushService,
    ) -> Result<Self, StorageError> {
        use presage::libsignal_service::{configuration::Endpoint, push_service::HttpAuthOverride};
        #[derive(serde::Deserialize)]
        struct Auth {
            username: String,
            password: String,
        }
        const AUTH: &str = "/v1/storage/auth";
        let http = |source| StorageError::Http {
            endpoint: AUTH,
            source,
        };
        let auth: Auth = service
            .request(
                reqwest::Method::GET,
                Endpoint::service(AUTH),
                HttpAuthOverride::NoOverride,
            )
            .map_err(|source| StorageError::Request {
                endpoint: AUTH,
                source,
            })?
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(http)?
            .json()
            .await
            .map_err(http)?;
        Ok(Self {
            service,
            auth: presage::libsignal_service::push_service::HttpAuth {
                username: auth.username,
                password: auth.password,
            },
        })
    }

    fn request(
        &self,
        method: reqwest::Method,
        endpoint: &'static str,
    ) -> Result<reqwest::RequestBuilder, StorageError> {
        use presage::libsignal_service::{configuration::Endpoint, push_service::HttpAuthOverride};
        self.service
            .request(
                method,
                Endpoint::storage(endpoint.to_owned()),
                HttpAuthOverride::Identified(self.auth.clone()),
            )
            .map_err(|source| StorageError::Request { endpoint, source })
    }

    /// The body of a successful answer from `endpoint`.
    async fn body(
        endpoint: &'static str,
        request: reqwest::RequestBuilder,
    ) -> Result<prost::bytes::Bytes, StorageError> {
        let http = |source| StorageError::Http { endpoint, source };
        request
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(http)?
            .bytes()
            .await
            .map_err(http)
    }

    /// The manifest, decoded and as its bytes (to change those).
    async fn manifest(
        &self,
        key: &[u8; 32],
    ) -> Result<(presage::proto::ManifestRecord, Vec<u8>), StorageError> {
        use presage::libsignal_service::prelude::ProtobufMessage as _;
        const MANIFEST: &str = "/v1/storage/manifest";
        let bytes = Self::body(MANIFEST, self.request(reqwest::Method::GET, MANIFEST)?).await?;
        let manifest = presage::proto::StorageManifest::decode(bytes).map_err(|source| {
            StorageError::Decode {
                endpoint: MANIFEST,
                what: "a manifest",
                source,
            }
        })?;
        let plaintext = decrypt(&manifest_key(key, manifest.version), &manifest.value)?;
        let record = presage::proto::ManifestRecord::decode(&*plaintext)
            .map_err(StorageError::InvalidManifest)?;
        Ok((record, plaintext))
    }

    /// Records by id, decrypted but not decoded.
    async fn read_raw(
        &self,
        key: &[u8; 32],
        ids: Vec<Vec<u8>>,
        ikm: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        use presage::libsignal_service::prelude::ProtobufMessage as _;
        const READ: &str = "/v1/storage/read";
        let body = presage::proto::ReadOperation { read_key: ids }.encode_to_vec();
        let request = self
            .request(reqwest::Method::PUT, READ)?
            .header("Content-Type", "application/x-protobuf")
            .body(body);
        let bytes = Self::body(READ, request).await?;
        let items =
            presage::proto::StorageItems::decode(bytes).map_err(|source| StorageError::Decode {
                endpoint: READ,
                what: "records",
                source,
            })?;
        items
            .items
            .into_iter()
            .map(|item| {
                let plaintext = decrypt(&item_key(key, &item.key, ikm), &item.value)?;
                Ok((item.key, plaintext))
            })
            .collect()
    }

    /// Writes; `false` if the manifest changed meanwhile (409).
    async fn write(
        &self,
        operation: &presage::proto::WriteOperation,
    ) -> Result<bool, StorageError> {
        use presage::libsignal_service::prelude::ProtobufMessage as _;
        const WRITE: &str = "/v1/storage";
        let http = |source| StorageError::Http {
            endpoint: WRITE,
            source,
        };
        let response = self
            .request(reqwest::Method::PUT, WRITE)?
            .header("Content-Type", "application/x-protobuf")
            .body(operation.encode_to_vec())
            .send()
            .await
            .map_err(http)?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Ok(false);
        }
        response.error_for_status().map_err(http)?;
        Ok(true)
    }
}

// The storage service's cipher (as libsignal-service has it): AES-256-GCM over
// `iv (12 bytes) || ciphertext || tag`, the manifest keyed by HMAC of its version, items by
// HKDF of the manifest's record IKM, or HMAC of their id on older accounts.

const IV_LEN: usize = 12;
const ITEM_KEY_INFO_PREFIX: &[u8] = b"20240801_SIGNAL_STORAGE_SERVICE_ITEM_";

fn manifest_key(storage_key: &[u8; 32], version: u64) -> [u8; 32] {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(storage_key).expect("any key length");
    mac.update(b"Manifest_");
    mac.update(version.to_string().as_bytes());
    mac.finalize().into_bytes().into()
}

fn item_key(storage_key: &[u8; 32], raw_id: &[u8], ikm: Option<&[u8]>) -> [u8; 32] {
    use base64::Engine as _;
    use hmac::{Hmac, KeyInit, Mac};
    match ikm {
        Some(ikm) => {
            let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(None, ikm);
            let mut key = [0u8; 32];
            hkdf.expand_multi_info(&[ITEM_KEY_INFO_PREFIX, raw_id], &mut key)
                .expect("32 bytes is a valid length");
            key
        }
        None => {
            let mut mac =
                Hmac::<sha2::Sha256>::new_from_slice(storage_key).expect("any key length");
            mac.update(b"Item_");
            mac.update(
                base64::engine::general_purpose::STANDARD
                    .encode(raw_id)
                    .as_bytes(),
            );
            mac.finalize().into_bytes().into()
        }
    }
}

fn decrypt(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, StorageError> {
    use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
    let undecryptable = || StorageError::Decrypt { len: blob.len() };
    let (iv, ciphertext) = blob
        .split_first_chunk::<IV_LEN>()
        .ok_or_else(undecryptable)?;
    Aes256Gcm::new(key.into())
        .decrypt(iv.into(), ciphertext)
        .map_err(|_| undecryptable())
}

fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
    let iv: [u8; IV_LEN] = rand::random();
    let ciphertext = Aes256Gcm::new(key.into())
        .encrypt((&iv).into(), plaintext)
        .expect("AES-GCM encryption doesn't fail");
    [&iv[..], &ciphertext].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As Signal-Desktop titles a chat: the nickname, the address book's name, the profile's,
    /// the username; nothing when only the number is known. Blank names don't count.
    #[test]
    fn contacts_are_named_as_signal_names_them() {
        use presage::proto::{ContactRecord, contact_record::Name};
        let all = ContactRecord {
            nickname: Some(Name {
                given: "Mum".into(),
                family: String::new(),
            }),
            system_given_name: "Anna".into(),
            system_family_name: "Schmidt".into(),
            given_name: "Ann".into(),
            family_name: "S.".into(),
            username: "ann.42".into(),
            e164: "+4915123456789".into(),
            ..Default::default()
        };
        assert_eq!(shown_name(&all).as_deref(), Some("Mum"));
        let no_nickname = ContactRecord {
            nickname: Some(Name {
                given: " ".into(),
                family: String::new(),
            }),
            ..all.clone()
        };
        assert_eq!(shown_name(&no_nickname).as_deref(), Some("Anna Schmidt"));
        let profile_only = ContactRecord {
            system_given_name: String::new(),
            system_family_name: String::new(),
            ..no_nickname.clone()
        };
        assert_eq!(shown_name(&profile_only).as_deref(), Some("Ann S."));
        let family_only = ContactRecord {
            system_family_name: "Schmidt".into(),
            ..profile_only.clone()
        };
        assert_eq!(shown_name(&family_only).as_deref(), Some("Schmidt"));
        let username_only = ContactRecord {
            given_name: String::new(),
            family_name: String::new(),
            ..profile_only
        };
        assert_eq!(shown_name(&username_only).as_deref(), Some("ann.42"));
        let number_only = ContactRecord {
            username: String::new(),
            ..username_only
        };
        assert_eq!(shown_name(&number_only), None);
    }

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

    /// The phone's verifications are taken from contact records, with the key they are for; a
    /// key that doesn't decode is no verification.
    #[test]
    fn reads_verifications_from_contact_records() {
        use presage::{
            libsignal_service::protocol::IdentityKeyPair,
            proto::{contact_record::IdentityState, verified::State},
        };
        let key = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let mut contact = presage::proto::ContactRecord {
            identity_key: key.serialize().to_vec(),
            ..Default::default()
        };
        contact.set_identity_state(IdentityState::Verified);
        assert_eq!(
            verification(&contact),
            Some(Verification {
                identity_key: key.serialize().to_vec(),
                state: State::Verified,
            })
        );
        // Changed since it was verified, as the phone saw it.
        contact.set_identity_state(IdentityState::Unverified);
        assert_eq!(
            verification(&contact).map(|v| v.state),
            Some(State::Unverified)
        );
        contact.set_identity_state(IdentityState::Default);
        assert_eq!(
            verification(&contact).map(|v| v.state),
            Some(State::Default)
        );
        contact.identity_key = vec![1, 2, 3];
        assert_eq!(verification(&contact), None);
    }

    /// Only what changed on the phone since the last read is taken: a verification made here
    /// meanwhile isn't undone by the phone repeating what it said before.
    #[test]
    fn takes_only_verifications_changed_on_the_phone() {
        use presage::proto::verified::State;
        let of = |key: u8, state| Verification {
            identity_key: vec![key; 33],
            state,
        };
        let before = HashMap::from([
            ("alice".to_string(), of(1, State::Default)),
            ("bob".to_string(), of(2, State::Verified)),
        ]);
        let now = HashMap::from([
            ("alice".to_string(), of(1, State::Default)),
            ("bob".to_string(), of(2, State::Default)),
            ("carol".to_string(), of(3, State::Verified)),
        ]);
        let changed: Vec<_> = changed_verifications(&before, &now)
            .into_iter()
            .map(|(aci, v)| (aci.as_str(), v.state))
            .collect();
        assert_eq!(
            changed,
            [("bob", State::Default), ("carol", State::Verified)]
        );
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

    #[test]
    fn archived_chats_are_not_pinned_on_the_phone() {
        // Android archived "a" and kept it in the account record's pins.
        let archived = ChatSettings {
            archived: true,
            ..ChatSettings::default()
        };
        let mut remote = map(&[("a", archived)]);
        set_pins(&mut remote, &[id("a"), id("b")]);
        assert_eq!(remote[&id("a")].pinned, None);
        assert_eq!(remote[&id("b")].pinned, Some(0));
        // Unarchived and pinned here again: a change of pins, which gets written.
        let local = map(&[("a", pinned(0)), ("b", pinned(1))]);
        assert_ne!(wanted_pins(&local), pinned_order(&remote));
    }

    /// The cipher here must be libsignal-service's, both ways.
    #[test]
    fn cipher_matches_libsignal_service() {
        use presage::libsignal_service::prelude::ProtobufMessage as _;
        use presage::libsignal_service::{StorageService, master_key::StorageServiceKey};
        let key = [7u8; 32];
        let theirs = StorageServiceKey::from_slice(&key).unwrap();
        let manifest = presage::proto::ManifestRecord {
            version: 12,
            source_device: 2,
            identifiers: Vec::new(),
            record_ikm: vec![3; 32],
        };
        let sealed = StorageService::encrypt_manifest(&theirs, &manifest);
        let opened = decrypt(&manifest_key(&key, sealed.version), &sealed.value).unwrap();
        assert_eq!(opened, manifest.encode_to_vec());
        let ours = presage::proto::StorageManifest {
            version: 12,
            value: encrypt(&manifest_key(&key, 12), &manifest.encode_to_vec()),
        };
        assert_eq!(
            StorageService::decrypt_manifest(&theirs, &ours).unwrap(),
            manifest
        );

        let record = presage::proto::StorageRecord {
            record: Some(Record::Contact(presage::proto::ContactRecord {
                aci: "x".into(),
                archived: true,
                ..Default::default()
            })),
        };
        let id = vec![9u8; 16];
        for ikm in [None, Some(&[4u8; 32][..])] {
            let item = StorageService::encrypt_item(&theirs, id.clone(), &record, ikm);
            let opened = decrypt(&item_key(&key, &id, ikm), &item.value).unwrap();
            assert_eq!(opened, record.encode_to_vec());
            let ours = presage::proto::StorageItem {
                key: id.clone(),
                value: encrypt(&item_key(&key, &id, ikm), &record.encode_to_vec()),
            };
            assert_eq!(
                StorageService::decrypt_item(&theirs, &ours, ikm).unwrap(),
                record
            );
        }
    }

    #[test]
    fn only_settings_that_differ_are_wanted() {
        let remote = ChatSettings {
            accepted: true,
            archived: true,
            ..ChatSettings::default()
        };
        let local = ChatSettings {
            last_read: 99,
            pinned: Some(0),
            ..ChatSettings::default()
        };
        // Unarchived here; accepted on the phone stays accepted.
        let wanted = wanted(&local, &remote);
        assert!(!wanted.archived && wanted.accepted);
        assert_eq!((wanted.last_read, wanted.pinned), (0, None));
        let mut pins = HashMap::new();
        for (at, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            pins.insert(
                ConversationId((*id).into()),
                ChatSettings {
                    pinned: Some(at as u32),
                    ..ChatSettings::default()
                },
            );
        }
        let wanted: Vec<_> = wanted_pins(&pins).iter().map(|id| id.0.clone()).collect();
        assert_eq!(wanted, ["a", "b", "c", "d"], "Signal shows four");
        assert!(pinned_entry(&ConversationId("g:zz".into())).is_none());
    }

    #[test]
    fn manifests_change_only_what_they_must() {
        use presage::libsignal_service::prelude::ProtobufMessage as _;
        use presage::proto::{ManifestRecord, manifest_record::Identifier};
        let id = |raw: u8, kind: identifier::Type| Identifier {
            raw: vec![raw; 16],
            r#type: kind as i32,
        };
        let manifest = ManifestRecord {
            version: 7,
            source_device: 1,
            identifiers: vec![
                id(1, identifier::Type::Contact),
                id(2, identifier::Type::Account),
                id(3, identifier::Type::ChatFolder),
            ],
            record_ikm: vec![5; 32],
        };
        let mut bytes = manifest.encode_to_vec();
        // A field from a newer Signal: number 9, varint 42.
        bytes.extend_from_slice(&[9 << 3, 42]);
        let replaced = vec![(vec![1; 16], vec![8; 16])];
        let next = next_manifest(&bytes, 8, 3, &replaced).unwrap();
        let read = ManifestRecord::decode(&*next).unwrap();
        assert_eq!((read.version, read.source_device), (8, 3));
        let raws: Vec<u8> = read.identifiers.iter().map(|i| i.raw[0]).collect();
        assert_eq!(raws, [8, 2, 3]);
        assert_eq!(read.identifiers[0].r#type(), identifier::Type::Contact);
        assert!(
            next.windows(2).any(|w| w == [9 << 3, 42]),
            "the newer field stays"
        );
        // An id that isn't in the manifest: refused.
        assert!(next_manifest(&bytes, 8, 3, &[(vec![6; 16], vec![9; 16])]).is_err());
    }
}
