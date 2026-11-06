//! Stories, over everything: who has them, the one shown, and posting one to a group.

use std::rc::Rc;

use b33p_core::state::{self, AttachmentState, ChatState};
use b33p_core::{AttachmentId, ConversationId, NewStory, StoryContent};
use b33p_platform::desktop::{self, Pick};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, IndexPath, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    IntoElement, KeyDownEvent, ObjectFit, Render, SharedString, Subscription, Window, div, img, px,
    rgb,
};

use crate::images::Drawn;
use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{avatar, avatar_color, icon};

/// The width of the list of who has stories.
const LIST: f32 = 280.0;
/// The arrows beside the story, and the gaps around them.
const ARROW: f32 = 40.0;

/// A person or group with stories, as the list shows it.
struct Ring {
    id: ConversationId,
    title: String,
    initials: String,
    time: String,
    unviewed: bool,
    picture: Option<Drawn>,
}

/// What the story shown holds.
enum Shown {
    Text {
        text: String,
        foreground: Hsla,
        background: Hsla,
    },
    Picture {
        id: AttachmentId,
        picture: Option<Drawn>,
        blur: Option<Drawn>,
        failed: Option<bool>,
    },
}

struct Current {
    author: String,
    time: String,
    position: String,
    shown: Shown,
}

pub struct StoriesView {
    shared: Rc<Shared>,
    text: Entity<InputState>,
    groups: Entity<SelectState<Vec<SharedString>>>,
    /// The groups the select offers, by id and by name, as it was last set.
    offered: Vec<(ConversationId, SharedString)>,
    /// The picture story asked for last: the view asks for each once, not on every draw.
    fetched: Option<AttachmentId>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// A colour as 0xAARRGGBB, drawn opaque.
fn opaque(argb: u32) -> Hsla {
    rgb(argb & 0x00ff_ffff).into()
}

/// The size of a story that is as tall as it can be and phone-shaped, within a box.
fn phone_shaped(width: f32, height: f32) -> (f32, f32) {
    let width = width.min(height * 9. / 16.);
    (width, height.min(width * 16. / 9.))
}

impl StoriesView {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| InputState::new(window, cx).placeholder("Text"));
        let groups = cx.new(|cx| SelectState::new(Vec::<SharedString>::new(), None, window, cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let subscription = cx.subscribe_in(
            &text,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.post_text(window, cx),
                _ => {}
            },
        );
        Self {
            shared,
            text,
            groups,
            offered: Vec::new(),
            fetched: None,
            focus,
            _subscriptions: vec![subscription],
        }
    }

    /// The group the select shows chosen.
    fn chosen_group(&self, cx: &App) -> Option<ConversationId> {
        let at = self.groups.read(cx).selected_index(cx)?.row;
        self.offered.get(at).map(|(id, _)| id.clone())
    }

    /// Rebuilds the select's choices when the groups or their names change; the choice stays with
    /// its group.
    fn offer_groups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let offered: Vec<(ConversationId, SharedString)> = {
            let chat = self.shared.chat.borrow();
            chat.story_groups()
                .into_iter()
                .map(|c| (c.id.clone(), SharedString::from(c.title.clone())))
                .collect()
        };
        if offered == self.offered {
            return;
        }
        let chosen = self.chosen_group(cx);
        let at = chosen
            .and_then(|id| offered.iter().position(|(other, _)| *other == id))
            .unwrap_or(0);
        let titles: Vec<SharedString> = offered.iter().map(|(_, title)| title.clone()).collect();
        let any = !titles.is_empty();
        self.offered = offered;
        self.groups.update(cx, |groups, cx| {
            groups.set_items(titles, window, cx);
            groups.set_selected_index(any.then(|| IndexPath::default().row(at)), window, cx);
        });
    }

    fn post_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text.read(cx).value().to_string();
        let Some(group) = self.chosen_group(cx) else {
            return;
        };
        let sent = self.shared.act(cx, |chat| {
            (
                chat.send_story(&group, NewStory::Text(text)),
                b33p_core::state::Update::default(),
            )
        });
        if sent {
            self.text
                .update(cx, |text, cx| text.set_value("", window, cx));
        }
    }

    fn post_photo(&mut self, cx: &mut Context<Self>) {
        let Some(picked) = desktop::ask_pick(
            &self.shared.desktop,
            "Post a photo as a story",
            Pick::Picture,
        ) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(paths) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let outcome = this.update(cx, |this, cx| {
                let Some(group) = this.chosen_group(cx) else {
                    return;
                };
                this.shared
                    .command(cx, |chat| chat.send_story(&group, NewStory::Picture(path)));
            });
            if outcome.is_err() {
                tracing::warn!("the stories closed before a photo was picked");
            }
        })
        .detach();
    }

    fn step(&self, by: isize, cx: &mut App) {
        self.shared.command(cx, |chat| chat.step_story(by));
    }

    fn close_story(&self, cx: &mut App) {
        self.shared.edit(cx, |chat| chat.close_story());
    }

    fn close_all(&self, cx: &mut App) {
        self.shared.ui.borrow_mut().stories_open = false;
        self.close_story(cx);
    }

    /// The story shown, as the viewer draws it.
    fn current(&self, chat: &ChatState) -> Option<Current> {
        let (story, at, count) = chat.current_story()?;
        let shown = match &story.content {
            StoryContent::Text {
                text,
                foreground,
                background,
            } => Shown::Text {
                text: text.clone(),
                foreground: opaque(*foreground),
                background: opaque(*background),
            },
            StoryContent::Picture(picture) => Shown::Picture {
                id: picture.id.clone(),
                picture: self.shared.images.downloaded(chat, &picture.id),
                blur: picture
                    .blurhash
                    .as_deref()
                    .and_then(|hash| self.shared.images.blur(hash, picture.width, picture.height)),
                failed: match chat.attachment(&picture.id) {
                    Some(AttachmentState::Failed(error)) => Some(error.retryable()),
                    _ => None,
                },
            },
        };
        Some(Current {
            author: story.author.clone(),
            time: state::format_time(story.timestamp),
            position: format!("{} / {count}", at + 1),
            shown,
        })
    }

    fn rings(&self, chat: &ChatState) -> Vec<Ring> {
        chat.story_rings()
            .iter()
            .map(|ring| Ring {
                id: ring.conversation.clone(),
                title: ring.title.clone(),
                initials: state::initials(&ring.title),
                time: state::format_list_time(ring.latest),
                unviewed: ring.unviewed,
                picture: self.shared.images.avatar(
                    &format!("chat:{}", ring.conversation.0),
                    chat.avatar(ring.conversation),
                ),
            })
            .collect()
    }
}

impl Focusable for StoriesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A button in the viewer: an icon on the stage.
fn stage_button(
    id: &'static str,
    name: &'static str,
    label: &'static str,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .size(px(theme::BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_M))
        .cursor_pointer()
        .hover(|button| button.bg(t.on_stage.opacity(0.08)))
        .aria_label(label)
        .child(icon(name).size(px(theme::ICON)).text_color(t.on_stage))
        .on_click(on_click)
}

impl Render for StoriesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        self.offer_groups(window, cx);
        let shared = self.shared.clone();
        let narrow = shared.ui.borrow().narrow;
        let (rings, current) = {
            let chat = shared.chat.borrow();
            (self.rings(&chat), self.current(&chat))
        };
        // A picture story is asked for when it comes up, once.
        let wanted = match current.as_ref().map(|c| &c.shown) {
            Some(Shown::Picture {
                id, picture: None, ..
            }) if self.fetched.as_ref() != Some(id) => Some(id.clone()),
            _ => None,
        };
        if let Some(id) = wanted {
            self.fetched = Some(id.clone());
            shared.act(cx, |chat| {
                (chat.fetch_attachment(&id), state::Update::default())
            });
        }
        let shown = current.is_some();
        let list_hidden = narrow && shown;
        let viewport = window.viewport_size();
        let (view_w, view_h): (f32, f32) = (viewport.width.into(), viewport.height.into());
        // What is left for the story: the list, the arrows, the page's padding and the header.
        let room_w =
            view_w - if list_hidden { 0. } else { LIST.min(view_w) } - 2. * (ARROW + 8.) - 32.;
        let room_h = view_h - 16. * 2. - 44. - 10.;
        let (story_w, story_h) = (room_w.max(60.), room_h.max(60.));

        let list = self.list(&t, rings, cx);
        let story = match current {
            None => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.on_stage.opacity(theme::SECONDARY))
                .child(if self.list_has_rings(&shared) {
                    "Pick someone's story."
                } else {
                    ""
                })
                .into_any_element(),
            Some(current) => self.story(&t, current, (story_w, story_h), cx),
        };
        div()
            .id("stories")
            .absolute()
            .inset_0()
            .occlude()
            .bg(t.stage)
            .text_color(t.on_stage)
            .track_focus(&self.focus)
            .key_context("Stories")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let by = match event.keystroke.key.as_str() {
                    "left" => -1,
                    "right" => 1,
                    _ => return,
                };
                if this.shared.chat.borrow().current_story().is_some() {
                    this.step(by, cx);
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_row()
            .when(!list_hidden, |page| page.child(list))
            .child(story)
    }
}

impl StoriesView {
    fn list_has_rings(&self, shared: &Shared) -> bool {
        !shared.chat.borrow().story_rings().is_empty()
    }

    /// Who has stories, and posting one.
    fn list(&self, t: &Tokens, rings: Vec<Ring>, cx: &mut Context<Self>) -> AnyElement {
        let shared = self.shared.clone();
        let t = *t;
        let secondary = t.on_stage.opacity(theme::SECONDARY);
        let none = rings.is_empty();
        let can_post = !self.offered.is_empty();
        let can_post_text = can_post && !self.text.read(cx).value().trim().is_empty();
        let rows = rings.into_iter().enumerate().map(|(index, ring)| {
            let id = ring.id.clone();
            let shared = shared.clone();
            let label = format!(
                "{}{}, {}",
                ring.title,
                if ring.unviewed { ", new" } else { "" },
                ring.time
            );
            div()
                .id(("ring", index))
                .flex_none()
                .h(px(56.))
                .p(px(6.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .rounded(px(theme::RADIUS_M))
                .cursor_pointer()
                .hover(|row| row.bg(t.on_stage.opacity(0.08)))
                .aria_label(SharedString::from(label))
                .on_click(move |_, _, cx| shared.command(cx, |chat| chat.open_story(&id)))
                // Ringed while there is something new.
                .child(
                    div()
                        .flex_none()
                        .size(px(44.))
                        .rounded_full()
                        .border_2()
                        .border_color(if ring.unviewed {
                            brighter(t.accent, 0.3)
                        } else {
                            t.on_stage.opacity(theme::FAINT)
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(avatar(
                            36.,
                            &ring.initials,
                            avatar_color(&ring.id),
                            ring.picture.clone(),
                            &t,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_weight(if ring.unviewed {
                                    FontWeight::BOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .child(ring.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(theme::BODY * theme::SMALL))
                                .text_color(secondary)
                                .child(ring.time.clone()),
                        ),
                )
        });
        div()
            .flex_none()
            .w(px(LIST))
            .max_w_full()
            .h_full()
            .p(px(theme::SPACE_M))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(theme::BODY * theme::HEADING))
                            .font_weight(FontWeight::BOLD)
                            .child("Stories"),
                    )
                    .child(stage_button(
                        "close-stories",
                        "x",
                        "Close stories",
                        &t,
                        cx.listener(|this, _, _, cx| this.close_all(cx)),
                    )),
            )
            .when(none, |list| {
                list.child(
                    div()
                        .text_color(secondary)
                        .child("No stories from the last day."),
                )
            })
            .child(
                div()
                    .id("rings")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .when(can_post, |list| {
                list.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("New story"))
                        .child(Select::new(&self.groups).placeholder("Group"))
                        .child(Input::new(&self.text))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(6.))
                                .child(
                                    Button::new("story-photo").label("Photo…").on_click(
                                        cx.listener(|this, _, _, cx| this.post_photo(cx)),
                                    ),
                                )
                                .child(
                                    Button::new("story-post")
                                        .primary()
                                        .label("Post")
                                        .disabled(!can_post_text)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.post_text(window, cx)
                                        })),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    /// The story shown, with its author and the arrows.
    fn story(
        &self,
        t: &Tokens,
        current: Current,
        (width, height): (f32, f32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *t;
        let secondary = t.on_stage.opacity(theme::SECONDARY);
        let arrow = |id: &'static str, name: &'static str, label: &'static str, by: isize| {
            div()
                .id(id)
                .flex_none()
                .w(px(ARROW))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .aria_label(label)
                .child(icon(name).size(px(28.)).text_color(t.on_stage))
                .on_click(cx.listener(move |this, _, _, cx| this.step(by, cx)))
        };
        let previous = arrow("previous-story", "chevron-left", "Previous story", -1);
        let next = arrow("next-story", "chevron-right", "Next story", 1);
        let body = match current.shown {
            Shown::Text {
                text,
                foreground,
                background,
            } => {
                let (w, h) = phone_shaped(width, height);
                div()
                    .w(px(w))
                    .h(px(h))
                    .rounded(px(theme::RADIUS_L))
                    .bg(background)
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .text_size(px(theme::BODY * theme::DISPLAY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(foreground)
                    .child(text)
                    .into_any_element()
            }
            Shown::Picture {
                id,
                picture,
                blur,
                failed,
            } => self.picture(&t, id, picture, blur, failed, (width, height)),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .p(px(theme::SPACE_L))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::BOLD).child(current.author))
                            .child(
                                div()
                                    .text_size(px(theme::BODY * theme::SMALL))
                                    .text_color(secondary)
                                    .child(format!("{} · {}", current.time, current.position)),
                            ),
                    )
                    .child(stage_button(
                        "close-story",
                        "x",
                        "Close story",
                        &t,
                        cx.listener(|this, _, _, cx| this.close_story(cx)),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .child(previous)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(body),
                    )
                    .child(next),
            )
            .into_any_element()
    }

    /// A picture story: the picture, or what stands in for it until it is there.
    #[allow(clippy::too_many_arguments)]
    fn picture(
        &self,
        t: &Tokens,
        id: AttachmentId,
        picture: Option<Drawn>,
        blur: Option<Drawn>,
        failed: Option<bool>,
        (width, height): (f32, f32),
    ) -> AnyElement {
        let shared = self.shared.clone();
        let frame = div()
            .w(px(width))
            .h(px(height))
            .flex()
            .items_center()
            .justify_center();
        if let Some(picture) = picture {
            return frame
                .child(img(picture).size_full().object_fit(ObjectFit::Contain))
                .into_any_element();
        }
        let stand_in = blur.map(|blur| {
            img(blur)
                .absolute()
                .inset_0()
                .size_full()
                .rounded(px(theme::RADIUS_L))
                .object_fit(ObjectFit::Contain)
        });
        let status: AnyElement = match failed {
            None => Spinner::new().large().into_any_element(),
            Some(retryable) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(theme::SPACE_S))
                .child(
                    div()
                        .text_color(t.on_stage.opacity(theme::SECONDARY))
                        .child("Couldn't load the picture."),
                )
                .when(retryable, |column| {
                    column.child(Button::new("retry-story").label("Try again").on_click(
                        move |_, _, cx| {
                            shared.act(cx, |chat| {
                                (chat.fetch_attachment(&id), state::Update::default())
                            });
                        },
                    ))
                })
                .into_any_element(),
        };
        frame
            .relative()
            .children(stand_in)
            .child(status)
            .into_any_element()
    }
}

fn brighter(color: Hsla, by: f32) -> Hsla {
    gpui_kit::hsla(
        color.h,
        color.s,
        (color.l + (1. - color.l) * by).min(1.),
        color.a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_story_is_phone_shaped_within_its_room() {
        // Wide room: the height limits.
        let (w, h) = phone_shaped(1000., 560.);
        assert_eq!(h, 560.);
        assert!((w - 315.).abs() < 0.01);
        // Narrow room: the width limits.
        let (w, h) = phone_shaped(180., 800.);
        assert_eq!(w, 180.);
        assert!((h - 320.).abs() < 0.01);
    }

    #[test]
    fn story_colours_are_drawn_opaque() {
        let color = opaque(0x0012_3456);
        assert_eq!(color.a, 1.);
        let again = opaque(0xff12_3456);
        assert_eq!((color.h, color.s, color.l), (again.h, again.s, again.l));
    }
}
