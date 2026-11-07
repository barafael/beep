//! What covers the open chat's messages while it is open: the search in the chat, the media
//! overview, the details, the activity calendar. Each is drawn while the state says it is
//! open.
//!
//! To add one: write it in its own module as a view (`Render`), keep it in [`Panels`], and put
//! the test for it being open in [`Panels::covering`].

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Entity, Window, div};

use crate::activity::CalendarPanel;
use crate::details::DetailsPanel;
use crate::media::MediaPanel;
use crate::search::SearchPanel;
use crate::shared::Shared;

/// The panels, made once: each keeps what it needs between the times it is open (a field).
pub struct Panels {
    search: Entity<SearchPanel>,
    media: Entity<MediaPanel>,
    details: Entity<DetailsPanel>,
    calendar: Entity<CalendarPanel>,
}

impl Panels {
    pub fn new<T: 'static>(shared: &Rc<Shared>, window: &mut Window, cx: &mut Context<T>) -> Self {
        Self {
            search: cx.new(|cx| SearchPanel::new(shared.clone(), window, cx)),
            media: cx.new(|_| MediaPanel::new(shared.clone())),
            details: cx.new(|cx| DetailsPanel::new(shared.clone(), window, cx)),
            calendar: cx.new(|_| CalendarPanel::new(shared.clone())),
        }
    }

    /// The panel over the messages, if one is open. In front last: the media over the details
    /// (the details open it), those over the search, and all over the calendar.
    pub fn covering(&self, shared: &Rc<Shared>, cx: &mut App) -> Option<AnyElement> {
        let (media, details, search) = {
            let chat = shared.chat.borrow();
            (
                chat.media().is_some(),
                chat.details().is_some(),
                chat.chat_search().is_some(),
            )
        };
        let calendar = shared.ui.borrow().calendar_open;
        // What is not open starts anew when it is: the search's field is emptied and focused,
        // the media open on the pictures, the details' fields follow the state.
        if !search {
            self.search.update(cx, |panel, _| panel.closed());
        }
        if !media {
            self.media.update(cx, |panel, _| panel.closed());
        }
        if !details {
            self.details.update(cx, |panel, _| panel.closed());
        }
        // Each fades in as it opens; one that stays open does not fade again.
        let panel = if media {
            ("media", self.media.clone().into_any_element())
        } else if details {
            ("details", self.details.clone().into_any_element())
        } else if search {
            ("search", self.search.clone().into_any_element())
        } else if calendar {
            ("calendar", self.calendar.clone().into_any_element())
        } else {
            return None;
        };
        Some(fade(panel.0, panel.1))
    }
}

/// `panel` fading in over the messages, as Slint's panels do. A box of its own, as large as the
/// place of the messages, holds the panel (which fills it) so that the panel's own element
/// stays as it is.
fn fade(name: &'static str, panel: AnyElement) -> AnyElement {
    crate::animation::fade_in(
        name,
        crate::animation::NORMAL,
        div().id(name).absolute().inset_0().child(panel),
    )
    .into_any_element()
}
