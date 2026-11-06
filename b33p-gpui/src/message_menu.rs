//! What can be done with a message: the buttons that show while the pointer rests on it, its
//! menu (also on a right click), the reaction picker, and the bar over the composer while
//! messages are picked.

use std::rc::Rc;

use b33p_core::state::Update;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::Focusable as _;
use gpui_kit::{
    Anchor, AnyElement, App, ClipboardItem, Context, FontWeight, SharedString, Window, div, px,
};

use crate::shared::{Confirm, Shared};
use crate::text::TextSelection;
use crate::theme::{self, Tokens};
use crate::widgets::icon;

/// What the menu of a message offers, as the message was when it was drawn.
#[derive(Clone)]
pub struct Info {
    pub index: usize,
    /// The message's identity, for whose text is selected.
    pub key: SharedString,
    pub notice: bool,
    pub deleted: bool,
    pub selected: bool,
    /// Its text, to copy (empty for a deleted message).
    pub text: String,
    pub can_forward: bool,
    pub can_edit: bool,
    /// An attachment that can be saved, with its id.
    pub save: Option<b33p_core::AttachmentId>,
    /// A message that didn't go out and can be sent again.
    pub can_send_again: bool,
}

/// Puts the cursor in the composer, as after a reply or an edit starts. (The composer's own
/// area may offer this too; this reaches it through what `Shared` keeps.)
pub fn focus_composer(shared: &Shared, cx: &mut App) {
    let Some(composer) = shared.composer.borrow().as_ref().and_then(|c| c.upgrade()) else {
        return;
    };
    shared.with_window(cx, |window, cx| {
        let handle = composer.read(cx).input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    });
}

pub fn reply(shared: &Shared, index: usize, cx: &mut App) {
    shared.redraw(cx, |chat| chat.reply_to(index));
    focus_composer(shared, cx);
}

pub fn react(shared: &Shared, index: usize, emoji: &str, cx: &mut App) {
    shared.command(cx, |chat| chat.react(index, emoji));
}

/// Puts the message in the composer, to be edited there.
pub fn edit(shared: &Shared, index: usize, cx: &mut App) {
    let text = shared.chat.borrow_mut().edit(index);
    if let Some(text) = text {
        shared.set_composer_text(&text, cx);
        shared.refresh(Update::default(), cx);
        focus_composer(shared, cx);
    }
}

/// Deletes the message here; for everyone can't be undone, so that is asked first.
pub fn delete(shared: &Shared, index: usize, everyone: bool, cx: &mut App) {
    if everyone {
        {
            let mut ui = shared.ui.borrow_mut();
            ui.confirm_message = index.to_string();
            ui.confirm = Confirm::DeleteForEveryone;
        }
        cx.refresh_windows();
    } else {
        shared.act(cx, |chat| chat.delete(index, false));
    }
}

pub fn forward(shared: &Shared, index: usize, cx: &mut App) {
    shared.edit(cx, |chat| {
        chat.start_forward(index);
    });
}

pub fn retry(shared: &Shared, index: usize, cx: &mut App) {
    shared.act(cx, |chat| {
        (
            chat.retry_send(index),
            Update {
                rows: true,
                ..Update::default()
            },
        )
    });
}

pub fn toggle_select(shared: &Shared, index: usize, cx: &mut App) {
    shared.redraw(cx, |chat| chat.toggle_selected(index));
}

pub fn select_through(shared: &Shared, index: usize, cx: &mut App) {
    shared.redraw(cx, |chat| chat.select_through(index));
}

pub fn reveal(shared: &Shared, index: usize, cx: &mut App) {
    shared.redraw(cx, |chat| chat.reveal(index));
}

pub fn copy(text: String, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// The menu: what right-clicking a message offers, and the button's "More".
pub fn menu(mut menu: PopupMenu, shared: &Rc<Shared>, info: &Info, cx: &App) -> PopupMenu {
    let item = |label: &'static str, run: Rc<dyn Fn(&Rc<Shared>, &Info, &mut App)>| {
        let (shared, info) = (shared.clone(), info.clone());
        PopupMenuItem::new(label).on_click(move |_, _, cx| run(&shared, &info, cx))
    };
    let reacts = !info.deleted && !info.notice;
    if !info.notice {
        menu = menu.item(item(
            if info.selected { "Unselect" } else { "Select" },
            Rc::new(|s, i, cx| toggle_select(s, i.index, cx)),
        ));
    }
    if reacts {
        menu = menu.item(item("Reply", Rc::new(|s, i, cx| reply(s, i.index, cx))));
    }
    if TextSelection::is_in(&info.key, cx) {
        menu = menu.item(item(
            "Copy selection",
            Rc::new(|_, _, cx| {
                TextSelection::copy(cx);
            }),
        ));
    }
    if !info.text.is_empty() {
        menu = menu.item(item(
            "Copy text",
            Rc::new(|_, i, cx| copy(i.text.clone(), cx)),
        ));
    }
    if info.save.is_some() {
        menu = menu.item(item(
            "Save attachment",
            Rc::new(|s, i, cx| {
                if let Some(id) = &i.save {
                    crate::attachments::save(s, id, cx);
                }
            }),
        ));
    }
    if info.can_forward {
        menu = menu.item(item(
            "Forward…",
            Rc::new(|s, i, cx| forward(s, i.index, cx)),
        ));
    }
    if info.can_edit {
        menu = menu.item(item("Edit", Rc::new(|s, i, cx| edit(s, i.index, cx))));
    }
    if info.can_send_again {
        menu = menu.item(item("Send again", Rc::new(|s, i, cx| retry(s, i.index, cx))));
    }
    if !info.notice {
        menu = menu.separator();
        menu = menu.item(item(
            "Delete for me",
            Rc::new(|s, i, cx| delete(s, i.index, false, cx)),
        ));
    }
    if info.can_edit {
        menu = menu.item(item(
            "Delete for everyone",
            Rc::new(|s, i, cx| delete(s, i.index, true, cx)),
        ));
    }
    menu
}

/// Quick reactions, as in Signal's own apps.
const QUICK: [&str; 6] = ["❤️", "👍", "👎", "😂", "😮", "😢"];

/// A message's buttons, which show while the pointer is on it: react, reply, more. The `group`
/// is the name of the row they show with.
pub fn toolbar(
    shared: &Rc<Shared>,
    info: &Info,
    outgoing: bool,
    group: SharedString,
) -> AnyElement {
    let id = info.index;
    let reacts = !info.deleted && !info.notice;
    let more = {
        let (shared, info) = (shared.clone(), info.clone());
        Button::new(SharedString::from(format!("more-{id}")))
            .ghost()
            .small()
            .icon(icon("ellipsis"))
            .tooltip("More")
            .dropdown_menu(move |menu, _, cx| self::menu(menu, &shared, &info, cx))
    };
    let reply = reacts.then(|| {
        let shared = shared.clone();
        Button::new(SharedString::from(format!("reply-{id}")))
            .ghost()
            .small()
            .icon(icon("reply"))
            .tooltip("Reply")
            .on_click(move |_, _, cx| reply(&shared, id, cx))
    });
    let react = reacts.then(|| reaction_button(shared, id));
    let buttons = div()
        .flex_none()
        .self_center()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.))
        // Always laid out, so the bubble doesn't move as they come and go; only seen while the
        // pointer is on the row, or on them (a menu or the picker open).
        .invisible()
        .group_hover(group, |s| s.visible());
    if outgoing {
        buttons.child(more).children(react).children(reply)
    } else {
        buttons.children(reply).children(react).child(more)
    }
    .into_any_element()
}

/// "React", which opens the picker: the quick reactions, and "+" for any other emoji.
fn reaction_button(shared: &Rc<Shared>, index: usize) -> Popover {
    let shared = shared.clone();
    Popover::new(SharedString::from(format!("react-{index}")))
        .anchor(Anchor::TopLeft)
        .trigger(
            Button::new(SharedString::from(format!("react-button-{index}")))
                .ghost()
                .small()
                .icon(icon("heart"))
                .tooltip("React"),
        )
        .content(move |_, window, cx| picker(&shared, index, window, cx))
}

/// The picker's content: the quick row, or every emoji by group after "+".
fn picker(
    shared: &Rc<Shared>,
    index: usize,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::popover::PopoverState>,
) -> AnyElement {
    let t = theme::tokens(cx);
    let all = window.use_keyed_state(
        SharedString::from(format!("react-all-{index}")),
        cx,
        |_, _| false,
    );
    let group = window.use_keyed_state(
        SharedString::from(format!("react-group-{index}")),
        cx,
        |_, _| 0usize,
    );
    let pick = |emoji: String, shared: Rc<Shared>| {
        cx.listener(
            move |state: &mut gpui_kit::component::popover::PopoverState, _: &gpui_kit::ClickEvent, window, cx| {
                react(&shared, index, &emoji, cx);
                state.dismiss(window, cx);
            },
        )
    };
    let cell = |emoji: &str, id: SharedString| {
        div()
            .id(id)
            .flex_none()
            .size(px(theme::BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .hover(move |s| s.bg(t.pressed))
            .aria_label(format!("React with {emoji}"))
            .text_size(px(theme::BODY * theme::HEADING))
            .child(SharedString::from(emoji.to_owned()))
    };
    if !*all.read(cx) {
        let more = all.clone();
        return div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .children(QUICK.iter().map(|emoji| {
                cell(emoji, SharedString::from(format!("quick-{index}-{emoji}")))
                    .on_click(pick((*emoji).to_owned(), shared.clone()))
            }))
            .child(
                div()
                    .id(SharedString::from(format!("more-emoji-{index}")))
                    .flex_none()
                    .size(px(theme::BUTTON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .hover(move |s| s.bg(t.pressed))
                    .aria_label("More emoji")
                    .child(icon("plus").size(px(theme::ICON)))
                    .on_click(move |_, _, cx| more.update(cx, |all, cx| {
                        *all = true;
                        cx.notify();
                    })),
            )
            .into_any_element();
    }
    let groups: Vec<emojis::Group> = emojis::Group::iter().collect();
    let shown = (*group.read(cx)).min(groups.len().saturating_sub(1));
    div()
        .flex()
        .flex_col()
        .gap(px(theme::SPACE_XS))
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(2.))
                .children(groups.iter().enumerate().map(|(at, g)| {
                    let group = group.clone();
                    let first = g.emojis().next().map_or("", |e| e.as_str());
                    cell(first, SharedString::from(format!("group-{index}-{at}")))
                        .size(px(28.))
                        .text_size(px(theme::BODY))
                        .when(at == shown, |c| c.bg(t.selected))
                        .on_click(move |_, _, cx| group.update(cx, |group, cx| {
                            *group = at;
                            cx.notify();
                        }))
                })),
        )
        .child(
            div()
                .id(SharedString::from(format!("emoji-grid-{index}-{shown}")))
                .w(px(8. * theme::BUTTON))
                .h(px(220.))
                .overflow_y_scroll()
                .flex()
                .flex_row()
                .flex_wrap()
                .children(groups.get(shown).into_iter().flat_map(|g| g.emojis()).filter(|e| e.skin_tone().is_none() || e.skin_tone() == Some(emojis::SkinTone::Default)).map(|emoji| {
                    let text = emoji.as_str();
                    cell(text, SharedString::from(format!("emoji-{index}-{text}")))
                        .on_click(pick(text.to_owned(), shared.clone()))
                })),
        )
        .into_any_element()
}

/// The bar over the composer while messages are picked: how many, and what to do with them.
pub fn selection_bar(shared: &Rc<Shared>, t: &Tokens) -> Option<AnyElement> {
    let chat = shared.chat.borrow();
    let count = chat.selection_len();
    if count == 0 {
        return None;
    }
    let can_forward = chat.can_forward_selection();
    let can_delete_everyone = chat.can_delete_selection_for_everyone();
    drop(chat);
    let button = |id: &'static str, name: &'static str, label: &'static str| {
        Button::new(id)
            .ghost()
            .small()
            .icon(icon(name))
            .tooltip(label)
    };
    let delete = {
        let shared = shared.clone();
        button("selection-delete", "trash", "Delete selected").dropdown_menu(move |menu, _, _| {
            let for_me = shared.clone();
            let menu = menu.item(PopupMenuItem::new("Delete for me").on_click(move |_, _, cx| {
                for_me.act(cx, |chat| chat.delete_selection(false));
            }));
            if can_delete_everyone {
                let everyone = shared.clone();
                menu.item(
                    PopupMenuItem::new("Delete for everyone…").on_click(move |_, _, cx| {
                        everyone.ui.borrow_mut().confirm = Confirm::DeleteSelectionForEveryone;
                        cx.refresh_windows();
                    }),
                )
            } else {
                menu
            }
        })
    };
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(theme::SPACE_XS))
            .pl(px(theme::SPACE_L))
            .pr(px(theme::SPACE_S))
            .bg(t.surface_raised)
            .border_t_1()
            .border_color(t.divider)
            // Over the composer: nothing under it answers the pointer.
            .occlude()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if count == 1 {
                        "1 message selected".to_owned()
                    } else {
                        format!("{count} messages selected")
                    }),
            )
            .child(button("selection-copy", "copy", "Copy selected").on_click({
                let shared = shared.clone();
                move |_, _, cx| {
                    let text = shared.chat.borrow().selected_text();
                    copy(text, cx);
                    shared.redraw(cx, |chat| chat.clear_selection());
                }
            }))
            .when(can_forward, |bar| {
                let shared = shared.clone();
                bar.child(button("selection-forward", "forward", "Forward selected").on_click(
                    move |_, _, cx| {
                        shared.edit(cx, |chat| {
                            chat.start_forward_selection();
                        })
                    },
                ))
            })
            .child(delete)
            .child(button("selection-clear", "x", "Clear selection").on_click({
                let shared = shared.clone();
                move |_, _, cx| shared.redraw(cx, |chat| chat.clear_selection())
            }))
            .into_any_element(),
    )
}
