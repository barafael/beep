//! The window: what Slint generates from `ui/app.slint` (`AppWindow`, `MessageItem`, …), wired
//! to the chat state.

use std::rc::Rc;

use b33p_core::settings::{
    Animations, NotificationContent, Settings, TextSize, Theme, WidgetStyle,
};
use b33p_core::state::{
    self, AttachmentState, ChatState, Connection, ConversationView, MessageView, MessagesUpdate,
    SaveState, Update,
};
use b33p_core::timeline::{self, Stretch};
use b33p_core::{
    Attachment, AttachmentContent, AttachmentId, AttachmentKind, Backend, ConversationId, Delivery,
    Failure, HistoryTransfer, SyncStage, Unsent, now_millis,
};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::rows::Rows;

use crate::generated::*;
use crate::{Images, Shared, qr_image};

mod callbacks;
mod items;
mod render;

pub(crate) use items::set_up_models;
use items::*;
pub(crate) use render::render;
use render::*;

/// Builds the window for `shared`, showing what `shared` holds. Not shown yet.
pub fn create(shared: &Rc<Shared>) -> Result<Rc<Window>, slint::PlatformError> {
    let window = AppWindow::new()?;
    if Backend::mock_requested() {
        window.set_window_title("b33p — MOCK (nothing is sent)".into());
    }
    show_settings(&window, &shared.settings.borrow(), &shared.style_note());
    window
        .global::<Prefs>()
        .set_notifications_supported(shared.notifier.available());
    window
        .global::<Prefs>()
        .set_macos(cfg!(target_os = "macos"));
    window
        .global::<Account>()
        .set_device_name(shared.settings.borrow().device_name().into());
    set_up_models(&window);
    crate::emoji::set_up(&window, shared);

    callbacks::wire(&window, shared);

    window
        .global::<Account>()
        .set_version(env!("CARGO_PKG_VERSION").into());

    Ok(Rc::new(Window(window)))
}

pub struct Window(AppWindow);

impl Window {
    pub fn set_device_name(&self, name: &str) {
        self.0.global::<Account>().set_device_name(name.into());
    }

    /// Brings the window in line with `chat`, touching only what `update` says changed.
    /// `whole_list`: something besides events changed the chat list; see
    /// [`ChatState::take_changed_chats`].
    pub fn render(&self, chat: &ChatState, images: &Images, update: Update, whole_list: bool) {
        render(&self.0, chat, images, update, whole_list);
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.0.show()?;
        crate::capture::exclude(self.0.window());
        Ok(())
    }

    /// For focusing a window anew on Linux; see `Shared::activate`.
    #[cfg(all(unix, not(target_os = "macos")))]
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
        match self.0.show() {
            Ok(()) => crate::capture::exclude(self.0.window()),
            Err(error) => tracing::warn!(%error, "failed to show the window"),
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
        self.0.global::<Composer>().set_draft(text.into());
    }

    /// The send button, for the harness; returns whether a command went out.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn send(&self, shared: &Shared) -> bool {
        let text = self.0.global::<Composer>().get_draft();
        callbacks::chat::send(shared, &self.0, &text)
    }

    /// A message's Edit, for the harness.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn edit(&self, shared: &Shared, index: usize) {
        callbacks::chat::edit(shared, &self.0, index);
    }

    /// Writes the window as drawn to `path`, a PNG; its size in pixels.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn screenshot(&self, path: &std::path::Path) -> Result<(u32, u32), String> {
        let pixels = self
            .0
            .window()
            .take_snapshot()
            .map_err(|error| format!("can't take a screenshot: {error}"))?;
        let (width, height) = (pixels.width(), pixels.height());
        let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(pixels.as_bytes()))
            .map_err(|error| error.to_string())?;
        Ok((width, height))
    }

    /// Gives the window a size, in logical pixels.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn resize(&self, width: f32, height: f32) {
        self.0
            .window()
            .set_size(slint::LogicalSize::new(width, height));
    }

    /// Opens or closes what only the window keeps open, as its own buttons do; see the
    /// harness's `show` action.
    #[cfg(all(feature = "harness", target_family = "unix"))]
    pub fn show_part(&self, what: &str, open: bool, chat: &ChatState) -> Result<(), String> {
        let window = &self.0;
        match what {
            "settings" => window.global::<Prefs>().set_settings_open(open),
            "stories" => window.global::<Panels>().set_stories_open(open),
            "calendar" => {
                if open {
                    render_calendar(window, chat);
                }
                window.global::<OpenChat>().set_calendar_open(open);
            }
            "unlink" | "start_over" | "leave_group" => {
                window.global::<Account>().set_confirm(match (what, open) {
                    (_, false) => Confirm::None,
                    ("unlink", true) => Confirm::Unlink,
                    ("leave_group", true) => Confirm::LeaveGroup,
                    _ => Confirm::StartOver,
                })
            }
            _ => return Err(format!("nothing called {what} to show")),
        }
        Ok(())
    }
}

/// The settings page shows choices by index, in the order of these enums.
fn show_settings(window: &AppWindow, settings: &Settings, style_note: &str) {
    // As build.rs chooses: Qt on Linux.
    window.global::<Prefs>().set_qt(cfg!(target_os = "linux"));
    window
        .global::<Prefs>()
        .set_theme(index_of(&Theme::ALL, settings.theme));
    window
        .global::<Prefs>()
        .set_widget_style(index_of(&WidgetStyle::ALL, settings.style));
    window.global::<Prefs>().set_style_note(style_note.into());
    window
        .global::<Prefs>()
        .set_text_size(index_of(&TextSize::ALL, settings.text_size));
    window
        .global::<Prefs>()
        .set_animations(index_of(&Animations::ALL, settings.animations));
    window
        .global::<Prefs>()
        .set_show_avatars(settings.show_avatars);
    window
        .global::<Prefs>()
        .set_notifications(settings.notifications);
    window.global::<Prefs>().set_notification_content(index_of(
        &NotificationContent::ALL,
        settings.notification_content,
    ));
    window
        .global::<Prefs>()
        .set_close_to_tray(settings.close_to_tray);
    window
        .global::<Prefs>()
        .set_read_receipts(settings.preferences.read_receipts);
    window
        .global::<Prefs>()
        .set_typing_indicators(settings.preferences.typing_indicators);
    window
        .global::<Prefs>()
        .set_link_previews(settings.preferences.link_previews);
    window
        .global::<Prefs>()
        .set_sync_to_phone(settings.preferences.sync_to_phone);
    window
        .global::<Prefs>()
        .set_autostart_supported(crate::autostart::SUPPORTED);
    window
        .global::<Prefs>()
        .set_start_with_system(crate::autostart::enabled());
    window
        .global::<Prefs>()
        .set_font_scale(settings.text_size.scale());
    window
        .global::<Prefs>()
        .set_show_activity(settings.show_activity);
    window
        .global::<Prefs>()
        .set_show_calendar(settings.show_calendar);
}

fn read_settings(window: &AppWindow) -> Settings {
    Settings {
        theme: at_index(&Theme::ALL, window.global::<Prefs>().get_theme()),
        style: at_index(
            &WidgetStyle::ALL,
            window.global::<Prefs>().get_widget_style(),
        ),
        animations: at_index(&Animations::ALL, window.global::<Prefs>().get_animations()),
        text_size: at_index(&TextSize::ALL, window.global::<Prefs>().get_text_size()),
        show_avatars: window.global::<Prefs>().get_show_avatars(),
        notifications: window.global::<Prefs>().get_notifications(),
        notification_content: at_index(
            &NotificationContent::ALL,
            window.global::<Prefs>().get_notification_content(),
        ),
        show_activity: window.global::<Prefs>().get_show_activity(),
        show_calendar: window.global::<Prefs>().get_show_calendar(),
        close_to_tray: window.global::<Prefs>().get_close_to_tray(),
        device_name: None,
        recent_emoji: Vec::new(),
        preferences: b33p_core::Preferences {
            read_receipts: window.global::<Prefs>().get_read_receipts(),
            typing_indicators: window.global::<Prefs>().get_typing_indicators(),
            link_previews: window.global::<Prefs>().get_link_previews(),
            sync_to_phone: window.global::<Prefs>().get_sync_to_phone(),
        },
    }
}

/// Shows a folder of b33p's in the desktop's file manager.
fn open_folder(dir: Option<std::path::PathBuf>) {
    let Some(dir) = dir else { return };
    // The log folder only exists once something was logged to it.
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            error = %b33p_core::scrub::scrubbed(&error),
            path = %b33p_core::scrub::scrubbed(&dir.display()),
            "could not make a folder"
        );
    }
    if let Err(error) = open::that_detached(&dir) {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "could not open a folder");
    }
}

/// The open chat's row of the message with this key (see `message_item`), where it is now.
fn row(chat: &ChatState, key: &str) -> Option<usize> {
    let (timestamp, author_id) = key.split_once(':')?;
    chat.row_of(&b33p_core::MessageRef {
        author_id: author_id.to_owned(),
        timestamp: timestamp.parse().ok()?,
    })
}

/// `action` on the message with this key, if it is still there.
fn at_row(chat: &mut ChatState, key: &str, action: fn(&mut ChatState, usize) -> Update) -> Update {
    match row(chat, key) {
        Some(row) => action(chat, row),
        None => Update::default(),
    }
}

/// `draft` with its bytes `from` to `to` replaced by `text` (the offsets clamped to the draft
/// and to character boundaries), and the byte offset just after `text`.
pub(crate) fn splice(draft: &str, text: &str, from: i32, to: i32) -> (String, usize) {
    let at = |offset: i32| {
        let mut at = usize::try_from(offset).unwrap_or(0).min(draft.len());
        while !draft.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let (from, to) = (at(from.min(to)), at(from.max(to)));
    let spliced = [&draft[..from], text, &draft[to..]].concat();
    (spliced, from + text.len())
}

/// Where `choice` sits in its enum's `ALL`, which is the order the settings page lists it in.
fn index_of<T: PartialEq>(all: &[T], choice: T) -> i32 {
    all.iter().position(|c| *c == choice).unwrap_or_default() as i32
}

/// The choice at `index` of its enum's `ALL`, or the default for an index out of range.
fn at_index<T: Copy + Default>(all: &[T], index: i32) -> T {
    usize::try_from(index)
        .ok()
        .and_then(|i| all.get(i).copied())
        .unwrap_or_default()
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
            window.global::<Prefs>().set_settings_open(false);
            window.global::<ChatList>().set_focus_search_request(
                window.global::<ChatList>().get_focus_search_request() + 1,
            );
            true
        }
        (true, false, false) if key.eq_ignore_ascii_case("f") => {
            if shared.chat.borrow().selected().is_none() {
                return false;
            }
            window
                .global::<Panels>()
                .set_chat_search_text(SharedString::new());
            shared.edit(|chat| chat.open_chat_search());
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
    // Asking before something is done: over everything else.
    if window.global::<Account>().get_confirm() != Confirm::None {
        window.global::<Account>().set_confirm(Confirm::None);
        return true;
    }
    let mut chat = shared.chat.borrow_mut();
    if chat.join().is_some() {
        chat.close_join();
    } else if window.global::<Panels>().get_stories_open() {
        if chat.current_story().is_some() {
            chat.close_story();
        } else {
            window.global::<Panels>().set_stories_open(false);
        }
    } else if chat.group_draft().is_some() {
        chat.close_group_draft();
    } else if chat.forwarding() {
        chat.cancel_forward();
    } else if chat.has_selection() {
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
    } else if window.global::<OpenChat>().get_calendar_open() {
        window.global::<OpenChat>().set_calendar_open(false);
    } else if chat.editing().is_some() {
        chat.cancel_edit();
        window.global::<Composer>().set_draft(chat.draft().into());
    } else if chat.replying().is_some() {
        chat.cancel_reply();
    } else if window.global::<Prefs>().get_settings_open() {
        window.global::<Prefs>().set_settings_open(false);
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
/// id first. They change the chat list; the messages only when the open chat closed with it
/// (archiving or marking it unread does).
fn chat_action<A>(
    shared: &Rc<Shared>,
    action: impl Fn(&mut ChatState, &ConversationId, A) -> Option<b33p_core::Command> + 'static,
) -> impl Fn(SharedString, A) + 'static {
    let shared = shared.clone();
    move |id, argument| {
        let id = ConversationId(id.into());
        shared.act(|chat| {
            let open = chat.selected().map(|c| c.id.clone());
            let command = action(chat, &id, argument);
            let update = if chat.selected().map(|c| &c.id) == open.as_ref() {
                Update {
                    conversations: true,
                    ..Update::default()
                }
            } else {
                Update::reset()
            };
            (command, update)
        });
    }
}

/// The start screen's line about what it waits for.
pub(crate) fn sync_status(stage: SyncStage) -> String {
    match stage {
        SyncStage::Starting => "Opening your messages…".into(),
        SyncStage::Connecting => "Connecting to Signal…".into(),
        SyncStage::Transfer(HistoryTransfer::Waiting) => {
            "Waiting for your phone to send your message history…".into()
        }
        SyncStage::Transfer(HistoryTransfer::Downloading) => {
            "Downloading your message history…".into()
        }
        SyncStage::Transfer(HistoryTransfer::Importing { messages }) => {
            format!("Importing your message history… {messages} messages")
        }
        SyncStage::Transfer(HistoryTransfer::Finished) => "Connecting to Signal…".into(),
        SyncStage::CatchingUp { messages: 0 } => "Catching up on your messages…".into(),
        SyncStage::CatchingUp { messages } => {
            format!("Catching up on your messages… {messages} new")
        }
    }
}
