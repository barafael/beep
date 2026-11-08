//! The icon in the system tray, which keeps b33p running while its window is closed.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use b33p_core::state::Update;
use slint::Image;

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
    DRAWN.set(None);
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

/// Shows `unread` on the tray icon: the count drawn into it, and in words in its tooltip.
pub fn show_unread(tray: &Tray, unread: u32) {
    tray.set_unread(unread.try_into().unwrap_or(i32::MAX));
    tray.set_unread_words(crate::badge::words(unread).into());
    // Redrawn only when what it says changes: above 9 it says "9+" for any count.
    let label = crate::badge::tray_label(unread);
    if label.is_some() && DRAWN.replace(label.clone()) != label {
        tray.set_unread_icon(unread_icon(unread));
    }
}

thread_local! {
    /// What the tray icon drawn last says; reset with each new tray.
    static DRAWN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The app icon with the count in a red circle in its top right corner. Empty if the icon
/// can't be rendered, which leaves the plain icon.
fn unread_icon(unread: u32) -> Image {
    let svg = include_bytes!("../assets/b33p.svg");
    let Some(mut pixels) = Image::load_from_svg_data(svg)
        .ok()
        .and_then(|icon| icon.to_rgba8())
    else {
        return Image::default();
    };
    crate::badge::add_to_icon(&mut pixels, unread);
    Image::from_rgba8(pixels)
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
            sender
                .try_send(b33p_platform::tray_host::wait_for_host(wait))
                .ok();
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

pub use b33p_platform::tray_host::{host_present, watch as watch_host};

/// Whether b33p's icon is registered with the StatusNotifierWatcher, which tray hosts take
/// their icons from. Its service is named `org.kde.StatusNotifierItem-<pid>-<n>`.
#[cfg(all(unix, not(target_os = "macos")))]
fn icon_registered() -> bool {
    let ours = format!("org.kde.StatusNotifierItem-{}-", std::process::id());
    b33p_platform::tray_host::registered_items()
        .is_ok_and(|items| items.iter().any(|item| item.starts_with(&ours)))
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn icon_registered() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_icon_has_the_count_top_right() {
        let icon = unread_icon(3);
        let pixels = icon.to_rgba8().expect("the icon renders");
        crate::badge::tests::save("tray-3", &pixels);
        let many = unread_icon(12).to_rgba8().expect("the icon renders");
        crate::badge::tests::save("tray-12", &many);
        let (width, height) = (pixels.width() as usize, pixels.height() as usize);
        assert!(width >= 32 && height >= 32);
        let at = |x: usize, y: usize| pixels.as_slice()[y * width + x];
        let rgba = |x, y| {
            let p = at(x, y);
            (p.r, p.g, p.b, p.a)
        };
        // Red just inside the circle's rim, at its middle height.
        let radius = width.min(height) as f32 * 0.34;
        let left = width - (2.0 * radius) as usize;
        assert_eq!(rgba(left + 5, radius as usize), (0xe5, 0x38, 0x3b, 255));
        // The count in white inside it, well inside the white rim.
        let (cx, cy) = (width as f32 - radius, radius);
        let white = (0..height)
            .flat_map(|y| (left..width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let distance = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                distance < radius * 0.75 && rgba(x, y) == (255, 255, 255, 255)
            })
            .count();
        assert!(white > 20, "{white} white pixels");
        // The rest of the icon is untouched: its bottom left corner is still transparent.
        assert_eq!(at(0, height - 1).a, 0);
    }
}
