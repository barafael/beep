//! The emoji picker: every emoji Unicode has, by group, found by name, and the ones picked
//! lately, which the settings keep. Ported from `b33p-slint/src/emoji.rs` and `ui/emoji.slint`.

use std::rc::Rc;
use std::sync::OnceLock;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, EventEmitter, IntoElement, Render, ScrollStrategy, SharedString, Subscription,
    UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::shared::Shared;
use crate::theme;
use crate::widgets::icon;

/// Results shown for a search at most: more don't fit on a screen anyway.
const FOUND: usize = 120;
/// Emoji to a row of the grid.
const COLUMNS: usize = 8;
/// A cell of the grid, and a tab above it.
const CELL: f32 = 36.;
const TAB: f32 = 28.;
/// The card's size, as in the Slint window: eight cells, the padding and a scroll bar's room.
pub const WIDTH: f32 = COLUMNS as f32 * CELL + 2. * theme::SPACE_S + 14.;
pub const HEIGHT: f32 = 340.;

/// A group of emoji, as Unicode orders them ("Smileys and emotion", ...).
pub struct Group {
    pub name: &'static str,
    /// One of its emoji, which names the group's tab.
    pub icon: &'static str,
    pub emoji: Vec<&'static str>,
}

/// All the groups, in Unicode's order.
pub fn groups() -> &'static [Group] {
    static GROUPS: OnceLock<Vec<Group>> = OnceLock::new();
    GROUPS.get_or_init(|| {
        emojis::Group::iter()
            .map(|group| {
                let emoji: Vec<&'static str> = group.emojis().map(|e| e.as_str()).collect();
                Group {
                    name: group_name(group),
                    icon: emoji.first().copied().unwrap_or_default(),
                    emoji,
                }
            })
            .collect()
    })
}

fn group_name(group: emojis::Group) -> &'static str {
    match group {
        emojis::Group::SmileysAndEmotion => "Smileys and emotion",
        emojis::Group::PeopleAndBody => "People and body",
        emojis::Group::AnimalsAndNature => "Animals and nature",
        emojis::Group::FoodAndDrink => "Food and drink",
        emojis::Group::TravelAndPlaces => "Travel and places",
        emojis::Group::Activities => "Activities",
        emojis::Group::Objects => "Objects",
        emojis::Group::Symbols => "Symbols",
        emojis::Group::Flags => "Flags",
    }
}

/// Emoji whose names or short codes contain every word of `text`, in Unicode's order.
pub fn search(text: &str) -> Vec<&'static str> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    emojis::iter()
        .filter(|emoji| {
            words.iter().all(|word| {
                emoji.name().contains(word.as_str())
                    || emoji.shortcodes().any(|code| code.contains(word.as_str()))
            })
        })
        .take(FOUND)
        .map(|emoji| emoji.as_str())
        .collect()
}

/// An emoji was picked: it goes to the front of the recent ones, which are kept in the
/// settings (saved when they changed).
pub fn used(shared: &Shared, emoji: &str) {
    let mut settings = shared.settings.borrow().clone();
    settings.used_emoji(emoji);
    if settings == *shared.settings.borrow() {
        return;
    }
    if let Err(error) = settings.save() {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "failed to save settings");
    }
    shared.settings.replace(settings);
}

/// What the picker says to whoever opened it.
pub struct Picked(pub String);

impl EventEmitter<Picked> for EmojiPicker {}

/// Picks an emoji: by group, among the ones used lately, or by name.
pub struct EmojiPicker {
    shared: Rc<Shared>,
    search: Entity<InputState>,
    /// The tab: `None` for the ones used lately, else the index of a group.
    group: Option<usize>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EmojiPicker {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji"));
        let subscription = cx.subscribe_in(&search, window, Self::search_event);
        Self {
            shared,
            search,
            group: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: vec![subscription],
        }
    }

    /// Starts over, as the popup opens: the recent ones (or the first group when there are
    /// none), no search, and the cursor in the search field.
    pub fn opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.group = if self.shared.settings.borrow().recent_emoji.is_empty() {
            Some(0)
        } else {
            None
        };
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
        cx.notify();
    }

    fn search_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
            // Enter takes the first found.
            InputEvent::PressEnter { .. } => {
                let query = input.read(cx).value().to_string();
                if let Some(first) = self.shown(&query).first() {
                    self.pick(first, cx);
                }
            }
            _ => {}
        }
    }

    /// What the grid shows for the tab and the search.
    fn shown(&self, query: &str) -> Vec<SharedString> {
        if !query.trim().is_empty() {
            return search(query).into_iter().map(SharedString::from).collect();
        }
        match self.group {
            None => self
                .shared
                .settings
                .borrow()
                .recent_emoji
                .iter()
                .map(|e| SharedString::from(e.clone()))
                .collect(),
            Some(group) => groups()
                .get(group)
                .map(|g| g.emoji.iter().copied().map(SharedString::from).collect())
                .unwrap_or_default(),
        }
    }

    fn pick(&mut self, emoji: &str, cx: &mut Context<Self>) {
        used(&self.shared, emoji);
        cx.emit(Picked(emoji.to_owned()));
        cx.notify();
    }

    fn show_group(&mut self, group: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.group = group;
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }
}

impl Render for EmojiPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let query = self.search.read(cx).value().to_string();
        let searching = !query.trim().is_empty();
        let shown = self.shown(&query);
        let has_recent = !self.shared.settings.borrow().recent_emoji.is_empty();
        let rows = shown.len().div_ceil(COLUMNS);

        let tab = |id: SharedString, label: SharedString, selected: bool| {
            div()
                .id(id)
                .flex_none()
                .size(px(TAB))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_S))
                .cursor_pointer()
                .aria_label(label)
                .when(selected, |tab| tab.bg(t.selected))
                .hover(|tab| tab.bg(t.hover))
        };
        let mut tabs = div().flex().flex_row().gap(px(2.)).flex_wrap();
        if has_recent {
            tabs = tabs.child(
                tab(
                    "emoji-tab-recent".into(),
                    "Recently used".into(),
                    self.group.is_none() && !searching,
                )
                .on_click(cx.listener(|this, _, window, cx| this.show_group(None, window, cx)))
                .child(icon("clock").size(px(theme::ICON_SMALL))),
            );
        }
        for (index, group) in groups().iter().enumerate() {
            tabs =
                tabs.child(
                    tab(
                        SharedString::from(format!("emoji-tab-{index}")),
                        group.name.into(),
                        self.group == Some(index) && !searching,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_group(Some(index), window, cx)
                    }))
                    .text_size(px(theme::BODY * theme::BODY_SMALL))
                    .child(group.icon),
                );
        }

        let grid =
            uniform_list(
                "emoji-grid",
                rows,
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    let t = theme::tokens(cx);
                    let query = this.search.read(cx).value().to_string();
                    let shown = this.shown(&query);
                    range
                        .map(|row| {
                            div().flex().flex_row().h(px(CELL)).children(
                                shown
                                    .iter()
                                    .skip(row * COLUMNS)
                                    .take(COLUMNS)
                                    .enumerate()
                                    .map(|(column, emoji)| {
                                        let picked = emoji.clone();
                                        div()
                                            .id(("emoji", row * COLUMNS + column))
                                            .flex_none()
                                            .size(px(CELL))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(theme::RADIUS_S))
                                            .cursor_pointer()
                                            .text_size(px(theme::BODY * theme::EMOJI))
                                            .aria_label(emoji.clone())
                                            .hover(|cell| cell.bg(t.hover))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.pick(&picked, cx)
                                            }))
                                            .child(emoji.clone())
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
            .child(Input::new(&self.search).cleanable(true))
            .child(tabs)
            .when(shown.is_empty() && searching, |card| {
                card.child(
                    div()
                        .text_center()
                        .text_color(t.text_tertiary)
                        .child("No emoji called that."),
                )
            })
            .child(grid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_are_found_by_name() {
        let found = search("thumbs up");
        assert_eq!(found.first().copied(), Some("👍"));
        assert!(search("Red Heart").contains(&"❤️"));
        assert!(search("   ").is_empty());
        assert!(search("no such emoji at all").is_empty());
    }

    #[test]
    fn searching_stops_at_a_screenful() {
        assert!(search("a").len() <= FOUND);
    }

    #[test]
    fn every_group_has_emoji() {
        let groups = groups();
        assert_eq!(groups.len(), 9);
        for group in groups {
            assert!(!group.emoji.is_empty(), "{}", group.name);
            assert!(!group.icon.is_empty());
        }
    }
}
