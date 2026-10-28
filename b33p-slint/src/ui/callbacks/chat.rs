//! The chat list and the open chat: picking a chat, its settings, the composer, the
//! messages and their attachments.

use super::*;

/// Sends `text`, the composer's: during an edit that is the edit, which the state keeps out of
/// the chat's draft. The composer is then empty, or after an edit holds what was being written
/// before it. Returns whether a command went out.
pub(in crate::ui) fn send(shared: &Shared, window: &AppWindow, text: &str) -> bool {
    let sent = shared.act(|chat| chat.compose(text));
    if sent {
        window
            .global::<Composer>()
            .set_draft(shared.chat.borrow().draft().into());
        shared.refresh(Update::default());
    }
    sent
}

/// Puts the message at `index` in the composer, to be edited there.
pub(in crate::ui) fn edit(shared: &Shared, window: &AppWindow, index: usize) {
    let text = shared.chat.borrow_mut().edit(index);
    if let Some(text) = text {
        window.global::<Composer>().set_draft(text.into());
        shared.refresh(Update::default());
    }
}

pub(super) fn wire(window: &AppWindow, shared: &Rc<Shared>) {
    window.global::<ChatList>().on_select({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                let command = chat.select(&ConversationId(id.into()));
                let update = Update::reset();
                (command, update)
            });
        }
    });
    window.global::<Composer>().on_send({
        let shared = shared.clone();
        let window = window.as_weak();
        move |text| {
            if let Some(window) = window.upgrade() {
                send(&shared, &window, &text);
            }
        }
    });
    // Typing changes the draft, and with it only what the composer offers: redrawing all of
    // the window for every key would be wasted.
    window.global::<Composer>().on_draft_edited({
        let shared = shared.clone();
        let window = window.as_weak();
        move |text| {
            let command = shared.chat.borrow_mut().draft_changed(&text);
            if let Some(command) = command {
                shared.commands.send(command);
            }
            if let Some(window) = window.upgrade() {
                render_composer(&window, &shared.chat.borrow());
            }
        }
    });
    window.global::<ChatList>().on_toggle_pin({
        let toggle = chat_action(shared, |chat, id, ()| chat.toggle_pin(id));
        move |id| toggle(id, ())
    });
    window
        .global::<ChatList>()
        .on_move_pin(chat_action(shared, |chat, id, by| chat.move_pin_by(id, by)));
    window
        .global::<ChatList>()
        .on_mark_read(chat_action(shared, |chat, id, read: bool| {
            chat.mark_unread(id, !read)
        }));
    window
        .global::<ChatList>()
        .on_mute(chat_action(shared, |chat, id, seconds: i32| {
            let until = match seconds {
                0 => None,
                ..0 => Some(u64::MAX),
                seconds => Some(now_millis() + seconds as u64 * 1000),
            };
            chat.mute(id, until)
        }));
    window
        .global::<ChatList>()
        .on_block(chat_action(shared, |chat, id, blocked| {
            chat.block(id, blocked)
        }));
    window.global::<OpenChat>().on_respond({
        let shared = shared.clone();
        move |answer| {
            use b33p_core::RequestResponse;
            let response = match answer {
                RequestAnswer::Accept => RequestResponse::Accept,
                RequestAnswer::Delete => RequestResponse::Delete,
                RequestAnswer::Block => RequestResponse::Block,
            };
            shared.act(|chat| {
                let id = chat.selected().map(|c| c.id.clone());
                (
                    id.and_then(|id| chat.respond(&id, response)),
                    Update::reset(),
                )
            });
        }
    });
    window
        .global::<ChatList>()
        .on_archive(chat_action(shared, |chat, id, archived| {
            chat.archive(id, archived)
        }));
    window
        .global::<ChatList>()
        .on_set_timer(chat_action(shared, |chat, id, seconds: i32| {
            chat.set_timer(id, u32::try_from(seconds).ok()?)
        }));
    window.global::<Composer>().on_pick_mention({
        let shared = shared.clone();
        let window = window.as_weak();
        move |index| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let draft = shared
                .chat
                .borrow_mut()
                .pick_mention(&window.global::<Composer>().get_draft(), index as usize);
            if let Some(draft) = draft {
                window.global::<Composer>().set_draft(draft.into());
                shared.refresh(Update::default());
            }
        }
    });
    window.global::<Composer>().on_splice_draft({
        let shared = shared.clone();
        let window = window.as_weak();
        move |text, from, to| {
            let Some(window) = window.upgrade() else {
                return 0;
            };
            let (draft, caret) = splice(&window.global::<Composer>().get_draft(), &text, from, to);
            window.global::<Composer>().set_draft(draft.as_str().into());
            shared.command(|chat| chat.draft_changed(&draft));
            caret as i32
        }
    });
    window.global::<OpenChat>().on_load_older({
        let shared = shared.clone();
        move || shared.command(|chat| chat.load_older())
    });
    window.global::<ChatList>().on_show_folder({
        let shared = shared.clone();
        move |folder| shared.redraw(|chat| chat.show_folder(usize::try_from(folder).ok()))
    });
    window.global::<ChatList>().on_show_archived({
        let shared = shared.clone();
        move |show| shared.redraw(|chat| chat.show_archived(show))
    });
    window.on_shortcut({
        let shared = shared.clone();
        let window = window.as_weak();
        move |key, control, alt, shift| {
            let Some(window) = window.upgrade() else {
                return false;
            };
            shortcut(&shared, &window, &key, control, alt, shift)
        }
    });
    window.global::<Composer>().on_edit_last({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let text = shared.chat.borrow_mut().edit_last();
            if let (Some(text), Some(window)) = (text, window.upgrade()) {
                window.global::<Composer>().set_draft(text.into());
                shared.refresh(Update::default());
            }
        }
    });
    window.global::<OpenChat>().on_edit({
        let shared = shared.clone();
        let window = window.as_weak();
        move |key| {
            let row = row(&shared.chat.borrow(), &key);
            if let (Some(row), Some(window)) = (row, window.upgrade()) {
                edit(&shared, &window, row);
            }
        }
    });
    window.global::<Composer>().on_cancel_edit({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            shared.edit(|chat| chat.cancel_edit());
            if let Some(window) = window.upgrade() {
                window
                    .global::<Composer>()
                    .set_draft(shared.chat.borrow().draft().into());
            }
        }
    });
    window.global::<OpenChat>().on_delete({
        let shared = shared.clone();
        move |key, everyone| {
            shared.act(|chat| match row(chat, &key) {
                Some(row) => chat.delete(row, everyone),
                None => (None, Update::default()),
            });
        }
    });
    window.global::<OpenChat>().on_reveal({
        let shared = shared.clone();
        move |key| shared.redraw(|chat| at_row(chat, &key, ChatState::reveal))
    });
    window.global::<OpenChat>().on_toggle_select({
        let shared = shared.clone();
        move |key| shared.redraw(|chat| at_row(chat, &key, ChatState::toggle_selected))
    });
    window.global::<OpenChat>().on_select_through({
        let shared = shared.clone();
        move |key| shared.redraw(|chat| at_row(chat, &key, ChatState::select_through))
    });
    window.global::<OpenChat>().on_selection_text({
        let shared = shared.clone();
        move || shared.chat.borrow().selected_text().into()
    });
    window.global::<OpenChat>().on_clear_selection({
        let shared = shared.clone();
        move || shared.redraw(|chat| chat.clear_selection())
    });
    window.global::<OpenChat>().on_forward_selection({
        let shared = shared.clone();
        move || {
            shared.edit(|chat| {
                chat.start_forward_selection();
            })
        }
    });
    window.global::<OpenChat>().on_delete_selection({
        let shared = shared.clone();
        move |everyone| {
            shared.act(|chat| chat.delete_selection(everyone));
        }
    });
    window.global::<OpenChat>().on_fetch_attachment({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                // Nothing to redraw unless a download starts (a row coming into view asks).
                let command = chat.fetch_attachment(&AttachmentId(id.into()));
                let rows = command.is_some();
                (
                    command,
                    Update {
                        rows,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.global::<OpenChat>().on_save_attachment({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                (
                    chat.save_attachment(&AttachmentId(id.into())),
                    Update {
                        rows: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.global::<OpenChat>().on_react({
        let shared = shared.clone();
        move |key, emoji| shared.command(|chat| chat.react(row(chat, &key)?, &emoji))
    });
    window.global::<OpenChat>().on_retry_send({
        let shared = shared.clone();
        move |key| {
            shared.act(|chat| {
                (
                    row(chat, &key).and_then(|row| chat.retry_send(row)),
                    Update {
                        rows: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.global::<OpenChat>().on_reply({
        let shared = shared.clone();
        move |key| shared.redraw(|chat| at_row(chat, &key, ChatState::reply_to))
    });
    window.global::<Composer>().on_cancel_reply({
        let shared = shared.clone();
        move || shared.edit(|chat| chat.cancel_reply())
    });
    window.global::<OpenChat>().on_open_attachment({
        let shared = shared.clone();
        move |id| shared.command(|chat| chat.open_attachment(&AttachmentId(id.into())))
    });
    window.global::<OpenChat>().on_open_calendar({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            render_calendar(&window, &shared.chat.borrow());
            window.global::<OpenChat>().set_calendar_open(true);
        }
    });
    window.global::<OpenChat>().on_open_link({
        let shared = shared.clone();
        move |link| {
            // A mention opens the chat with whom it mentions, if there is one.
            if let Some(service_id) = link.strip_prefix(crate::markdown::MENTION) {
                let id = ConversationId::contact(service_id);
                shared.act(|chat| (chat.select(&id), Update::reset()));
            // A group's invite link is for joining here, not for the browser.
            } else if link.starts_with("https://signal.group/#")
                || link.starts_with("sgnl://signal.group/#")
            {
                shared.command(|chat| chat.open_join(Some(&link)));
            } else {
                crate::open_link(&link);
            }
        }
    });
    window.global::<Composer>().on_attach({
        let window = window.as_weak();
        move || {
            // The files go to the chat they were picked for: not to another one opened meanwhile.
            let chat = window
                .upgrade()
                .map(|window| window.global::<ChatList>().get_selected_id());
            // The dialog blocks until the user is done, so it gets its own thread.
            let window = window.clone();
            std::thread::spawn(move || {
                let files = rfd::FileDialog::new()
                    .set_title("Attach files")
                    .pick_files()
                    .unwrap_or_default();
                let files: Vec<SharedString> = files
                    .iter()
                    .map(|f| f.to_string_lossy().as_ref().into())
                    .collect();
                // Fails only once b33p is quitting.
                window
                    .upgrade_in_event_loop(move |window| {
                        if Some(window.global::<ChatList>().get_selected_id()) == chat {
                            window
                                .global::<Composer>()
                                .invoke_stage_files(ModelRc::new(VecModel::from(files)));
                        }
                    })
                    .ok();
            });
        }
    });
    window.global::<Composer>().on_paste({
        let window = window.as_weak();
        move || {
            // Not here: the window may own what was copied, and has to be free to hand it over.
            let window = window.clone();
            std::thread::spawn(move || {
                let files: Option<Vec<SharedString>> = crate::clipboard::pasted().map(|files| {
                    files
                        .iter()
                        .map(|f| f.to_string_lossy().as_ref().into())
                        .collect()
                });
                // Fails only once b33p is quitting.
                window
                    .upgrade_in_event_loop(move |window| match files {
                        Some(files) => window
                            .global::<Composer>()
                            .invoke_stage_files(ModelRc::new(VecModel::from(files))),
                        None => window.global::<Composer>().set_paste_text(
                            window.global::<Composer>().get_paste_text().wrapping_add(1),
                        ),
                    })
                    .ok();
            });
        }
    });
    window.global::<Composer>().on_files_dropped({
        let shared = shared.clone();
        move |event| {
            let Ok(paths) = event.data.file_paths() else {
                return;
            };
            let files: Vec<_> = paths.map(std::path::Path::to_path_buf).collect();
            shared.edit(|chat| chat.stage_files(files));
        }
    });
    window.global::<Composer>().on_stage_files({
        let shared = shared.clone();
        move |files| {
            shared.edit(|chat| {
                chat.stage_files(files.iter().map(|f| std::path::PathBuf::from(f.as_str())))
            })
        }
    });
    window.global::<Composer>().on_unstage({
        let shared = shared.clone();
        move |index| shared.edit(|chat| chat.unstage(index as usize))
    });
}
