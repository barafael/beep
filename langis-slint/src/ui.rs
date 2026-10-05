// Included once per widget style by `main.rs`: the types Slint generates (`AppWindow`,
// `MessageItem`, …) are separate per style, this code is the same for all of them.

use std::rc::Rc;

use langis_core::settings::{NotificationContent, Settings, TextSize, Theme, WidgetStyle};
use langis_core::state::{
    self, AttachmentState, ChatState, ConversationView, MessageView, MessagesUpdate, SaveState,
    Update,
};
use langis_core::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, Backend, ConversationId, Delivery,
    HistoryTransfer, now_millis,
};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{Images, Shared, Ui, qr_image};

/// Builds this style's window for `shared`, showing what `shared` holds. Not shown yet.
pub fn create(shared: &Rc<Shared>) -> Result<Rc<dyn Ui>, slint::PlatformError> {
    let window = AppWindow::new()?;
    if Backend::mock_requested() {
        window.set_window_title("langis — MOCK (nothing is sent)".into());
    }
    show_settings(&window, &shared.settings.borrow(), &shared.style_note());
    window.set_device_name(shared.device_name.as_str().into());
    // The window owns the models; `vec_model` gets them back for updates.
    window.set_conversations(ModelRc::new(VecModel::<ConversationItem>::default()));
    window.set_messages(ModelRc::new(VecModel::<MessageItem>::default()));
    window.set_media_rows(ModelRc::new(VecModel::<MediaRow>::default()));
    window.set_media_files(ModelRc::new(VecModel::<SharedItem>::default()));
    window.set_media_links(ModelRc::new(VecModel::<SharedItem>::default()));

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
            if shared.act(|chat| (chat.compose(&text), Update::default()))
                && let Some(window) = window.upgrade()
            {
                window.set_draft(SharedString::new());
                shared.refresh(Update::default());
            }
        }
    });
    window.on_unlock({
        let shared = shared.clone();
        move |password, confirmation| {
            shared.act(|chat| (chat.unlock(&password, &confirmation), Update { screen: true, ..Update::default() }));
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
    window.on_mark_read(chat_action(shared, |chat, id, read: bool| chat.mark_unread(id, !read)));
    window.on_mute(chat_action(shared, |chat, id, seconds: i32| {
        let until = match seconds {
            0 => None,
            ..0 => Some(u64::MAX),
            seconds => Some(now_millis() + seconds as u64 * 1000),
        };
        chat.mute(id, until)
    }));
    window.on_archive(chat_action(shared, |chat, id, archived| chat.archive(id, archived)));
    window.on_set_timer(chat_action(shared, |chat, id, seconds: i32| {
        chat.set_timer(id, u32::try_from(seconds).ok()?)
    }));
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
            shared.act(|chat| (chat.forward_to(&ConversationId(id.into())), Update::default()));
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
            let Some(window) = window.upgrade() else { return };
            let draft = shared.chat.borrow_mut().pick_mention(&window.get_draft(), index as usize);
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
            let Some(window) = window.upgrade() else { return false };
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
        move || {
            shared.act(|chat| {
                chat.cancel_edit();
                (None, Update::default())
            });
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
    window.on_settings_changed({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            let Some(window) = window.upgrade() else { return };
            let settings = read_settings(&window);
            window.set_font_scale(settings.text_size.scale());
            shared.apply_settings(settings);
            window.set_style_note(shared.style_note().into());
        }
    });
    window.on_fetch_attachment({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| (chat.fetch_attachment(&AttachmentId(id.into())), Update { rows: true, ..Update::default() }));
        }
    });
    window.on_save_attachment({
        let shared = shared.clone();
        move |id| {
            shared.act(|chat| (chat.save_attachment(&AttachmentId(id.into())), Update { rows: true, ..Update::default() }));
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
            shared.act(|chat| (chat.open_attachment(&AttachmentId(id.into())), Update::default()));
        }
    });
    window.on_open_link(|link| crate::open_link(&link));
    window.on_attach({
        let window = window.as_weak();
        move || {
            // The dialog blocks until the user is done, so it gets its own thread.
            let window = window.clone();
            std::thread::spawn(move || {
                let files = rfd::FileDialog::new().set_title("Attach files").pick_files().unwrap_or_default();
                let files: Vec<SharedString> = files.iter().map(|f| f.to_string_lossy().as_ref().into()).collect();
                let _ = window.upgrade_in_event_loop(move |window| {
                    window.invoke_stage_files(ModelRc::new(VecModel::from(files)));
                });
            });
        }
    });
    window.on_paste({
        let shared = shared.clone();
        move || {
            let Some(files) = crate::clipboard::pasted() else {
                return false;
            };
            shared.act(|chat| {
                chat.stage_files(files);
                (None, Update::default())
            });
            true
        }
    });
    window.on_files_dropped({
        let shared = shared.clone();
        move |event| {
            let Ok(paths) = event.data.file_paths() else { return };
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

struct Window(AppWindow);

impl Ui for Window {
    fn render(&self, chat: &ChatState, images: &Images, update: Update) {
        render(&self.0, chat, images, update);
    }

    fn show(&self) -> Result<(), slint::PlatformError> {
        self.0.show()
    }

    fn hide(&self) {
        if let Err(error) = self.0.hide() {
            tracing::warn!(%error, "failed to close a window");
        }
    }

    /// On Wayland the compositor decides; it lets langis to the front when given an
    /// activation token (see `Shared::activate`), and may only highlight it otherwise.
    fn raise(&self) {
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

    fn visible(&self) -> bool {
        self.0.window().is_visible()
    }

    /// Slint's public API doesn't offer this yet, but both the Qt and the winit backend keep it
    /// up to date internally.
    fn active(&self) -> bool {
        i_slint_core::window::WindowInner::from_pub(self.0.window()).active()
    }

    fn geometry(&self) -> (slint::PhysicalPosition, slint::PhysicalSize) {
        (self.0.window().position(), self.0.window().size())
    }

    fn take_geometry(&self, (position, size): (slint::PhysicalPosition, slint::PhysicalSize)) {
        self.0.window().set_size(size);
        self.0.window().set_position(position);
    }

    fn open_settings(&self) {
        self.0.set_settings_open(true);
    }
}

/// The settings page shows choices by index, in the order of these enums.
fn show_settings(window: &AppWindow, settings: &Settings, style_note: &str) {
    window.set_theme(match settings.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    let style = WidgetStyle::ALL.iter().position(|s| *s == settings.style).unwrap_or_default();
    window.set_widget_style(style as i32);
    window.set_style_note(style_note.into());
    window.set_text_size(match settings.text_size {
        TextSize::Small => 0,
        TextSize::Default => 1,
        TextSize::Large => 2,
    });
    window.set_show_avatars(settings.show_avatars);
    window.set_notifications(settings.notifications);
    window.set_notification_content(match settings.notification_content {
        NotificationContent::NameAndMessage => 0,
        NotificationContent::NameOnly => 1,
        NotificationContent::Nothing => 2,
    });
    window.set_close_to_tray(settings.close_to_tray);
    window.set_read_receipts(settings.privacy.read_receipts);
    window.set_typing_indicators(settings.privacy.typing_indicators);
    window.set_autostart_supported(crate::autostart::SUPPORTED);
    window.set_start_with_system(crate::autostart::enabled());
    window.set_font_scale(settings.text_size.scale());
}

fn read_settings(window: &AppWindow) -> Settings {
    Settings {
        theme: match window.get_theme() {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        },
        style: WidgetStyle::ALL.get(window.get_widget_style() as usize).copied().unwrap_or_default(),
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
        close_to_tray: window.get_close_to_tray(),
        privacy: langis_core::Privacy {
            read_receipts: window.get_read_receipts(),
            typing_indicators: window.get_typing_indicators(),
        },
    }
}

/// Carries out the shortcut for a key pressed anywhere in the window; whether it was one.
fn shortcut(shared: &Rc<Shared>, window: &AppWindow, key: &str, control: bool, alt: bool, shift: bool) -> bool {
    use slint::platform::Key;
    let is = |k: Key| key.chars().eq(std::iter::once(char::from(k)));
    let open = |f: &dyn Fn(&mut ChatState) -> Option<Option<langis_core::Command>>| {
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
    let mut chat = shared.chat.borrow_mut();
    if chat.forwarding() {
        chat.cancel_forward();
    } else if chat.details().is_some() {
        chat.close_details();
    } else if chat.media().is_some() {
        chat.close_media();
    } else if chat.chat_search().is_some() {
        chat.close_chat_search();
    } else if chat.editing().is_some() {
        chat.cancel_edit();
        window.set_draft(SharedString::new());
    } else if chat.replying().is_some() {
        chat.cancel_reply();
    } else if window.get_settings_open() {
        window.set_settings_open(false);
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
    action: impl Fn(&mut ChatState, &ConversationId, A) -> Option<langis_core::Command> + 'static,
) -> impl Fn(SharedString, A) + 'static {
    let shared = shared.clone();
    move |id, argument| {
        let id = ConversationId(id.into());
        shared.act(|chat| (action(chat, &id, argument), Update::reset()));
    }
}

/// Brings the window in line with `chat`, touching only what `update` says changed (plus the
/// cheap status properties).
fn render(window: &AppWindow, chat: &ChatState, images: &Images, update: Update) {
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
            Some(HistoryTransfer::Waiting) => "Waiting for your phone to send your message history…".into(),
            Some(HistoryTransfer::Downloading) => "Downloading your message history…".into(),
            Some(HistoryTransfer::Importing { messages }) => {
                format!("Importing your message history… {messages} messages")
            }
        }
        .into(),
    );
    window.set_history_skippable(chat.transfer() == Some(HistoryTransfer::Waiting));
    window.set_error(chat.error().unwrap_or_default().into());
    let draft = window.get_draft();
    window.set_can_send(chat.can_compose(&draft));
    let candidates: Vec<SharedString> =
        chat.mention_candidates(&draft).iter().map(|p| p.name.as_str().into()).collect();
    window.set_mention_candidates(ModelRc::new(VecModel::from(candidates)));
    let staged: Vec<SharedString> = chat
        .staged()
        .iter()
        .map(|path| path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy()).as_ref().into())
        .collect();
    window.set_staged(ModelRc::new(VecModel::from(staged)));
    render_media(window, chat, images);
    render_details(window, chat);
    let search = chat.chat_search();
    window.set_chat_search_open(search.is_some());
    window.set_chat_search_busy(search.is_some_and(|(query, results)| !query.is_empty() && results.is_none()));
    let results: Vec<SearchResult> = search
        .and_then(|(query, results)| Some((query, results?)))
        .map(|(query, results)| {
            results
                .iter()
                .map(|m| SearchResult {
                    sender: m.sender.as_str().into(),
                    when: state::format_time(m.timestamp).into(),
                    text: state::snippet(&m.body, query).into(),
                })
                .collect()
        })
        .unwrap_or_default();
    window.set_search_results(ModelRc::new(VecModel::from(results)));
    window.set_forwarding(chat.forwarding());
    if chat.forwarding() {
        let targets: Vec<_> = chat
            .forward_targets(&window.get_forward_filter())
            .into_iter()
            .map(|c| ConversationItem {
                id: c.id.0.as_str().into(),
                title: c.title.as_str().into(),
                initials: state::initials(&c.title).into(),
                avatar_color: avatar_color(&c.id),
                avatar: chat.avatar(&c.id).map(|a| images.avatar(&c.id, a)).unwrap_or_default(),
                ..Default::default()
            })
            .collect();
        window.set_forward_targets(ModelRc::new(VecModel::from(targets)));
    }
    let (archived, archived_unread) = chat.archived();
    window.set_archived_count(archived as i32);
    window.set_archived_unread(archived_unread as i32);
    window.set_showing_archived(chat.showing_archived());
    window.set_typing(chat.selected().map(|c| state::typing_label(&chat.typing(&c.id))).unwrap_or_default().into());
    window.set_editing(chat.editing().is_some());
    let replying = chat.replying();
    window.set_reply_author(replying.map_or("", |q| q.author.as_str()).into());
    window.set_reply_text(replying.map_or("", |q| q.text.as_str()).into());

    let messages = window.get_messages();
    let messages = vec_model(&messages);
    match update.messages {
        MessagesUpdate::Unchanged => {}
        MessagesUpdate::Appended(n) => {
            for message in chat.last_messages(n) {
                messages.push(message_item(&message, chat, images));
            }
        }
        MessagesUpdate::Prepended(n) => {
            // The list keeps what is on screen in place.
            let items: Vec<_> = chat.messages().take(n + 1).map(|m| message_item(&m, chat, images)).collect();
            for (row, item) in items.into_iter().enumerate() {
                if row < n {
                    messages.insert(row, item);
                } else {
                    // The first of those already shown: its day label or run may have changed.
                    messages.set_row_data(row, item);
                }
            }
        }
        MessagesUpdate::Reset => {
            messages.set_vec(chat.messages().map(|m| message_item(&m, chat, images)).collect::<Vec<_>>())
        }
    }
    // Messages changed in place (downloads, reactions): redraw only rows that differ.
    if update.rows && update.messages != MessagesUpdate::Reset {
        for (row, message) in chat.messages().enumerate() {
            let item = message_item(&message, chat, images);
            if !messages.row_data(row).is_some_and(|old| same_message(&old, &item)) {
                messages.set_row_data(row, item);
            }
        }
    }

    // After the rows are there.
    if let Some((row, count)) = chat.jump()
        && count as i32 != window.get_jump_serial()
    {
        window.set_jump_row(row as i32);
        window.set_jump_serial(count as i32);
    }

    if update.conversations {
        let pinned = chat.pinned().len();
        let items: Vec<_> = chat.conversations().map(|c| conversation_item(&c, pinned, images)).collect();
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
        if !chat.searching() {
            window.set_search_text(SharedString::new());
        }
    }
}

/// Brings a list in line with `items`, touching only rows that differ, so it keeps its scroll
/// position. `same` compares rows (nested models compare by identity otherwise).
fn update_list<T: Clone + 'static>(model: &ModelRc<T>, items: Vec<T>, same: impl Fn(&T, &T) -> bool) {
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
        groups.chunks(4).map(|line| line.join(" ")).collect::<Vec<_>>().join("\n")
    });
    window.set_details_safety_number(safety.unwrap_or_default().into());
    window.set_details_description(details.description.unwrap_or_default().into());
    let members: Vec<MemberItem> = details
        .members
        .iter()
        .map(|(person, admin)| {
            let id = ConversationId::contact(&person.service_id);
            MemberItem {
                name: person.name.as_str().into(),
                initials: state::initials(&person.name).into(),
                color: avatar_color(&id),
                admin: *admin,
            }
        })
        .collect();
    window.set_details_members(ModelRc::new(VecModel::from(members)));
}

/// The media overview.
fn render_media(window: &AppWindow, chat: &ChatState, images: &Images) {
    const PER_ROW: usize = 4;
    const TILE: f32 = 110.;
    let media = chat.media();
    window.set_media_open(media.is_some());
    window.set_media_busy(matches!(media, Some(None)));
    let Some(Some(overview)) = media else {
        // Nothing left over for the next chat's.
        vec_model(&window.get_media_rows()).set_vec(Vec::new());
        vec_model(&window.get_media_files()).set_vec(Vec::new());
        vec_model(&window.get_media_links()).set_vec(Vec::new());
        return;
    };
    let tile = |a: &Attachment| AttachmentItem {
        width: TILE,
        height: TILE,
        ..attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images)
    };
    let rows: Vec<MediaRow> = overview
        .media
        .chunks(PER_ROW)
        .map(|row| MediaRow { tiles: ModelRc::new(VecModel::from(row.iter().map(|a| tile(a)).collect::<Vec<_>>())) })
        .collect();
    update_list(&window.get_media_rows(), rows, |a, b| {
        a.tiles.row_count() == b.tiles.row_count() && a.tiles.iter().zip(b.tiles.iter()).all(|(x, y)| x == y)
    });
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
            url: (*url).into(),
            sender: m.sender.as_str().into(),
            when: state::format_time(m.timestamp).into(),
            ..Default::default()
        })
        .collect();
    update_list(&window.get_media_links(), links, |a, b| a == b);
}

/// The `VecModel` behind a model set up in [`create`].
fn vec_model<T: Clone + 'static>(model: &ModelRc<T>) -> &VecModel<T> {
    model.as_any().downcast_ref().expect("models are VecModels, set up in create")
}

fn conversation_item(view: &ConversationView, pinned: usize, images: &Images) -> ConversationItem {
    let c = view.conversation;
    ConversationItem {
        id: c.id.0.as_str().into(),
        title: c.title.as_str().into(),
        initials: state::initials(&c.title).into(),
        avatar_color: avatar_color(&c.id),
        avatar: view.avatar.map(|avatar| images.avatar(&c.id, avatar)).unwrap_or_default(),
        preview: view.preview().into(),
        time: view.last_message.map(|m| state::format_list_time(m.timestamp)).unwrap_or_default().into(),
        subtitle: c.subtitle.as_str().into(),
        unread: view.unread as i32,
        pinned: view.pinned(),
        can_move_up: c.settings.pinned.is_some_and(|at| at > 0),
        can_move_down: c.settings.pinned.is_some_and(|at| (at as usize) + 1 < pinned),
        muted: view.muted(),
        marked_unread: view.marked_unread(),
        archived: c.settings.archived,
        typing: state::typing_label(&view.typing).into(),
        timer: c.expire_timer as i32,
        timer_label: langis_core::format_timer(c.expire_timer).into(),
    }
}

/// A stable colour per person or group, for avatars without a picture and group senders' names.
fn avatar_color(id: &ConversationId) -> Color {
    // Muted hues in the spirit of Signal's avatar palette; all readable with white text.
    const PALETTE: [u32; 8] = [0x3a76f0, 0x7d4fc9, 0xc4314b, 0xd36c1c, 0x2b8a60, 0x1e8a9e, 0x9a5b3c, 0x5c6bc0];
    Color::from_argb_encoded(0xff00_0000 | PALETTE[state::avatar_index(id, PALETTE.len())])
}

fn message_item(view: &MessageView, chat: &ChatState, images: &Images) -> MessageItem {
    let m = view.message;
    let attachments: Vec<_> =
        m.attachments.iter().map(|a| attachment_item(a, chat.attachment(&a.id), chat.saved(&a.id), images)).collect();
    let in_group = chat.selected().is_some_and(|c| c.is_group) && !m.outgoing;
    let sender = ConversationId::contact(&m.sender_id);
    MessageItem {
        sender: m.sender.as_str().into(),
        body: crate::rich_text(view.body, view.styles, view.revealed),
        has_body: !view.body.is_empty(),
        time: state::format_clock(m.timestamp).into(),
        outgoing: m.outgoing,
        show_sender: view.show_sender,
        day: view.day.as_deref().unwrap_or_default().into(),
        first: view.first_of_run,
        last: view.last_of_run,
        avatar_column: in_group,
        sender_avatar: view.sender_avatar.map(|picture| images.avatar(&sender, picture)).unwrap_or_default(),
        sender_initials: state::initials(&m.sender).into(),
        sender_color: avatar_color(&sender),
        quote_author: m.quote.as_ref().map_or("", |q| q.author.as_str()).into(),
        quote_text: m.quote.as_ref().map_or("", |q| q.text.as_str()).into(),
        attachments: ModelRc::new(VecModel::from(attachments)),
        reactions: ModelRc::new(VecModel::from(reaction_items(&m.reactions))),
        text: if m.deleted { "" } else { view.body }.into(),
        delivery: match (m.outgoing, m.delivery) {
            (false, _) => 0,
            (true, Delivery::Sent) => 1,
            (true, Delivery::Delivered) => 2,
            (true, Delivery::Read) => 3,
        },
        edited: m.edited_at.is_some(),
        deleted: m.deleted,
        expires: m.expires_at.map(|at| state::format_remaining(at, now_millis())).unwrap_or_default().into(),
        can_edit: chat.can_edit(m),
        first_unread: view.first_unread,
        can_forward: ChatState::can_forward(m),
        highlighted: view.highlighted,
    }
}

/// One chip per emoji, in the order they first came, counting everyone who chose it.
fn reaction_items(reactions: &[langis_core::Reaction]) -> Vec<ReactionItem> {
    let mut items: Vec<ReactionItem> = Vec::new();
    for reaction in reactions {
        match items.iter_mut().find(|item| item.emoji.as_str() == reaction.emoji) {
            Some(item) => {
                item.count += 1;
                item.mine |= reaction.outgoing;
            }
            None => items.push(ReactionItem { emoji: reaction.emoji.as_str().into(), count: 1, mine: reaction.outgoing }),
        }
    }
    items
}

/// Like `==`, but comparing the nested lists by content: each item gets new models, which would
/// never compare equal and so redraw every row with an attachment or a reaction.
fn same_message(a: &MessageItem, b: &MessageItem) -> bool {
    fn same<T: PartialEq>(a: &ModelRc<T>, b: &ModelRc<T>) -> bool {
        a.row_count() == b.row_count() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
    }
    same(&a.attachments, &b.attachments)
        && same(&a.reactions, &b.reactions)
        && MessageItem { attachments: b.attachments.clone(), reactions: b.reactions.clone(), ..a.clone() } == *b
}

fn attachment_item(
    attachment: &Attachment,
    download: Option<&AttachmentState>,
    save: Option<&SaveState>,
    images: &Images,
) -> AttachmentItem {
    let kind = attachment.kind;
    let inline = matches!(kind, AttachmentKind::Image | AttachmentKind::Sticker);
    let picture = match download {
        Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => Some(picture),
        _ => None,
    };
    // Fit into the bubble, keeping the shape the sender reported (or the picture has).
    let (max, fallback) = if kind == AttachmentKind::Sticker { (128.0, (128.0, 128.0)) } else { (280.0, (240.0, 180.0)) };
    let (w, h) = match picture {
        Some(p) => (p.width as f32, p.height as f32),
        None if attachment.width > 0 && attachment.height > 0 => (attachment.width as f32, attachment.height as f32),
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
            path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("disk")
        ),
        Some(SaveState::Failed(error)) => format!("Couldn't save: {error}"),
        None => failed.clone().map(|error| format!("{error} Click to retry.")).unwrap_or_default(),
    };
    AttachmentItem {
        id: attachment.id.0.as_str().into(),
        inline,
        hidden: kind == AttachmentKind::LongText,
        fetch: download.is_none() && state::shown_inline(kind),
        picture: picture.map(|p| images.attachments.get(&attachment.id, p)).unwrap_or_default(),
        width: w * scale,
        height: h * scale,
        label: label.into(),
        status: status.into(),
        can_retry: failed.is_some() && state::shown_inline(kind),
        can_save: !matches!(kind, AttachmentKind::ViewOnce | AttachmentKind::LongText | AttachmentKind::Sticker),
    }
}
