//! Joining a group through its invite link: paste it, see what the group shows of itself, then
//! join or ask to.

use std::rc::Rc;

use b33p_core::GroupPreview;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, FontWeight, IntoElement, Render, Subscription, Window, div, px};

use crate::dialog;
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{icon, modal};

pub struct JoinView {
    shared: Rc<Shared>,
    link: Entity<InputState>,
    /// The link the state had when this was last drawn: the field follows it when it changes
    /// from elsewhere (a link opened while the dialog is open).
    known: String,
    /// How long the field's text was, to tell a paste (many characters at once) from typing.
    length: usize,
    _subscriptions: Vec<Subscription>,
}

/// Whether `text` is, or starts as, a group's invite link.
fn looks_like_link(text: &str) -> bool {
    // Something has to follow the host: the link's key.
    text.trim()
        .split_once("signal.group/")
        .is_some_and(|(_, rest)| rest.trim_start_matches('#').chars().count() > 1)
}

/// What the dialog says about the group it found.
fn explanation(preview: &GroupPreview) -> &'static str {
    if preview.requested {
        "You asked to join already. An admin has to let you in."
    } else if preview.approval {
        "An admin has to let you in. Asking shows them your name and picture."
    } else {
        "Its members see your name and picture once you join."
    }
}

fn members(count: u32) -> String {
    if count == 1 {
        "1 member".to_owned()
    } else {
        format!("{count} members")
    }
}

impl JoinView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let known = shared
            .chat
            .borrow()
            .join()
            .map(|(link, _, _)| link.to_owned())
            .unwrap_or_default();
        let link = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://signal.group/#…")
                .default_value(known.clone())
        });
        link.update(cx, |link, cx| link.focus(window, cx));
        let subscription = cx.subscribe_in(
            &link,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    let pasted = text.len() > this.length + 1;
                    this.length = text.len();
                    if pasted && looks_like_link(&text) && text.trim() != this.known {
                        this.look_up(cx);
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.look_up(cx),
                _ => {}
            },
        );
        let length = known.len();
        Self {
            shared,
            link,
            known,
            length,
            _subscriptions: vec![subscription],
        }
    }

    fn look_up(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().to_string();
        self.shared.command(cx, |chat| chat.preview_link(&link));
    }
}

impl Render for JoinView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let (state_link, preview, busy) = {
            let chat = shared.chat.borrow();
            let Some((link, preview, busy)) = chat.join() else {
                return div().into_any_element();
            };
            (link.to_owned(), preview.cloned(), busy)
        };
        if state_link != self.known {
            // Looked up from elsewhere: the field shows what was looked up.
            if self.link.read(cx).value().trim() != state_link {
                self.length = state_link.len();
                self.link.update(cx, |input, cx| {
                    input.set_value(state_link.clone(), window, cx)
                });
            }
            self.known = state_link;
        }
        let blank = self.link.read(cx).value().trim().is_empty();
        let can_join = preview.is_some() && !busy && preview.as_ref().is_some_and(|p| !p.requested);
        let approval = preview.as_ref().is_some_and(|p| p.approval);
        let content = dialog::layout("Join a group")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_S))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.link).disabled(busy)),
                    )
                    .child(
                        Button::new("look-up")
                            .label("Look up")
                            .debug_selector(|| "Look up".into())
                            .disabled(busy || blank)
                            .on_click(cx.listener(|this, _, _, cx| this.look_up(cx))),
                    ),
            )
            .when(busy, |layout| {
                layout.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACE_S))
                        .child(Spinner::new().small())
                        .child(dialog::hint(&t, "Looking it up…")),
                )
            })
            .children(preview.as_ref().filter(|_| !busy).map(|preview| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(theme::SPACE_M))
                    .child(
                        div().flex_none().child(
                            div()
                                .size(px(40.))
                                .rounded_full()
                                .bg(t.accent)
                                .text_color(t.on_accent)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon("users").size(px(theme::ICON))),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(theme::SPACE_XS))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(preview.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::BODY * theme::SMALL))
                                    .text_color(t.text_secondary)
                                    .child(members(preview.members)),
                            )
                            .child(dialog::hint(&t, explanation(preview))),
                    )
            }))
            .child(
                dialog::buttons()
                    .child(
                        Button::new("join-cancel")
                            .label("Cancel")
                            .debug_selector(|| "Cancel".into())
                            .on_click({
                                let shared = shared.clone();
                                move |_, _, cx| shared.edit(cx, |chat| chat.close_join())
                            }),
                    )
                    .child(
                        Button::new("join")
                            .primary()
                            .label(if approval { "Ask to join" } else { "Join" })
                            .disabled(!can_join)
                            .on_click({
                                let shared = shared.clone();
                                move |_, _, cx| shared.command(cx, |chat| chat.join_group())
                            }),
                    ),
            );
        modal(&t, 440., content, move |_, cx| {
            shared.edit(cx, |chat| chat.close_join())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_told_from_other_text() {
        assert!(looks_like_link("https://signal.group/#CjQKIA"));
        assert!(looks_like_link("  https://signal.group/#CjQKIA \n"));
        assert!(!looks_like_link("https://signal.group/"));
        assert!(!looks_like_link("hello"));
    }

    #[test]
    fn members_are_counted_in_the_singular_too() {
        assert_eq!(members(1), "1 member");
        assert_eq!(members(12), "12 members");
    }

    #[test]
    fn the_explanation_says_what_joining_means() {
        let mut preview = GroupPreview {
            link: String::new(),
            title: "Rust".into(),
            members: 3,
            approval: false,
            requested: false,
        };
        assert!(explanation(&preview).starts_with("Its members"));
        preview.approval = true;
        assert!(explanation(&preview).starts_with("An admin has to let you in. Asking"));
        preview.requested = true;
        assert!(explanation(&preview).starts_with("You asked"));
    }
}
