//! What the dialogs over the window have in common: their layout (a title, what they hold, their
//! buttons at the bottom right), and the list of people the group and forward dialogs choose
//! from.

use std::rc::Rc;

use b33p_core::state::{self, ChatState};
use b33p_core::{Conversation, ConversationId};
use gpui_kit::Role;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Div, FontWeight, Hsla, SharedString, UniformListScrollHandle, Window, div, px,
    uniform_list,
};

use crate::images::Drawn;
use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, avatar, avatar_color, icon};

/// The height of a row in a list of people.
pub const PERSON_ROW: f32 = 48.0;

/// A dialog's contents: its title, then what it holds, spaced alike in every dialog.
pub fn layout(title: impl Into<SharedString>) -> Div {
    let title: SharedString = title.into();
    div()
        .flex()
        .flex_col()
        .gap(px(theme::SPACE_M))
        .p(px(theme::SPACE_L))
        .when(!title.is_empty(), |layout| {
            layout.child(
                div()
                    .text_size(px(theme::BODY * theme::TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
        })
}

/// A dialog's buttons, at its bottom right: Cancel first, then what the dialog is for.
pub fn buttons() -> Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .justify_end()
        .gap(px(theme::SPACE_S))
        .pt(px(theme::SPACE_XS))
}

/// A hint or a line of explanation, in the quieter colour.
pub fn hint(t: &Tokens, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::BODY * theme::BODY_SMALL))
        .text_color(t.text_secondary)
        .child(text.into())
}

/// Someone, or a chat, offered in a list.
#[derive(Clone)]
pub struct Person {
    pub id: ConversationId,
    pub title: String,
    pub initials: String,
    pub color: Hsla,
    pub picture: Option<Drawn>,
    /// Whether they are chosen, for a list that chooses several; `None` for one that picks one.
    pub chosen: Option<bool>,
}

impl Person {
    pub fn of(
        conversation: &Conversation,
        chat: &ChatState,
        shared: &Shared,
        chosen: Option<bool>,
    ) -> Self {
        let id = conversation.id.clone();
        Self {
            title: conversation.title.clone(),
            initials: state::initials(&conversation.title),
            color: avatar_color(&id),
            picture: shared
                .images
                .avatar(&format!("chat:{}", id.0), chat.avatar(&id)),
            chosen,
            id,
        }
    }
}

/// The label a screen reader reads for a person's row.
fn row_label(person: &Person, verb: &str) -> SharedString {
    match person.chosen {
        Some(true) => format!("{}, chosen", person.title).into(),
        Some(false) => person.title.clone().into(),
        None => format!("{verb} {}", person.title).into(),
    }
}

/// A scrolling list of people (rows of avatar and name, with a check circle when the list
/// chooses several); `pick` answers a click on one.
pub fn people(
    id: &'static str,
    rows: Vec<Person>,
    scroll: &UniformListScrollHandle,
    t: &Tokens,
    verb: &'static str,
    pick: impl Fn(&ConversationId, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let count = rows.len();
    let t = *t;
    let pick = Rc::new(pick);
    uniform_list(id, count, move |range, _, _| {
        range
            .filter_map(|index| rows.get(index).map(|person| (index, person.clone())))
            .map(|(index, person)| {
                let target = person.id.clone();
                let pick = pick.clone();
                div()
                    .id(("person", index))
                    .w_full()
                    .h(px(PERSON_ROW))
                    .px(px(6.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_M))
                    .rounded(px(theme::RADIUS_S))
                    .cursor_pointer()
                    .hover(|row| row.bg(t.hover))
                    .speaks(Role::Button, row_label(&person, verb))
                    .on_click(move |_, window, cx| pick(&target, window, cx))
                    .child(avatar(
                        36.,
                        &person.initials,
                        person.color,
                        person.picture.clone(),
                        &t,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(person.title.clone()),
                    )
                    .children(person.chosen.map(|chosen| check(chosen, &t)))
            })
            .collect()
    })
    .track_scroll(scroll)
    .flex_1()
    .min_h_0()
    .into_any_element()
}

/// The circle at the end of a row of a list that chooses several.
fn check(chosen: bool, t: &Tokens) -> Div {
    div()
        .flex_none()
        .size(px(22.))
        .rounded_full()
        .border_2()
        .border_color(if chosen { t.accent } else { t.divider })
        .when(chosen, |circle| circle.bg(t.accent))
        .flex()
        .items_center()
        .justify_center()
        .when(chosen, |circle| {
            circle.child(icon("check").size(px(14.)).text_color(t.on_accent))
        })
}

/// How tall a dialog that fills its height may be: `wanted`, unless the window is shorter.
pub fn height(window: &Window, wanted: f32) -> f32 {
    let available: f32 = window.viewport_size().height.into();
    wanted.min(available * 0.9)
}
