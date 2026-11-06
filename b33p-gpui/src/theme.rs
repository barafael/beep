//! The look: colours derived from the component library's theme (which follows the desktop's
//! light or dark scheme), and the sizes the Slint window (`b33p-slint/ui/theme.slint`) uses.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::{App, Hsla, Pixels, Window, hsla, px};

use b33p_core::settings::Theme as ThemeSetting;

/// Opacities of the text colour, from the primary text to the faintest.
pub const SECONDARY: f32 = 0.7;
pub const TERTIARY: f32 = 0.55;
pub const FAINT: f32 = 0.4;

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
        hsla(color.h, color.s, (color.l + (1. - color.l) * by).min(1.), color.a)
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
        if let Some(family) = family {
            theme.font_family = family;
        }
    });
}
