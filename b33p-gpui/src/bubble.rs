//! One row of the open chat's messages: a bubble with its quote, link preview, attachments,
//! text, time and ticks, and the reactions under it; a notice; and the day and "new messages"
//! lines between them.

use std::rc::Rc;

use b33p_core::state::{self, ChatState, MessageView, Update};
use b33p_core::{
    AttachmentId, ConversationId, Delivery, LinkPreview, Reaction, Unsent, now_millis,
};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, FontWeight, MouseButton, Role, SharedString,
    div, px,
};

use crate::attachments::{self, Env};
use crate::message_menu::{self, Info};
use crate::shared::Shared;
use crate::text::{self, Look, RichText, Target};
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, avatar, avatar_color, icon};

/// The row of message `index` of the open chat.
pub fn row(index: usize, shared: &Rc<Shared>, cx: &mut App) -> AnyElement {
    let t = theme::tokens(cx);
    let mut to_fetch = Vec::new();
    let element = {
        let chat = shared.chat.borrow();
        let Some(view) = chat.message_view(index) else {
            return div().into_any_element();
        };
        build(index, &view, &chat, shared, &t, &mut to_fetch)
    };
    // The list only draws the rows on screen, so this downloads only what is seen. Asked for
    // with the state no longer borrowed.
    for id in to_fetch {
        attachments::fetch(shared, &id, cx);
    }
    // A message that just came or was just sent fades in; others, as a chat opens or scrolls,
    // are simply there. (Only a fade: moving the row would upset how the list measures it.)
    if let Some((first, at)) = shared.arrived.get()
        && index >= first
        && shared.animates()
        && cx.background_executor().now().duration_since(at) < ARRIVAL_IS_NEWS
    {
        return crate::animation::fade_in(
            SharedString::from(format!("arrives-{index}")),
            crate::animation::SLOW,
            div().w_full().child(element),
        )
        .into_any_element();
    }
    element
}

/// How long after it came a message is drawn fading in, if it is drawn then at all.
const ARRIVAL_IS_NEWS: std::time::Duration = std::time::Duration::from_secs(1);

fn build(
    index: usize,
    view: &MessageView,
    chat: &ChatState,
    shared: &Rc<Shared>,
    t: &Tokens,
    to_fetch: &mut Vec<AttachmentId>,
) -> AnyElement {
    let m = view.message;
    let selecting = chat.has_selection();
    let info = Info {
        index,
        key: key(view),
        notice: m.notice,
        deleted: m.deleted,
        selected: view.selected,
        text: if m.deleted {
            String::new()
        } else {
            view.body.to_owned()
        },
        can_forward: ChatState::can_forward(m),
        can_edit: chat.can_edit(m),
        save: m
            .attachments
            .iter()
            .find(|a| attachments::can_save(a.kind))
            .map(|a| a.id.clone()),
        can_send_again: m.unsent == Some(Unsent::Failed),
    };
    // Inside the row, which the scroller gives no sides, so that a picked message's tint
    // reaches the edges of the list.
    let mut column = div().w_full().px(px(theme::SPACE_M)).flex().flex_col();
    if view.first_unread {
        let line = || {
            div()
                .flex_1()
                .h(px(1.))
                .bg(t.accent.opacity(theme::TERTIARY))
        };
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .pt(px(10.))
                .text_size(px(theme::BODY * theme::SMALL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.accent)
                .child(line())
                .child("New messages")
                .child(line()),
        );
    }
    if let Some(day) = &view.day {
        column = column.child(
            div().flex().justify_center().pt(px(14.)).pb(px(6.)).child(
                div()
                    .h(px(22.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(t.surface_raised)
                    .border_1()
                    .border_color(t.divider)
                    .text_size(px(theme::BODY * theme::SMALL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text_secondary)
                    .child(SharedString::from(day.clone())),
            ),
        );
    }
    column = if m.notice {
        column.child(notice(view, t))
    } else {
        column.child(message(
            index, view, chat, &info, selecting, shared, t, to_fetch,
        ))
    };
    let menu_shared = shared.clone();
    div()
        .id(SharedString::from(format!("row-{index}")))
        .group(group(index))
        .w_full()
        // The buttons are drawn for this row only while the pointer is on it (or they were
        // pressed); see `message_menu::toolbar`.
        .debug_selector(|| format!("Message row {index}"))
        .on_hover({
            let shared = shared.clone();
            let can_react = !m.deleted && !m.notice && !selecting;
            move |hovered, window, cx| {
                let now = hovered.then_some(index);
                if now != shared.hovered_row.get()
                    && (*hovered || shared.hovered_row.get() == Some(index))
                {
                    shared.hovered_row.set(now);
                    window.refresh();
                    // The quick reactions come after a dwell, and go with the pointer.
                    crate::reaction_bar::hover(&shared, index, *hovered, can_react, window, cx);
                }
            }
        })
        // While messages are picked, or Ctrl is held, a click anywhere on the row picks this
        // message or unpicks it; with Shift, everything from the one last picked. Taken
        // before what is under the pointer sees it.
        .capture_any_mouse_down({
            let shared = shared.clone();
            move |event, _, cx| {
                if event.button != MouseButton::Left
                    || !(selecting || text::picks(event.modifiers, cx))
                {
                    return;
                }
                if event.modifiers.shift {
                    message_menu::select_through(&shared, index, cx);
                } else {
                    message_menu::toggle_select(&shared, index, cx);
                }
                cx.stop_propagation();
            }
        })
        // Picked: the row is tinted.
        .when(view.selected, |row| row.bg(t.selected))
        .child(column)
        .context_menu(move |menu, window, cx| {
            message_menu::menu(menu, &menu_shared, &info, window, cx)
        })
        .into_any_element()
}

/// The name of the row's hover group, which the buttons that show on hover go by.
fn group(index: usize) -> SharedString {
    SharedString::from(format!("message-{index}"))
}

/// The message's identity, which tells its text from another's among those that can be selected.
fn key(view: &MessageView) -> SharedString {
    SharedString::from(format!(
        "{}:{}",
        view.message.timestamp, view.message.sender_id
    ))
}

/// A line about the conversation that nobody wrote: a changed safety number, a new timer.
fn notice(view: &MessageView, t: &Tokens) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .justify_center()
        .items_center()
        .gap(px(6.))
        .px(px(theme::SPACE_XL))
        .py(px(6.))
        .when(view.message.timer_change.is_some(), |row| {
            row.child(
                icon("timer")
                    .size(px(theme::ICON_SMALL))
                    .text_color(t.text_tertiary),
            )
        })
        .child(
            div()
                .debug_selector(|| "Notice".into())
                .min_w_0()
                .text_center()
                .text_size(px(theme::BODY * theme::SMALL))
                .text_color(t.text_secondary)
                .child(SharedString::from(view.body.to_owned())),
        )
        .into_any_element()
}

/// Opens a link from a message: a person's mention opens their chat, a group's invite link the
/// dialog that joins it, and only web addresses go to the browser, since links come from
/// message text and nothing else (files, other apps' schemes) may be opened by it.
pub fn open_link(shared: &Shared, target: &Target, cx: &mut App) {
    match target {
        Target::Mention(service_id) => {
            let id = ConversationId::contact(service_id);
            shared.act(cx, |chat| (chat.select(&id), Update::reset()));
        }
        Target::Url(link) => open_url(shared, link, cx),
        Target::Spoiler => {}
    }
}

/// See [`open_link`].
pub fn open_url(shared: &Shared, link: &str, cx: &mut App) {
    if link.starts_with("https://signal.group/#") || link.starts_with("sgnl://signal.group/#") {
        shared.command(cx, |chat| chat.open_join(Some(link)));
    } else if (link.starts_with("https://") || link.starts_with("http://"))
        && let Err(error) = open::that_detached(link)
    {
        tracing::warn!(error = %b33p_core::scrub::chain(&error), "couldn't open a link");
    }
}

/// What a screen reader says for a message: its text, then who, when and how far it got.
fn described(view: &MessageView) -> String {
    let m = view.message;
    let text = if m.deleted {
        "This message was deleted."
    } else {
        view.body
    };
    let who = if m.outgoing { "You" } else { &m.sender };
    let mut out = format!("{text}. {who}, {}", state::format_clock(m.timestamp));
    if m.edited_at.is_some() {
        out.push_str(", edited");
    }
    if m.outgoing {
        out.push_str(match (m.unsent, m.delivery) {
            (Some(Unsent::Sending), _) => ", sending",
            (Some(_), _) => ", not sent",
            (None, Delivery::Sent) => ", sent",
            (None, Delivery::Delivered) => ", delivered",
            (None, Delivery::Read) => ", read",
        });
    }
    match m.reactions.len() {
        0 => {}
        1 => out.push_str(", 1 reaction"),
        n => out.push_str(&format!(", {n} reactions")),
    }
    if let Some(at) = m.expires_at {
        out.push_str(&format!(
            ", disappears in {}",
            state::format_remaining(at, now_millis())
        ));
    }
    out
}

/// The ticks, texts and buttons below a message's text.
struct Footer {
    last: bool,
    outgoing: bool,
    edited: bool,
    expires: Option<String>,
    time: String,
    delivery: Delivery,
    unsent: Option<Unsent>,
    index: usize,
}

#[allow(clippy::too_many_arguments)]
fn message(
    index: usize,
    view: &MessageView,
    chat: &ChatState,
    info: &Info,
    selecting: bool,
    shared: &Rc<Shared>,
    t: &Tokens,
    to_fetch: &mut Vec<AttachmentId>,
) -> AnyElement {
    let m = view.message;
    let outgoing = m.outgoing;
    let in_group = chat.selected().is_some_and(|c| c.is_group());
    let sender = ConversationId::contact(&m.sender_id);
    let (background, ink) = if outgoing {
        (t.bubble_outgoing, t.bubble_outgoing_text)
    } else {
        (t.bubble_incoming, t.bubble_incoming_text)
    };
    let faint = ink.opacity(theme::SECONDARY);
    let sending = outgoing && m.unsent == Some(Unsent::Sending);
    let scope = format!("{index}");

    // Within a run, the corners facing the neighbouring bubbles are tighter.
    let radius = px(theme::BUBBLE_RADIUS);
    let tight = px(theme::BUBBLE_RADIUS_TIGHT);
    let side = |tightened: bool| if tightened { tight } else { radius };
    let incoming = !outgoing;
    // A search result jumped to is lit.
    let unlit = background;
    let background = if view.highlighted {
        background.blend(t.highlight.opacity(0.5))
    } else {
        background
    };

    let time = state::format_clock(m.timestamp);
    let described = described(view);
    let mut bubble = div()
        .id(SharedString::from(format!("bubble-{index}")))
        .speaks_as(
            Role::Article,
            SharedString::from(described),
            format!("Message {index}"),
        )
        // The time of a message within a run, which shows it only on the run's last.
        .when(!view.last_of_run && !time.is_empty(), |b| {
            let time = SharedString::from(time.clone());
            b.tooltip(move |window, cx| Tooltip::new(time.clone()).build(window, cx))
        })
        .flex()
        .flex_col()
        .gap(px(2.))
        .min_w_0()
        .max_w_full()
        .px(px(theme::SPACE_M))
        .pt(px(7.))
        .pb(px(6.))
        .bg(background)
        .text_color(ink)
        .rounded_tl(side(incoming && !view.first_of_run))
        .rounded_bl(side(incoming && !view.last_of_run))
        .rounded_tr(side(outgoing && !view.first_of_run))
        .rounded_br(side(outgoing && !view.last_of_run))
        .when(sending, |b| b.opacity(theme::SECONDARY))
        .when(view.highlighted, |b| b.border_2().border_color(t.accent))
        .when(!view.highlighted && incoming, |b| {
            b.border_1().border_color(t.divider)
        });

    if let Some(quote) = &m.quote {
        bubble = bubble.child(
            div()
                .flex()
                .flex_row()
                .rounded(px(theme::RADIUS_S))
                .bg(ink.opacity(0.1))
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .w(px(3.))
                        .bg(if outgoing { ink } else { t.accent }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .p(px(6.))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(theme::BODY * theme::SMALL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ink)
                                .child(SharedString::from(quote.author.clone())),
                        )
                        .child(
                            div()
                                .line_clamp(2)
                                .text_size(px(theme::BODY * theme::BODY_SMALL))
                                .text_color(faint)
                                .child(SharedString::from(quote.text.clone())),
                        ),
                ),
        );
    }

    let env = Env {
        shared,
        t,
        ink,
        scope: &scope,
        clickable: !selecting,
    };
    if let Some(preview) = &m.preview {
        bubble = bubble.child(link_preview(
            index, preview, chat, &env, faint, selecting, to_fetch,
        ));
    }
    for attachment in &m.attachments {
        let item = attachments::item(
            attachment,
            chat.attachment(&attachment.id),
            chat.saved(&attachment.id),
            &shared.images,
        );
        if let Some(view) = attachments::view(item, &env, to_fetch) {
            bubble = bubble.child(view);
        }
    }

    if !view.body.is_empty() {
        let rich = text::build(view.body, view.styles, view.mentions, view.revealed);
        let look = Look {
            link: if outgoing { ink } else { t.accent },
            ink,
            selection: ink.opacity(theme::FAINT),
        };
        let on_target = {
            let shared = shared.clone();
            move |target: &Target, _: &mut gpui_kit::Window, cx: &mut App| match target {
                Target::Spoiler => message_menu::reveal(&shared, index, cx),
                target => open_link(&shared, target, cx),
            }
        };
        let body = RichText::new(key(view), rich, look, on_target)
            .interactive(!selecting)
            .selectable(!m.deleted);
        bubble = bubble.child(
            div()
                .min_w_0()
                .when(m.deleted, |d| d.italic().text_color(faint))
                .child(body),
        );
    }

    let footer = Footer {
        last: view.last_of_run,
        outgoing,
        edited: m.edited_at.is_some(),
        expires: m
            .expires_at
            .map(|at| state::format_remaining(at, now_millis())),
        time,
        delivery: m.delivery,
        unsent: m.unsent,
        index,
    };
    // Once per run, on its last message; always if it changed, goes away or failed to go out.
    let failed = outgoing && matches!(m.unsent, Some(Unsent::Failed | Unsent::Refused));
    if footer.last || footer.edited || footer.expires.is_some() || failed {
        bubble = bubble.child(footer_row(&footer, shared, t, ink));
    }

    // A message jumped to lights up at once, then settles into its highlight, so that the eye
    // finds it. With motion off it is only the highlight.
    let bubble = if view.highlighted && shared.animates() {
        let highlight = t.highlight;
        bubble
            .with_animation(
                SharedString::from(format!("flash-{index}")),
                Animation::new(theme::FLASH).with_easing(gpui_kit::ease_out_quint()),
                move |bubble, delta| bubble.bg(unlit.blend(highlight.opacity(1. - 0.5 * delta))),
            )
            .into_any_element()
    } else {
        bubble.into_any_element()
    };
    // Picked: a check at the corner facing the middle.
    let bubble = div()
        .relative()
        .flex()
        .flex_col()
        .min_w_0()
        .max_w_full()
        .child(bubble)
        .when(view.selected, |b| {
            b.child(
                div()
                    .absolute()
                    .top(px(-6.))
                    .when(outgoing, |c| c.left(px(-8.)))
                    .when(!outgoing, |c| c.right(px(-8.)))
                    .size(px(20.))
                    .rounded_full()
                    .bg(t.accent)
                    .border_2()
                    .border_color(t.surface)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(t.on_accent)
                    .child(icon("check").size(px(12.))),
            )
        });

    // The quick reactions, over the bubble's top edge once the pointer has rested on it.
    let bubble = bubble.children(crate::reaction_bar::bar(
        shared,
        index,
        outgoing,
        !m.deleted && !m.notice && !selecting,
        t,
    ));
    let toolbar = (!selecting).then(|| message_menu::toolbar(shared, info, outgoing, group(index)));
    let reactions = reaction_chips(index, &m.reactions, outgoing, selecting, shared, t);
    let content = div()
        .min_w_0()
        .max_w(gpui_kit::relative(0.7))
        .flex()
        .flex_col()
        .gap(px(2.))
        .when(outgoing, |c| c.items_end())
        .when(!outgoing, |c| c.items_start())
        .when(view.show_sender, |c| {
            c.child(
                div()
                    .text_size(px(theme::BODY * theme::SMALL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(avatar_color(&sender))
                    .child(SharedString::from(m.sender.clone())),
            )
        })
        .child(bubble)
        .children(reactions);
    let row = div()
        .w_full()
        .flex()
        .flex_row()
        .items_end()
        .gap(px(theme::SPACE_XS))
        .pt(px(if view.first_of_run { 6. } else { 1. }))
        .pb(px(1.))
        .when(outgoing, |row| row.justify_end())
        .when(in_group && !outgoing, |row| {
            // The sender's picture, at the end of their run.
            let picture = shared
                .images
                .avatar(&format!("person:{}", m.sender_id), view.sender_avatar);
            row.child(
                div()
                    .flex_none()
                    .size(px(28.))
                    .children(view.last_of_run.then(|| {
                        avatar(
                            28.,
                            &state::initials(&m.sender),
                            avatar_color(&sender),
                            picture,
                            t,
                        )
                    })),
            )
        });
    if outgoing {
        row.children(toolbar).child(content)
    } else {
        row.child(content).children(toolbar)
    }
    .into_any_element()
}

/// The time, ticks and the like, at the end of a bubble.
fn footer_row(f: &Footer, shared: &Rc<Shared>, t: &Tokens, ink: gpui_kit::Hsla) -> AnyElement {
    let faint = ink.opacity(theme::SECONDARY);
    let caption = px(theme::BODY * theme::CAPTION);
    let small = px(theme::BODY * theme::SMALL);
    let index = f.index;
    let sending = f.outgoing && f.unsent == Some(Unsent::Sending);
    let failed = f.outgoing && matches!(f.unsent, Some(Unsent::Failed | Unsent::Refused));
    let can_send_again = f.unsent == Some(Unsent::Failed);
    div()
        .flex()
        .flex_row()
        .justify_end()
        .items_center()
        .gap(px(theme::SPACE_XS))
        .children(f.expires.as_ref().map(|left| {
            div()
                .id(SharedString::from(format!("expires-{index}")))
                .speaks_as(
                    Role::Label,
                    format!("Disappears in {left}"),
                    format!("Disappears {index}"),
                )
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.))
                .text_color(faint)
                .child(icon("timer").size(px(11.)))
                .child(
                    div()
                        .text_size(caption)
                        .child(SharedString::from(left.clone())),
                )
        }))
        .when(f.edited, |row| {
            row.child(
                div()
                    .italic()
                    .text_size(caption)
                    .text_color(faint)
                    .child("edited"),
            )
        })
        .when(f.last, |row| {
            row.child(
                div()
                    .text_size(small)
                    .text_color(faint)
                    .child(SharedString::from(f.time.clone())),
            )
        })
        .when(f.last && f.outgoing && f.unsent.is_none(), |row| {
            // Sent, delivered; read: cut out of a filled pill, so it tells from delivered at a
            // glance.
            let read = f.delivery == Delivery::Read;
            let label = match f.delivery {
                Delivery::Sent => "Sent",
                Delivery::Delivered => "Delivered",
                Delivery::Read => "Read",
            };
            row.child(
                div()
                    .id(SharedString::from(format!("ticks-{index}")))
                    .flex_none()
                    .w(px(18.))
                    .h(px(13.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(read, |pill| pill.bg(ink))
                    .speaks_as(Role::Label, label, format!("Delivery {index}"))
                    .child(
                        icon(if f.delivery == Delivery::Sent {
                            "check"
                        } else {
                            "check-check"
                        })
                        .size(px(11.))
                        .text_color(if read {
                            t.bubble_outgoing
                        } else {
                            faint
                        }),
                    ),
            )
        })
        .when(f.last && sending, |row| {
            row.child(
                div()
                    .italic()
                    .text_size(caption)
                    .text_color(faint)
                    .child("Sending…"),
            )
        })
        .when(failed, |row| {
            // On every bubble that failed, not only a run's last. Refused by Signal: sending
            // it again won't help.
            let shared = shared.clone();
            row.child(
                div()
                    .id(SharedString::from(format!("retry-{index}")))
                    .debug_selector(|| format!("Not sent {index}"))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACE_XS))
                    .px(px(5.))
                    .rounded_full()
                    .bg(t.danger)
                    .text_color(t.on_accent)
                    .when(can_send_again, |pill| {
                        pill.cursor_pointer()
                            .speaks_as(Role::Button, "Send again", format!("Send again {index}"))
                            .on_click(move |_, _, cx| message_menu::retry(&shared, index, cx))
                            .child(icon("rotate-cw").size(px(11.)))
                    })
                    .child(
                        div()
                            .text_size(caption)
                            .font_weight(FontWeight::BOLD)
                            .child(if can_send_again {
                                "Not sent · Retry"
                            } else {
                                "Not sent"
                            }),
                    ),
            )
        })
        .into_any_element()
}

/// The card for a link in a message: its picture, title, description and site; a click opens
/// the link.
fn link_preview(
    index: usize,
    preview: &LinkPreview,
    chat: &ChatState,
    env: &Env,
    faint: gpui_kit::Hsla,
    selecting: bool,
    to_fetch: &mut Vec<AttachmentId>,
) -> AnyElement {
    let (shared, ink) = (env.shared, env.ink);
    let site = preview
        .url
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
        .to_owned();
    let image = preview.image.as_ref().and_then(|attachment| {
        let mut item = attachments::item(
            attachment,
            chat.attachment(&attachment.id),
            chat.saved(&attachment.id),
            &shared.images,
        );
        // A preview's picture stays small, and a click on it is a click on the card.
        let scale = (150. / item.height.max(1.)).min(1.);
        item.can_save = false;
        item.width *= scale;
        item.height *= scale;
        let env = Env {
            scope: "preview",
            clickable: false,
            ..*env
        };
        attachments::view(item, &env, to_fetch)
    });
    let url = preview.url.clone();
    let click_shared = shared.clone();
    div()
        .id(SharedString::from(format!("preview-{index}")))
        .flex()
        .flex_col()
        .gap(px(3.))
        .min_w_0()
        .p(px(theme::SPACE_S))
        .rounded(px(theme::RADIUS_M))
        .bg(ink.opacity(0.1))
        .when(!selecting, |card| {
            card.cursor_pointer()
                .speaks_as(
                    Role::Link,
                    format!("Open link {}", preview.url),
                    format!("Link preview {index}"),
                )
                .on_click(move |_, _, cx| open_url(&click_shared, &url, cx))
        })
        .children(image)
        .when(!preview.title.is_empty(), |card| {
            card.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink)
                    .child(SharedString::from(preview.title.clone())),
            )
        })
        .when(!preview.description.is_empty(), |card| {
            card.child(
                div()
                    .line_clamp(3)
                    .text_size(px(theme::BODY * theme::BODY_SMALL))
                    .text_color(faint)
                    .child(SharedString::from(preview.description.clone())),
            )
        })
        .when(!site.is_empty(), |card| {
            card.child(
                div()
                    .truncate()
                    .text_size(px(theme::BODY * theme::CAPTION))
                    .text_color(faint)
                    .child(SharedString::from(site)),
            )
        })
        .into_any_element()
}

/// One chip per emoji, in the order they first came, counting everyone who chose it: the emoji,
/// how many, and whether one of them is ours.
pub fn chips(reactions: &[Reaction]) -> Vec<(String, u32, bool)> {
    let mut chips: Vec<(String, u32, bool)> = Vec::new();
    for reaction in reactions {
        match chips
            .iter_mut()
            .find(|(emoji, ..)| *emoji == reaction.emoji)
        {
            Some(chip) => {
                chip.1 += 1;
                chip.2 |= reaction.outgoing;
            }
            None => chips.push((reaction.emoji.clone(), 1, reaction.outgoing)),
        }
    }
    chips
}

/// The chips under a bubble; a click on one reacts with it, or takes our reaction back.
fn reaction_chips(
    index: usize,
    reactions: &[Reaction],
    outgoing: bool,
    selecting: bool,
    shared: &Rc<Shared>,
    t: &Tokens,
) -> Option<AnyElement> {
    if reactions.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(theme::SPACE_XS))
            .px(px(6.))
            .when(outgoing, |row| row.justify_end())
            .children(chips(reactions).into_iter().map(|(emoji, count, mine)| {
                let label = format!("{emoji} {count}{}", if mine { ", yours" } else { "" });
                let react = emoji.clone();
                let shared = shared.clone();
                div()
                    .id(SharedString::from(format!("chip-{index}-{emoji}")))
                    .h(px(24.))
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(if mine { t.accent } else { t.divider })
                    .bg(if mine { t.selected } else { t.surface_raised })
                    .text_size(px(theme::BODY * theme::BODY_SMALL))
                    .text_color(t.text)
                    .speaks(Role::Button, label)
                    .when(!selecting, |chip| {
                        chip.cursor_pointer().on_click(move |_, _, cx| {
                            message_menu::react(&shared, index, &react, cx)
                        })
                    })
                    .child(SharedString::from(if count > 1 {
                        format!("{emoji} {count}")
                    } else {
                        emoji
                    }))
            }))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reaction(emoji: &str, who: &str, outgoing: bool) -> Reaction {
        Reaction {
            emoji: emoji.into(),
            sender_id: who.into(),
            sender: who.into(),
            outgoing,
        }
    }

    #[test]
    fn reactions_are_counted_per_emoji_in_the_order_they_came() {
        let reactions = [
            reaction("👍", "a", false),
            reaction("❤️", "b", false),
            reaction("👍", "c", true),
        ];
        assert_eq!(
            chips(&reactions),
            vec![("👍".to_owned(), 2, true), ("❤️".to_owned(), 1, false)]
        );
        assert!(chips(&[]).is_empty());
    }
}
