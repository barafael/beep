//! Small pieces the views are made of: icons, icon buttons, avatars.

use b33p_core::ConversationId;
use b33p_core::state;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, ClickEvent, Div, ElementId, Hsla, ObjectFit, Rgba, SharedString, Window, div, hsla, img, px, rgb,
};
use gpui_kit::FontWeight;

use crate::images::Drawn;
use crate::theme::Tokens;

/// A Lucide icon, by the name the Slint window's `@lucide/<name>.svg` uses.
pub fn icon(name: &'static str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/{name}.svg")))
}

/// A button with an icon and no label, which says what it does on hover and to screen readers.
pub fn icon_button(
    id: impl Into<ElementId>,
    name: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .icon(icon(name))
        .tooltip(label)
        .on_click(on_click)
}

/// A stable colour per person or group, for avatars without a picture and group senders' names.
pub fn avatar_color(id: &ConversationId) -> Hsla {
    // Muted hues in the spirit of Signal's avatar palette; all readable with white text.
    const PALETTE: [u32; 8] = [
        0x3a76f0, 0x7d4fc9, 0xc4314b, 0xd36c1c, 0x2b8a60, 0x1e8a9e, 0x9a5b3c, 0x5c6bc0,
    ];
    let color: Rgba = rgb(PALETTE[state::avatar_index(id, PALETTE.len())]);
    color.into()
}

/// A round picture, or initials on a colour when there is none.
pub fn avatar(
    size: f32,
    initials: &str,
    color: Hsla,
    picture: Option<Drawn>,
    tokens: &Tokens,
) -> Div {
    let base = div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center();
    match picture {
        // Rounded itself: clipping to the circle is only to the rectangle.
        Some(picture) => base.child(
            img(picture)
                .size_full()
                .rounded_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => base
            .bg(color)
            .text_color(tokens.on_accent)
            .text_size(px(size * 0.4))
            .font_weight(FontWeight::MEDIUM)
            .child(SharedString::from(initials.to_owned())),
    }
}

/// A thin line between parts.
pub fn divider(tokens: &Tokens) -> Div {
    div().flex_none().h(px(1.)).w_full().bg(tokens.divider)
}

pub fn fade(color: Hsla, by: f32) -> Hsla {
    hsla(color.h, color.s, color.l, color.a * by)
}
