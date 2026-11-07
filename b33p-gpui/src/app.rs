//! The window's root: the error banner, then whichever screen the state is on.

use std::rc::Rc;

use b33p_core::state::{Screen, Update};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, FocusHandle, Focusable, IntoElement, MouseMoveEvent, Render, StyleRefinement,
    Window, div, px,
};

use crate::actions::{
    CloseFront, FocusSearch, NextChat, NextUnreadChat, Pinned, PreviousChat, SearchInChat,
};
use crate::chat_list::ChatListView;
use crate::conversation::ConversationView;
use crate::overlays::Overlays;
use crate::shared::Shared;
use crate::start::StartView;
use crate::theme::{self, Tokens};
use crate::widgets::{icon, icon_button};

pub struct AppView {
    shared: Rc<Shared>,
    start: Entity<StartView>,
    list: Entity<ChatListView>,
    conversation: Entity<ConversationView>,
    overlays: Entity<Overlays>,
    /// The settings page, while settings are open.
    settings: Option<Entity<crate::settings::SettingsView>>,
    focus: FocusHandle,
    /// Dragging the divider: where the pointer was, and the sidebar's width then.
    dragging: Option<(f32, f32)>,
    /// Whether the settings were open when the chat list was last drawn: the list is drawn
    /// cached, and shows it on its button.
    list_showed_settings: bool,
}

impl AppView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = cx.new(|cx| StartView::new(shared.clone(), window, cx));
        let list = cx.new(|cx| ChatListView::new(shared.clone(), window, cx));
        let conversation = cx.new(|cx| ConversationView::new(shared.clone(), window, cx));
        let overlays = cx.new(|_| Overlays::new(shared.clone()));
        let focus = cx.focus_handle();
        // The window's own state, for what the state needs to know about it.
        cx.observe_window_activation(window, |this, window, cx| {
            this.shared.ui.borrow_mut().window_active = window.is_window_active();
            cx.notify();
        })
        .detach();
        shared.ui.borrow_mut().window_active = window.is_window_active();
        Self {
            shared,
            start,
            list,
            conversation,
            overlays,
            settings: None,
            focus,
            dragging: None,
            list_showed_settings: false,
        }
    }

    /// Esc closes what is open, the innermost first. Returns whether there was something.
    fn close_front(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let shared = self.shared.clone();
        // Asking before something is done: over everything else.
        if shared.ui.borrow().confirm != crate::shared::Confirm::None {
            shared.ui.borrow_mut().confirm = crate::shared::Confirm::None;
            cx.notify();
            return true;
        }
        // The emoji or sticker picker is in front of the composer.
        if shared.ui.borrow().composer_popup != crate::composer::Popup::None {
            shared.ui.borrow_mut().composer_popup = crate::composer::Popup::None;
            cx.notify();
            return true;
        }
        let mut chat = shared.chat.borrow_mut();
        let mut update = Update::default();
        if chat.join().is_some() {
            chat.close_join();
        } else if shared.ui.borrow().stories_open {
            if chat.current_story().is_some() {
                chat.close_story();
            } else {
                shared.ui.borrow_mut().stories_open = false;
            }
        } else if chat.group_draft().is_some() {
            chat.close_group_draft();
        } else if chat.forwarding() {
            chat.cancel_forward();
        } else if chat.has_selection() {
            update = chat.clear_selection();
        } else if chat.details().is_some() {
            chat.close_details();
        } else if chat.media().is_some() {
            chat.close_media();
        } else if chat.chat_search().is_some() {
            chat.close_chat_search();
        } else if shared.ui.borrow().calendar_open {
            shared.ui.borrow_mut().calendar_open = false;
        } else if chat.editing().is_some() {
            chat.cancel_edit();
        } else if chat.replying().is_some() {
            chat.cancel_reply();
        } else if shared.ui.borrow().settings_open {
            shared.ui.borrow_mut().settings_open = false;
        } else if chat.searching() {
            update = chat.search("");
            drop(chat);
            self.list
                .update(cx, |list, cx| list.clear_search(window, cx));
            shared.refresh(update, cx);
            return true;
        } else if shared.narrow() && chat.selected().is_some() {
            update = chat.deselect();
        } else {
            return false;
        }
        drop(chat);
        shared.refresh(update, cx);
        true
    }

    /// Picks the chat the shortcut names. Returns whether there was one.
    fn open(
        &self,
        cx: &mut Context<Self>,
        pick: impl FnOnce(&mut b33p_core::state::ChatState) -> Option<Option<b33p_core::Command>>,
    ) {
        self.shared.act(cx, |chat| match pick(chat) {
            Some(command) => (command, Update::reset()),
            None => (None, Update::default()),
        });
    }
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let narrow = window.viewport_size().width < px(640.);
        shared.ui.borrow_mut().narrow = narrow;
        {
            let mut ui = shared.ui.borrow_mut();
            ui.window_active = window.is_window_active();
            ui.window_visible = true;
        }
        let screen = shared.chat.borrow().screen().clone();
        let banner = self.banner(&t);
        let content = match screen {
            Screen::Chats => self.chats(narrow, &t, window, cx).into_any_element(),
            _ => self.start.clone().into_any_element(),
        };
        div()
            .id("app")
            .relative()
            .key_context("App")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(t.surface)
            .text_color(t.text)
            .font_family(gpui_kit::component::Theme::global(cx).font_family.clone())
            .text_size(theme::body(shared.settings.borrow().text_size.scale()))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.shared.ui.borrow_mut().settings_open = false;
                this.list
                    .update(cx, |list, cx| list.focus_search(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SearchInChat, _, cx| {
                if this.shared.chat.borrow().selected().is_some() {
                    this.shared.edit(cx, |chat| chat.open_chat_search());
                }
            }))
            .on_action(cx.listener(|this, _: &NextChat, _, cx| {
                if !this.list.update(cx, |list, cx| list.move_cursor(1, cx)) {
                    this.open(cx, |chat| chat.select_relative(1));
                }
            }))
            .on_action(cx.listener(|this, _: &PreviousChat, _, cx| {
                if !this.list.update(cx, |list, cx| list.move_cursor(-1, cx)) {
                    this.open(cx, |chat| chat.select_relative(-1));
                }
            }))
            .on_action(cx.listener(|this, _: &NextUnreadChat, _, cx| {
                this.open(cx, |chat| chat.select_next_unread());
            }))
            .on_action(cx.listener(|this, action: &Pinned, _, cx| {
                let n = action.0;
                this.open(cx, |chat| chat.select_pinned(n));
            }))
            .on_action(cx.listener(|this, _: &CloseFront, window, cx| {
                this.close_front(window, cx);
            }))
            .children(banner)
            .child(div().flex_1().min_h_0().child(content))
            .child(self.overlays.clone())
    }
}

impl AppView {
    /// The error, which slides down from the top while there is one.
    fn banner(&self, t: &Tokens) -> Option<gpui_kit::AnyElement> {
        let chat = self.shared.chat.borrow();
        let message = chat.error()?;
        let can_retry = chat.can_retry_error();
        let shared = self.shared.clone();
        let retry_shared = self.shared.clone();
        Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .pl(px(theme::SPACE_L))
                .pr(px(theme::SPACE_S))
                .py(px(6.))
                .min_h(px(36.))
                .bg(t.danger)
                .text_color(t.on_accent)
                .child(icon("circle-alert").size(px(theme::ICON)))
                .child(div().flex_1().child(message))
                .when(can_retry, |row| {
                    row.child(
                        Button::new("retry-error")
                            .ghost()
                            .small()
                            .icon(icon("rotate-cw"))
                            .label("Try again")
                            .on_click(move |_, _, cx| {
                                retry_shared.command(cx, |chat| chat.retry_error())
                            }),
                    )
                })
                .child(icon_button(
                    "dismiss-error",
                    "x",
                    "Dismiss",
                    move |_, _, cx| shared.edit(cx, |chat| chat.dismiss_error()),
                ))
                .into_any_element(),
        )
    }

    fn chats(
        &mut self,
        narrow: bool,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let shared = self.shared.clone();
        let (settings_open, selected) = {
            let chat = shared.chat.borrow();
            (shared.ui.borrow().settings_open, chat.selected().is_some())
        };
        let show_list = !narrow || (!selected && !settings_open);
        let sidebar = shared.ui.borrow().sidebar_width;
        let divider = div()
            .id("sidebar-divider")
            .flex_none()
            .w(px(1.))
            .h_full()
            .bg(t.divider)
            .cursor_col_resize()
            .child(
                // A wider target than the line, to take hold of.
                div().absolute().top_0().bottom_0().left(px(-3.)).w(px(7.)),
            )
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(move |this, event: &gpui_kit::MouseDownEvent, _, _| {
                    let x: f32 = event.position.x.into();
                    let width = this.shared.ui.borrow().sidebar_width;
                    this.dragging = Some((x, width));
                }),
            );
        if !settings_open {
            // Made anew when settings open again.
            self.settings = None;
        }
        // Drawn cached: the list is the costliest part of a frame after the messages (the search
        // field, the buttons and the rows are laid out and painted again), and most frames, those
        // of scrolling and typing, change nothing in it. What it reads from the state arrives
        // with a refresh of the window; what it reads from the window alone is noted here.
        if self.list_showed_settings != settings_open {
            self.list_showed_settings = settings_open;
            self.list.update(cx, |_, cx| cx.notify());
        }
        let list = self.list.clone().cached(if narrow {
            StyleRefinement::default().flex_1().w_full().h_full()
        } else {
            StyleRefinement::default()
                .flex_none()
                .w(px(sidebar))
                .h_full()
        });
        let main: gpui_kit::AnyElement = if settings_open {
            crate::settings::page(&mut self.settings, &self.shared, window, cx)
        } else if selected {
            self.conversation.clone().into_any_element()
        } else if narrow {
            div().into_any_element()
        } else {
            empty_state(t)
        };
        div()
            .size_full()
            .flex()
            .flex_row()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some((from, width)) = this.dragging {
                    if event.pressed_button != Some(gpui_kit::MouseButton::Left) {
                        this.dragging = None;
                        return;
                    }
                    let x: f32 = event.position.x.into();
                    this.shared.ui.borrow_mut().sidebar_width =
                        (width + x - from).clamp(220., 480.);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = None),
            )
            .when(show_list, |row| row.child(list))
            .when(show_list && !narrow, |row| row.child(divider))
            .when(!(narrow && show_list), |row| row.child(main))
            .when(false, |row| row.w(px(sidebar)))
    }
}

/// What shows where a chat would, before one is picked.
fn empty_state(t: &Tokens) -> gpui_kit::AnyElement {
    let keys = if cfg!(target_os = "macos") {
        "⌘K searches, ⌥↑ and ⌥↓ go through the chats."
    } else {
        "Ctrl+K searches, Alt+↑ and Alt+↓ go through the chats."
    };
    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACE_M))
        .child(
            gpui_kit::img("b33p.svg")
                .size(px(88.))
                .opacity(theme::TERTIARY),
        )
        .child(
            div()
                .text_center()
                .text_color(t.text_tertiary)
                .child("Pick a chat on the left,\nor search for someone to start a new one."),
        )
        .child(
            div()
                .text_size(px(theme::BODY * theme::SMALL))
                .text_color(t.text_faint)
                .child(keys),
        )
        .into_any_element()
}
