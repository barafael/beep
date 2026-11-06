//! What covers the open chat's messages while it is open: the search in the chat, the media
//! overview, the details, the activity calendar. Each is drawn while the state says it is
//! open.
//!
//! To add one: write its function in its own module, which returns `Some(element)` while it is
//! open, and put it in the list in [`covering`].

use std::rc::Rc;

use gpui_kit::{AnyElement, App};

use crate::shared::Shared;
use crate::theme::Tokens;

/// The panel over the messages, if one is open.
pub fn covering(shared: &Rc<Shared>, t: &Tokens, cx: &mut App) -> Option<AnyElement> {
    let panels: Vec<Option<AnyElement>> = vec![
        // e.g. crate::search::panel(shared, t, cx),
    ];
    let _ = (shared, t, cx);
    panels.into_iter().flatten().next()
}
