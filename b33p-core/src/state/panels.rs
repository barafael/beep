//! What opens beside or over the chat: groups, details, media, stories, the chat's search.

use super::*;

impl ChatState {
    /// Starts choosing people for a new group, or to add to `adding_to`.
    pub fn open_group_draft(&mut self, adding_to: Option<ConversationId>) {
        self.group_draft = Some(GroupDraft {
            adding_to,
            chosen: Vec::new(),
        });
    }

    pub fn close_group_draft(&mut self) {
        self.group_draft = None;
    }

    /// Starts joining a group through its invite link, looking `link` up at once if given.
    pub fn open_join(&mut self, link: Option<&str>) -> Option<Command> {
        self.join = Some(JoinDraft {
            link: link.unwrap_or_default().to_owned(),
            preview: None,
            busy: false,
        });
        link.and_then(|link| self.preview_link(link))
    }

    pub fn close_join(&mut self) {
        self.join = None;
    }

    /// Looks up what the group behind `link` shows of itself; nothing while not synced or
    /// not joining.
    pub fn preview_link(&mut self, link: &str) -> Option<Command> {
        let synced = self.synced;
        let join = self.join.as_mut().filter(|_| synced)?;
        let link = link.trim();
        if link.is_empty() {
            return None;
        }
        join.link = link.to_owned();
        join.preview = None;
        join.busy = true;
        Some(Command::PreviewGroupLink(link.to_owned()))
    }

    /// Joins the group looked up with [`Self::preview_link`], or asks to.
    pub fn join_group(&mut self) -> Option<Command> {
        let join = self.join.as_mut()?;
        let preview = join.preview.as_ref().filter(|_| !join.busy)?;
        join.busy = true;
        Some(Command::JoinGroup(preview.link.clone()))
    }

    /// While joining through a link: the link typed, what its group shows of itself once
    /// looked up, and whether that or the joining is under way.
    pub fn join(&self) -> Option<(&str, Option<&crate::GroupPreview>, bool)> {
        let join = self.join.as_ref()?;
        Some((&join.link, join.preview.as_ref(), join.busy))
    }

    /// Chooses someone for the group, or unchooses them.
    pub fn toggle_group_member(&mut self, id: &ConversationId) {
        if let Some(draft) = &mut self.group_draft {
            match draft.chosen.iter().position(|c| c == id) {
                Some(at) => {
                    draft.chosen.remove(at);
                }
                None => draft.chosen.push(id.clone()),
            }
        }
    }

    /// While choosing: whether it is for a new group, and who is chosen.
    pub fn group_draft(&self) -> Option<(bool, &[ConversationId])> {
        let draft = self.group_draft.as_ref()?;
        Some((draft.adding_to.is_none(), &draft.chosen))
    }

    /// Makes the new group named `title`, or adds the chosen people to the group.
    pub fn finish_group_draft(&mut self, title: &str) -> Option<Command> {
        let draft = self.group_draft.as_ref()?;
        let command = match draft.adding_to.clone() {
            None => self.create_group(title, &draft.chosen.clone())?,
            Some(group) => {
                let members = draft
                    .chosen
                    .iter()
                    .filter_map(ConversationId::contact_id)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if members.is_empty() {
                    return None;
                }
                self.edit_group(&group, crate::GroupEdit::AddMembers(members))?
            }
        };
        self.group_draft = None;
        Some(command)
    }

    /// People who can be chosen: contacts whose name contains `filter`, without those
    /// already in the group being added to, and without strangers whose message request is
    /// unanswered (adding them would answer it).
    pub fn group_candidates(&self, filter: &str) -> Vec<&Conversation> {
        let filter = search_term(filter);
        let already: HashSet<&str> = self
            .group_draft
            .as_ref()
            .and_then(|draft| self.conversation(draft.adding_to.as_ref()?))
            .map(|group| {
                group
                    .members
                    .iter()
                    .map(|p| p.service_id.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let mut candidates: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|c| !c.is_group() && !c.note_to_self && !c.settings.blocked)
            .filter(|c| !self.is_request(c))
            .filter(|c| !c.id.contact_id().is_some_and(|p| already.contains(p)))
            .filter(|c| title_matches(c, &filter))
            .collect();
        candidates.sort_by_cached_key(|c| c.title.to_lowercase());
        candidates
    }

    /// Makes a group named `title` with these contacts.
    pub fn create_group(&mut self, title: &str, members: &[ConversationId]) -> Option<Command> {
        let title = title.trim();
        if title.is_empty() || !self.synced {
            return None;
        }
        let members = members
            .iter()
            .filter_map(ConversationId::contact_id)
            .map(str::to_owned)
            .collect();
        Some(Command::CreateGroup {
            title: title.to_owned(),
            members,
        })
    }

    /// Changes a group.
    pub fn edit_group(&mut self, id: &ConversationId, edit: crate::GroupEdit) -> Option<Command> {
        use crate::GroupEdit::{AcceptInvitation, CancelJoinRequest, DeclineInvitation};
        let invitation = matches!(edit, AcceptInvitation | DeclineInvitation);
        let cancelling = edit == CancelJoinRequest;
        // An invitation is answered, and a request taken back, by someone not in the group
        // yet; anything else needs being in.
        let group = self.conversation(id).filter(|c| {
            c.is_group()
                && if invitation {
                    c.invited
                } else if cancelling {
                    c.requested
                } else {
                    !c.left
                }
        })?;
        if let crate::GroupEdit::Title(title) = &edit
            && (title.trim().is_empty() || title.trim() == group.title)
        {
            return None;
        }
        Some(Command::EditGroup {
            conversation: id.clone(),
            edit,
        })
    }

    /// Shows the selected conversation's details page; returns the command that gathers what it
    /// shows.
    pub fn open_details(&mut self) -> Option<Command> {
        let id = self.selected.clone()?;
        self.details = Some((id.clone(), None));
        Some(Command::LoadDetails(id))
    }

    pub fn close_details(&mut self) {
        self.details = None;
    }

    /// Marks the safety number on the open details page verified, or clears that.
    pub fn set_verified(&mut self, verified: bool) -> Option<Command> {
        let (conversation, details) = self.details.as_ref()?;
        details.as_ref()?.safety_number.as_ref()?;
        Some(Command::SetVerified {
            conversation: conversation.clone(),
            verified,
        })
    }

    /// The details page, if shown: `Some(None)` while it is gathered.
    pub fn details(&self) -> Option<Option<&Details>> {
        self.details.as_ref().map(|(_, details)| details.as_ref())
    }

    /// Shows the selected conversation's media, files and links; returns the command that
    /// gathers them.
    pub fn open_media(&mut self) -> Option<Command> {
        let id = self.selected.clone()?;
        self.media = Some((id.clone(), None));
        Some(Command::LoadMedia(id))
    }

    pub fn close_media(&mut self) {
        self.media = None;
    }

    /// The media overview, if shown: `Some(None)` while it is gathered.
    pub fn media(&self) -> Option<Option<MediaOverview<'_>>> {
        let (_, gathered) = self.media.as_ref()?;
        let Some(Gathered { messages, links }) = gathered else {
            return Some(None);
        };
        let mut overview = MediaOverview {
            media: Vec::new(),
            files: Vec::new(),
            links: links
                .iter()
                .map(|(at, link)| (&messages[*at], link.clone()))
                .collect(),
        };
        for message in messages.iter().filter(|m| !m.deleted) {
            for attachment in &message.attachments {
                match attachment.kind {
                    AttachmentKind::Image | AttachmentKind::Video => {
                        overview.media.push(attachment)
                    }
                    AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::VoiceNote => {
                        overview.files.push((message, attachment))
                    }
                    AttachmentKind::Sticker
                    | AttachmentKind::LongText
                    | AttachmentKind::ViewOnce => {}
                }
            }
        }
        Some(Some(overview))
    }

    /// Who has stories: unseen ones first, then the newest.
    pub fn story_rings(&self) -> Vec<StoryRing<'_>> {
        let mut rings: Vec<StoryRing<'_>> = Vec::new();
        let mut ring_of: HashMap<&ConversationId, usize> = HashMap::new();
        for story in self.stories.iter().filter(|s| self.story_shown(s)) {
            match ring_of.get(&story.conversation) {
                Some(&at) => {
                    let ring = &mut rings[at];
                    ring.latest = ring.latest.max(story.timestamp);
                    ring.count += 1;
                    ring.unviewed |= !story.viewed;
                }
                None => {
                    ring_of.insert(&story.conversation, rings.len());
                    rings.push(StoryRing {
                        conversation: &story.conversation,
                        title: self
                            .conversation(&story.conversation)
                            .map_or_else(|| story.author.clone(), |c| c.title.clone()),
                        latest: story.timestamp,
                        count: 1,
                        unviewed: !story.viewed,
                    });
                }
            }
        }
        rings.sort_by_key(|r| (!r.unviewed, Reverse(r.latest)));
        rings
    }

    /// Stories not seen yet, e.g. for a dot on the stories button.
    pub fn unviewed_stories(&self) -> usize {
        self.stories
            .iter()
            .filter(|s| !s.viewed && self.story_shown(s))
            .count()
    }

    /// Whether a story is listed: not from strangers (chats that are message requests, or
    /// none at all), as watching it would tell them.
    pub(super) fn story_shown(&self, story: &crate::Story) -> bool {
        story.outgoing
            || self
                .conversation(&story.conversation)
                .is_some_and(|c| !self.is_request(c) && !c.settings.blocked)
    }

    /// A conversation's stories, if they are shown at all.
    pub(super) fn stories_of<'a>(
        &'a self,
        id: &'a ConversationId,
    ) -> impl Iterator<Item = &'a crate::Story> {
        self.stories
            .iter()
            .filter(move |s| &s.conversation == id && self.story_shown(s))
    }

    /// Shows a conversation's stories, from the first not seen yet.
    pub fn open_story(&mut self, id: &ConversationId) -> Option<Command> {
        let at = self
            .stories_of(id)
            .position(|s| !s.viewed)
            .unwrap_or_default();
        self.stories_of(id).nth(at)?;
        self.story_view = Some((id.clone(), at));
        self.view_current()
    }

    /// The next (`by` 1) or previous (-1) story of the same conversation; past the last, the
    /// viewer closes.
    pub fn step_story(&mut self, by: isize) -> Option<Command> {
        let (id, at) = self.story_view.clone()?;
        let count = self.stories_of(&id).count();
        match at.checked_add_signed(by).filter(|&next| next < count) {
            Some(next) => {
                self.story_view = Some((id, next));
                self.view_current()
            }
            None if by > 0 => {
                self.story_view = None;
                None
            }
            None => None,
        }
    }

    pub fn close_story(&mut self) {
        self.story_view = None;
    }

    /// The story shown, which of how many it is.
    pub fn current_story(&self) -> Option<(&crate::Story, usize, usize)> {
        let (id, at) = self.story_view.as_ref()?;
        let story = self.stories_of(id).nth(*at)?;
        Some((story, *at, self.stories_of(id).count()))
    }

    /// Marks the story shown as seen, the first time.
    pub(super) fn view_current(&mut self) -> Option<Command> {
        let (id, at) = self.story_view.clone()?;
        // A conversation's stories are shown all or none, so its index holds among them all.
        self.stories_of(&id).nth(at)?;
        let story = self
            .stories
            .iter_mut()
            .filter(|s| s.conversation == id)
            .nth(at)?;
        if story.viewed {
            return None;
        }
        story.viewed = true;
        Some(Command::ViewStory(MessageRef {
            author_id: story.author_id.clone(),
            timestamp: story.timestamp,
        }))
    }

    /// Groups a story can be posted to, by name.
    pub fn story_groups(&self) -> Vec<&Conversation> {
        let mut groups: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|c| c.is_group() && !c.left)
            .collect();
        groups.sort_by_cached_key(|c| c.title.to_lowercase());
        groups
    }

    /// Posts a story to a group.
    pub fn send_story(&mut self, id: &ConversationId, story: crate::NewStory) -> Option<Command> {
        self.conversation(id).filter(|c| c.is_group() && !c.left)?;
        if let crate::NewStory::Text(text) = &story
            && text.trim().is_empty()
        {
            return None;
        }
        Some(Command::SendStory {
            conversation: id.clone(),
            story,
        })
    }

    /// Opens the search in the selected conversation.
    pub fn open_chat_search(&mut self) {
        if let Some(conversation) = self.selected.clone() {
            self.chat_search = Some(ChatSearch {
                conversation,
                query: String::new(),
                results: None,
            });
        }
    }

    pub fn close_chat_search(&mut self) {
        self.chat_search = None;
    }

    /// Searches the selected conversation for `query`.
    pub fn search_chat(&mut self, query: &str) -> Option<Command> {
        let search = self.chat_search.as_mut()?;
        let query = query.trim();
        search.query = query.to_owned();
        search.results = (query.is_empty()).then(Vec::new);
        (!query.is_empty()).then(|| Command::SearchChat {
            conversation: search.conversation.clone(),
            query: query.to_owned(),
        })
    }

    /// The search in the selected conversation, if open: what is looked for, and what was found
    /// (`None` while looking).
    pub fn chat_search(&self) -> Option<(&str, Option<&[ChatMessage]>)> {
        let search = self.chat_search.as_ref()?;
        Some((&search.query, search.results.as_deref()))
    }

    /// Shows the `index`th search result in the chat: closes the search, marks the message, and
    /// scrolls to it, loading older history first if needed (through
    /// [`ChatState::take_commands`]).
    pub fn show_result(&mut self, index: usize) -> Update {
        let target = self
            .chat_search
            .as_ref()
            .and_then(|search| search.results.as_ref()?.get(index))
            .map(ChatMessage::reference);
        let Some(target) = target else {
            return Update::default();
        };
        self.chat_search = None;
        let unmarked = self.highlight.replace(target.clone());
        self.changed_rows
            .get_mut()
            .note(unmarked.into_iter().chain([target.clone()]));
        self.pending_jump = Some(target);
        self.continue_jump();
        Update {
            rows: true,
            ..Update::default()
        }
    }
}
