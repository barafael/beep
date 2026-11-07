//! What can be done with a message: the buttons that show while the pointer rests on it, its
//! menu (also on a right click), the reaction picker, and the bar over the composer while
//! messages are picked.

use std::rc::Rc;

use b33p_core::state::Update;
use gpui_kit::Focusable as _;
use gpui_kit::Role;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, ClipboardItem, Context, FontWeight, SharedString, Window, div, px,
};

use crate::shared::{Confirm, Shared};
use crate::text::TextSelection;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, icon};

/// What a menu entry does with the message it is for.
type Run = Rc<dyn Fn(&Rc<Shared>, &Info, &mut Window, &mut App)>;

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
    /// Pinning is offered (messages can be pinned in the chat, and this one isn't, and can be),
    /// or unpinning is (it is pinned, and may be unpinned).
    pub pin: PinOffer,
    /// Pinning one more message replaces the chat's oldest pin.
    pub pins_full: bool,
    /// One of ours that went out: its info says who got it and read it.
    pub can_info: bool,
}

/// What a message's menu offers about pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinOffer {
    None,
    Pin,
    Unpin,
}

/// How long a pin lasts, as Signal-Desktop's "Pin message for…" offers, in seconds; `None` is
/// until unpinned.
pub const PIN_CHOICES: [(&str, Option<u32>); 4] = [
    ("For 24 hours", Some(24 * 3600)),
    ("For 7 days", Some(7 * 24 * 3600)),
    ("For 30 days", Some(30 * 24 * 3600)),
    ("Until unpinned", None),
];

/// Puts the cursor in the composer, as after a reply or an edit starts. (The composer's own
/// area may offer this too; this reaches it through what `Shared` keeps.)
pub fn focus_composer(shared: &Shared, window: &mut Window, cx: &mut App) {
    let Some(composer) = shared.composer.borrow().as_ref().and_then(|c| c.upgrade()) else {
        return;
    };
    let handle = composer.read(cx).input.read(cx).focus_handle(cx);
    window.focus(&handle, cx);
}

pub fn reply(shared: &Shared, index: usize, window: &mut Window, cx: &mut App) {
    shared.redraw(cx, |chat| chat.reply_to(index));
    focus_composer(shared, window, cx);
}

pub fn react(shared: &Shared, index: usize, emoji: &str, cx: &mut App) {
    shared.command(cx, |chat| chat.react(index, emoji));
}

/// Puts the message in the composer, to be edited there. (With the window the event came in:
/// the window is being updated, and can't be reached through its handle meanwhile.)
pub fn edit(shared: &Shared, index: usize, window: &mut Window, cx: &mut App) {
    let text = shared.chat.borrow_mut().edit(index);
    let Some(text) = text else {
        return;
    };
    if let Some(composer) = shared.composer.borrow().as_ref().and_then(|c| c.upgrade()) {
        composer.update(cx, |composer, cx| composer.set_text(&text, window, cx));
    }
    shared.refresh(Update::default(), cx);
    focus_composer(shared, window, cx);
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

pub fn pin(shared: &Shared, index: usize, seconds: Option<u32>, cx: &mut App) {
    shared.command(cx, |chat| chat.pin_message(index, seconds));
}

pub fn unpin(shared: &Shared, index: usize, cx: &mut App) {
    shared.command(cx, |chat| chat.unpin_message(index));
}

/// Opens the info of one of our messages: who it went to, and when they got it and read it.
pub fn open_info(shared: &Shared, index: usize, cx: &mut App) {
    shared.command(cx, |chat| chat.open_message_info(index));
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
pub fn menu(
    mut menu: PopupMenu,
    shared: &Rc<Shared>,
    info: &Info,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let item = |label: &'static str, run: Run| {
        let (shared, info) = (shared.clone(), info.clone());
        PopupMenuItem::new(label).on_click(move |_, window, cx| run(&shared, &info, window, cx))
    };
    let reacts = !info.deleted && !info.notice;
    if !info.notice {
        menu = menu.item(item(
            if info.selected { "Unselect" } else { "Select" },
            Rc::new(|s, i, _, cx| toggle_select(s, i.index, cx)),
        ));
    }
    if reacts {
        menu = menu.item(item(
            "Reply",
            Rc::new(|s, i, window, cx| reply(s, i.index, window, cx)),
        ));
    }
    if reacts {
        // The quick reactions; the buttons beside the message have every emoji.
        let (shared, index) = (shared.clone(), info.index);
        menu = menu.submenu("React…", window, cx, move |mut sub, _, _| {
            for emoji in QUICK {
                let shared = shared.clone();
                sub = sub.item(
                    PopupMenuItem::new(emoji)
                        .on_click(move |_, _, cx| react(&shared, index, emoji, cx)),
                );
            }
            sub
        });
    }
    if TextSelection::is_in(&info.key, cx) {
        menu = menu.item(item(
            "Copy selection",
            Rc::new(|_, _, _, cx| {
                TextSelection::copy(cx);
            }),
        ));
    }
    if !info.text.is_empty() {
        menu = menu.item(item(
            "Copy text",
            Rc::new(|_, i, _, cx| copy(i.text.clone(), cx)),
        ));
    }
    if info.save.is_some() {
        menu = menu.item(item(
            "Save attachment",
            Rc::new(|s, i, _, cx| {
                if let Some(id) = &i.save {
                    crate::attachments::save(s, id, cx);
                }
            }),
        ));
    }
    if info.can_forward {
        menu = menu.item(item(
            "Forward…",
            Rc::new(|s, i, _, cx| forward(s, i.index, cx)),
        ));
    }
    if info.can_edit {
        menu = menu.item(item(
            "Edit",
            Rc::new(|s, i, window, cx| edit(s, i.index, window, cx)),
        ));
    }
    if info.can_send_again {
        menu = menu.item(item(
            "Send again",
            Rc::new(|s, i, _, cx| retry(s, i.index, cx)),
        ));
    }
    // As Signal-Desktop's "Pin message for…", in a submenu instead of a dialog.
    if info.pin == PinOffer::Pin {
        let (shared, index) = (shared.clone(), info.index);
        let label = if info.pins_full {
            "Pin, replacing the oldest pin"
        } else {
            "Pin"
        };
        menu = menu.submenu(label, window, cx, move |mut sub, _, _| {
            for (label, seconds) in PIN_CHOICES {
                let shared = shared.clone();
                sub = sub.item(
                    PopupMenuItem::new(label)
                        .on_click(move |_, _, cx| pin(&shared, index, seconds, cx)),
                );
            }
            sub
        });
    }
    if info.pin == PinOffer::Unpin {
        menu = menu.item(item("Unpin", Rc::new(|s, i, _, cx| unpin(s, i.index, cx))));
    }
    if info.can_info {
        menu = menu.item(item(
            "Info",
            Rc::new(|s, i, _, cx| open_info(s, i.index, cx)),
        ));
    }
    if !info.notice {
        menu = menu.separator();
        menu = menu.item(item(
            "Delete for me",
            Rc::new(|s, i, _, cx| delete(s, i.index, false, cx)),
        ));
    }
    if info.can_edit {
        menu = menu.item(item(
            "Delete for everyone",
            Rc::new(|s, i, _, cx| delete(s, i.index, true, cx)),
        ));
    }
    menu
}

/// Quick reactions, as in Signal's own apps.
pub const QUICK: [&str; 6] = ["❤️", "👍", "👎", "😂", "😮", "😢"];

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
    // Every row has room for its buttons, so that a bubble doesn't move as they come and go, but
    // only the row the pointer is on, or whose buttons were pressed last, has them: three
    // buttons with their menus on each of the dozen rows on screen were more than a quarter of
    // what drawing a frame cost while scrolling.
    if shared.hovered_row.get() != Some(id) && shared.pinned_row.get() != Some(id) {
        let buttons = if reacts { 3. } else { 1. };
        // The size of a small button, in rems as the button is, and the gaps of 2 pixels (an
        // eighth of a rem) between them.
        return div()
            .flex_none()
            .w(gpui_kit::rems(1.5 * buttons + 0.125 * (buttons - 1.)))
            .into_any_element();
    }
    let more = {
        let (shared, info) = (shared.clone(), info.clone());
        Button::new(SharedString::from(format!("more-{id}")))
            .ghost()
            .small()
            .icon(icon("ellipsis"))
            .tooltip("More")
            .dropdown_menu(move |menu, window, cx| self::menu(menu, &shared, &info, window, cx))
    };
    let reply = reacts.then(|| {
        let shared = shared.clone();
        Button::new(SharedString::from(format!("reply-{id}")))
            .ghost()
            .small()
            .icon(icon("reply"))
            .tooltip("Reply")
            .on_click(move |_, window, cx| reply(&shared, id, window, cx))
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
        .group_hover(group, |s| s.visible())
        // A menu or the picker open from these buttons outlives the pointer being on the row, and
        // goes with the buttons: they stay for as long as no other row's are pressed.
        .on_mouse_down(gpui_kit::MouseButton::Left, {
            let shared = shared.clone();
            move |_, _, _| shared.pinned_row.set(Some(id))
        });
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
        .content(move |_, window, cx| picker(&shared, index, false, window, cx))
}

/// The picker's content: the quick row, or every emoji by group after "+" (at once, if `full`:
/// the quick row is on the bar over the message, which has its own "+").
pub fn picker(
    shared: &Rc<Shared>,
    index: usize,
    full: bool,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::popover::PopoverState>,
) -> AnyElement {
    let t = theme::tokens(cx);
    let all = window.use_keyed_state(
        SharedString::from(format!("react-all-{index}-{full}")),
        cx,
        move |_, _| full,
    );
    let group = window.use_keyed_state(
        SharedString::from(format!("react-group-{index}")),
        cx,
        |_, _| 0usize,
    );
    let pick = |emoji: String, shared: Rc<Shared>| {
        cx.listener(
            move |state: &mut gpui_kit::component::popover::PopoverState,
                  _: &gpui_kit::ClickEvent,
                  window,
                  cx| {
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
            .speaks(Role::Button, format!("React with {emoji}"))
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
                    .speaks(Role::Button, "More emoji")
                    .child(icon("plus").size(px(theme::ICON)))
                    .on_click(move |_, _, cx| {
                        more.update(cx, |all, cx| {
                            *all = true;
                            cx.notify();
                        })
                    }),
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
                        .on_click(move |_, _, cx| {
                            group.update(cx, |group, cx| {
                                *group = at;
                                cx.notify();
                            })
                        })
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
                .children(
                    groups
                        .get(shown)
                        .into_iter()
                        .flat_map(|g| g.emojis())
                        .filter(|e| {
                            e.skin_tone().is_none()
                                || e.skin_tone() == Some(emojis::SkinTone::Default)
                        })
                        .map(|emoji| {
                            let text = emoji.as_str();
                            cell(text, SharedString::from(format!("emoji-{index}-{text}")))
                                .on_click(pick(text.to_owned(), shared.clone()))
                        }),
                ),
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
            .debug_selector(|| label.into())
    };
    let delete = {
        let shared = shared.clone();
        button("selection-delete", "trash", "Delete selected").dropdown_menu(move |menu, _, _| {
            let for_me = shared.clone();
            let menu = menu.item(
                PopupMenuItem::new("Delete for me").on_click(move |_, _, cx| {
                    for_me.act(cx, |chat| chat.delete_selection(false));
                }),
            );
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
    let bar = div()
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
            bar.child(
                button("selection-forward", "forward", "Forward selected").on_click(
                    move |_, _, cx| {
                        shared.edit(cx, |chat| {
                            chat.start_forward_selection();
                        })
                    },
                ),
            )
        })
        .child(delete)
        .child(button("selection-clear", "x", "Clear selection").on_click({
            let shared = shared.clone();
            move |_, _, cx| shared.redraw(cx, |chat| chat.clear_selection())
        }));
    // Fades in over the composer.
    Some(
        crate::animation::fade_in("selection-bar-fade", crate::animation::FAST, bar)
            .into_any_element(),
    )
}
