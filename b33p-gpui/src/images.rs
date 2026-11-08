//! Pictures from the backend as the images GPUI draws, converted once, so that drawing a list
//! again neither copies pixels nor makes an unchanged row look changed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use b33p_core::state::{AttachmentState, ChatState};
use b33p_core::{AttachmentContent, AttachmentId, Picture};
use gpui_kit::{App, RenderImage};
use image::{Frame, RgbaImage};
use smallvec::smallvec;

/// What the window draws a picture from.
pub type Drawn = Arc<RenderImage>;

/// A picture as the state has it, with what it was drawn as.
type Kept = (Arc<Picture>, Drawn);

/// The longest side, in pixels, a picture is kept at when it is drawn in a message: a bubble
/// shows one at most 280 points wide, so this is plenty even on a screen of three times.
pub const IN_MESSAGE: u32 = 1280;
/// The same for a tile of the media overview, 110 points wide.
pub const TILE: u32 = 384;
/// The most a picture is ever kept at, for the viewer: what a phone makes, but not the 100
/// megapixels a sender can claim, which would be 400 MB of video memory for one.
pub const FULL: u32 = 4096;

/// How many people's and chats' pictures are kept; those of the ones scrolled past are drawn
/// again when they show again.
const KEPT_PEOPLE: usize = 512;

/// How many blurred stand-ins are kept (a few kilobytes each, and as many on the GPU): one for
/// every picture of the thread that was drawn, however long it is.
const KEPT_BLURS: usize = 256;

#[derive(Default)]
pub struct Images {
    /// By attachment and the longest side it was drawn at, so that the overview's small ones and
    /// the message's are not one another's.
    attachments: RefCell<HashMap<(AttachmentId, u32), Kept>>,
    people: RefCell<HashMap<String, (Arc<Picture>, Drawn)>>,
    /// Decoded BlurHashes, by (a hash of) the hash and the size: looked up for every picture
    /// drawn, so the key is one that costs no allocation.
    blurs: RefCell<HashMap<(u64, u32, u32), Option<Drawn>>>,
    /// The "Profile pictures" setting.
    pub avatars_enabled: Cell<bool>,
    /// [`ChatState::downloads_forgotten`] when `attachments` last let go of what it forgot.
    forgotten: Cell<u64>,
    /// Pictures no longer kept, whose place on the GPU [`Images::release`] frees: GPUI keeps an
    /// image there until it is told to drop it, however many pictures the window drew since.
    discarded: RefCell<Vec<Drawn>>,
}

fn hash_of(text: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// A picture's pixels (RGBA) as the BGRA GPUI draws.
pub fn draw(picture: &Picture) -> Option<Drawn> {
    draw_within(picture, FULL)
}

/// [`draw`], made smaller if its longer side is longer than `side`: what is drawn small is not
/// kept (nor copied, nor sent to the graphics) at the size of a camera's picture.
pub fn draw_within(picture: &Picture, side: u32) -> Option<Drawn> {
    let (mut width, mut height) = (picture.width, picture.height);
    let mut pixels = if width.max(height) > side && width > 0 && height > 0 {
        let source = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
            width,
            height,
            picture.rgba.as_slice(),
        )?;
        let scale = f64::from(side) / f64::from(width.max(height));
        width = ((f64::from(width) * scale).round() as u32).max(1);
        height = ((f64::from(height) * scale).round() as u32).max(1);
        // Averages the pixels each new one stands for, which is what a small picture needs.
        image::imageops::thumbnail(&source, width, height).into_raw()
    } else {
        picture.rgba.to_vec()
    };
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let image = RgbaImage::from_raw(width, height, pixels)?;
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
        let drawn = draw_within(picture, TILE)?;
        // Contacts who left stay otherwise, with their place on the GPU, until b33p ends.
        if people.len() >= KEPT_PEOPLE {
            let mut discarded = self.discarded.borrow_mut();
            discarded.extend(people.drain().map(|(_, (_, drawn))| drawn));
        }
        if let Some((_, replaced)) = people.insert(key.to_owned(), (picture.clone(), drawn.clone()))
        {
            self.discarded.borrow_mut().push(replaced);
        }
        Some(drawn)
    }

    /// A downloaded picture, drawn for a message.
    pub fn attachment(&self, id: &AttachmentId, picture: &Arc<Picture>) -> Option<Drawn> {
        self.scaled(id, picture, IN_MESSAGE)
    }

    /// A downloaded picture, drawn with its longer side at most `side`.
    fn scaled(&self, id: &AttachmentId, picture: &Arc<Picture>, side: u32) -> Option<Drawn> {
        let mut attachments = self.attachments.borrow_mut();
        let key = (id.clone(), side);
        if let Some((kept, drawn)) = attachments.get(&key)
            && Arc::ptr_eq(kept, picture)
        {
            return Some(drawn.clone());
        }
        let drawn = draw_within(picture, side)?;
        if let Some((_, replaced)) = attachments.insert(key, (picture.clone(), drawn.clone())) {
            self.discarded.borrow_mut().push(replaced);
        }
        Some(drawn)
    }

    /// The downloaded picture of attachment `id`, if there is one, as large as it is kept.
    pub fn downloaded(&self, chat: &ChatState, id: &AttachmentId) -> Option<Drawn> {
        self.downloaded_within(chat, id, FULL)
    }

    /// The downloaded picture of attachment `id`, if there is one, kept no larger than `side`.
    pub fn downloaded_within(
        &self,
        chat: &ChatState,
        id: &AttachmentId,
        side: u32,
    ) -> Option<Drawn> {
        match chat.attachment(id) {
            Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => {
                self.scaled(id, picture, side)
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
        let key = (hash_of(hash), w, h);
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
        let mut blurs = self.blurs.borrow_mut();
        // Those of the messages scrolled past are decoded again when they show again.
        if blurs.len() >= KEPT_BLURS {
            let mut discarded = self.discarded.borrow_mut();
            discarded.extend(blurs.drain().filter_map(|(_, blur)| blur));
        }
        blurs.insert(key, drawn.clone());
        drawn
    }

    /// Lets go of the pictures the state let go of (to keep memory in bounds).
    pub fn forget_gone(&self, chat: &ChatState) {
        if self.forgotten.replace(chat.downloads_forgotten()) == chat.downloads_forgotten() {
            return;
        }
        let mut discarded = self.discarded.borrow_mut();
        self.attachments.borrow_mut().retain(|(id, _), (picture, drawn)| {
            let stays = matches!(
                chat.attachment(id),
                Some(AttachmentState::Ready(AttachmentContent::Picture(kept))) if Arc::ptr_eq(kept, picture)
            );
            if !stays {
                discarded.push(drawn.clone());
            }
            stays
        });
    }

    /// Frees the video memory of the pictures let go of. Only those: nothing says which of the
    /// others are on screen, and a picture freed there is drawn from nothing.
    pub fn release(&self, cx: &mut App) {
        for drawn in self.discarded.take() {
            cx.drop_image(drawn, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_picture_is_kept_smaller_and_a_small_one_as_it_is() {
        let big = Picture {
            width: 4000,
            height: 2000,
            rgba: vec![0x80; 4000 * 2000 * 4],
        };
        let drawn = draw_within(&big, 1000).expect("drawn");
        let size = drawn.size(0);
        assert_eq!((size.width.0, size.height.0), (1000, 500));
        let small = Picture {
            width: 300,
            height: 200,
            rgba: vec![0x80; 300 * 200 * 4],
        };
        let drawn = draw_within(&small, 1000).expect("drawn");
        let size = drawn.size(0);
        assert_eq!((size.width.0, size.height.0), (300, 200));
    }

    fn picture(shade: u8) -> Arc<Picture> {
        Arc::new(Picture {
            width: 1,
            height: 1,
            rgba: vec![shade; 4],
        })
    }

    #[test]
    fn a_picture_is_drawn_once_and_a_replaced_one_is_left_for_release() {
        let images = Images::default();
        let id = AttachmentId("a".into());
        let first = picture(1);
        let drawn = images.attachment(&id, &first).expect("drawn");
        let again = images.attachment(&id, &first).expect("drawn");
        assert!(
            Arc::ptr_eq(&drawn, &again),
            "the same picture is not drawn anew"
        );
        assert!(images.discarded.borrow().is_empty());

        let second = picture(2);
        let replacement = images.attachment(&id, &second).expect("drawn");
        assert!(!Arc::ptr_eq(&drawn, &replacement));
        let discarded = images.discarded.borrow();
        assert_eq!(discarded.len(), 1);
        assert!(Arc::ptr_eq(&discarded[0], &drawn));
    }

    #[test]
    fn avatars_follow_the_same_rule() {
        let images = Images::default();
        images.avatars_enabled.set(true);
        let first = picture(1);
        let drawn = images.avatar("chat:x", Some(&first)).expect("drawn");
        let second = picture(2);
        images.avatar("chat:x", Some(&second)).expect("drawn");
        assert!(Arc::ptr_eq(&images.discarded.borrow()[0], &drawn));
    }

    #[test]
    fn blurred_stand_ins_are_kept_in_bounds() {
        let images = Images::default();
        for i in 0..KEPT_BLURS * 3 {
            // Not valid hashes, which is kept too (a hash that doesn't decode is not tried again).
            images.blur(&format!("{i:08}"), 100, 100);
            assert!(images.blurs.borrow().len() <= KEPT_BLURS);
        }
    }
}
