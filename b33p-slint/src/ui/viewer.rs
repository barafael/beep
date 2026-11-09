//! The picture viewer over the window (`ui/viewer.slint`): what it shows, from
//! [`ChatState::viewer`], and what its keys and buttons do.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Duration;

use b33p_core::{FullPicture, resample};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use super::*;

/// How far one notch of the wheel scrolls, as the backend reports it: winit 60 logical pixels
/// a line.
const NOTCH: f32 = 60.0;

/// How long the zoom stays put before what shows is resampled for it: longer than the gaps
/// within a burst of wheel notches (65 ms at most, as Othereum/sigwin measured), so a spin
/// resamples once, at its end. Meanwhile the window draws the picture scaled as it is.
const SETTLE: Duration = Duration::from_millis(100);

/// Resampling the viewer's picture for the zoom it shows at; see `Viewer.sharp`.
#[derive(Default)]
struct Sharpening {
    /// The full picture shown.
    full: RefCell<Option<FullPicture>>,
    timer: slint::Timer,
    /// Counts what was asked for: a resampling done after another was asked for is dropped.
    asked: Cell<u64>,
}

thread_local! {
    static SHARPENING: Sharpening = Sharpening::default();
}

/// Resamples the full picture for `zoom` once the zoom has settled, if it shows smaller than
/// its own size; on a thread, so the window goes on meanwhile.
fn sharpen_later(window: slint::Weak<AppWindow>, zoom: f32) {
    SHARPENING.with(|sharpening| {
        let asked = sharpening.asked.get().wrapping_add(1);
        sharpening.asked.set(asked);
        sharpening
            .timer
            .start(slint::TimerMode::SingleShot, SETTLE, move || {
                sharpen(window.clone(), zoom, asked);
            });
    });
}

fn sharpen(window: slint::Weak<AppWindow>, zoom: f32, asked: u64) {
    let Some(FullPicture {
        pixels: full,
        width,
        height,
    }) = SHARPENING.with(|s| s.full.borrow().clone())
    else {
        return;
    };
    let (width, height) = resample::scaled((width, height), f64::from(zoom));
    if width >= full.width || height >= full.height {
        // Drawn at its size or larger: as it is.
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("b33p-viewer".into())
        .spawn(move || {
            let Some(sharp) = resample::shrink(&full, width, height) else {
                return;
            };
            drop(full);
            let pixels =
                SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&sharp.rgba, width, height);
            drop(sharp);
            let shown = window.upgrade_in_event_loop(move |window| {
                if SHARPENING.with(|s| s.asked.get()) != asked {
                    return;
                }
                let viewer = window.global::<Viewer>();
                viewer.set_sharp(slint::Image::from_rgba8(pixels));
                viewer.set_sharp_zoom(zoom);
            });
            if let Err(error) = shown {
                tracing::debug!(%error, "the window went before the picture was resampled");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(error = %b33p_core::scrub::chain(&error), "can't resample the picture shown");
    }
}

/// Shows `full` (or nothing) as the viewer's full picture to resample.
fn set_full(window: &AppWindow, full: Option<&FullPicture>) {
    let same = SHARPENING.with(|s| match (&*s.full.borrow(), full) {
        (Some(kept), Some(new)) => Arc::ptr_eq(&kept.pixels, &new.pixels),
        (None, None) => true,
        _ => false,
    });
    if same {
        return;
    }
    SHARPENING.with(|s| {
        s.full.replace(full.cloned());
        s.asked.set(s.asked.get().wrapping_add(1));
        s.timer.stop();
    });
    let viewer = window.global::<Viewer>();
    viewer.set_sharp(slint::Image::default());
    viewer.set_sharp_zoom(0.0);
    if full.is_some() {
        sharpen_later(window.as_weak(), viewer.get_shown_zoom());
    }
}

/// Brings the viewer in line with `chat`.
pub(super) fn render_viewer(window: &AppWindow, chat: &ChatState, images: &Images) {
    let viewer = window.global::<Viewer>();
    let Some(view) = chat.viewer() else {
        if viewer.get_open() {
            viewer.set_open(false);
            viewer.set_key(SharedString::new());
            viewer.set_picture(slint::Image::default());
        }
        set_full(window, None);
        images.forget_viewed();
        return;
    };
    let attachment = view.attachment;
    let key = attachment.id.0.as_str();
    if viewer.get_key() != key {
        // Another picture starts fitted to the window.
        viewer.set_zoom(0.0);
        viewer.set_key(key.into());
    }
    // The size it really has: known once it is there, reported by the sender until then.
    let reported = (attachment.width > 0 && attachment.height > 0)
        .then_some((attachment.width, attachment.height));
    let stand_in = || match view.thumbnail {
        Some(thumbnail) => images
            .attachments
            .get(&attachment.id, thumbnail, crate::picture_image),
        None => attachment
            .blurhash
            .as_deref()
            .zip(reported)
            .map(|(hash, (w, h))| images.blur(hash, w, h))
            .unwrap_or_default(),
    };
    let thumbnail_size = view.thumbnail.map(|t| (t.width, t.height));
    let (picture, (width, height), error) = match view.full {
        Some(Ok(full)) => (images.viewed(&full.pixels), (full.width, full.height), None),
        Some(Err(error)) => (
            stand_in(),
            reported.or(thumbnail_size).unwrap_or((1, 1)),
            Some(error.to_string()),
        ),
        None => (
            stand_in(),
            reported.or(thumbnail_size).unwrap_or((1, 1)),
            None,
        ),
    };
    match view.full {
        Some(Ok(full)) => set_full(window, Some(full)),
        _ => {
            set_full(window, None);
            images.forget_viewed();
        }
    }
    viewer.set_picture(picture);
    viewer.set_picture_width(width.try_into().unwrap_or(i32::MAX));
    viewer.set_picture_height(height.try_into().unwrap_or(i32::MAX));
    viewer.set_loading(view.full.is_none());
    viewer.set_error(error.unwrap_or_default().into());
    viewer.set_sender(view.message.sender.as_str().into());
    viewer.set_time(state::format_time(view.message.timestamp).into());
    viewer.set_index(view.index.try_into().unwrap_or(i32::MAX));
    viewer.set_count(view.count.try_into().unwrap_or(i32::MAX));
    viewer.set_status(save_status(chat.saved(&attachment.id)).into());
    viewer.set_open(true);
}

/// How saving the picture went, in the words the chat uses under it.
fn save_status(save: Option<&SaveState>) -> String {
    match save {
        None => String::new(),
        Some(SaveState::Saving) => "Saving…".into(),
        Some(SaveState::Saved(path)) => format!(
            "Saved to {}",
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("disk")
        ),
        Some(SaveState::Failed(error)) => format!("Couldn't save: {error}"),
    }
}

/// A click on a picture in the chat (or the media overview): photos open in the viewer,
/// anything else in the desktop's app.
pub(crate) fn open_picture(chat: &mut ChatState, id: &str) -> Option<b33p_core::Command> {
    chat.open_picture(&AttachmentId(id.into()))
}

/// Closes the viewer; the focus goes back to where it goes after Esc (the composer, if the
/// chat can be written to).
pub(super) fn close_viewer(shared: &Shared, window: &AppWindow) {
    shared.edit(ChatState::close_viewer);
    window.set_escaped(window.get_escaped().wrapping_add(1));
}

/// What the viewer's zoom does that needs no chat: the wheel, catching at 100%, and the
/// double click. `clock` tells the time, for the wheel's hold at 100%.
pub(crate) fn wire_zoom(window: &AppWindow, clock: impl Fn() -> Duration + 'static) {
    let viewer = window.global::<Viewer>();
    viewer.set_notch(NOTCH);
    let wheel = RefCell::new(state::Wheel::default());
    viewer.on_wheel_zoom(move |zoom, notches, fit| {
        wheel.borrow_mut().turn(zoom, notches, fit, clock())
    });
    viewer.on_toggle_zoom(state::toggle_zoom);
    viewer.on_zoomed({
        let window = window.as_weak();
        move |zoom| sharpen_later(window.clone(), zoom)
    });
}

pub(super) fn wire_viewer(window: &AppWindow, shared: &Rc<Shared>) {
    let started = std::time::Instant::now();
    wire_zoom(window, move || started.elapsed());
    let viewer = window.global::<Viewer>();
    viewer.on_step({
        let shared = shared.clone();
        move |by| shared.command(|chat| chat.step_viewer(by))
    });
    viewer.on_close({
        let shared = shared.clone();
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                close_viewer(&shared, &window);
            }
        }
    });
    viewer.on_save({
        let shared = shared.clone();
        move || {
            shared.act(|chat| {
                let id = chat.viewer().map(|view| view.attachment.id.clone());
                (
                    id.and_then(|id| chat.save_attachment(&id)),
                    Update {
                        rows: true,
                        ..Update::default()
                    },
                )
            });
        }
    });
    viewer.on_open_outside({
        let shared = shared.clone();
        move || {
            shared.command(|chat| {
                let id = chat.viewer().map(|view| view.attachment.id.clone())?;
                chat.open_attachment(&id)
            });
        }
    });
}
