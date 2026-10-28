//! The icon in the system tray, which keeps b33p running while its window is closed.

use std::rc::Rc;
use std::time::Duration;

use b33p_core::state::Update;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::Shared;

// Slint's code, not ours, discards what it likes.
#[allow(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/tray.rs"));
}

pub use generated::Tray;

/// Puts b33p in the tray: clicking the icon opens the window, its menu also quits.
pub fn create(shared: &Rc<Shared>) -> Result<Tray, slint::PlatformError> {
    let tray = Tray::new()?;
    tray.set_shown(shared.settings.borrow().close_to_tray);
    tray.set_unread_icon(unread_icon());
    tray.on_open({
        let shared = Rc::downgrade(shared);
        move || {
            if let Some(shared) = shared.upgrade() {
                shared.activate(None);
            }
        }
    });
    tray.on_quit(|| {
        // Fails only once the event loop has stopped, which is what quitting asks for.
        slint::quit_event_loop().ok();
    });
    Ok(tray)
}

/// The app icon with a red dot in its top right corner, for unread messages. Empty if the icon
/// can't be rendered, which leaves the plain icon.
fn unread_icon() -> Image {
    let svg = include_bytes!("../assets/b33p.svg");
    let Some(mut pixels) = Image::load_from_svg_data(svg)
        .ok()
        .and_then(|icon| icon.to_rgba8())
    else {
        return Image::default();
    };
    add_dot(&mut pixels);
    Image::from_rgba8(pixels)
}

/// Draws the unread dot: red with a white rim, so it stands out on the blue icon and on both
/// light and dark panels. Anti-aliased by how much of each pixel the circles cover.
fn add_dot(pixels: &mut SharedPixelBuffer<Rgba8Pixel>) {
    let size = pixels.width().min(pixels.height()) as f32;
    let radius = size * 0.16;
    let rim = size * 0.04;
    let (cx, cy) = (pixels.width() as f32 - radius - rim, radius + rim);
    let width = pixels.width() as usize;
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i % width) as f32 + 0.5, (i / width) as f32 + 0.5);
        let distance = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        let cover = |r: f32| (r - distance + 0.5).clamp(0.0, 1.0);
        let (outer, inner) = (cover(radius + rim), cover(radius));
        if outer == 0.0 {
            continue;
        }
        let blend = |under: u8, over: u8, alpha: f32| {
            (under as f32 * (1.0 - alpha) + over as f32 * alpha).round() as u8
        };
        let (r, g, b) = (
            blend(255, 0xe5, inner),
            blend(255, 0x38, inner),
            blend(255, 0x3b, inner),
        );
        *pixel = Rgba8Pixel {
            r: blend(pixel.r, r, outer),
            g: blend(pixel.g, g, outer),
            b: blend(pixel.b, b, outer),
            a: blend(pixel.a, 255, outer),
        };
    }
}

/// For a start into the tray (`--hidden`) before the desktop shows tray icons, as can happen
/// at login: gives the tray `wait` to come up, then opens the window after all, so b33p isn't
/// left running out of reach.
pub fn open_unless_host_appears(shared: &Rc<Shared>, wait: Duration) {
    let (sender, appeared) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("b33p-tray-wait".into())
        .spawn(move || {
            // The only message, so there is room for it; nobody is left to read it only if
            // the wait below could not start.
            sender.try_send(wait_for_host(wait)).ok();
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "can't wait for the tray");
    }
    let shared = shared.clone();
    let waiting = slint::spawn_local(async move {
        // Also when the thread didn't start.
        if appeared.recv().await != Ok(true) {
            tracing::info!("no tray showed up; opening the window");
            shared.stop_hiding();
            return;
        }
        // Slint gives up on an icon that found nowhere to register, so then it needs a new one.
        // Slint registers on this thread too, so this can't overlap with its own attempt; and
        // as replacing the icon takes down the old one, it never shows twice.
        if icon_registered() {
            return;
        }
        tracing::info!("the tray came up; putting b33p in it");
        match create(&shared) {
            Ok(tray) => {
                shared.tray.replace(Some(tray));
                shared.refresh(Update::default());
                // Slint brings up the icon in its next timer pass; with nothing else going on
                // (no window shown) that needs a timer.
                slint::Timer::single_shot(Duration::ZERO, || {});
            }
            Err(error) => {
                tracing::warn!(%error, "failed to put b33p in the tray");
                shared.stop_hiding();
            }
        }
    });
    if let Err(error) = waiting {
        tracing::warn!(%error, "can't wait for the tray; the window stays hidden");
    }
}

/// Whether a tray host is there, or comes up within `wait`.
#[cfg(all(unix, not(target_os = "macos")))]
fn wait_for_host(wait: Duration) -> bool {
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
        // Until `watch_host` runs (and if it never can), nothing says when a host comes: ask
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
fn wait_for_host(_wait: Duration) -> bool {
    host_present()
}

/// Whether something shows tray icons right now. Without it, a closed window would leave b33p
/// running where nobody can reach it.
///
/// On Linux that is a StatusNotifierItem host: KDE Plasma, most other desktops, GNOME only with
/// the AppIndicator extension. Asked on the UI thread (closing the window, opening settings),
/// so it answers from what [`watch_host`] keeps up to date, and asks the bus only until that
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
pub fn watch_host() {
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
pub fn watch_host() {}

/// Whether b33p's icon is registered with the StatusNotifierWatcher, which tray hosts take
/// their icons from. Its service is named `org.kde.StatusNotifierItem-<pid>-<n>`.
#[cfg(all(unix, not(target_os = "macos")))]
fn icon_registered() -> bool {
    let ours = format!("org.kde.StatusNotifierItem-{}-", std::process::id());
    watcher_property("RegisteredStatusNotifierItems")
        .and_then(|value| Ok(Vec::<String>::try_from(value)?))
        .is_ok_and(|items| items.iter().any(|item| item.starts_with(&ours)))
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

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn icon_registered() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_icon_has_a_red_dot_top_right() {
        let icon = unread_icon();
        let pixels = icon.to_rgba8().expect("the icon renders");
        let (width, height) = (pixels.width() as usize, pixels.height() as usize);
        assert!(width >= 32 && height >= 32);
        let at = |x: usize, y: usize| pixels.as_slice()[y * width + x];
        let size = width.min(height) as f32;
        let centre = at(width - (size * 0.27) as usize, (size * 0.27) as usize);
        assert_eq!(
            (centre.r, centre.g, centre.b, centre.a),
            (0xe5, 0x38, 0x3b, 255)
        );
        // The rest of the icon is untouched: its bottom left corner is still transparent.
        assert_eq!(at(0, height - 1).a, 0);
    }
}
