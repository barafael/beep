//! The icon in the system tray, which keeps langis running while its window is closed.

use std::rc::Rc;
use std::time::{Duration, Instant};

use langis_core::state::Update;

use crate::Shared;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/tray.rs"));
}

pub use generated::Tray;

/// Puts langis in the tray: clicking the icon opens the window, its menu also quits.
pub fn create(shared: &Rc<Shared>) -> Result<Tray, slint::PlatformError> {
    let tray = Tray::new()?;
    tray.set_shown(shared.settings.borrow().close_to_tray);
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

/// For a start into the tray (`--hidden`) before the desktop shows tray icons, as can happen
/// at login: gives the tray `wait` to come up, then opens the window after all, so langis isn't
/// left running out of reach.
pub fn open_unless_host_appears(shared: &Rc<Shared>, wait: Duration) {
    let (sender, appeared) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("langis-tray-wait".into())
        .spawn(move || {
            let deadline = Instant::now() + wait;
            let mut present = host_present();
            while !present && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(500));
                present = host_present();
            }
            let _ = sender.try_send(present);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "can't wait for the tray");
    }
    let shared = shared.clone();
    let _ = slint::spawn_local(async move {
        // Also when the thread didn't start.
        if appeared.recv().await != Ok(true) {
            tracing::info!("no tray showed up; opening the window");
            shared.stop_hiding();
            return;
        }
        // Slint gives up on an icon that found no tray, so it needs a new one.
        match create(&shared) {
            Ok(tray) => {
                shared.tray.replace(Some(tray));
                shared.refresh(Update::default());
                // Slint brings up the icon in its next timer pass; with nothing else going on
                // (no window shown) that needs a timer.
                slint::Timer::single_shot(Duration::ZERO, || {});
            }
            Err(error) => {
                tracing::warn!(%error, "failed to put langis in the tray");
                shared.stop_hiding();
            }
        }
    });
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
