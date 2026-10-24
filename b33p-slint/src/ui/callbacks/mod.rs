//! What the window's callbacks do: each group wires its own.

use super::*;

mod app;
mod chat;
mod groups;
mod panels;

pub(super) fn wire(window: &AppWindow, shared: &Rc<Shared>) {
    app::wire(window, shared);
    chat::wire(window, shared);
    groups::wire(window, shared);
    panels::wire(window, shared);
}
