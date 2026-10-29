//! Groups: how they are listed, what their changes say, editing them, joining by invite link.

use crate::Error;

use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::{
    libsignal_service::zkgroup::groups::{GroupMasterKey, GroupSecretParams},
    libsignal_service::{content::GroupContextV2, protocol::ServiceId},
    store::{ContentsStore, Thread},
};
use tracing::{debug, warn};

use crate::{Conversation, ConversationId, Event, attachments, scrub::scrubbed};

use super::{
    Ctx, Mgr, Threads,
    conversations::{
        Names, display_name, ensure_conversation, first_settings, members, own_id, register,
        sender_name,
    },
    history::to_chat_message,
};

/// The id typing indicators and the like name a group by.
pub(super) fn group_identifier(master_key: &[u8; 32]) -> [u8; 32] {
    GroupSecretParams::derive_from_master_key(GroupMasterKey::new(*master_key))
        .get_group_identifier()
}

/// A group as a conversation, registered: title, members, our role, timer.
pub(super) async fn group_conversation(
    manager: &Mgr,
    threads: &Threads,
    names: &mut Names,
    key: [u8; 32],
    group: presage::model::groups::Group,
) -> Conversation {
    use presage::libsignal_service::groups_v2::Role;
    let own = manager.registration_data().service_ids.aci();
    let us = group.members.iter().find(|m| m.aci == own);
    let left = us.is_none();
    let admin = us.is_some_and(|m| m.role == Role::Administrator);
    let timer = group
        .disappearing_messages_timer
        .as_ref()
        .map_or(0, |t| t.duration);
    let people = people(manager, names, &group).await;
    // Invited (by our ACI, or by our PNI if someone had only our number).
    let ids = &manager.registration_data().service_ids;
    let invited = left
        && group.pending_members.iter().any(|m| {
            use presage::model::ServiceIdType;
            match m.service_id_type {
                ServiceIdType::AccountIdentity => m.uuid == ids.aci,
                ServiceIdType::PhoneNumberIdentity => m.uuid == ids.pni,
            }
        });
    let requested = left && !invited && group.requesting_members.iter().any(|m| m.aci == own);
    let subtitle = if invited {
        "You're invited to this group.".into()
    } else if requested {
        "You asked to join this group; an admin has to let you in.".into()
    } else if left {
        "You aren't a member of this group.".into()
    } else {
        members(group.members.len())
    };
    let mut conversation = register(threads, Thread::Group(key), group.title, subtitle, timer);
    conversation.members = people;
    conversation.admin = admin;
    conversation.left = left;
    conversation.invited = invited;
    conversation.requested = requested;
    conversation
}

/// Tells the UI how a group is now, e.g. after a change.
pub(super) async fn refresh_group(manager: &Mgr, ctx: &Ctx, key: [u8; 32]) {
    let Ok(Some(group)) = manager.store().group(key).await else {
        return;
    };
    let mut conversation =
        group_conversation(manager, &ctx.threads, &mut Names::new(), key, group).await;
    let id = conversation.id.clone();
    // A group that is new here has nothing unread yet.
    conversation.settings = first_settings(ctx, &id, 0).await;
    ctx.events.send(Event::Conversations(vec![conversation]));
}

/// Keeps and shows the changes to a group that no message told of, which presage read from the
/// server's log: each as the message that would have told of it, from its editor, so it reads
/// as the others do. They were made before now, in this order; when exactly, the log doesn't
/// say.
pub(super) async fn on_missed_changes(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    key: [u8; 32],
    missed: Vec<presage::model::messages::MissedGroupChange>,
) -> bool {
    use presage::libsignal_service::content::{Content, ContentBody, Metadata};
    use presage::libsignal_service::push_service::DEFAULT_DEVICE_ID;
    let thread = Thread::Group(key);
    let conversation = super::conversations::conversation_id(&thread);
    let now = crate::now_millis();
    let count = missed.len() as u64;
    for (n, missed) in (0u64..).zip(missed) {
        let at = now.saturating_sub(count - n);
        let time = chrono::DateTime::from_timestamp_millis(at as i64).unwrap_or_default();
        let data = presage::proto::DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(key.to_vec()),
                revision: Some(missed.changes.version),
                group_change: Some(missed.signed.encode_to_vec()),
            }),
            timestamp: Some(at),
            ..Default::default()
        };
        let content = Content {
            metadata: Metadata {
                sender: ServiceId::Aci(missed.changes.editor),
                destination: own_id(manager),
                sender_device: *DEFAULT_DEVICE_ID,
                pni_verified: None,
                client_timestamp: time,
                server_timestamp: time,
                needs_receipt: false,
                unidentified_sender: false,
                was_plaintext: false,
                server_guid: None,
            },
            body: ContentBody::DataMessage(data),
        };
        if let Err(error) = manager.store().save_message(&thread, content.clone()).await {
            warn!(error = %scrubbed(&error), "failed to keep a missed group change");
            continue;
        }
        if let Some(message) = to_chat_message(manager, names, conversation.clone(), &content).await
        {
            ctx.events.send(Event::Message(message));
        }
    }
    refresh_group(manager, ctx, key).await;
    true
}

/// The group change a message carries, decrypted with the group's key, if the server signed
/// it. Only a signed change counts: the bytes are the sender's, so any member could make up
/// one that was never applied (removals, admin rights, a timer) and have it narrated or acted
/// on. Changes made here go out with the server's signature too (presage sends the change
/// the server returned), so they pass.
pub(super) async fn signed_group_change(
    manager: &Mgr,
    key: [u8; 32],
    context: &GroupContextV2,
) -> Option<presage::libsignal_service::groups_v2::GroupChanges> {
    use presage::libsignal_service::groups_v2::GroupOperations;
    let change = presage::proto::GroupChange::decode(context.group_change.as_deref()?).ok()?;
    let servers = presage::libsignal_service::configuration::ServiceConfiguration::from(
        manager.registration_data().signal_servers.clone(),
    );
    let signed = <[u8; 64]>::try_from(change.server_signature.as_slice()).is_ok_and(|signature| {
        servers
            .zkgroup_server_public_params
            .verify_signature(&change.actions, signature)
            .is_ok()
    });
    if !signed {
        debug!("a group change without the server's signature; ignored");
        return None;
    }
    let operations = GroupOperations::new(GroupSecretParams::derive_from_master_key(
        GroupMasterKey::new(key),
    ));
    operations.decrypt_group_change(change).ok()
}

/// If `context`'s change removed us, as a member, an invitee or a requester, drops us from the
/// stored group: presage can't read the group from the server any more (it answers 403 to a
/// non-member), so its copy would still count us in, or keep inviting us.
pub(super) async fn forget_membership(manager: &Mgr, key: [u8; 32], context: &GroupContextV2) {
    let ids = &manager.registration_data().service_ids;
    let Some(changes) = signed_group_change(manager, key, context).await else {
        return;
    };
    let removed = own_removal(&changes, ids.aci(), ids.pni());
    let store = manager.store().clone();
    let Ok(Some(mut group)) = store.group(key).await else {
        return;
    };
    if removed.is_empty() || group.revision >= changes.version {
        return;
    }
    if removed.contains(&Removal::Member) {
        group.members.retain(|member| member.aci != ids.aci());
    }
    if removed.contains(&Removal::Invitee) {
        group
            .pending_members
            .retain(|member| member.uuid != ids.aci && member.uuid != ids.pni);
    }
    if removed.contains(&Removal::Requester) {
        group
            .requesting_members
            .retain(|member| member.aci != ids.aci());
    }
    group.revision = changes.version;
    if let Err(error) = store.save_group(key, group).await {
        warn!(error = %scrubbed(&error), "failed to note that we were removed from a group");
    }
}

/// How a group change can drop us; see [`own_removal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal {
    Member,
    Invitee,
    Requester,
}

/// The ways `changes` drops us from a group: as a member, as an invitee (by our ACI, or by our
/// PNI if someone had only our number) or as someone asking to join.
fn own_removal(
    changes: &presage::libsignal_service::groups_v2::GroupChanges,
    aci: presage::libsignal_service::protocol::Aci,
    pni: presage::libsignal_service::protocol::Pni,
) -> Vec<Removal> {
    use presage::libsignal_service::groups_v2::GroupChange;
    let mut removed = Vec::new();
    for change in &changes.changes {
        let removal = match change {
            GroupChange::DeleteMember(who) if *who == aci => Removal::Member,
            GroupChange::DeletePendingMember(who)
                if *who == ServiceId::Aci(aci) || *who == ServiceId::Pni(pni) =>
            {
                Removal::Invitee
            }
            GroupChange::DeleteRequestingMember(who) if *who == aci => Removal::Requester,
            _ => continue,
        };
        if !removed.contains(&removal) {
            removed.push(removal);
        }
    }
    removed
}

/// Whether `changes` lets us into the group: adds us, or takes our request to join or our
/// invitation (by our ACI, or by our PNI if someone had only our number). What b33p showed of
/// the group from before, its picture, may have changed while we were out.
pub(super) fn lets_us_in(
    changes: &presage::libsignal_service::groups_v2::GroupChanges,
    aci: presage::libsignal_service::protocol::Aci,
    pni: presage::libsignal_service::protocol::Pni,
) -> bool {
    use presage::libsignal_service::groups_v2::GroupChange;
    changes.changes.iter().any(|change| match change {
        GroupChange::NewMember(member) => member.aci == aci,
        GroupChange::PromoteRequestingMember { aci: who, .. } => *who == aci,
        GroupChange::PromotePendingMember { address, .. } => {
            *address == ServiceId::Aci(aci) || *address == ServiceId::Pni(pni)
        }
        GroupChange::PromotePendingPniAciMemberProfileKey(promoted) => promoted.aci == aci,
        _ => false,
    })
}

/// Who brought us into a group, by the message that told us: the editor of a change that lets us
/// in ([`lets_us_in`]), or the sender of a new group's announcement (revision 0 and nothing else
/// in it, as presage's `create_group` sends to the members it made the group with). Signal-Desktop
/// keeps the same as a group's `addedBy` (ts/util/isConversationAccepted.preload.ts).
pub(super) fn added_by(
    changes: Option<&presage::libsignal_service::groups_v2::GroupChanges>,
    announcement: bool,
    sender: ServiceId,
    aci: presage::libsignal_service::protocol::Aci,
    pni: presage::libsignal_service::protocol::Pni,
) -> Option<ServiceId> {
    match changes {
        Some(changes) => lets_us_in(changes, aci, pni).then_some(ServiceId::Aci(changes.editor)),
        None => announcement.then_some(sender),
    }
}

/// Whether `data` is a new group's announcement, as [`added_by`] takes it: at revision 0, without
/// a change, and nothing else in it but what any message carries (the sender's profile key, the
/// chat's timer). Not a member's reaction, delete or profile key update (which accepting the
/// group sends) while the group is still at revision 0: that member didn't bring us in.
pub(super) fn is_announcement(data: &presage::libsignal_service::content::DataMessage) -> bool {
    let Some(context) = &data.group_v2 else {
        return false;
    };
    let rest = presage::libsignal_service::content::DataMessage {
        group_v2: None,
        profile_key: None,
        timestamp: None,
        expire_timer: None,
        expire_timer_version: None,
        required_protocol_version: None,
        ..data.clone()
    };
    context.revision == Some(0)
        && context.group_change.is_none()
        && rest == presage::libsignal_service::content::DataMessage::default()
}

/// A group isn't a message request when whoever brought us into it is someone we accepted (or
/// wrote to): Signal-Desktop's `isConversationAccepted`, by `isFromOrAddedByTrustedContact` and
/// `isTrustedContact` (profile sharing, which accepting or writing to someone gives). Accepts the
/// group `id` then, as a member, not when only invited; says so.
pub(super) async fn accept_from_trusted(
    manager: &Mgr,
    ctx: &Ctx,
    key: [u8; 32],
    id: &ConversationId,
    context: &GroupContextV2,
    announcement: bool,
    sender: ServiceId,
) {
    let ids = &manager.registration_data().service_ids;
    let (aci, pni) = (ids.aci(), ids.pni());
    let changes = match context.group_change {
        Some(_) => match signed_group_change(manager, key, context).await {
            Some(changes) => Some(changes),
            // A change without the server's signature brings nobody in.
            None => return,
        },
        None => None,
    };
    let Some(adder) = added_by(changes.as_ref(), announcement, sender, aci, pni) else {
        return;
    };
    if adder == ServiceId::Aci(aci) || !is_member(manager, key, ServiceId::Aci(aci)).await {
        return;
    }
    let accepted = async |id: &ConversationId| {
        ctx.tables
            .settings(id)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.accepted)
    };
    let by = ConversationId::contact(&adder.service_id_string());
    if accepted(id).await || !accepted(&by).await {
        return;
    }
    match ctx.tables.update(id, |s| s.accepted = true).await {
        Ok(settings) => {
            ctx.events.send(Event::Settings {
                conversation: id.clone(),
                settings,
            });
        }
        Err(error) => warn!(error = %scrubbed(&error), "failed to accept a group from a contact"),
    }
}

/// Whether `changes` is `sender` changing their own place in the group: leaving it as a member
/// or an invitee, asking to join through the link or taking that back, or joining through it.
/// The messages a non-member may still send to the group (Desktop sends its request to the
/// members, ts/groups.preload.ts `modifyGroupV2`).
pub(super) fn own_membership_change(
    changes: &presage::libsignal_service::groups_v2::GroupChanges,
    sender: ServiceId,
) -> bool {
    use presage::libsignal_service::groups_v2::GroupChange;
    ServiceId::Aci(changes.editor) == sender
        && changes.changes.iter().any(|change| match change {
            GroupChange::DeleteMember(who) | GroupChange::DeleteRequestingMember(who) => {
                ServiceId::Aci(*who) == sender
            }
            GroupChange::DeletePendingMember(who) => *who == sender,
            GroupChange::NewRequestingMember(member) => ServiceId::Aci(member.aci) == sender,
            GroupChange::NewMember(member) => ServiceId::Aci(member.aci) == sender,
            _ => false,
        })
}

/// Why someone can't be put in a group.
pub(super) const PHONE_NUMBER_ONLY: &str = "someone chosen is known only by their phone number: they can be \
     invited once the group exists (make it, then add them)";

/// A group member's ACI; `None` for those known only by their phone number (PNI).
pub(super) fn member_aci(id: &str) -> Option<presage::libsignal_service::protocol::Aci> {
    match ServiceId::parse_from_service_id_string(id)? {
        ServiceId::Aci(aci) => Some(aci),
        ServiceId::Pni(_) => None,
    }
}

/// Refuses to leave a group we are the only admin of, while others are in it: it would be
/// left without one. Signal's apps ask to choose a new admin first; here it is done by hand.
pub(super) async fn only_admin_check(manager: &Mgr, key: [u8; 32]) -> Result<(), Error> {
    use presage::libsignal_service::groups_v2::Role;
    let own = manager.registration_data().service_ids.aci();
    let Ok(Some(group)) = manager.store().group(key).await else {
        return Ok(());
    };
    let admin = |m: &&presage::model::groups::Member| m.role == Role::Administrator;
    let we_admin = group.members.iter().filter(admin).any(|m| m.aci == own);
    let other_admin = group.members.iter().filter(admin).any(|m| m.aci != own);
    let others = group.members.iter().any(|m| m.aci != own);
    if we_admin && !other_admin && others {
        return Err(Error::refused(
            "you are the group's only admin: make someone else an admin first",
        ));
    }
    Ok(())
}

/// A group change for presage, or why it can't be made (a member isn't an ACI, a picture
/// can't be read).
pub(super) async fn group_edit(
    edit: crate::GroupEdit,
) -> Result<presage::manager::GroupEdit, Error> {
    use crate::GroupEdit as Edit;
    use presage::manager::GroupEdit as To;
    let aci = |member: &str| member_aci(member).ok_or_else(|| Error::refused(PHONE_NUMBER_ONLY));
    Ok(match edit {
        Edit::Title(title) => To::Title(title.trim().to_owned()),
        Edit::Description(description) => To::Description(description.trim().to_owned()),
        // Reading, scaling and encoding a photo takes a while: not on this thread.
        Edit::SetPicture(path) => To::Avatar(
            tokio::task::spawn_blocking(move || group_picture(&path))
                .await
                .map_err(|e| Error::refused(e.to_string()))??,
        ),
        Edit::RemovePicture => To::Avatar(Vec::new()),
        // Someone known only by phone number is invited by it.
        Edit::AddMembers(members) => To::AddMembers(
            members
                .iter()
                .map(|m| ServiceId::parse_from_service_id_string(m))
                .collect::<Option<_>>()
                .ok_or_else(|| Error::refused(PHONE_NUMBER_ONLY))?,
        ),
        Edit::RemoveMember(member) => To::RemoveMember(aci(&member)?),
        Edit::SetAdmin(member, admin) => To::SetAdmin(aci(&member)?, admin),
        Edit::Leave => To::Leave,
        Edit::AcceptInvitation => To::AcceptInvitation,
        Edit::DeclineInvitation => To::DeclineInvitation,
        Edit::InviteLink(access) => {
            use presage::libsignal_service::groups_v2::AccessRequired;
            To::InviteLinkAccess(match access {
                crate::InviteLinkAccess::Off => AccessRequired::Unsatisfiable,
                crate::InviteLinkAccess::Anyone => AccessRequired::Any,
                crate::InviteLinkAccess::Approval => AccessRequired::Administrator,
            })
        }
        Edit::ResetInviteLink => To::ResetInviteLink,
        Edit::ApproveRequest(member) => To::ApproveRequest(aci(&member)?),
        Edit::DenyRequest(member) => To::DenyRequest(aci(&member)?),
        // Not a change to the group: see `Command::EditGroup`.
        Edit::CancelJoinRequest => return Err(Error::refused("not in the group")),
    })
}

/// What kind of change `edit` is, for the log: not what it says, nor who it names.
pub(super) fn edit_kind(edit: &presage::manager::GroupEdit) -> &'static str {
    use presage::manager::GroupEdit as Edit;
    match edit {
        Edit::Title(_) => "title",
        Edit::Description(_) => "description",
        Edit::Avatar(_) => "picture",
        Edit::Timer(_) => "timer",
        Edit::AddMembers(_) => "add members",
        Edit::RemoveMember(_) => "remove a member",
        Edit::SetAdmin(..) => "admin rights",
        Edit::Leave => "leave",
        Edit::AcceptInvitation => "accept an invitation",
        Edit::DeclineInvitation => "decline an invitation",
        Edit::InviteLinkAccess(_) => "invite link access",
        Edit::ResetInviteLink => "reset the invite link",
        Edit::ApproveRequest(_) => "approve a request",
        Edit::DenyRequest(_) => "deny a request",
    }
}

pub(super) fn parse_group_link(
    link: &str,
) -> Result<presage::libsignal_service::groups_v2::GroupInviteLink, Error> {
    presage::libsignal_service::groups_v2::GroupInviteLink::parse(link)
        .ok_or_else(|| Error::refused("That isn't a Signal group link."))
}

/// The conversation of a group we are a member of.
pub(super) async fn member_of(manager: &Mgr, ctx: &Ctx, key: [u8; 32]) -> Option<ConversationId> {
    let own = manager.registration_data().service_ids.aci();
    let group = manager.store().group(key).await.ok()??;
    if !group.members.iter().any(|m| m.aci == own) {
        return None;
    }
    Some(ensure_conversation(manager, ctx, Thread::Group(key), 0).await)
}

/// For a group we asked to join: whether the request still stands. A refusal isn't told to
/// us (nor is the link being turned off), so the stored group would say we wait for ever; and
/// an acceptance may be on its way still, in which case the server shows us the group.
pub(super) async fn check_request(manager: &mut Mgr, ctx: &Ctx, key: [u8; 32]) {
    use presage::libsignal_service::groups_v2::GroupInviteLink;
    use presage::libsignal_service::push_service::ServiceError;
    let own = manager.registration_data().service_ids.aci();
    let asking = |group: &presage::model::groups::Group| {
        !group.members.iter().any(|m| m.aci == own)
            && group.requesting_members.iter().any(|m| m.aci == own)
    };
    let Ok(Some(group)) = manager.store().group(key).await else {
        return;
    };
    if !asking(&group) {
        return;
    }
    match manager.fetch_group(&key).await {
        // Let in: the group is stored as it is now, with us in it.
        Ok(_) => {
            refresh_group(manager, ctx, key).await;
            return;
        }
        Err(presage::Error::ServiceError(ServiceError::Unauthorized)) => {}
        Err(error) => {
            debug!(%error, "couldn't check on a request to join");
            return;
        }
    }
    let link = GroupInviteLink {
        master_key: key,
        password: group.invite_link_password,
    };
    let standing = match manager.group_join_info(&link).await {
        Ok(info) => info.pending_admin_approval,
        // The link is gone, and the request with it.
        Err(presage::Error::ServiceError(ServiceError::Unauthorized)) => false,
        Err(error) => {
            debug!(%error, "couldn't check on a request to join");
            return;
        }
    };
    if standing {
        return;
    }
    // Read again: receiving may have replaced the group meanwhile.
    let Ok(Some(mut group)) = manager.store().group(key).await else {
        return;
    };
    if asking(&group) {
        group.requesting_members.retain(|m| m.aci != own);
        if let Err(error) = manager.store().save_group(key, group).await {
            warn!(%error, "failed to note that a request to join is over");
        }
    }
    refresh_group(manager, ctx, key).await;
}

/// For a group we are an admin of: brings it up to date, so that requests to join show;
/// whether it did.
/// Nobody tells an admin of a request. The one asking isn't a member, and tells only the
/// members it knew, if any. So a request shows only once the group is fetched, and the details
/// are where an admin looks for one. Signal-Desktop reads a group's log whenever its chat
/// opens, at most every five minutes (`waitThenMaybeUpdateGroup`).
pub(super) async fn fetch_as_admin(manager: &mut Mgr, ctx: &Ctx, key: [u8; 32]) -> bool {
    use presage::libsignal_service::groups_v2::Role;
    let own = manager.registration_data().service_ids.aci();
    let Ok(Some(group)) = manager.store().group(key).await else {
        return false;
    };
    let admin = group
        .members
        .iter()
        .any(|m| m.aci == own && m.role == Role::Administrator);
    if !admin {
        return false;
    }
    // What changed since, from the group's log: a request among it, perhaps.
    match manager.update_group(&key).await {
        Ok(missed) => {
            on_missed_changes(manager, ctx, &mut Names::new(), key, missed).await;
            true
        }
        Err(error) => {
            debug!(%error, "couldn't update a group we are an admin of");
            false
        }
    }
}

/// The side of a group picture as sent, as Signal's clients have it.
const GROUP_PICTURE_SIDE: u32 = 1024;

/// The picture at `path` as a group's: cropped to a square, no larger than
/// [`GROUP_PICTURE_SIDE`], as JPEG.
fn group_picture(path: &std::path::Path) -> Result<Vec<u8>, Error> {
    let bytes = std::fs::read(path)
        .map_err(|error| Error::storage(format!("couldn't read the picture: {error}")))?;
    let image = attachments::decode_limited(&bytes)
        .ok_or_else(|| Error::refused("that file isn't a picture"))?;
    let side = image.width().min(image.height()).min(GROUP_PICTURE_SIDE);
    if side == 0 {
        return Err(Error::refused("that picture is empty"));
    }
    let square = image.resize_to_fill(side, side, image::imageops::FilterType::Lanczos3);
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(square.to_rgb8())
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut jpeg, 85,
        ))
        .map_err(|error| Error::refused(format!("couldn't encode the picture: {error}")))?;
    Ok(jpeg.into_inner())
}

/// Shows a message we stored ourselves (e.g. a group change), as it will read after a restart.
pub(super) async fn show_stored(
    manager: &Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    thread: &Thread,
    timestamp: u64,
) {
    if let Ok(Some(content)) = manager.store().message(thread, timestamp).await
        && let Some(message) =
            to_chat_message(manager, &mut Names::new(), conversation.clone(), &content).await
    {
        ctx.events.send(Event::Message(message));
    }
}

/// What a group change says, from the change itself (decrypted with the group's key, and only
/// if the server signed it; see [`signed_group_change`]), and the timer it sets, if it does.
/// `editor` is who made it; `editor_is_us` whether that is us.
pub(super) async fn group_notice(
    manager: &Mgr,
    names: &mut Names,
    context: &GroupContextV2,
    editor: &str,
    editor_is_us: bool,
) -> Option<(String, Option<u32>)> {
    use presage::libsignal_service::groups_v2::{GroupChange, Role};
    let key: [u8; 32] = context.master_key.clone()?.try_into().ok()?;
    if context.group_change.is_none() {
        // A group's first message, from whoever made it.
        return (context.revision == Some(0)).then(|| {
            let text = if editor_is_us {
                "You created the group.".to_string()
            } else {
                format!("{editor} added you to the group.")
            };
            (text, None)
        });
    }
    let changes = signed_group_change(manager, key, context).await?;
    let editor_id = ServiceId::Aci(changes.editor);
    let editor = display_name(manager, names, editor_id).await;
    let mut lines = Vec::new();
    let mut timer = None;
    // Turning the link on sets its password too; that is one thing to say, not two.
    let sets_link_access = changes
        .changes
        .iter()
        .any(|change| matches!(change, GroupChange::InviteLinkAccess(_)));
    for change in changes.changes {
        let line = match change {
            GroupChange::NewMember(member) => {
                let name = display_name(manager, names, ServiceId::Aci(member.aci)).await;
                if ServiceId::Aci(member.aci) == editor_id {
                    format!("{editor} joined the group.")
                } else {
                    format!("{editor} added {name}.")
                }
            }
            GroupChange::DeleteMember(aci) => {
                let name = display_name(manager, names, ServiceId::Aci(aci)).await;
                if ServiceId::Aci(aci) == editor_id {
                    format!("{name} left the group.")
                } else {
                    format!("{editor} removed {name}.")
                }
            }
            GroupChange::ModifyMemberRole { aci, role } => {
                let name = display_name(manager, names, ServiceId::Aci(aci)).await;
                if role == Role::Administrator {
                    format!("{editor} made {name} an admin.")
                } else {
                    format!("{editor} took {name}'s admin rights.")
                }
            }
            GroupChange::NewPendingMember(_) => format!("{editor} invited someone."),
            GroupChange::PromotePendingMember { address, .. } => {
                let name = display_name(manager, names, address).await;
                format!("{name} joined the group.")
            }
            // Invited by phone number, and joined.
            GroupChange::PromotePendingPniAciMemberProfileKey(member) => {
                let name = display_name(manager, names, ServiceId::Aci(member.aci)).await;
                format!("{name} joined the group.")
            }
            GroupChange::DeletePendingMember(invitee) => {
                // Our own invitation may be for our PNI, which only we can tell is us.
                let own_pni = ServiceId::Pni(manager.registration_data().service_ids.pni());
                if invitee == editor_id || (editor_id == own_id(manager) && invitee == own_pni) {
                    format!("{editor} declined the invitation.")
                } else {
                    format!("{editor} withdrew an invitation.")
                }
            }
            GroupChange::Title(title) => format!("{editor} named the group “{title}”."),
            GroupChange::Description(_) => format!("{editor} changed the group's description."),
            GroupChange::Avatar(_) => format!("{editor} changed the group's picture."),
            GroupChange::Timer(t) => {
                let seconds = t.map_or(0, |t| t.duration);
                timer = Some(seconds);
                crate::timer_notice(&editor, seconds)
            }
            GroupChange::NewRequestingMember(member) => {
                let name = display_name(manager, names, ServiceId::Aci(member.aci)).await;
                format!("{name} asked to join the group.")
            }
            GroupChange::PromoteRequestingMember { aci, .. } => {
                if ServiceId::Aci(aci) == own_id(manager) {
                    format!("{editor} accepted your request to join.")
                } else {
                    let name = display_name(manager, names, ServiceId::Aci(aci)).await;
                    format!("{editor} accepted {name}'s request to join.")
                }
            }
            GroupChange::DeleteRequestingMember(aci) => {
                let name = display_name(manager, names, ServiceId::Aci(aci)).await;
                if ServiceId::Aci(aci) == editor_id {
                    format!("{name} took back the request to join.")
                } else if ServiceId::Aci(aci) == own_id(manager) {
                    format!("{editor} turned your request to join down.")
                } else {
                    format!("{editor} turned {name}'s request to join down.")
                }
            }
            GroupChange::InviteLinkAccess(access) => {
                use presage::libsignal_service::groups_v2::AccessRequired;
                match access {
                    AccessRequired::Any => format!("{editor} turned the invite link on."),
                    AccessRequired::Administrator => {
                        format!("{editor} turned the invite link on, with admin approval.")
                    }
                    _ => format!("{editor} turned the invite link off."),
                }
            }
            GroupChange::InviteLinkPassword(_) if !sets_link_access => {
                format!("{editor} reset the invite link.")
            }
            _ => continue,
        };
        lines.push(line);
    }
    (!lines.is_empty()).then(|| (lines.join("\n"), timer))
}

/// A group's members other than us, by name.
async fn people(
    manager: &Mgr,
    names: &mut Names,
    group: &presage::model::groups::Group,
) -> Vec<crate::Person> {
    let own = own_id(manager);
    let mut people = Vec::new();
    for member in &group.members {
        let who = ServiceId::Aci(member.aci);
        if who != own {
            people.push(crate::Person {
                service_id: who.service_id_string(),
                name: sender_name(manager, names, who).await,
            });
        }
    }
    people.sort_by_cached_key(|p| p.name.to_lowercase());
    people
}

/// Whether `who` is in the group `key`, as its stored copy has it.
pub(super) async fn is_member(manager: &Mgr, key: [u8; 32], who: ServiceId) -> bool {
    matches!(
        manager.store().group(key).await,
        Ok(Some(group)) if group.members.iter().any(|member| ServiceId::Aci(member.aci) == who)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::tests::aci;

    fn changes(
        editor: presage::libsignal_service::protocol::Aci,
        changes: Vec<presage::libsignal_service::groups_v2::GroupChange>,
    ) -> presage::libsignal_service::groups_v2::GroupChanges {
        presage::libsignal_service::groups_v2::GroupChanges {
            group_id: [0; 32],
            editor,
            version: 7,
            changes,
            change_epoch: 0,
        }
    }

    /// Someone who isn't in a group may still send it a change of their own place in it: leaving
    /// (as a member or an invitee), asking to join or taking that back. Being removed by someone
    /// else, or any other change, is not it.
    #[test]
    fn a_non_member_may_only_change_their_own_membership() {
        use presage::libsignal_service::groups_v2::{GroupChange, RequestingMember};
        use presage::libsignal_service::zkgroup::profiles::ProfileKey;
        let (bob, admin) = (aci(2), aci(3));
        let sender = ServiceId::Aci(bob);
        let request = |who| {
            GroupChange::NewRequestingMember(RequestingMember {
                aci: who,
                profile_key: ProfileKey::create([0; 32]),
                timestamp: 0,
            })
        };
        assert!(own_membership_change(
            &changes(bob, vec![request(bob)]),
            sender
        ));
        assert!(own_membership_change(
            &changes(bob, vec![GroupChange::DeleteRequestingMember(bob)]),
            sender
        ));
        assert!(!own_membership_change(
            &changes(bob, vec![request(admin)]),
            sender
        ));
        assert!(!own_membership_change(
            &changes(admin, vec![request(bob)]),
            sender
        ));
        assert!(own_membership_change(
            &changes(bob, vec![GroupChange::DeleteMember(bob)]),
            sender
        ));
        assert!(own_membership_change(
            &changes(bob, vec![GroupChange::DeletePendingMember(sender)]),
            sender
        ));
        assert!(!own_membership_change(
            &changes(admin, vec![GroupChange::DeleteMember(bob)]),
            sender
        ));
        assert!(!own_membership_change(
            &changes(bob, vec![GroupChange::DeleteMember(admin)]),
            sender
        ));
        assert!(!own_membership_change(
            &changes(bob, vec![GroupChange::Title("hi".into())]),
            sender
        ));
        assert!(!own_membership_change(&changes(bob, vec![]), sender));
    }

    /// A change that takes us out of a group, in whichever role, is recognised, so that the
    /// stored group (which can't be fetched any more) stops counting or inviting us.
    #[test]
    fn a_change_that_drops_us_is_recognised() {
        use presage::libsignal_service::groups_v2::GroupChange;
        use presage::libsignal_service::protocol::Pni;
        let (own, other, admin) = (aci(1), aci(2), aci(3));
        let own_pni = Pni::from(presage::libsignal_service::prelude::Uuid::from_u128(11));
        let removal = |change| own_removal(&changes(admin, vec![change]), own, own_pni);
        assert_eq!(removal(GroupChange::DeleteMember(own)), [Removal::Member]);
        assert_eq!(
            removal(GroupChange::DeletePendingMember(ServiceId::Aci(own))),
            [Removal::Invitee]
        );
        assert_eq!(
            removal(GroupChange::DeletePendingMember(ServiceId::Pni(own_pni))),
            [Removal::Invitee]
        );
        assert_eq!(
            removal(GroupChange::DeleteRequestingMember(own)),
            [Removal::Requester]
        );
        assert!(removal(GroupChange::DeleteMember(other)).is_empty());
        assert!(removal(GroupChange::DeletePendingMember(ServiceId::Aci(other))).is_empty());
        assert!(removal(GroupChange::Title("hi".into())).is_empty());
        let both = own_removal(
            &changes(
                admin,
                vec![
                    GroupChange::DeleteMember(own),
                    GroupChange::DeletePendingMember(ServiceId::Aci(own)),
                    GroupChange::DeleteMember(own),
                ],
            ),
            own,
            own_pni,
        );
        assert_eq!(both, [Removal::Member, Removal::Invitee]);
    }

    /// A change that lets us in, however we asked or were asked, is recognised, so that the
    /// group's picture is shown as it is now, not as it was before we were out. (libsignal-service
    /// makes no `PromotedMember` outside it, so that way in goes untested.)
    #[test]
    fn a_change_that_lets_us_in_is_recognised() {
        use presage::libsignal_service::groups_v2::{GroupChange, Member, Role};
        use presage::libsignal_service::protocol::Pni;
        use presage::libsignal_service::zkgroup::profiles::ProfileKey;
        let (own, other, admin) = (aci(1), aci(2), aci(3));
        let own_pni = Pni::from(presage::libsignal_service::prelude::Uuid::from_u128(11));
        let profile_key = ProfileKey::create([0; 32]);
        let member = |aci| Member {
            aci,
            role: Role::Default,
            profile_key,
            joined_at_version: 7,
            label: None,
            label_emoji: None,
        };
        let lets_in = |change| lets_us_in(&changes(admin, vec![change]), own, own_pni);
        assert!(lets_in(GroupChange::NewMember(member(own))));
        assert!(lets_in(GroupChange::PromoteRequestingMember {
            aci: own,
            role: Role::Default
        }));
        assert!(lets_in(GroupChange::PromotePendingMember {
            address: ServiceId::Aci(own),
            profile_key
        }));
        assert!(lets_in(GroupChange::PromotePendingMember {
            address: ServiceId::Pni(own_pni),
            profile_key
        }));
        assert!(!lets_in(GroupChange::NewMember(member(other))));
        assert!(!lets_in(GroupChange::PromoteRequestingMember {
            aci: other,
            role: Role::Default
        }));
        assert!(!lets_in(GroupChange::DeleteMember(own)));
        assert!(!lets_in(GroupChange::Title("hi".into())));
    }

    /// Who brought us into a group: the editor of a change that lets us in, or the sender of the
    /// announcement of a group made with us in it; nobody for any other change or message.
    #[test]
    fn who_added_us_is_known() {
        use presage::libsignal_service::groups_v2::{GroupChange, Member, Role};
        use presage::libsignal_service::protocol::Pni;
        use presage::libsignal_service::zkgroup::profiles::ProfileKey;
        let (own, other, admin) = (aci(1), aci(2), aci(3));
        let own_pni = Pni::from(presage::libsignal_service::prelude::Uuid::from_u128(11));
        let sender = ServiceId::Aci(other);
        let member = |aci| Member {
            aci,
            role: Role::Default,
            profile_key: ProfileKey::create([0; 32]),
            joined_at_version: 7,
            label: None,
            label_emoji: None,
        };
        let by_change = |change| {
            added_by(
                Some(&changes(admin, vec![change])),
                false,
                sender,
                own,
                own_pni,
            )
        };
        assert_eq!(
            by_change(GroupChange::NewMember(member(own))),
            Some(ServiceId::Aci(admin))
        );
        assert_eq!(
            by_change(GroupChange::PromoteRequestingMember {
                aci: own,
                role: Role::Default
            }),
            Some(ServiceId::Aci(admin))
        );
        assert_eq!(by_change(GroupChange::NewMember(member(other))), None);
        assert_eq!(by_change(GroupChange::Title("hi".into())), None);
        // A change that lets us in is the editor's, even sent by someone else.
        assert_eq!(
            added_by(
                Some(&changes(admin, vec![GroupChange::NewMember(member(own))])),
                true,
                sender,
                own,
                own_pni
            ),
            Some(ServiceId::Aci(admin))
        );
        assert_eq!(added_by(None, true, sender, own, own_pni), Some(sender));
        assert_eq!(added_by(None, false, sender, own, own_pni), None);
    }

    /// A group picture goes out as a square JPEG no larger than Signal's clients send,
    /// whatever the file was; something that isn't a picture is refused with a reason.
    #[test]
    fn a_group_picture_is_a_square_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let wide = dir.path().join("wide.png");
        image::RgbaImage::from_pixel(3000, 1200, image::Rgba([10, 200, 30, 255]))
            .save(&wide)
            .unwrap();
        let jpeg = group_picture(&wide).unwrap();
        let sent = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(
            image::guess_format(&jpeg).unwrap(),
            image::ImageFormat::Jpeg
        );
        assert_eq!((sent.width(), sent.height()), (1024, 1024));
        let small = dir.path().join("small.png");
        image::RgbaImage::from_pixel(30, 50, image::Rgba([0, 0, 0, 255]))
            .save(&small)
            .unwrap();
        let sent = image::load_from_memory(&group_picture(&small).unwrap()).unwrap();
        assert_eq!((sent.width(), sent.height()), (30, 30));
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        assert_eq!(
            group_picture(&text).unwrap_err(),
            Error::refused("that file isn't a picture")
        );
        assert!(group_picture(&dir.path().join("missing.png")).is_err());
    }

    /// The message presage's `create_group` sends is the announcement; a member's profile key
    /// update or reaction at revision 0, such as accepting the group sends, isn't.
    #[test]
    fn only_a_bare_message_at_revision_0_announces_a_group() {
        use presage::libsignal_service::content::DataMessage;
        use presage::proto::data_message::{Flags, Reaction};
        let created = DataMessage {
            group_v2: Some(GroupContextV2 {
                master_key: Some(vec![1; 32]),
                revision: Some(0),
                ..Default::default()
            }),
            profile_key: Some(vec![2; 32]),
            timestamp: Some(1),
            ..Default::default()
        };
        assert!(is_announcement(&created));
        let accepted = DataMessage {
            flags: Some(Flags::ProfileKeyUpdate as u32),
            ..created.clone()
        };
        assert!(!is_announcement(&accepted));
        let reacted = DataMessage {
            reaction: Some(Reaction::default()),
            ..created.clone()
        };
        assert!(!is_announcement(&reacted));
        let later = DataMessage {
            group_v2: Some(GroupContextV2 {
                revision: Some(1),
                ..created.group_v2.clone().unwrap()
            }),
            ..created
        };
        assert!(!is_announcement(&later));
    }
}
