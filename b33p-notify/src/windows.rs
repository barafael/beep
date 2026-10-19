//! Windows toast notifications. A text box with a Send button gives inline replies; the
//! `Activated` event reports clicks and answers while the app runs. (Handling them after the
//! app exited would need a registered COM activator, which b33p doesn't need: closing the
//! window quits.)

use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{IReference, TypedEventHandler};
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::core::{HSTRING, IInspectable, Interface};

use crate::{Action, AppInfo, Backend, Notification};

/// Toasts are grouped under this name, so withdrawing can find them by tag.
const GROUP: &str = "messages";

pub struct Toasts {
    app_id: HSTRING,
    notifier: ToastNotifier,
    actions: async_channel::Sender<Action>,
}

impl Toasts {
    pub fn new(
        app: &AppInfo,
        actions: async_channel::Sender<Action>,
    ) -> windows::core::Result<Self> {
        register(app)?;
        let app_id = HSTRING::from(app.id);
        // Attributes our windows and toasts to the registered id.
        unsafe { SetCurrentProcessExplicitAppUserModelID(&app_id)? };
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&app_id)?;
        Ok(Self {
            app_id,
            notifier,
            actions,
        })
    }

    fn try_show(&self, notification: Notification) -> windows::core::Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(toast_xml(&notification)))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        if let Some(key) = &notification.key {
            toast.SetTag(&tag(key))?;
            toast.SetGroup(&HSTRING::from(GROUP))?;
        }
        let actions = self.actions.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, args| {
                if let Some(action) = args
                    .as_ref()
                    .and_then(|args| activation(args).ok().flatten())
                {
                    let _ = actions.try_send(action);
                }
                Ok(())
            },
        ))?;
        self.notifier.Show(&toast)
    }
}

impl Backend for Toasts {
    fn show(&self, notification: Notification) {
        if let Err(error) = self.try_show(notification) {
            tracing::warn!(%error, "failed to show a notification");
        }
    }

    fn withdraw(&self, key: &str) {
        let removed = ToastNotificationManager::History().and_then(|history| {
            history.RemoveGroupedTagWithId(&tag(key), &HSTRING::from(GROUP), &self.app_id)
        });
        if let Err(error) = removed {
            tracing::debug!(%error, "failed to withdraw a notification");
        }
    }
}

/// Unpackaged apps must register their AppUserModelID (with a display name) before Windows
/// shows their toasts; the icon is optional, and taken from a file.
fn register(app: &AppInfo) -> windows::core::Result<()> {
    let key = windows_registry::CURRENT_USER.create(app_key(app))?;
    key.set_string("DisplayName", app.name)?;
    if let Some(path) = icon_path() {
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, app.icon));
        match written {
            Ok(()) => key.set_string("IconUri", path.to_string_lossy())?,
            Err(error) => tracing::debug!(%error, "no icon for notifications"),
        }
    }
    Ok(())
}

pub fn unregister(app: &AppInfo) {
    match windows_registry::CURRENT_USER.remove_tree(app_key(app)) {
        Ok(()) => println!("Removed the notification registration."),
        Err(error) => println!("No notification registration removed: {error}"),
    }
    if let Some(path) = icon_path() {
        let _ = std::fs::remove_file(&path);
        if let Some(dir) = path.parent() {
            // Only if nothing else is in it.
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn app_key(app: &AppInfo) -> String {
    format!(r"Software\Classes\AppUserModelId\{}", app.id)
}

/// Where the toasts' icon is written: in the user's local application data, which outlives
/// the folder b33p was unpacked to.
fn icon_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")?;
    Some(
        std::path::Path::new(&dir)
            .join("b33p")
            .join("notification-icon.ico"),
    )
}

/// Reads what the user did from the `Activated` event.
fn activation(args: &IInspectable) -> windows::core::Result<Option<Action>> {
    let args: ToastActivatedEventArgs = args.cast()?;
    let arguments = args.Arguments()?.to_string();
    Ok(match arguments.split_once(':') {
        Some(("open", key)) => Some(Action::Open {
            key: (!key.is_empty()).then(|| key.to_string()),
            activation_token: None,
        }),
        Some(("reply", key)) => {
            // Text inputs come back as boxed strings.
            let text = args
                .UserInput()?
                .Lookup(&HSTRING::from("reply"))?
                .cast::<IReference<HSTRING>>()?
                .Value()?;
            Some(Action::Reply {
                key: key.to_string(),
                text: text.to_string(),
            })
        }
        _ => None,
    })
}

fn toast_xml(notification: &Notification) -> String {
    let key = notification.key.as_deref().unwrap_or_default();
    let reply = match &notification.key {
        Some(key) => format!(
            r#"<actions><input id="reply" type="text" placeHolderContent="Reply…"/><action content="Send" arguments="reply:{key}" hint-inputId="reply" activationType="foreground"/></actions>"#,
            key = escape(key)
        ),
        None => String::new(),
    };
    format!(
        r#"<toast launch="open:{key}"><visual><binding template="ToastGeneric"><text>{title}</text><text>{body}</text></binding></visual>{reply}</toast>"#,
        key = escape(key),
        title = escape(&notification.title),
        body = escape(&notification.body),
    )
}

/// Tags are limited to 64 characters, so long keys are hashed.
fn tag(key: &str) -> HSTRING {
    if key.chars().count() <= 64 {
        return HSTRING::from(key);
    }
    let hash = key.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    HSTRING::from(format!("{hash:016x}"))
}

use crate::escape_xml as escape;
