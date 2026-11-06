//! Pictures from the backend as the images GPUI draws, converted once, so that drawing a list
//! again neither copies pixels nor makes an unchanged row look changed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use b33p_core::state::{AttachmentState, ChatState};
use b33p_core::{AttachmentContent, AttachmentId, Picture};
use gpui_kit::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;

/// What the window draws a picture from.
pub type Drawn = Arc<RenderImage>;

#[derive(Default)]
pub struct Images {
    attachments: RefCell<HashMap<AttachmentId, (Arc<Picture>, Drawn)>>,
    people: RefCell<HashMap<String, (Arc<Picture>, Drawn)>>,
    /// Decoded BlurHashes, by hash and size.
    blurs: RefCell<HashMap<(String, u32, u32), Option<Drawn>>>,
    /// The "Profile pictures" setting.
    pub avatars_enabled: Cell<bool>,
    /// [`ChatState::downloads_forgotten`] when `attachments` last let go of what it forgot.
    forgotten: Cell<u64>,
}

/// A picture's pixels (RGBA) as the BGRA GPUI draws.
pub fn draw(picture: &Picture) -> Option<Drawn> {
    let mut pixels = picture.rgba.to_vec();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = RgbaImage::from_raw(picture.width, picture.height, pixels)?;
    Some(Arc::new(RenderImage::new(smallvec![Frame::new(image)])))
}

impl Images {
    /// The picture of an avatar (a chat's or a person's, by `key`), or `None` when pictures are
    /// turned off or there is none.
    pub fn avatar(&self, key: &str, picture: Option<&Arc<Picture>>) -> Option<Drawn> {
        if !self.avatars_enabled.get() {
            return None;
        }
        let picture = picture?;
        let mut people = self.people.borrow_mut();
        if let Some((kept, drawn)) = people.get(key)
            && Arc::ptr_eq(kept, picture)
        {
            return Some(drawn.clone());
        }
        let drawn = draw(picture)?;
        people.insert(key.to_owned(), (picture.clone(), drawn.clone()));
        Some(drawn)
    }

    /// A downloaded picture, drawn.
    pub fn attachment(&self, id: &AttachmentId, picture: &Arc<Picture>) -> Option<Drawn> {
        let mut attachments = self.attachments.borrow_mut();
        if let Some((kept, drawn)) = attachments.get(id)
            && Arc::ptr_eq(kept, picture)
        {
            return Some(drawn.clone());
        }
        let drawn = draw(picture)?;
        attachments.insert(id.clone(), (picture.clone(), drawn.clone()));
        Some(drawn)
    }

    /// The downloaded picture of attachment `id`, if there is one.
    pub fn downloaded(&self, chat: &ChatState, id: &AttachmentId) -> Option<Drawn> {
        match chat.attachment(id) {
            Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => {
                self.attachment(id, picture)
            }
            _ => None,
        }
    }

    /// A small blurred picture for a BlurHash, with the shape of `width` × `height`; `None` if
    /// the hash is invalid.
    pub fn blur(&self, hash: &str, width: u32, height: u32) -> Option<Drawn> {
        // Blurred anyway: a few dozen pixels are plenty, scaled up by the UI. The sizes are
        // the sender's, so they are taken wide enough not to overflow.
        let (width, height) = (u64::from(width), u64::from(height));
        let (w, h) = if width >= height {
            (32, (32 * height / width.max(1)).clamp(4, 32) as u32)
        } else {
            ((32 * width / height.max(1)).clamp(4, 32) as u32, 32)
        };
        let key = (hash.to_owned(), w, h);
        if let Some(drawn) = self.blurs.borrow().get(&key) {
            return drawn.clone();
        }
        // A hash is base-83 text, 6 to 166 characters long; nothing else is worth decoding.
        let plausible = hash.is_ascii() && (6..=166).contains(&hash.len());
        let drawn = match plausible.then(|| blurhash::decode(hash, w, h, 1.0)) {
            Some(Ok(rgba)) => draw(&Picture {
                width: w,
                height: h,
                rgba,
            }),
            _ => None,
        };
        self.blurs.borrow_mut().insert(key, drawn.clone());
        drawn
    }

    /// Lets go of the pictures the state let go of (to keep memory in bounds).
    pub fn forget_gone(&self, chat: &ChatState) {
        if self.forgotten.replace(chat.downloads_forgotten()) == chat.downloads_forgotten() {
            return;
        }
        self.attachments.borrow_mut().retain(|id, (picture, _)| {
            matches!(
                chat.attachment(id),
                Some(AttachmentState::Ready(AttachmentContent::Picture(kept))) if Arc::ptr_eq(kept, picture)
            )
        });
    }
}
