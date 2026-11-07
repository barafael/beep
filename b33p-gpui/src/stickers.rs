//! The sticker picker: the installed packs as tabs, the chosen pack's stickers as a grid. The
//! packs come from the phone, so the first opening asks for them (`ChatState::load_stickers`)
//! and shows a spinner until they arrive.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use b33p_core::Picture;
use gpui_kit::Role;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, EventEmitter, IntoElement, Render, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, div, img, px, uniform_list,
};

use crate::images::{Drawn, draw};
use crate::shared::Shared;
use crate::theme;
use crate::widgets::{Speaks as _, icon};

/// Stickers to a row.
const PER_ROW: usize = 6;
/// A sticker's tile, and the room beside the grid for a scroll bar.
const TILE: f32 = 72.;
pub const WIDTH: f32 = PER_ROW as f32 * TILE + 2. * theme::SPACE_S + 14.;
pub const HEIGHT: f32 = 340.;

/// A sticker was picked: its pack's id and its own.
pub struct Picked {
    pub pack: String,
    pub sticker: u32,
}

impl EventEmitter<Picked> for StickerPicker {}

pub struct StickerPicker {
    shared: Rc<Shared>,
    /// The pack shown, by position.
    pack: usize,
    scroll: UniformListScrollHandle,
    /// The stickers' pictures as GPUI draws them, converted once per picture.
    drawn: HashMap<(String, u32), (Arc<Picture>, Drawn)>,
}

impl StickerPicker {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self {
            shared,
            pack: 0,
            scroll: UniformListScrollHandle::new(),
            drawn: HashMap::new(),
        }
    }

    /// As the popup opens: asks for the packs, the first time.
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        let command = self.shared.chat.borrow_mut().load_stickers();
        if let Some(command) = command {
            self.shared.commands.send(command);
        }
        cx.notify();
    }

    fn choose(&mut self, pack: usize, cx: &mut Context<Self>) {
        self.pack = pack;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn picture(&mut self, pack: &str, id: u32, picture: &Arc<Picture>) -> Option<Drawn> {
        let key = (pack.to_owned(), id);
        if let Some((kept, drawn)) = self.drawn.get(&key)
            && Arc::ptr_eq(kept, picture)
        {
            return Some(drawn.clone());
        }
        let drawn = draw(picture)?;
        self.drawn.insert(key, (picture.clone(), drawn.clone()));
        Some(drawn)
    }
}

impl Render for StickerPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        // Titles and what is in the chosen pack, copied out so that the state is not held while
        // the grid is built.
        let (busy, titles, pack) = {
            let chat = self.shared.chat.borrow();
            let packs = chat.sticker_packs();
            let busy = matches!(packs, Some(None));
            let packs = packs.flatten().unwrap_or_default();
            let chosen = self.pack.min(packs.len().saturating_sub(1));
            (
                busy,
                packs.iter().map(|p| p.title.clone()).collect::<Vec<_>>(),
                packs.get(chosen).cloned().map(|p| (chosen, p)),
            )
        };
        let (chosen, rows) = match &pack {
            Some((chosen, pack)) => (*chosen, pack.stickers.len().div_ceil(PER_ROW)),
            None => (0, 0),
        };
        let pack_id = pack.as_ref().map(|(_, p)| p.id.clone()).unwrap_or_default();
        // Converted here, while this view can be changed, for the rows built below.
        let tiles: Vec<(u32, SharedString, Option<Drawn>)> = pack
            .as_ref()
            .map(|(_, pack)| {
                pack.stickers
                    .iter()
                    .map(|s| {
                        (
                            s.id,
                            SharedString::from(s.emoji.clone()),
                            self.picture(&pack.id, s.id, &s.picture),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tiles = Rc::new(tiles);

        let tabs = div()
            .id("sticker-tabs")
            .flex_none()
            .flex()
            .flex_row()
            .gap(px(theme::SPACE_XS))
            .overflow_x_scroll()
            .children(titles.iter().enumerate().map(|(index, title)| {
                let selected = index == chosen;
                div()
                    .id(("sticker-pack", index))
                    .flex_none()
                    .h(px(28.))
                    .px(px(theme::SPACE_M))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_size(px(theme::BODY * theme::BODY_SMALL))
                    .speaks(Role::Tab, title.clone())
                    .when(selected, |tab| {
                        tab.bg(t.selected)
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    })
                    .hover(|tab| tab.bg(t.hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(index, cx)))
                    .child(title.clone())
            }));

        let grid = uniform_list(
            "sticker-grid",
            rows,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                let t = theme::tokens(cx);
                let tiles = tiles.clone();
                let pack_id = pack_id.clone();
                range
                    .map(|row| {
                        div().flex().flex_row().h(px(TILE)).children(
                            tiles
                                .iter()
                                .skip(row * PER_ROW)
                                .take(PER_ROW)
                                .enumerate()
                                .map(|(column, (id, emoji, picture))| {
                                    let (pack, id) = (pack_id.clone(), *id);
                                    div()
                                        .id(("sticker", row * PER_ROW + column))
                                        .flex_none()
                                        .size(px(TILE))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(theme::RADIUS_M))
                                        .cursor_pointer()
                                        .speaks(Role::Button, format!("Send sticker {emoji}"))
                                        .hover(|tile| tile.bg(t.hover))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(Picked {
                                                pack: pack.clone(),
                                                sticker: id,
                                            })
                                        }))
                                        .when_some(picture.clone(), |tile, picture| {
                                            tile.child(
                                                img(picture)
                                                    .size(px(TILE - 12.))
                                                    .object_fit(gpui_kit::ObjectFit::Contain),
                                            )
                                        })
                                }),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0();

        div()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex()
            .flex_col()
            .gap(px(theme::SPACE_S))
            .p(px(theme::SPACE_S))
            .child(tabs)
            .when(busy, |card| {
                card.child(
                    div()
                        .flex()
                        .justify_center()
                        .p(px(theme::SPACE_S))
                        .child(Spinner::new().small()),
                )
            })
            .when(!busy && titles.is_empty(), |card| {
                card.child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(theme::SPACE_M))
                        .p(px(theme::SPACE_XL))
                        .text_color(t.text_tertiary)
                        .child(icon("sticker").size(px(40.)).text_color(t.text_faint))
                        .child(div().text_center().child(
                            "No sticker packs yet. Install some on your phone; they show up here.",
                        )),
                )
            })
            .child(grid)
    }
}
