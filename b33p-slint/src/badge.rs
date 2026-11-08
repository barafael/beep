//! The unread count as a badge: a red circle with the number in white, on the tray icon (every
//! platform) and over the taskbar button (Windows, `ITaskbarList3::SetOverlayIcon`). The count
//! is `ChatState::unread_total`, the one the tray's dot showed: unread messages, muted chats
//! left out.
//!
//! The digits are drawn here, from strokes, rather than with a font: the same pixels on every
//! platform, at any size, with nothing to load, and testable without a display.
//!
//! Ported from Othereum/sigwin 61394d1, 7561454 and 57a59bd.

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// The badge's red, the same as the unread dot's was.
const RED: [u8; 3] = [0xe5, 0x38, 0x3b];

/// What the taskbar's badge says: up to two digits, then "99+". Nothing for no unread.
#[cfg(any(windows, test))]
pub fn taskbar_label(n: u32) -> Option<String> {
    match n {
        0 => None,
        1..=99 => Some(n.to_string()),
        _ => Some("99+".into()),
    }
}

/// What the tray's badge says: one digit, then "9+". The tray icon is scaled down to about 20
/// pixels, and a third of that holds no more than a digit and a plus (sigwin stops at 9, too);
/// the tooltip has the exact count.
pub fn tray_label(n: u32) -> Option<String> {
    match n {
        0 => None,
        1..=9 => Some(n.to_string()),
        _ => Some("9+".into()),
    }
}

/// The count in words, for the tooltip and screen readers.
pub fn words(n: u32) -> String {
    match n {
        1 => "1 unread message".into(),
        n => format!("{n} unread messages"),
    }
}

/// Where the text goes in a badge whose inner (red) circle has `radius`, centred on `(cx, cy)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBox {
    /// The left end of the first glyph's centre line.
    pub x: f32,
    /// The top of the glyphs' centre lines.
    pub y: f32,
    /// How tall a digit's centre line is: the glyphs' unit.
    pub height: f32,
    /// The strokes' width, in pixels.
    pub stroke: f32,
}

/// How tall a digit is next to the circle's radius, when it fits.
const DIGIT_HEIGHT: f32 = 1.1;
/// How wide the text may be next to the radius: what fits inside the circle with a margin.
const MAX_WIDTH: f32 = 1.5;
/// Stroke width next to a digit's height: a bold weight.
const STROKE: f32 = 0.18;
/// The space between glyphs, next to a digit's height.
const GAP: f32 = 0.14;

/// Lays out `label` in a circle of `radius` around `(cx, cy)`: as tall as [`DIGIT_HEIGHT`]
/// allows, shrunk until it fits [`MAX_WIDTH`], so two digits and "99+" get smaller ones. The
/// ink's top and left edges land on whole pixels, so horizontal strokes stay sharp.
pub fn text_box(label: &str, cx: f32, cy: f32, radius: f32) -> TextBox {
    let advance: f32 = label.chars().map(|c| glyph(c).0).sum::<f32>()
        + GAP * label.chars().count().saturating_sub(1) as f32;
    // The ink is a stroke wider than the centre lines: width (advance + STROKE) * height.
    let fits = MAX_WIDTH * radius / (advance + STROKE);
    let height = (DIGIT_HEIGHT * radius).min(fits);
    let stroke = (STROKE * height).max(1.0);
    let ink_height = (height + stroke).round().max(1.0);
    let height = ink_height - stroke;
    let ink_width = advance * height + stroke;
    let top = (cy - ink_height / 2.0).round();
    let left = (cx - ink_width / 2.0).round();
    TextBox {
        x: left + stroke / 2.0,
        y: top + stroke / 2.0,
        height,
        stroke,
    }
}

/// A glyph: its advance (width next to its height) and its strokes, as polylines in units of
/// its height, y down.
type Glyph = (f32, Vec<Vec<(f32, f32)>>);

/// An elliptic arc from `from` to `to` degrees (0 is right, 90 is down), as a polyline.
fn arc(cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    let steps = (((to - from).abs() / 10.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

fn glyph(c: char) -> Glyph {
    const W: f32 = 0.56;
    let line = |points: &[(f32, f32)]| points.to_vec();
    match c {
        '0' => (W, vec![arc(0.28, 0.5, 0.28, 0.5, 0.0, 360.0)]),
        '1' => (W, vec![line(&[(0.08, 0.2), (0.32, 0.0), (0.32, 1.0)])]),
        '2' => {
            let mut stroke = arc(0.28, 0.27, 0.28, 0.27, 195.0, 400.0);
            stroke.extend([(0.0, 1.0), (W, 1.0)]);
            (W, vec![stroke])
        }
        '3' => (
            W,
            vec![
                arc(0.28, 0.25, 0.26, 0.25, 200.0, 450.0),
                arc(0.28, 0.75, 0.28, 0.25, 270.0, 520.0),
            ],
        ),
        '4' => (
            W,
            vec![line(&[(0.42, 1.0), (0.42, 0.0), (0.0, 0.7), (W, 0.7)])],
        ),
        '5' => {
            let mut stroke = line(&[(0.52, 0.0), (0.07, 0.0), (0.04, 0.45)]);
            stroke.extend(arc(0.28, 0.67, 0.28, 0.33, 225.0, 520.0));
            (W, vec![stroke])
        }
        '6' => (
            W,
            vec![
                arc(0.28, 0.68, 0.28, 0.32, 0.0, 360.0),
                line(&[(0.46, 0.0), (0.03, 0.6)]),
            ],
        ),
        '7' => (W, vec![line(&[(0.0, 0.0), (W, 0.0), (0.18, 1.0)])]),
        '8' => (
            W,
            vec![
                arc(0.28, 0.245, 0.24, 0.245, 0.0, 360.0),
                arc(0.28, 0.74, 0.28, 0.26, 0.0, 360.0),
            ],
        ),
        '9' => (
            W,
            vec![
                arc(0.28, 0.32, 0.28, 0.32, 0.0, 360.0),
                line(&[(0.53, 0.4), (0.1, 1.0)]),
            ],
        ),
        '+' => (
            0.5,
            vec![
                line(&[(0.0, 0.5), (0.5, 0.5)]),
                line(&[(0.25, 0.25), (0.25, 0.75)]),
            ],
        ),
        _ => (0.0, Vec::new()),
    }
}

/// The text's strokes as line segments in pixels.
fn segments(label: &str, text: TextBox) -> Vec<((f32, f32), (f32, f32))> {
    let mut x = text.x;
    let mut out = Vec::new();
    for c in label.chars() {
        let (advance, strokes) = glyph(c);
        let at = |(gx, gy): (f32, f32)| (x + gx * text.height, text.y + gy * text.height);
        for stroke in strokes {
            out.extend(stroke.windows(2).map(|pair| (at(pair[0]), at(pair[1]))));
        }
        x += (advance + GAP) * text.height;
    }
    out
}

fn distance_to_segment(
    (px, py): (f32, f32),
    ((ax, ay), (bx, by)): ((f32, f32), (f32, f32)),
) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / length).clamp(0.0, 1.0)
    };
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}

/// Draws a badge over `pixels` (straight alpha, `width` wide): a circle of `radius` around
/// `(cx, cy)`, with a white rim `rim` wide around a red one, and `label` in white inside. Each
/// pixel is covered by how much of it the shapes cover, so the edges are anti-aliased.
pub fn draw(
    pixels: &mut [Rgba8Pixel],
    width: usize,
    (cx, cy): (f32, f32),
    radius: f32,
    rim: f32,
    label: &str,
) {
    let inner = radius - rim;
    let text = text_box(label, cx, cy, inner);
    let strokes = segments(label, text);
    let height = pixels.len() / width.max(1);
    let x_range =
        ((cx - radius).floor().max(0.0) as usize)..((cx + radius).ceil() as usize).min(width);
    let y_range =
        ((cy - radius).floor().max(0.0) as usize)..((cy + radius).ceil() as usize).min(height);
    for y in y_range {
        for x in x_range.clone() {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let distance = ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt();
            let cover = |r: f32| (r - distance + 0.5).clamp(0.0, 1.0);
            let (outer, red) = (cover(radius), cover(inner));
            if outer == 0.0 {
                continue;
            }
            let ink = strokes
                .iter()
                .map(|&s| distance_to_segment(p, s))
                .fold(f32::INFINITY, f32::min);
            let white = (text.stroke / 2.0 - ink + 0.5).clamp(0.0, 1.0);
            // White, then red over it where the inner circle is, then the text over that.
            let colour = |i: usize| {
                let c = 255.0 * (1.0 - red) + f32::from(RED[i]) * red;
                c * (1.0 - white) + 255.0 * white
            };
            let pixel = &mut pixels[y * width + x];
            // Source over, in straight alpha.
            let under = f32::from(pixel.a) / 255.0 * (1.0 - outer);
            let alpha = outer + under;
            let blend = |over: f32, under_c: u8| {
                ((over * outer + f32::from(under_c) * under) / alpha).round() as u8
            };
            *pixel = Rgba8Pixel {
                r: blend(colour(0), pixel.r),
                g: blend(colour(1), pixel.g),
                b: blend(colour(2), pixel.b),
                a: (alpha * 255.0).round() as u8,
            };
        }
    }
}

/// The taskbar's badge for `n` at `size` pixels square: the circle fills it, no rim (Windows
/// draws it over the corner of the app's icon). `None` for no unread.
// Tested everywhere, used only on Windows.
#[cfg(any(windows, test))]
pub fn overlay(size: u32, n: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let label = taskbar_label(n)?;
    let mut pixels = SharedPixelBuffer::new(size, size);
    let half = size as f32 / 2.0;
    draw(
        pixels.make_mut_slice(),
        size as usize,
        (half, half),
        half,
        0.0,
        &label,
    );
    Some(pixels)
}

/// Draws the tray's badge for `n` into the top right corner of the app's icon: about two
/// thirds of it wide, with a white rim so it stands out on the blue icon and on both light and
/// dark panels. Leaves the icon alone for no unread.
pub fn add_to_icon(pixels: &mut SharedPixelBuffer<Rgba8Pixel>, n: u32) {
    let Some(label) = tray_label(n) else { return };
    let (width, height) = (pixels.width() as f32, pixels.height() as f32);
    let size = width.min(height);
    let radius = size * 0.34;
    draw(
        pixels.make_mut_slice(),
        width as usize,
        (width - radius, radius),
        radius,
        size * 0.04,
        &label,
    );
}

/// Over the taskbar button: the overlay icon, kept up to date with the count and the window.
#[cfg(windows)]
pub mod taskbar {
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::Shell::{
        DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIcon, DestroyIcon, HICON, RegisterWindowMessageW, SM_CXSMICON, WM_DPICHANGED,
        WM_NCDESTROY,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    /// Our id for the subclass on the main window.
    const SUBCLASS: usize = 0xb33b;

    thread_local! {
        /// The main window, once it has a native one.
        static WINDOW: Cell<Option<isize>> = const { Cell::new(None) };
        static COUNT: Cell<u32> = const { Cell::new(0) };
        /// Created once, on the UI thread, and kept.
        static LIST: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
        /// The registered "TaskbarButtonCreated" message.
        static BUTTON_CREATED: Cell<u32> = const { Cell::new(0) };
    }

    /// The count changed.
    pub fn set_count(n: u32) {
        if COUNT.replace(n) != n {
            apply();
        }
    }

    /// The window was shown: follows its native window, which Slint creates only once the
    /// event loop runs, so it is looked for again for a few seconds until it is there.
    pub fn attach<C: ComponentHandle + 'static>(component: &C) {
        find_window(component.as_weak(), 50);
    }

    fn find_window<C: ComponentHandle + 'static>(weak: slint::Weak<C>, tries: u32) {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let handle = component.window().window_handle();
        match handle.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(win32)) => follow(win32.hwnd.get()),
            Ok(_) => tracing::warn!("not a Win32 window: no unread count on the taskbar"),
            Err(error) if tries == 0 => {
                tracing::warn!(%error, "no native window: no unread count on the taskbar");
            }
            Err(_) => slint::Timer::single_shot(Duration::from_millis(100), move || {
                find_window(weak, tries - 1);
            }),
        }
    }

    fn follow(hwnd: isize) {
        if WINDOW.get() == Some(hwnd) {
            // Shown again: its taskbar button may be new.
            apply();
            return;
        }
        // SAFETY: our own window, on its UI thread; the procedure touches only this module's
        // thread-locals and leaves Slint's work to its event loop.
        let subclassed = unsafe { SetWindowSubclass(HWND(hwnd as _), Some(subclass), SUBCLASS, 0) };
        if !subclassed.as_bool() {
            tracing::warn!("can't follow the taskbar button; its unread count may go stale");
        }
        WINDOW.set(Some(hwnd));
        apply();
    }

    /// "TaskbarButtonCreated": the button exists now, first or again after Explorer restarted,
    /// which drops overlay icons.
    fn button_created() -> u32 {
        if BUTTON_CREATED.get() == 0 {
            // SAFETY: a constant, NUL-terminated name.
            BUTTON_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) });
        }
        BUTTON_CREATED.get()
    }

    unsafe extern "system" fn subclass(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if message == WM_DPICHANGED || message == button_created() {
            // After the message is handled: the window has its new DPI then, and the call to
            // Explorer may wait on messages, which shouldn't run inside this one.
            if let Err(error) = slint::invoke_from_event_loop(apply) {
                tracing::debug!(%error, "taskbar badge not redrawn");
            }
        } else if message == WM_NCDESTROY {
            if WINDOW.get() == Some(hwnd.0 as isize) {
                WINDOW.set(None);
            }
            // SAFETY: removing our own subclass from the window being destroyed.
            if !unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS) }.as_bool() {
                tracing::debug!("taskbar subclass not removed");
            }
        }
        // SAFETY: passing the message on, as every subclass procedure must.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    fn list() -> windows::core::Result<ITaskbarList3> {
        if let Some(list) = LIST.with_borrow(Clone::clone) {
            return Ok(list);
        }
        // SAFETY: COM on the UI thread, which winit has made single-threaded already; then
        // this only says so again (or that it is), which is not an error for creating one.
        let list: ITaskbarList3 = unsafe {
            if CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_err() {
                tracing::debug!("COM is set up differently on the UI thread");
            }
            let list: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
            list.HrInit()?;
            list
        };
        LIST.set(Some(list.clone()));
        Ok(list)
    }

    /// Shows the current count over the taskbar button, or nothing for none.
    fn apply() {
        let Some(hwnd) = WINDOW.get() else { return };
        if let Err(error) = show(HWND(hwnd as _), COUNT.get()) {
            // Before the button exists this fails; "TaskbarButtonCreated" comes then.
            tracing::debug!(%error, "taskbar badge not shown");
        }
    }

    fn show(hwnd: HWND, n: u32) -> windows::core::Result<()> {
        let list = list()?;
        // SAFETY: a query on our own window.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)) }.max(16);
        let Some(pixels) = super::overlay(size as u32, n) else {
            // SAFETY: our own window; no icon takes the badge away.
            return unsafe { list.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null()) };
        };
        let icon = icon(&pixels)?;
        let description = HSTRING::from(super::words(n));
        // SAFETY: our own window and icon, and a description that outlives the call; the
        // taskbar copies the icon, so it goes right after.
        unsafe {
            let shown = list.SetOverlayIcon(hwnd, icon, PCWSTR(description.as_ptr()));
            DestroyIcon(icon)?;
            shown
        }
    }

    /// An icon of straight-alpha pixels, as winit makes its window icons.
    fn icon(pixels: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) -> windows::core::Result<HICON> {
        let mut bgra = Vec::with_capacity(pixels.as_slice().len() * 4);
        let mut mask = Vec::with_capacity(pixels.as_slice().len());
        for p in pixels.as_slice() {
            bgra.extend([p.b, p.g, p.r, p.a]);
            mask.push(p.a.wrapping_sub(u8::MAX));
        }
        // SAFETY: both buffers hold width * height pixels at the depths given.
        unsafe {
            CreateIcon(
                None,
                pixels.width() as i32,
                pixels.height() as i32,
                1,
                32,
                mask.as_ptr(),
                bgra.as_ptr(),
            )
        }
    }
}

/// Elsewhere the tray's badge is all there is.
#[cfg(not(windows))]
pub mod taskbar {
    pub fn set_count(_: u32) {}
    pub fn attach<C: slint::ComponentHandle + 'static>(_: &C) {}
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn labels_stop_at_two_digits_on_the_taskbar_and_one_in_the_tray() {
        let taskbar: Vec<_> = [0, 1, 9, 10, 99, 100, 5000]
            .map(taskbar_label)
            .into_iter()
            .collect();
        assert_eq!(
            taskbar,
            [
                None,
                Some("1".into()),
                Some("9".into()),
                Some("10".into()),
                Some("99".into()),
                Some("99+".into()),
                Some("99+".into())
            ]
        );
        let tray: Vec<_> = [0, 1, 9, 10, 150].map(tray_label).into_iter().collect();
        assert_eq!(
            tray,
            [
                None,
                Some("1".into()),
                Some("9".into()),
                Some("9+".into()),
                Some("9+".into())
            ]
        );
    }

    #[test]
    fn words_count_messages() {
        assert_eq!(words(1), "1 unread message");
        assert_eq!(words(3), "3 unread messages");
        assert_eq!(words(120), "120 unread messages");
    }

    #[test]
    fn text_shrinks_to_fit_the_circle() {
        let one = text_box("1", 8.0, 8.0, 8.0);
        let two = text_box("99", 8.0, 8.0, 8.0);
        let three = text_box("99+", 8.0, 8.0, 8.0);
        let ink = |t: TextBox| t.height + t.stroke;
        assert!(ink(one) >= ink(two) && ink(two) > ink(three));
        for (label, text) in [("1", one), ("99", two), ("99+", three)] {
            let advance: f32 =
                label.chars().map(|c| glyph(c).0).sum::<f32>() + GAP * (label.len() - 1) as f32;
            let ink_width = advance * text.height + text.stroke;
            assert!(ink_width <= MAX_WIDTH * 8.0 + 1.0, "{label}: {ink_width}");
            // Centred, to the pixel the snapping allows.
            let left = text.x - text.stroke / 2.0;
            assert!((left + ink_width / 2.0 - 8.0).abs() <= 0.5, "{label}");
            // Ink edges on whole pixels.
            assert_eq!(left.fract(), 0.0);
            assert_eq!((text.y - text.stroke / 2.0).fract(), 0.0);
            assert!(text.stroke >= 1.0);
        }
    }

    #[test]
    fn every_label_character_has_a_glyph() {
        for c in "0123456789+".chars() {
            let (advance, strokes) = glyph(c);
            assert!(advance > 0.0 && !strokes.is_empty(), "{c}");
            for point in strokes.iter().flatten() {
                assert!((-0.01..=0.57).contains(&point.0), "{c}: {point:?}");
                assert!((-0.01..=1.01).contains(&point.1), "{c}: {point:?}");
            }
        }
    }

    fn rgba(p: Rgba8Pixel) -> (u8, u8, u8, u8) {
        (p.r, p.g, p.b, p.a)
    }

    #[test]
    fn overlay_is_a_red_circle_with_white_text() {
        assert!(overlay(16, 0).is_none());
        for size in [16, 20, 24, 32] {
            for n in [1, 7, 42, 99, 100] {
                let pixels = overlay(size, n).expect("a badge");
                let at = |x: u32, y: u32| pixels.as_slice()[(y * size + x) as usize];
                // The corners are outside the circle.
                assert_eq!(at(0, 0).a, 0);
                assert_eq!(at(size - 1, size - 1).a, 0);
                // Red inside the edge, left of the text.
                assert_eq!(rgba(at(1, size / 2)), (0xe5, 0x38, 0x3b, 255), "{size} {n}");
                // Some of the text is solid white.
                assert!(
                    pixels
                        .as_slice()
                        .iter()
                        .any(|&p| rgba(p) == (255, 255, 255, 255)),
                    "{size} {n}"
                );
                save(&format!("overlay-{size}-{n}"), &pixels);
            }
        }
    }

    /// With `B33P_BADGE_PNG=<dir>`, saves the badge there eight times as large, for a look.
    pub fn save(name: &str, pixels: &SharedPixelBuffer<Rgba8Pixel>) {
        let Some(dir) = std::env::var_os("B33P_BADGE_PNG") else {
            return;
        };
        let (width, height) = (pixels.width() as usize, pixels.height() as usize);
        let mut data = Vec::with_capacity(width * height * 64 * 4);
        for y in 0..height * 8 {
            for x in 0..width * 8 {
                let p = pixels.as_slice()[(y / 8) * width + x / 8];
                data.extend([p.r, p.g, p.b, p.a]);
            }
        }
        let file = std::fs::File::create(std::path::Path::new(&dir).join(format!("{name}.png")))
            .expect("the file opens");
        let mut encoder = png::Encoder::new(file, width as u32 * 8, height as u32 * 8);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&data))
            .expect("the picture is written");
    }

    #[test]
    fn a_one_is_a_white_stem_in_the_middle() {
        let pixels = overlay(32, 1).expect("a badge");
        let at = |x: usize, y: usize| rgba(pixels.as_slice()[y * 32 + x]);
        let text = text_box("1", 16.0, 16.0, 16.0);
        let stem = (text.x + 0.32 * text.height) as usize;
        assert_eq!(at(stem, 18), (255, 255, 255, 255));
        // Beside it the circle is red.
        assert_eq!(at(stem + 4, 18), (0xe5, 0x38, 0x3b, 255));
        assert_eq!(at(stem.saturating_sub(5), 24), (0xe5, 0x38, 0x3b, 255));
    }

    #[test]
    fn the_tray_badge_goes_top_right_with_a_white_rim() {
        let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(64, 64);
        add_to_icon(&mut pixels, 0);
        assert!(pixels.as_slice().iter().all(|p| p.a == 0));
        add_to_icon(&mut pixels, 3);
        let at = |x: usize, y: usize| rgba(pixels.as_slice()[y * 64 + x]);
        // The rim, at the circle's left edge, at its middle height.
        let radius = 64.0 * 0.34;
        let (cx, cy) = (64.0 - radius, radius);
        assert_eq!(
            at((cx - radius + 1.0) as usize, cy as usize),
            (255, 255, 255, 255)
        );
        // Red just inside the rim.
        assert_eq!(
            at((cx - radius + 5.0) as usize, cy as usize),
            (0xe5, 0x38, 0x3b, 255)
        );
        // The rest of the icon is untouched.
        assert_eq!(at(0, 63).3, 0);
        assert_eq!(at(10, 50).3, 0);
    }
}
