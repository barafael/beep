//! What opens over the whole window: dialogs, and the story viewer. Each is drawn while the
//! state (or [`crate::shared::UiState`]) says it is open, so that the harness, a shortcut and
//! a button open the same one.
//!
//! To add one: write its view in its own module, and put it in the list in
//! [`Overlays::render`], kept by [`keep`] for as long as it is open. A view made anew each
//! time it opens starts with empty fields and the focus where it belongs.

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, FocusHandle, IntoElement, Render, Window, div};

use crate::confirm::ConfirmView;
use crate::forward::ForwardView;
use crate::group_draft::GroupDraftView;
use crate::join::JoinView;
use crate::shared::{Confirm, Shared};
use crate::stories::StoriesView;

pub struct Overlays {
    shared: Rc<Shared>,
    stories: Option<Entity<StoriesView>>,
    group_draft: Option<Entity<GroupDraftView>>,
    confirm: Option<Entity<ConfirmView>>,
    join: Option<Entity<JoinView>>,
    forward: Option<Entity<ForwardView>>,
    /// What had the focus before the first dialog opened, to give it back when the last closes:
    /// without a focused element Esc and the shortcuts have nowhere to start from.
    before: Option<FocusHandle>,
    any_open: bool,
}

impl Overlays {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self {
            shared,
            stories: None,
            group_draft: None,
            confirm: None,
            join: None,
            forward: None,
            before: None,
            any_open: false,
        }
    }
}

/// The view in `slot` while `open`: made when it opens (`make`), let go of when it closes.
fn keep<V: Render + 'static>(
    slot: &mut Option<Entity<V>>,
    open: bool,
    window: &mut Window,
    cx: &mut Context<Overlays>,
    make: impl FnOnce(&mut Window, &mut Context<V>) -> V,
) -> Option<AnyElement> {
    if !open {
        *slot = None;
        return None;
    }
    let view = slot.get_or_insert_with(|| cx.new(|cx| make(window, cx)));
    Some(view.clone().into_any_element())
}

impl Render for Overlays {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shared = self.shared.clone();
        let (stories, group_draft, join, forwarding) = {
            let chat = shared.chat.borrow();
            (
                shared.ui.borrow().stories_open,
                chat.group_draft().is_some(),
                chat.join().is_some(),
                chat.forwarding(),
            )
        };
        let confirming = shared.ui.borrow().confirm != Confirm::None;
        let will_open = stories || group_draft || join || forwarding || confirming;
        // The focus is noted before a view takes it, and given back after the last is gone.
        if will_open && !self.any_open {
            self.before = window.focused(cx);
        }
        // The innermost last: later ones are drawn over earlier ones.
        let open: Vec<Option<AnyElement>> = vec![
            keep(&mut self.stories, stories, window, cx, {
                let shared = shared.clone();
                move |window, cx| StoriesView::new(shared, window, cx)
            }),
            keep(&mut self.group_draft, group_draft, window, cx, {
                let shared = shared.clone();
                move |window, cx| GroupDraftView::new(shared, window, cx)
            }),
            keep(&mut self.confirm, confirming, window, cx, {
                let shared = shared.clone();
                move |window, cx| ConfirmView::new(shared, window, cx)
            }),
            keep(&mut self.join, join, window, cx, {
                let shared = shared.clone();
                move |window, cx| JoinView::new(shared, window, cx)
            }),
            keep(&mut self.forward, forwarding, window, cx, {
                let shared = shared.clone();
                move |window, cx| ForwardView::new(shared, window, cx)
            }),
        ];
        if !will_open
            && self.any_open
            && let Some(before) = self.before.take()
        {
            before.focus(window, cx);
        }
        self.any_open = will_open;
        // Over the whole window, which a child of the root's column wouldn't be on its own.
        div()
            .absolute()
            .inset_0()
            .when(!will_open, |overlays| overlays.size_0())
            .children(open.into_iter().flatten())
    }
}
