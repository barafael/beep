//! A message's attachments: pictures, videos and stickers drawn in place, files, audio and voice
//! messages with a player. [`item`] says what to show of one (sizes, texts, what is downloaded);
//! [`view`] draws it.

use std::cell::Cell;
use std::rc::Rc;

use b33p_core::state::{self, AttachmentState, SaveState};
use b33p_core::{Attachment, AttachmentContent, AttachmentId, AttachmentKind};
use gpui_kit::component::Sizable as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Bounds, ElementId, FontWeight, Hsla, MouseButton, ObjectFit, SharedString, canvas,
    div, img, px,
};

use crate::images::{Drawn, Images};
use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::icon;

/// What to show of an attachment, from what the state knows of it.
pub struct AttachmentItem {
    pub id: AttachmentId,
    /// Drawn in the message itself (pictures, stickers, videos), not as a line of text.
    pub inline: bool,
    /// A long message's full text: it replaces the message's text and isn't shown itself.
    pub hidden: bool,
    /// Asked for as soon as it is shown.
    pub fetch: bool,
    pub picture: Option<Drawn>,
    /// Pixels, fit into the bubble.
    pub width: f32,
    pub height: f32,
    pub label: String,
    /// How saving went, or why the download failed; empty if there is nothing to say.
    pub status: String,
    pub can_retry: bool,
    pub can_save: bool,
    /// The blurred stand-in, until the picture is there.
    pub placeholder: Option<Drawn>,
    pub caption: String,
    pub video: bool,
    pub audio: bool,
    pub gif: bool,
}

/// The size an inline attachment is drawn at: the shape the sender reported (or the picture
/// has), no larger than `max` on its longer side, and never enlarged.
pub fn fit(sticker: bool, reported: (u32, u32), picture: Option<(u32, u32)>) -> (f32, f32) {
    let (max, fallback) = if sticker {
        (128.0, (128.0, 128.0))
    } else {
        (280.0, (240.0, 180.0))
    };
    let (w, h) = match picture {
        Some((w, h)) => (w as f32, h as f32),
        None if reported.0 > 0 && reported.1 > 0 => (reported.0 as f32, reported.1 as f32),
        None => fallback,
    };
    let scale = (max / w.max(h)).min(1.0);
    (w * scale, h * scale)
}

/// What an attachment is called: its file name and size, or what kind it is and its size.
pub fn label(kind: AttachmentKind, file_name: Option<&str>, size: u64) -> String {
    let size = state::format_size(size);
    match (kind, file_name) {
        (AttachmentKind::ViewOnce, _) => "View-once media: open it on your phone".to_string(),
        (AttachmentKind::File, Some(name)) => format!("{name} · {size}"),
        _ => format!("{} · {size}", state::describe_kind(kind)),
    }
}

/// Saving says how it went; otherwise a failed download says why (and to click to retry, when
/// that may work).
pub fn status(download: Option<&AttachmentState>, save: Option<&SaveState>) -> String {
    match save {
        Some(SaveState::Saving) => "Saving…".into(),
        Some(SaveState::Saved(path)) => format!(
            "Saved to {}",
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("disk")
        ),
        Some(SaveState::Failed(error)) => format!("Couldn't save: {error}"),
        None => match download {
            Some(AttachmentState::Failed(error)) if error.retryable() => {
                format!("{error} Click to retry.")
            }
            Some(AttachmentState::Failed(error)) => error.to_string(),
            _ => String::new(),
        },
    }
}

/// Whether saving it makes sense.
pub fn can_save(kind: AttachmentKind) -> bool {
    !matches!(
        kind,
        AttachmentKind::ViewOnce | AttachmentKind::LongText | AttachmentKind::Sticker
    )
}

pub fn item(
    attachment: &Attachment,
    download: Option<&AttachmentState>,
    save: Option<&SaveState>,
    images: &Images,
) -> AttachmentItem {
    let kind = attachment.kind;
    let inline = matches!(
        kind,
        AttachmentKind::Image | AttachmentKind::Sticker | AttachmentKind::Video
    );
    let picture = match download {
        Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => Some(picture),
        _ => None,
    };
    let (width, height) = fit(
        kind == AttachmentKind::Sticker,
        (attachment.width, attachment.height),
        picture.map(|p| (p.width, p.height)),
    );
    // The blurred stand-in has the shape the picture will have.
    let shape = picture.map_or((attachment.width, attachment.height), |p| {
        (p.width, p.height)
    });
    let shape = if shape.0 > 0 && shape.1 > 0 {
        shape
    } else {
        (width as u32, height as u32)
    };
    let can_retry = matches!(download, Some(AttachmentState::Failed(error)) if error.retryable())
        && state::shown_inline(kind);
    AttachmentItem {
        id: attachment.id.clone(),
        inline,
        hidden: kind == AttachmentKind::LongText,
        fetch: download.is_none() && state::shown_inline(kind),
        picture: picture.and_then(|p| images.attachment(&attachment.id, p)),
        width,
        height,
        label: label(kind, attachment.file_name.as_deref(), attachment.size),
        status: status(download, save),
        can_retry,
        can_save: can_save(kind),
        placeholder: attachment
            .blurhash
            .as_deref()
            .and_then(|hash| images.blur(hash, shape.0, shape.1)),
        caption: attachment.caption.clone().unwrap_or_default(),
        video: kind == AttachmentKind::Video,
        audio: matches!(kind, AttachmentKind::Audio | AttachmentKind::VoiceNote),
        gif: attachment.gif,
    }
}

/// What drawing an attachment needs besides the attachment.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    pub shared: &'a Rc<Shared>,
    pub t: &'a Tokens,
    /// The text colour on the bubble this is in.
    pub ink: Hsla,
    /// Tells this message's attachments from another's, for element ids.
    pub scope: &'a str,
    /// Whether a click on it does something: not while messages are picked (the row takes the
    /// click), nor on a link preview's picture (the card does).
    pub clickable: bool,
}

fn small() -> f32 {
    theme::BODY * theme::BODY_SMALL
}

/// A link-like action beside an attachment ("Open", "Save").
fn action(
    id: impl Into<ElementId>,
    text: &'static str,
    label: String,
    env: &Env,
    run: impl Fn(&mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    let ink = env.ink;
    div()
        .id(id)
        .flex_none()
        .cursor_pointer()
        .text_size(px(small()))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(ink.opacity(theme::SECONDARY))
        .hover(move |s| s.text_color(ink))
        .aria_label(format!("{text} {label}"))
        .on_click(move |_, window, cx| run(window, cx))
        .child(text)
        .into_any_element()
}

/// Draws the attachment. Those that are to be downloaded as soon as they show are added to
/// `to_fetch`, for the caller to ask for once it no longer holds the state.
pub fn view(
    item: AttachmentItem,
    env: &Env,
    to_fetch: &mut Vec<AttachmentId>,
) -> Option<AnyElement> {
    if item.fetch {
        to_fetch.push(item.id.clone());
    }
    if item.hidden {
        return None;
    }
    let ink = env.ink;
    let t = env.t;
    let shared = env.shared;
    let scope = env.scope;
    let open = {
        let shared = shared.clone();
        let id = item.id.clone();
        Rc::new(move |_: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
            shared.command(cx, |chat| chat.open_attachment(&id));
        }) as Rc<dyn Fn(&mut gpui_kit::Window, &mut gpui_kit::App)>
    };
    let open_it = open.clone();
    let mut column = div().flex().flex_col().gap(px(theme::SPACE_XS)).min_w_0();

    if item.inline {
        let has_picture = item.picture.is_some();
        let can_open = (has_picture || item.video) && env.clickable;
        let retry = item.can_retry && !has_picture && env.clickable;
        let mut frame = div()
            .id(SharedString::from(format!("att-{scope}-{}", item.id.0)))
            .relative()
            .flex_none()
            .w(px(item.width))
            .h(px(item.height))
            .rounded(px(theme::RADIUS_M))
            .overflow_hidden()
            .bg(ink.opacity(0.1))
            .flex()
            .items_center()
            .justify_center();
        if let Some(blur) = item.placeholder.clone()
            && !has_picture
        {
            frame = frame.child(
                img(blur)
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            );
        }
        if let Some(picture) = item.picture.clone() {
            frame = frame.child(
                img(picture)
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            );
        }
        if item.video {
            // Videos play in the desktop's player.
            frame = frame.child(
                div()
                    .size(px(48.))
                    .rounded_full()
                    .bg(t.scrim)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(t.on_stage)
                    .map(|badge| {
                        if item.gif {
                            badge
                                .text_size(px(small()))
                                .font_weight(FontWeight::BOLD)
                                .child("GIF")
                        } else {
                            // A triangle's weight is left of its box's centre.
                            badge.pl(px(3.)).child(icon("play").size(px(22.)))
                        }
                    }),
            );
        }
        if !has_picture && item.status.is_empty() && !item.video {
            frame = frame.child(Spinner::new().small());
        }
        if !has_picture && !item.status.is_empty() {
            frame = frame.child(
                div()
                    .px(px(theme::SPACE_S))
                    .text_center()
                    .text_size(px(theme::BODY * theme::SMALL))
                    .text_color(ink.opacity(theme::SECONDARY))
                    .child(SharedString::from(item.status.clone())),
            );
        }
        if can_open || retry {
            let label = if item.video {
                "Play video"
            } else if has_picture {
                "Open picture"
            } else {
                "Download again"
            };
            let (shared, id) = (shared.clone(), item.id.clone());
            frame = frame
                .cursor_pointer()
                .aria_label(label)
                .on_click(move |_, window, cx| {
                    if can_open {
                        open_it(window, cx);
                    } else {
                        fetch(&shared, &id, cx);
                    }
                });
        }
        column = column.child(frame);
    }

    if item.audio {
        column = column.child(player(&item, env));
    }

    if !item.caption.is_empty() {
        column = column.child(
            div()
                .text_color(ink)
                .child(SharedString::from(item.caption.clone())),
        );
    }

    if (!item.inline && !item.audio) || item.can_save {
        let file = !item.inline && !item.audio;
        let status = if item.inline && item.picture.is_none() {
            String::new()
        } else {
            item.status.clone()
        };
        column = column.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .when(file, |row| {
                    row.child(icon("file").size(px(theme::ICON_SMALL)).text_color(ink))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(ink)
                                .child(SharedString::from(item.label.clone())),
                        )
                })
                .when(!item.inline && item.can_save && env.clickable, |row| {
                    let open = open.clone();
                    row.child(action(
                        SharedString::from(format!("open-{scope}-{}", item.id.0)),
                        "Open",
                        item.label.clone(),
                        env,
                        move |window, cx| open(window, cx),
                    ))
                })
                .when(status.is_empty() && !file, |row| row.child(div().flex_1()))
                .when(!status.is_empty(), |row| {
                    row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::BODY * theme::SMALL))
                            .text_color(ink.opacity(theme::SECONDARY))
                            .child(SharedString::from(status.clone())),
                    )
                })
                .when(item.can_save && env.clickable, |row| {
                    let (shared, id) = (shared.clone(), item.id.clone());
                    row.child(action(
                        SharedString::from(format!("save-{scope}-{}", item.id.0)),
                        "Save",
                        item.label.clone(),
                        env,
                        move |_, cx| save(&shared, &id, cx),
                    ))
                }),
        );
    }
    Some(column.into_any_element())
}

/// Asks for the download of attachment `id`, and redraws when one starts.
pub fn fetch(shared: &Shared, id: &AttachmentId, cx: &mut gpui_kit::App) {
    shared.act(cx, |chat| {
        // Nothing to redraw unless a download starts (a row coming into view asks).
        let command = chat.fetch_attachment(id);
        let rows = command.is_some();
        (
            command,
            state::Update {
                rows,
                ..state::Update::default()
            },
        )
    });
}

/// Saves attachment `id` to disk.
pub fn save(shared: &Shared, id: &AttachmentId, cx: &mut gpui_kit::App) {
    shared.act(cx, |chat| {
        (
            chat.save_attachment(id),
            state::Update {
                rows: true,
                ..state::Update::default()
            },
        )
    });
}

/// A play button, a bar to seek along, and how far it got (or what it is, before it plays).
fn player(item: &AttachmentItem, env: &Env) -> AnyElement {
    let ink = env.ink;
    let shared = env.shared;
    let playback = shared.playback.borrow().clone();
    let current = playback.id == item.id.0;
    let playing = current && !playback.paused;
    let progress = if current { playback.progress } else { 0. };
    let bounds = Rc::new(Cell::new(Bounds::<gpui_kit::Pixels>::default()));
    let seek = {
        let (shared, id, bounds) = (shared.clone(), item.id.clone(), bounds.clone());
        Rc::new(move |x: gpui_kit::Pixels, cx: &mut gpui_kit::App| {
            let track = bounds.get();
            let width: f32 = track.size.width.into();
            if width <= 0. {
                return;
            }
            let at: f32 = (x - track.left()).into();
            shared.seek_playback(id.clone(), (at / width).clamp(0., 1.), cx);
        })
    };
    let scope = env.scope;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_S))
        .child(
            div()
                .id(SharedString::from(format!("play-{scope}-{}", item.id.0)))
                .flex_none()
                .size(px(36.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(ink.opacity(0.1))
                .hover(move |s| s.bg(ink.opacity(0.2)))
                .aria_label(if playing { "Pause" } else { "Play" })
                .on_click({
                    let (shared, id) = (shared.clone(), item.id.clone());
                    move |_, _, cx| shared.toggle_playback(id.clone(), cx)
                })
                .text_color(ink)
                .child(
                    div()
                        .when(!playing, |d| d.pl(px(1.)))
                        .child(icon(if playing { "pause" } else { "play" }).size(px(16.))),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::SPACE_XS))
                .min_w(px(160.))
                .child(
                    div()
                        .id(SharedString::from(format!("seek-{scope}-{}", item.id.0)))
                        .relative()
                        .h(px(12.))
                        .w_full()
                        .cursor_pointer()
                        .aria_label("Position")
                        .child(
                            canvas(
                                {
                                    let bounds = bounds.clone();
                                    move |track, _, _| bounds.set(track)
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .left_0()
                                .right_0()
                                .h(px(4.))
                                .rounded_full()
                                .bg(ink.opacity(0.2)),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .left_0()
                                .h(px(4.))
                                .w(gpui_kit::relative(progress.clamp(0., 1.)))
                                .rounded_full()
                                .bg(ink),
                        )
                        .when(current, |track| {
                            // The knob, kept inside the bar at both ends.
                            track.child(
                                div()
                                    .absolute()
                                    .top(px(1.))
                                    .left(gpui_kit::relative(progress.clamp(0., 1.)))
                                    .ml(px(-5. * progress.clamp(0., 1.)))
                                    .size(px(10.))
                                    .rounded_full()
                                    .bg(ink),
                            )
                        })
                        .on_mouse_down(MouseButton::Left, {
                            let seek = seek.clone();
                            move |event, _, cx| {
                                seek(event.position.x, cx);
                                cx.stop_propagation();
                            }
                        })
                        .on_mouse_move(move |event, _, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                seek(event.position.x, cx);
                            }
                        }),
                )
                .child(
                    div()
                        .text_size(px(theme::BODY * theme::SMALL))
                        .text_color(ink.opacity(theme::SECONDARY))
                        .child(SharedString::from(if current {
                            playback.time
                        } else {
                            item.label.clone()
                        })),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_in_the_bubble_without_growing() {
        // Larger than the limit: scaled down by its longer side, keeping the shape.
        assert_eq!(fit(false, (1120, 560), None), (280., 140.));
        assert_eq!(fit(false, (560, 1120), None), (140., 280.));
        // Smaller: as it is.
        assert_eq!(fit(false, (100, 50), None), (100., 50.));
        // The picture's own shape, when it is there, wins over what the sender said.
        assert_eq!(fit(false, (100, 100), Some((560, 280))), (280., 140.));
    }

    #[test]
    fn a_picture_of_unknown_shape_gets_a_default_one() {
        assert_eq!(fit(false, (0, 0), None), (240., 180.));
        assert_eq!(fit(false, (0, 50), None), (240., 180.));
    }

    #[test]
    fn stickers_are_smaller() {
        assert_eq!(fit(true, (512, 512), None), (128., 128.));
        assert_eq!(fit(true, (0, 0), None), (128., 128.));
    }

    #[test]
    fn labels_say_what_and_how_big() {
        assert_eq!(
            label(AttachmentKind::File, Some("report.pdf"), 240_000),
            "report.pdf · 240 KB"
        );
        assert_eq!(label(AttachmentKind::File, None, 12), "File · 12 B");
        assert_eq!(
            label(AttachmentKind::VoiceNote, Some("ignored.m4a"), 1_500_000),
            "Voice message · 1.5 MB"
        );
        assert!(label(AttachmentKind::ViewOnce, None, 1).starts_with("View-once media"));
    }

    #[test]
    fn saving_says_how_it_went() {
        assert_eq!(status(None, Some(&SaveState::Saving)), "Saving…");
        assert_eq!(
            status(
                None,
                Some(&SaveState::Saved("/home/me/Downloads/a.pdf".into()))
            ),
            "Saved to Downloads"
        );
        assert_eq!(
            status(None, Some(&SaveState::Saved("a.pdf".into()))),
            "Saved to disk"
        );
        assert_eq!(status(None, None), "");
        assert_eq!(status(Some(&AttachmentState::Loading), None), "");
    }

    #[test]
    fn only_what_can_be_kept_can_be_saved() {
        assert!(can_save(AttachmentKind::File));
        assert!(can_save(AttachmentKind::Image));
        assert!(can_save(AttachmentKind::VoiceNote));
        assert!(!can_save(AttachmentKind::Sticker));
        assert!(!can_save(AttachmentKind::LongText));
        assert!(!can_save(AttachmentKind::ViewOnce));
    }
}
