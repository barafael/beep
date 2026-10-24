//! What the window's lists hold, made from the chat state; and those lists.

use super::*;

/// Gives the window the models that [`render`] keeps up to date: the window owns them, and
/// [`vec_model`] gets them back for updates.
pub(crate) fn set_up_models(window: &AppWindow) {
    window
        .global::<ChatList>()
        .set_conversations(ModelRc::new(VecModel::<ConversationItem>::default()));
    window
        .global::<OpenChat>()
        .set_messages(ModelRc::new(Rows::<MessageItem>::default()));
    window
        .global::<Panels>()
        .set_media_tiles(ModelRc::new(VecModel::<AttachmentItem>::default()));
    window
        .global::<Groups>()
        .set_group_candidates(ModelRc::new(VecModel::<CandidateItem>::default()));
    window
        .global::<Panels>()
        .set_story_rings(ModelRc::new(VecModel::<RingItem>::default()));
    window
        .global::<Panels>()
        .set_media_files(ModelRc::new(VecModel::<SharedItem>::default()));
    window
        .global::<Panels>()
        .set_media_links(ModelRc::new(VecModel::<SharedItem>::default()));
    window
        .global::<Groups>()
        .set_details_members(ModelRc::new(VecModel::<MemberItem>::default()));
    window
        .global::<Groups>()
        .set_details_requests(ModelRc::new(VecModel::<MemberItem>::default()));
    window
        .global::<Composer>()
        .set_staged(ModelRc::new(VecModel::<SharedString>::default()));
    window
        .global::<Panels>()
        .set_search_results(ModelRc::new(VecModel::<SearchResult>::default()));
    window
        .global::<Panels>()
        .set_forward_targets(ModelRc::new(VecModel::<ConversationItem>::default()));
    window
        .global::<ChatList>()
        .set_folders(ModelRc::new(VecModel::<SharedString>::default()));
    window
        .global::<ChatList>()
        .set_folder_unread(ModelRc::new(VecModel::<i32>::default()));
    window
        .global::<Composer>()
        .set_mention_candidates(ModelRc::new(VecModel::<SharedString>::default()));
    window
        .global::<OpenChat>()
        .set_activity(ModelRc::new(VecModel::<ActivityItem>::default()));
    window
        .global::<Composer>()
        .set_sticker_rows(ModelRc::new(VecModel::<StickerRow>::default()));
}

/// Empties a model, unless it is: emptying an empty one still resets its rows.
pub(super) fn clear<T: Clone + 'static>(model: &ModelRc<T>) {
    let model = vec_model(model);
    if model.row_count() > 0 {
        model.set_vec(Vec::new());
    }
}

/// The `VecModel` behind a model set up in [`set_up_models`].
pub(super) fn vec_model<T: Clone + 'static>(model: &ModelRc<T>) -> &VecModel<T> {
    model
        .as_any()
        .downcast_ref()
        .expect("models are VecModels, set up in create")
}

/// The open chat's messages, set up in [`set_up_models`].
pub(super) fn message_rows(model: &ModelRc<MessageItem>) -> &Rows<MessageItem> {
    model
        .as_any()
        .downcast_ref()
        .expect("the messages are Rows, set up in create")
}

/// `list_time` formats the time of the last message; see [`state::list_time_formatter`].
pub(super) fn conversation_item(
    view: &ConversationView,
    pinned: usize,
    chat: &ChatState,
    images: &Images,
    list_time: &dyn Fn(u64) -> String,
) -> ConversationItem {
    let c = view.conversation;
    ConversationItem {
        id: c.id.0.as_str().into(),
        title: c.title.as_str().into(),
        initials: state::initials(&c.title).into(),
        avatar_color: avatar_color(&c.id),
        avatar: view
            .avatar
            .map(|avatar| images.avatar(&c.id, avatar))
            .unwrap_or_default(),
        preview: view.preview().into(),
        preview_kind: preview_kind(view.preview_kind()),
        time: view
            .last_message
            .map(|m| list_time(m.timestamp))
            .unwrap_or_default()
            .into(),
        subtitle: c.subtitle.as_str().into(),
        unread: view.unread as i32,
        pinned: view.pinned(),
        request: chat.is_request(c),
        blocked: c.settings.blocked,
        left: c.left,
        invited: c.invited,
        requested: c.requested,
        admin: c.admin,
        safety_changed: c.trust == b33p_core::Trust::Changed,
        group: c.is_group(),
        can_move_up: c.settings.pinned.is_some_and(|at| at > 0),
        can_move_down: c
            .settings
            .pinned
            .is_some_and(|at| (at as usize) + 1 < pinned),
        muted: view.muted(),
        marked_unread: view.marked_unread(),
        archived: c.settings.archived,
        typing: state::typing_label(&view.typing).into(),
        timer: c.expire_timer as i32,
        timer_label: b33p_core::format_timer(c.expire_timer).into(),
    }
}

/// A member of the open group, or someone asking to join it.
pub(super) fn member_item(person: &b33p_core::Person, admin: bool) -> MemberItem {
    MemberItem {
        id: person.service_id.as_str().into(),
        name: person.name.as_str().into(),
        initials: state::initials(&person.name).into(),
        color: avatar_color(&ConversationId::contact(&person.service_id)),
        admin,
    }
}

/// A chat's picture as the window shows it; empty without one.
pub(super) fn avatar_image(chat: &ChatState, images: &Images, id: &ConversationId) -> slint::Image {
    chat.avatar(id)
        .map(|avatar| images.avatar(id, avatar))
        .unwrap_or_default()
}

/// A stable colour per person or group, for avatars without a picture and group senders' names.
pub(super) fn avatar_color(id: &ConversationId) -> Color {
    // Muted hues in the spirit of Signal's avatar palette; all readable with white text.
    const PALETTE: [u32; 8] = [
        0x3a76f0, 0x7d4fc9, 0xc4314b, 0xd36c1c, 0x2b8a60, 0x1e8a9e, 0x9a5b3c, 0x5c6bc0,
    ];
    Color::from_argb_encoded(0xff00_0000 | PALETTE[state::avatar_index(id, PALETTE.len())])
}

/// `in_group`: the open chat is a group, so the sender is named.
pub(super) fn message_item(
    view: &MessageView,
    in_group: bool,
    chat: &ChatState,
    images: &Images,
) -> MessageItem {
    let m = view.message;
    let attachments: Vec<_> = m
        .attachments
        .iter()
        .map(|a| attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images))
        .collect();
    let in_group = in_group && !m.outgoing;
    let sender = ConversationId::contact(&m.sender_id);
    MessageItem {
        key: format!("{}:{}", m.timestamp, m.sender_id).into(),
        sender: m.sender.as_str().into(),
        body: images.text(view.body, view.styles, view.revealed),
        selected: view.selected,
        has_body: !view.body.is_empty(),
        time: state::format_clock(m.timestamp).into(),
        outgoing: m.outgoing,
        show_sender: view.show_sender,
        day: view.day.as_deref().unwrap_or_default().into(),
        first: view.first_of_run,
        last: view.last_of_run,
        avatar_column: in_group,
        sender_avatar: view
            .sender_avatar
            .map(|picture| images.avatar(&sender, picture))
            .unwrap_or_default(),
        sender_initials: state::initials(&m.sender).into(),
        sender_color: avatar_color(&sender),
        quote_author: m.quote.as_ref().map_or("", |q| q.author.as_str()).into(),
        quote_text: m.quote.as_ref().map_or("", |q| q.text.as_str()).into(),
        attachments: ModelRc::new(VecModel::from(attachments)),
        reactions: ModelRc::new(VecModel::from(reaction_items(&m.reactions))),
        text: if m.deleted { "" } else { view.body }.into(),
        delivery: match (m.outgoing, m.unsent, m.delivery) {
            (false, ..) => DeliveryState::Incoming,
            (true, Some(Unsent::Sending), _) => DeliveryState::Sending,
            (true, Some(Unsent::Failed | Unsent::Refused), _) => DeliveryState::Failed,
            (true, None, Delivery::Sent) => DeliveryState::Sent,
            (true, None, Delivery::Delivered) => DeliveryState::Delivered,
            (true, None, Delivery::Read) => DeliveryState::Read,
        },
        edited: m.edited_at.is_some(),
        deleted: m.deleted,
        notice: m.notice,
        can_send_again: m.unsent == Some(Unsent::Failed),
        timer_notice: m.notice && m.timer_change.is_some(),
        expires: m
            .expires_at
            .map(|at| state::format_remaining(at, now_millis()))
            .unwrap_or_default()
            .into(),
        can_edit: chat.can_edit(m),
        first_unread: view.first_unread,
        can_forward: ChatState::can_forward(m),
        preview_url: m.preview.as_ref().map_or("", |p| p.url.as_str()).into(),
        preview_title: m.preview.as_ref().map_or("", |p| p.title.as_str()).into(),
        preview_description: m
            .preview
            .as_ref()
            .map_or("", |p| p.description.as_str())
            .into(),
        preview_site: m
            .preview
            .as_ref()
            .and_then(|p| p.url.strip_prefix("https://")?.split('/').next())
            .unwrap_or_default()
            .into(),
        has_preview_image: m.preview.as_ref().is_some_and(|p| p.image.is_some()),
        preview_image: m
            .preview
            .as_ref()
            .and_then(|p| p.image.as_ref())
            .map(|a| {
                let item = attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images);
                // A preview's picture stays small.
                let scale = (150. / item.height.max(1.)).min(1.);
                AttachmentItem {
                    can_save: false,
                    width: item.width * scale,
                    height: item.height * scale,
                    ..item
                }
            })
            .unwrap_or_default(),
        highlighted: view.highlighted,
        fresh: false,
    }
}

/// What kind of attachment it is, in words.
/// The icon before a chat's preview.
pub(super) fn preview_kind(kind: Option<state::PreviewKind>) -> PreviewKind {
    match kind {
        None => PreviewKind::Words,
        Some(state::PreviewKind::Timer) => PreviewKind::Timer,
        Some(state::PreviewKind::Attachment(kind)) => match kind {
            AttachmentKind::Image | AttachmentKind::ViewOnce => PreviewKind::Photo,
            AttachmentKind::Sticker => PreviewKind::Sticker,
            AttachmentKind::Video => PreviewKind::Video,
            AttachmentKind::Audio => PreviewKind::Audio,
            AttachmentKind::VoiceNote => PreviewKind::Voice,
            AttachmentKind::File | AttachmentKind::LongText => PreviewKind::File,
        },
    }
}

/// One chip per emoji, in the order they first came, counting everyone who chose it.
pub(super) fn reaction_items(reactions: &[b33p_core::Reaction]) -> Vec<ReactionItem> {
    let mut items: Vec<ReactionItem> = Vec::new();
    for reaction in reactions {
        match items
            .iter_mut()
            .find(|item| item.emoji.as_str() == reaction.emoji)
        {
            Some(item) => {
                item.count += 1;
                item.mine |= reaction.outgoing;
            }
            None => items.push(ReactionItem {
                emoji: reaction.emoji.as_str().into(),
                count: 1,
                mine: reaction.outgoing,
            }),
        }
    }
    items
}

/// Once new rows had time to fade in, they are like the others: shown again (scrolled back to,
/// or redrawn), they don't fade in again.
pub(super) fn settle_fresh(window: &AppWindow) {
    let window = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
        let Some(window) = window.upgrade() else {
            return;
        };
        let messages = window.global::<OpenChat>().get_messages();
        // The new ones are at the end.
        for row in (0..messages.row_count()).rev() {
            match messages.row_data(row) {
                Some(item) if item.fresh => messages.set_row_data(
                    row,
                    MessageItem {
                        fresh: false,
                        ..item
                    },
                ),
                _ => break,
            }
        }
    });
}

/// Like `==`, but comparing the nested lists by content: each item gets new models, which would
/// never compare equal and so redraw every row with an attachment or a reaction.
pub(super) fn same_message(a: &MessageItem, b: &MessageItem) -> bool {
    fn same<T: PartialEq>(a: &ModelRc<T>, b: &ModelRc<T>) -> bool {
        a.row_count() == b.row_count() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
    }
    same(&a.attachments, &b.attachments)
        && same(&a.reactions, &b.reactions)
        && MessageItem {
            attachments: b.attachments.clone(),
            reactions: b.reactions.clone(),
            ..a.clone()
        } == *b
}

pub(super) fn attachment_item(
    attachment: &Attachment,
    download: Option<&AttachmentState>,
    save: Option<&SaveState>,
    images: &Images,
) -> AttachmentItem {
    let kind = attachment.kind;
    let inline = matches!(
        kind,
        AttachmentKind::Image | AttachmentKind::Sticker | AttachmentKind::Video
    );
    let picture = match download {
        Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => Some(picture),
        _ => None,
    };
    // Fit into the bubble, keeping the shape the sender reported (or the picture has).
    let (max, fallback) = if kind == AttachmentKind::Sticker {
        (128.0, (128.0, 128.0))
    } else {
        (280.0, (240.0, 180.0))
    };
    let (w, h) = match picture {
        Some(p) => (p.width as f32, p.height as f32),
        None if attachment.width > 0 && attachment.height > 0 => {
            (attachment.width as f32, attachment.height as f32)
        }
        None => fallback,
    };
    let scale = (max / w.max(h)).min(1.0);
    let size = state::format_size(attachment.size);
    let label = match (kind, &attachment.file_name) {
        (AttachmentKind::ViewOnce, _) => "View-once media: open it on your phone".to_string(),
        (AttachmentKind::File, Some(name)) => format!("{name} · {size}"),
        _ => format!("{} · {size}", state::describe_kind(kind)),
    };
    let failed = match download {
        Some(AttachmentState::Failed(error)) => Some(error.clone()),
        _ => None,
    };
    // Saving says how it went; otherwise a failed download says why (and to click to retry,
    // when that may work).
    let status = match save {
        Some(SaveState::Saving) => "Saving…".into(),
        Some(SaveState::Saved(path)) => format!(
            "Saved to {}",
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("disk")
        ),
        Some(SaveState::Failed(error)) => format!("Couldn't save: {error}"),
        None => match &failed {
            Some(error) if error.retryable() => format!("{error} Click to retry."),
            Some(error) => error.to_string(),
            None => String::new(),
        },
    };
    AttachmentItem {
        id: attachment.id.0.as_str().into(),
        inline,
        hidden: kind == AttachmentKind::LongText,
        fetch: download.is_none() && state::shown_inline(kind),
        picture: picture
            .map(|p| {
                images
                    .attachments
                    .get(&attachment.id, p, crate::picture_image)
            })
            .unwrap_or_default(),
        width: w * scale,
        height: h * scale,
        label: label.into(),
        status: status.into(),
        can_retry: failed.as_ref().is_some_and(b33p_core::Error::retryable)
            && state::shown_inline(kind),
        can_save: !matches!(
            kind,
            AttachmentKind::ViewOnce | AttachmentKind::LongText | AttachmentKind::Sticker
        ),
        placeholder: attachment
            .blurhash
            .as_deref()
            .map(|hash| images.blur(hash, w as u32, h as u32))
            .unwrap_or_default(),
        caption: attachment.caption.as_deref().unwrap_or_default().into(),
        video: kind == AttachmentKind::Video,
        audio: matches!(kind, AttachmentKind::Audio | AttachmentKind::VoiceNote),
        gif: attachment.gif,
    }
}
