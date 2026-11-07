//! The list of chats: its header, search, folders and rows.

use std::rc::Rc;

use b33p_core::state::{self, ChatState, ConversationView, PreviewKind, Update};
use b33p_core::{AttachmentKind, ConversationId, now_millis};
use gpui_kit::Role;
use gpui_kit::component::Selectable as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::*;
use gpui_kit::{
    ClickEvent, Context, Entity, FontWeight, IntoElement, Render, SharedString, Subscription,
    UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, avatar, avatar_color, icon, icon_button};

/// What a chat's menu entry does: to the conversation it is for.
pub type MenuAction = Rc<dyn Fn(&Rc<Shared>, &ConversationId, &mut gpui_kit::App)>;

/// A row's height.
const ROW: f32 = 64.0;

/// How long to mute for, in seconds; below 0 is for good.
pub const MUTE_CHOICES: [(&str, i64); 4] = [
    ("For 1 hour", 3600),
    ("For 8 hours", 28800),
    ("For 1 week", 604800),
    ("Always", -1),
];

pub struct ChatListView {
    shared: Rc<Shared>,
    pub search: Entity<InputState>,
    /// The result the arrow keys point at while searching.
    cursor: usize,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl ChatListView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search or start a chat"));
        let subscription = cx.subscribe_in(&search, window, Self::search_event);
        Self {
            shared,
            search,
            cursor: 0,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: vec![subscription],
        }
    }

    fn search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let text = input.read(cx).value().to_string();
                self.cursor = 0;
                self.shared.redraw(cx, |chat| chat.search(&text));
            }
            InputEvent::PressEnter { .. } => {
                let text = input.read(cx).value().to_string();
                let target = {
                    let chat = self.shared.chat.borrow();
                    chat.conversations()
                        .nth(self.cursor)
                        .map(|c| c.conversation.id.clone())
                };
                match target {
                    Some(id) if !text.is_empty() => self.shared.select(cx, &id),
                    _ if state::looks_like_number(&text) => self.find_number(&text, cx),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn find_number(&self, number: &str, cx: &mut Context<Self>) {
        self.shared.command(cx, |chat| {
            chat.find_number(number, b33p_core::AfterFinding::Open)
        });
    }

    /// Moves the keyboard cursor through the search results.
    pub fn move_cursor(&mut self, by: isize, cx: &mut Context<Self>) -> bool {
        let count = self.shared.chat.borrow().conversations().count();
        if count == 0 || self.search.read(cx).value().is_empty() {
            return false;
        }
        self.cursor = (self.cursor as isize + by).clamp(0, count as isize - 1) as usize;
        self.scroll
            .scroll_to_item(self.cursor, gpui_kit::ScrollStrategy::Nearest);
        cx.notify();
        true
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    /// Empties the search field (the state's search is cleared by the caller).
    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.cursor = 0;
    }
}

/// The icon before a chat's preview.
fn preview_icon(kind: Option<PreviewKind>) -> Option<&'static str> {
    Some(match kind? {
        PreviewKind::Timer => "timer",
        PreviewKind::Attachment(kind) => match kind {
            AttachmentKind::Image | AttachmentKind::ViewOnce => "image",
            AttachmentKind::Sticker => "sticker",
            AttachmentKind::Video => "film",
            AttachmentKind::Audio => "headphones",
            AttachmentKind::VoiceNote => "mic",
            AttachmentKind::File | AttachmentKind::LongText => "paperclip",
        },
    })
}

impl Render for ChatListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let (narrow, sidebar_width) = {
            let ui = shared.ui.borrow();
            (ui.narrow, ui.sidebar_width)
        };
        let chat = shared.chat.borrow();
        let showing_archived = chat.showing_archived();
        let (archived, archived_unread) = chat.archived();
        let count = chat.conversations().count();
        let search_text = self.search.read(cx).value().to_string();
        let is_number = state::looks_like_number(&search_text);
        let connection = match chat.connection() {
            state::Connection::Online => None,
            state::Connection::CatchingUp => Some((false, "Catching up…")),
            state::Connection::Offline => Some((true, "Not connected · trying again")),
        };
        let unviewed = chat.unviewed_stories();
        let settings_open = shared.ui.borrow().settings_open;

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h(px(theme::HEADER))
            .px(px(theme::SPACE_M))
            .gap(px(theme::SPACE_XS))
            .when(showing_archived, |header| {
                header.child(icon_button(
                    "back-to-chats",
                    "arrow-left",
                    "Back to chats",
                    {
                        let shared = shared.clone();
                        move |_, _, cx| shared.redraw(cx, |chat| chat.show_archived(false))
                    },
                ))
            })
            .child(
                div()
                    .flex_1()
                    .text_size(px(theme::BODY * theme::HEADING))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if showing_archived {
                        "Archived"
                    } else {
                        "Chats"
                    }),
            )
            .child(
                icon_button(
                    "stories",
                    "circle-dashed",
                    if unviewed > 0 {
                        "Stories, new"
                    } else {
                        "Stories"
                    },
                    {
                        let shared = shared.clone();
                        move |_, _, cx| {
                            shared.ui.borrow_mut().stories_open = true;
                            shared.refresh(Update::default(), cx);
                        }
                    },
                )
                .when(unviewed > 0, |button| button.selected(true)),
            )
            .child(icon_button("new-group", "plus", "New group", {
                let shared = shared.clone();
                move |_, _, cx| shared.edit(cx, |chat| chat.open_group_draft(None))
            }))
            .child(icon_button("join-group", "link", "Join a group by link", {
                let shared = shared.clone();
                move |_, _, cx| shared.command(cx, |chat| chat.open_join(None))
            }))
            .child(
                icon_button("settings", "settings", "Settings", {
                    let shared = shared.clone();
                    move |_, _, cx| {
                        let open = !shared.ui.borrow().settings_open;
                        shared.ui.borrow_mut().settings_open = open;
                        shared.refresh(Update::default(), cx);
                    }
                })
                .selected(settings_open),
            );

        let status = connection.map(|(offline, text)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(theme::SPACE_M))
                .pb(px(6.))
                .text_size(px(theme::BODY * theme::BODY_SMALL))
                .text_color(t.text_secondary)
                .child(if offline {
                    icon("wifi-off")
                        .size(px(theme::ICON_SMALL))
                        .into_any_element()
                } else {
                    Spinner::new().small().into_any_element()
                })
                .child(text)
        });

        let search = div()
            .px(px(theme::SPACE_M))
            .pb(px(theme::SPACE_S))
            .child(Input::new(&self.search).cleanable(true));

        let number = is_number.then(|| {
            div().px(px(theme::SPACE_M)).pb(px(theme::SPACE_S)).child(
                Button::new("message-number")
                    .label(format!("Message {search_text}"))
                    .debug_selector({
                        let label = format!("Message {search_text}");
                        move || label.clone()
                    })
                    .on_click({
                        let number = search_text.clone();
                        cx.listener(move |this, _, _, cx| this.find_number(&number, cx))
                    }),
            )
        });

        let folders = (!chat.folders().is_empty() && !showing_archived && search_text.is_empty())
            .then(|| {
                let current = chat.folder();
                let tab =
                    |id: usize, label: String, unread: u32, selected: bool, at: Option<usize>| {
                        let shared = shared.clone();
                        // As a screen reader hears it: the unread count with the name.
                        let said = match (label.as_str(), unread) {
                            ("All", _) => "All chats".to_owned(),
                            (name, 0) => name.to_owned(),
                            (name, unread) => format!("{name}, {unread} unread"),
                        };
                        let mut tab = div()
                            .id(("folder", id))
                            .speaks(Role::Tab, said)
                            .aria_selected(selected)
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .px(px(10.))
                            .py(px(4.))
                            .rounded_full()
                            .cursor_pointer()
                            .text_size(px(theme::BODY * theme::BODY_SMALL))
                            .when(selected, |tab| tab.bg(t.accent).text_color(t.on_accent))
                            .when(!selected, |tab| {
                                tab.text_color(t.text_secondary).hover(|s| s.bg(t.hover))
                            })
                            .on_click(move |_, _, cx| {
                                shared.redraw(cx, |chat| chat.show_folder(at));
                            })
                            .child(label);
                        if unread > 0 {
                            tab = tab.child(
                                div()
                                    .text_size(px(theme::BODY * theme::CAPTION))
                                    .font_weight(FontWeight::BOLD)
                                    .child(unread.to_string()),
                            );
                        }
                        tab
                    };
                let mut row = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(theme::SPACE_XS))
                    .px(px(theme::SPACE_M))
                    .pb(px(6.))
                    .child(tab(usize::MAX, "All".into(), 0, current.is_none(), None));
                for (at, folder) in chat.folders().iter().enumerate() {
                    row = row.child(tab(
                        at,
                        folder.name.clone(),
                        chat.folder_unread(at),
                        current == Some(at),
                        Some(at),
                    ));
                }
                row
            });

        let archive_entry =
            (archived > 0 && !showing_archived && search_text.is_empty()).then(|| {
                let shared = shared.clone();
                div().px(px(theme::SPACE_S)).child(
                    div()
                        .id("archived-chats")
                        .speaks(
                            Role::Button,
                            if archived_unread > 0 {
                                format!("Archived chats, {archived}, {archived_unread} unread")
                            } else {
                                format!("Archived chats, {archived}")
                            },
                        )
                        .h(px(40.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACE_M))
                        .px(px(10.))
                        .rounded(px(theme::RADIUS_M))
                        .cursor_pointer()
                        .hover(|s| s.bg(t.hover))
                        .on_click(move |_, _, cx| {
                            shared.redraw(cx, |chat| chat.show_archived(true))
                        })
                        .child(
                            div()
                                .w(px(40.))
                                .flex()
                                .justify_center()
                                .child(icon("archive").size(px(theme::ICON))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_color(t.text_secondary)
                                .child("Archived chats"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::BODY * theme::SMALL))
                                .text_color(t.text_tertiary)
                                .child(if archived_unread > 0 {
                                    format!("{archived} · {archived_unread} unread")
                                } else {
                                    archived.to_string()
                                }),
                        ),
                )
            });

        let empty = if count == 0 && !is_number {
            if !search_text.is_empty() {
                Some((
                    "search",
                    "No chats found. Type a phone number to message someone new.",
                ))
            } else if showing_archived {
                Some(("archive", "No archived chats."))
            } else {
                None
            }
        } else {
            None
        };
        let empty = empty.map(|(name, text)| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(theme::SPACE_S))
                .p(px(theme::SPACE_XL))
                .text_color(t.text_tertiary)
                .child(icon(name).size(px(28.)))
                .child(div().text_center().child(text))
        });
        drop(chat);

        let list = uniform_list(
            "chat-list",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let t = theme::tokens(cx);
                let searching = !this.search.read(cx).value().is_empty();
                let rows: Vec<_> = {
                    let chat = this.shared.chat.borrow();
                    let pinned = chat.pinned().len();
                    let list_time = state::list_time_formatter();
                    let selected = chat.selected().map(|c| c.id.clone());
                    chat.conversations()
                        .enumerate()
                        .skip(range.start)
                        .take(range.len())
                        .map(|(index, view)| {
                            row(
                                index,
                                &view,
                                pinned,
                                &chat,
                                &this.shared,
                                &list_time,
                                selected.as_ref() == Some(&view.conversation.id),
                                searching && index == this.cursor,
                                &t,
                            )
                        })
                        .collect()
                };
                rows
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .px(px(theme::SPACE_S));

        div()
            .flex()
            .flex_col()
            .h_full()
            .bg(t.surface_raised)
            .when(!narrow, |pane| pane.w(px(sidebar_width)).flex_none())
            .when(narrow, |pane| pane.flex_1().w_full())
            .child(header)
            .children(status)
            .child(search)
            .children(number)
            .children(folders)
            .children(archive_entry)
            .children(empty)
            .child(list)
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    index: usize,
    view: &ConversationView,
    pinned_count: usize,
    chat: &ChatState,
    shared: &Rc<Shared>,
    list_time: &dyn Fn(u64) -> String,
    selected: bool,
    pointed_at: bool,
    t: &Tokens,
) -> gpui_kit::AnyElement {
    let c = view.conversation;
    let id = c.id.clone();
    let typing = state::typing_label(&view.typing);
    let preview = view.preview();
    let unread = view.unread;
    let muted = view.muted();
    let marked_unread = view.marked_unread();
    let pinned = view.pinned();
    let blocked = c.settings.blocked;
    let request = chat.is_request(c);
    let time = view
        .last_message
        .map(|m| list_time(m.timestamp))
        .unwrap_or_default();
    let picture = shared.images.avatar(&format!("chat:{}", id.0), view.avatar);
    let can_up = c.settings.pinned.is_some_and(|at| at > 0);
    let can_down = c
        .settings
        .pinned
        .is_some_and(|at| (at as usize) + 1 < pinned_count);
    let archived = c.settings.archived;
    let has_unread = unread > 0 || marked_unread;
    let label = format!(
        "{}{}{}{}{}, {}",
        c.title,
        if unread > 0 {
            format!(", {unread} unread")
        } else if marked_unread {
            ", unread".into()
        } else {
            String::new()
        },
        if request { ", message request" } else { "" },
        if pinned { ", pinned" } else { "" },
        if muted { ", muted" } else { "" },
        if typing.is_empty() { &preview } else { &typing },
    );

    let click_shared = shared.clone();
    let click_id = id.clone();
    let menu_shared = shared.clone();
    let menu_id = id.clone();
    let t = *t;
    let kind_icon = if blocked || !typing.is_empty() {
        None
    } else {
        preview_icon(view.preview_kind())
    };
    div()
        .id(("chat", index))
        .h(px(ROW))
        .flex_none()
        .rounded(px(theme::RADIUS_M))
        .cursor_pointer()
        .when(selected, |row| row.bg(t.selected))
        .when(!selected, |row| row.hover(|s| s.bg(t.hover)))
        .when(pointed_at, |row| row.border_1().border_color(t.accent))
        .speaks(Role::ListItem, SharedString::from(label))
        .aria_selected(selected)
        .on_click(move |_, _, cx| click_shared.select(cx, &click_id))
        .context_menu(move |menu, _window, _cx| {
            let shared = menu_shared.clone();
            let id = menu_id.clone();
            let mut menu = menu;
            let item = |label: &'static str, run: MenuAction| {
                let shared = shared.clone();
                let id = id.clone();
                PopupMenuItem::new(label).on_click(move |_, _, cx| run(&shared, &id, cx))
            };
            menu = menu.item(item(
                if pinned { "Unpin" } else { "Pin" },
                Rc::new(|s, id, cx| s.chat_action(cx, id, |chat, id| chat.toggle_pin(id))),
            ));
            if can_up {
                menu = menu.item(item(
                    "Move up",
                    Rc::new(|s, id, cx| s.chat_action(cx, id, |chat, id| chat.move_pin_by(id, -1))),
                ));
            }
            if can_down {
                menu = menu.item(item(
                    "Move down",
                    Rc::new(|s, id, cx| s.chat_action(cx, id, |chat, id| chat.move_pin_by(id, 1))),
                ));
            }
            menu = menu.item(item(
                if has_unread {
                    "Mark as read"
                } else {
                    "Mark as unread"
                },
                Rc::new(move |s, id, cx| {
                    s.chat_action(cx, id, |chat, id| chat.mark_unread(id, !has_unread))
                }),
            ));
            if muted {
                menu = menu.item(item(
                    "Unmute",
                    Rc::new(|s, id, cx| s.chat_action(cx, id, |chat, id| chat.mute(id, None))),
                ));
            } else {
                for (label, seconds) in MUTE_CHOICES {
                    menu = menu.item(item(
                        label,
                        Rc::new(move |s, id, cx| {
                            s.chat_action(cx, id, |chat, id| {
                                let until = if seconds < 0 {
                                    u64::MAX
                                } else {
                                    now_millis() + seconds as u64 * 1000
                                };
                                chat.mute(id, Some(until))
                            })
                        }),
                    ));
                }
            }
            menu = menu.separator();
            menu = menu.item(item(
                if blocked { "Unblock" } else { "Block" },
                Rc::new(move |s, id, cx| {
                    s.chat_action(cx, id, |chat, id| chat.block(id, !blocked))
                }),
            ));
            menu.item(item(
                if archived { "Unarchive" } else { "Archive" },
                Rc::new(move |s, id, cx| {
                    s.chat_action(cx, id, |chat, id| chat.archive(id, !archived))
                }),
            ))
        })
        .child(
            div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_M))
                .p(px(10.))
                .child(avatar(
                    44.,
                    &state::initials(&c.title),
                    avatar_color(&id),
                    picture,
                    &t,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(if unread > 0 {
                                            FontWeight::BOLD
                                        } else {
                                            FontWeight::SEMIBOLD
                                        })
                                        .child(c.title.clone()),
                                )
                                .when(muted, |row| {
                                    row.child(
                                        icon("bell-off")
                                            .size(px(theme::ICON_SMALL))
                                            .text_color(t.text_tertiary),
                                    )
                                })
                                .when(pinned, |row| {
                                    row.child(
                                        icon("pin")
                                            .size(px(theme::ICON_SMALL))
                                            .text_color(t.text_tertiary),
                                    )
                                })
                                .child(
                                    div()
                                        .text_size(px(theme::BODY * theme::CAPTION))
                                        .text_color(if unread > 0 {
                                            t.accent
                                        } else {
                                            t.text_tertiary
                                        })
                                        .child(time),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(theme::BODY * theme::BODY_SMALL))
                                .when(request, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(t.accent)
                                            .child("Message request ·"),
                                    )
                                })
                                .children(kind_icon.map(|name| {
                                    icon(name)
                                        .size(px(theme::ICON_SMALL))
                                        .text_color(t.text_tertiary)
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(if !typing.is_empty() {
                                            t.accent
                                        } else {
                                            t.text_secondary
                                        })
                                        .child(if blocked {
                                            "Blocked".to_owned()
                                        } else if !typing.is_empty() {
                                            typing
                                        } else {
                                            preview
                                        }),
                                )
                                .when(unread == 0 && marked_unread, |row| {
                                    row.child(
                                        div().flex_none().size(px(10.)).rounded_full().bg(t.accent),
                                    )
                                })
                                .when(unread > 0, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .h(px(18.))
                                            .min_w(px(18.))
                                            .px(px(5.))
                                            .rounded_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .bg(if muted { t.text_faint } else { t.accent })
                                            .text_color(t.on_accent)
                                            .text_size(px(theme::BODY * theme::CAPTION))
                                            .font_weight(FontWeight::BOLD)
                                            .child(unread.to_string()),
                                    )
                                }),
                        ),
                ),
        )
        .into_any_element()
}

#[allow(dead_code)]
fn _unused(_: ClickEvent) {}
