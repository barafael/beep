//! The protocol of b33p's agent harness (b33p-slint/src/agent.rs): one JSON document per line
//! each way, these types serialized. A client sends [`Request`]s and reads [`Reply`]s, and
//! [`Pushed`] snapshots once subscribed.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use serde::{Deserialize, Serialize};

/// What b33p logs once its harness listens (b33p-slint/src/agent.rs), which its clients wait for.
pub const LISTENING: &str = "the agent harness is listening";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Echoed in the reply; 0 when a request didn't parse.
    #[serde(default)]
    pub id: u64,
    #[serde(flatten)]
    pub call: Call,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Call {
    /// Answered with [`Pong`].
    Ping,
    /// Answered with [`Snapshot`].
    Snapshot,
    /// Answered with [`Subscribed`], then a [`Pushed`] snapshot after every redraw.
    Subscribe,
    /// Answered with [`Unsubscribed`].
    Unsubscribe,
    /// Answered with [`Acted`].
    Act { action: Action },
    /// Writes the window, as drawn, to `path` as a PNG. Answered with [`Shot`].
    Screenshot { path: String },
}

/// The answer to request `id`: `{"id": …, "result": …}` or `{"id": …, "error": "…"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply<T> {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome<T>,
}

/// A request's result, or why there is none. Tagged by its field: a result that doesn't parse
/// says why, where an untagged reply would only say that no form matched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Outcome<T> {
    #[serde(rename = "result")]
    Ok(T),
    #[serde(rename = "error")]
    Err(String),
}

impl<T> From<Result<T, String>> for Outcome<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(found) => Self::Ok(found),
            Err(error) => Self::Err(error),
        }
    }
}

impl<T> From<Outcome<T>> for Result<T, String> {
    fn from(outcome: Outcome<T>) -> Self {
        match outcome {
            Outcome::Ok(found) => Ok(found),
            Outcome::Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub version: String,
    /// Whether the app runs against the mock backend.
    pub mock: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subscribed {
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unsubscribed {
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Acted {
    /// Whether a command went to the backend.
    pub command_sent: bool,
}

/// A screenshot's size in pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
}

/// Pushed to a subscriber after every redraw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pushed {
    pub event: PushedEvent,
    pub seq: u64,
    pub snapshot: Snapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushedEvent {
    Update,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetStyle {
    Native,
    Fusion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSize {
    Small,
    Default,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Animations {
    System,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationContent {
    NameAndMessage,
    NameOnly,
    Nothing,
}

/// The enums to and from b33p-core's, which name the same variants. A match per direction, so a
/// variant added on either side doesn't compile until both have it.
#[cfg(feature = "core")]
mod core_enums {
    macro_rules! same_variants {
        ($core:path => $name:ident: $($variant:ident),+) => {
            impl From<$core> for super::$name {
                fn from(value: $core) -> Self {
                    use $core as Core;
                    match value {
                        $(Core::$variant => Self::$variant,)+
                    }
                }
            }

            impl From<super::$name> for $core {
                fn from(value: super::$name) -> Self {
                    match value {
                        $(super::$name::$variant => Self::$variant,)+
                    }
                }
            }
        };
    }

    same_variants!(b33p_core::settings::Theme => Theme: System, Light, Dark);
    same_variants!(b33p_core::settings::WidgetStyle => WidgetStyle: Native, Fusion);
    same_variants!(b33p_core::settings::TextSize => TextSize: Small, Default, Large);
    same_variants!(b33p_core::settings::Animations => Animations: System, On, Off);
    same_variants!(
        b33p_core::settings::NotificationContent => NotificationContent: NameAndMessage, NameOnly, Nothing
    );
    same_variants!(b33p_core::InviteLinkAccess => InviteLinkAccess: Off, Anyone, Approval);
}

/// How far a message of ours got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Sending,
    /// Failed, and may go out if sent again.
    Failed,
    /// Signal refused it: sending it again wouldn't help.
    Refused,
    Sent,
    Delivered,
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    VoiceNote,
    Sticker,
    LongText,
    ViewOnce,
    File,
}

/// An attachment fetched for display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Download {
    Loading,
    /// Downloaded and decrypted.
    Ready,
    Failed(String),
}

/// An attachment saved to the downloads folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Saved {
    Saving,
    /// Where it was written.
    Saved(String),
    Failed(String),
}

/// Everything the window's own handlers can do, named for what a person does. Every arm takes
/// the same road as its button or key: the same `ChatState` method, with the same redraw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Select {
        conversation: String,
    },
    Back,
    SelectRelative {
        by: i32,
    },
    SelectNextUnread,
    SelectPinned {
        n: u32,
    },
    /// Puts text in the composer, as typed.
    Type {
        text: String,
    },
    /// Sends what is in the composer.
    Send,
    Edit {
        index: u32,
    },
    CancelEdit,
    ReplyTo {
        index: u32,
    },
    CancelReply,
    React {
        index: u32,
        emoji: String,
    },
    Delete {
        index: u32,
        everyone: bool,
    },
    ToggleSelect {
        index: u32,
    },
    SelectThrough {
        index: u32,
    },
    ClearSelection,
    ForwardSelection,
    DeleteSelection {
        everyone: bool,
    },
    TogglePin {
        conversation: String,
    },
    MovePin {
        conversation: String,
        by: i32,
    },
    MarkRead {
        conversation: String,
        read: bool,
    },
    /// 0 unmutes, below 0 mutes for good, otherwise minutes from now.
    Mute {
        conversation: String,
        minutes: i32,
    },
    Archive {
        conversation: String,
        archived: bool,
    },
    Block {
        conversation: String,
        blocked: bool,
    },
    SetTimer {
        conversation: String,
        seconds: u32,
    },
    AnswerInvitation {
        conversation: String,
        accept: bool,
    },
    ConfirmSafetyNumber {
        conversation: String,
    },
    /// Answers the message request of the open chat.
    Respond {
        answer: Answer,
    },
    /// The group actions work on the open chat, as their buttons do.
    RenameGroup {
        title: String,
    },
    /// The group's picture from an image file; none takes it away.
    SetGroupPicture {
        path: Option<String>,
    },
    DescribeGroup {
        description: String,
    },
    SetMemberAdmin {
        member: String,
        admin: bool,
    },
    RemoveMember {
        member: String,
    },
    LeaveGroup,
    CreateGroup {
        title: String,
        members: Vec<String>,
    },
    FindNumber {
        number: String,
        open: bool,
    },
    OpenDetails,
    CloseDetails,
    OpenMedia,
    CloseMedia,
    /// The sticker picker's ▦: gathers the installed packs, the first time.
    OpenStickers,
    /// A sticker of an installed pack, to the selected conversation.
    SendSticker {
        pack: String,
        sticker: u32,
    },
    OpenStory {
        conversation: String,
    },
    StepStory {
        by: i32,
    },
    CloseStory,
    PostStoryText {
        group: String,
        text: String,
    },
    OpenNewGroup,
    /// The open group's "add members": the draft its candidates and `FinishGroupDraft` add to.
    OpenAddMembers,
    /// Joining a group through its invite link: with a link, it is looked up at once.
    OpenJoinGroup {
        link: Option<String>,
    },
    PreviewGroupLink {
        link: String,
    },
    JoinGroup,
    CloseJoinGroup,
    SetInviteLink {
        access: InviteLinkAccess,
    },
    ResetInviteLink,
    AnswerJoinRequest {
        member: String,
        approve: bool,
    },
    CancelJoinRequest,
    CloseGroupDraft,
    FinishGroupDraft {
        title: String,
    },
    ToggleCandidate {
        conversation: String,
    },
    OpenChatSearch,
    CloseChatSearch,
    SearchChat {
        text: String,
    },
    ShowResult {
        index: u32,
    },
    /// Searches the chat list.
    Search {
        text: String,
    },
    Forward {
        index: u32,
    },
    ForwardTo {
        conversation: String,
    },
    CancelForward,
    LoadOlder,
    ShowFolder {
        index: i32,
    },
    ShowArchived {
        show: bool,
    },
    DismissError,
    /// The error banner's "Try again".
    RetryError,
    Unlock {
        password: String,
        confirmation: String,
    },
    SkipHistory,
    /// Goes to the chats while catching up, as the start screen's button does.
    Start,
    /// On the failure screen.
    Retry,
    UsePassword,
    /// What the confirmation dialog's button does: deletes the data here.
    StartOver,
    Unlink,
    StageFiles {
        paths: Vec<String>,
    },
    Unstage {
        index: u32,
    },
    FetchAttachment {
        id: String,
    },
    OpenAttachment {
        id: String,
    },
    /// As the menu's Save: to the downloads folder.
    SaveAttachment {
        id: String,
    },
    /// Sends the message at `index` in the open chat again, after it failed.
    RetrySend {
        index: u32,
    },
    /// Pretends the window lost or regained focus, for tests of read receipts.
    SetWindowFocused {
        focused: bool,
    },
    /// Names this computer for the phone's list of linked devices; empty for the default.
    RenameDevice {
        name: String,
    },
    /// Opens (or closes) what only the window keeps open, as its buttons do: "settings",
    /// "stories", "calendar", or the dialogs that ask first: "unlink", "start_over",
    /// "leave_group".
    Show {
        what: String,
        open: bool,
    },
    /// Gives the window this size, in logical pixels, as dragging its edge would.
    Resize {
        width: f32,
        height: f32,
    },
    /// Changes the given settings and leaves the rest as they are.
    SetSettings {
        theme: Option<Theme>,
        style: Option<WidgetStyle>,
        text_size: Option<TextSize>,
        animations: Option<Animations>,
        show_avatars: Option<bool>,
        notifications: Option<bool>,
        notification_content: Option<NotificationContent>,
        show_activity: Option<bool>,
        show_calendar: Option<bool>,
        close_to_tray: Option<bool>,
        read_receipts: Option<bool>,
        typing_indicators: Option<bool>,
        link_previews: Option<bool>,
        sync_to_phone: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Accept,
    Delete,
    Block,
}

/// What the window shows, as data. Shaped like what `ui.rs` reads from the state, not like
/// the Slint tree: the harness reports meaning, not widgets. Messages keep their position as
/// `index`, since actions on them (`react`, `edit`, …) take it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub mock: bool,
    pub screen: ScreenSnapshot,
    pub account: String,
    pub synced: bool,
    pub error: Option<String>,
    /// The error offers "Try again" (`retry_error`).
    pub can_retry_error: bool,
    pub unread_total: u32,
    /// Only a direct `snapshot` answer has it; pushed updates don't look at the window.
    pub window: Option<WindowSnapshot>,
    pub conversations: Vec<ConversationSnapshot>,
    pub selected: Option<String>,
    pub messages: Vec<MessageSnapshot>,
    pub draft: String,
    pub replying_to: Option<QuoteSnapshot>,
    pub editing: bool,
    pub staged: Vec<String>,
    pub typing: Vec<String>,
    /// The join-a-group dialog, while open.
    pub join: Option<JoinSnapshot>,
    /// The story shown, while one is.
    pub story: Option<StorySnapshot>,
    pub settings: SettingsSnapshot,
    pub stickers: StickersSnapshot,
    /// The phone's chat folders, in its order.
    pub folders: Vec<FolderSnapshot>,
    /// The open conversation's details, once `OpenDetails` has loaded them.
    pub details: Option<DetailsSnapshot>,
}

/// What a conversation's details page shows, for a group most of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DetailsSnapshot {
    pub description: Option<String>,
    pub members: Vec<MemberSnapshot>,
    /// The group's invite link, while it is on.
    pub invite_link: Option<String>,
    pub invite_link_access: InviteLinkAccess,
    /// Who asked to join through the link (service IDs).
    pub requests: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemberSnapshot {
    pub service_id: String,
    pub name: String,
    pub admin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteLinkAccess {
    Off,
    Anyone,
    Approval,
}

/// A chat folder, as the storage service has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FolderSnapshot {
    pub name: String,
    pub only_unread: bool,
    pub show_muted: bool,
    pub all_individual: bool,
    pub all_groups: bool,
    pub included: Vec<String>,
    pub excluded: Vec<String>,
}

/// The installed sticker packs, as the picker has them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickersSnapshot {
    /// The picker hasn't been opened.
    NotAsked,
    Gathering,
    Installed(Vec<StickerPackSnapshot>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StickerPackSnapshot {
    /// Hex.
    pub id: String,
    pub title: String,
    pub stickers: Vec<StickerSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StickerSnapshot {
    pub id: u32,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StorySnapshot {
    pub author: String,
    /// Which of the author's stories it is, counted from 1, and how many there are.
    pub position: usize,
    pub count: usize,
    /// A text story's text; a picture story has none.
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinSnapshot {
    pub link: String,
    /// The link is being looked up, or the group joined.
    pub busy: bool,
    /// What the link showed of its group, once looked up.
    pub title: Option<String>,
    pub members: u32,
    pub approval: bool,
    pub requested: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum ScreenSnapshot {
    /// Starting up, with what the screen says it waits for.
    Syncing {
        status: String,
    },
    Password {
        new: bool,
        error: Option<String>,
    },
    /// The QR's link, for a primary driven by a script (the testbox's presage-cli and Android).
    Link {
        url: String,
    },
    Chats,
    /// See b33p-core's `Event::Failed`.
    Failed {
        failure: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub visible: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationSnapshot {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub group: bool,
    pub note_to_self: bool,
    pub unread: u32,
    pub marked_unread: bool,
    pub pinned: bool,
    pub muted: bool,
    pub archived: bool,
    pub blocked: bool,
    pub accepted: bool,
    pub request: bool,
    pub invited: bool,
    pub left: bool,
    /// We asked to join through the group's link, and wait for an admin.
    pub requested: bool,
    pub expire_timer: u32,
    pub preview: String,
    /// How far our last message got; `None` without one of ours.
    pub last_delivery: Option<Delivery>,
    pub typing: Vec<String>,
    /// Whether a picture shows for it: a profile or group avatar, downloaded and decoded.
    pub avatar: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageSnapshot {
    /// What the actions taking a message's `index` name it by.
    pub index: u32,
    pub sender: String,
    pub sender_id: String,
    pub text: String,
    pub outgoing: bool,
    pub notice: bool,
    pub deleted: bool,
    pub edited: bool,
    pub timestamp: u64,
    pub delivery: Option<Delivery>,
    pub day: Option<String>,
    pub first_unread: bool,
    pub selected: bool,
    pub reactions: Vec<ReactionSnapshot>,
    pub quote: Option<QuoteSnapshot>,
    pub attachments: Vec<AttachmentSnapshot>,
    /// Who it mentions, by service id. Absent in a b33p built before it.
    #[serde(default)]
    pub mentions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReactionSnapshot {
    pub emoji: String,
    pub sender: String,
    pub outgoing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuoteSnapshot {
    pub author: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachmentSnapshot {
    pub id: String,
    pub kind: AttachmentKind,
    pub file_name: Option<String>,
    pub size: u64,
    pub caption: Option<String>,
    /// After `fetch_attachment`.
    pub download: Option<Download>,
    /// After `save_attachment`.
    pub saved: Option<Saved>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub theme: Theme,
    pub style: WidgetStyle,
    pub text_size: TextSize,
    pub animations: Animations,
    pub show_avatars: bool,
    pub notifications: bool,
    pub notification_content: NotificationContent,
    pub show_activity: bool,
    pub show_calendar: bool,
    pub close_to_tray: bool,
    pub read_receipts: bool,
    pub typing_indicators: bool,
    pub link_previews: bool,
    pub sync_to_phone: bool,
    pub device_name: String,
}
