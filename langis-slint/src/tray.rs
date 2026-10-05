//! The icon in the system tray, which keeps langis running while its window is closed.

use std::rc::Rc;
use std::time::{Duration, Instant};

use langis_core::state::Update;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::Shared;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/tray.rs"));
}

pub use generated::Tray;

/// Puts langis in the tray: clicking the icon opens the window, its menu also quits.
pub fn create(shared: &Rc<Shared>) -> Result<Tray, slint::PlatformError> {
    let tray = Tray::new()?;
    tray.set_shown(shared.settings.borrow().close_to_tray);
    tray.set_unread_icon(unread_icon());
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

/// The app icon with a red dot in its top right corner, for unread messages. Empty if the icon
/// can't be rendered, which leaves the plain icon.
fn unread_icon() -> Image {
    let svg = include_bytes!("../assets/langis.svg");
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
        // Slint gives up on an icon that found nowhere to register, so then it needs a new one.
        // Slint registers on this thread too, so this can't overlap with its own attempt; and
        // as replacing the icon takes down the old one, it never shows twice.
        if icon_registered() {
            return;
        }
        tracing::info!("the tray came up; putting langis in it");
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
    watcher_property("IsStatusNotifierHostRegistered")
        .and_then(|value| Ok(bool::try_from(value)?))
        .unwrap_or_else(|error| {
            tracing::debug!(%error, "no tray host");
            false
        })
}

/// Whether langis' icon is registered with the StatusNotifierWatcher, which tray hosts take
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
    let connection = zbus::blocking::Connection::session()?;
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
