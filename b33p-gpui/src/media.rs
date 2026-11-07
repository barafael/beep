//! The media, files and links of the open chat, over its messages: pictures and videos as a
//! grid of tiles, files to open or save, and the links that were sent.

use std::ops::Range;
use std::rc::Rc;

use b33p_core::state::{self, AttachmentState, SaveState};
use b33p_core::{Attachment, AttachmentKind};
use gpui_kit::Role;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, FontWeight, IntoElement, ObjectFit, Render, SharedString, Window,
    div, img, px, uniform_list,
};

use crate::panel;
use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, icon};

/// The tiles are about this wide, stretched to fill a row.
const TILE: f32 = 110.;
const GAP: f32 = 4.;
/// A file's row: who sent it and when, and the file below.
const FILE_ROW: f32 = 76.;
const LINK_ROW: f32 = 56.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Media,
    Files,
    Links,
}

pub struct MediaPanel {
    shared: Rc<Shared>,
    tab: Tab,
}

impl MediaPanel {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self {
            shared,
            tab: Tab::Media,
        }
    }

    /// The panel is not shown: it opens on the pictures again.
    pub fn closed(&mut self) {
        self.tab = Tab::Media;
    }
}

/// What the three tabs hold, counted.
struct Counts {
    media: usize,
    files: usize,
    links: usize,
}

impl Render for MediaPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let (busy, counts) = {
            let chat = shared.chat.borrow();
            match chat.media() {
                Some(Some(overview)) => (
                    false,
                    Counts {
                        media: overview.media.len(),
                        files: overview.files.len(),
                        links: overview.links.len(),
                    },
                ),
                _ => (
                    true,
                    Counts {
                        media: 0,
                        files: 0,
                        links: 0,
                    },
                ),
            }
        };
        let tab = self.tab;
        let tabs = div()
            .flex_none()
            .flex()
            .flex_row()
            .gap(px(theme::SPACE_XS))
            .p(px(theme::SPACE_M))
            .pb(px(theme::SPACE_S))
            .children(
                [
                    ("Media", Tab::Media),
                    ("Files", Tab::Files),
                    ("Links", Tab::Links),
                ]
                .into_iter()
                .map(|(label, choice)| {
                    panel::pill(
                        &t,
                        label,
                        label,
                        tab == choice,
                        cx.listener(move |this, _, _, cx| {
                            this.tab = choice;
                            cx.notify();
                        }),
                    )
                }),
            );
        let empty = match tab {
            Tab::Media => counts.media == 0,
            Tab::Files => counts.files == 0,
            Tab::Links => counts.links == 0,
        };
        let body: AnyElement = if busy {
            panel::busy().into_any_element()
        } else if empty {
            let (name, text) = match tab {
                Tab::Media => ("image", "No photos or videos yet."),
                Tab::Files => ("file", "No files yet."),
                Tab::Links => ("link", "No links yet."),
            };
            panel::empty_state(&t, name, text).into_any_element()
        } else {
            match tab {
                Tab::Media => self.tiles(counts.media, window, cx),
                Tab::Files => self.files(counts.files, cx),
                Tab::Links => self.links(counts.links, cx),
            }
        };
        let close = {
            let shared = shared.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                shared.edit(cx, |chat| chat.close_media());
            }
        };
        panel::sheet("media-panel", &t)
            .child(panel::header(
                &t,
                "Back to chat",
                "Media, files and links",
                None,
                None,
                close,
            ))
            .child(tabs)
            .child(body)
    }
}

impl MediaPanel {
    /// The pictures and videos, as many tiles across as fit.
    fn tiles(&self, count: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let width = {
            let ui = self.shared.ui.borrow();
            let sidebar = if ui.narrow { 0. } else { ui.sidebar_width };
            // Beside the padding of the list and its scroll bar.
            window.viewport_size().width.as_f32() - sidebar - 2. * theme::SPACE_M
        };
        let columns = (((width + GAP) / (TILE + GAP)).floor() as usize).max(1);
        let size = (width - (columns - 1) as f32 * GAP) / columns as f32;
        let rows = count.div_ceil(columns);
        uniform_list(
            "media-tiles",
            rows,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let t = theme::tokens(cx);
                let from = range.start * columns;
                let to = (range.end * columns).min(count);
                let shown = this.shared.chat.borrow().media().flatten().map(|overview| {
                    overview.media[from.min(count)..to]
                        .iter()
                        .map(|a| (*a).clone())
                        .collect::<Vec<Attachment>>()
                });
                let shown = shown.unwrap_or_default();
                fetch_pictures(&this.shared, &shown);
                shown
                    .chunks(columns)
                    .enumerate()
                    .map(|(row, tiles)| {
                        div().flex().flex_row().gap(px(GAP)).pb(px(GAP)).children(
                            tiles.iter().enumerate().map(|(column, a)| {
                                tile(from + row * columns + column, a, size, &this.shared, &t, cx)
                            }),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .px(px(theme::SPACE_M))
        .into_any_element()
    }

    fn files(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        uniform_list(
            "media-files",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let t = theme::tokens(cx);
                let chat = this.shared.chat.borrow();
                let Some(overview) = chat.media().flatten() else {
                    return Vec::new();
                };
                range
                    .filter_map(|index| {
                        let (message, attachment) = overview.files.get(index)?;
                        Some(file_row(
                            index,
                            &message.sender,
                            &state::format_time(message.timestamp),
                            attachment,
                            &chat,
                            &this.shared,
                            &t,
                        ))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .into_any_element()
    }

    fn links(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        uniform_list(
            "media-links",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let t = theme::tokens(cx);
                let chat = this.shared.chat.borrow();
                let Some(overview) = chat.media().flatten() else {
                    return Vec::new();
                };
                range
                    .filter_map(|index| {
                        let (message, url) = overview.links.get(index)?;
                        Some(link_row(
                            index,
                            url,
                            &message.sender,
                            &state::format_time(message.timestamp),
                            &this.shared,
                            &t,
                        ))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .px(px(theme::SPACE_S))
        .into_any_element()
    }
}

/// Asks for the pictures that came into view and aren't downloaded yet, as the messages do.
fn fetch_pictures(shared: &Rc<Shared>, shown: &[Attachment]) {
    for attachment in shown {
        let wanted = {
            let chat = shared.chat.borrow();
            chat.attachment(&attachment.id).is_none() && state::shown_inline(attachment.kind)
        };
        if !wanted {
            continue;
        }
        let command = shared.chat.borrow_mut().fetch_attachment(&attachment.id);
        if let Some(command) = command {
            shared.commands.send(command);
        }
    }
}

/// One tile: the picture (a blurred stand-in until it is there), and a mark on videos.
fn tile(
    index: usize,
    attachment: &Attachment,
    size: f32,
    shared: &Rc<Shared>,
    t: &Tokens,
    _cx: &mut App,
) -> AnyElement {
    let picture = shared
        .images
        .downloaded(&shared.chat.borrow(), &attachment.id);
    let placeholder = match (&picture, &attachment.blurhash) {
        (None, Some(hash)) => shared
            .images
            .blur(hash, attachment.width, attachment.height),
        _ => None,
    };
    let video = attachment.kind == AttachmentKind::Video;
    let gif = attachment.gif;
    let id = attachment.id.clone();
    let shared = shared.clone();
    div()
        .id(("media-tile", index))
        .relative()
        .flex_none()
        .size(px(size))
        .rounded(px(theme::RADIUS_S))
        .overflow_hidden()
        .bg(t.surface_raised)
        .cursor_pointer()
        .speaks(
            Role::Button,
            if video { "Play video" } else { "Open picture" },
        )
        .on_click(move |_, _, cx| shared.command(cx, |chat| chat.open_attachment(&id)))
        .children(placeholder.map(|blurred| {
            img(blurred)
                .size_full()
                .absolute()
                .inset_0()
                .object_fit(ObjectFit::Cover)
        }))
        .children(picture.map(|picture| {
            img(picture)
                .size_full()
                .absolute()
                .inset_0()
                .object_fit(ObjectFit::Cover)
        }))
        .when(video, |tile| {
            tile.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(36.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(t.scrim)
                            .text_color(t.on_stage)
                            .child(if gif {
                                div()
                                    .text_size(px(theme::BODY * theme::CAPTION))
                                    .font_weight(FontWeight::BOLD)
                                    .child("GIF")
                                    .into_any_element()
                            } else {
                                icon("play").size(px(16.)).into_any_element()
                            }),
                    ),
            )
        })
        .into_any_element()
}

/// A file and who sent it: what it is, and to open it or save it.
fn file_row(
    index: usize,
    sender: &str,
    when: &str,
    attachment: &Attachment,
    chat: &state::ChatState,
    shared: &Rc<Shared>,
    t: &Tokens,
) -> AnyElement {
    let size = state::format_size(attachment.size);
    let kind = attachment.kind;
    let label = match (kind, &attachment.file_name) {
        (AttachmentKind::ViewOnce, _) => "View-once media: open it on your phone".to_owned(),
        (AttachmentKind::File, Some(name)) => format!("{name} · {size}"),
        _ => format!("{} · {size}", state::describe_kind(kind)),
    };
    let audio = matches!(kind, AttachmentKind::Audio | AttachmentKind::VoiceNote);
    let can_save = !matches!(
        kind,
        AttachmentKind::ViewOnce | AttachmentKind::LongText | AttachmentKind::Sticker
    );
    let status = match (chat.saved(&attachment.id), chat.attachment(&attachment.id)) {
        (Some(SaveState::Saving), _) => "Saving…".to_owned(),
        (Some(SaveState::Saved(path)), _) => format!(
            "Saved to {}",
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("disk")
        ),
        (Some(SaveState::Failed(error)), _) => format!("Couldn't save: {error}"),
        (None, Some(AttachmentState::Failed(error))) => error.to_string(),
        _ => String::new(),
    };
    // Whether it is playing, whether it is the one that is (playing or paused), and how far.
    let playing = {
        let playback = shared.playback.borrow();
        let current = playback.id == attachment.id.0;
        (current && !playback.paused, current, playback.time.clone())
    };
    let id = attachment.id.clone();
    let play_id = id.clone();
    let save_id = id.clone();
    let open_id = id;
    let (open_shared, save_shared, play_shared) = (shared.clone(), shared.clone(), shared.clone());
    div()
        .id(("media-file", index))
        .w_full()
        .h(px(FILE_ROW))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(2.))
        .px(px(theme::SPACE_L))
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(theme::SPACE_S))
                .text_size(px(theme::BODY * theme::SMALL))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text_secondary)
                        .child(SharedString::from(sender.to_owned())),
                )
                .child(
                    div()
                        .text_color(t.text_tertiary)
                        .child(SharedString::from(when.to_owned())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACE_S))
                .child(if audio {
                    let is_playing = playing.0;
                    Button::new(("media-play", index))
                        .ghost()
                        .small()
                        .icon(icon(if is_playing { "pause" } else { "play" }))
                        .tooltip(if is_playing { "Pause" } else { "Play" })
                        .on_click(move |_, _, cx| play_shared.toggle_playback(play_id.clone(), cx))
                        .into_any_element()
                } else {
                    icon("file").size(px(theme::ICON_SMALL)).into_any_element()
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().child(SharedString::from(
                            if audio && playing.1 && !playing.2.is_empty() {
                                playing.2.clone()
                            } else {
                                label
                            },
                        )))
                        .when(!status.is_empty(), |c| {
                            c.child(
                                div()
                                    .truncate()
                                    .text_size(px(theme::BODY * theme::SMALL))
                                    .text_color(t.text_tertiary)
                                    .child(SharedString::from(status.clone())),
                            )
                        }),
                )
                .when(!audio && can_save, |row| {
                    row.child(
                        Button::new(("media-open", index))
                            .small()
                            .label("Open")
                            .debug_selector(|| "Open".into())
                            .on_click(move |_, _, cx| {
                                open_shared.command(cx, |chat| chat.open_attachment(&open_id))
                            }),
                    )
                })
                .when(can_save, |row| {
                    row.child(
                        Button::new(("media-save", index))
                            .small()
                            .label("Save")
                            .debug_selector(|| "Save".into())
                            .on_click(move |_, _, cx| {
                                save_shared.act(cx, |chat| {
                                    (chat.save_attachment(&save_id), state::Update::default())
                                });
                            }),
                    )
                }),
        )
        .into_any_element()
}

/// A link and who sent it; a click opens it.
fn link_row(
    index: usize,
    url: &str,
    sender: &str,
    when: &str,
    shared: &Rc<Shared>,
    t: &Tokens,
) -> AnyElement {
    let shared = shared.clone();
    let open = url.to_owned();
    div()
        .id(("media-link", index))
        .w_full()
        .h(px(LINK_ROW))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_M))
        .px(px(theme::SPACE_S))
        .rounded(px(theme::RADIUS_S))
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .active(|s| s.bg(t.pressed))
        .speaks(Role::Link, format!("Open link {url}"))
        .on_click(move |_, _, cx| open_link(&shared, &open, cx))
        .child(icon("link").size(px(theme::ICON)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_color(t.accent)
                        .child(SharedString::from(url.to_owned())),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::BODY * theme::SMALL))
                        .text_color(t.text_tertiary)
                        .child(SharedString::from(format!("{sender} · {when}"))),
                ),
        )
        .into_any_element()
}

/// Opens a link from a message: a group's invite link is for joining here; anything else
/// goes to the browser, but only web addresses, as links come from message text and nothing
/// else (files, other apps' schemes) should be opened by one.
fn open_link(shared: &Rc<Shared>, link: &str, cx: &mut App) {
    if link.starts_with("https://signal.group/#") || link.starts_with("sgnl://signal.group/#") {
        shared.command(cx, |chat| chat.open_join(Some(link)));
    } else if (link.starts_with("https://") || link.starts_with("http://"))
        && let Err(error) = open::that_detached(link)
    {
        tracing::warn!(error = %b33p_core::scrub::chain(&error), "couldn't open a link");
    }
}
