//! The window: what Slint generates from `ui/app.slint` (`AppWindow`, `MessageItem`, …), wired
//! to the chat state.

use std::rc::Rc;

use b33p_core::settings::{
    Animations, NotificationContent, Settings, TextSize, Theme, WidgetStyle,
};
use b33p_core::state::{
    self, AttachmentState, ChatState, ConversationView, MessageView, MessagesUpdate, SaveState,
    Update,
};
use b33p_core::timeline::{self, Stretch};
use b33p_core::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, Backend, ConversationId, Delivery,
    HistoryTransfer, Unsent, now_millis,
};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::generated::*;
use crate::{Images, Shared, qr_image};

/// Builds the window for `shared`, showing what `shared` holds. Not shown yet.
pub fn create(shared: &Rc<Shared>) -> Result<Rc<Window>, slint::PlatformError> {
    let window = AppWindow::new()?;
    if Backend::mock_requested() {
        window.set_window_title("b33p — MOCK (nothing is sent)".into());
    }
    show_settings(&window, &shared.settings.borrow(), &shared.style_note());
    window.set_device_name(shared.device_name.as_str().into());
    set_up_models(&window);

    window.window().on_close_requested({
        let shared = shared.clone();
        move || shared.close_requested()
    });
    window.on_settings_opened({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_tray_available(crate::tray::host_present());
            }
        }
    });
    window.on_start_with_system_changed({
        let window = window.as_weak();
        move |enabled| {
            if let Err(error) = crate::autostart::set(enabled) {
                tracing::warn!(%error, "failed to change starting with the system");
            }
            if let Some(window) = window.upgrade() {
                window.set_start_with_system(crate::autostart::enabled());
            }
        }
    });
    window.on_select({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                let command = chat.select(&ConversationId(id.into()));
                let update = Update::reset();
                (command, update)
            });
        }
    });
    window.on_send({
        let shared = shared.clone();
        let window = window.as_weak();
        move |text| {
            if shared.act(|chat| chat.compose(&text))
                && let Some(window) = window.upgrade()
            {
                // Empty once sent; after an edit, what was being written before it.
                window.set_draft(shared.chat.borrow().draft().into());
                shared.refresh(Update::default());
            }
        }
    });
    window.on_unlock({
        let shared = shared.clone();
        move |password, confirmation| {
            shared.act(|chat| {
                (
                    chat.unlock(&password, &confirmation),
                    Update {
                        screen: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.on_draft_edited({
        let shared = shared.clone();
        move |text| {
            shared.act(|chat| (chat.draft_changed(&text), Update::default()));
        }
    });
    window.on_toggle_pin({
        let toggle = chat_action(shared, |chat, id, ()| chat.toggle_pin(id));
        move |id| toggle(id, ())
    });
    window.on_move_pin(chat_action(shared, |chat, id, by| chat.move_pin_by(id, by)));
    window.on_mark_read(chat_action(shared, |chat, id, read: bool| {
        chat.mark_unread(id, !read)
    }));
    window.on_mute(chat_action(shared, |chat, id, seconds: i32| {
        let until = match seconds {
            0 => None,
            ..0 => Some(u64::MAX),
            seconds => Some(now_millis() + seconds as u64 * 1000),
        };
        chat.mute(id, until)
    }));
    window.on_block(chat_action(shared, |chat, id, blocked| {
        chat.block(id, blocked)
    }));
    window.on_respond({
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
    window.on_archive(chat_action(shared, |chat, id, archived| {
        chat.archive(id, archived)
    }));
    window.on_set_timer(chat_action(shared, |chat, id, seconds: i32| {
        chat.set_timer(id, u32::try_from(seconds).ok()?)
    }));
    let edit_selected = |shared: &Rc<Shared>, edit: fn(String, bool) -> b33p_core::GroupEdit| {
        let shared = shared.clone();
        move |text: SharedString, flag: bool| {
            shared.act(|chat| {
                let id = chat.selected().map(|c| c.id.clone());
                (
                    id.and_then(|id| chat.edit_group(&id, edit(text.to_string(), flag))),
                    Update::default(),
                )
            });
        }
    };
    window.on_answer_invitation({
        let shared = shared.clone();
        move |id, accept| {
            let edit = if accept {
                b33p_core::GroupEdit::AcceptInvitation
            } else {
                b33p_core::GroupEdit::DeclineInvitation
            };
            shared.act(|chat| {
                (
                    chat.edit_group(&ConversationId(id.into()), edit),
                    Update::default(),
                )
            });
        }
    });
    window.on_find_number({
        let shared = shared.clone();
        move |number, open| {
            let then = if open {
                b33p_core::AfterFinding::Open
            } else {
                b33p_core::AfterFinding::Choose
            };
            shared.act(|chat| (chat.find_number(&number, then), Update::default()));
        }
    });
    window.on_confirm_safety_number({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                (
                    chat.confirm_safety_number(&ConversationId(id.into())),
                    Update::default(),
                )
            });
        }
    });
    window.on_set_verified({
        let shared = shared.clone();
        move |verified| {
            shared.act(|chat| (chat.set_verified(verified), Update::default()));
        }
    });
    window.on_rename_group({
        let edit = edit_selected(shared, |text, _| b33p_core::GroupEdit::Title(text));
        move |text| edit(text, false)
    });
    window.on_describe_group({
        let edit = edit_selected(shared, |text, _| b33p_core::GroupEdit::Description(text));
        move |text| edit(text, false)
    });
    window.on_change_group_picture({
        let window = window.as_weak();
        move || {
            let window = window.clone();
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Choose the group's picture")
                    .add_filter("Pictures", &["jpg", "jpeg", "png", "webp", "gif"])
                    .pick_file();
                if let Some(path) = picked {
                    let path: SharedString = path.to_string_lossy().as_ref().into();
                    let _ = window.upgrade_in_event_loop(move |window| {
                        window.invoke_group_picture_chosen(path)
                    });
                }
            });
        }
    });
    window.on_group_picture_chosen({
        let edit = edit_selected(shared, |path, _| {
            b33p_core::GroupEdit::SetPicture(path.into())
        });
        move |path| edit(path, false)
    });
    window.on_remove_group_picture({
        let edit = edit_selected(shared, |_, _| b33p_core::GroupEdit::RemovePicture);
        move || edit(SharedString::new(), false)
    });
    window.on_set_member_admin(edit_selected(shared, b33p_core::GroupEdit::SetAdmin));
    window.on_remove_member({
        let edit = edit_selected(shared, |id, _| b33p_core::GroupEdit::RemoveMember(id));
        move |id| edit(id, false)
    });
    window.on_leave_group({
        let edit = edit_selected(shared, |_, _| b33p_core::GroupEdit::Leave);
        move || edit(SharedString::new(), false)
    });
    window.on_open_new_group({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.open_group_draft(None);
                (None, Update::default())
            });
        }
    });
    window.on_open_add_members({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                let id = chat.selected().map(|c| c.id.clone());
                chat.open_group_draft(id);
                (None, Update::default())
            });
        }
    });
    window.on_toggle_candidate({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                chat.toggle_group_member(&ConversationId(id.into()));
                (None, Update::default())
            });
        }
    });
    window.on_group_draft_edited({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.on_is_blank(|text| text.trim().is_empty());
    window.on_finish_group_draft({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let name = window.get_group_draft_name();
            shared.act(|chat| (chat.finish_group_draft(&name), Update::default()));
        }
    });
    window.on_close_group_draft({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.close_group_draft();
                (None, Update::default())
            });
        }
    });
    window.on_open_stories({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.on_open_story({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                (
                    chat.open_story(&ConversationId(id.into())),
                    Update::default(),
                )
            });
        }
    });
    window.on_step_story({
        let shared = shared.clone();
        move |by| {
            shared.act(|chat| (chat.step_story(by as isize), Update::default()));
        }
    });
    window.on_close_story({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.close_story();
                (None, Update::default())
            });
        }
    });
    window.on_post_story_text({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let text = window.get_story_text().to_string();
            let at = window.get_story_group().max(0) as usize;
            let group = window
                .get_story_group_ids()
                .row_data(at)
                .map(|id| ConversationId(id.into()));
            let sent = shared.act(|chat| {
                let command =
                    group.and_then(|g| chat.send_story(&g, b33p_core::NewStory::Text(text)));
                (command, Update::default())
            });
            if sent {
                window.set_story_text(SharedString::new());
            }
        }
    });
    window.on_post_story_photo({
        let window = window.as_weak();
        move || {
            let window = window.clone();
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new()
                    .set_title("Post a photo as a story")
                    .add_filter("Pictures", &["jpg", "jpeg", "png", "webp", "gif"])
                    .pick_file();
                if let Some(path) = picked {
                    let path: SharedString = path.to_string_lossy().as_ref().into();
                    let _ = window.upgrade_in_event_loop(move |window| {
                        window.invoke_story_photo_chosen(path)
                    });
                }
            });
        }
    });
    window.on_story_photo_chosen({
        let shared = shared.clone();
        let window = window.as_weak();
        move |path| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let at = window.get_story_group().max(0) as usize;
            let group = window
                .get_story_group_ids()
                .row_data(at)
                .map(|id| ConversationId(id.into()));
            shared.act(|chat| {
                let story = b33p_core::NewStory::Picture(std::path::PathBuf::from(path.as_str()));
                (
                    group.and_then(|g| chat.send_story(&g, story)),
                    Update::default(),
                )
            });
        }
    });
    window.on_open_details({
        let shared = shared.clone();
        move || {
            shared.act(|chat| (chat.open_details(), Update::default()));
        }
    });
    window.on_close_details({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.close_details();
                (None, Update::default())
            });
        }
    });
    window.on_open_stickers({
        let shared = shared.clone();
        move || {
            shared.act(|chat| (chat.load_stickers(), Update::default()));
        }
    });
    window.on_sticker_pack_chosen({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.on_send_sticker({
        let shared = shared.clone();
        move |pack, id| {
            shared.act(|chat| (chat.send_sticker(&pack, id as u32), Update::default()));
        }
    });
    window.on_open_media({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                window.set_media_tab(0);
            }
            shared.act(|chat| (chat.open_media(), Update::default()));
        }
    });
    window.on_close_media({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.close_media();
                (None, Update::default())
            });
        }
    });
    window.on_open_chat_search({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.open_chat_search();
                (None, Update::default())
            });
        }
    });
    window.on_close_chat_search({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.close_chat_search();
                (None, Update::default())
            });
        }
    });
    window.on_chat_search({
        let shared = shared.clone();
        move |text| {
            shared.act(|chat| (chat.search_chat(&text), Update::default()));
        }
    });
    window.on_show_result({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| (None, chat.show_result(index as usize)));
        }
    });
    window.on_forward({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| {
                chat.start_forward(index as usize);
                (None, Update::default())
            });
        }
    });
    window.on_forward_to({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                (
                    chat.forward_to(&ConversationId(id.into())),
                    Update::default(),
                )
            });
        }
    });
    window.on_forward_filter_edited({
        let shared = shared.clone();
        move || shared.refresh(Update::default())
    });
    window.on_cancel_forward({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.cancel_forward();
                (None, Update::default())
            });
        }
    });
    window.on_pick_mention({
        let shared = shared.clone();
        let window = window.as_weak();
        move |index| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let draft = shared
                .chat
                .borrow_mut()
                .pick_mention(&window.get_draft(), index as usize);
            if let Some(draft) = draft {
                window.set_draft(draft.into());
                shared.refresh(Update::default());
            }
        }
    });
    window.on_load_older({
        let shared = shared.clone();
        move || {
            shared.act(|chat| (chat.load_older(), Update::default()));
        }
    });
    window.on_show_folder({
        let shared = shared.clone();
        move |folder| {
            shared.act(|chat| (None, chat.show_folder(usize::try_from(folder).ok())));
        }
    });
    window.on_show_archived({
        let shared = shared.clone();
        move |show| {
            shared.act(|chat| (None, chat.show_archived(show)));
        }
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
    window.on_edit_last({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let text = shared.chat.borrow_mut().edit_last();
            if let (Some(text), Some(window)) = (text, window.upgrade()) {
                window.set_draft(text.into());
                shared.refresh(Update::default());
            }
        }
    });
    window.on_edit({
        let shared = shared.clone();
        let window = window.as_weak();
        move |index| {
            let text = shared.chat.borrow_mut().edit(index as usize);
            if let (Some(text), Some(window)) = (text, window.upgrade()) {
                window.set_draft(text.into());
                shared.refresh(Update::default());
            }
        }
    });
    window.on_cancel_edit({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            shared.act(|chat| {
                chat.cancel_edit();
                (None, Update::default())
            });
            if let Some(window) = window.upgrade() {
                window.set_draft(shared.chat.borrow().draft().into());
            }
        }
    });
    window.on_delete({
        let shared = shared.clone();
        move |index, everyone| {
            shared.act(|chat| chat.delete(index as usize, everyone));
        }
    });
    window.on_reveal({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| (None, chat.reveal(index as usize)));
        }
    });
    window.on_toggle_select({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| (None, chat.toggle_selected(index as usize)));
        }
    });
    window.on_select_through({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| (None, chat.select_through(index as usize)));
        }
    });
    window.on_clear_selection({
        let shared = shared.clone();
        move || {
            shared.act(|chat| (None, chat.clear_selection()));
        }
    });
    window.on_forward_selection({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.start_forward_selection();
                (None, Update::default())
            });
        }
    });
    window.on_delete_selection({
        let shared = shared.clone();
        move |everyone| {
            shared.act(|chat| chat.delete_selection(everyone));
        }
    });
    window.on_settings_changed({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let settings = read_settings(&window);
            window.set_font_scale(settings.text_size.scale());
            shared.apply_settings(settings);
            window.set_style_note(shared.style_note().into());
            render_activity(&window, &shared.chat.borrow());
        }
    });
    window.on_fetch_attachment({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                (
                    chat.fetch_attachment(&AttachmentId(id.into())),
                    Update {
                        rows: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    window.on_save_attachment({
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
    window.on_react({
        let shared = shared.clone();
        move |index, emoji| {
            shared.act(|chat| (chat.react(index as usize, &emoji), Update::default()));
        }
    });
    window.on_reply({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| (None, chat.reply_to(index as usize)));
        }
    });
    window.on_cancel_reply({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.cancel_reply();
                (None, Update::default())
            });
        }
    });
    window.on_open_attachment({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| {
                (
                    chat.open_attachment(&AttachmentId(id.into())),
                    Update::default(),
                )
            });
        }
    });
    window.on_open_calendar({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            render_calendar(&window, &shared.chat.borrow());
            window.set_calendar_open(true);
        }
    });
    window.on_open_link(|link| crate::open_link(&link));
    window.global::<Playback>().on_toggle({
        let shared = Rc::downgrade(shared);
        move |id| {
            if let Some(shared) = shared.upgrade() {
                shared.toggle_playback(AttachmentId(id.into()));
            }
        }
    });
    window.on_attach({
        let window = window.as_weak();
        move || {
            // The files go to the chat they were picked for: not to another one opened meanwhile.
            let chat = window.upgrade().map(|window| window.get_selected_id());
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
                let _ = window.upgrade_in_event_loop(move |window| {
                    if Some(window.get_selected_id()) == chat {
                        window.invoke_stage_files(ModelRc::new(VecModel::from(files)));
                    }
                });
            });
        }
    });
    window.on_paste({
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
                let _ = window.upgrade_in_event_loop(move |window| match files {
                    Some(files) => window.invoke_stage_files(ModelRc::new(VecModel::from(files))),
                    None => window.set_paste_text(window.get_paste_text().wrapping_add(1)),
                });
            });
        }
    });
    window.on_files_dropped({
        let shared = shared.clone();
        move |event| {
            let Ok(paths) = event.data.file_paths() else {
                return;
            };
            let files: Vec<_> = paths.map(std::path::Path::to_path_buf).collect();
            shared.act(|chat| {
                chat.stage_files(files);
                (None, Update::default())
            });
        }
    });
    window.on_stage_files({
        let shared = shared.clone();
        move |files| {
            shared.act(|chat| {
                chat.stage_files(files.iter().map(|f| std::path::PathBuf::from(f.as_str())));
                (None, Update::default())
            });
        }
    });
    window.on_unstage({
        let shared = shared.clone();
        move |index| {
            shared.act(|chat| {
                chat.unstage(index as usize);
                (None, Update::default())
            });
        }
    });
    window.on_back({
        let shared = shared.clone();
        move || {
            shared.act(|chat| (None, chat.deselect()));
        }
    });
    window.on_search({
        let shared = shared.clone();
        move |text| {
            shared.act(|chat| (None, chat.search(&text)));
        }
    });
    window.on_skip_history({
        let shared = shared.clone();
        move || {
            shared.act(|chat| (chat.skip_history(), Update::default()));
        }
    });
    window.on_dismiss_error({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                chat.dismiss_error();
                (None, Update::default())
            });
        }
    });

    Ok(Rc::new(Window(window)))
}

pub struct Window(AppWindow);

impl Window {
    /// Brings the window in line with `chat`, touching only what `update` says changed.
    pub fn render(&self, chat: &ChatState, images: &Images, update: Update) {
        render(&self.0, chat, images, update);
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.0.show()
    }

    pub fn hide(&self) {
        if let Err(error) = self.0.hide() {
            tracing::warn!(%error, "failed to close a window");
        }
    }

    /// Shows the window and brings it to the front, e.g. after a notification or the tray icon
    /// was clicked. On Wayland the compositor decides; it lets b33p to the front when given an
    /// activation token (see `Shared::activate`), and may only highlight it otherwise.
    pub fn raise(&self) {
        self.0.window().set_minimized(false);
        if let Err(error) = self.0.show() {
            tracing::warn!(%error, "failed to show the window");
        }
        // Not in Slint's public API. On Qt this raises and activates the window, which on
        // Wayland asks the compositor with the token in `XDG_ACTIVATION_TOKEN`, if there is one.
        let adapter = i_slint_core::window::WindowInner::from_pub(self.0.window()).window_adapter();
        if let Some(internal) = adapter.internal(i_slint_core::InternalToken)
            && let Err(error) = internal.bring_to_front()
        {
            tracing::warn!(%error, "failed to bring the window to the front");
        }
    }

    pub fn visible(&self) -> bool {
        self.0.window().is_visible()
    }

    /// Whether the window is the active one. Slint's public API doesn't offer this yet, but both
    /// the Qt and the winit backend keep it up to date internally.
    pub fn active(&self) -> bool {
        i_slint_core::window::WindowInner::from_pub(self.0.window()).active()
    }

    /// What plays: the attachment (empty for nothing), paused, progress 0…1, and the time.
    pub fn show_playback(&self, id: &str, paused: bool, progress: f32, time: &str) {
        let playback = self.0.global::<Playback>();
        playback.set_id(id.into());
        playback.set_paused(paused);
        playback.set_progress(progress);
        playback.set_time(time.into());
    }

    /// Whether changes animate (fades, slides) or happen at once.
    pub fn set_animations(&self, enabled: bool) {
        self.0
            .global::<Motion>()
            .set_scale(if enabled { 1.0 } else { 0.0 });
    }

    /// Puts `settings` on the window, as at start-up; the harness calls this after changing
    /// settings from outside the window.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn show_settings(&self, settings: &Settings, style_note: &str) {
        show_settings(&self.0, settings, style_note);
    }

    /// Puts `text` in the composer, as if typed; the harness calls this, since its keystrokes
    /// don't go through the text edit that would otherwise hold them.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn set_draft(&self, text: &str) {
        self.0.set_draft(text.into());
    }
}

/// The settings page shows choices by index, in the order of these enums.
fn show_settings(window: &AppWindow, settings: &Settings, style_note: &str) {
    // As build.rs chooses: Qt on Linux.
    window.set_qt(cfg!(target_os = "linux"));
    window.set_theme(match settings.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    let style = WidgetStyle::ALL
        .iter()
        .position(|s| *s == settings.style)
        .unwrap_or_default();
    window.set_widget_style(style as i32);
    window.set_style_note(style_note.into());
    window.set_text_size(match settings.text_size {
        TextSize::Small => 0,
        TextSize::Default => 1,
        TextSize::Large => 2,
    });
    let animations = Animations::ALL
        .iter()
        .position(|a| *a == settings.animations)
        .unwrap_or_default();
    window.set_animations(animations as i32);
    window.set_show_avatars(settings.show_avatars);
    window.set_notifications(settings.notifications);
    window.set_notification_content(match settings.notification_content {
        NotificationContent::NameAndMessage => 0,
        NotificationContent::NameOnly => 1,
        NotificationContent::Nothing => 2,
    });
    window.set_close_to_tray(settings.close_to_tray);
    window.set_read_receipts(settings.preferences.read_receipts);
    window.set_typing_indicators(settings.preferences.typing_indicators);
    window.set_link_previews(settings.preferences.link_previews);
    window.set_sync_to_phone(settings.preferences.sync_to_phone);
    window.set_autostart_supported(crate::autostart::SUPPORTED);
    window.set_start_with_system(crate::autostart::enabled());
    window.set_font_scale(settings.text_size.scale());
    window.set_show_activity(settings.show_activity);
    window.set_show_calendar(settings.show_calendar);
}

fn read_settings(window: &AppWindow) -> Settings {
    Settings {
        theme: match window.get_theme() {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        },
        style: WidgetStyle::ALL
            .get(window.get_widget_style() as usize)
            .copied()
            .unwrap_or_default(),
        animations: Animations::ALL
            .get(window.get_animations() as usize)
            .copied()
            .unwrap_or_default(),
        text_size: match window.get_text_size() {
            0 => TextSize::Small,
            2 => TextSize::Large,
            _ => TextSize::Default,
        },
        show_avatars: window.get_show_avatars(),
        notifications: window.get_notifications(),
        notification_content: match window.get_notification_content() {
            1 => NotificationContent::NameOnly,
            2 => NotificationContent::Nothing,
            _ => NotificationContent::NameAndMessage,
        },
        show_activity: window.get_show_activity(),
        show_calendar: window.get_show_calendar(),
        close_to_tray: window.get_close_to_tray(),
        preferences: b33p_core::Preferences {
            read_receipts: window.get_read_receipts(),
            typing_indicators: window.get_typing_indicators(),
            link_previews: window.get_link_previews(),
            sync_to_phone: window.get_sync_to_phone(),
        },
    }
}

/// Carries out the shortcut for a key pressed anywhere in the window; whether it was one.
fn shortcut(
    shared: &Rc<Shared>,
    window: &AppWindow,
    key: &str,
    control: bool,
    alt: bool,
    shift: bool,
) -> bool {
    use slint::platform::Key;
    let is = |k: Key| key.chars().eq(std::iter::once(char::from(k)));
    let open = |f: &dyn Fn(&mut ChatState) -> Option<Option<b33p_core::Command>>| {
        let mut done = false;
        shared.act(|chat| match f(chat) {
            Some(command) => {
                done = true;
                (command, Update::reset())
            }
            None => (None, Update::default()),
        });
        done
    };
    match (control, alt, shift) {
        (true, false, false) if key.eq_ignore_ascii_case("k") => {
            window.set_settings_open(false);
            window.set_focus_search_request(window.get_focus_search_request() + 1);
            true
        }
        (true, false, false) if key.eq_ignore_ascii_case("f") => {
            if shared.chat.borrow().selected().is_none() {
                return false;
            }
            window.set_chat_search_text(SharedString::new());
            shared.act(|chat| {
                chat.open_chat_search();
                (None, Update::default())
            });
            true
        }
        (false, true, false) if is(Key::DownArrow) => open(&|chat| chat.select_relative(1)),
        (false, true, false) if is(Key::UpArrow) => open(&|chat| chat.select_relative(-1)),
        (false, true, true) if is(Key::DownArrow) => open(&|chat| chat.select_next_unread()),
        (false, true, false) => match key.parse::<usize>() {
            Ok(n @ 1..=9) => open(&|chat| chat.select_pinned(n - 1)),
            _ => false,
        },
        (false, false, false) if is(Key::Escape) => escape(shared, window),
        _ => false,
    }
}

/// Esc closes what is open, the innermost first.
fn escape(shared: &Rc<Shared>, window: &AppWindow) -> bool {
    let closed = close_front(shared, window);
    if closed {
        // What had the focus may have closed with it.
        window.set_escaped(window.get_escaped().wrapping_add(1));
    }
    closed
}

/// Closes the innermost of what is open; whether there was something.
fn close_front(shared: &Rc<Shared>, window: &AppWindow) -> bool {
    let mut chat = shared.chat.borrow_mut();
    if window.get_stories_open() {
        if chat.current_story().is_some() {
            chat.close_story();
        } else {
            window.set_stories_open(false);
        }
    } else if chat.group_draft().is_some() {
        chat.close_group_draft();
    } else if chat.forwarding() {
        chat.cancel_forward();
    } else if !chat.selection().is_empty() {
        let update = chat.clear_selection();
        drop(chat);
        shared.refresh(update);
        return true;
    } else if chat.details().is_some() {
        chat.close_details();
    } else if chat.media().is_some() {
        chat.close_media();
    } else if chat.chat_search().is_some() {
        chat.close_chat_search();
    } else if chat.editing().is_some() {
        chat.cancel_edit();
        window.set_draft(chat.draft().into());
    } else if chat.replying().is_some() {
        chat.cancel_reply();
    } else if window.get_settings_open() {
        window.set_settings_open(false);
    } else if chat.searching() {
        let update = chat.search("");
        drop(chat);
        shared.refresh(update);
        return true;
    } else if window.get_narrow() && chat.selected().is_some() {
        let update = chat.deselect();
        drop(chat);
        shared.refresh(update);
        return true;
    } else {
        return false;
    }
    drop(chat);
    shared.refresh(Update::default());
    true
}

/// A handler for one of `ConversationMenu`'s actions (app.slint), which take the conversation
/// id first. Archiving or marking unread may close the chat, so everything is redrawn.
fn chat_action<A>(
    shared: &Rc<Shared>,
    action: impl Fn(&mut ChatState, &ConversationId, A) -> Option<b33p_core::Command> + 'static,
) -> impl Fn(SharedString, A) + 'static {
    let shared = shared.clone();
    move |id, argument| {
        let id = ConversationId(id.into());
        shared.act(|chat| (action(chat, &id, argument), Update::reset()));
    }
}

/// Brings the window in line with `chat`, touching only what `update` says changed (plus the
/// cheap status properties).
pub(crate) fn render(window: &AppWindow, chat: &ChatState, images: &Images, update: Update) {
    if update.screen {
        match chat.screen() {
            state::Screen::Connecting => window.set_screen(Screen::Connecting),
            state::Screen::Password { new, error } => {
                window.set_password_new(*new);
                window.set_password_error(error.as_deref().unwrap_or_default().into());
                window.set_screen(Screen::Password);
            }
            state::Screen::Link(qr) => {
                window.set_link_qr(qr_image(qr));
                window.set_screen(Screen::Link);
            }
            state::Screen::Chats => window.set_screen(Screen::Chats),
        }
        window.set_account(chat.account().into());
    }
    window.set_synced(chat.synced());
    window.set_history_status(
        match chat.transfer() {
            None | Some(HistoryTransfer::Finished) => String::new(),
            Some(HistoryTransfer::Waiting) => {
                "Waiting for your phone to send your message history…".into()
            }
            Some(HistoryTransfer::Downloading) => "Downloading your message history…".into(),
            Some(HistoryTransfer::Importing { messages }) => {
                format!("Importing your message history… {messages} messages")
            }
        }
        .into(),
    );
    window.set_history_skippable(chat.transfer() == Some(HistoryTransfer::Waiting));
    window.set_error(chat.error().unwrap_or_default().into());
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
    window.set_staged(ModelRc::new(VecModel::from(staged)));
    render_media(window, chat, images);
    render_stickers(window, chat);
    render_details(window, chat);
    render_stories(window, chat, images);
    let search = chat.chat_search();
    window.set_chat_search_open(search.is_some());
    window.set_chat_search_busy(
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
    window.set_search_results(ModelRc::new(VecModel::from(results)));
    window.set_search_is_number(state::looks_like_number(&window.get_search_text()));
    window.set_group_filter_is_number(state::looks_like_number(&window.get_group_draft_filter()));
    let draft = chat.group_draft();
    window.set_group_draft_open(draft.is_some());
    if let Some((new, chosen)) = draft {
        window.set_group_draft_new(new);
        window.set_group_draft_chosen(chosen.len() as i32);
        let candidates: Vec<CandidateItem> = chat
            .group_candidates(&window.get_group_draft_filter())
            .into_iter()
            .map(|c| CandidateItem {
                id: c.id.0.as_str().into(),
                title: c.title.as_str().into(),
                initials: state::initials(&c.title).into(),
                color: avatar_color(&c.id),
                avatar: chat
                    .avatar(&c.id)
                    .map(|a| images.avatar(&c.id, a))
                    .unwrap_or_default(),
                chosen: chosen.contains(&c.id),
            })
            .collect();
        update_list(&window.get_group_candidates(), candidates, |a, b| a == b);
    }
    window.set_forwarding(chat.forwarding());
    window.set_forward_preview(chat.forwarded().unwrap_or_default().into());
    if chat.forwarding() {
        let targets: Vec<_> = chat
            .forward_targets(&window.get_forward_filter())
            .into_iter()
            .map(|c| ConversationItem {
                id: c.id.0.as_str().into(),
                title: c.title.as_str().into(),
                initials: state::initials(&c.title).into(),
                avatar_color: avatar_color(&c.id),
                avatar: chat
                    .avatar(&c.id)
                    .map(|a| images.avatar(&c.id, a))
                    .unwrap_or_default(),
                ..Default::default()
            })
            .collect();
        window.set_forward_targets(ModelRc::new(VecModel::from(targets)));
    }
    let (archived, archived_unread) = chat.archived();
    window.set_archived_count(archived as i32);
    window.set_archived_unread(archived_unread as i32);
    window.set_showing_archived(chat.showing_archived());
    let folders: Vec<SharedString> = chat
        .folders()
        .iter()
        .map(|f| f.name.as_str().into())
        .collect();
    let unread: Vec<i32> = (0..folders.len())
        .map(|at| chat.folder_unread(at) as i32)
        .collect();
    window.set_folders(ModelRc::new(VecModel::from(folders)));
    window.set_folder_unread(ModelRc::new(VecModel::from(unread)));
    window.set_folder(chat.folder().map_or(-1, |at| at as i32));
    window.set_typing(
        chat.selected()
            .map(|c| state::typing_label(&chat.typing(&c.id)))
            .unwrap_or_default()
            .into(),
    );
    // An edit that ended without being sent (its message was deleted, here or elsewhere)
    // leaves the composer to the draft again, so its text can't be sent as a new message.
    let editing = chat.editing().is_some();
    if window.get_editing() && !editing {
        window.set_draft(chat.draft().into());
    }
    window.set_editing(editing);
    window.set_selected_count(chat.selection().len() as i32);
    window.set_selection_text(chat.selected_text().into());
    window.set_can_forward_selection(chat.can_forward_selection());
    window.set_can_delete_selection_for_everyone(chat.can_delete_selection_for_everyone());
    let replying = chat.replying();
    window.set_reply_author(replying.map_or("", |q| q.author.as_str()).into());
    window.set_reply_text(replying.map_or("", |q| q.text.as_str()).into());

    let messages = window.get_messages();
    let messages = vec_model(&messages);
    match update.messages {
        MessagesUpdate::Unchanged => {}
        MessagesUpdate::Appended(n) => {
            for message in chat.last_messages(n) {
                messages.push(MessageItem {
                    fresh: true,
                    ..message_item(&message, chat, images)
                });
            }
            settle_fresh(window);
        }
        MessagesUpdate::Prepended(n) => {
            // The list keeps what is on screen in place.
            let items: Vec<_> = chat
                .messages()
                .take(n + 1)
                .map(|m| message_item(&m, chat, images))
                .collect();
            for (row, item) in items.into_iter().enumerate() {
                if row < n {
                    messages.insert(row, item);
                } else {
                    // The first of those already shown: its day label or run may have changed.
                    messages.set_row_data(row, item);
                }
            }
        }
        MessagesUpdate::Reset => messages.set_vec(
            chat.messages()
                .map(|m| message_item(&m, chat, images))
                .collect::<Vec<_>>(),
        ),
    }
    if update.messages != MessagesUpdate::Unchanged {
        render_activity(window, chat);
        if window.get_calendar_open() {
            render_calendar(window, chat);
        }
    }
    // Messages changed in place (downloads, reactions): redraw only rows that differ.
    if update.rows && update.messages != MessagesUpdate::Reset {
        for (row, message) in chat.messages().enumerate() {
            let item = message_item(&message, chat, images);
            if !messages
                .row_data(row)
                .is_some_and(|old| same_message(&old, &item))
            {
                messages.set_row_data(row, item);
            }
        }
    }

    // After the rows are there.
    if update.scroll_to_end {
        window.set_scroll_to_end_serial(window.get_scroll_to_end_serial().wrapping_add(1));
    }
    if let Some((row, count)) = chat.jump()
        && count as i32 != window.get_jump_serial()
    {
        window.set_jump_row(row as i32);
        window.set_jump_serial(count as i32);
    }

    if update.conversations {
        let pinned = chat.pinned().len();
        let items: Vec<_> = chat
            .conversations()
            .map(|c| conversation_item(&c, pinned, chat, images))
            .collect();
        let conversations = window.get_conversations();
        let conversations = vec_model(&conversations);
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
        window.set_selected(chat.selected_index().map_or(-1, |i| i as i32));
        let selected_id = chat.selected().map_or("", |c| c.id.0.as_str());
        if window.get_selected_id() != selected_id {
            // Each chat has its own draft, however it was opened.
            window.set_draft(chat.draft().into());
        }
        window.set_selected_id(selected_id.into());
        window.set_current(
            chat.selected_view()
                .map(|view| conversation_item(&view, pinned, chat, images))
                .unwrap_or_default(),
        );
        if !chat.searching() {
            window.set_search_text(SharedString::new());
        }
    }
    // Last, once switching chats or ending an edit has put the right draft in the composer.
    let draft = window.get_draft();
    window.set_can_send(chat.can_compose(&draft));
    let candidates: Vec<SharedString> = chat
        .mention_candidates(&draft)
        .iter()
        .map(|p| p.name.as_str().into())
        .collect();
    window.set_mention_candidates(ModelRc::new(VecModel::from(candidates)));
}

/// Brings a list in line with `items`, touching only rows that differ, so it keeps its scroll
/// position. `same` compares rows (nested models compare by identity otherwise).
fn update_list<T: Clone + 'static>(
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
fn render_stickers(window: &AppWindow, chat: &ChatState) {
    const PER_ROW: usize = 6;
    let packs = chat.sticker_packs();
    window.set_stickers_busy(matches!(packs, Some(None)));
    let packs = packs.flatten().unwrap_or_default();
    let titles: Vec<SharedString> = packs.iter().map(|p| p.title.as_str().into()).collect();
    if window.get_sticker_packs().row_count() != titles.len() {
        window.set_sticker_packs(ModelRc::new(VecModel::from(titles)));
    }
    let chosen = (window.get_sticker_pack().max(0) as usize).min(packs.len().saturating_sub(1));
    let Some(pack) = packs.get(chosen) else {
        window.set_sticker_rows(ModelRc::new(VecModel::<StickerRow>::default()));
        return;
    };
    // Only when the pack changes, so the grid keeps its place otherwise.
    let shown = window.get_sticker_rows();
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
    window.set_sticker_rows(ModelRc::new(VecModel::from(rows)));
}

/// The stories overlay.
fn render_stories(window: &AppWindow, chat: &ChatState, images: &Images) {
    window.set_unviewed_stories(chat.unviewed_stories() as i32);
    if !window.get_stories_open() {
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
            avatar: chat
                .avatar(ring.conversation)
                .map(|a| images.avatar(ring.conversation, a))
                .unwrap_or_default(),
            unviewed: ring.unviewed,
            time: state::format_list_time(ring.latest).into(),
        })
        .collect();
    update_list(&window.get_story_rings(), rings, |a, b| a == b);
    // Rebuilt when the groups or their names change; the choice stays with its group.
    let groups = chat.story_groups();
    let ids: Vec<ConversationId> = groups.iter().map(|c| c.id.clone()).collect();
    let titles: Vec<SharedString> = groups.iter().map(|c| c.title.as_str().into()).collect();
    let offered: Vec<SharedString> = window.get_story_group_ids().iter().collect();
    if !offered
        .iter()
        .map(SharedString::as_str)
        .eq(ids.iter().map(|id| id.0.as_str()))
        || !window.get_story_groups().iter().eq(titles.iter().cloned())
    {
        let chosen = offered.get(window.get_story_group().max(0) as usize);
        let at = chosen
            .and_then(|c| ids.iter().position(|id| id.0 == c.as_str()))
            .unwrap_or(0);
        let ids: Vec<SharedString> = ids.iter().map(|id| id.0.as_str().into()).collect();
        window.set_story_groups(ModelRc::new(VecModel::from(titles)));
        window.set_story_group_ids(ModelRc::new(VecModel::from(ids)));
        window.set_story_group(at as i32);
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
    window.set_story(story);
}

/// The details page.
fn render_details(window: &AppWindow, chat: &ChatState) {
    let details = chat.details();
    window.set_details_open(details.is_some());
    window.set_details_busy(matches!(details, Some(None)));
    let details = details.flatten().cloned().unwrap_or_default();
    window.set_details_about(details.about.unwrap_or_default().into());
    window.set_details_phone(details.phone.unwrap_or_default().into());
    // Four groups of five digits per line, as on phones.
    let safety = details.safety_number.map(|number| {
        let groups: Vec<&str> = number.split_whitespace().collect();
        groups
            .chunks(4)
            .map(|line| line.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    });
    window.set_details_safety_number(safety.unwrap_or_default().into());
    window.set_details_verified(details.trust == b33p_core::Trust::Verified);
    window.set_details_safety_changed(details.trust == b33p_core::Trust::Changed);
    window.set_details_description(details.description.unwrap_or_default().into());
    let members: Vec<MemberItem> = details
        .members
        .iter()
        .map(|(person, admin)| {
            let id = ConversationId::contact(&person.service_id);
            MemberItem {
                id: person.service_id.as_str().into(),
                name: person.name.as_str().into(),
                initials: state::initials(&person.name).into(),
                color: avatar_color(&id),
                admin: *admin,
            }
        })
        .collect();
    // Row by row: a new model would rebuild the rows, closing a member's menu under the
    // pointer whenever anything happens.
    update_list(&window.get_details_members(), members, |a, b| a == b);
}

/// The media overview.
fn render_media(window: &AppWindow, chat: &ChatState, images: &Images) {
    let media = chat.media();
    window.set_media_open(media.is_some());
    window.set_media_busy(matches!(media, Some(None)));
    let Some(Some(overview)) = media else {
        // Nothing left over for the next chat's.
        vec_model(&window.get_media_tiles()).set_vec(Vec::new());
        vec_model(&window.get_media_files()).set_vec(Vec::new());
        vec_model(&window.get_media_links()).set_vec(Vec::new());
        return;
    };
    // The UI lays the tiles out by its width; their size here is beside the point.
    let tiles: Vec<AttachmentItem> = overview
        .media
        .iter()
        .map(|a| attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images))
        .collect();
    update_list(&window.get_media_tiles(), tiles, |a, b| a == b);
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
    update_list(&window.get_media_files(), files, |a, b| a == b);
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
    update_list(&window.get_media_links(), links, |a, b| a == b);
}

/// The open chat's activity strip, or none while it is turned off.
fn render_activity(window: &AppWindow, chat: &ChatState) {
    let items = if window.get_show_activity() {
        activity_items(chat)
    } else {
        Vec::new()
    };
    window.set_activity(ModelRc::new(VecModel::from(items)));
}

/// Weeks or months (whichever fit) from top to bottom, long silences folded into a short gap.
fn activity_items(chat: &ChatState) -> Vec<ActivityItem> {
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
fn render_calendar(window: &AppWindow, chat: &ChatState) {
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
    window.set_calendar_summary(summary.into());
    window.set_calendar(ModelRc::new(VecModel::from(years)));
}

/// Gives the window the models that [`render`] keeps up to date: the window owns them, and
/// [`vec_model`] gets them back for updates.
pub(crate) fn set_up_models(window: &AppWindow) {
    window.set_conversations(ModelRc::new(VecModel::<ConversationItem>::default()));
    window.set_messages(ModelRc::new(VecModel::<MessageItem>::default()));
    window.set_media_tiles(ModelRc::new(VecModel::<AttachmentItem>::default()));
    window.set_group_candidates(ModelRc::new(VecModel::<CandidateItem>::default()));
    window.set_story_rings(ModelRc::new(VecModel::<RingItem>::default()));
    window.set_media_files(ModelRc::new(VecModel::<SharedItem>::default()));
    window.set_media_links(ModelRc::new(VecModel::<SharedItem>::default()));
    window.set_details_members(ModelRc::new(VecModel::<MemberItem>::default()));
}

/// The `VecModel` behind a model set up in [`set_up_models`].
fn vec_model<T: Clone + 'static>(model: &ModelRc<T>) -> &VecModel<T> {
    model
        .as_any()
        .downcast_ref()
        .expect("models are VecModels, set up in create")
}

fn conversation_item(
    view: &ConversationView,
    pinned: usize,
    chat: &ChatState,
    images: &Images,
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
        time: view
            .last_message
            .map(|m| state::format_list_time(m.timestamp))
            .unwrap_or_default()
            .into(),
        subtitle: c.subtitle.as_str().into(),
        unread: view.unread as i32,
        pinned: view.pinned(),
        request: chat.is_request(c),
        blocked: c.settings.blocked,
        left: c.left,
        invited: c.invited,
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

/// A stable colour per person or group, for avatars without a picture and group senders' names.
fn avatar_color(id: &ConversationId) -> Color {
    // Muted hues in the spirit of Signal's avatar palette; all readable with white text.
    const PALETTE: [u32; 8] = [
        0x3a76f0, 0x7d4fc9, 0xc4314b, 0xd36c1c, 0x2b8a60, 0x1e8a9e, 0x9a5b3c, 0x5c6bc0,
    ];
    Color::from_argb_encoded(0xff00_0000 | PALETTE[state::avatar_index(id, PALETTE.len())])
}

fn message_item(view: &MessageView, chat: &ChatState, images: &Images) -> MessageItem {
    let m = view.message;
    let attachments: Vec<_> = m
        .attachments
        .iter()
        .map(|a| attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images))
        .collect();
    let in_group = chat.selected().is_some_and(|c| c.is_group()) && !m.outgoing;
    let sender = ConversationId::contact(&m.sender_id);
    MessageItem {
        sender: m.sender.as_str().into(),
        body: crate::rich_text(view.body, view.styles, view.revealed),
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
            (true, Some(Unsent::Failed), _) => DeliveryState::Failed,
            (true, None, Delivery::Sent) => DeliveryState::Sent,
            (true, None, Delivery::Delivered) => DeliveryState::Delivered,
            (true, None, Delivery::Read) => DeliveryState::Read,
        },
        edited: m.edited_at.is_some(),
        deleted: m.deleted,
        notice: m.notice,
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

/// One chip per emoji, in the order they first came, counting everyone who chose it.
fn reaction_items(reactions: &[b33p_core::Reaction]) -> Vec<ReactionItem> {
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

/// Like `==`, but comparing the nested lists by content: each item gets new models, which would
/// never compare equal and so redraw every row with an attachment or a reaction.
/// Once new rows had time to fade in, they are like the others: shown again (scrolled back to,
/// or redrawn), they don't fade in again.
fn settle_fresh(window: &AppWindow) {
    let window = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
        let Some(window) = window.upgrade() else {
            return;
        };
        let messages = window.get_messages();
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

fn same_message(a: &MessageItem, b: &MessageItem) -> bool {
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

fn attachment_item(
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
        (AttachmentKind::File, Some(name)) => format!("📎 {name} · {size}"),
        _ => format!("{} · {size}", state::describe_kind(kind)),
    };
    let failed = match download {
        Some(AttachmentState::Failed(error)) => Some(error.clone()),
        _ => None,
    };
    // Saving says how it went; otherwise a failed download says why (click to retry).
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
        None => failed
            .clone()
            .map(|error| format!("{error} Click to retry."))
            .unwrap_or_default(),
    };
    AttachmentItem {
        id: attachment.id.0.as_str().into(),
        inline,
        hidden: kind == AttachmentKind::LongText,
        fetch: download.is_none() && state::shown_inline(kind),
        picture: picture
            .map(|p| images.attachments.get(&attachment.id, p))
            .unwrap_or_default(),
        width: w * scale,
        height: h * scale,
        label: label.into(),
        status: status.into(),
        can_retry: failed.is_some() && state::shown_inline(kind),
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
