//! The pieces the panels over the messages share: the sheet they are drawn on, their header
//! with a way back, section titles, empty and busy states, and the pills of a tab row.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, SharedString, Stateful, Window, div,
    px,
};

use crate::theme::{self, Tokens};
use crate::widgets::{divider, icon, icon_button};

/// What a panel is drawn on: it fills the place of the messages and takes every pointer event
/// over them, so that the list beneath neither scrolls nor answers clicks.
pub fn sheet(id: &'static str, t: &Tokens) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .bg(t.surface)
        .text_color(t.text)
        .occlude()
}

/// The top of a panel: a way back, a title and under it, quieter, a subtitle; `extra` goes at
/// the right end.
pub fn header(
    t: &Tokens,
    back_label: &'static str,
    title: impl Into<SharedString>,
    subtitle: Option<String>,
    extra: Option<AnyElement>,
    back: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let middle = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .truncate()
                .text_size(px(theme::BODY * theme::TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(subtitle.map(|subtitle| {
            div()
                .truncate()
                .text_size(px(theme::BODY * theme::SMALL))
                .text_color(t.text_tertiary)
                .child(SharedString::from(subtitle))
        }));
    header_with(t, back_label, middle, extra, back)
}

/// A panel's top with `middle` between the way back and `extra`: a field, for the search.
pub fn header_with(
    t: &Tokens,
    back_label: &'static str,
    middle: impl IntoElement,
    extra: Option<AnyElement>,
    back: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(theme::PANEL_HEADER))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .pl(px(theme::SPACE_S))
                .pr(px(theme::SPACE_M))
                .child(icon_button("panel-back", "arrow-left", back_label, back))
                .child(middle)
                .children(extra),
        )
        .child(divider(t))
}

/// Nothing to show: an icon and a line about it, in the middle of what is left.
pub fn empty_state(t: &Tokens, name: &'static str, text: &'static str) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACE_M))
        .p(px(theme::SPACE_XL))
        .text_color(t.text_tertiary)
        .child(icon(name).size(px(40.)).text_color(t.text_faint))
        .child(div().text_center().child(text))
}

/// Waiting for something: a spinner in the middle.
pub fn busy() -> Div {
    div()
        .flex()
        .justify_center()
        .p(px(theme::SPACE_S))
        .child(Spinner::new().small())
}

/// A heading within a page, in the accent.
pub fn section_title(t: &Tokens, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::BODY * theme::BODY_SMALL))
        .font_weight(FontWeight::BOLD)
        .text_color(t.accent)
        .child(text.into())
}

/// A paragraph of explanation, quieter than the text.
pub fn hint(t: &Tokens, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::BODY * theme::BODY_SMALL))
        .text_color(t.text_tertiary)
        .child(text.into())
}

/// One of a row of choices, the chosen one tinted.
pub fn pill(
    t: &Tokens,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let (hover, tint) = (t.hover, t.selected);
    div()
        .id(id)
        .h(px(28.))
        .px(px(theme::SPACE_M))
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .text_size(px(theme::BODY * theme::BODY_SMALL))
        .aria_label(label.clone())
        .when(selected, |p| p.bg(tint).font_weight(FontWeight::SEMIBOLD))
        .when(!selected, |p| p.hover(|s| s.bg(hover)))
        .on_click(on_click)
        .child(label)
}
