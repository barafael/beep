//! User preferences, kept in a small text file in the config directory. They live outside the
//! encrypted database because some (like the theme) apply before it is unlocked.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// Follow the desktop.
    #[default]
    System,
    Light,
    Dark,
}

/// The look of the widgets, for UIs that offer several.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WidgetStyle {
    /// The platform's own (Qt on Linux, in the desktop's Qt style).
    #[default]
    Native,
    /// Qt's Fusion style: the native widgets, but the same on every desktop.
    Fusion,
    Fluent,
    Material,
    Cupertino,
    Cosmic,
}

impl WidgetStyle {
    pub const ALL: [Self; 6] = [
        Self::Native,
        Self::Fusion,
        Self::Fluent,
        Self::Material,
        Self::Cupertino,
        Self::Cosmic,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Fusion => "fusion",
            Self::Fluent => "fluent",
            Self::Material => "material",
            Self::Cupertino => "cupertino",
            Self::Cosmic => "cosmic",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextSize {
    Small,
    #[default]
    Default,
    Large,
}

impl TextSize {
    /// Factor applied to the platform's default font size.
    pub fn scale(self) -> f32 {
        match self {
            TextSize::Small => 0.875,
            TextSize::Default => 1.0,
            TextSize::Large => 1.2,
        }
    }
}

/// How much of a message a desktop notification reveals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationContent {
    #[default]
    NameAndMessage,
    NameOnly,
    /// Only that there is a new message.
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    pub style: WidgetStyle,
    pub text_size: TextSize,
    /// Show profile and group pictures rather than only initials.
    pub show_avatars: bool,
    /// Desktop notifications for incoming messages.
    pub notifications: bool,
    pub notification_content: NotificationContent,
    /// A strip beside a chat showing when it was active. Off unless chosen.
    pub show_activity: bool,
    /// A button above a chat opening a year-by-year calendar of it. Off unless chosen.
    pub show_calendar: bool,
    /// Closing the window leaves langis running in the tray, still receiving messages.
    pub close_to_tray: bool,
    pub preferences: crate::Preferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            style: WidgetStyle::default(),
            text_size: TextSize::default(),
            show_avatars: true,
            notifications: true,
            notification_content: NotificationContent::default(),
            show_activity: false,
            show_calendar: false,
            close_to_tray: true,
            preferences: crate::Preferences::default(),
        }
    }
}

impl Settings {
    /// The saved settings; defaults for anything missing or unreadable.
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path()
            .ok_or_else(|| std::io::Error::other("could not determine a config directory"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_text())
    }

    /// `key = value` lines. Unknown keys and values are ignored, so older and newer versions
    /// can share the file.
    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("theme", "system") => settings.theme = Theme::System,
                ("theme", "light") => settings.theme = Theme::Light,
                ("theme", "dark") => settings.theme = Theme::Dark,
                ("style", value) => {
                    if let Some(style) = WidgetStyle::ALL.into_iter().find(|s| s.key() == value) {
                        settings.style = style;
                    }
                }
                ("text_size", "small") => settings.text_size = TextSize::Small,
                ("text_size", "default") => settings.text_size = TextSize::Default,
                ("text_size", "large") => settings.text_size = TextSize::Large,
                ("show_avatars", "true") => settings.show_avatars = true,
                ("show_avatars", "false") => settings.show_avatars = false,
                ("notifications", "true") => settings.notifications = true,
                ("notifications", "false") => settings.notifications = false,
                ("notification_content", "name_and_message") => {
                    settings.notification_content = NotificationContent::NameAndMessage
                }
                ("notification_content", "name_only") => {
                    settings.notification_content = NotificationContent::NameOnly
                }
                ("notification_content", "nothing") => {
                    settings.notification_content = NotificationContent::Nothing
                }
                ("show_activity", "true") => settings.show_activity = true,
                ("show_activity", "false") => settings.show_activity = false,
                ("show_calendar", "true") => settings.show_calendar = true,
                ("show_calendar", "false") => settings.show_calendar = false,
                ("close_to_tray", "true") => settings.close_to_tray = true,
                ("close_to_tray", "false") => settings.close_to_tray = false,
                ("read_receipts", "true") => settings.preferences.read_receipts = true,
                ("read_receipts", "false") => settings.preferences.read_receipts = false,
                ("typing_indicators", "true") => settings.preferences.typing_indicators = true,
                ("typing_indicators", "false") => settings.preferences.typing_indicators = false,
                ("link_previews", "true") => settings.preferences.link_previews = true,
                ("link_previews", "false") => settings.preferences.link_previews = false,
                ("sync_to_phone", "true") => settings.preferences.sync_to_phone = true,
                ("sync_to_phone", "false") => settings.preferences.sync_to_phone = false,
                _ => {}
            }
        }
        settings
    }

    fn to_text(&self) -> String {
        let theme = match self.theme {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        };
        let text_size = match self.text_size {
            TextSize::Small => "small",
            TextSize::Default => "default",
            TextSize::Large => "large",
        };
        let notification_content = match self.notification_content {
            NotificationContent::NameAndMessage => "name_and_message",
            NotificationContent::NameOnly => "name_only",
            NotificationContent::Nothing => "nothing",
        };
        format!(
            "theme = {theme}\nstyle = {}\ntext_size = {text_size}\nshow_avatars = {}\nnotifications = {}\n\
             notification_content = {notification_content}\nshow_activity = {}\nshow_calendar = {}\n\
             close_to_tray = {}\n\
             read_receipts = {}\ntyping_indicators = {}\nlink_previews = {}\nsync_to_phone = {}\n",
            self.style.key(),
            self.show_avatars,
            self.notifications,
            self.show_activity,
            self.show_calendar,
            self.close_to_tray,
            self.preferences.read_receipts,
            self.preferences.typing_indicators,
            self.preferences.link_previews,
            self.preferences.sync_to_phone
        )
    }
}

fn path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("org", "langis", "langis")?;
    Some(dirs.config_dir().join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for style in WidgetStyle::ALL {
            let settings = Settings {
                theme: Theme::Dark,
                style,
                text_size: TextSize::Large,
                show_avatars: false,
                notifications: false,
                notification_content: NotificationContent::NameOnly,
                show_activity: true,
                show_calendar: true,
                close_to_tray: false,
                preferences: crate::Preferences {
                    read_receipts: false,
                    typing_indicators: false,
                    link_previews: true,
                    sync_to_phone: true,
                },
            };
            assert_eq!(Settings::parse(&settings.to_text()), settings);
        }
    }

    #[test]
    fn ignores_what_it_does_not_know() {
        let settings =
            Settings::parse("theme = sepia\nfuture_option = 3\ngarbage\ntext_size=small\n");
        assert_eq!(
            settings,
            Settings {
                text_size: TextSize::Small,
                ..Settings::default()
            }
        );
    }
}
