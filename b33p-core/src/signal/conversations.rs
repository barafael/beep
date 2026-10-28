//! The chat list: conversations as the UI knows them, their settings, how people are named.

use super::errors::failed;
use crate::Error;
use std::{collections::HashMap, time::Duration};

use presage::{
    libsignal_service::{content::ContentBody, protocol::ServiceId},
    store::{ContentExt, ContentsStore, Thread},
};
use tracing::{debug, info, warn};

use crate::{
    ChatMessage, ChatSettings, Conversation, ConversationId, ConversationKind, Event,
    local::LocalStore, scrub::scrubbed,
};

use super::{
    Ctx, Mgr, Threads,
    groups::group_conversation,
    history::{stored_at, stored_since, to_chat_message},
    trust::{pin_verified, safety_number, trust_given, trust_of},
};

/// A read of b33p's own tables that may fail without stopping what it serves: said in the
/// log, then taken as empty.
pub(super) fn or_logged<T: Default>(result: Result<T, sqlx::Error>, what: &'static str) -> T {
    result.unwrap_or_else(|error| {
        warn!(what, error = %scrubbed(&error), "failed to read from the database");
        T::default()
    })
}

/// Part of the chat list couldn't be read: said, so it isn't taken for an empty account.
fn unreadable(ctx: &Ctx, what: &'static str, error: &impl std::fmt::Display) {
    warn!(what, error = %scrubbed(error), "failed to read the chat list");
    ctx.events.send(Event::Error(Error::storage(
        "Some chats couldn't be read from the stored data. Restart b33p to try again.",
    )));
}

pub(crate) fn conversation_id(thread: &Thread) -> ConversationId {
    match thread {
        Thread::Contact(id) => ConversationId::contact(&id.service_id_string()),
        Thread::Group(key) => ConversationId::group(key),
    }
}

/// A short, stable name for `id` in the log. The ID itself names the contact, or holds the
/// group's master key, so it must not be written out.
pub(super) fn tag(id: &ConversationId) -> String {
    use sha2::Digest as _;
    hex::encode(&sha2::Sha256::digest(id.0.as_bytes())[..4])
}

/// Remembers how to reach a conversation and describes it for the UI; settings are added by
/// [`with_settings`].
pub(super) fn register(
    threads: &Threads,
    thread: Thread,
    title: String,
    subtitle: String,
    expire_timer: u32,
) -> Conversation {
    let id = conversation_id(&thread);
    let kind = if matches!(thread, Thread::Group(_)) {
        ConversationKind::Group
    } else {
        ConversationKind::Contact
    };
    threads.borrow_mut().insert(id.clone(), thread);
    let title = crate::format::name(&title);
    let title = if title.is_empty() {
        "Unknown".into()
    } else {
        title
    };
    Conversation {
        id,
        title,
        kind,
        subtitle,
        settings: ChatSettings::default(),
        expire_timer,
        ..Conversation::default()
    }
}

/// A contact's phone number as Signal sends it: "+4915123456789".
pub(super) async fn e164_of(manager: &Mgr, who: ServiceId) -> Option<String> {
    use presage::libsignal_service::prelude::phonenumber::Mode;
    let contact = manager.store().contact_by_id(&who).await.ok()??;
    Some(contact.phone_number?.format().mode(Mode::E164).to_string())
}

/// A phone number as people write it: "+49 151 23456789".
pub(super) fn display_number(
    number: &presage::libsignal_service::prelude::phonenumber::PhoneNumber,
) -> String {
    use presage::libsignal_service::prelude::phonenumber::Mode;
    number.format().mode(Mode::International).to_string()
}

pub(super) fn members(count: usize) -> String {
    match count {
        1 => "1 member".into(),
        n => format!("{n} members"),
    }
}

/// The line under a conversation's title; see [`Conversation::subtitle`].
async fn subtitle(manager: &Mgr, thread: &Thread) -> String {
    match thread {
        Thread::Contact(id) if *id == own_id(manager) => "Messages to yourself".into(),
        Thread::Contact(id) => match manager.store().contact_by_id(id).await {
            Ok(Some(contact)) => contact
                .phone_number
                .as_ref()
                .map(display_number)
                .unwrap_or_default(),
            _ => String::new(),
        },
        Thread::Group(key) => match manager.store().group(*key).await {
            Ok(Some(group)) => members(group.members.len()),
            _ => String::new(),
        },
    }
}

/// The thread a conversation id stands for; see [`conversation_id`].
pub(super) fn thread_of(id: &ConversationId) -> Option<Thread> {
    if let Some(service_id) = id.contact_id() {
        return ServiceId::parse_from_service_id_string(service_id).map(Thread::Contact);
    }
    let key = hex::decode(id.group_key()?).ok()?;
    Some(Thread::Group(key.try_into().ok()?))
}

/// Adds what b33p remembers about each conversation.
/// Conversations new to b33p (all of them, right after linking with the history) count as read
/// up to their newest stored message, or every chat would start out unread. This happens before
/// receiving starts: a message received in the meantime would count as read with them.
async fn with_settings(manager: &Mgr, ctx: &Ctx, conversations: &mut [Conversation]) {
    let mut remembered = match ctx.tables.conversations().await {
        Ok(remembered) => remembered,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to read conversation settings");
            return;
        }
    };
    for conversation in conversations {
        conversation.settings = match remembered.remove(&conversation.id) {
            Some(settings) => settings,
            None => {
                let thread = ctx.threads.borrow().get(&conversation.id).cloned();
                let newest = match thread {
                    Some(thread) => newest_stored(manager, &thread).await,
                    None => 0,
                };
                first_settings(ctx, &conversation.id, newest).await
            }
        };
    }
}

/// The settings of a conversation, made for one b33p has kept none for yet: read up to `read`,
/// so that what comes after it counts as unread (and not, later, as read along with it).
pub(super) async fn first_settings(ctx: &Ctx, id: &ConversationId, read: u64) -> ChatSettings {
    match ctx.tables.settings(id).await {
        Ok(Some(settings)) => settings,
        Ok(None) => ctx
            .tables
            .mark_read(id, read)
            .await
            .unwrap_or_else(|error| {
                warn!(error = %scrubbed(&error), "failed to remember a conversation");
                ChatSettings::default()
            }),
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to read a conversation's settings");
            ChatSettings::default()
        }
    }
}

/// When the newest content stored in a thread was sent, or 0 for none.
async fn newest_stored(manager: &Mgr, thread: &Thread) -> u64 {
    match manager.store().messages_before(thread, None, 1).await {
        Ok(contents) => contents.first().map_or(0, |content| content.timestamp()),
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to read a thread's newest message");
            0
        }
    }
}

/// A thread's disappearing-message timer (seconds; 0 for off).
pub(super) async fn expire_timer(manager: &Mgr, thread: &Thread) -> u32 {
    match manager.store().expire_timer(thread).await {
        Ok(Some((timer, _))) => timer,
        _ => 0,
    }
}

pub(super) async fn send_conversations(manager: &Mgr, ctx: &Ctx) -> usize {
    let threads = &ctx.threads;
    let store = manager.store();
    let mut all = Vec::new();
    // Group members are mostly contacts; others are looked up once each.
    let mut names = Names::new();
    // Only verified contacts can have a changed safety number; the rest need no key lookup.
    let verifications = or_logged(ctx.tables.verifications().await, "verifications");
    // The store's pins follow what is kept (also for verifications from before pinning).
    for (aci, verification) in &verifications {
        if let Some(who) = ServiceId::parse_from_service_id_string(aci) {
            pin_verified(manager, who, verification).await;
        }
    }
    let contacts_read = store.contacts().await;
    if let Err(error) = &contacts_read {
        unreadable(ctx, "contacts", error);
    }
    if let Ok(contacts) = contacts_read {
        for contact in contacts.flatten() {
            let thread = Thread::Contact(ServiceId::Aci(contact.uuid.into()));
            let number = contact
                .phone_number
                .as_ref()
                .map(display_number)
                .unwrap_or_default();
            let name = crate::format::name(&contact.name);
            let title = if name.is_empty() {
                number.clone()
            } else {
                name
            };
            let timer = contact.expire_timer;
            names.insert(
                ServiceId::Aci(contact.uuid.into()).service_id_string(),
                title.clone(),
            );
            let who = ServiceId::Aci(contact.uuid.into());
            let mut conversation = register(threads, thread, title, number, timer);
            conversation.trust =
                trust_given(manager, who, verifications.get(&who.service_id_string())).await;
            ctx.trust.borrow_mut().insert(who, conversation.trust);
            all.push(conversation);
        }
    }
    let contacts = all.len();
    // Messages to yourself; listed once there are some.
    let own = Thread::Contact(own_id(manager));
    let timer = expire_timer(manager, &own).await;
    let mut note_to_self = register(
        threads,
        own,
        "Note to Self".into(),
        "Messages to yourself".into(),
        timer,
    );
    note_to_self.note_to_self = true;
    all.push(note_to_self);
    match store.groups().await {
        Ok(groups) => {
            for (key, group) in groups.flatten() {
                all.push(group_conversation(manager, threads, &mut names, key, group).await);
            }
        }
        Err(error) => unreadable(ctx, "groups", &error),
    }
    with_settings(manager, ctx, &mut all).await;
    ctx.events.send(Event::Conversations(all));
    match ctx.tables.folders().await {
        Ok(folders) => {
            ctx.events.send(Event::Folders(folders));
        }
        Err(error) => warn!(error = %scrubbed(&error), "failed to read the chat folders"),
    }
    contacts
}

/// Sends each conversation's newest stored message, so the UI can order conversations by
/// activity and show previews before any history is opened.
pub(super) async fn send_latest_messages(manager: Mgr, ctx: Ctx) {
    let threads: Vec<_> = ctx
        .threads
        .borrow()
        .iter()
        .map(|(id, thread)| (id.clone(), thread.clone()))
        .collect();
    let mut names = Names::new();
    for (id, thread) in threads {
        // Only for the preview and the order: `LoadHistory` replaces it with the full message
        // (delivery, edits, …).
        let Some(latest) = latest_message(&manager, &mut names, &id, &thread).await else {
            continue;
        };
        if !ctx.events.sent(Event::Latest(latest)) {
            return;
        }
    }
}

/// Sends how many messages in each conversation are unread. Before receiving starts: each
/// message received after counts itself.
pub(super) async fn send_unread(manager: &Mgr, ctx: &Ctx) {
    let threads: Vec<_> = ctx
        .threads
        .borrow()
        .iter()
        .map(|(id, thread)| (id.clone(), thread.clone()))
        .collect();
    let remembered = match ctx.tables.conversations().await {
        Ok(remembered) => remembered,
        Err(error) => {
            warn!(error = %scrubbed(&error), "failed to read conversation settings");
            return;
        }
    };
    let mut unread = Vec::new();
    for (id, thread) in threads {
        let Some(settings) = remembered.get(&id) else {
            continue;
        };
        let count = count_unread(manager, &thread, settings.last_read).await;
        if count > 0 {
            unread.push((id, count));
        }
    }
    ctx.events.send(Event::Unread(unread));
}

/// A conversation's newest stored message.
///
/// The store can only return whole time ranges, so this looks at the last week first and only
/// reaches further back for quiet conversations, instead of reading every chat's full history.
async fn latest_message(
    manager: &Mgr,
    names: &mut Names,
    id: &ConversationId,
    thread: &Thread,
) -> Option<ChatMessage> {
    // Receipts and the like are stored too; look past them a few at a time.
    let mut before = None;
    loop {
        let contents = manager
            .store()
            .messages_before(thread, before, 20)
            .await
            .ok()?;
        for content in &contents {
            if let Some(message) = to_chat_message(manager, names, id.clone(), content).await {
                return Some(message);
            }
        }
        before = Some(stored_at(contents.last()?));
    }
}

/// Incoming messages in a thread after `last_read`.
async fn count_unread(manager: &Mgr, thread: &Thread, last_read: u64) -> u32 {
    let own = own_id(manager);
    let from = last_read.saturating_add(1);
    let Some(contents) = stored_since(manager.store(), thread, from).await else {
        return 0;
    };
    let count = contents
        .iter()
        .filter(|content| {
            content.metadata.sender != own
                && matches!(&content.body, ContentBody::DataMessage(data)
                    if data.reaction.is_none()
                        && (data.body.is_some() || !data.attachments.is_empty()))
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

pub(super) fn own_id(manager: &Mgr) -> ServiceId {
    ServiceId::Aci(manager.registration_data().service_ids.aci())
}

/// Display names by sender service ID, so each sender is looked up in the store only once.
pub(super) type Names = HashMap<String, String>;

pub(super) async fn sender_name(manager: &Mgr, names: &mut Names, sender: ServiceId) -> String {
    let id = sender.service_id_string();
    if let Some(name) = names.get(&id) {
        return name.clone();
    }
    let title = manager
        .thread_title(&Thread::Contact(sender))
        .await
        .unwrap_or_default();
    let name = crate::format::name(&title);
    // Without a contact yet, presage titles it with its id (`thread_title`): not kept, so the
    // name shows once the contact is saved.
    if !title.is_empty() && title != id {
        names.insert(id, name.clone());
    }
    name
}

/// We wrote to `id`, here or from another device of ours: that accepts the chat, as Signal's apps
/// take it (they share our profile with whom we write to), and says so if it wasn't yet.
pub(super) async fn wrote_to(ctx: &Ctx, id: &ConversationId) {
    let accepted = ctx
        .tables
        .settings(id)
        .await
        .ok()
        .flatten()
        .is_some_and(|s| s.accepted);
    if !accepted && let Ok(settings) = ctx.tables.update(id, |s| s.accepted = true).await {
        ctx.events.send(Event::Settings {
            conversation: id.clone(),
            settings,
        });
    }
}

pub(super) async fn is_blocked(ctx: &Ctx, id: &ConversationId) -> bool {
    ctx.tables
        .settings(id)
        .await
        .ok()
        .flatten()
        .is_some_and(|s| s.blocked)
}

/// Whether `id` may be told that its messages arrived or were read: not while it is a message
/// request. Which chats are comes with the phone's settings (`settings_read`); before those are
/// read, only a chat accepted here counts, which is known without the phone.
pub(super) async fn accepted(ctx: &Ctx, id: &ConversationId, settings_read: bool) -> bool {
    if settings_read {
        !is_unanswered_request(ctx, id).await
    } else {
        ctx.tables
            .settings(id)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.accepted)
    }
}

/// Whether `id` is a message request not answered yet. Only known once the phone's settings
/// (which say who was accepted) have been read.
pub(super) async fn is_unanswered_request(ctx: &Ctx, id: &ConversationId) -> bool {
    ctx.tables.storage_version().await.unwrap_or(0) > 0
        && !ctx
            .tables
            .settings(id)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.accepted)
}

/// Registers `thread` and announces it to the UI if it's the first we hear of it, by something
/// sent at `first`: that and what follows it are unread.
pub(super) async fn ensure_conversation(
    manager: &Mgr,
    ctx: &Ctx,
    thread: Thread,
    first: u64,
) -> ConversationId {
    let id = conversation_id(&thread);
    if !ctx.threads.borrow().contains_key(&id) {
        let title = manager.thread_title(&thread).await.unwrap_or_default();
        let subtitle = subtitle(manager, &thread).await;
        let timer = expire_timer(manager, &thread).await;
        let group = match &thread {
            Thread::Group(key) => manager
                .store()
                .group(*key)
                .await
                .ok()
                .flatten()
                .map(|group| (*key, group)),
            Thread::Contact(_) => None,
        };
        let mut conversation = match group {
            Some((key, group)) => {
                group_conversation(manager, &ctx.threads, &mut Names::new(), key, group).await
            }
            None => {
                let contact = match thread {
                    Thread::Contact(who) => Some(who),
                    Thread::Group(_) => None,
                };
                // Titled by their id: presage hasn't saved them as a contact yet.
                if let Some(who @ ServiceId::Aci(_)) = contact
                    && title == who.service_id_string()
                {
                    retitle_once_saved(manager, ctx, who);
                }
                let mut conversation = register(&ctx.threads, thread, title, subtitle, timer);
                if let Some(who) = contact {
                    conversation.trust = trust_of(manager, &ctx.tables, who).await;
                    ctx.trust.borrow_mut().insert(who, conversation.trust);
                }
                conversation
            }
        };
        conversation.settings = first_settings(ctx, &id, first.saturating_sub(1)).await;
        info!(conversation = %tag(&id), "new conversation");
        ctx.events.send(Event::Conversations(vec![conversation]));
    }
    id
}

/// Titles `who`'s chat once presage has saved them as a contact. presage fetches a new
/// sender's profile, with their name, in a task of its own when their message comes, and says
/// nothing when it is done: until then their chat is titled by their id.
fn retitle_once_saved(manager: &Mgr, ctx: &Ctx, who: ServiceId) {
    let (manager, ctx) = (manager.clone(), ctx.clone());
    tokio::task::spawn_local(async move {
        // About a minute in all: the fetch is a request to the server.
        let mut wait = Duration::from_millis(250);
        for _ in 0..8 {
            tokio::time::sleep(wait).await;
            if matches!(manager.store().contact_by_id(&who).await, Ok(Some(_))) {
                send_conversations(&manager, &ctx).await;
                return;
            }
            wait *= 2;
        }
        debug!("a new sender wasn't saved as a contact");
    });
}

/// How the UI names someone: "You" for us.
pub(super) async fn display_name(manager: &Mgr, names: &mut Names, who: ServiceId) -> String {
    if who == own_id(manager) {
        "You".into()
    } else {
        sender_name(manager, names, who).await
    }
}

/// What the details page shows about a thread. Whatever can't be found is left out.
pub(super) async fn details(
    manager: &mut Mgr,
    tables: &LocalStore,
    thread: &Thread,
) -> crate::Details {
    let mut details = crate::Details::default();
    match thread {
        Thread::Contact(who) => {
            if let Ok(Some(contact)) = manager.store().contact_by_id(who).await {
                details.phone = contact.phone_number.as_ref().map(display_number);
            }
            if *who != own_id(manager) {
                details.safety_number = safety_number(manager, *who).await;
                details.trust = trust_of(manager, tables, *who).await;
            }
            if let (ServiceId::Aci(aci), Ok(Some(key))) =
                (who, manager.store().profile_key(who).await)
            {
                match manager.retrieve_profile_by_uuid(*aci, key).await {
                    Ok(profile) => {
                        let about = [profile.about_emoji, profile.about]
                            .into_iter()
                            .flatten()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        details.about = (!about.is_empty()).then_some(about);
                    }
                    Err(error) => debug!(error = %scrubbed(&error), "couldn't fetch a profile"),
                }
            }
        }
        Thread::Group(key) => {
            if let Ok(Some(group)) = manager.store().group(*key).await {
                details.description = group.description.clone().filter(|d| !d.is_empty());
                let mut names = Names::new();
                for member in &group.members {
                    let who = ServiceId::Aci(member.aci);
                    let person = crate::Person {
                        service_id: who.service_id_string(),
                        name: display_name(manager, &mut names, who).await,
                    };
                    let admin =
                        member.role == presage::libsignal_service::groups_v2::Role::Administrator;
                    details.members.push((person, admin));
                }
                // Admins first, then by name.
                details
                    .members
                    .sort_by_cached_key(|(p, admin)| (!admin, p.name.to_lowercase()));
                use presage::libsignal_service::groups_v2::AccessRequired;
                let access = group
                    .access_control
                    .as_ref()
                    .map_or(AccessRequired::Unsatisfiable, |a| a.add_from_invite_link);
                details.invite_link_access = match access {
                    AccessRequired::Any => crate::InviteLinkAccess::Anyone,
                    AccessRequired::Administrator => crate::InviteLinkAccess::Approval,
                    _ => crate::InviteLinkAccess::Off,
                };
                details.invite_link = group.invite_link(*key).map(|link| link.to_url());
                for member in &group.requesting_members {
                    let who = ServiceId::Aci(member.aci);
                    details.requests.push(crate::Person {
                        service_id: who.service_id_string(),
                        name: display_name(manager, &mut names, who).await,
                    });
                }
            }
        }
    }
    details
}

/// A phone number as typed: international ("+49 151 …"), or in the way of `own`'s country
/// ("0151 …"); `None` unless it is a valid number.
fn parse_number(
    text: &str,
    own: &presage::libsignal_service::prelude::phonenumber::PhoneNumber,
) -> Option<presage::libsignal_service::prelude::phonenumber::PhoneNumber> {
    use presage::libsignal_service::prelude::phonenumber;
    let number = phonenumber::parse(own.country().id(), text.trim()).ok()?;
    phonenumber::is_valid(&number).then_some(number)
}

/// Finds who `number` belongs to on Signal (their ACI, or only their PNI if they don't let
/// themselves be found by number) and makes a chat with them, named by the number unless we
/// know them; returns that chat.
pub(super) async fn find_number(
    manager: &mut Mgr,
    ctx: &Ctx,
    number: &str,
) -> Result<ConversationId, Error> {
    let own = manager.registration_data().phone_number.clone();
    let parsed = parse_number(number, &own)
        .ok_or_else(|| Error::refused(format!("“{number}” isn't a phone number.")))?;
    let shown = display_number(&parsed);
    let found = manager
        .discover_contacts_by_phone_number([&parsed])
        .await
        .map_err(failed("failed to look the number up"))?;
    let who = found
        .into_iter()
        .find_map(|(_, who)| who)
        .ok_or_else(|| Error::refused(format!("Nobody on Signal has the number {shown}.")))?;
    let thread = Thread::Contact(who);
    let id = conversation_id(&thread);
    if !ctx.threads.borrow().contains_key(&id) {
        let title = match manager.thread_title(&thread).await {
            Ok(title) if !title.is_empty() && title != who.service_id_string() => title,
            _ => shown.clone(),
        };
        let mut conversation = register(&ctx.threads, thread, title, shown, 0);
        conversation.trust = trust_of(manager, &ctx.tables, who).await;
        conversation.settings = ctx
            .tables
            .settings(&id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        ctx.events.send(Event::Conversations(vec![conversation]));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{Background, GroupIds};
    use crate::{Events, Preferences};
    use sqlx::sqlite::SqliteConnectOptions;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn numbers_are_read_the_way_our_country_writes_them() {
        use presage::libsignal_service::prelude::phonenumber;
        let own = phonenumber::parse(None, "+49 151 23456789").unwrap();
        // As contact discovery gets them.
        let e164 = |text| {
            parse_number(text, &own).map(|n| n.format().mode(phonenumber::Mode::E164).to_string())
        };
        assert_eq!(e164("+49 1512 3456780").as_deref(), Some("+4915123456780"));
        assert_eq!(e164(" 01512 3456780 ").as_deref(), Some("+4915123456780"));
        assert!(
            parse_number("+44 7911 123456", &own).is_some(),
            "another country's"
        );
        assert_eq!(parse_number("Ann", &own), None);
        assert_eq!(parse_number("+49 1", &own), None, "too short to be valid");
    }

    /// Before the phone's settings are read, only a chat accepted here hears that its
    /// messages arrived; after, every chat but an unanswered message request does.
    #[test]
    fn receipts_go_only_to_accepted_chats() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = SqliteConnectOptions::new()
                .filename(dir.join("local.db3"))
                .create_if_missing(true);
            let (events, _ui) = async_channel::unbounded();
            let ctx = Ctx {
                threads: Rc::default(),
                events: Events::new(events),
                tables: LocalStore::open(options).await.unwrap(),
                preferences: Rc::new(Cell::new(Preferences::default())),
                expiring: Rc::default(),
                storage_changed: Rc::default(),
                storage_read: Rc::new(tokio::sync::watch::Sender::new(false)),
                trust: Rc::default(),
                group_ids: GroupIds::default(),
                background: Background::default(),
                outbox: tokio::sync::mpsc::channel(1).0,
            };
            let (ann, bob) = (
                ConversationId::contact("ann"),
                ConversationId::contact("bob"),
            );
            ctx.tables
                .update(&ann, |s| s.accepted = true)
                .await
                .unwrap();
            assert!(accepted(&ctx, &ann, false).await);
            assert!(!accepted(&ctx, &bob, false).await);

            ctx.tables
                .take_remote(&HashMap::new(), 1, |_, _, _| Vec::new())
                .await
                .unwrap();
            assert!(accepted(&ctx, &ann, true).await);
            assert!(!accepted(&ctx, &bob, true).await);
            ctx.tables
                .update(&bob, |s| s.accepted = true)
                .await
                .unwrap();
            assert!(accepted(&ctx, &bob, true).await);
            ctx.tables.close().await;
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A conversation new to b33p is read up to what was there when it came, so what arrives
    /// after stays unread; one it has settings for keeps them.
    #[test]
    fn a_new_conversation_is_read_up_to_its_start() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = SqliteConnectOptions::new()
                .filename(dir.join("local.db3"))
                .create_if_missing(true);
            let (events, _ui) = async_channel::unbounded();
            let ctx = Ctx {
                threads: Rc::default(),
                events: Events::new(events),
                tables: LocalStore::open(options).await.unwrap(),
                preferences: Rc::new(Cell::new(Preferences::default())),
                expiring: Rc::default(),
                storage_changed: Rc::default(),
                storage_read: Rc::new(tokio::sync::watch::Sender::new(false)),
                trust: Rc::default(),
                group_ids: GroupIds::default(),
                background: Background::default(),
                outbox: tokio::sync::mpsc::channel(1).0,
            };
            let ann = ConversationId::contact("ann");
            assert_eq!(first_settings(&ctx, &ann, 10).await.last_read, 10);
            ctx.tables
                .update(&ann, |s| s.marked_unread = true)
                .await
                .unwrap();
            let kept = first_settings(&ctx, &ann, 20).await;
            assert_eq!(kept.last_read, 10);
            assert!(kept.marked_unread);
            ctx.tables.close().await;
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
