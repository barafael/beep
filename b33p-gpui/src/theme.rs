//! The look: colours derived from the component library's theme (which follows the desktop's
//! light or dark scheme), and the sizes the Slint window (`b33p-slint/ui/theme.slint`) uses.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{App, Hsla, Pixels, Window, hsla, px};

use b33p_core::settings::Theme as ThemeSetting;

/// Opacities of the text colour, from the primary text to the faintest.
pub const SECONDARY: f32 = 0.7;
pub const TERTIARY: f32 = 0.55;
pub const FAINT: f32 = 0.4;

pub const RADIUS_XS: f32 = 2.0;
pub const RADIUS_S: f32 = 6.0;
pub const RADIUS_M: f32 = 8.0;
pub const RADIUS_L: f32 = 12.0;
pub const BUBBLE_RADIUS: f32 = 16.0;
pub const BUBBLE_RADIUS_TIGHT: f32 = 5.0;

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_S: f32 = 8.0;
pub const SPACE_M: f32 = 12.0;
pub const SPACE_L: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;

pub const ICON: f32 = 18.0;
pub const ICON_SMALL: f32 = 14.0;
pub const BUTTON: f32 = 32.0;
pub const HEADER: f32 = 60.0;
pub const PANEL_HEADER: f32 = 48.0;

/// Font sizes relative to the body text, as the Slint window sets them.
pub const CAPTION: f32 = 0.75;
pub const SMALL: f32 = 0.8;
pub const BODY_SMALL: f32 = 0.85;
pub const TITLE: f32 = 1.1;
pub const HEADING: f32 = 1.3;
pub const DISPLAY: f32 = 1.6;
pub const EMOJI: f32 = 1.2;

/// The body text's size at the default "Text size" setting.
pub const BODY: f32 = 14.0;

pub fn body(scale: f32) -> Pixels {
    px(BODY * scale)
}

/// Every colour the window draws with, for the theme in force.
#[derive(Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_faint: Hsla,
    pub on_accent: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub subtle: Hsla,
    pub selected: Hsla,
    pub accent: Hsla,
    pub divider: Hsla,
    pub surface: Hsla,
    pub surface_raised: Hsla,
    pub field: Hsla,
    pub scrim: Hsla,
    pub stage: Hsla,
    pub on_stage: Hsla,
    pub danger: Hsla,
    pub danger_text: Hsla,
    pub bubble_outgoing: Hsla,
    pub bubble_outgoing_text: Hsla,
    pub bubble_incoming: Hsla,
    pub bubble_incoming_text: Hsla,
    pub highlight: Hsla,
}

pub fn tokens(cx: &App) -> Tokens {
    let theme = cx.theme();
    let dark = theme.is_dark();
    let text = theme.foreground;
    let accent = theme.primary;
    let white = hsla(0., 0., 1., 1.);
    // The accent made darker, for what white text sits on.
    let darker = |color: Hsla, by: f32| hsla(color.h, color.s, color.l * (1. - by), color.a);
    let brighter = |color: Hsla, by: f32| {
        hsla(
            color.h,
            color.s,
            (color.l + (1. - color.l) * by).min(1.),
            color.a,
        )
    };
    let raised = if dark {
        brighter(theme.background, 0.08)
    } else {
        darker(theme.background, 0.05)
    };
    Tokens {
        dark,
        text,
        text_secondary: text.opacity(SECONDARY),
        text_tertiary: text.opacity(TERTIARY),
        text_faint: text.opacity(FAINT),
        on_accent: white,
        hover: text.opacity(0.06),
        pressed: text.opacity(0.1),
        subtle: text.opacity(0.025),
        selected: accent.opacity(0.18),
        accent,
        divider: theme.border,
        surface: theme.background,
        surface_raised: raised,
        field: theme.input,
        scrim: hsla(0., 0., 0., 0.53),
        stage: hsla(0.58, 0.25, 0.08, 1.),
        on_stage: white,
        danger: hsla(0.97, 0.6, 0.47, 1.),
        danger_text: if dark {
            hsla(0.97, 1., 0.74, 1.)
        } else {
            hsla(0.97, 0.6, 0.47, 1.)
        },
        bubble_outgoing: darker(accent, 0.35),
        bubble_outgoing_text: white,
        bubble_incoming: raised,
        bubble_incoming_text: text,
        highlight: brighter(accent, 0.6),
    }
}

/// The first of these an installed font has, as the body text: what a desktop is likely to set
/// as its own, then what is likely to be there.
const FAMILIES: [&str; 8] = [
    "Adwaita Sans",
    "Inter",
    "Cantarell",
    "Segoe UI",
    "SF Pro Text",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
];

/// How long the caret blinks after the last key or click before it stays on. GPUI draws the
/// whole window again for every blink, so a caret that blinked for as long as a chat is open
/// would keep the window drawing twice a second, and the processor and the GPU busy, with nobody
/// typing; GTK stops its caret after ten seconds for the same reason.
const CARET_BLINK_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// The caret's blinking: the component library's pace, for [`CARET_BLINK_FOR`] after activity.
fn caret_blink() -> gpui_kit::component::input::CursorBlink {
    gpui_kit::component::input::CursorBlink::default().with_idle_timeout(CARET_BLINK_FOR)
}

/// Makes the component library's theme the one the settings choose, with b33p's accent and a
/// font that is there.
pub fn apply(setting: ThemeSetting, window: Option<&mut Window>, cx: &mut App) {
    match setting {
        ThemeSetting::System => Theme::sync_system_appearance(window, cx),
        ThemeSetting::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemeSetting::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
    let installed = cx.text_system().all_font_names();
    let family = FAMILIES
        .iter()
        .find(|family| installed.iter().any(|name| name == *family))
        .map(|family| (*family).into());
    Theme::update(cx, |theme| {
        // Signal's blue, lighter where the ground is dark.
        let accent = if theme.is_dark() {
            hsla(0.617, 0.9, 0.66, 1.)
        } else {
            hsla(0.617, 0.86, 0.59, 1.)
        };
        theme.cursor_blink = caret_blink();
        let colors = &mut theme.colors;
        colors.primary = accent;
        colors.primary_hover = hsla(accent.h, accent.s, accent.l * 0.92, 1.);
        colors.primary_active = hsla(accent.h, accent.s, accent.l * 0.84, 1.);
        colors.button_primary = accent;
        colors.button_primary_hover = colors.primary_hover;
        colors.button_primary_active = colors.primary_active;
        colors.button_primary_foreground = hsla(0., 0., 1., 1.);
        colors.link = accent;
        colors.ring = accent;
        colors.selection = accent.opacity(0.3);
        // The buttons read the resolved tokens, which follow the colours above only once they
        // are derived again: else a primary button keeps the stock theme's black.
        theme.tokens = (&theme.colors).into();
        if let Some(family) = family {
            theme.font_family = family;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caret_blinks_for_a_while_and_then_rests() {
        let blink = caret_blink();
        assert!(blink.interval().is_some(), "it blinks at first");
        assert_eq!(blink.idle_timeout(), Some(CARET_BLINK_FOR));
    }

    /// What a line of a view may not say outside the files that own it, and what to use
    /// instead. The look is the theme's: colours, text sizes and corners are named here, not
    /// scattered through the views (`b33p-slint/tests/theme.rs` keeps the Slint window to the
    /// same).
    fn problems(line: &str) -> Vec<&'static str> {
        // Comments may say anything.
        let code = line.split("//").next().unwrap_or_default();
        let mut found = Vec::new();
        if ["rgb(", "rgba(", "hsla(", "hsl("]
            .iter()
            .any(|constructor| {
                code.match_indices(constructor).any(|(at, _)| {
                    // Not the tail of another name (`stage_rgb(`).
                    !code[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                })
            })
        {
            found.push("a colour of its own: give it a name in theme.rs");
        }
        if literal_after(code, "text_size(px(") {
            found.push("a text size of its own: use px(theme::BODY * theme::SMALL), …");
        }
        if [
            "rounded(px(",
            "rounded_t(px(",
            "rounded_b(px(",
            "rounded_l(px(",
            "rounded_r(px(",
        ]
        .iter()
        .any(|key| literal_after(code, key))
        {
            found.push("a corner of its own: use theme::RADIUS_…, or rounded_full() for a pill");
        }
        found
    }

    /// Whether `code` has `key` followed by a plain number.
    fn literal_after(code: &str, key: &str) -> bool {
        code.match_indices(key)
            .any(|(at, _)| code[at + key.len()..].starts_with(|c: char| c.is_ascii_digit()))
    }

    /// Files that may name colours: the theme itself, the avatars' palette (one colour for each
    /// person, from a fixed set), and the stories, whose colours come with them.
    const MAY_NAME_COLOURS: [&str; 3] = ["theme.rs", "widgets.rs", "stories.rs"];

    #[test]
    fn the_look_comes_from_the_theme() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut wrong = Vec::new();
        for entry in std::fs::read_dir(&src).expect("the src folder") {
            let path = entry.expect("a file").path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            // The tests build what they need, and say what they like.
            if !name.ends_with(".rs")
                || matches!(name.as_str(), "ui_tests.rs" | "e2e_tests.rs" | "testing.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable");
            // What follows the first test module is tests.
            let code = text
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or_default();
            for (number, line) in code.lines().enumerate() {
                for problem in problems(line) {
                    if problem.starts_with("a colour") && MAY_NAME_COLOURS.contains(&name.as_str())
                    {
                        continue;
                    }
                    wrong.push(format!(
                        "{name}:{}: {problem}\n    {}",
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    }

    #[test]
    fn the_check_finds_what_it_should() {
        assert!(!problems("let c = rgb(0xc4314b);").is_empty());
        assert!(!problems("x.bg(hsla(0.6, 0.9, 0.5, 1.))").is_empty());
        assert!(problems("let stage_rgb(x) = 1;").is_empty());
        assert!(problems("text: \"#general\"").is_empty());
        assert!(!problems(".text_size(px(12.))").is_empty());
        assert!(problems(".text_size(px(theme::BODY * theme::SMALL))").is_empty());
        assert!(problems(".text_size(px(size * 0.4))").is_empty());
        assert!(!problems(".rounded(px(6.))").is_empty());
        assert!(problems(".rounded(px(theme::RADIUS_S))").is_empty());
        assert!(problems(".rounded_full()").is_empty());
        assert!(problems("// .rounded(px(6.)) in a comment").is_empty());
    }
}
