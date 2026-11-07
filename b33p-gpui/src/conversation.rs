//! The open chat: its header, messages and composer.

use std::rc::Rc;

use b33p_core::state::{self, Update};
use gpui_kit::Role;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::message_scroller::MessageScroller;
use gpui_kit::component::{Selectable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, FocusHandle, Focusable as _, FontWeight, IntoElement, Render, Window, div, px,
};

use crate::actions::{CloseFront, CopyMessages};
use crate::bubble;
use crate::chat_list::MUTE_CHOICES;
use crate::composer::ComposerView;
use crate::message_menu;
use crate::shared::Shared;
use crate::text::TextSelection;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, avatar, avatar_color, divider, icon, icon_button};

pub struct ConversationView {
    shared: Rc<Shared>,
    pub composer: Entity<ComposerView>,
    /// What covers the messages while it is open: search, media, details, the calendar.
    panels: crate::panels::Panels,
    /// The bar beside the messages that shows when the chat was active.
    strip: Entity<crate::activity::ActivityStrip>,
    /// Has the keys while text of a message is selected, so that Ctrl+C copies it.
    messages_focus: FocusHandle,
}

impl ConversationView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| ComposerView::new(shared.clone(), window, cx));
        let panels = crate::panels::Panels::new(&shared, window, cx);
        let strip = cx.new(|cx| crate::activity::ActivityStrip::new(shared.clone(), cx));
        let messages_focus = cx.focus_handle();
        Self {
            shared,
            composer,
            panels,
            strip,
            messages_focus,
        }
    }
}

impl Render for ConversationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let narrow = shared.narrow();

        // Older messages come in as the top is neared.
        {
            let top = shared.scroller.read(cx).logical_scroll_top().item_ix;
            if top < 4 && shared.chat.borrow().can_load_older() {
                let command = shared.chat.borrow_mut().load_older();
                if let Some(command) = command {
                    shared.commands.send(command);
                }
            }
        }

        let header = self.header(&t, narrow);
        let strip_shown = crate::activity::ActivityStrip::shown(&self.strip, &shared, cx);
        let messages = self.messages(window, cx);

        let root = div()
            .id("conversation")
            .group("conversation")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.surface)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(if strip_shown {
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .child(div().flex_1().min_w_0().h_full().child(messages))
                            .child(self.strip.clone())
                            .into_any_element()
                    } else {
                        messages.into_any_element()
                    })
                    // A panel over the messages, while one is open.
                    .children(self.panels.covering(&shared, cx)),
            )
            .child(
                // The bar of what to do with picked messages covers the composer, or the bars
                // that stand in for it (a request, a blocked chat, ...).
                div()
                    .relative()
                    .flex_none()
                    .child(self.composer.clone())
                    .children(message_menu::selection_bar(&shared, &t)),
            );
        // Files dragged onto the chat are attached.
        crate::composer::accepting_files(root, &shared, &t)
    }
}

impl ConversationView {
    /// The messages, a list that draws only the rows on screen.
    fn messages(&self, window: &Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let shared = self.shared.clone();
        // Messages that came while scrolled away from the end are counted for the button that
        // goes there; scrolled to it again, they are seen.
        if !shared.scroller.read(cx).is_scrolled_up() {
            shared.unseen.set(0);
        }
        let unseen = shared.unseen.get();
        cx.default_global::<TextSelection>().focus = Some(self.messages_focus.clone());
        // Writing in the composer drops the selection of text, so that Ctrl+C there copies the
        // composer's own.
        let writing = self
            .composer
            .read(cx)
            .input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if writing {
            TextSelection::clear(cx);
        }
        let list = MessageScroller::new("messages", shared.scroller.clone(), {
            let shared = shared.clone();
            move |index, _window, cx| bubble::row(index, &shared, cx)
        })
        // The rows carry their own spacing: tight within a run, a little more between runs.
        .with_row_style(gpui_kit::StyleRefinement::default().pb(px(0.)).px(px(0.)))
        .with_jump_button_label(if unseen > 0 {
            format!("Newest messages, {unseen} new")
        } else {
            "Newest messages".to_owned()
        })
        .with_jump_button_renderer(move |button| {
            if unseen > 0 {
                button.label(unseen.to_string())
            } else {
                button
            }
        });
        div()
            .id("messages-area")
            .size_full()
            .key_context("Messages")
            .track_focus(&self.messages_focus)
            // Keys that are the messages' own while they have the focus: Ctrl+C copies the text
            // selected, or else the picked messages; Esc drops the selection of text, and
            // otherwise is for whatever is open.
            .on_action(cx.listener(|this, _: &CopyMessages, _, cx| {
                if TextSelection::copy(cx) {
                    return;
                }
                let text = this.shared.chat.borrow().selected_text();
                if text.is_empty() {
                    cx.propagate();
                } else {
                    message_menu::copy(text, cx);
                    this.shared.redraw(cx, |chat| chat.clear_selection());
                }
            }))
            .on_action(cx.listener(|_, _: &CloseFront, _, cx| {
                if TextSelection::clear(cx) {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            // A click on the messages that isn't on text drops the selection of text, and
            // leaves the composer its focus.
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                if TextSelection::clear(cx) {
                    window.refresh();
                }
            })
            .child(list)
            .into_any_element()
    }

    fn header(&self, t: &Tokens, narrow: bool) -> gpui_kit::AnyElement {
        let shared = self.shared.clone();
        let chat = shared.chat.borrow();
        let Some(view) = chat.selected_view() else {
            return div().into_any_element();
        };
        let c = view.conversation;
        let id = c.id.clone();
        let typing = state::typing_label(&chat.typing(&c.id));
        let picture = shared.images.avatar(&format!("chat:{}", id.0), view.avatar);
        let timer = (c.expire_timer > 0).then(|| b33p_core::format_timer(c.expire_timer));
        let subtitle = c.subtitle.clone();
        let pinned = view.pinned();
        let muted = view.muted();
        let marked_unread = view.marked_unread() || view.unread > 0;
        let blocked = c.settings.blocked;
        let archived = c.settings.archived;
        let show_calendar = shared.settings.borrow().show_calendar;
        let calendar_open = shared.ui.borrow().calendar_open;
        let title = c.title.clone();
        let initials = state::initials(&c.title);
        drop(chat);

        let menu_shared = shared.clone();
        let menu_id = id.clone();
        div()
            .flex_none()
            .h(px(theme::HEADER))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_M))
                    .pl(px(if narrow {
                        theme::SPACE_XS
                    } else {
                        theme::SPACE_L
                    }))
                    .pr(px(theme::SPACE_M))
                    .when(narrow, |row| {
                        row.child(icon_button("back", "arrow-left", "Back", {
                            let shared = shared.clone();
                            move |_, _, cx| shared.redraw(cx, |chat| chat.deselect())
                        }))
                    })
                    .child(
                        div()
                            .id("chat-details")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACE_M))
                            .cursor_pointer()
                            .speaks(Role::Button, format!("Details of {title}"))
                            .on_click({
                                let shared = shared.clone();
                                move |_, _, cx| shared.command(cx, |chat| chat.open_details())
                            })
                            .child(avatar(36., &initials, avatar_color(&id), picture, t))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(theme::BODY * theme::TITLE))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(px(theme::SPACE_XS))
                                            .text_size(px(theme::BODY * theme::SMALL))
                                            .text_color(if typing.is_empty() {
                                                t.text_tertiary
                                            } else {
                                                t.accent
                                            })
                                            .map(|row| {
                                                if !typing.is_empty() {
                                                    return row.child(typing.clone());
                                                }
                                                let row = row.when(!subtitle.is_empty(), |row| {
                                                    row.child(
                                                        div().truncate().child(subtitle.clone()),
                                                    )
                                                });
                                                match &timer {
                                                    Some(label) => row
                                                        .when(!subtitle.is_empty(), |row| {
                                                            row.child("·")
                                                        })
                                                        .child(icon("timer").size(px(12.)))
                                                        .child(label.clone()),
                                                    None => row,
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .when(show_calendar, |row| {
                        row.child(
                            icon_button("calendar", "calendar", "Activity calendar", {
                                let shared = shared.clone();
                                move |_, _, cx| {
                                    let open = !shared.ui.borrow().calendar_open;
                                    shared.ui.borrow_mut().calendar_open = open;
                                    shared.refresh(Update::default(), cx);
                                }
                            })
                            .selected(calendar_open),
                        )
                    })
                    .child(icon_button("search-chat", "search", "Search this chat", {
                        let shared = shared.clone();
                        move |_, _, cx| shared.edit(cx, |chat| chat.open_chat_search())
                    }))
                    .child(
                        Button::new("more")
                            .ghost()
                            .small()
                            .icon(icon("ellipsis-vertical"))
                            .tooltip("More")
                            .dropdown_menu(move |menu, _, _| {
                                chat_menu(
                                    menu,
                                    &menu_shared,
                                    &menu_id,
                                    pinned,
                                    muted,
                                    marked_unread,
                                    blocked,
                                    archived,
                                )
                            }),
                    ),
            )
            .child(divider(t))
            .into_any_element()
    }
}

/// The chat header's "More" menu.
#[allow(clippy::too_many_arguments)]
fn chat_menu(
    menu: gpui_kit::component::menu::PopupMenu,
    shared: &Rc<Shared>,
    id: &b33p_core::ConversationId,
    pinned: bool,
    muted: bool,
    unread: bool,
    blocked: bool,
    archived: bool,
) -> gpui_kit::component::menu::PopupMenu {
    let item = |label: &'static str, run: crate::chat_list::MenuAction| {
        let shared = shared.clone();
        let id = id.clone();
        PopupMenuItem::new(label).on_click(move |_, _, cx| run(&shared, &id, cx))
    };
    let mut menu = menu.item(item(
        if pinned { "Unpin" } else { "Pin" },
        Rc::new(|s, id, cx| s.chat_action(cx, id, |chat, id| chat.toggle_pin(id))),
    ));
    menu = menu.item(item(
        if unread {
            "Mark as read"
        } else {
            "Mark as unread"
        },
        Rc::new(move |s, id, cx| s.chat_action(cx, id, |chat, id| chat.mark_unread(id, !unread))),
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
                            b33p_core::now_millis() + seconds as u64 * 1000
                        };
                        chat.mute(id, Some(until))
                    })
                }),
            ));
        }
    }
    menu = menu.item(item(
        "Details",
        Rc::new(|s, _, cx| s.command(cx, |chat| chat.open_details())),
    ));
    menu = menu.item(item(
        "Media, files and links",
        Rc::new(|s, _, cx| s.command(cx, |chat| chat.open_media())),
    ));
    menu = menu.separator();
    menu = menu.item(item(
        if blocked { "Unblock" } else { "Block" },
        Rc::new(move |s, id, cx| s.chat_action(cx, id, |chat, id| chat.block(id, !blocked))),
    ));
    menu.item(item(
        if archived { "Unarchive" } else { "Archive" },
        Rc::new(move |s, id, cx| s.chat_action(cx, id, |chat, id| chat.archive(id, !archived))),
    ))
}
