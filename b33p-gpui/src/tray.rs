//! The icon in the system tray, which keeps b33p running while its window is closed. GPUI has
//! no tray of its own, so this is the platform's: the StatusNotifierItem protocol on Linux
//! (`ksni`, which rides on the same `zbus` as the notifications and the single-instance lock),
//! `tray-icon` on Windows and macOS.
//!
//! The icon is drawn here, from the shapes of `b33p-slint/assets/b33p.svg`, so that no SVG
//! renderer is needed: a tray icon is pixels, and these are few.

use async_channel::Sender;

pub use b33p_platform::tray_host::{host_present, wait_for_host, watch as watch_host};

/// What the user asked of the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A click on the icon, or "Open b33p" in its menu: show the window.
    Open,
    /// "Quit" in its menu.
    Quit,
}

/// The icon, and what it shows. Dropping it takes the icon away.
pub struct Tray {
    inner: platform::Tray,
}

impl Tray {
    /// Puts b33p in the tray when `shown`; `events` hears what the user does with it. `None` if
    /// the platform has no way to (no session bus, say): then nothing is in the tray, and
    /// closing the window must quit.
    pub fn new(events: Sender<Event>, shown: bool) -> Option<Self> {
        platform::Tray::new(events, shown).map(|inner| Self { inner })
    }

    /// The unread messages the icon says it has: a dot on it, and a line in its tooltip.
    pub fn set_unread(&self, unread: u32) {
        self.inner.set_unread(unread);
    }

    /// Shows or takes away the icon, as "Keep running in the tray" says.
    pub fn set_shown(&self, shown: bool) {
        self.inner.set_shown(shown);
    }
}

/// What the tooltip says.
pub fn tooltip(unread: u32) -> String {
    match unread {
        0 => "b33p".into(),
        1 => "b33p — 1 unread message".into(),
        n => format!("b33p — {n} unread messages"),
    }
}

/// The pixels (RGBA, straight alpha) of b33p's logo at `size` square, with a red dot in the
/// top right corner for unread messages.
///
/// The logo is the circle, the tail and the three dots of `b33p.svg`, in its 64 by 64 units,
/// covered with 4 by 4 samples per pixel.
pub fn icon_rgba(size: u32, unread: bool) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    const BLUE: [f32; 3] = [0x3a as f32, 0x76 as f32, 0xf0 as f32];
    const WHITE: [f32; 3] = [255., 255., 255.];
    let in_circle = |x: f32, y: f32, cx: f32, cy: f32, r: f32| (x - cx).powi(2) + (y - cy).powi(2) <= r * r;
    let in_tail = |x: f32, y: f32| {
        // The triangle (14, 54), (18, 40), (28, 48).
        let side = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        let (p, q, r) = ((14., 54.), (18., 40.), (28., 48.));
        let signs = [side(p, q), side(q, r), side(r, p)];
        signs.iter().all(|s| *s >= 0.) || signs.iter().all(|s| *s <= 0.)
    };
    let scale = 64. / size as f32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut covered = 0;
            let mut colour = [0f32; 3];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * scale;
                    let white = [22., 32., 42.]
                        .iter()
                        .any(|cx| in_circle(x, y, *cx, 30., 3.5));
                    let blue = in_circle(x, y, 32., 30., 24.) || in_tail(x, y);
                    if white || blue {
                        covered += 1;
                        let source = if white { WHITE } else { BLUE };
                        for (sum, value) in colour.iter_mut().zip(source) {
                            *sum += value;
                        }
                    }
                }
            }
            let samples = (SAMPLES * SAMPLES) as f32;
            if covered == 0 {
                pixels.extend([0, 0, 0, 0]);
            } else {
                let [r, g, b] = colour.map(|sum| (sum / covered as f32).round() as u8);
                pixels.extend([r, g, b, (255. * covered as f32 / samples).round() as u8]);
            }
        }
    }
    if unread {
        add_dot(&mut pixels, size);
    }
    pixels
}

/// Draws the unread dot: red with a white rim, so it stands out on the blue icon and on both
/// light and dark panels. Anti-aliased by how much of each pixel the circles cover.
fn add_dot(pixels: &mut [u8], size: u32) {
    let side = size as f32;
    let radius = side * 0.16;
    let rim = side * 0.04;
    let (cx, cy) = (side - radius - rim, radius + rim);
    let blend = |under: u8, over: u8, alpha: f32| {
        (f32::from(under) * (1.0 - alpha) + f32::from(over) * alpha).round() as u8
    };
    for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i as u32 % size) as f32 + 0.5, (i as u32 / size) as f32 + 0.5);
        let distance = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        let cover = |r: f32| (r - distance + 0.5).clamp(0.0, 1.0);
        let (outer, inner) = (cover(radius + rim), cover(radius));
        if outer == 0.0 {
            continue;
        }
        let red = [
            blend(255, 0xe5, inner),
            blend(255, 0x38, inner),
            blend(255, 0x3b, inner),
        ];
        for (channel, over) in pixel.iter_mut().zip(red) {
            *channel = blend(*channel, over, outer);
        }
        pixel[3] = blend(pixel[3], 255, outer);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::cell::{Cell, RefCell};

    use async_channel::Sender;
    use ksni::blocking::{Handle, TrayMethods as _};

    use super::{Event, icon_rgba, tooltip};

    /// What the tray host asks for: the icon's properties and its menu.
    struct Item {
        unread: u32,
        events: Sender<Event>,
    }

    impl Item {
        /// Told to the UI thread, which opens the window or quits; nothing here waits for it.
        fn send(&self, event: Event) {
            // Unbounded, and closed only once b33p is quitting.
            if let Err(error) = self.events.try_send(event) {
                tracing::debug!(%error, "the tray's event wasn't taken");
            }
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "io.github.barafael.B33p".into()
        }

        fn title(&self) -> String {
            "b33p".into()
        }

        fn category(&self) -> ksni::Category {
            ksni::Category::Communications
        }

        /// Hosts take the nearest of these sizes.
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            [22, 32, 48, 64]
                .into_iter()
                .map(|size| {
                    let rgba = icon_rgba(size, self.unread > 0);
                    // The protocol wants ARGB, most significant byte first.
                    let data = rgba
                        .chunks_exact(4)
                        .flat_map(|p| [p[3], p[0], p[1], p[2]])
                        .collect();
                    ksni::Icon {
                        width: size as i32,
                        height: size as i32,
                        data,
                    }
                })
                .collect()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: tooltip(self.unread),
                ..ksni::ToolTip::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(Event::Open);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::{MenuItem, StandardItem};
            vec![
                StandardItem {
                    label: "Open b33p".into(),
                    activate: Box::new(|this: &mut Self| this.send(Event::Open)),
                    ..StandardItem::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|this: &mut Self| this.send(Event::Quit)),
                    ..StandardItem::default()
                }
                .into(),
            ]
        }
    }

    pub struct Tray {
        events: Sender<Event>,
        unread: Cell<u32>,
        /// Present while the icon is shown.
        handle: RefCell<Option<Handle<Item>>>,
    }

    impl Tray {
        pub fn new(events: Sender<Event>, shown: bool) -> Option<Self> {
            let tray = Self {
                events,
                unread: Cell::new(0),
                handle: RefCell::new(None),
            };
            if shown {
                tray.show()?;
            }
            Some(tray)
        }

        /// Registers the icon with the StatusNotifierWatcher. Where no watcher is there yet (the
        /// desktop is still coming up at login) the icon waits for one, instead of failing:
        /// whether b33p may run without a window is [`super::host_present`]'s to say.
        fn show(&self) -> Option<()> {
            let item = Item {
                unread: self.unread.get(),
                events: self.events.clone(),
            };
            match item.assume_sni_available(true).spawn() {
                Ok(handle) => {
                    self.handle.replace(Some(handle));
                    Some(())
                }
                Err(error) => {
                    tracing::warn!(%error, "can't put b33p in the tray");
                    None
                }
            }
        }

        pub fn set_unread(&self, unread: u32) {
            if self.unread.replace(unread) == unread {
                return;
            }
            if let Some(handle) = &*self.handle.borrow() {
                handle.update(|item| item.unread = unread);
            }
        }

        pub fn set_shown(&self, shown: bool) {
            let present = self.handle.borrow().is_some();
            match (shown, present) {
                (true, false) => {
                    // A failure is logged; the next change to the setting tries again.
                    let _shown: Option<()> = self.show();
                }
                (false, true) => {
                    if let Some(handle) = self.handle.take() {
                        // The icon goes once the service has stopped; not worth waiting for.
                        drop(handle.shutdown());
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod platform {
    use std::cell::{Cell, RefCell};

    use async_channel::Sender;
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{Event, icon_rgba, tooltip};

    pub struct Tray {
        icon: RefCell<Option<TrayIcon>>,
        unread: Cell<u32>,
    }

    fn icon(unread: bool) -> Option<Icon> {
        const SIZE: u32 = 32;
        Icon::from_rgba(icon_rgba(SIZE, unread), SIZE, SIZE)
            .map_err(|error| tracing::warn!(%error, "the tray icon isn't valid"))
            .ok()
    }

    impl Tray {
        pub fn new(events: Sender<Event>, shown: bool) -> Option<Self> {
            let menu = Menu::new();
            let open = MenuItem::new("Open b33p", true, None);
            let quit = MenuItem::new("Quit", true, None);
            if let Err(error) = menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]) {
                tracing::warn!(%error, "can't make the tray's menu");
                return None;
            }
            let (open_id, quit_id): (MenuId, MenuId) = (open.id().clone(), quit.id().clone());
            // Called from the event loop of this thread; the window is opened by the UI's own.
            let menu_events = events.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let answer = if event.id == open_id {
                    Event::Open
                } else if event.id == quit_id {
                    Event::Quit
                } else {
                    return;
                };
                if let Err(error) = menu_events.try_send(answer) {
                    tracing::debug!(%error, "the tray's event wasn't taken");
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                    && let Err(error) = events.try_send(Event::Open)
                {
                    tracing::debug!(%error, "the tray's event wasn't taken");
                }
            }));
            let mut builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip(tooltip(0));
            if let Some(icon) = icon(false) {
                builder = builder.with_icon(icon);
            }
            let tray = Self {
                icon: RefCell::new(None),
                unread: Cell::new(0),
            };
            match builder.build() {
                Ok(icon) => {
                    if let Err(error) = icon.set_visible(shown) {
                        tracing::warn!(%error, "can't show the tray icon");
                    }
                    tray.icon.replace(Some(icon));
                    Some(tray)
                }
                Err(error) => {
                    tracing::warn!(%error, "can't put b33p in the tray");
                    None
                }
            }
        }

        pub fn set_unread(&self, unread: u32) {
            if self.unread.replace(unread) == unread {
                return;
            }
            if let Some(tray) = &*self.icon.borrow() {
                tray.set_icon(icon(unread > 0)).ok();
                tray.set_tooltip(Some(tooltip(unread))).ok();
            }
        }

        pub fn set_shown(&self, shown: bool) {
            if let Some(tray) = &*self.icon.borrow()
                && let Err(error) = tray.set_visible(shown)
            {
                tracing::warn!(%error, "can't change whether the tray icon shows");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pixels: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }

    #[test]
    fn the_logo_is_a_blue_bubble_with_three_white_dots() {
        let size = 64;
        let pixels = icon_rgba(size, false);
        assert_eq!(pixels.len(), (size * size * 4) as usize);
        // The bubble's body, the middle dot, and the corner outside it.
        assert_eq!(at(&pixels, size, 32, 12), [0x3a, 0x76, 0xf0, 255]);
        assert_eq!(at(&pixels, size, 32, 30), [255, 255, 255, 255]);
        assert_eq!(at(&pixels, size, 2, 2)[3], 0);
        // The tail below and left of the circle is part of it.
        assert_eq!(at(&pixels, size, 18, 47)[3], 255);
    }

    #[test]
    fn unread_adds_a_red_dot_top_right_and_leaves_the_rest() {
        let size = 64;
        let plain = icon_rgba(size, false);
        let unread = icon_rgba(size, true);
        let dot = (size as f32 * 0.2) as u32;
        assert_eq!(at(&unread, size, size - dot, dot), [0xe5, 0x38, 0x3b, 255]);
        // Far from the dot, nothing changed: the bottom left is transparent either way.
        assert_eq!(at(&unread, size, 0, size - 1), at(&plain, size, 0, size - 1));
        assert_eq!(at(&unread, size, 0, size - 1)[3], 0);
    }

    #[test]
    fn the_tooltip_counts_the_unread_messages() {
        assert_eq!(tooltip(0), "b33p");
        assert_eq!(tooltip(1), "b33p — 1 unread message");
        assert_eq!(tooltip(7), "b33p — 7 unread messages");
    }
}
