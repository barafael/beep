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
}

impl WidgetStyle {
    pub const ALL: [Self; 2] = [Self::Native, Self::Fusion];

    /// Styles b33p no longer has ("fluent", …) aren't keys, so they read as the default.
    pub fn key(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Fusion => "fusion",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.key() == key)
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
    pub const ALL: [Self; 3] = [Self::Small, Self::Default, Self::Large];

    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Default => "default",
            Self::Large => "large",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.key() == key)
    }

    /// Factor applied to the platform's default font size.
    pub fn scale(self) -> f32 {
        match self {
            TextSize::Small => 0.875,
            TextSize::Default => 1.0,
            TextSize::Large => 1.2,
        }
    }
}

/// Whether the UI animates its changes (fades, slides).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Animations {
    /// As the desktop says: off when it asks for reduced motion.
    #[default]
    System,
    On,
    Off,
}

impl Animations {
    pub const ALL: [Self; 3] = [Self::System, Self::On, Self::Off];

    /// Whether to animate, given whether the desktop asks for reduced motion.
    pub fn enabled(self, system_reduces_motion: bool) -> bool {
        match self {
            Self::System => !system_reduces_motion,
            Self::On => true,
            Self::Off => false,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|animations| animations.key() == key)
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

impl NotificationContent {
    pub const ALL: [Self; 3] = [Self::NameAndMessage, Self::NameOnly, Self::Nothing];

    pub fn key(self) -> &'static str {
        match self {
            Self::NameAndMessage => "name_and_message",
            Self::NameOnly => "name_only",
            Self::Nothing => "nothing",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|content| content.key() == key)
    }
}

impl Theme {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == key)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    pub style: WidgetStyle,
    pub text_size: TextSize,
    pub animations: Animations,
    /// Show profile and group pictures rather than only initials.
    pub show_avatars: bool,
    /// Desktop notifications for incoming messages.
    pub notifications: bool,
    pub notification_content: NotificationContent,
    /// A strip beside a chat showing when it was active. Off unless chosen.
    pub show_activity: bool,
    /// A button above a chat opening a year-by-year calendar of it. Off unless chosen.
    pub show_calendar: bool,
    /// Closing the window leaves b33p running in the tray, still receiving messages.
    pub close_to_tray: bool,
    /// How this computer appears under Linked devices on the phone, if chosen; otherwise
    /// [`default_device_name`].
    pub device_name: Option<String>,
    pub preferences: crate::Preferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            style: WidgetStyle::default(),
            text_size: TextSize::default(),
            animations: Animations::default(),
            show_avatars: true,
            notifications: true,
            notification_content: NotificationContent::default(),
            show_activity: false,
            show_calendar: false,
            close_to_tray: true,
            device_name: None,
            preferences: crate::Preferences::default(),
        }
    }
}

/// The longest device name taken, in characters; Signal's apps keep theirs about this short.
pub const MAX_DEVICE_NAME: usize = 50;

/// "b33p on <computer name>", so that several computers can be told apart on the phone.
pub fn default_device_name() -> String {
    let host = gethostname::gethostname();
    let host = host.to_string_lossy();
    // The computer's name, not its domain.
    let host = host.split('.').next().unwrap_or_default().trim();
    if host.is_empty() {
        "b33p".into()
    } else {
        clean_device_name(&format!("b33p on {host}"))
    }
}

/// One line, of at most [`MAX_DEVICE_NAME`] characters.
pub fn clean_device_name(name: &str) -> String {
    let one_line = name.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(MAX_DEVICE_NAME).collect()
}

impl Settings {
    /// The name this computer links as.
    pub fn device_name(&self) -> String {
        self.device_name.clone().unwrap_or_else(default_device_name)
    }

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
        // Whole or not at all: a file cut short (a crash, a full disk) would read as defaults,
        // privacy settings included. Written beside it, then put in its place.
        let written = path.with_extension("conf.new");
        let mut file = std::fs::File::create(&written)?;
        std::io::Write::write_all(&mut file, self.to_text().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(written, &path)?;
        // And the rename itself, or a power cut can bring the old file back.
        #[cfg(unix)]
        if let Some(dir) = path.parent() {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
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
                ("theme", value) => {
                    if let Some(theme) = Theme::from_key(value) {
                        settings.theme = theme;
                    }
                }
                ("style", value) => {
                    if let Some(style) = WidgetStyle::from_key(value) {
                        settings.style = style;
                    }
                }
                ("text_size", value) => {
                    if let Some(text_size) = TextSize::from_key(value) {
                        settings.text_size = text_size;
                    }
                }
                ("animations", value) => {
                    if let Some(animations) = Animations::from_key(value) {
                        settings.animations = animations;
                    }
                }
                ("show_avatars", "true") => settings.show_avatars = true,
                ("show_avatars", "false") => settings.show_avatars = false,
                ("notifications", "true") => settings.notifications = true,
                ("notifications", "false") => settings.notifications = false,
                ("notification_content", value) => {
                    if let Some(content) = NotificationContent::from_key(value) {
                        settings.notification_content = content;
                    }
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
                ("device_name", value) => {
                    let name = clean_device_name(value);
                    settings.device_name = (!name.is_empty()).then_some(name);
                }
                _ => {}
            }
        }
        settings
    }

    fn to_text(&self) -> String {
        format!(
            "theme = {}\nstyle = {}\ntext_size = {}\nanimations = {}\nshow_avatars = {}\nnotifications = {}\n\
             notification_content = {}\nshow_activity = {}\nshow_calendar = {}\n\
             close_to_tray = {}\n\
             read_receipts = {}\ntyping_indicators = {}\nlink_previews = {}\nsync_to_phone = {}\n",
            self.theme.key(),
            self.style.key(),
            self.text_size.key(),
            self.animations.key(),
            self.show_avatars,
            self.notifications,
            self.notification_content.key(),
            self.show_activity,
            self.show_calendar,
            self.close_to_tray,
            self.preferences.read_receipts,
            self.preferences.typing_indicators,
            self.preferences.link_previews,
            self.preferences.sync_to_phone
        ) + &self
            .device_name
            .as_deref()
            .map(|name| format!("device_name = {}\n", clean_device_name(name)))
            .unwrap_or_default()
    }
}

/// Where b33p keeps its own files on this platform, under its one application id.
pub fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("io.github", "barafael", "b33p")
}

/// Where the messages and keys are kept. `B33P_DATA_DIR` keeps a separate account, e.g. to
/// link a second device for testing; a development aid, so release builds always use the
/// standard place.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(dir) = std::env::var_os("B33P_DATA_DIR")
    {
        return Some(PathBuf::from(dir));
    }
    Some(project_dirs()?.data_dir().to_path_buf())
}

/// Where the log file goes: beside a separate data directory (`B33P_DATA_DIR`), else in the
/// platform's place for local data.
pub fn log_dir() -> Option<PathBuf> {
    if custom_data_dir() {
        return Some(data_dir()?.join("logs"));
    }
    Some(project_dirs()?.data_local_dir().join("logs"))
}

/// Whether [`data_dir`] is the standard place.
pub fn custom_data_dir() -> bool {
    cfg!(debug_assertions) && std::env::var_os("B33P_DATA_DIR").is_some()
}

fn path() -> Option<PathBuf> {
    Some(project_dirs()?.config_dir().join("settings.conf"))
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
                animations: Animations::Off,
                show_avatars: false,
                notifications: false,
                notification_content: NotificationContent::NameOnly,
                show_activity: true,
                show_calendar: true,
                close_to_tray: false,
                device_name: Some("Work = laptop".into()),
                preferences: crate::Preferences {
                    read_receipts: false,
                    typing_indicators: false,
                    link_previews: true,
                    sync_to_phone: true,
                },
            };
            assert_eq!(Settings::parse(&settings.to_text()), settings);
        }
        let unnamed = Settings::default();
        assert_eq!(Settings::parse(&unnamed.to_text()), unnamed);
    }

    #[test]
    fn device_names_are_one_short_line() {
        assert_eq!(clean_device_name("  my\nlaptop  "), "my laptop");
        assert_eq!(
            clean_device_name(&"x".repeat(80)).chars().count(),
            MAX_DEVICE_NAME
        );
        assert!(default_device_name().starts_with("b33p"));
        assert_eq!(
            Settings::parse("device_name =   ").device_name,
            None,
            "an empty name is the default one"
        );
    }

    #[test]
    fn animations_round_trip_and_follow_the_desktop() {
        for animations in Animations::ALL {
            let settings = Settings {
                animations,
                ..Settings::default()
            };
            assert_eq!(Settings::parse(&settings.to_text()), settings);
        }
        assert!(Animations::System.enabled(false));
        assert!(!Animations::System.enabled(true));
        assert!(Animations::On.enabled(true));
        assert!(!Animations::Off.enabled(false));
    }

    /// Every enum reads back what each of its values writes.
    #[test]
    fn enum_keys_round_trip() {
        for theme in Theme::ALL {
            assert_eq!(Theme::from_key(theme.key()), Some(theme));
        }
        for style in WidgetStyle::ALL {
            assert_eq!(WidgetStyle::from_key(style.key()), Some(style));
        }
        for text_size in TextSize::ALL {
            assert_eq!(TextSize::from_key(text_size.key()), Some(text_size));
        }
        for animations in Animations::ALL {
            assert_eq!(Animations::from_key(animations.key()), Some(animations));
        }
        for content in NotificationContent::ALL {
            assert_eq!(NotificationContent::from_key(content.key()), Some(content));
        }
    }

    #[test]
    fn ignores_what_it_does_not_know() {
        let settings =
            // `style = fluent`: a style earlier versions had.
            Settings::parse(
                "theme = sepia\nfuture_option = 3\ngarbage\nstyle = fluent\ntext_size=small\n",
            );
        assert_eq!(
            settings,
            Settings {
                text_size: TextSize::Small,
                ..Settings::default()
            }
        );
    }
}
