//! The settings page.

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, div};

use crate::app::AppView;
use crate::shared::Shared;
use crate::theme::Tokens;

/// Not built yet.
pub fn page(_shared: &Rc<Shared>, _t: &Tokens, _cx: &mut Context<AppView>) -> AnyElement {
    div().flex_1().child("Settings").into_any_element()
}
