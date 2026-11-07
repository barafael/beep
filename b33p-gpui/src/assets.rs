//! What the window loads by name: its own pictures, and the Lucide icons it draws.

use std::borrow::Cow;

use gpui_kit::assets::{Assets as ComponentIcons, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

pub struct Assets;

/// The app's icon, which the Slint window draws too.
const LOGO: &[u8] = include_bytes!("../../b33p-slint/assets/b33p.svg");

// The icons `widgets::icon` is asked for, besides the hundred the components draw themselves
// (`ComponentIcons`). The complete catalog (`AllAssets`, 1,830 icons) is three quarters of a
// megabyte of the binary, nearly all of it unused. `every_icon_the_window_names_is_embedded`
// fails when a name that is a Lucide icon is written in the source and is not here.
icon_assets!(
    ClientIcons,
    [
        Activity,
        Archive,
        BellOff,
        Bold,
        CheckCheck,
        CircleDashed,
        Clock,
        FaceSlightlySmiling,
        Film,
        Forward,
        Headphones,
        Hourglass,
        Image,
        Italic,
        Link,
        LogOut,
        MessageCircleQuestionMark,
        Paperclip,
        Pin,
        Reply,
        Send,
        ShieldAlert,
        ShieldCheck,
        SquarePen,
        Sticker,
        Timer,
        Trash,
        Type,
        Unlink,
        Users,
        WifiOff,
        X,
    ]
);

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "b33p.svg" {
            return Ok(Some(Cow::Borrowed(LOGO)));
        }
        if let Some(icon) = ClientIcons.load(path)? {
            return Ok(Some(icon));
        }
        ComponentIcons.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = ClientIcons.list(path)?;
        for name in ComponentIcons.list(path)? {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use gpui_kit::AssetSource as _;

    use super::Assets;

    /// Every piece between two quotes of the sources that is hyphen-separated lowercase words:
    /// the pieces between a literal and the next are taken too, which only adds names.
    fn words(dir: &Path, found: &mut BTreeSet<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                words(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && let Ok(source) = fs::read_to_string(&path)
            {
                for piece in source.split('"') {
                    let word = !piece.is_empty()
                        && piece
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                    if word {
                        found.insert(piece.to_owned());
                    }
                }
            }
        }
    }

    #[test]
    fn every_icon_the_window_names_is_embedded() {
        let mut named = BTreeSet::new();
        words(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut named,
        );
        let catalog = gpui_kit::assets::AllAssets;
        let missing: Vec<_> = named
            .iter()
            .filter(|name| {
                let path = format!("icons/{name}.svg");
                matches!(catalog.load(&path), Ok(Some(_)))
                    && !matches!(Assets.load(&path), Ok(Some(_)))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "add these to ClientIcons in assets.rs: {missing:?}"
        );
    }
}
