//! What the window loads by name: its own pictures, and the Lucide icons the kit brings.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

pub struct Assets;

/// The app's icon, which the Slint window draws too.
const LOGO: &[u8] = include_bytes!("../../b33p-slint/assets/b33p.svg");

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "b33p.svg" {
            return Ok(Some(Cow::Borrowed(LOGO)));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::AllAssets.list(path)
    }
}
