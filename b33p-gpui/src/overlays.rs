//! What opens over the whole window: dialogs, and the story viewer. Each is drawn while the
//! state (or [`crate::shared::UiState`]) says it is open, so that the harness, a shortcut and
//! a button open the same one.
//!
//! To add one: write its function in its own module, which returns `Some(element)` while it is
//! open, and put it in the list in [`Overlays::render`].

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, IntoElement, Render, Window, div};

use crate::shared::Shared;
use crate::theme;

pub struct Overlays {
    shared: Rc<Shared>,
}

impl Overlays {
    pub fn new(shared: Rc<Shared>) -> Self {
        Self { shared }
    }
}

impl Render for Overlays {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::tokens(cx);
        let shared = &self.shared;
        // The innermost last: later ones are drawn over earlier ones.
        let open: Vec<Option<AnyElement>> = vec![
            // e.g. crate::dialogs::forward(shared, &t, cx),
        ];
        let _ = (&t, shared);
        div().children(open.into_iter().flatten())
    }
}
