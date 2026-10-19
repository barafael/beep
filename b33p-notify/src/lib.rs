//! Desktop notifications: show one per conversation, open the conversation on click and,
//! where the desktop supports it, answer it straight from the popup.
//!
//! | Platform | Service | Inline reply |
//! |---|---|---|
//! | Linux, BSD | `org.freedesktop.Notifications` over D-Bus | KDE Plasma (`inline-reply` capability) |
//! | Windows 10/11 | Toast notifications | yes |
//! | macOS, others | none yet | – |
//!
//! Without a notification service, [`Notifier`] silently does nothing.

#[cfg(all(unix, not(target_os = "macos")))]
mod freedesktop;
#[cfg(windows)]
mod windows;

/// `text` as character data in XML or the HTML subset notification bodies may carry.
#[cfg(any(all(unix, not(target_os = "macos")), windows))]
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Identifies the app to the notification service.
pub struct AppInfo {
    /// Reverse-DNS id. On Linux it names the `.desktop` file (for the app's name, icon and
    /// grouping); on Windows it becomes the AppUserModelID.
    pub id: &'static str,
    pub name: &'static str,
    /// The app's icon as an `.ico` file, for Windows, whose toasts take it from a file;
    /// elsewhere the desktop finds it through `id`.
    pub icon: &'static [u8],
}

/// Removes what [`Notifier::new`] registered with the system, if anything: on Windows, the
/// AppUserModelID and its icon. For leaving no trace after the app is deleted.
pub fn unregister(app: &AppInfo) {
    #[cfg(windows)]
    windows::unregister(app);
    let _ = app;
}

pub struct Notification {
    /// Which conversation it is about; a newer notification with the same key replaces the
    /// older one. `None` for a summary that just opens the app.
    pub key: Option<String>,
    pub title: String,
    pub body: String,
}

/// What the user did with a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Clicked it: bring the app to the front, showing this conversation if there is one.
    Open {
        key: Option<String>,
        /// Lets the app take focus on Wayland, where the notification service provides one.
        activation_token: Option<String>,
    },
    /// Typed an answer into the popup.
    Reply { key: String, text: String },
}

trait Backend {
    fn show(&self, notification: Notification);
    fn withdraw(&self, key: &str);

    fn available(&self) -> bool {
        true
    }
}

#[allow(dead_code)] // Used where the platform has no backend, or it fails to start.
struct Disabled;

impl Backend for Disabled {
    fn show(&self, _: Notification) {}
    fn withdraw(&self, _: &str) {}

    fn available(&self) -> bool {
        false
    }
}

pub struct Notifier {
    backend: Box<dyn Backend>,
}

impl Notifier {
    /// Connects to the platform's notification service. User actions arrive on the returned
    /// channel, from whichever thread the service calls back on.
    pub fn new(app: &AppInfo) -> (Self, async_channel::Receiver<Action>) {
        let (actions, receiver) = async_channel::unbounded();
        (
            Self {
                backend: backend(app, actions),
            },
            receiver,
        )
    }

    pub fn show(&self, notification: Notification) {
        self.backend.show(notification);
    }

    /// Whether notifications can show at all: not where there is no service for them (macOS,
    /// for now), or it failed to start.
    pub fn available(&self) -> bool {
        self.backend.available()
    }

    /// Takes back the notification for `key`, e.g. because the conversation was read.
    pub fn withdraw(&self, key: &str) {
        self.backend.withdraw(key);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn backend(app: &AppInfo, actions: async_channel::Sender<Action>) -> Box<dyn Backend> {
    Box::new(freedesktop::Freedesktop::new(app, actions))
}

#[cfg(windows)]
fn backend(app: &AppInfo, actions: async_channel::Sender<Action>) -> Box<dyn Backend> {
    match windows::Toasts::new(app, actions) {
        Ok(toasts) => Box::new(toasts),
        Err(error) => {
            tracing::warn!(%error, "notifications unavailable");
            Box::new(Disabled)
        }
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn backend(_: &AppInfo, _: async_channel::Sender<Action>) -> Box<dyn Backend> {
    Box::new(Disabled)
}
