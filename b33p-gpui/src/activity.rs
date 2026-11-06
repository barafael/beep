//! When the open chat was active: the strip beside its messages (a bar per week or month, from
//! the first message at the top to now at the bottom; a click goes there) and the calendar
//! over them (a cell per day, coloured by how much was written).

use std::rc::Rc;

use b33p_core::timeline::{self, Stretch};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, FontWeight, IntoElement, Render, SharedString, Subscription, Window,
    div, px, relative,
};

use crate::panel;
use crate::shared::Shared;
use crate::theme::{self, Tokens};

/// How wide the strip is, and where its centre line runs; bars reach this far from it.
const STRIP: f32 = 56.;
const CENTRE: f32 = 38.;
const REACH: f32 = 11.;

/// Silences of at least this many weeks or months are folded.
const FOLD: usize = 6;
/// A folded silence is drawn this many times as tall as a week or month.
const QUIET_WEIGHT: f32 = 2.0;

/// One bar of the strip.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Where it starts and how tall it is, as fractions of the strip.
    pub y: f32,
    pub height: f32,
    /// The messages it covers, from the first to the one after the last.
    pub first: usize,
    pub end: usize,
    /// The year, where it begins.
    pub year: Option<String>,
    /// How much was written by others and by us, 0 to 1.
    pub others: f32,
    pub mine: f32,
    pub media: bool,
    pub quiet: bool,
    pub label: String,
    pub summary: String,
}

/// Weeks or months (whichever fit) from top to bottom, long silences folded into a short gap.
pub fn items(chat: &b33p_core::state::ChatState) -> Vec<Item> {
    let span = chat.activity_span();
    let buckets = chat.activity(span);
    // Square roots, so a quiet week still shows beside a busy one.
    let busiest = buckets
        .iter()
        .map(|b| b.mine.max(b.others))
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let scale = |n: u32| (n as f32 / busiest).sqrt();
    let stretches = timeline::fold_quiet(buckets, span, FOLD);
    let weight = |s: &Stretch| {
        if matches!(s, Stretch::Quiet { .. }) {
            QUIET_WEIGHT
        } else {
            1.0
        }
    };
    let total: f32 = stretches.iter().map(weight).sum();
    let count = chat.messages().len();
    let mut y = 0.0;
    let mut year = None;
    stretches
        .iter()
        .enumerate()
        .map(|(i, stretch)| {
            let height = weight(stretch) / total;
            let end = stretches.get(i + 1).map_or(count, Stretch::first);
            let mut item = Item {
                y,
                height,
                first: stretch.first(),
                end,
                year: None,
                others: 0.,
                mine: 0.,
                media: false,
                quiet: false,
                label: String::new(),
                summary: String::new(),
            };
            y += height;
            match stretch {
                Stretch::Active(bucket) => {
                    if year != Some(bucket.start.year()) {
                        year = Some(bucket.start.year());
                        item.year = Some(bucket.start.year().to_string());
                    }
                    item.mine = scale(bucket.mine);
                    item.others = scale(bucket.others);
                    item.media = bucket.media > 0;
                    item.label = bucket.label(span);
                    item.summary = bucket.summary();
                }
                Stretch::Quiet { from, until, .. } => {
                    item.quiet = true;
                    item.label =
                        format!("{} – {}", from.strftime("%B %Y"), until.strftime("%B %Y"));
                    item.summary = Stretch::quiet_summary(*from, *until);
                }
            }
            item
        })
        .collect()
}

/// What `items` was computed from; it is computed again when that changes.
#[derive(Clone, PartialEq)]
struct Key {
    conversation: Option<b33p_core::ConversationId>,
    count: usize,
    first: Option<u64>,
    last: Option<u64>,
}

pub struct ActivityStrip {
    shared: Rc<Shared>,
    key: Option<Key>,
    items: Vec<Item>,
    _scrolling: Subscription,
}

impl ActivityStrip {
    pub fn new(shared: Rc<Shared>, cx: &mut Context<Self>) -> Self {
        // The part in view is marked, so scrolling draws it again.
        let scrolling = cx.observe(&shared.scroller.clone(), |_, _, cx| cx.notify());
        Self {
            shared,
            key: None,
            items: Vec::new(),
            _scrolling: scrolling,
        }
    }

    /// Whether there is enough to show: the setting is on and the chat has more than one bar,
    /// as a strip of one says nothing.
    pub fn shown(strip: &gpui_kit::Entity<Self>, shared: &Shared, cx: &mut App) -> bool {
        if !shared.settings.borrow().show_activity || shared.chat.borrow().selected().is_none() {
            return false;
        }
        strip.update(cx, |strip, _| {
            strip.refresh();
            strip.items.len() > 1
        })
    }

    fn refresh(&mut self) {
        let chat = self.shared.chat.borrow();
        let key = Key {
            conversation: chat.selected().map(|c| c.id.clone()),
            count: chat.messages().len(),
            first: chat.messages().next().map(|m| m.message.timestamp),
            last: chat.messages().last().map(|m| m.message.timestamp),
        };
        if self.key.as_ref() != Some(&key) {
            self.items = items(&chat);
            self.key = Some(key);
        }
    }
}

/// The messages in view, as far as can be told: the list knows where it starts, and the rest is
/// how many rows of about this height fit in `height`.
pub fn visible_rows(top: usize, count: usize, tail: bool, height: f32) -> (usize, usize) {
    const ROW: f32 = 64.;
    let last = count.saturating_sub(1);
    if tail {
        let rows = (height / ROW).ceil() as usize;
        return (last.saturating_sub(rows.saturating_sub(1)), last);
    }
    let top = top.min(last);
    (top, (top + (height / ROW).ceil() as usize).min(last))
}

impl Render for ActivityStrip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        self.refresh();
        let shared = self.shared.clone();
        let (top, bottom) = {
            let scroller = shared.scroller.read(cx);
            let height = window.viewport_size().height.as_f32() - theme::HEADER - 80.;
            visible_rows(
                scroller.logical_scroll_top().item_ix,
                scroller.item_count(),
                scroller.is_following_tail(),
                height,
            )
        };
        div()
            .id("activity-strip")
            .relative()
            .flex_none()
            .w(px(STRIP))
            .h_full()
            .bg(t.surface_raised)
            .children(self.items.iter().enumerate().map(|(index, item)| {
                bar(
                    index,
                    item,
                    item.first <= bottom && item.end > top,
                    &shared,
                    &t,
                )
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .w(px(1.))
                    .h_full()
                    .bg(t.divider),
            )
    }
}

/// One bar of the strip, with what it holds on hover.
fn bar(index: usize, item: &Item, in_view: bool, shared: &Rc<Shared>, t: &Tokens) -> AnyElement {
    let tall = item.height;
    let jump = item.first;
    let shared = shared.clone();
    let (label, summary) = (item.label.clone(), item.summary.clone());
    let text_secondary = t.text_secondary;
    div()
        .id(("activity-bar", index))
        .absolute()
        .left_0()
        .w_full()
        .top(relative(item.y))
        .h(relative(tall))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            shared
                .scroller
                .update(cx, |scroller, cx| scroller.scroll_to_item(jump, cx));
        })
        .tooltip(move |window, cx| {
            let (label, summary) = (label.clone(), summary.clone());
            Tooltip::element(move |_, _| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(theme::BODY * theme::BODY_SMALL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(label.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(theme::BODY * theme::SMALL))
                            .text_color(text_secondary)
                            .child(SharedString::from(summary.clone())),
                    )
            })
            .build(window, cx)
        })
        .when(in_view, |bar| {
            bar.child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .left(px(CENTRE - REACH - 3.))
                    .w(px(2. * REACH + 6.))
                    .bg(t.accent.opacity(0.18)),
            )
        })
        .when(item.quiet, |bar| {
            bar.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .pl(px(CENTRE - 5.))
                    .text_size(px(theme::BODY * theme::CAPTION))
                    .text_color(t.text_faint)
                    .child("⋯"),
            )
        })
        .when(!item.quiet, |bar| {
            // Thin rows touch; thicker ones keep a hairline apart.
            bar.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(CENTRE - item.others * REACH))
                    .w(px(item.others * REACH))
                    .h_full()
                    .bg(t.text_faint),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(CENTRE))
                    .w(px(item.mine * REACH))
                    .h_full()
                    .bg(t.accent),
            )
        })
        .when(item.media, |bar| {
            bar.child(
                div()
                    .absolute()
                    .right(px(2.))
                    .top(relative(0.5))
                    .size(px(4.))
                    .rounded(px(2.))
                    .bg(t.text_tertiary),
            )
        })
        .children(item.year.clone().map(|year| {
            div()
                .absolute()
                .top_0()
                .left(px(3.))
                .text_size(px(theme::BODY * theme::CAPTION))
                .text_color(t.text_tertiary)
                .child(SharedString::from(year))
        }))
        .into_any_element()
}

// The calendar.

/// How much of a year's busiest day a day's messages are, 0 to 1, as the colour's strength:
/// square roots, so a quiet day shows beside a busy one.
pub fn level(messages: u32, busiest: u32) -> f32 {
    if messages == 0 {
        0.
    } else {
        (messages as f32 / busiest.max(1) as f32).sqrt()
    }
}

pub struct CalendarPanel {
    shared: Rc<Shared>,
}

impl CalendarPanel {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self { shared }
    }
}

const GAP: f32 = 2.;
const LABELS: f32 = 30.;

impl Render for CalendarPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        let width = {
            let ui = shared.ui.borrow();
            window.viewport_size().width.as_f32() - if ui.narrow { 0. } else { ui.sidebar_width }
        };
        // As large as fits 53 weeks beside the padding and the scroll bar, within reason.
        let cell = ((width - 2. * theme::SPACE_L - 20. - LABELS) / 53. - GAP).clamp(5., 13.);
        let (summary, years) = {
            let chat = shared.chat.borrow();
            let days = chat.activity(timeline::Span::Day);
            let busiest = days.iter().map(|d| d.messages()).max().unwrap_or(0);
            let mut summary = days
                .first()
                .map(|d| d.start.strftime("Since %B %Y · ").to_string())
                .unwrap_or_default();
            summary.push_str(&timeline::Bucket::total(&days).summary());
            let years: Vec<AnyElement> = timeline::calendar(&days)
                .into_iter()
                .rev()
                .map(|year| year_block(&year, busiest, cell, &shared, &t))
                .collect();
            (summary, years)
        };
        let close = {
            let shared = shared.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                shared.ui.borrow_mut().calendar_open = false;
                shared.refresh(Default::default(), cx);
            }
        };
        panel::sheet("calendar-panel", &t)
            .child(panel::header(
                &t,
                "Back to chat",
                "Activity",
                Some(summary),
                None,
                close,
            ))
            .child(
                div()
                    .id("calendar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .children(years),
            )
            .child(legend(&t))
    }
}

/// A year: its name and totals, and a cell per day in columns of weeks.
fn year_block(
    year: &timeline::CalendarYear<'_>,
    busiest: u32,
    cell: f32,
    shared: &Rc<Shared>,
    t: &Tokens,
) -> AnyElement {
    let head = div()
        .flex()
        .flex_row()
        .gap(px(10.))
        .child(
            div()
                .font_weight(FontWeight::BOLD)
                .child(year.year.to_string()),
        )
        .child(
            div()
                .text_size(px(theme::BODY * theme::BODY_SMALL))
                .text_color(t.text_secondary)
                .child(if year.total.messages() == 0 {
                    "No messages".to_owned()
                } else {
                    format!(
                        "{} · {}% from you",
                        year.total.summary(),
                        year.total.mine_percent()
                    )
                }),
        );
    let block = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(theme::SPACE_L))
        .pt(px(theme::SPACE_L))
        .child(head);
    if year.total.messages() == 0 {
        return block.into_any_element();
    }
    let step = cell + GAP;
    let mut grid = div()
        .relative()
        .flex_none()
        .w(px(LABELS + 53. * step))
        .h(px(16. + 7. * step));
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    for (month, column) in MONTHS.iter().zip(year.months) {
        grid = grid.child(
            div()
                .absolute()
                .top_0()
                .left(px(LABELS + f32::from(column) * step))
                .text_size(px(theme::BODY * theme::CAPTION))
                .text_color(t.text_tertiary)
                .child(*month),
        );
    }
    for (row, name) in ["Mon", "", "Wed", "", "Fri", "", ""]
        .into_iter()
        .enumerate()
    {
        grid = grid.child(
            div()
                .absolute()
                .left_0()
                .top(px(16. + row as f32 * step))
                .h(px(cell))
                .flex()
                .items_center()
                .text_size(px(theme::BODY * theme::CAPTION))
                .text_color(t.text_tertiary)
                .child(name),
        );
    }
    for (at, day) in year.days.iter().enumerate() {
        let messages = day.bucket.map_or(0, |b| b.messages());
        let active = messages > 0;
        let fill = if day.bucket.is_none() {
            t.subtle
        } else if !active {
            t.hover
        } else {
            t.accent.opacity(0.3 + 0.7 * level(messages, busiest))
        };
        let first = day.bucket.map_or(0, |b| b.first);
        let summary = day
            .bucket
            .map_or_else(|| "No messages".to_owned(), |b| b.summary());
        let label = day.label.clone();
        let shared = shared.clone();
        let text = t.text;
        let secondary = t.text_secondary;
        let mut square = div()
            .id(("day", year.year as usize * 400 + at))
            .absolute()
            .left(px(LABELS + f32::from(day.column) * step))
            .top(px(16. + f32::from(day.row) * step))
            .size(px(cell))
            .rounded(px(2.))
            .bg(fill);
        if active {
            square = square
                .cursor_pointer()
                .hover(move |s| s.border_1().border_color(text))
                .on_click(move |_, _, cx| {
                    shared.ui.borrow_mut().calendar_open = false;
                    shared
                        .scroller
                        .update(cx, |scroller, cx| scroller.scroll_to_item(first, cx));
                    shared.refresh(Default::default(), cx);
                });
        }
        if day.bucket.is_some() {
            square = square.tooltip(move |window, cx| {
                let (label, summary) = (label.clone(), summary.clone());
                Tooltip::element(move |_, _| {
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(theme::BODY * theme::BODY_SMALL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(label.clone())),
                        )
                        .child(
                            div()
                                .text_size(px(theme::BODY * theme::SMALL))
                                .text_color(secondary)
                                .child(SharedString::from(summary.clone())),
                        )
                })
                .build(window, cx)
            });
        }
        grid = grid.child(square);
    }
    block.child(grid).into_any_element()
}

/// "Less" to "More": what the shades mean.
fn legend(t: &Tokens) -> gpui_kit::Div {
    let small = |text: &'static str| {
        div()
            .text_size(px(theme::BODY * theme::CAPTION))
            .text_color(t.text_tertiary)
            .child(text)
    };
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACE_XS))
        .p(px(theme::SPACE_L))
        .pt(px(theme::SPACE_S))
        .child(small("Less"))
        .children([0., 0.25, 0.5, 0.75, 1.].map(|level| {
            div().size(px(11.)).rounded(px(2.)).bg(if level <= 0. {
                t.hover
            } else {
                t.accent.opacity(0.3 + 0.7 * level)
            })
        }))
        .child(small("More"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_day_still_shows_beside_a_busy_one() {
        assert_eq!(level(0, 100), 0.);
        assert!(level(1, 100) > 0.05);
        assert_eq!(level(100, 100), 1.);
        // Nothing busier than nothing: no division by zero.
        assert_eq!(level(0, 0), 0.);
    }

    #[test]
    fn the_visible_part_follows_the_top_and_the_tail() {
        // Scrolled in the middle: from the top row, as many as fit.
        assert_eq!(visible_rows(10, 100, false, 640.), (10, 20));
        // At the end: the last rows.
        assert_eq!(visible_rows(100, 100, true, 640.), (90, 99));
        // Fewer messages than fit.
        assert_eq!(visible_rows(0, 3, false, 640.), (0, 2));
        assert_eq!(visible_rows(0, 0, true, 640.), (0, 0));
    }
}
