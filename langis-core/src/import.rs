//! Turns the frames of a message history archive (Signal's backup format) into what presage
//! stores: contacts with their profile keys, groups, and text messages.
//!
//! The archive only refers back to frames it has already sent (libsignal validates this), so
//! frames are converted one at a time. Groups are held back until all recipients are known,
//! because their members' profile keys come from the contact frames.

use std::collections::HashMap;

use libsignal_message_backup::proto::backup as proto;
use presage::{
    libsignal_service::{
        content::{Content, ContentBody, DataMessage, GroupContextV2, Metadata},
        groups_v2::Role,
        prelude::{ProfileKey, Uuid, phonenumber},
        protocol::{Aci, ServiceId},
        push_service::DEFAULT_DEVICE_ID,
    },
    model::{
        contacts::Contact,
        groups::{Group, Member},
    },
    proto::{
        SyncMessage,
        sync_message::{Content as SyncContent, Sent},
    },
    store::{ContentsStore, Thread},
};
use protobuf::Message as _;

/// What a frame turns into, ready to be stored.
#[derive(Debug)]
pub(crate) enum Record {
    Contact { contact: Contact, profile_key: Option<ProfileKey> },
    Group { master_key: [u8; 32], group: Group },
    Message { thread: Thread, content: Content },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stats {
    pub contacts: usize,
    pub groups: usize,
    pub messages: usize,
    /// Frames and chat items this version does not import (attachments-only messages, calls,
    /// group updates, …).
    pub skipped: usize,
}

/// Who a recipient id in the archive stands for.
#[derive(Clone, Copy)]
enum Party {
    Me,
    Contact(Aci),
    Group { master_key: [u8; 32], revision: u32 },
}

pub(crate) struct Importer {
    me: Aci,
    recipients: HashMap<u64, Party>,
    /// Chat id to recipient id.
    chats: HashMap<u64, u64>,
    profile_keys: HashMap<Aci, ProfileKey>,
    /// Until the first non-recipient frame; see the module docs.
    pending_groups: Vec<([u8; 32], proto::Group)>,
    stats: Stats,
}

impl Importer {
    /// `me` is our own ACI: the author of outgoing messages.
    pub(crate) fn new(me: Aci) -> Self {
        Self {
            me,
            recipients: HashMap::new(),
            chats: HashMap::new(),
            profile_keys: HashMap::new(),
            pending_groups: Vec::new(),
            stats: Stats::default(),
        }
    }

    pub(crate) fn stats(&self) -> Stats {
        self.stats
    }

    /// Converts one encoded `Frame` (not the leading `BackupInfo`).
    pub(crate) fn frame(&mut self, bytes: &[u8]) -> Result<Vec<Record>, String> {
        let frame = proto::Frame::parse_from_bytes(bytes).map_err(|e| format!("malformed history frame: {e}"))?;
        let mut records = Vec::new();
        match frame.item {
            Some(proto::frame::Item::Recipient(recipient)) => records.extend(self.recipient(recipient)),
            Some(proto::frame::Item::Chat(chat)) => {
                records.extend(self.flush_groups());
                self.chats.insert(chat.id, chat.recipientId);
            }
            Some(proto::frame::Item::ChatItem(item)) => {
                records.extend(self.flush_groups());
                match self.message(&item) {
                    Some((thread, content)) => {
                        self.stats.messages += 1;
                        records.push(Record::Message { thread, content });
                    }
                    None => self.stats.skipped += 1,
                }
            }
            // Account settings, sticker packs, calls, notification profiles, chat folders.
            Some(_) => self.stats.skipped += 1,
            None => {}
        }
        Ok(records)
    }

    /// Call after the last frame.
    pub(crate) fn finish(&mut self) -> Vec<Record> {
        self.flush_groups()
    }

    fn recipient(&mut self, recipient: proto::Recipient) -> Option<Record> {
        use proto::recipient::Destination;
        match recipient.destination? {
            Destination::Self_(_) => {
                self.recipients.insert(recipient.id, Party::Me);
                None
            }
            Destination::Contact(contact) => {
                let (contact, profile_key) = convert_contact(&contact)?;
                let aci = Aci::from_uuid_bytes(contact.uuid.into_bytes());
                self.recipients.insert(recipient.id, Party::Contact(aci));
                if let Some(key) = profile_key {
                    self.profile_keys.insert(aci, key);
                }
                self.stats.contacts += 1;
                Some(Record::Contact { contact, profile_key })
            }
            Destination::Group(group) => {
                let master_key: [u8; 32] = group.masterKey.as_slice().try_into().ok()?;
                let revision = group.snapshot.version;
                self.recipients.insert(recipient.id, Party::Group { master_key, revision });
                self.pending_groups.push((master_key, group));
                None
            }
            // Distribution lists (stories), release notes, call links.
            _ => {
                self.stats.skipped += 1;
                None
            }
        }
    }

    fn flush_groups(&mut self) -> Vec<Record> {
        let pending = std::mem::take(&mut self.pending_groups);
        self.stats.groups += pending.len();
        pending
            .into_iter()
            .map(|(master_key, group)| Record::Group { master_key, group: self.convert_group(&group) })
            .collect()
    }

    fn convert_group(&self, group: &proto::Group) -> Group {
        let snapshot = &group.snapshot;
        let text = |blob: &protobuf::MessageField<proto::group::GroupAttributeBlob>| match &blob.content {
            Some(proto::group::group_attribute_blob::Content::Title(text))
            | Some(proto::group::group_attribute_blob::Content::DescriptionText(text)) => text.clone(),
            _ => String::new(),
        };
        let members = snapshot
            .members
            .iter()
            .filter_map(|member| {
                let aci = Aci::from_uuid_bytes(member.userId.as_slice().try_into().ok()?);
                Some(Member {
                    aci,
                    role: match member.role.enum_value_or_default() {
                        proto::group::member::Role::ADMINISTRATOR => Role::Administrator,
                        _ => Role::Default,
                    },
                    // Unknown keys get fixed up when presage next fetches the group.
                    profile_key: self.profile_keys.get(&aci).copied().unwrap_or(ProfileKey::create([0; 32])),
                    joined_at_revision: member.joinedAtVersion,
                    label: None,
                    label_emoji: None,
                })
            })
            .collect();
        let description = text(&snapshot.description);
        Group {
            title: text(&snapshot.title),
            avatar: snapshot.avatarUrl.clone(),
            disappearing_messages_timer: None,
            access_control: None,
            revision: snapshot.version,
            members,
            pending_members: Vec::new(),
            requesting_members: Vec::new(),
            invite_link_password: snapshot.inviteLinkPassword.clone(),
            description: (!description.is_empty()).then_some(description),
        }
    }

    /// A text message as presage would have stored it on receipt: a data message if incoming,
    /// a "sent" sync message if we wrote it. Anything else is skipped for now.
    fn message(&self, item: &proto::ChatItem) -> Option<(Thread, Content)> {
        let Some(proto::chat_item::Item::StandardMessage(message)) = &item.item else { return None };
        let body = message.text.body.clone();
        if body.is_empty() {
            return None;
        }
        let chat = self.recipients.get(self.chats.get(&item.chatId)?)?;
        let (thread, group_v2) = match *chat {
            Party::Me => (Thread::Contact(ServiceId::Aci(self.me)), None),
            Party::Contact(aci) => (Thread::Contact(ServiceId::Aci(aci)), None),
            Party::Group { master_key, revision } => (
                Thread::Group(master_key),
                Some(GroupContextV2 { master_key: Some(master_key.to_vec()), revision: Some(revision), ..Default::default() }),
            ),
        };
        let author = match self.recipients.get(&item.authorId)? {
            Party::Me => self.me,
            Party::Contact(aci) => *aci,
            Party::Group { .. } => return None,
        };
        let data = DataMessage { body: Some(body), timestamp: Some(item.dateSent), group_v2, ..Default::default() };

        use proto::chat_item::DirectionalDetails;
        let (body, server_timestamp, sealed) = match &item.directionalDetails {
            Some(DirectionalDetails::Outgoing(outgoing)) => {
                let destination = match thread {
                    Thread::Contact(id) => Some(id.service_id_string()),
                    Thread::Group(_) => None,
                };
                let sent = Sent {
                    destination_service_id: destination,
                    timestamp: Some(item.dateSent),
                    message: Some(data),
                    ..Default::default()
                };
                let sync = SyncMessage { content: Some(SyncContent::Sent(sent)), ..Default::default() };
                (ContentBody::SynchronizeMessage(sync), outgoing.dateReceived, false)
            }
            Some(DirectionalDetails::Incoming(incoming)) => (
                ContentBody::DataMessage(data),
                incoming.dateServerSent.unwrap_or(incoming.dateReceived),
                incoming.sealedSender,
            ),
            _ => return None,
        };
        let metadata = Metadata {
            sender: ServiceId::Aci(author),
            destination: ServiceId::Aci(self.me),
            sender_device: *DEFAULT_DEVICE_ID,
            pni_verified: None,
            client_timestamp: datetime(item.dateSent),
            server_timestamp: datetime(server_timestamp),
            needs_receipt: false,
            unidentified_sender: sealed,
            was_plaintext: false,
            server_guid: None,
        };
        Some((thread, Content { metadata, body }))
    }
}

/// Contacts without an ACI can't be messaged or stored by presage, so they are dropped.
fn convert_contact(contact: &proto::Contact) -> Option<(Contact, Option<ProfileKey>)> {
    let uuid = Uuid::from_slice(contact.aci.as_deref()?).ok()?;
    let profile_key: Option<[u8; 32]> = contact.profileKey.as_deref().and_then(|key| key.try_into().ok());
    let phone_number = contact.e164.and_then(|number| phonenumber::parse(None, format!("+{number}")).ok());
    let contact = Contact {
        uuid,
        phone_number,
        name: display_name(contact),
        verified: Default::default(),
        profile_key: profile_key.map(Vec::from).unwrap_or_default(),
        expire_timer: 0,
        expire_timer_version: 2,
        inbox_position: 0,
        avatar: None,
    };
    Some((contact, profile_key.map(ProfileKey::create)))
}

/// In Signal's order of preference: the nickname you gave them, their name in your address
/// book, then the name on their profile. Empty if none is set.
fn display_name(contact: &proto::Contact) -> String {
    let join = |given: &str, family: &str| format!("{given} {family}").trim().to_owned();
    let nickname = contact.nickname.as_ref().map(|name| join(&name.given, &name.family)).unwrap_or_default();
    let system = join(&contact.systemGivenName, &contact.systemFamilyName);
    let profile = join(
        contact.profileGivenName.as_deref().unwrap_or_default(),
        contact.profileFamilyName.as_deref().unwrap_or_default(),
    );
    [nickname, system, profile].into_iter().find(|name| !name.is_empty()).unwrap_or_default()
}

fn datetime(millis: u64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(millis as i64).unwrap_or_default()
}

/// Stores one record. Messages are keyed by timestamp and thread, so importing twice is harmless.
pub(crate) async fn store<S: ContentsStore>(store: &mut S, record: Record) -> Result<(), String>
where
    S::ContentsStoreError: std::fmt::Display,
{
    let result = match record {
        Record::Contact { contact, profile_key } => {
            let uuid = contact.uuid;
            match store.save_contact(&contact).await {
                Ok(()) => match profile_key {
                    Some(key) => store.upsert_profile_key(&uuid, key).await.map(drop),
                    None => Ok(()),
                },
                Err(error) => Err(error),
            }
        }
        Record::Group { master_key, group } => store.save_group(master_key, group).await,
        Record::Message { thread, content } => store.save_message(&thread, content).await,
    };
    result.map_err(|e| format!("failed to store the message history: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use presage::{model::identity::OnNewIdentity, store::ContentExt as _};
    use presage_store_sqlite::SqliteStore;
    use proto::{chat_item::DirectionalDetails, recipient::Destination};
    use protobuf::MessageField;

    const ME: [u8; 16] = [1; 16];
    const ALICE: [u8; 16] = [2; 16];
    const GROUP_KEY: [u8; 32] = [7; 32];
    const ALICE_KEY: [u8; 32] = [9; 32];

    fn frame(item: proto::frame::Item) -> Vec<u8> {
        proto::Frame { item: Some(item), ..Default::default() }.write_to_bytes().unwrap()
    }

    fn recipient(id: u64, destination: Destination) -> Vec<u8> {
        frame(proto::frame::Item::Recipient(proto::Recipient {
            id,
            destination: Some(destination),
            ..Default::default()
        }))
    }

    fn chat(id: u64, recipient_id: u64) -> Vec<u8> {
        frame(proto::frame::Item::Chat(proto::Chat { id, recipientId: recipient_id, ..Default::default() }))
    }

    fn text(chat_id: u64, author_id: u64, sent: u64, outgoing: bool, body: &str) -> Vec<u8> {
        let direction = if outgoing {
            DirectionalDetails::Outgoing(Default::default())
        } else {
            DirectionalDetails::Incoming(proto::chat_item::IncomingMessageDetails {
                dateReceived: sent + 5,
                ..Default::default()
            })
        };
        let message = proto::StandardMessage {
            text: MessageField::some(proto::Text { body: body.into(), ..Default::default() }),
            ..Default::default()
        };
        frame(proto::frame::Item::ChatItem(proto::ChatItem {
            chatId: chat_id,
            authorId: author_id,
            dateSent: sent,
            directionalDetails: Some(direction),
            item: Some(proto::chat_item::Item::StandardMessage(message)),
            ..Default::default()
        }))
    }

    /// Recipients 1 (me), 2 (Alice) and 3 (a group); chats 10 (Alice) and 11 (the group).
    fn archive() -> Vec<Vec<u8>> {
        let alice = proto::Contact {
            aci: Some(ALICE.to_vec()),
            e164: Some(4915112345678),
            profileKey: Some(ALICE_KEY.to_vec()),
            profileGivenName: Some("Ali".into()),
            systemGivenName: "Alice".into(),
            systemFamilyName: "Liddell".into(),
            ..Default::default()
        };
        let title = proto::group::GroupAttributeBlob {
            content: Some(proto::group::group_attribute_blob::Content::Title("Climbing".into())),
            ..Default::default()
        };
        let member = |id: [u8; 16]| proto::group::Member { userId: id.to_vec(), ..Default::default() };
        let group = proto::Group {
            masterKey: GROUP_KEY.to_vec(),
            snapshot: MessageField::some(proto::group::GroupSnapshot {
                title: MessageField::some(title),
                version: 5,
                members: vec![member(ME), member(ALICE)],
                ..Default::default()
            }),
            ..Default::default()
        };
        let update = frame(proto::frame::Item::ChatItem(proto::ChatItem {
            chatId: 11,
            authorId: 2,
            dateSent: 4000,
            directionalDetails: Some(DirectionalDetails::Directionless(Default::default())),
            item: Some(proto::chat_item::Item::UpdateMessage(Default::default())),
            ..Default::default()
        }));
        vec![
            recipient(1, Destination::Self_(Default::default())),
            recipient(3, Destination::Group(group)),
            recipient(2, Destination::Contact(alice)),
            chat(10, 2),
            chat(11, 3),
            text(10, 2, 1000, false, "Hi!"),
            text(10, 1, 2000, true, "Hello, Alice"),
            text(11, 2, 3000, false, "Who's in on Saturday?"),
            update,
        ]
    }

    fn import(frames: &[Vec<u8>]) -> (Vec<Record>, Stats) {
        let mut importer = Importer::new(Aci::from_uuid_bytes(ME));
        let mut records = Vec::new();
        for frame in frames {
            records.extend(importer.frame(frame).unwrap());
        }
        records.extend(importer.finish());
        (records, importer.stats())
    }

    #[test]
    fn converts_contacts_groups_and_text_messages() {
        let (records, stats) = import(&archive());
        assert_eq!(stats, Stats { contacts: 1, groups: 1, messages: 3, skipped: 1 });

        let Some(Record::Contact { contact, profile_key }) =
            records.iter().find(|r| matches!(r, Record::Contact { .. }))
        else {
            panic!("no contact")
        };
        // The address book name wins over the profile name.
        assert_eq!(contact.name, "Alice Liddell");
        assert_eq!(contact.phone_number.as_ref().unwrap().to_string(), "+4915112345678");
        assert!(profile_key.is_some());

        // The group was held back until Alice's profile key was known.
        let Some(Record::Group { master_key, group }) = records.iter().find(|r| matches!(r, Record::Group { .. }))
        else {
            panic!("no group")
        };
        assert_eq!(*master_key, GROUP_KEY);
        assert_eq!(group.title, "Climbing");
        assert_eq!(group.revision, 5);
        let alice = group.members.iter().find(|m| m.aci == Aci::from_uuid_bytes(ALICE)).unwrap();
        assert_eq!(alice.profile_key.get_bytes(), ALICE_KEY);
    }

    #[test]
    fn messages_look_like_received_ones() {
        let (records, _) = import(&archive());
        let messages: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::Message { thread, content } => Some((thread, content)),
                _ => None,
            })
            .collect();
        let alice = Thread::Contact(ServiceId::Aci(Aci::from_uuid_bytes(ALICE)));

        let (thread, incoming) = messages[0];
        assert_eq!(*thread, alice);
        assert_eq!(incoming.metadata.sender, ServiceId::Aci(Aci::from_uuid_bytes(ALICE)));
        assert_eq!(incoming.timestamp(), 1000);
        assert!(matches!(&incoming.body, ContentBody::DataMessage(DataMessage { body: Some(b), .. }) if b == "Hi!"));

        // What we wrote comes back as a "sent" sync message addressed to Alice.
        let (thread, outgoing) = messages[1];
        assert_eq!(*thread, alice);
        assert_eq!(outgoing.timestamp(), 2000);
        let ContentBody::SynchronizeMessage(SyncMessage { content: Some(SyncContent::Sent(sent)), .. }) = &outgoing.body
        else {
            panic!("not a sync message: {:?}", outgoing.body)
        };
        assert_eq!(sent.destination_service_id.as_deref(), Some(alice_id().as_str()));
        assert_eq!(sent.message.as_ref().unwrap().body.as_deref(), Some("Hello, Alice"));

        let (thread, in_group) = messages[2];
        assert_eq!(*thread, Thread::Group(GROUP_KEY));
        let ContentBody::DataMessage(data) = &in_group.body else { panic!("not a data message") };
        assert_eq!(data.group_v2.as_ref().unwrap().master_key.as_deref(), Some(&GROUP_KEY[..]));
    }

    fn alice_id() -> String {
        ServiceId::Aci(Aci::from_uuid_bytes(ALICE)).service_id_string()
    }

    #[test]
    fn stores_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("langis-import-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.join("store.db3").display());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut store = SqliteStore::open(&url, OnNewIdentity::Trust).await.unwrap();
            // Twice: importing again must not duplicate anything.
            for _ in 0..2 {
                for record in import(&archive()).0 {
                    super::store(&mut store, record).await.unwrap();
                }
            }

            let alice = ServiceId::Aci(Aci::from_uuid_bytes(ALICE));
            let contacts: Vec<_> = store.contacts().await.unwrap().flatten().collect();
            assert_eq!(contacts.len(), 1);
            assert_eq!(contacts[0].name, "Alice Liddell");
            assert!(store.profile_key(&alice).await.unwrap().is_some());
            assert_eq!(store.group(GROUP_KEY).await.unwrap().unwrap().title, "Climbing");
            assert_eq!(store.messages(&Thread::Contact(alice), ..).await.unwrap().count(), 2);
            assert_eq!(store.messages(&Thread::Group(GROUP_KEY), ..).await.unwrap().count(), 1);
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
