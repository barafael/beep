//! The search in the open chat: a field over the messages, and what it found, each with the
//! words looked for marked. A click on a result closes the search and shows the message.

use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use b33p_core::state;
use gpui_kit::Role;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, Focusable as _, FontWeight, HighlightStyle, IntoElement, Render,
    SharedString, StyledText, Subscription, Task, Window, div, px, uniform_list,
};

use crate::panel;
use crate::shared::Shared;
use crate::theme::{self, Tokens};
use crate::widgets::{Speaks as _, icon};

/// How long typing must pause before the chat is searched; Enter does not wait.
const PAUSE: Duration = Duration::from_millis(300);
/// The height of a result: the sender and time, and the snippet over up to two lines.
const RESULT: f32 = 72.;

pub struct SearchPanel {
    shared: Rc<Shared>,
    input: Entity<InputState>,
    /// The panel was closed since it last drew: the field is emptied and takes the focus.
    fresh: bool,
    pending: Option<Task<()>>,
    _subscription: Subscription,
}

/// A result, as the list draws it.
struct Found {
    sender: SharedString,
    when: SharedString,
    snippet: SharedString,
    marked: Option<Range<usize>>,
    /// The whole text, for screen readers.
    text: String,
}

impl SearchPanel {
    pub fn new(shared: Rc<Shared>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search this chat"));
        let subscription = cx.subscribe_in(&input, window, Self::input_event);
        Self {
            shared,
            input,
            fresh: true,
            pending: None,
            _subscription: subscription,
        }
    }

    /// The panel is not shown: the next time it is, it starts anew.
    pub fn closed(&mut self) {
        self.fresh = true;
        self.pending = None;
    }

    fn input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                // After a pause in typing.
                self.pending = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(PAUSE).await;
                    this.update(cx, |this, cx| this.search(cx)).ok();
                }));
            }
            InputEvent::PressEnter { .. } => self.search(cx),
            _ => {}
        }
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        let text = self.input.read(cx).value().to_string();
        self.shared.command(cx, |chat| chat.search_chat(&text));
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = self.shared.clone();
        // What the state searches for, when it is not what was typed (the harness searches
        // too): the field shows it, unless it is being typed in.
        let looked_for = shared
            .chat
            .borrow()
            .chat_search()
            .map(|(query, _)| query.to_owned())
            .unwrap_or_default();
        let typing = self.input.focus_handle(cx).is_focused(window);
        if self.fresh {
            self.fresh = false;
            let text = looked_for.clone();
            self.input.update(cx, |input, cx| {
                input.set_value(text, window, cx);
                input.focus(window, cx);
            });
        } else if !typing && self.input.read(cx).value().trim() != looked_for {
            let text = looked_for.clone();
            self.input
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }

        let (busy, found): (bool, Vec<Found>) = {
            let chat = shared.chat.borrow();
            match chat.chat_search() {
                None => (false, Vec::new()),
                Some((query, results)) => (
                    !query.is_empty() && results.is_none(),
                    results
                        .unwrap_or_default()
                        .iter()
                        .map(|m| {
                            let (text, marked) = state::snippet(&m.body, query);
                            Found {
                                sender: m.sender.clone().into(),
                                when: state::format_time(m.timestamp).into(),
                                snippet: text.clone().into(),
                                marked,
                                text,
                            }
                        })
                        .collect(),
                ),
            }
        };
        let count = found.len();
        let close = {
            let shared = shared.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                shared.edit(cx, |chat| chat.close_chat_search());
            }
        };
        let body: AnyElement = if busy {
            panel::busy().into_any_element()
        } else if looked_for.is_empty() {
            panel::empty_state(&t, "search", "Search what was written in this chat.")
                .into_any_element()
        } else if count == 0 {
            panel::empty_state(&t, "search", "No messages found.").into_any_element()
        } else {
            let found = Rc::new(found);
            uniform_list(
                "search-results",
                count,
                cx.processor(move |_, range: Range<usize>, _, cx| {
                    let t = theme::tokens(cx);
                    range
                        .map(|index| result(index, &found[index], &shared, &t))
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .px(px(theme::SPACE_S))
            .into_any_element()
        };
        panel::sheet("search-panel", &t)
            .child(panel::header_with(
                &t,
                "Close search",
                div().flex_1().min_w_0().child(
                    Input::new(&self.input)
                        .cleanable(true)
                        .prefix(icon("search").size(px(theme::ICON_SMALL))),
                ),
                None,
                close,
            ))
            .child(body)
    }
}

/// One result: who wrote it and when, and the words around the match.
fn result(index: usize, found: &Found, shared: &Rc<Shared>, t: &Tokens) -> AnyElement {
    let shared = shared.clone();
    let marked = found.marked.clone();
    let accent = t.accent;
    let snippet = StyledText::new(found.snippet.clone()).with_highlights(marked.map(|range| {
        (
            range,
            HighlightStyle {
                color: Some(accent),
                font_weight: Some(FontWeight::BOLD),
                ..HighlightStyle::default()
            },
        )
    }));
    div()
        .id(("search-result", index))
        .h(px(RESULT))
        .w_full()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(theme::SPACE_S))
        .rounded(px(theme::RADIUS_S))
        .cursor_pointer()
        .hover(|s| s.bg(t.hover))
        .active(|s| s.bg(t.pressed))
        .speaks(
            Role::ListItem,
            format!("{}, {}: {}", found.sender, found.when, found.text),
        )
        .on_click(move |_, _, cx| shared.redraw(cx, |chat| chat.show_result(index)))
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(theme::SPACE_S))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(found.sender.clone()),
                )
                .child(
                    div()
                        .text_size(px(theme::BODY * theme::SMALL))
                        .text_color(t.text_tertiary)
                        .child(found.when.clone()),
                ),
        )
        .child(
            div()
                .line_clamp(2)
                .text_color(t.text_secondary)
                .child(snippet),
        )
        .into_any_element()
}
