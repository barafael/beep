//! The calls the Desktop and Android drivers answer (testbox/desktop/driver.ts, the `android`
//! tool), one JSON document per line each way: a [`Numbered`] call, answered with an [`Answer`].
//! The xtask sends them; the Android driver reads them with the same types.

use serde::{Deserialize, Serialize};

/// A call as it goes over the socket: `{"id": …, "method": …, "params": {…}}`.
#[derive(Serialize, Deserialize)]
pub struct Numbered<C> {
    pub id: u64,
    #[serde(flatten)]
    pub call: C,
}

/// What a driver logs once it takes calls.
pub const LISTENING: &str = "driver listening on";

/// The Android driver's answer: `{"id": …, "result": …}` or `{"id": …, "error": "…"}`
/// (b33p-harness `Reply`, which the xtask reads them as).
#[derive(Serialize)]
#[serde(untagged)]
pub enum Answer<T> {
    Done {
        id: u64,
        result: T,
    },
    /// Without an id when the call didn't parse far enough to have one.
    Failed {
        id: Option<u64>,
        error: String,
    },
}

/// Enough of a line to tell whose it is: a call's, or a reply's (pushed updates have none).
#[derive(Deserialize)]
pub struct Id {
    pub id: Option<u64>,
}

/// Signal-Desktop, driven with Playwright.
#[derive(Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Desktop {
    /// Starts Desktop; the driver listens before it does.
    Launch,
    /// The `sgnl://linkdevice` URL the link screen shows now; answered with a string.
    ProvisioningUrl,
    /// Resolves once Desktop shows its inbox.
    Inbox {
        timeout_ms: u64,
    },
    OpenConversation {
        aci: String,
    },
    Send {
        text: String,
    },
    /// `paths` as the container sees them, sent with `text` as the caption.
    SendFiles {
        paths: Vec<String>,
        text: String,
    },
    EditMessage {
        old_text: String,
        text: String,
    },
    /// Reacts to the newest message with `text` with the first emoji of Desktop's picker.
    ReactToMessage {
        text: String,
    },
    /// Waits for a reaction on a message with `text` in the open conversation.
    WaitForReaction {
        text: String,
        timeout_ms: u64,
    },
    /// Sends `text` as a reply to the newest message with `quoted`.
    ReplyToMessage {
        quoted: String,
        text: String,
    },
    /// Waits for a message with `text` that quotes one with `quoted`.
    WaitForQuote {
        text: String,
        quoted: String,
        timeout_ms: u64,
    },
    /// Deletes the newest message with `text` for everyone.
    DeleteForEveryone {
        text: String,
    },
    SetProfileAvatar,
    /// Records a voice message from the fake microphone and sends it in the open conversation.
    SendVoiceMessage,
    WaitForIncomingAudio {
        timeout_ms: u64,
    },
    /// Waits for a sticker in the open conversation, its picture loaded.
    WaitForSticker {
        timeout_ms: u64,
    },
    WaitForMessage {
        text: String,
        timeout_ms: u64,
    },
    WaitForBold {
        text: String,
        timeout_ms: u64,
    },
    WaitForImage {
        text: String,
        timeout_ms: u64,
    },
    WaitForFile {
        file_name: String,
        timeout_ms: u64,
    },
    WaitForDelivered {
        text: String,
        timeout_ms: u64,
    },
    /// Waits for Desktop's own `text` in the open conversation to show `status`: "sent",
    /// "delivered" or "read", and nothing further.
    WaitForStatus {
        text: String,
        status: String,
        timeout_ms: u64,
    },
    /// Opens the conversation the left pane lists as `title` (a group's), accepting it if it is a
    /// request.
    OpenTitled {
        title: String,
    },
    /// Waits for `text` in the open conversation, a notice's included.
    WaitForText {
        text: String,
        timeout_ms: u64,
    },
    /// Edits the open group, called `old_title` now, as its settings do: a new name, a new
    /// description, or both.
    EditGroup {
        old_title: String,
        title: Option<String>,
        description: Option<String>,
    },
    /// Gives the open group, called `old_title` now, the first of Desktop's preset pictures.
    SetGroupPicture {
        old_title: String,
    },
    /// Leaves the open group, from its settings.
    LeaveGroup,
    /// Sends `text` in the open conversation after a mention of `member`, picked as a person
    /// does: "@" and the name typed, the suggestion taken.
    SendMention {
        member: String,
        text: String,
    },
    /// Waits up to `timeout_ms` for a story with `text` posted to the group `from` names, in the
    /// Stories tab; back to the chats after.
    StoryShows {
        from: String,
        text: String,
        timeout_ms: u64,
    },
    /// Opens a group's invite `link` as a click on it does, and asks to join, or joins, in the
    /// dialog that opens.
    JoinGroup {
        link: String,
    },
    /// Turns read receipts on or off, and resolves once Desktop has written that to the storage
    /// service.
    SetReadReceipts {
        on: bool,
        timeout_ms: u64,
    },
}

/// Signal-Android on the emulator.
#[derive(Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Android {
    /// Registers `number` with `name` as the profile's given name.
    Register {
        number: String,
        name: String,
    },
    NoteToSelf {
        text: String,
    },
    LinkDevice {
        url: String,
        transfer: bool,
    },
    /// Opens the chat whose row in the chat list shows `text`.
    OpenConversation {
        text: String,
    },
    Send {
        text: String,
    },
    /// Waits for what `wanted` names anywhere on the screen.
    WaitFor {
        wanted: crate::agent::Wanted,
        timeout_ms: u64,
    },
    /// Picks `item` from the menu of the chat list's row showing `text` (a long press); a mute is
    /// for good.
    ChatMenu {
        text: String,
        item: ChatMenuItem,
    },
    /// Fails unless the menu of the row showing `text` offers each of `items`, then closes it:
    /// what the row's chat is now (read or not, pinned or not, muted or not).
    ChatMenuOffers {
        text: String,
        items: Vec<ChatMenuItem>,
    },
    /// Where logcat is now, for [`Android::AwaitStorageSync`]: answered with a number.
    LogMark,
    /// Waits until the app has taken in a newer storage manifest since `after` (its log line
    /// "[Remote Sync] Saved new manifest", StorageSyncJob.kt).
    AwaitStorageSync {
        after: u64,
        timeout_ms: u64,
    },
    /// Adds the suggested chat folder `name` (Settings, Chats).
    AddSuggestedFolder {
        name: String,
    },
    /// Makes `change` to `member` of the group `group` names, from the group's settings, as its
    /// admin; done once confirmed.
    GroupMember {
        group: String,
        member: String,
        change: MemberChange,
    },
    /// Turns the invite link of the group `group` names on, from off, from the group's settings,
    /// as its admin; with admin approval if `approval`.
    GroupLink {
        group: String,
        approval: bool,
    },
    /// Approves `member`'s request to join the group `group` names, as its admin.
    ApproveRequest {
        group: String,
        member: String,
    },
}

/// What an admin changes about a member, from the member's sheet (Signal-Android
/// RecipientBottomSheetDialogFragment.kt, strings.xml `RecipientBottomSheet_*`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberChange {
    MakeAdmin,
    RemoveAdmin,
    Remove,
}

impl MemberChange {
    /// The sheet's item (`_make_admin`, `_remove_as_admin`, `_remove_from_group`).
    pub fn item(self) -> &'static str {
        match self {
            Self::MakeAdmin => "Make admin",
            Self::RemoveAdmin => "Remove as admin",
            Self::Remove => "Remove from group",
        }
    }

    /// Words of the confirmation's message (`_s_will_be_able_to_edit_group`,
    /// `_remove_s_as_group_admin`, `_remove_s_from_the_group`), and its button (`_make_admin`,
    /// `_remove_as_admin`, `_remove`: RecipientDialogViewModel.java `setPositiveButton`).
    pub fn confirmation(self) -> (&'static str, &'static str) {
        match self {
            Self::MakeAdmin => ("will be able to edit this group", "Make admin"),
            Self::RemoveAdmin => ("as group admin?", "Remove as admin"),
            Self::Remove => ("from the group?", "Remove"),
        }
    }
}

/// An item of the menu a chat list's row opens on a long press (Signal-Android
/// ConversationListFragment.java `onConversationLongClick`), named as it is labelled (strings.xml
/// `ConversationListFragment_read_plural`, `_unread_plural`, `_pin`, `_unpin`, `_mute`,
/// `_unmute`, `_archive`).
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum ChatMenuItem {
    Read,
    Unread,
    Pin,
    Unpin,
    Mute,
    Unmute,
    Archive,
}

impl ChatMenuItem {
    pub fn label(self) -> &'static str {
        self.into()
    }
}
