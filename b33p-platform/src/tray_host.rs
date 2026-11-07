//! Whether the desktop shows tray icons. Closing the window leaves b33p running in the tray
//! only where something shows it; elsewhere nobody could reach it again.
//!
//! On Linux that is a StatusNotifierItem host (KDE Plasma, most other desktops, GNOME only
//! with the AppIndicator extension), which the StatusNotifierWatcher on the session bus knows
//! about. Windows and macOS always have one. Shared by both windows' trays.

use std::time::Duration;

/// Whether a tray host is there, or comes up within `wait`. Blocks: not for the UI thread.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn wait_for_host(wait: Duration) -> bool {
    use std::sync::{PoisonError, atomic::Ordering};
    use std::time::Instant;
    let deadline = Instant::now() + wait;
    let mut changed = host::CHANGED
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    loop {
        if host_present() {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        // Until `watch` runs (and if it never can), nothing says when a host comes: ask
        // again every so often.
        let next = if host::WATCHED.load(Ordering::Acquire) {
            left
        } else {
            left.min(Duration::from_millis(500))
        };
        changed = host::CHANGED
            .1
            .wait_timeout(changed, next)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn wait_for_host(_wait: Duration) -> bool {
    host_present()
}

/// Whether something shows tray icons right now. Without it, a closed window would leave b33p
/// running where nobody can reach it.
///
/// On Linux that is a StatusNotifierItem host: KDE Plasma, most other desktops, GNOME only with
/// the AppIndicator extension. Asked on the UI thread (closing the window, opening settings),
/// so it answers from what [`watch`] keeps up to date, and asks the bus only until that
/// runs.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn host_present() -> bool {
    use std::sync::atomic::Ordering;
    if host::WATCHED.load(Ordering::Acquire) {
        return host::PRESENT.load(Ordering::Acquire);
    }
    query_host(watcher_property)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn query_host(property: impl Fn(&str) -> zbus::Result<zbus::zvariant::OwnedValue>) -> bool {
    property("IsStatusNotifierHostRegistered")
        .and_then(|value| Ok(bool::try_from(value)?))
        .unwrap_or_else(|error| {
            tracing::debug!(%error, "no tray host");
            false
        })
}

#[cfg(all(unix, not(target_os = "macos")))]
mod host {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Condvar, Mutex, PoisonError};

    /// What [`super::host_present`] answers once `WATCHED`.
    pub static PRESENT: AtomicBool = AtomicBool::new(false);
    pub static WATCHED: AtomicBool = AtomicBool::new(false);
    /// Notified when `PRESENT` or `WATCHED` may have changed, for [`super::wait_for_host`].
    pub static CHANGED: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

    /// Wakes whoever waits on `CHANGED`. Under its lock, so a waiter that just found nothing
    /// has gone to sleep by then and is woken.
    pub fn notify() {
        let _held = CHANGED.0.lock().unwrap_or_else(PoisonError::into_inner);
        CHANGED.1.notify_all();
    }
}

/// Keeps [`host_present`]'s answer up to date on threads of its own: asked once, then again
/// whenever a tray host comes or goes, or the watcher that knows about them does (a panel
/// restarting). Without a session bus, `host_present` keeps asking, and finds none.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn watch() {
    use std::sync::atomic::Ordering;
    let spawned = std::thread::Builder::new()
        .name("b33p-tray-host".into())
        .spawn(|| {
            let Ok(connection) = zbus::blocking::Connection::session() else {
                return;
            };
            let ask = {
                let connection = connection.clone();
                move || {
                    let present = query_host(|name| property_on(&connection, name));
                    host::PRESENT.store(present, Ordering::Release);
                    host::notify();
                }
            };
            let hosts = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.kde.StatusNotifierWatcher")
                .map(|rule| rule.build());
            let watcher = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")
                .and_then(|rule| rule.interface("org.freedesktop.DBus"))
                .and_then(|rule| rule.member("NameOwnerChanged"))
                .and_then(|rule| rule.add_arg("org.kde.StatusNotifierWatcher"))
                .map(|rule| rule.build());
            let (Ok(hosts), Ok(watcher)) = (hosts, watcher) else {
                return;
            };
            let iterators = [hosts, watcher].map(|rule| {
                zbus::blocking::MessageIterator::for_match_rule(rule, &connection, Some(8))
            });
            let [Ok(hosts), Ok(watcher)] = iterators else {
                return;
            };
            // Subscribed first, so nothing between asking and listening goes unnoticed.
            ask();
            host::WATCHED.store(true, Ordering::Release);
            host::notify();
            let on_watcher = {
                let ask = ask.clone();
                move || {
                    for _ in watcher {
                        ask();
                    }
                }
            };
            let spawned = std::thread::Builder::new()
                .name("b33p-tray-watcher".into())
                .spawn(on_watcher);
            // Tray hosts are still followed; only a restarted watcher would go unnoticed.
            if let Err(error) = spawned {
                tracing::warn!(%error, "can't watch for the tray watcher");
            }
            for _ in hosts {
                ask();
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "can't watch for the tray");
    }
}

/// Windows and macOS always have one.
#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn watch() {}

/// The services registered with the StatusNotifierWatcher, which tray hosts take their icons
/// from; an icon's is named `org.kde.StatusNotifierItem-<pid>-<n>`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn registered_items() -> zbus::Result<Vec<String>> {
    Ok(Vec::<String>::try_from(watcher_property(
        "RegisteredStatusNotifierItems",
    )?)?)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn watcher_property(name: &str) -> zbus::Result<zbus::zvariant::OwnedValue> {
    property_on(&zbus::blocking::Connection::session()?, name)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn property_on(
    connection: &zbus::blocking::Connection,
    name: &str,
) -> zbus::Result<zbus::zvariant::OwnedValue> {
    let reply = connection.call_method(
        Some("org.kde.StatusNotifierWatcher"),
        "/StatusNotifierWatcher",
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.kde.StatusNotifierWatcher", name),
    )?;
    reply.body().deserialize()
}

/// Windows and macOS always have one.
#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn host_present() -> bool {
    true
}
