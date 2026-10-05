//! The icon in the system tray, which keeps langis running while its window is closed.

use std::rc::Rc;

use crate::Shared;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/tray.rs"));
}

pub use generated::Tray;

/// Puts langis in the tray: clicking the icon opens the window, its menu also quits.
pub fn create(shared: &Rc<Shared>) -> Result<Tray, slint::PlatformError> {
    let tray = Tray::new()?;
    tray.on_open({
        let shared = Rc::downgrade(shared);
        move || {
            if let Some(ui) = shared.upgrade().and_then(|shared| shared.ui()) {
                ui.raise();
            }
        }
    });
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    Ok(tray)
}

/// Whether something shows tray icons right now. Without it, a closed window would leave langis
/// running where nobody can reach it.
///
/// On Linux that is a StatusNotifierItem host: KDE Plasma, most other desktops, GNOME only with
/// the AppIndicator extension.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn host_present() -> bool {
    let registered = || -> zbus::Result<bool> {
        let connection = zbus::blocking::Connection::session()?;
        let reply = connection.call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(
                "org.kde.StatusNotifierWatcher",
                "IsStatusNotifierHostRegistered",
            ),
        )?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        Ok(bool::try_from(value)?)
    };
    registered().unwrap_or_else(|error| {
        tracing::debug!(%error, "no tray host");
        false
    })
}

/// Windows and macOS always have one.
#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn host_present() -> bool {
    true
}
