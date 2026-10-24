//! Bringing the window in line with the chat state.

use super::*;

/// Brings the window in line with `chat`, touching only what `update` says changed (plus the
/// cheap status properties).
pub(crate) fn render(
    window: &AppWindow,
    chat: &ChatState,
    images: &Images,
    update: Update,
    whole_list: bool,
) {
    if update.screen {
        match chat.screen() {
            state::Screen::Syncing(_) => window.global::<Account>().set_screen(Screen::Syncing),
            state::Screen::Password { new, error } => {
                window.global::<Account>().set_password_new(*new);
                window
                    .global::<Account>()
                    .set_password_error(error.as_deref().unwrap_or_default().into());
                window.global::<Account>().set_screen(Screen::Password);
            }
            state::Screen::Link(qr) => {
                window.global::<Account>().set_link_qr(qr_image(qr));
                window.global::<Account>().set_screen(Screen::Link);
            }
            state::Screen::Chats => window.global::<Account>().set_screen(Screen::Chats),
            state::Screen::Failed { failure, message } => {
                window
                    .global::<Account>()
                    .set_failed_message(message.into());
                window
                    .global::<Account>()
                    .set_can_retry(failure.can_retry());
                window
                    .global::<Account>()
                    .set_can_use_password(failure.can_use_password());
                window
                    .global::<Account>()
                    .set_failed_retrying(*failure == Failure::KeyringUnavailable);
                window.global::<Account>().set_screen(Screen::Failed);
            }
        }
        window
            .global::<Account>()
            .set_can_start_over(chat.can_start_over());
        window
            .global::<Account>()
            .set_account(chat.account().into());
    }
    if let Some(stage) = chat.sync_stage() {
        window
            .global::<Account>()
            .set_sync_status(sync_status(stage).into());
        window
            .global::<Account>()
            .set_history_skippable(stage == SyncStage::Transfer(HistoryTransfer::Waiting));
    }
    window.global::<Account>().set_can_start(chat.can_start());
    window
        .global::<OpenChat>()
        .set_can_load_older(chat.can_load_older());
    window
        .global::<Account>()
        .set_error(chat.error().unwrap_or_default().into());
    window
        .global::<Account>()
        .set_can_retry_error(chat.can_retry_error());
    window
        .global::<Account>()
        .set_offline(chat.connection() == Connection::Offline);
    window.global::<Account>().set_connection_status(
        match chat.connection() {
            Connection::Online => "",
            Connection::CatchingUp => "Catching up…",
            Connection::Offline => "Not connected · trying again",
        }
        .into(),
    );
    let staged: Vec<SharedString> = chat
        .staged()
        .iter()
        .map(|path| {
            path.file_name()
                .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
                .as_ref()
                .into()
        })
        .collect();
    update_list(&window.global::<Composer>().get_staged(), staged, |a, b| {
        a == b
    });
    render_media(window, chat, images);
    render_stickers(window, chat);
    render_details(window, chat);
    render_stories(window, chat, images);
    let search = chat.chat_search();
    window
        .global::<Panels>()
        .set_chat_search_open(search.is_some());
    window.global::<Panels>().set_chat_search_busy(
        search.is_some_and(|(query, results)| !query.is_empty() && results.is_none()),
    );
    let results: Vec<SearchResult> = search
        .and_then(|(query, results)| Some((query, results?)))
        .map(|(query, results)| {
            results
                .iter()
                .map(|m| {
                    let (text, found) = state::snippet(&m.body, query);
                    SearchResult {
                        sender: m.sender.as_str().into(),
                        when: state::format_time(m.timestamp).into(),
                        snippet: crate::highlighted(&text, found, window.get_accent()),
                        text: text.into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    update_list(
        &window.global::<Panels>().get_search_results(),
        results,
        |a, b| a == b,
    );
    window
        .global::<ChatList>()
        .set_search_is_number(state::looks_like_number(
            &window.global::<ChatList>().get_search_text(),
        ));
    window
        .global::<Groups>()
        .set_group_filter_is_number(state::looks_like_number(
            &window.global::<Groups>().get_group_draft_filter(),
        ));
    let draft = chat.group_draft();
    window
        .global::<Groups>()
        .set_group_draft_open(draft.is_some());
    if let Some((new, chosen)) = draft {
        window.global::<Groups>().set_group_draft_new(new);
        window
            .global::<Groups>()
            .set_group_draft_chosen(chosen.len() as i32);
        let candidates: Vec<CandidateItem> = chat
            .group_candidates(&window.global::<Groups>().get_group_draft_filter())
            .into_iter()
            .map(|c| CandidateItem {
                id: c.id.0.as_str().into(),
                title: c.title.as_str().into(),
                initials: state::initials(&c.title).into(),
                color: avatar_color(&c.id),
                avatar: avatar_image(chat, images, &c.id),
                chosen: chosen.contains(&c.id),
            })
            .collect();
        update_list(
            &window.global::<Groups>().get_group_candidates(),
            candidates,
            |a, b| a == b,
        );
    }
    let join = chat.join();
    // The field is the person's while the dialog is open: set only as it opens.
    let was_open = window.global::<Groups>().get_join_open();
    window.global::<Groups>().set_join_open(join.is_some());
    if let Some((link, preview, busy)) = join {
        if !was_open {
            window.global::<Groups>().set_join_link(link.into());
        }
        window.global::<Groups>().set_join_busy(busy);
        let preview = preview.cloned().unwrap_or_default();
        window
            .global::<Groups>()
            .set_join_title(preview.title.into());
        window
            .global::<Groups>()
            .set_join_members(preview.members as i32);
        window
            .global::<Groups>()
            .set_join_approval(preview.approval);
        window
            .global::<Groups>()
            .set_join_requested(preview.requested);
    }
    window.global::<Panels>().set_forwarding(chat.forwarding());
    window
        .global::<Panels>()
        .set_forward_preview(chat.forwarded().unwrap_or_default().into());
    if chat.forwarding() {
        let targets: Vec<_> = chat
            .forward_targets(&window.global::<Panels>().get_forward_filter())
            .into_iter()
            .map(|c| ConversationItem {
                id: c.id.0.as_str().into(),
                title: c.title.as_str().into(),
                initials: state::initials(&c.title).into(),
                avatar_color: avatar_color(&c.id),
                avatar: avatar_image(chat, images, &c.id),
                ..Default::default()
            })
            .collect();
        update_list(
            &window.global::<Panels>().get_forward_targets(),
            targets,
            |a, b| a == b,
        );
    }
    let (archived, archived_unread) = chat.archived();
    window
        .global::<ChatList>()
        .set_archived_count(archived as i32);
    window
        .global::<ChatList>()
        .set_archived_unread(archived_unread as i32);
    window
        .global::<ChatList>()
        .set_showing_archived(chat.showing_archived());
    let folders: Vec<SharedString> = chat
        .folders()
        .iter()
        .map(|f| f.name.as_str().into())
        .collect();
    let unread: Vec<i32> = (0..folders.len())
        .map(|at| chat.folder_unread(at) as i32)
        .collect();
    update_list(
        &window.global::<ChatList>().get_folders(),
        folders,
        |a, b| a == b,
    );
    update_list(
        &window.global::<ChatList>().get_folder_unread(),
        unread,
        |a, b| a == b,
    );
    window
        .global::<ChatList>()
        .set_folder(chat.folder().map_or(-1, |at| at as i32));
    window.global::<OpenChat>().set_typing(
        chat.selected()
            .map(|c| state::typing_label(&chat.typing(&c.id)))
            .unwrap_or_default()
            .into(),
    );
    // An edit that ended without being sent (its message was deleted, here or elsewhere)
    // leaves the composer to the draft again, so its text can't be sent as a new message.
    let editing = chat.editing().is_some();
    if window.global::<Composer>().get_editing() && !editing {
        window.global::<Composer>().set_draft(chat.draft().into());
    }
    window.global::<Composer>().set_editing(editing);
    window
        .global::<OpenChat>()
        .set_selected_count(chat.selection_len() as i32);

    window
        .global::<OpenChat>()
        .set_can_forward_selection(chat.can_forward_selection());
    window
        .global::<OpenChat>()
        .set_can_delete_selection_for_everyone(chat.can_delete_selection_for_everyone());
    let replying = chat.replying();
    window
        .global::<Composer>()
        .set_reply_author(replying.map_or("", |q| q.author.as_str()).into());
    window
        .global::<Composer>()
        .set_reply_text(replying.map_or("", |q| q.text.as_str()).into());

    let messages = window.global::<OpenChat>().get_messages();
    let messages = message_rows(&messages);
    let in_group = chat.selected().is_some_and(|c| c.is_group());
    match update.messages {
        MessagesUpdate::Unchanged => {}
        MessagesUpdate::Appended(n) => {
            // The one before them may no longer end its run (its time and ticks go).
            if let Some(before) = messages.row_count().checked_sub(1)
                && let Some(view) = chat.message_view(before)
            {
                let item = message_item(&view, in_group, chat, images);
                if !messages
                    .row_data(before)
                    .is_some_and(|old| same_message(&old, &item))
                {
                    messages.set_row_data(before, item);
                }
            }
            messages.extend(chat.last_messages(n).map(|message| MessageItem {
                fresh: true,
                ..message_item(&message, in_group, chat, images)
            }));
            window.global::<OpenChat>().set_appended_total(
                window
                    .global::<OpenChat>()
                    .get_appended_total()
                    .wrapping_add(n as i32),
            );
            settle_fresh(window);
        }
        MessagesUpdate::Prepended(n) => {
            // In one go; the list keeps what is on screen in place.
            let mut items: Vec<_> = chat
                .messages()
                .take(n + 1)
                .map(|m| message_item(&m, in_group, chat, images))
                .collect();
            // The first of those already shown: its day label or run may have changed.
            let first_shown = (items.len() > n).then(|| items.pop()).flatten();
            messages.prepend(items);
            if let Some(item) = first_shown {
                redraw_message(messages, n, item);
            }
        }
        MessagesUpdate::Reset => messages.set_vec(
            chat.messages()
                .map(|m| message_item(&m, in_group, chat, images))
                .collect::<Vec<_>>(),
        ),
    }
    if update.messages != MessagesUpdate::Unchanged {
        render_activity(window, chat);
        if window.global::<OpenChat>().get_calendar_open() {
            render_calendar(window, chat);
        }
    }
    // Messages changed in place (downloads, reactions): redraw only rows that differ, and of
    // those only the changed messages' when the state names them. Taken at every redraw so
    // that they don't pile up; named without `update.rows`, they are redrawn all the same.
    let changed = chat.take_changed_rows();
    if update.messages != MessagesUpdate::Reset {
        match changed {
            None if update.rows => {
                for (row, message) in chat.messages().enumerate() {
                    let item = message_item(&message, in_group, chat, images);
                    redraw_message(messages, row, item);
                }
            }
            None => {}
            Some(changed) => {
                for row in rows_around(chat, &changed) {
                    if let Some(message) = chat.message_view(row) {
                        let item = message_item(&message, in_group, chat, images);
                        redraw_message(messages, row, item);
                    }
                }
            }
        }
    }

    // After the rows are there.
    if update.scroll_to_end {
        window.global::<OpenChat>().set_scroll_to_end_serial(
            window
                .global::<OpenChat>()
                .get_scroll_to_end_serial()
                .wrapping_add(1),
        );
    }
    if let Some((row, count)) = chat.jump()
        && count as i32 != window.global::<OpenChat>().get_jump_serial()
    {
        window.global::<OpenChat>().set_jump_row(row as i32);
        window.global::<OpenChat>().set_jump_serial(count as i32);
    }

    if update.conversations {
        let pinned = chat.pinned().len();
        let list_time = state::list_time_formatter();
        let changed = chat.take_changed_chats().filter(|_| !whole_list);
        let conversations = window.global::<ChatList>().get_conversations();
        let conversations = vec_model(&conversations);
        let order: Vec<&b33p_core::ConversationId> = chat.listed_ids().collect();
        match changed {
            // Only these chats changed: their rows, and those that moved. Nothing else of the
            // other rows can differ, so they aren't even made.
            Some(changed) if conversations.row_count() == order.len() => {
                for (row, id) in order.into_iter().enumerate() {
                    let moved = conversations
                        .row_data(row)
                        .is_none_or(|shown| shown.id.as_str() != id.0);
                    if !moved && !changed.contains(id) {
                        continue;
                    }
                    if let Some(view) = chat.conversation_view(id) {
                        let item = conversation_item(&view, pinned, chat, images, &list_time);
                        if conversations.row_data(row).as_ref() != Some(&item) {
                            conversations.set_row_data(row, item);
                        }
                    }
                }
            }
            _ => {
                let items: Vec<_> = chat
                    .conversations()
                    .map(|c| conversation_item(&c, pinned, chat, images, &list_time))
                    .collect();
                // Update in place where possible so the list keeps its scroll position.
                if conversations.row_count() == items.len() {
                    for (row, item) in items.into_iter().enumerate() {
                        if conversations.row_data(row).as_ref() != Some(&item) {
                            conversations.set_row_data(row, item);
                        }
                    }
                } else {
                    conversations.set_vec(items);
                }
            }
        }
        window
            .global::<ChatList>()
            .set_selected(chat.selected_index().map_or(-1, |i| i as i32));
        let selected_id = chat.selected().map_or("", |c| c.id.0.as_str());
        if window.global::<ChatList>().get_selected_id() != selected_id {
            // Each chat has its own draft, however it was opened.
            window.global::<Composer>().set_draft(chat.draft().into());
        }
        window
            .global::<ChatList>()
            .set_selected_id(selected_id.into());
        window.global::<ChatList>().set_current(
            chat.selected_view()
                .map(|view| conversation_item(&view, pinned, chat, images, &list_time))
                .unwrap_or_default(),
        );
        if !chat.searching() {
            window
                .global::<ChatList>()
                .set_search_text(SharedString::new());
        }
    }
    // Last, once switching chats or ending an edit has put the right draft in the composer.
    render_composer(window, chat);
    images.forget_gone(chat);
}

/// The rows of these messages of the open chat and those beside them, whose runs may end or
/// start with theirs; in order, once each.
pub(super) fn rows_around(chat: &ChatState, changed: &[b33p_core::MessageRef]) -> Vec<usize> {
    let mut rows: Vec<usize> = changed
        .iter()
        .filter_map(|message| chat.row_of(message))
        .flat_map(|row| row.saturating_sub(1)..=row + 1)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// Puts `item` in `row`, unless the row shows the same already: setting it would draw it anew.
pub(super) fn redraw_message(messages: &Rows<MessageItem>, row: usize, item: MessageItem) {
    if !messages
        .row_data(row)
        .is_some_and(|old| same_message(&old, &item))
    {
        messages.set_row_data(row, item);
    }
}

/// What the composer offers for its draft: sending it, and people to mention.
pub(super) fn render_composer(window: &AppWindow, chat: &ChatState) {
    let draft = window.global::<Composer>().get_draft();
    window
        .global::<Composer>()
        .set_can_send(chat.can_compose(&draft));
    let candidates: Vec<SharedString> = chat
        .mention_candidates(&draft)
        .iter()
        .map(|p| p.name.as_str().into())
        .collect();
    update_list(
        &window.global::<Composer>().get_mention_candidates(),
        candidates,
        |a, b| a == b,
    );
}

/// Brings a list in line with `items`, touching only rows that differ, so it keeps its scroll
/// position. `same` compares rows (nested models compare by identity otherwise).
pub(super) fn update_list<T: Clone + 'static>(
    model: &ModelRc<T>,
    items: Vec<T>,
    same: impl Fn(&T, &T) -> bool,
) {
    let model = vec_model(model);
    if model.row_count() != items.len() {
        model.set_vec(items);
        return;
    }
    for (row, item) in items.into_iter().enumerate() {
        if !model.row_data(row).is_some_and(|old| same(&old, &item)) {
            model.set_row_data(row, item);
        }
    }
}

/// The sticker picker. Pictures are converted each time; the picker is small.
pub(super) fn render_stickers(window: &AppWindow, chat: &ChatState) {
    const PER_ROW: usize = 6;
    let packs = chat.sticker_packs();
    window
        .global::<Composer>()
        .set_stickers_busy(matches!(packs, Some(None)));
    let packs = packs.flatten().unwrap_or_default();
    let titles: Vec<SharedString> = packs.iter().map(|p| p.title.as_str().into()).collect();
    if window.global::<Composer>().get_sticker_packs().row_count() != titles.len() {
        window
            .global::<Composer>()
            .set_sticker_packs(ModelRc::new(VecModel::from(titles)));
    }
    let chosen = (window.global::<Composer>().get_sticker_pack().max(0) as usize)
        .min(packs.len().saturating_sub(1));
    let Some(pack) = packs.get(chosen) else {
        clear(&window.global::<Composer>().get_sticker_rows());
        return;
    };
    // Only when the pack changes, so the grid keeps its place otherwise.
    let shown = window.global::<Composer>().get_sticker_rows();
    let same_pack = shown
        .row_data(0)
        .and_then(|row| row.tiles.row_data(0))
        .is_some_and(|tile| tile.pack.as_str() == pack.id);
    if same_pack {
        return;
    }
    let rows: Vec<StickerRow> = pack
        .stickers
        .chunks(PER_ROW)
        .map(|row| StickerRow {
            tiles: ModelRc::new(VecModel::from(
                row.iter()
                    .map(|sticker| StickerTile {
                        pack: pack.id.as_str().into(),
                        id: sticker.id as i32,
                        picture: crate::picture_image(&sticker.picture),
                        emoji: sticker.emoji.as_str().into(),
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    window
        .global::<Composer>()
        .set_sticker_rows(ModelRc::new(VecModel::from(rows)));
}

/// The stories overlay.
pub(super) fn render_stories(window: &AppWindow, chat: &ChatState, images: &Images) {
    window
        .global::<Panels>()
        .set_unviewed_stories(chat.unviewed_stories() as i32);
    if !window.global::<Panels>().get_stories_open() {
        return;
    }
    let rings: Vec<RingItem> = chat
        .story_rings()
        .iter()
        .map(|ring| RingItem {
            id: ring.conversation.0.as_str().into(),
            title: ring.title.as_str().into(),
            initials: state::initials(&ring.title).into(),
            color: avatar_color(ring.conversation),
            avatar: avatar_image(chat, images, ring.conversation),
            unviewed: ring.unviewed,
            time: state::format_list_time(ring.latest).into(),
        })
        .collect();
    update_list(
        &window.global::<Panels>().get_story_rings(),
        rings,
        |a, b| a == b,
    );
    // Rebuilt when the groups or their names change; the choice stays with its group.
    let groups = chat.story_groups();
    let ids: Vec<ConversationId> = groups.iter().map(|c| c.id.clone()).collect();
    let titles: Vec<SharedString> = groups.iter().map(|c| c.title.as_str().into()).collect();
    let offered: Vec<SharedString> = window
        .global::<Panels>()
        .get_story_group_ids()
        .iter()
        .collect();
    if !offered
        .iter()
        .map(SharedString::as_str)
        .eq(ids.iter().map(|id| id.0.as_str()))
        || !window
            .global::<Panels>()
            .get_story_groups()
            .iter()
            .eq(titles.iter().cloned())
    {
        let chosen = offered.get(window.global::<Panels>().get_story_group().max(0) as usize);
        let at = chosen
            .and_then(|c| ids.iter().position(|id| id.0 == c.as_str()))
            .unwrap_or(0);
        let ids: Vec<SharedString> = ids.iter().map(|id| id.0.as_str().into()).collect();
        window
            .global::<Panels>()
            .set_story_groups(ModelRc::new(VecModel::from(titles)));
        window
            .global::<Panels>()
            .set_story_group_ids(ModelRc::new(VecModel::from(ids)));
        window.global::<Panels>().set_story_group(at as i32);
    }
    let story = match chat.current_story() {
        Some((story, at, count)) => {
            let mut item = StoryItem {
                shown: true,
                author: story.author.as_str().into(),
                time: state::format_time(story.timestamp).into(),
                position: format!("{} / {count}", at + 1).into(),
                ..Default::default()
            };
            match &story.content {
                b33p_core::StoryContent::Text {
                    text,
                    foreground,
                    background,
                } => {
                    item.text_story = true;
                    item.text = text.as_str().into();
                    item.foreground = Color::from_argb_encoded(*foreground | 0xff00_0000);
                    item.background = Color::from_argb_encoded(*background | 0xff00_0000);
                }
                b33p_core::StoryContent::Picture(picture) => {
                    let shown = attachment_item(
                        picture,
                        chat.attachment(&picture.id),
                        chat.saved(&picture.id),
                        images,
                    );
                    // Phone-shaped: as tall as the window allows.
                    let scale = (560. / shown.height.max(1.)).min(360. / shown.width.max(1.));
                    item.picture = AttachmentItem {
                        width: shown.width * scale,
                        height: shown.height * scale,
                        can_save: false,
                        ..shown
                    };
                }
            }
            item
        }
        None => StoryItem::default(),
    };
    window.global::<Panels>().set_story(story);
}

/// The details page.
pub(super) fn render_details(window: &AppWindow, chat: &ChatState) {
    let details = chat.details();
    window
        .global::<Groups>()
        .set_details_open(details.is_some());
    window
        .global::<Groups>()
        .set_details_busy(matches!(details, Some(None)));
    let details = details.flatten().cloned().unwrap_or_default();
    window
        .global::<Groups>()
        .set_details_about(details.about.unwrap_or_default().into());
    window
        .global::<Groups>()
        .set_details_phone(details.phone.unwrap_or_default().into());
    // Four groups of five digits per line, as on phones.
    let safety = details.safety_number.map(|number| {
        let groups: Vec<&str> = number.split_whitespace().collect();
        groups
            .chunks(4)
            .map(|line| line.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    });
    window
        .global::<Groups>()
        .set_details_safety_number(safety.unwrap_or_default().into());
    window
        .global::<Groups>()
        .set_details_verified(details.trust == b33p_core::Trust::Verified);
    window
        .global::<Groups>()
        .set_details_safety_changed(details.trust == b33p_core::Trust::Changed);
    window
        .global::<Groups>()
        .set_details_description(details.description.unwrap_or_default().into());
    window
        .global::<Groups>()
        .set_details_invite_link(details.invite_link.unwrap_or_default().into());
    window
        .global::<Groups>()
        .set_details_invite_access(match details.invite_link_access {
            b33p_core::InviteLinkAccess::Off => 0,
            b33p_core::InviteLinkAccess::Anyone => 1,
            b33p_core::InviteLinkAccess::Approval => 2,
        });
    let requests: Vec<MemberItem> = details
        .requests
        .iter()
        .map(|person| member_item(person, false))
        .collect();
    update_list(
        &window.global::<Groups>().get_details_requests(),
        requests,
        |a, b| a == b,
    );
    let members: Vec<MemberItem> = details
        .members
        .iter()
        .map(|(person, admin)| member_item(person, *admin))
        .collect();
    // Row by row: a new model would rebuild the rows, closing a member's menu under the
    // pointer whenever anything happens.
    update_list(
        &window.global::<Groups>().get_details_members(),
        members,
        |a, b| a == b,
    );
}

/// The media overview.
pub(super) fn render_media(window: &AppWindow, chat: &ChatState, images: &Images) {
    let media = chat.media();
    window.global::<Panels>().set_media_open(media.is_some());
    window
        .global::<Panels>()
        .set_media_busy(matches!(media, Some(None)));
    let Some(Some(overview)) = media else {
        // Nothing left over for the next chat's.
        clear(&window.global::<Panels>().get_media_tiles());
        clear(&window.global::<Panels>().get_media_files());
        clear(&window.global::<Panels>().get_media_links());
        return;
    };
    // The UI lays the tiles out by its width; their size here is beside the point.
    let tiles: Vec<AttachmentItem> = overview
        .media
        .iter()
        .map(|a| attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images))
        .collect();
    update_list(
        &window.global::<Panels>().get_media_tiles(),
        tiles,
        |a, b| a == b,
    );
    let files: Vec<SharedItem> = overview
        .files
        .iter()
        .map(|(m, a)| SharedItem {
            attachment: attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images),
            url: SharedString::new(),
            sender: m.sender.as_str().into(),
            when: state::format_time(m.timestamp).into(),
        })
        .collect();
    update_list(
        &window.global::<Panels>().get_media_files(),
        files,
        |a, b| a == b,
    );
    let links: Vec<SharedItem> = overview
        .links
        .iter()
        .map(|(m, url)| SharedItem {
            url: url.as_str().into(),
            sender: m.sender.as_str().into(),
            when: state::format_time(m.timestamp).into(),
            ..Default::default()
        })
        .collect();
    update_list(
        &window.global::<Panels>().get_media_links(),
        links,
        |a, b| a == b,
    );
}

/// The open chat's activity strip, or none while it is turned off.
pub(super) fn render_activity(window: &AppWindow, chat: &ChatState) {
    let items = if window.global::<Prefs>().get_show_activity() {
        activity_items(chat)
    } else {
        Vec::new()
    };
    update_list(
        &window.global::<OpenChat>().get_activity(),
        items,
        |a, b| a == b,
    );
}

/// Weeks or months (whichever fit) from top to bottom, long silences folded into a short gap.
pub(super) fn activity_items(chat: &ChatState) -> Vec<ActivityItem> {
    /// Silences of at least this many weeks or months are folded.
    const FOLD: usize = 6;
    /// A folded silence is drawn this many times as tall as a week or month.
    const QUIET_WEIGHT: f32 = 2.0;
    let span = chat.activity_span();
    let buckets = chat.activity(span);
    // Square roots, so a quiet week still shows beside a busy one.
    let busiest = buckets
        .iter()
        .map(|b| b.mine.max(b.others))
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let scale = |n: u32| (n as f32 / busiest).sqrt();
    let stretches = timeline::fold_quiet(buckets, span, FOLD);
    let weight = |s: &Stretch| {
        if matches!(s, Stretch::Quiet { .. }) {
            QUIET_WEIGHT
        } else {
            1.0
        }
    };
    let total: f32 = stretches.iter().map(weight).sum();
    let count = chat.messages().len();
    let mut y = 0.0;
    let mut year = None;
    stretches
        .iter()
        .enumerate()
        .map(|(i, stretch)| {
            let height = weight(stretch) / total;
            let end = stretches.get(i + 1).map_or(count, Stretch::first);
            let mut item = ActivityItem {
                y,
                height,
                first: stretch.first() as i32,
                end: end as i32,
                ..Default::default()
            };
            y += height;
            match stretch {
                Stretch::Active(bucket) => {
                    if year != Some(bucket.start.year()) {
                        year = Some(bucket.start.year());
                        item.year = bucket.start.year().to_string().into();
                    }
                    item.mine = scale(bucket.mine);
                    item.others = scale(bucket.others);
                    item.media = bucket.media > 0;
                    item.label = bucket.label(span).into();
                    item.summary = bucket.summary().into();
                }
                Stretch::Quiet { from, until, .. } => {
                    item.quiet = true;
                    item.label =
                        format!("{} – {}", from.strftime("%B %Y"), until.strftime("%B %Y")).into();
                    item.summary = Stretch::quiet_summary(*from, *until).into();
                }
            }
            item
        })
        .collect()
}

/// The open chat's calendar: a grid of days per year, newest year first.
pub(super) fn render_calendar(window: &AppWindow, chat: &ChatState) {
    let days = chat.activity(timeline::Span::Day);
    let busiest = days.iter().map(|d| d.messages()).max().unwrap_or(0).max(1) as f32;
    let mut summary = days
        .first()
        .map(|d| d.start.strftime("Since %B %Y · ").to_string())
        .unwrap_or_default();
    summary.push_str(&timeline::Bucket::total(&days).summary());
    let years: Vec<CalendarYear> = timeline::calendar(&days)
        .into_iter()
        .rev()
        .map(|year| {
            if year.total.messages() == 0 {
                return CalendarYear {
                    year: year.year.to_string().into(),
                    summary: "No messages".into(),
                    quiet: true,
                    ..Default::default()
                };
            }
            let cells: Vec<DayCell> = year
                .days
                .into_iter()
                .map(|day| {
                    let n = day.bucket.map_or(0, |b| b.messages());
                    DayCell {
                        column: day.column.into(),
                        row: day.row.into(),
                        level: if n == 0 {
                            0.0
                        } else {
                            (n as f32 / busiest).sqrt()
                        },
                        label: day.label.into(),
                        summary: day
                            .bucket
                            .map_or_else(|| "No messages".into(), |b| b.summary())
                            .into(),
                        first: day.bucket.map_or(0, |b| b.first as i32),
                        active: n > 0,
                        outside: day.bucket.is_none(),
                    }
                })
                .collect();
            let months: Vec<i32> = year.months.iter().map(|&c| c.into()).collect();
            CalendarYear {
                year: year.year.to_string().into(),
                summary: format!(
                    "{} · {}% from you",
                    year.total.summary(),
                    year.total.mine_percent()
                )
                .into(),
                quiet: false,
                days: ModelRc::new(VecModel::from(cells)),
                months: ModelRc::new(VecModel::from(months)),
            }
        })
        .collect();
    window
        .global::<OpenChat>()
        .set_calendar_summary(summary.into());
    window
        .global::<OpenChat>()
        .set_calendar(ModelRc::new(VecModel::from(years)));
}
