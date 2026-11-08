//! The picture viewer: a picture of the open chat over the whole window, at full size, and
//! going from it to the chat's other pictures. Ported in idea from Othereum/sigwin's image
//! viewer (88ea8a5), which opens a window of its own; here it is part of the main window.

use super::*;

/// The picture shown in the viewer, while it is open.
pub(super) struct Viewing {
    conversation: ConversationId,
    attachment: AttachmentId,
    /// Opened from the media overview, whose pictures reach further back than the loaded
    /// history: going on goes through those.
    from_media: bool,
    /// The picture at full size: `None` while it downloads.
    full: Option<Result<FullPicture, crate::Error>>,
}

/// What the viewer shows; see [`ChatState::viewer`].
#[derive(Debug)]
pub struct ViewerView<'a> {
    pub message: &'a ChatMessage,
    pub attachment: &'a Attachment,
    /// Its place among the chat's pictures, oldest first, and how many there are.
    pub index: usize,
    pub count: usize,
    /// At full size, once downloaded: `None` while it downloads.
    pub full: Option<Result<&'a FullPicture, &'a crate::Error>>,
    /// The picture as the chat shows it, if it is there: shown until the full one is.
    pub thumbnail: Option<&'a Arc<Picture>>,
}

impl ChatState {
    /// A click on a picture in the open chat: a photo opens in the viewer, anything else (a
    /// sticker, a video, a file, a story's picture) in the desktop's app for it.
    pub fn open_picture(&mut self, id: &AttachmentId) -> Option<Command> {
        if self.viewable(id).is_some() {
            self.open_viewer(id)
        } else {
            self.open_attachment(id)
        }
    }

    /// Opens a photo of the open chat in the viewer, and asks for it at full size; `None` if
    /// it isn't one of the chat's photos (and the viewer stays as it was).
    pub fn open_viewer(&mut self, id: &AttachmentId) -> Option<Command> {
        let from_media = self.viewable(id)?;
        let conversation = self.selected.clone()?;
        self.viewing = Some(Viewing {
            conversation,
            attachment: id.clone(),
            from_media,
            full: None,
        });
        self.fetch_viewed()
    }

    /// Goes `by` pictures on (back, if negative) among the chat's, stopping at the first and
    /// the last; asks for the new one at full size.
    pub fn step_viewer(&mut self, by: i32) -> Option<Command> {
        let viewing = self.viewing.as_ref()?;
        let pictures = self.viewer_pictures(viewing.from_media);
        let at = pictures
            .iter()
            .position(|(_, a)| a.id == viewing.attachment)?;
        let to = at
            .saturating_add_signed(by as isize)
            .min(pictures.len() - 1);
        if to == at {
            return None;
        }
        let id = pictures[to].1.id.clone();
        let viewing = self.viewing.as_mut()?;
        viewing.attachment = id;
        // Only the picture shown is kept at full size: the one before goes.
        viewing.full = None;
        self.fetch_viewed()
    }

    pub fn close_viewer(&mut self) {
        self.viewing = None;
    }

    /// What the viewer shows, while it is open: closed by itself when its chat closes or its
    /// picture goes (deleted, disappeared).
    pub fn viewer(&self) -> Option<ViewerView<'_>> {
        let viewing = self.viewing.as_ref()?;
        if self.selected.as_ref() != Some(&viewing.conversation) {
            return None;
        }
        let pictures = self.viewer_pictures(viewing.from_media);
        let index = pictures
            .iter()
            .position(|(_, a)| a.id == viewing.attachment)?;
        let (message, attachment) = pictures[index];
        let thumbnail = match self.attachments.get(&attachment.id) {
            Some(AttachmentState::Ready(AttachmentContent::Picture(picture))) => Some(picture),
            _ => None,
        };
        Some(ViewerView {
            message,
            attachment,
            index,
            count: pictures.len(),
            full: viewing.full.as_ref().map(Result::as_ref),
            thumbnail,
        })
    }

    /// [`Event::FullPicture`]: kept if it is still the one shown. Whether it was.
    pub(super) fn full_picture_arrived(
        &mut self,
        id: &AttachmentId,
        picture: Result<FullPicture, crate::Error>,
    ) -> bool {
        match &mut self.viewing {
            Some(viewing) if &viewing.attachment == id => {
                viewing.full = Some(picture);
                true
            }
            _ => false,
        }
    }

    fn fetch_viewed(&self) -> Option<Command> {
        let viewing = self.viewing.as_ref()?;
        let attachment = self.find_attachment(&viewing.attachment)?;
        Some(Command::FetchFullPicture(attachment.clone()))
    }

    /// Whether `id` is one of the open chat's photos, and if so whether it is only in the
    /// media overview's (not in the loaded history).
    fn viewable(&self, id: &AttachmentId) -> Option<bool> {
        let shown = |from_media| {
            self.viewer_pictures(from_media)
                .iter()
                .any(|(_, a)| &a.id == id)
        };
        if shown(false) {
            Some(false)
        } else if shown(true) {
            Some(true)
        } else {
            None
        }
    }

    /// The open chat's photos, oldest first, with their messages: from the loaded history, or
    /// from the media overview's. Not stickers, videos, link previews' pictures or view-once
    /// media, nor what was deleted.
    fn viewer_pictures(&self, from_media: bool) -> Vec<(&ChatMessage, &Attachment)> {
        let messages: &[ChatMessage] = if from_media {
            match &self.media {
                Some((of, Some(gathered))) if Some(of) == self.selected.as_ref() => {
                    &gathered.messages
                }
                _ => &[],
            }
        } else {
            self.selected_history()
        };
        let mut pictures: Vec<_> = messages
            .iter()
            .filter(|m| !m.deleted)
            .flat_map(|m| {
                m.attachments
                    .iter()
                    .filter(|a| a.kind == AttachmentKind::Image)
                    .map(move |a| (m, a))
            })
            .collect();
        // Stable: an album's pictures stay in their order.
        pictures.sort_by_key(|(m, _)| m.timestamp);
        pictures
    }
}

/// How much one notch of the mouse wheel zooms the viewer.
pub const ZOOM_STEP: f32 = 1.1;
/// The viewer zooms in at most this far (800%), or to the fitted size if that is larger.
pub const ZOOM_MAX: f32 = 8.0;
/// How close to 100% the wheel may end up before it goes all the way there.
const NEAR_ACTUAL: f32 = 0.05;

/// The viewer's zoom after `notches` of the mouse wheel (positive zooms in): 1.0 shows the
/// picture's pixels one to one. Steps rarely land on 100%, the sharpest view, so one that
/// would pass it, or end up near it, stops there: it catches. Only on the way to it, or the
/// small steps of a touchpad could never leave. Never smaller than `fit` (what fits the
/// window) or 100%, whichever is smaller, nor larger than [`ZOOM_MAX`] or `fit`. Ported in idea
/// from Othereum/sigwin's `wheel_scale` (88ea8a5).
pub fn wheel_zoom(zoom: f32, notches: f32, fit: f32) -> f32 {
    let next = zoom * ZOOM_STEP.powf(notches);
    let passes = (zoom < 1.0 && next > 1.0) || (zoom > 1.0 && next < 1.0);
    let (from, to) = ((zoom - 1.0).abs(), (next - 1.0).abs());
    let next = if passes || (to < from && to <= NEAR_ACTUAL) {
        1.0
    } else {
        next
    };
    let low = fit.min(1.0);
    let high = ZOOM_MAX.max(fit);
    next.clamp(low, high)
}

/// The wheel does nothing for this long after landing on 100%, so a fast spin still catches
/// there: the notches of one burst come at most 65 ms apart (as Othereum/sigwin measured).
pub const HOLD_AT_ACTUAL: std::time::Duration = std::time::Duration::from_millis(120);

/// The viewer's mouse wheel: [`wheel_zoom`], and a moment's hold once it lands on 100%.
#[derive(Debug, Default)]
pub struct Wheel {
    held_until: Option<std::time::Duration>,
}

impl Wheel {
    /// The zoom after `notches` at `now`, on any clock that doesn't go back.
    pub fn turn(&mut self, zoom: f32, notches: f32, fit: f32, now: std::time::Duration) -> f32 {
        if self.held_until.is_some_and(|until| now < until) {
            return zoom;
        }
        let zoomed = wheel_zoom(zoom, notches, fit);
        if zoomed == 1.0 && zoom != 1.0 {
            self.held_until = Some(now + HOLD_AT_ACTUAL);
        }
        zoomed
    }
}

/// What a double click does to the viewer's zoom: from anything but 100% to 100%, and from
/// there back to fitting the window.
pub fn toggle_zoom(zoom: f32, fit: f32) -> f32 {
    if (zoom - 1.0).abs() < 1e-3 { fit } else { 1.0 }
}
