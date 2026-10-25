//! Attachments: downloading, keeping them in bounds, saving and opening.

use super::*;

impl ChatState {
    /// Opens an attachment of the selected conversation in the desktop's default app.
    pub fn open_attachment(&self, id: &AttachmentId) -> Option<Command> {
        let attachment = self
            .find_attachment(id)
            .filter(|a| a.kind != AttachmentKind::ViewOnce)?;
        Some(Command::OpenAttachment(attachment.clone()))
    }

    /// The download of an attachment for display, if it was asked for.
    pub fn attachment(&self, id: &AttachmentId) -> Option<&AttachmentState> {
        self.attachments.get(id)
    }

    /// Changes whenever downloads are let go of to keep memory in bounds: a cache of what
    /// [`ChatState::attachment`] gave can then let go of them too.
    pub fn downloads_forgotten(&self) -> u64 {
        self.forgotten
    }

    /// Keeps the downloaded pictures and audio within [`KEPT_DOWNLOADS`]: those that came first
    /// go first, except what shows (the open chat's, the media overview's, the story's). What
    /// goes is downloaded again when it shows again.
    pub(super) fn keep_downloads_in_bounds(&mut self) {
        #[cfg(test)]
        let kept = self.kept_downloads.unwrap_or(KEPT_DOWNLOADS);
        #[cfg(not(test))]
        let kept = KEPT_DOWNLOADS;
        if self.downloaded_bytes <= kept {
            return;
        }
        let mut shown: HashSet<AttachmentId> = HashSet::new();
        let mut add = |message: &ChatMessage| {
            shown.extend(message.attachments.iter().map(|a| a.id.clone()));
            if let Some(image) = message.preview.as_ref().and_then(|p| p.image.as_ref()) {
                shown.insert(image.id.clone());
            }
        };
        self.selected_history().iter().for_each(&mut add);
        if let Some((_, Some(gathered))) = &self.media {
            gathered.messages.iter().for_each(&mut add);
        }
        if let Some((story, _, _)) = self.current_story()
            && let crate::StoryContent::Picture(picture) = &story.content
        {
            shown.insert(picture.id.clone());
        }
        let mut staying = Vec::new();
        while self.downloaded_bytes > kept
            && let Some(id) = self.downloaded.pop_front()
        {
            if shown.contains(&id) {
                staying.push(id);
                continue;
            }
            match self.attachments.remove(&id) {
                Some(AttachmentState::Ready(content)) => {
                    self.downloaded_bytes -= download_size(&content);
                    self.forgotten += 1;
                }
                // Gone already, or on its way again: not counted.
                Some(other) => {
                    self.attachments.insert(id, other);
                }
                None => {}
            }
        }
        for id in staying.into_iter().rev() {
            self.downloaded.push_front(id);
        }
    }

    /// Saving an attachment, if it was asked for.
    pub fn saved(&self, id: &AttachmentId) -> Option<&SaveState> {
        self.saves.get(id)
    }

    /// The open chat's messages with this attachment (or link preview picture): the rows to
    /// redraw for it.
    pub(super) fn attachment_rows(&self, id: &AttachmentId) -> Vec<MessageRef> {
        self.selected_history()
            .iter()
            .filter(|m| {
                m.attachments.iter().any(|a| &a.id == id)
                    || m.preview
                        .as_ref()
                        .and_then(|p| p.image.as_ref())
                        .is_some_and(|a| &a.id == id)
            })
            .map(ChatMessage::reference)
            .collect()
    }

    /// Asks for an inline attachment of a message being shown, unless it is already there or
    /// on its way. After a failure that may pass, asking again retries. Not in a message
    /// request, as in Signal's apps.
    pub fn fetch_attachment(&mut self, id: &AttachmentId) -> Option<Command> {
        match self.attachments.get(id) {
            Some(AttachmentState::Loading | AttachmentState::Ready(_)) => return None,
            // Gone from the server, or not what it should be: asking again won't change that.
            Some(AttachmentState::Failed(error)) if !error.retryable() => return None,
            _ => {}
        }
        let story = self.current_story().is_some_and(|(story, _, _)| {
            matches!(&story.content, crate::StoryContent::Picture(picture) if &picture.id == id)
        });
        if !story && self.selected().is_some_and(|chat| self.is_request(chat)) {
            return None;
        }
        let attachment = self
            .find_attachment(id)
            .filter(|a| shown_inline(a.kind))?
            .clone();
        self.attachments
            .insert(id.clone(), AttachmentState::Loading);
        let shown = self.attachment_rows(id);
        self.changed_rows.get_mut().note(shown);
        Some(Command::FetchAttachment(attachment))
    }

    /// Asks for an audio or voice message of the selected conversation, to play it; `None` if
    /// it is there or on its way.
    pub fn fetch_audio(&mut self, id: &AttachmentId) -> Option<Command> {
        if matches!(
            self.attachments.get(id),
            Some(AttachmentState::Loading | AttachmentState::Ready(_))
        ) {
            return None;
        }
        let attachment = self
            .find_attachment(id)
            .filter(|a| matches!(a.kind, AttachmentKind::Audio | AttachmentKind::VoiceNote))?
            .clone();
        self.attachments
            .insert(id.clone(), AttachmentState::Loading);
        let shown = self.attachment_rows(id);
        self.changed_rows.get_mut().note(shown);
        Some(Command::FetchAttachment(attachment))
    }

    /// Saves an attachment of the selected conversation to the downloads folder.
    pub fn save_attachment(&mut self, id: &AttachmentId) -> Option<Command> {
        if matches!(self.saves.get(id), Some(SaveState::Saving)) {
            return None;
        }
        let attachment = self
            .find_attachment(id)
            .filter(|a| a.kind != AttachmentKind::ViewOnce)?
            .clone();
        self.saves.insert(id.clone(), SaveState::Saving);
        let shown = self.attachment_rows(id);
        self.changed_rows.get_mut().note(shown);
        Some(Command::SaveAttachment(attachment))
    }

    /// Only attachments of the selected conversation can be asked for.
    pub(super) fn find_attachment(&self, id: &AttachmentId) -> Option<&Attachment> {
        // A story's picture, while it is shown.
        if let Some((story, _, _)) = self.current_story()
            && let crate::StoryContent::Picture(picture) = &story.content
            && &picture.id == id
        {
            return Some(picture);
        }
        let selected = self.selected.as_ref()?;
        let shown = self.history.get(selected).into_iter().flatten();
        // The media overview reaches further back than the loaded history.
        let gathered = self
            .media
            .iter()
            .filter(|(of, _)| of == selected)
            .flat_map(|(_, media)| media.iter().flat_map(|gathered| &gathered.messages));
        shown
            .chain(gathered)
            .flat_map(|m| {
                let preview = m.preview.iter().filter_map(|p| p.image.as_ref());
                m.attachments.iter().chain(preview)
            })
            .find(|a| &a.id == id)
    }
}

/// Where a download for display stands; see [`ChatState::attachment`].
#[derive(Clone, Debug, PartialEq)]
pub enum AttachmentState {
    Loading,
    Ready(AttachmentContent),
    /// Can be asked for again if the error says that may work; see
    /// [`ChatState::fetch_attachment`].
    Failed(crate::Error),
}

/// Where saving an attachment stands; see [`ChatState::saved`]. Independent of
/// [`AttachmentState`], so a shown picture stays shown while and after it is saved.
#[derive(Clone, Debug, PartialEq)]
pub enum SaveState {
    Saving,
    Saved(std::path::PathBuf),
    Failed(crate::Error),
}

/// A word or two for what an attachment is, e.g. for previews. Windows draw an icon beside
/// it; see [`ConversationView::preview_kind`].
pub fn describe_kind(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "Photo",
        AttachmentKind::Video => "Video",
        AttachmentKind::Audio => "Audio",
        AttachmentKind::VoiceNote => "Voice message",
        AttachmentKind::Sticker => "Sticker",
        AttachmentKind::LongText => "Long message",
        AttachmentKind::ViewOnce => "View-once media",
        AttachmentKind::File => "File",
    }
}

/// [`describe_kind`] with an emoji before it, for notifications, which have no icons.
pub(super) fn describe_kind_for_notice(kind: AttachmentKind) -> String {
    let emoji = match kind {
        AttachmentKind::Image => "📷 ",
        AttachmentKind::Video => "🎞 ",
        AttachmentKind::Audio => "🎵 ",
        AttachmentKind::VoiceNote => "🎤 ",
        AttachmentKind::File => "📎 ",
        AttachmentKind::Sticker | AttachmentKind::LongText | AttachmentKind::ViewOnce => "",
    };
    format!("{emoji}{}", describe_kind(kind))
}

/// Downloaded pictures and audio kept in memory at most, in bytes, beyond what shows: a
/// picture is kept as pixels, 1.6 MB at 640 px.
pub(super) const KEPT_DOWNLOADS: usize = 160 * 1024 * 1024;

/// What a download takes up in memory.
pub(super) fn download_size(content: &AttachmentContent) -> usize {
    match content {
        AttachmentContent::Picture(picture) => picture.rgba.len(),
        AttachmentContent::Audio(audio) => audio.len(),
        AttachmentContent::Text(text) => text.len(),
    }
}

/// Whether an attachment is fetched as soon as its message is shown.
pub fn shown_inline(kind: AttachmentKind) -> bool {
    matches!(
        kind,
        AttachmentKind::Image | AttachmentKind::Sticker | AttachmentKind::LongText
    )
}

/// "240 KB", "1.2 MB": sizes as people read them.
pub fn format_size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{} KB", bytes / 1_000),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}
