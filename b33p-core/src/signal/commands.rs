//! What each command from the UI does in a linked session.

use super::errors::{db, failed, send_failed, store_failed};
use crate::outbox::Sends as _;
use crate::{Error, ErrorKind, scrub::scrubbed};
use std::rc::Rc;

use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::{
    libsignal_service::{
        content::{ContentBody, DataMessage},
        protocol::ServiceId,
    },
    proto::{
        EditMessage, SyncMessage,
        data_message::{Delete, Flags},
        sync_message::{Content as SyncContent, Sent},
    },
    store::{ContentsStore, Thread},
};
use tracing::{debug, error, warn};

use crate::{
    AttachmentKind, ChatMessage, Command, ConversationId, Event, Events, Reaction, apply_reaction,
    attachments, format, now_millis, scrub::scrub,
};

use super::{
    BackgroundKind, Ctx, Mgr,
    avatars::{send_contact_avatar, send_group_avatar},
    conversations::{
        Names, conversation_id, details, display_name, ensure_conversation, find_number, or_logged,
        own_id, thread_of,
    },
    groups::{
        PHONE_NUMBER_ONLY, check_request, edit_kind, fetch_as_admin, group_edit, member_aci,
        member_of, only_admin_check, parse_group_link, refresh_group, show_stored,
    },
    history::{
        checkpoint, data_of, decorate, erase_deleted, history_page, is_outgoing, media, original,
        search, start_timers, to_chat_message, to_reaction,
    },
    send::{
        Outgoing, aci_binary, body_ranges, group_context, link_preview, respond, send_blocked,
        send_data, send_edit, send_new, send_read, send_sync_at, send_typing, sticker_message,
        sticker_packs,
    },
    stories::{
        STORY_BACKGROUND, STORY_FOREGROUND, keep_story, send_stories, send_viewed, story_recipients,
    },
    trust::{refresh_trust, set_verified},
};

/// Our messages that hadn't gone out when b33p last stopped ([`LocalStore::keep_unsent`]), to
/// show as failed. One that went out after all, and is stored as ours, is forgotten instead:
/// b33p stopped between sending it and forgetting it.
///
/// [`LocalStore::keep_unsent`]: crate::local::LocalStore::keep_unsent
pub(super) async fn send_interrupted(manager: &Mgr, ctx: &Ctx) {
    let unsent = match ctx.tables.unsent().await {
        Ok(unsent) => unsent,
        Err(error) => {
            warn!(%error, "failed to read the messages that didn't go out");
            return;
        }
    };
    let mut interrupted = Vec::new();
    for send in unsent {
        let Command::Send {
            conversation,
            timestamp,
            ..
        } = &send
        else {
            continue;
        };
        let stored = match thread_of(conversation) {
            Some(thread) => manager
                .store()
                .message(&thread, *timestamp)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        if stored.is_some_and(|content| is_outgoing(manager, &content)) {
            if let Err(error) = ctx.tables.sent(conversation, *timestamp).await {
                warn!(%error, "failed to forget a message that went out");
            }
        } else {
            interrupted.push(send);
        }
    }
    if !interrupted.is_empty() {
        warn!(
            count = interrupted.len(),
            "messages didn't go out before b33p stopped"
        );
        ctx.events.send(Event::Interrupted(interrupted));
    }
}

/// Says why `command` failed: in the log, and to the user, who may try it again.
pub(super) fn command_failed(events: &Events, error: Error, command: Command) {
    match error.kind {
        ErrorKind::Storage => error!(error = %scrub(&error.message), "command failed"),
        ErrorKind::Unreachable | ErrorKind::Refused => {
            warn!(error = %scrub(&error.message), kind = ?error.kind, "command failed")
        }
    }
    events.send(Event::CommandFailed { error, command });
}

/// Whether `command` is taken in order (`in_order` in `run_linked`): it sends something to the
/// other side, where the order is seen. One task takes them one after another, so a message's
/// edit, reactions and deletion never overtake it, and accepting a request goes before the
/// reply; meanwhile everything else (typing, receipts, history, settings) keeps flowing,
/// however long an upload takes.
pub(super) fn outgoing(command: &Command) -> bool {
    matches!(
        command,
        Command::Send { .. }
            | Command::SendSticker { .. }
            | Command::Forward { .. }
            | Command::Edit { .. }
            // Deleting only here, too: a message deleted while still sending would otherwise
            // come back once it is sent and stored.
            | Command::Delete { .. }
            | Command::React { .. }
            | Command::Respond { .. }
            | Command::SetTimer { .. }
    )
}

/// The message `original` names (its conversation and timestamp), as stored: what only it
/// has of what is forwarded, a sticker's pack and a link's preview. `None` if it isn't stored.
async fn forwarded_original(
    manager: &Mgr,
    ctx: &Ctx,
    original: &(ConversationId, u64),
) -> Option<presage::proto::DataMessage> {
    let thread = ctx.threads.borrow().get(&original.0).cloned()?;
    let stored = manager
        .store()
        .message(&thread, original.1)
        .await
        .ok()
        .flatten()?;
    data_of(&stored).map(|(data, _)| data.clone())
}

pub(super) async fn handle(
    manager: &mut Mgr,
    ctx: &Ctx,
    downloads: &Rc<attachments::Downloads>,
    command: Command,
) {
    // Kept to try again, should it fail.
    let retry = command.clone();
    // Chat settings the phone keeps too: write them there, if that is wanted.
    let to_storage = matches!(
        command,
        Command::SetPins(_)
            | Command::Mute { .. }
            | Command::Archive { .. }
            | Command::MarkUnread(_)
            | Command::Block { .. }
            | Command::Respond { .. }
            | Command::Preferences(_)
    );
    // Sending may meet a new key of a verified contact (and verifying is about keys).
    let touches_keys = matches!(
        command,
        Command::Send { .. }
            | Command::SendStory { .. }
            | Command::SendSticker { .. }
            | Command::Forward { .. }
            | Command::Edit { .. }
            | Command::Delete { .. }
            | Command::React { .. }
            | Command::Respond { .. }
            | Command::CreateGroup { .. }
            | Command::EditGroup { .. }
            | Command::JoinGroup(_)
            | Command::SetTimer { .. }
            | Command::SetVerified { .. }
    );
    let result = handle_command(manager, ctx, downloads, command).await;
    if to_storage && ctx.preferences.get().sync_to_phone {
        ctx.storage_changed.notify_one();
    }
    if touches_keys {
        refresh_trust(manager, ctx).await;
    }
    if let Err(error) = result {
        command_failed(&ctx.events, error, retry);
    }
}

async fn handle_command(
    manager: &mut Mgr,
    ctx: &Ctx,
    downloads: &Rc<attachments::Downloads>,
    command: Command,
) -> Result<(), Error> {
    let events = &ctx.events;
    let thread = |id: &ConversationId| {
        ctx.threads
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::refused("unknown conversation"))
    };
    match command {
        // In the background, so commands keep flowing while files download.
        Command::FetchAttachment(attachment) => {
            tokio::task::spawn_local(attachments::fetch(
                downloads.clone(),
                attachment,
                events.clone(),
            ));
        }
        Command::SaveAttachment(attachment) => {
            tokio::task::spawn_local(attachments::save(
                downloads.clone(),
                attachment,
                events.clone(),
            ));
        }
        // Already unlocked, and nothing failed.
        Command::Unlock(_) | Command::Retry | Command::StartOver | Command::UsePassword => {}
        // Taken care of by `run_linked`.
        Command::Unlink | Command::SetDeviceName(_) => {}
        // Only meaningful during the transfer; see `link_with_history`.
        Command::SkipHistory => {}
        Command::LoadHistory {
            conversation: id,
            before,
        } => {
            let Ok(thread) = thread(&id) else {
                return Ok(());
            };
            let (contents, older) = history_page(manager, &thread, before).await?;
            let mut names = Names::new();
            let mut messages = Vec::new();
            let mut reactions = Vec::new();
            // Oldest first, so reactions find their message and later ones win.
            for content in contents.iter().rev() {
                if let Some((target, reaction, remove)) =
                    to_reaction(manager, &mut names, content).await
                {
                    let target = original(ctx, &id, target).await;
                    if messages.iter().any(|m: &ChatMessage| m.is(&target)) {
                        apply_reaction(&mut messages, &target, reaction, remove);
                    } else {
                        reactions.push((target, reaction, remove));
                    }
                } else if let Some(mut message) =
                    to_chat_message(manager, &mut names, id.clone(), content).await
                {
                    // Stores from before edits took their message's place keep them apart,
                    // after it: the newest version stands for the message.
                    let reference = message.reference();
                    match messages.iter_mut().find(|m| m.is(&reference)) {
                        Some(earlier) => {
                            message.reactions = std::mem::take(&mut earlier.reactions);
                            *earlier = message;
                        }
                        None => messages.push(message),
                    }
                }
            }
            decorate(ctx, &id, &mut messages).await;
            // The store keeps nothing of messages deleted for everyone; those of this page.
            let own = own_id(manager).service_id_string();
            let page = older.unwrap_or(0)..before.unwrap_or(u64::MAX);
            let deleted = or_logged(ctx.tables.deleted(&id, page).await, "deleted messages");
            for target in deleted {
                let outgoing = target.author_id == own;
                let sender = match ServiceId::parse_from_service_id_string(&target.author_id) {
                    Some(author) => display_name(manager, &mut names, author).await,
                    None => String::new(),
                };
                messages.push(ChatMessage {
                    conversation: id.clone(),
                    sender_id: target.author_id,
                    sender,
                    timestamp: target.timestamp,
                    outgoing,
                    deleted: true,
                    ..Default::default()
                });
            }
            events.send(Event::History {
                conversation: id,
                messages,
                reactions,
                older,
            });
        }
        Command::React {
            conversation,
            target,
            emoji,
            remove,
        } => {
            let thread = thread(&conversation)?;
            let message = DataMessage {
                reaction: Some(presage::proto::data_message::Reaction {
                    emoji: Some(emoji.clone()),
                    remove: Some(remove),
                    target_author_aci: Some(target.author_id.clone()),
                    target_author_aci_binary: aci_binary(&target.author_id),
                    target_sent_timestamp: Some(target.timestamp),
                }),
                ..Default::default()
            };
            send_data(manager, ctx, &thread, message, now_millis()).await?;
            let reaction = Reaction {
                emoji,
                sender_id: own_id(manager).service_id_string(),
                sender: "You".into(),
                outgoing: true,
            };
            events.send(Event::Reaction {
                conversation,
                target,
                reaction,
                remove,
            });
        }
        Command::OpenAttachment(attachment) => {
            tokio::task::spawn_local(attachments::open(
                downloads.clone(),
                attachment,
                events.clone(),
            ));
        }
        Command::Send {
            conversation,
            timestamp,
            body,
            quote,
            files,
            styles,
            mentions,
        } => {
            let sent = async {
                let pointers = attachments::upload(manager, &files).await?;
                let preview = if ctx.preferences.get().link_previews {
                    link_preview(manager, &body).await
                } else {
                    None
                };
                let outgoing = Outgoing {
                    body,
                    styles,
                    mentions,
                    quote,
                    pointers,
                    preview,
                    sticker: None,
                };
                send_new(manager, ctx, conversation.clone(), outgoing, timestamp).await
            };
            if let Err(error) = sent.await {
                ctx.events.send(Event::SendFailed {
                    conversation,
                    timestamp,
                    retryable: error.retryable(),
                });
                return Err(error);
            }
            ctx.tables
                .sent(&conversation, timestamp)
                .await
                .map_err(db)?;
        }
        Command::DropUnsent {
            conversation,
            timestamp,
        } => ctx
            .tables
            .sent(&conversation, timestamp)
            .await
            .map_err(db)?,
        Command::ViewStory(story) => {
            ctx.tables.story_viewed(&story).await.map_err(db)?;
            send_stories(manager, ctx).await;
            let receipts = ctx.preferences.get().read_receipts;
            tokio::task::spawn_local(send_viewed(
                manager.clone(),
                ctx.outbox.clone(),
                story,
                receipts,
            ));
        }
        Command::SendStory {
            conversation,
            story,
        } => {
            let Thread::Group(key) = thread(&conversation)? else {
                return Err(Error::refused("stories go to groups here"));
            };
            let timestamp = now_millis();
            let mut stored = crate::local::StoredStory {
                author: own_id(manager).service_id_string(),
                timestamp,
                conversation: conversation.clone(),
                viewed: true,
                outgoing: true,
                ..Default::default()
            };
            let attachment = match story {
                crate::NewStory::Text(text) => {
                    stored.text = text.clone();
                    stored.foreground = STORY_FOREGROUND;
                    stored.background = STORY_BACKGROUND;
                    presage::proto::story_message::Attachment::TextAttachment(
                        presage::proto::TextAttachment {
                            text: Some(text),
                            text_style: Some(
                                presage::proto::text_attachment::Style::Regular as i32,
                            ),
                            text_foreground_color: Some(STORY_FOREGROUND),
                            background: Some(presage::proto::text_attachment::Background::Color(
                                STORY_BACKGROUND,
                            )),
                            ..Default::default()
                        },
                    )
                }
                crate::NewStory::Picture(path) => {
                    let pointer = attachments::upload(manager, &[path])
                        .await?
                        .pop()
                        .ok_or_else(|| Error::unreachable("nothing uploaded"))?;
                    stored.pointer = Some(pointer.encode_to_vec());
                    presage::proto::story_message::Attachment::FileAttachment(pointer)
                }
            };
            let message = presage::proto::StoryMessage {
                group: Some(group_context(manager, &key).await),
                attachment: Some(attachment),
                allows_replies: Some(true),
                ..Default::default()
            };
            ctx.outbox
                .group_message(key, ContentBody::StoryMessage(message.clone()), timestamp)
                .await
                .map_err(send_failed("failed to post the story"))?;
            keep_story(ctx, &stored).await.map_err(db)?;
            send_stories(manager, ctx).await;
            // Sending tells our other devices of data messages only; this one they hear of here,
            // at the story's time: Desktop files a sent story under its envelope's.
            let sync = SyncMessage {
                content: Some(SyncContent::Sent(Sent {
                    timestamp: Some(timestamp),
                    story_message: Some(message),
                    story_message_recipients: story_recipients(manager, key).await,
                    ..Default::default()
                })),
                ..Default::default()
            };
            send_sync_at(manager, &ctx.outbox, sync, timestamp).await;
        }
        Command::CreateGroup { title, members } => {
            let members = members
                .iter()
                .map(|id| member_aci(id))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| Error::refused(PHONE_NUMBER_ONLY))?;
            let (key, timestamp) = ctx
                .outbox
                .create_group(title.trim().to_owned(), members)
                .await
                .map_err(send_failed("failed to create the group"))?;
            let thread = Thread::Group(key);
            // Ours, so not a request: accepted before it is first shown, and said so should it
            // be shown already (a chat listed again keeps the settings it had).
            let settings = ctx
                .tables
                .update(&conversation_id(&thread), |s| s.accepted = true)
                .await
                .map_err(db)?;
            let id = ensure_conversation(manager, ctx, thread.clone(), 0).await;
            events.send(Event::Settings {
                conversation: id.clone(),
                settings,
            });
            refresh_group(manager, ctx, key).await;
            show_stored(manager, ctx, &id, &thread, timestamp).await;
            events.send(Event::GroupCreated(id));
        }
        Command::EditGroup { conversation, edit } => {
            let Thread::Group(key) = thread(&conversation)? else {
                return Err(Error::refused("not a group"));
            };
            if edit == crate::GroupEdit::CancelJoinRequest {
                manager
                    .cancel_join_request(&key)
                    .await
                    .map_err(failed("failed to take the request back"))?;
                debug!("took a request to join back");
                refresh_group(manager, ctx, key).await;
                return Ok(());
            }
            let edit = group_edit(edit).await?;
            if matches!(edit, presage::manager::GroupEdit::Leave) {
                only_admin_check(manager, key).await?;
            }
            let picture = matches!(edit, presage::manager::GroupEdit::Avatar(_));
            let kind = edit_kind(&edit);
            debug!(kind, "changing a group");
            let timestamp = ctx
                .outbox
                .change_group(key, edit)
                .await
                .map_err(send_failed("failed to change the group"))?;
            debug!(kind, timestamp, "changed a group, and sent the change");
            refresh_group(manager, ctx, key).await;
            if picture {
                send_group_avatar(manager.clone(), ctx, key).await;
            }
            show_stored(manager, ctx, &conversation, &Thread::Group(key), timestamp).await;
        }
        Command::PreviewGroupLink(link) => {
            let parsed = parse_group_link(&link)?;
            // A group we are in already just opens.
            if let Some(id) = member_of(manager, ctx, parsed.master_key).await {
                events.send(Event::GroupJoined(id));
                return Ok(());
            }
            let info = manager
                .group_join_info(&parsed)
                .await
                .map_err(failed("couldn't look the group up"))?;
            use presage::libsignal_service::groups_v2::AccessRequired;
            let approval = match info.add_from_invite_link {
                AccessRequired::Any => false,
                AccessRequired::Administrator => true,
                _ => return Err(Error::refused("This group's invite link is turned off.")),
            };
            events.send(Event::GroupPreview(crate::GroupPreview {
                link: link.trim().to_owned(),
                title: info.title,
                members: info.member_count,
                approval,
                requested: info.pending_admin_approval,
            }));
        }
        Command::JoinGroup(link) => {
            let parsed = parse_group_link(&link)?;
            let key = parsed.master_key;
            if let Some(id) = member_of(manager, ctx, key).await {
                events.send(Event::GroupJoined(id));
                return Ok(());
            }
            let joined = ctx
                .outbox
                .join_group(parsed)
                .await
                .map_err(send_failed("couldn't join the group"))?;
            let thread = Thread::Group(key);
            let id = ensure_conversation(manager, ctx, thread.clone(), 0).await;
            // Our doing, so not a request; said so, as the chat may be listed already (a chat
            // listed again keeps the settings it had).
            let settings = ctx
                .tables
                .update(&id, |s| s.accepted = true)
                .await
                .map_err(db)?;
            events.send(Event::Settings {
                conversation: id.clone(),
                settings,
            });
            refresh_group(manager, ctx, key).await;
            // Our own change, which doesn't come back to us: the picture isn't sent with it.
            send_group_avatar(manager.clone(), ctx, key).await;
            if let presage::manager::GroupJoin::Joined { timestamp } = joined {
                show_stored(manager, ctx, &id, &thread, timestamp).await;
            }
            events.send(Event::GroupJoined(id));
        }
        Command::LoadStickers => {
            let (manager, events) = (manager.clone(), events.clone());
            ctx.background.run(BackgroundKind::Stickers, async move {
                let packs = sticker_packs(&manager).await;
                events.send(Event::StickerPacks(packs));
            });
        }
        Command::SendSticker {
            conversation,
            pack,
            sticker,
        } => {
            let sticker = sticker_message(manager, &pack, sticker).await?;
            let outgoing = Outgoing {
                body: String::new(),
                styles: Vec::new(),
                mentions: Vec::new(),
                quote: None,
                pointers: Vec::new(),
                preview: None,
                sticker: Some(sticker),
            };
            send_new(manager, ctx, conversation, outgoing, now_millis()).await?;
        }
        Command::Block {
            conversation,
            blocked,
        } => {
            ctx.tables
                .update(&conversation, |s| s.blocked = blocked)
                .await
                .map_err(db)?;
            send_blocked(manager, ctx).await;
        }
        Command::Respond {
            conversation,
            response,
        } => {
            let thread = thread(&conversation)?;
            respond(manager, ctx, &conversation, &thread, response).await?;
        }
        Command::SetVerified {
            conversation,
            verified,
        } => set_verified(manager, ctx, &conversation, verified).await?,
        Command::FindNumber { number, then } => {
            let conversation = find_number(manager, ctx, &number).await?;
            events.send(Event::Found { conversation, then });
        }
        Command::LoadDetails(conversation) => {
            let Ok(thread) = thread(&conversation) else {
                return Ok(());
            };
            if let Thread::Group(key) = thread {
                check_request(manager, ctx, key).await;
            }
            let stored = details(manager, &ctx.tables, &thread).await;
            events.send(Event::Details {
                conversation: conversation.clone(),
                details: stored,
            });
            // A contact's profile again from the server, then their details and picture: a new
            // picture or about doesn't change their profile key, which presage caches it by.
            if let Thread::Contact(ServiceId::Aci(aci)) = thread
                && let Ok(Some(key)) = manager.store().profile_key(&ServiceId::Aci(aci)).await
            {
                match manager.refresh_profile(aci, key).await {
                    Ok(_) => {
                        let details = details(manager, &ctx.tables, &thread).await;
                        events.send(Event::Details {
                            conversation: conversation.clone(),
                            details,
                        });
                        send_contact_avatar(manager.clone(), ctx, aci).await;
                    }
                    Err(error) => debug!(error = %scrubbed(&error), "couldn't refresh a profile"),
                }
            }
            // The stored group shows at once; an admin's again once fetched, with the requests
            // to join that came meanwhile.
            if let Thread::Group(key) = thread
                && fetch_as_admin(manager, ctx, key).await
            {
                let details = details(manager, &ctx.tables, &thread).await;
                events.send(Event::Details {
                    conversation,
                    details,
                });
            }
        }
        Command::LoadMedia(conversation) => {
            let Ok(thread) = thread(&conversation) else {
                return Ok(());
            };
            let (manager, events) = (manager.clone(), events.clone());
            ctx.background.run(BackgroundKind::Media, async move {
                match media(&manager, &thread, &conversation).await {
                    Ok(messages) => {
                        events.send(Event::Media {
                            conversation,
                            messages,
                        });
                    }
                    Err(error) => command_failed(&events, error, Command::LoadMedia(conversation)),
                }
            });
        }
        // Each pause in typing searches again: the last search is the one that counts.
        Command::SearchChat {
            conversation,
            query,
        } => {
            let Ok(thread) = thread(&conversation) else {
                return Ok(());
            };
            let (manager, events) = (manager.clone(), events.clone());
            ctx.background.run(BackgroundKind::Search, async move {
                match search(&manager, &thread, &conversation, &query).await {
                    Ok(messages) => {
                        events.send(Event::SearchResults {
                            conversation,
                            query,
                            messages,
                        });
                    }
                    Err(error) => {
                        let retry = Command::SearchChat {
                            conversation,
                            query,
                        };
                        command_failed(&events, error, retry);
                    }
                }
            });
        }
        // The attachments are already on the CDN; their pointers work for anyone.
        Command::Forward {
            conversation,
            body,
            styles,
            attachments,
            original,
        } => {
            // As Signal's apps forward (Signal-Desktop's `maybeForwardMessages`): a sticker as
            // the sticker it is, not as a picture, and a link with its preview, while the text
            // still has the link. Both are in the stored message only.
            let stored = forwarded_original(manager, ctx, &original).await;
            let sticker = stored
                .as_ref()
                .and_then(|data| data.sticker.clone())
                .filter(|_| {
                    attachments
                        .iter()
                        .any(|a| a.kind == AttachmentKind::Sticker)
                });
            let links = format::links(&body);
            let preview = stored
                .iter()
                .flat_map(|data| data.preview.iter())
                .find(|preview| preview.url.as_ref().is_some_and(|url| links.contains(url)))
                .map(|preview| {
                    let shown = crate::LinkPreview {
                        url: preview.url.clone().unwrap_or_default(),
                        title: preview.title.clone().unwrap_or_default(),
                        description: preview.description.clone().unwrap_or_default(),
                        image: preview.image.as_ref().map(attachments::describe_preview),
                    };
                    (preview.clone(), shown)
                });
            let pointers = attachments
                .iter()
                .filter(|a| sticker.is_none() || a.kind != AttachmentKind::Sticker)
                .filter_map(attachments::pointer)
                .collect();
            let outgoing = Outgoing {
                body,
                styles,
                mentions: Vec::new(),
                quote: None,
                pointers,
                preview,
                sticker,
            };
            send_new(manager, ctx, conversation, outgoing, now_millis()).await?;
        }
        Command::Edit {
            conversation,
            target,
            body,
            styles,
            mentions,
        } => {
            let thread = thread(&conversation)?;
            // An edit replaces the whole message, so all but the text comes from the message
            // as sent: its attachments, quote, sticker and timer stay (receivers show what
            // the edit carries, and nothing else).
            let stored = manager
                .store()
                .message(&thread, target.timestamp)
                .await
                .ok()
                .flatten();
            let original = stored
                .as_ref()
                .and_then(data_of)
                .map(|(data, _)| data.clone())
                .unwrap_or_default();
            // Kept as the earlier version.
            let previous = original.body.clone().unwrap_or_default();
            let (text, ranges) = format::encode(&body, &styles, &mentions);
            let timestamp = now_millis();
            let mut data = DataMessage {
                body: (!text.is_empty()).then_some(text),
                body_ranges: body_ranges(ranges),
                timestamp: Some(timestamp),
                ..original
            };
            // The preview follows the text: gone with its link, made anew for a new one.
            let links = format::links(&body);
            data.preview
                .retain(|preview| preview.url.as_ref().is_some_and(|url| links.contains(url)));
            if data.preview.is_empty()
                && ctx.preferences.get().link_previews
                && let Some((sent, _)) = link_preview(manager, &body).await
            {
                data.preview.push(sent);
            }
            send_edit(
                manager,
                ctx,
                &thread,
                target.timestamp,
                data.clone(),
                timestamp,
            )
            .await?;
            // For after a restart; the state keeps the text before by itself.
            ctx.tables
                .record_edit(&conversation, &target, timestamp, &previous)
                .await
                .map_err(db)?;
            ctx.tables
                .sent_again(&conversation, target.timestamp)
                .await
                .map_err(db)?;
            // Shown as it will be read back: the edit in the message's place.
            let edited = stored.map(|mut content| {
                content.body = ContentBody::EditMessage(EditMessage {
                    target_sent_timestamp: Some(target.timestamp),
                    data_message: Some(data),
                });
                content
            });
            let message = match edited {
                Some(content) => {
                    to_chat_message(manager, &mut Names::new(), conversation.clone(), &content)
                        .await
                }
                None => None,
            };
            let message = message.unwrap_or_else(|| ChatMessage {
                conversation: conversation.clone(),
                sender_id: target.author_id.clone(),
                sender: "You".into(),
                body,
                timestamp: target.timestamp,
                outgoing: true,
                styles,
                edited_at: Some(timestamp),
                ..Default::default()
            });
            events.send(Event::Edited {
                conversation,
                target,
                message,
            });
        }
        Command::Delete {
            conversation,
            target,
            for_everyone: true,
        } => {
            let thread = thread(&conversation)?;
            let message = DataMessage {
                delete: Some(Delete {
                    target_sent_timestamp: Some(target.timestamp),
                }),
                ..Default::default()
            };
            send_data(manager, ctx, &thread, message, now_millis()).await?;
            erase_deleted(manager, ctx, &conversation, &thread, &target).await?;
        }
        Command::Delete {
            conversation,
            target,
            for_everyone: false,
        } => {
            let thread = thread(&conversation)?;
            let mut store = manager.store().clone();
            store
                .delete_message(&thread, target.timestamp)
                .await
                .map_err(store_failed("failed to delete"))?;
            ctx.tables
                .forget(&[(conversation, target)])
                .await
                .map_err(db)?;
            checkpoint(ctx).await;
        }
        Command::Typing {
            conversation,
            typing,
        } => {
            if !ctx.preferences.get().typing_indicators {
                return Ok(());
            }
            let thread = thread(&conversation)?;
            send_typing(&ctx.outbox, thread, typing).await;
        }
        Command::MarkRead {
            conversation,
            messages,
            expiring,
        } => {
            let newest = messages
                .iter()
                .map(|m| m.timestamp)
                .max()
                .unwrap_or_default();
            ctx.tables
                .mark_read(&conversation, newest)
                .await
                .map_err(db)?;
            if messages.is_empty() {
                return Ok(());
            }
            start_timers(ctx, &conversation, &expiring, now_millis()).await;
            tokio::task::spawn_local(send_read(
                manager.clone(),
                ctx.clone(),
                conversation,
                messages,
            ));
        }
        Command::MarkUnread(conversation) => {
            ctx.tables
                .update(&conversation, |s| s.marked_unread = true)
                .await
                .map_err(db)?;
        }
        Command::SetPins(order) => ctx.tables.set_pins(&order).await.map_err(db)?,
        Command::Mute {
            conversation,
            until,
        } => {
            ctx.tables
                .update(&conversation, |s| s.muted_until = until)
                .await
                .map_err(db)?;
        }
        Command::Archive {
            conversation,
            archived,
        } => {
            ctx.tables
                .update(&conversation, |s| s.set_archived(archived))
                .await
                .map_err(db)?;
        }
        Command::SetTimer {
            conversation,
            seconds,
        } => {
            let thread = thread(&conversation)?;
            if let Thread::Group(key) = thread {
                let edit = presage::manager::GroupEdit::Timer(seconds);
                let timestamp = ctx
                    .outbox
                    .change_group(key, edit)
                    .await
                    .map_err(send_failed("failed to change the group"))?;
                refresh_group(manager, ctx, key).await;
                show_stored(manager, ctx, &conversation, &thread, timestamp).await;
                return Ok(());
            }
            let timestamp = now_millis();
            let message = DataMessage {
                flags: Some(Flags::ExpirationTimerUpdate as u32),
                expire_timer: Some(seconds),
                ..Default::default()
            };
            send_data(manager, ctx, &thread, message, timestamp).await?;
            events.send(Event::Message(ChatMessage {
                conversation,
                sender_id: own_id(manager).service_id_string(),
                sender: "You".into(),
                body: crate::timer_notice("You", seconds),
                notice: true,
                timestamp,
                outgoing: true,
                expire_timer: (seconds > 0).then_some(seconds),
                timer_change: Some(seconds),
                ..Default::default()
            }));
        }
        Command::Preferences(preferences) => ctx.preferences.set(preferences),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageRef;

    #[test]
    fn what_the_other_side_sees_is_taken_in_order() {
        let conversation = ConversationId("c".into());
        let target = MessageRef {
            author_id: "a".into(),
            timestamp: 1,
        };
        let delete = |for_everyone| Command::Delete {
            conversation: conversation.clone(),
            target: target.clone(),
            for_everyone,
        };
        assert!(outgoing(&delete(true)));
        assert!(outgoing(&delete(false)));
        assert!(outgoing(&Command::React {
            conversation: conversation.clone(),
            target: target.clone(),
            emoji: "👍".into(),
            remove: false,
        }));
        // These must not wait behind an upload.
        assert!(!outgoing(&Command::Typing {
            conversation: conversation.clone(),
            typing: true,
        }));
        assert!(!outgoing(&Command::MarkRead {
            conversation: conversation.clone(),
            messages: vec![target],
            expiring: Vec::new(),
        }));
        assert!(!outgoing(&Command::LoadHistory {
            conversation,
            before: None,
        }));
    }
}
