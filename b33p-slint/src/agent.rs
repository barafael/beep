//! The agent harness: a socket a script can use to read what the window shows and to act as
//! if it were the input. For tests and demonstrations of the real app — an agent drives it the
//! way a person would, through [`Shared::act`] and the refreshes that follow, so what works
//! here is what works by hand. Compiled only with `--features harness`, so a release build
//! doesn't carry it; and only on Unix so far.
//!
//! It listens on the path in `B33P_AGENT_SOCKET`, if that is set; otherwise it does nothing.
//! The protocol is one JSON document per line (a client lives in `tests/harness.rs`):
//!
//! → `{"id":1,"method":"ping"}`
//! ← `{"id":1,"result":{"version":"…","mock":true}}`
//! → `{"id":2,"method":"snapshot"}`
//! ← `{"id":2,"result":{…}}` — the state, as [`Snapshot`] has it
//! → `{"id":3,"method":"act","action":{"type":"select","conversation":"c:alice"}}`
//! ← `{"id":3,"result":{"command_sent":true}}` — whether a command went to the backend
//! → `{"id":4,"method":"subscribe"}`
//! ← `{"id":4,"result":{"seq":0}}`
//! ← `{"event":"update","seq":1,"snapshot":{…}}` — pushed after every redraw
//!
//! Errors come as `{"id":…,"error":"…"}`, including for requests that didn't parse; `id` is
//! then 0 or missing, and the connection stays open.
//!
//! The socket's file is 0600, so only programs of the same user can connect. It carries what
//! the window shows, message text included: point `B33P_AGENT_SOCKET` into a private
//! directory (`XDG_RUNTIME_DIR`), never into /tmp.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use serde::{Deserialize, Serialize};
use serde_json::json;

use b33p_core::settings::{
    Animations, NotificationContent, Settings, TextSize, Theme, WidgetStyle,
};
use b33p_core::state::{self, ChatState, Update};
use b33p_core::{AfterFinding, Backend, Command, ConversationId, GroupEdit};

use crate::Shared;
use crate::ui;

/// Starts listening when `B33P_AGENT_SOCKET` names a path, else stays out of the way. Called
/// once, from `main`, once the window exists.
pub fn start(shared: &Rc<Shared>) {
    let Some(path) = std::env::var_os("B33P_AGENT_SOCKET") else {
        return;
    };
    let path = PathBuf::from(path);
    let path_for_log = path.display().to_string();
    // A socket left over from a run that died would keep `bind` from working.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), path = %b33p_core::scrub::scrubbed(&path.display()), "can't open the agent socket");
            return;
        }
    };
    // Connect needs write permission on the file; nobody but the owner gets any.
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    // Requests are carried to the UI thread, which alone may touch the state; answers and
    // pushed updates go back through the connection's own channel (`Job::reply`).
    let jobs = async_channel::unbounded();
    slint::spawn_local(pump(Rc::downgrade(shared), jobs.1)).expect("the event loop is set up");
    let spawned = std::thread::Builder::new()
        .name("b33p-agent".into())
        .spawn(move || {
            accept(listener, jobs.0);
            let _ = std::fs::remove_file(&path);
        });
    if let Err(error) = spawned {
        tracing::warn!(error = %b33p_core::scrub::scrubbed(&error), "can't start the agent listener");
        return;
    }
    tracing::info!(path = %b33p_core::scrub::scrubbed(&path_for_log), "the agent harness is listening");
}

/// What a connection asks, on its way to the UI thread. `request` is `None` once the
/// connection is gone and only its subscription is left to take down.
struct Job {
    connection: u64,
    reply: async_channel::Sender<String>,
    request: Option<Request>,
}

/// One connection's traffic, in outbound JSON lines. The pump (on the UI thread) puts answers
/// and pushed snapshots in; a thread of the connection's own writes them to the socket, so a
/// slow reader can't stall the window.
type Outbound = async_channel::Sender<String>;

fn accept(listener: UnixListener, jobs: async_channel::Sender<Job>) {
    for (connection, stream) in listener.incoming().enumerate() {
        let Ok(stream) = stream else { break };
        let Ok(reader) = stream.try_clone() else {
            continue;
        };
        let jobs = jobs.clone();
        let (out, for_writer) = async_channel::unbounded();
        let _ = std::thread::Builder::new()
            .name(format!("b33p-agent-out-{connection}"))
            .spawn(move || write_out(stream, for_writer));
        let _ = std::thread::Builder::new()
            .name(format!("b33p-agent-in-{connection}"))
            .spawn(move || read_requests(reader, connection as u64, out, jobs));
    }
}

fn read_requests(
    reader: UnixStream,
    connection: u64,
    out: Outbound,
    jobs: async_channel::Sender<Job>,
) {
    let mut lines = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match lines.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let id = serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .and_then(|value| value.get("id").and_then(serde_json::Value::as_u64))
            .unwrap_or(0);
        let request = match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => request,
            Err(error) => {
                let _ = out.send_blocking(
                    json!({ "id": id, "error": format!("not a request: {error}") }).to_string(),
                );
                continue;
            }
        };
        if jobs
            .send_blocking(Job {
                connection,
                reply: out.clone(),
                request: Some(request),
            })
            .is_err()
        {
            return; // The app is shutting down.
        }
    }
    // The connection went away; the pump takes down its subscription.
    let _ = jobs.send_blocking(Job {
        connection,
        reply: out,
        request: None,
    });
}

fn write_out(mut stream: UnixStream, out: async_channel::Receiver<String>) {
    while let Ok(line) = out.recv_blocking() {
        if stream.write_all(line.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
            return;
        }
    }
}

/// Answers requests on the UI thread, in the order connections sent them. Ends when the app
/// does (`shared` going away).
async fn pump(shared: Weak<Shared>, jobs: async_channel::Receiver<Job>) {
    while let Ok(job) = jobs.recv().await {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        match job.request {
            None => shared.agent.unsubscribe(job.connection),
            Some(request) => answer(&shared, job.connection, &job.reply, request),
        }
    }
}

fn answer(shared: &Rc<Shared>, connection: u64, reply: &Outbound, request: Request) {
    let result = match request.call {
        Call::Ping => json!({
            "version": env!("CARGO_PKG_VERSION"),
            "mock": Backend::mock_requested(),
        }),
        Call::Snapshot => serde_json::to_value(snapshot(
            &shared.chat.borrow(),
            &shared.settings.borrow(),
            shared.ui().as_deref(),
        ))
        .expect("a snapshot always serializes"),
        Call::Subscribe => {
            let seq = shared.agent.subscribe(connection, reply.clone());
            json!({ "seq": seq })
        }
        Call::Unsubscribe => {
            shared.agent.unsubscribe(connection);
            json!({ "ok": true })
        }
        Call::Act { action } => match perform(shared, action) {
            Ok(command_sent) => json!({ "command_sent": command_sent }),
            Err(error) => return send(reply, request.id, Err(error)),
        },
    };
    send(reply, request.id, Ok(result));
}

fn send(reply: &Outbound, id: u64, result: Result<serde_json::Value, String>) {
    let line = match result {
        Ok(value) => json!({ "id": id, "result": value }),
        Err(error) => json!({ "id": id, "error": error }),
    };
    let _ = reply.send_blocking(line.to_string());
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: u64,
    #[serde(flatten)]
    call: Call,
}

#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum Call {
    Ping,
    Snapshot,
    Subscribe,
    Unsubscribe,
    Act { action: Action },
}

/// Everything the window's own handlers can do, named for what a person does. Every arm takes
/// the same road as its button or key: the same `ChatState` method, with the same redraw.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Action {
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
    /// Joining a group through its invite link: with a link, it is looked up at once.
    OpenJoinGroup {
        link: Option<String>,
    },
    PreviewGroupLink {
        link: String,
    },
    JoinGroup,
    CloseJoinGroup,
    /// "off", "anyone" or "approval".
    SetInviteLink {
        access: String,
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
    Unlock {
        password: String,
        confirmation: String,
    },
    SkipHistory,
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
    /// Sends the message at `index` in the open chat again, after it failed.
    RetrySend {
        index: usize,
    },
    /// Pretends the window lost or regained focus, for tests of read receipts.
    SetWindowFocused {
        focused: bool,
    },
    /// Names this computer for the phone's list of linked devices; empty for the default.
    RenameDevice {
        name: String,
    },
    /// Changes the given settings and leaves the rest as they are.
    SetSettings {
        theme: Option<String>,
        style: Option<String>,
        text_size: Option<String>,
        animations: Option<String>,
        show_avatars: Option<bool>,
        notifications: Option<bool>,
        notification_content: Option<String>,
        show_activity: Option<bool>,
        show_calendar: Option<bool>,
        close_to_tray: Option<bool>,
        read_receipts: Option<bool>,
        typing_indicators: Option<bool>,
        link_previews: Option<bool>,
        sync_to_phone: Option<bool>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Answer {
    Accept,
    Delete,
    Block,
}

/// Carries `action` out the way the window's own handler does, and reports whether it sent a
/// command to the backend.
fn perform(shared: &Rc<Shared>, action: Action) -> Result<bool, String> {
    Ok(match action {
        Action::Select { conversation } => shared.act(|chat| {
            let command = chat.select(&ConversationId(conversation));
            (command, Update::reset())
        }),
        Action::Back => shared.act(|chat| (None, chat.deselect())),
        Action::SelectRelative { by } => {
            shared.act(|chat| (chat.select_relative(by as isize).flatten(), Update::reset()))
        }
        Action::SelectNextUnread => {
            shared.act(|chat| (chat.select_next_unread().flatten(), Update::reset()))
        }
        Action::SelectPinned { n } => {
            shared.act(|chat| (chat.select_pinned(n as usize).flatten(), Update::reset()))
        }
        Action::Type { text } => {
            // The state takes the text, and the composer shows it: a typist's keystrokes
            // would be in the field already.
            let typed = shared.act(|chat| (chat.draft_changed(&text), Update::default()));
            if let Some(window) = shared.ui() {
                window.set_draft(&text);
                shared.refresh(Update::default());
            }
            typed
        }
        Action::Send => {
            // The send button sends the composer's text; the harness keeps it in the draft.
            let sent = shared.act(|chat| {
                let text = chat.draft().to_string();
                chat.compose(&text)
            });
            // As the button does: the composer is empty once sent.
            if let Some(window) = shared.ui() {
                window.set_draft(shared.chat.borrow().draft());
            }
            sent
        }
        Action::Edit { index } => shared.act(|chat| {
            if let Some(text) = chat.edit(index as usize) {
                chat.draft_changed(&text);
            }
            (None, Update::default())
        }),
        Action::CancelEdit => shared.act(|chat| {
            chat.cancel_edit();
            (None, Update::default())
        }),
        Action::ReplyTo { index } => shared.act(|chat| (None, chat.reply_to(index as usize))),
        Action::CancelReply => shared.act(|chat| {
            chat.cancel_reply();
            (None, Update::default())
        }),
        Action::React { index, emoji } => {
            shared.act(|chat| (chat.react(index as usize, &emoji), Update::default()))
        }
        Action::Delete { index, everyone } => {
            shared.act(|chat| chat.delete(index as usize, everyone))
        }
        Action::ToggleSelect { index } => {
            shared.act(|chat| (None, chat.toggle_selected(index as usize)))
        }
        Action::SelectThrough { index } => {
            shared.act(|chat| (None, chat.select_through(index as usize)))
        }
        Action::ClearSelection => shared.act(|chat| (None, chat.clear_selection())),
        Action::ForwardSelection => shared.act(|chat| {
            chat.start_forward_selection();
            (None, Update::default())
        }),
        Action::DeleteSelection { everyone } => shared.act(|chat| chat.delete_selection(everyone)),
        Action::TogglePin { conversation } => {
            conversation_action(shared, conversation, Update::reset(), ChatState::toggle_pin)
        }
        Action::MovePin { conversation, by } => {
            conversation_action(shared, conversation, Update::reset(), |chat, id| {
                chat.move_pin_by(id, by)
            })
        }
        Action::MarkRead { conversation, read } => {
            conversation_action(shared, conversation, Update::reset(), |chat, id| {
                chat.mark_unread(id, !read)
            })
        }
        Action::Mute {
            conversation,
            minutes,
        } => conversation_action(shared, conversation, Update::reset(), |chat, id| {
            let until = match minutes {
                0 => None,
                ..0 => Some(u64::MAX),
                minutes => Some(b33p_core::now_millis() + minutes as u64 * 60_000),
            };
            chat.mute(id, until)
        }),
        Action::Archive {
            conversation,
            archived,
        } => conversation_action(shared, conversation, Update::reset(), |chat, id| {
            chat.archive(id, archived)
        }),
        Action::Block {
            conversation,
            blocked,
        } => conversation_action(shared, conversation, Update::reset(), |chat, id| {
            chat.block(id, blocked)
        }),
        Action::SetTimer {
            conversation,
            seconds,
        } => conversation_action(shared, conversation, Update::reset(), |chat, id| {
            chat.set_timer(id, seconds)
        }),
        Action::AnswerInvitation {
            conversation,
            accept,
        } => {
            let edit = if accept {
                GroupEdit::AcceptInvitation
            } else {
                GroupEdit::DeclineInvitation
            };
            shared.act(|chat| {
                (
                    chat.edit_group(&ConversationId(conversation), edit),
                    Update::default(),
                )
            })
        }
        Action::ConfirmSafetyNumber { conversation } => shared.act(|chat| {
            (
                chat.confirm_safety_number(&ConversationId(conversation)),
                Update::default(),
            )
        }),
        Action::Respond { answer } => {
            use b33p_core::RequestResponse;
            let response = match answer {
                Answer::Accept => RequestResponse::Accept,
                Answer::Delete => RequestResponse::Delete,
                Answer::Block => RequestResponse::Block,
            };
            shared.act(|chat| {
                let id = chat.selected().map(|c| c.id.clone());
                (
                    id.and_then(|id| chat.respond(&id, response)),
                    Update::reset(),
                )
            })
        }
        Action::RenameGroup { title } => selected(shared, |id, chat| {
            chat.edit_group(id, GroupEdit::Title(title))
        }),
        Action::SetGroupPicture { path } => selected(shared, |id, chat| {
            chat.edit_group(
                id,
                match path {
                    Some(path) => GroupEdit::SetPicture(path.into()),
                    None => GroupEdit::RemovePicture,
                },
            )
        }),
        Action::DescribeGroup { description } => selected(shared, |id, chat| {
            chat.edit_group(id, GroupEdit::Description(description))
        }),
        Action::SetMemberAdmin { member, admin } => selected(shared, |id, chat| {
            chat.edit_group(id, GroupEdit::SetAdmin(member, admin))
        }),
        Action::RemoveMember { member } => selected(shared, |id, chat| {
            chat.edit_group(id, GroupEdit::RemoveMember(member))
        }),
        Action::LeaveGroup => selected(shared, |id, chat| chat.edit_group(id, GroupEdit::Leave)),
        Action::CreateGroup { title, members } => shared.act(|chat| {
            let members = members.into_iter().map(ConversationId).collect::<Vec<_>>();
            (chat.create_group(&title, &members), Update::default())
        }),
        Action::FindNumber { number, open } => {
            let then = if open {
                AfterFinding::Open
            } else {
                AfterFinding::Choose
            };
            shared.act(|chat| (chat.find_number(&number, then), Update::default()))
        }
        Action::OpenDetails => shared.act(|chat| (chat.open_details(), Update::default())),
        Action::CloseDetails => shared.act(|chat| {
            chat.close_details();
            (None, Update::default())
        }),
        Action::OpenMedia => shared.act(|chat| (chat.open_media(), Update::default())),
        Action::CloseMedia => shared.act(|chat| {
            chat.close_media();
            (None, Update::default())
        }),
        Action::OpenStory { conversation } => shared.act(|chat| {
            (
                chat.open_story(&ConversationId(conversation)),
                Update::default(),
            )
        }),
        Action::StepStory { by } => {
            shared.act(|chat| (chat.step_story(by as isize), Update::default()))
        }
        Action::CloseStory => shared.act(|chat| {
            chat.close_story();
            (None, Update::default())
        }),
        Action::PostStoryText { group, text } => shared.act(|chat| {
            let story = b33p_core::NewStory::Text(text);
            (
                chat.send_story(&ConversationId(group), story),
                Update::default(),
            )
        }),
        Action::OpenNewGroup => shared.act(|chat| {
            chat.open_group_draft(None);
            (None, Update::default())
        }),
        Action::OpenJoinGroup { link } => {
            shared.act(|chat| (chat.open_join(link.as_deref()), Update::default()))
        }
        Action::PreviewGroupLink { link } => {
            shared.act(|chat| (chat.preview_link(&link), Update::default()))
        }
        Action::JoinGroup => shared.act(|chat| (chat.join_group(), Update::default())),
        Action::CloseJoinGroup => shared.act(|chat| {
            chat.close_join();
            (None, Update::default())
        }),
        Action::SetInviteLink { access } => {
            let access = match access.as_str() {
                "anyone" => b33p_core::InviteLinkAccess::Anyone,
                "approval" => b33p_core::InviteLinkAccess::Approval,
                _ => b33p_core::InviteLinkAccess::Off,
            };
            selected(shared, move |id, chat| {
                chat.edit_group(id, GroupEdit::InviteLink(access))
            })
        }
        Action::ResetInviteLink => selected(shared, |id, chat| {
            chat.edit_group(id, GroupEdit::ResetInviteLink)
        }),
        Action::AnswerJoinRequest { member, approve } => selected(shared, |id, chat| {
            let edit = if approve {
                GroupEdit::ApproveRequest(member)
            } else {
                GroupEdit::DenyRequest(member)
            };
            chat.edit_group(id, edit)
        }),
        Action::CancelJoinRequest => selected(shared, |id, chat| {
            chat.edit_group(id, GroupEdit::CancelJoinRequest)
        }),
        Action::CloseGroupDraft => shared.act(|chat| {
            chat.close_group_draft();
            (None, Update::default())
        }),
        Action::FinishGroupDraft { title } => {
            shared.act(|chat| (chat.finish_group_draft(&title), Update::default()))
        }
        Action::ToggleCandidate { conversation } => shared.act(|chat| {
            chat.toggle_group_member(&ConversationId(conversation));
            (None, Update::default())
        }),
        Action::OpenChatSearch => shared.act(|chat| {
            chat.open_chat_search();
            (None, Update::default())
        }),
        Action::CloseChatSearch => shared.act(|chat| {
            chat.close_chat_search();
            (None, Update::default())
        }),
        Action::SearchChat { text } => {
            shared.act(|chat| (chat.search_chat(&text), Update::default()))
        }
        Action::ShowResult { index } => shared.act(|chat| (None, chat.show_result(index as usize))),
        Action::Search { text } => shared.act(|chat| (None, chat.search(&text))),
        Action::Forward { index } => shared.act(|chat| {
            chat.start_forward(index as usize);
            (None, Update::default())
        }),
        Action::ForwardTo { conversation } => shared.act(|chat| {
            (
                chat.forward_to(&ConversationId(conversation)),
                Update::default(),
            )
        }),
        Action::CancelForward => shared.act(|chat| {
            chat.cancel_forward();
            (None, Update::default())
        }),
        Action::LoadOlder => shared.act(|chat| (chat.load_older(), Update::default())),
        Action::ShowFolder { index } => {
            shared.act(|chat| (None, chat.show_folder(usize::try_from(index).ok())))
        }
        Action::ShowArchived { show } => shared.act(|chat| (None, chat.show_archived(show))),
        Action::DismissError => shared.act(|chat| {
            chat.dismiss_error();
            (None, Update::default())
        }),
        Action::Unlock {
            password,
            confirmation,
        } => shared.act(|chat| {
            (
                chat.unlock(&password, &confirmation),
                Update {
                    screen: true,
                    ..Update::default()
                },
            )
        }),
        Action::SkipHistory => shared.act(|chat| (chat.skip_history(), Update::default())),
        Action::Retry => shared.act(|chat| {
            (
                chat.retry(),
                Update {
                    screen: true,
                    ..Update::default()
                },
            )
        }),
        Action::UsePassword => shared.act(|chat| {
            (
                chat.use_password(),
                Update {
                    screen: true,
                    ..Update::default()
                },
            )
        }),
        Action::StartOver => shared.act(|chat| {
            (
                chat.start_over(),
                Update {
                    screen: true,
                    ..Update::reset()
                },
            )
        }),
        Action::Unlink => shared.act(|chat| {
            (
                chat.unlink(),
                Update {
                    screen: true,
                    ..Update::reset()
                },
            )
        }),
        Action::StageFiles { paths } => shared.act(|chat| {
            chat.stage_files(paths.into_iter().map(PathBuf::from));
            (None, Update::default())
        }),
        Action::Unstage { index } => shared.act(|chat| {
            chat.unstage(index as usize);
            (None, Update::default())
        }),
        Action::RetrySend { index } => shared.act(|chat| {
            (
                chat.retry_send(index),
                Update {
                    rows: true,
                    ..Update::default()
                },
            )
        }),
        Action::FetchAttachment { id } => shared.act(|chat| {
            let command = chat.fetch_attachment(&b33p_core::AttachmentId(id));
            let rows = command.is_some();
            (
                command,
                Update {
                    rows,
                    ..Update::default()
                },
            )
        }),
        Action::OpenAttachment { id } => shared.act(|chat| {
            (
                chat.open_attachment(&b33p_core::AttachmentId(id)),
                Update::default(),
            )
        }),
        Action::SetWindowFocused { focused } => shared.act(|chat| {
            chat.set_window_focused(focused);
            (None, Update::default())
        }),
        Action::RenameDevice { name } => shared.rename_device(&name),
        Action::SetSettings {
            theme,
            style,
            text_size,
            animations,
            show_avatars,
            notifications,
            notification_content,
            show_activity,
            show_calendar,
            close_to_tray,
            read_receipts,
            typing_indicators,
            link_previews,
            sync_to_phone,
        } => {
            let mut settings = shared.settings.borrow().clone();
            if let Some(key) = theme {
                settings.theme = of_key(&key, Theme::from_key, "theme")?;
            }
            if let Some(key) = style {
                settings.style = of_key(&key, WidgetStyle::from_key, "style")?;
            }
            if let Some(key) = text_size {
                settings.text_size = of_key(&key, TextSize::from_key, "text_size")?;
            }
            if let Some(key) = animations {
                settings.animations = of_key(&key, Animations::from_key, "animations")?;
            }
            if let Some(key) = notification_content {
                settings.notification_content =
                    of_key(&key, NotificationContent::from_key, "notification_content")?;
            }
            if let Some(value) = show_avatars {
                settings.show_avatars = value;
            }
            if let Some(value) = notifications {
                settings.notifications = value;
            }
            if let Some(value) = show_activity {
                settings.show_activity = value;
            }
            if let Some(value) = show_calendar {
                settings.show_calendar = value;
            }
            if let Some(value) = close_to_tray {
                settings.close_to_tray = value;
            }
            if let Some(value) = read_receipts {
                settings.preferences.read_receipts = value;
            }
            if let Some(value) = typing_indicators {
                settings.preferences.typing_indicators = value;
            }
            if let Some(value) = link_previews {
                settings.preferences.link_previews = value;
            }
            if let Some(value) = sync_to_phone {
                settings.preferences.sync_to_phone = value;
            }
            // As the settings screen does: the new values go on the window (fonts, colors),
            // are saved, and reach the backend.
            if let Some(window) = shared.ui() {
                window.show_settings(&settings, &shared.style_note());
            }
            shared.apply_settings(settings);
            true
        }
    })
}

/// An action on one conversation, whichever chat it is in.
fn conversation_action(
    shared: &Rc<Shared>,
    id: String,
    update: Update,
    action: impl FnOnce(&mut ChatState, &ConversationId) -> Option<Command>,
) -> bool {
    shared.act(|chat| {
        let command = action(chat, &ConversationId(id));
        (command, update)
    })
}

/// An action on the open conversation, as the window's own buttons in it do.
fn selected(
    shared: &Rc<Shared>,
    action: impl FnOnce(&ConversationId, &mut ChatState) -> Option<Command>,
) -> bool {
    shared.act(|chat| {
        let id = chat.selected().map(|c| c.id.clone());
        (id.and_then(|id| action(&id, chat)), Update::default())
    })
}

fn of_key<T>(key: &str, parse: fn(&str) -> Option<T>, what: &str) -> Result<T, String> {
    parse(key).ok_or_else(|| format!("unknown {what}: {key}"))
}

/// What the window shows, as data. Shaped like what `ui.rs` reads from the state, not like
/// the Slint tree: the harness reports meaning, not widgets. Messages keep their position as
/// `index`, since actions on them (`react`, `edit`, …) take it.
#[derive(Serialize)]
struct Snapshot {
    mock: bool,
    screen: ScreenSnapshot,
    account: String,
    synced: bool,
    error: Option<String>,
    unread_total: u32,
    /// Only a direct `snapshot` answer has it; pushed updates don't look at the window.
    window: Option<WindowSnapshot>,
    conversations: Vec<ConversationSnapshot>,
    selected: Option<String>,
    messages: Vec<MessageSnapshot>,
    draft: String,
    replying_to: Option<QuoteSnapshot>,
    editing: bool,
    staged: Vec<String>,
    typing: Vec<String>,
    /// The join-a-group dialog, while open.
    join: Option<JoinSnapshot>,
    /// The story shown, while one is.
    story: Option<StorySnapshot>,
    settings: SettingsSnapshot,
}

#[derive(Serialize)]
struct StorySnapshot {
    author: String,
    /// Which of the author's stories it is, counted from 1, and how many there are.
    position: usize,
    count: usize,
    /// A text story's text; a picture story has none.
    text: Option<String>,
}

#[derive(Serialize)]
struct JoinSnapshot {
    link: String,
    /// The link is being looked up, or the group joined.
    busy: bool,
    /// What the link showed of its group, once looked up.
    title: Option<String>,
    members: u32,
    approval: bool,
    requested: bool,
}

#[derive(Serialize)]
#[serde(tag = "name", rename_all = "snake_case")]
enum ScreenSnapshot {
    /// Starting up, with what the screen says it waits for.
    Syncing {
        status: String,
    },
    Password {
        new: bool,
        error: Option<String>,
    },
    Link,
    Chats,
    /// See [`b33p_core::Event::Failed`].
    Failed {
        failure: String,
        message: String,
    },
}

#[derive(Serialize)]
struct WindowSnapshot {
    visible: bool,
    active: bool,
}

#[derive(Serialize)]
struct ConversationSnapshot {
    id: String,
    title: String,
    subtitle: String,
    group: bool,
    note_to_self: bool,
    unread: u32,
    marked_unread: bool,
    pinned: bool,
    muted: bool,
    archived: bool,
    blocked: bool,
    accepted: bool,
    request: bool,
    invited: bool,
    left: bool,
    /// We asked to join through the group's link, and wait for an admin.
    requested: bool,
    expire_timer: u32,
    preview: String,
    /// How far our last message got; `None` without one of ours.
    last_delivery: Option<&'static str>,
    typing: Vec<String>,
}

#[derive(Serialize)]
struct MessageSnapshot {
    index: usize,
    sender: String,
    sender_id: String,
    text: String,
    outgoing: bool,
    notice: bool,
    deleted: bool,
    edited: bool,
    timestamp: u64,
    delivery: Option<&'static str>,
    day: Option<String>,
    first_unread: bool,
    selected: bool,
    reactions: Vec<ReactionSnapshot>,
    quote: Option<QuoteSnapshot>,
    attachments: Vec<AttachmentSnapshot>,
}

#[derive(Serialize)]
struct ReactionSnapshot {
    emoji: String,
    sender: String,
    outgoing: bool,
}

#[derive(Serialize)]
struct QuoteSnapshot {
    author: String,
    text: String,
}

#[derive(Serialize)]
struct AttachmentSnapshot {
    id: String,
    kind: &'static str,
    file_name: Option<String>,
    size: u64,
    caption: Option<String>,
}

#[derive(Serialize)]
struct SettingsSnapshot {
    theme: &'static str,
    style: &'static str,
    text_size: &'static str,
    animations: &'static str,
    show_avatars: bool,
    notifications: bool,
    notification_content: &'static str,
    show_activity: bool,
    show_calendar: bool,
    close_to_tray: bool,
    read_receipts: bool,
    typing_indicators: bool,
    link_previews: bool,
    sync_to_phone: bool,
    device_name: String,
}

fn snapshot(chat: &ChatState, settings: &Settings, window: Option<&ui::Window>) -> Snapshot {
    let conversations = chat
        .conversations()
        .map(|view| ConversationSnapshot {
            id: view.conversation.id.0.clone(),
            title: view.conversation.title.clone(),
            subtitle: view.conversation.subtitle.clone(),
            group: view.conversation.is_group(),
            note_to_self: view.conversation.note_to_self,
            unread: view.unread,
            marked_unread: view.marked_unread(),
            pinned: view.pinned(),
            muted: view.muted(),
            archived: view.conversation.settings.archived,
            blocked: view.conversation.settings.blocked,
            accepted: view.conversation.settings.accepted,
            request: chat.is_request(view.conversation),
            invited: view.conversation.invited,
            left: view.conversation.left,
            requested: view.conversation.requested,
            expire_timer: view.conversation.expire_timer,
            preview: view.preview(),
            last_delivery: view.last_message.filter(|m| m.outgoing).map(delivery),
            typing: view.typing.iter().map(|name| name.to_string()).collect(),
        })
        .collect();
    let messages = chat
        .messages()
        .enumerate()
        .map(|(index, view)| MessageSnapshot {
            index,
            sender: view.message.sender.clone(),
            sender_id: view.message.sender_id.clone(),
            text: view.body.to_string(),
            outgoing: view.message.outgoing,
            notice: view.message.notice,
            deleted: view.message.deleted,
            edited: view.message.edited_at.is_some(),
            timestamp: view.message.timestamp,
            delivery: view.message.outgoing.then(|| delivery(view.message)),
            day: view.day.clone(),
            first_unread: view.first_unread,
            selected: view.selected,
            reactions: view
                .message
                .reactions
                .iter()
                .map(|reaction| ReactionSnapshot {
                    emoji: reaction.emoji.clone(),
                    sender: reaction.sender.clone(),
                    outgoing: reaction.outgoing,
                })
                .collect(),
            quote: view.message.quote.as_ref().map(|quote| QuoteSnapshot {
                author: quote.author.clone(),
                text: quote.text.clone(),
            }),
            attachments: view
                .message
                .attachments
                .iter()
                .map(|attachment| AttachmentSnapshot {
                    id: attachment.id.0.clone(),
                    kind: kind_name(attachment.kind),
                    file_name: attachment.file_name.clone(),
                    size: attachment.size,
                    caption: attachment.caption.clone(),
                })
                .collect(),
        })
        .collect();
    Snapshot {
        mock: Backend::mock_requested(),
        screen: match chat.screen() {
            state::Screen::Syncing(stage) => ScreenSnapshot::Syncing {
                status: ui::sync_status(*stage),
            },
            state::Screen::Password { new, error } => ScreenSnapshot::Password {
                new: *new,
                error: error.clone(),
            },
            state::Screen::Link(_) => ScreenSnapshot::Link,
            state::Screen::Chats => ScreenSnapshot::Chats,
            state::Screen::Failed { failure, message } => ScreenSnapshot::Failed {
                failure: format!("{failure:?}"),
                message: message.clone(),
            },
        },
        account: chat.account().to_string(),
        synced: chat.synced(),
        error: chat.error().map(str::to_string),
        unread_total: chat.unread_total(),
        window: window.map(|window| WindowSnapshot {
            visible: window.visible(),
            active: window.active(),
        }),
        conversations,
        selected: chat.selected().map(|c| c.id.0.clone()),
        messages,
        draft: chat.draft().to_string(),
        replying_to: chat.replying().map(|quote| QuoteSnapshot {
            author: quote.author.clone(),
            text: quote.text.clone(),
        }),
        editing: chat.editing().is_some(),
        staged: chat
            .staged()
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        typing: chat
            .selected()
            .map(|c| chat.typing(&c.id).into_iter().map(str::to_string).collect())
            .unwrap_or_default(),
        story: chat
            .current_story()
            .map(|(story, at, count)| StorySnapshot {
                author: story.author.clone(),
                position: at + 1,
                count,
                text: match &story.content {
                    b33p_core::StoryContent::Text { text, .. } => Some(text.clone()),
                    b33p_core::StoryContent::Picture(_) => None,
                },
            }),
        join: chat.join().map(|(link, preview, busy)| JoinSnapshot {
            link: link.to_string(),
            busy,
            title: preview.map(|p| p.title.clone()),
            members: preview.map_or(0, |p| p.members),
            approval: preview.is_some_and(|p| p.approval),
            requested: preview.is_some_and(|p| p.requested),
        }),
        settings: SettingsSnapshot {
            theme: settings.theme.key(),
            style: settings.style.key(),
            text_size: settings.text_size.key(),
            animations: settings.animations.key(),
            show_avatars: settings.show_avatars,
            notifications: settings.notifications,
            notification_content: settings.notification_content.key(),
            show_activity: settings.show_activity,
            show_calendar: settings.show_calendar,
            close_to_tray: settings.close_to_tray,
            read_receipts: settings.preferences.read_receipts,
            typing_indicators: settings.preferences.typing_indicators,
            link_previews: settings.preferences.link_previews,
            sync_to_phone: settings.preferences.sync_to_phone,
            device_name: settings.device_name(),
        },
    }
}

fn delivery(message: &b33p_core::ChatMessage) -> &'static str {
    match message.unsent {
        Some(b33p_core::Unsent::Sending) => "sending",
        Some(b33p_core::Unsent::Failed) => "failed",
        None => match message.delivery {
            b33p_core::Delivery::Sent => "sent",
            b33p_core::Delivery::Delivered => "delivered",
            b33p_core::Delivery::Read => "read",
        },
    }
}

fn kind_name(kind: b33p_core::AttachmentKind) -> &'static str {
    use b33p_core::AttachmentKind as Kind;
    match kind {
        Kind::Image => "image",
        Kind::Video => "video",
        Kind::Audio => "audio",
        Kind::VoiceNote => "voice_note",
        Kind::Sticker => "sticker",
        Kind::LongText => "long_text",
        Kind::ViewOnce => "view_once",
        Kind::File => "file",
    }
}

/// Who is subscribed to updates, and how far they have been pushed. Only the UI thread
/// touches this: the pump answers, and every redraw publishes.
pub struct Handle(RefCell<Option<Inner>>);

impl Default for Handle {
    fn default() -> Self {
        Self(RefCell::new(Some(Inner {
            seq: 0,
            subscribers: HashMap::new(),
        })))
    }
}

struct Inner {
    seq: u64,
    subscribers: HashMap<u64, Outbound>,
}

impl Handle {
    /// Pushes the current state to subscribed agents. Cheap while nobody listens: nothing is
    /// serialized. A subscriber that stopped reading is dropped.
    pub fn publish(&self, chat: &ChatState, settings: &Settings, window: Option<&ui::Window>) {
        let mut inner = self.0.borrow_mut();
        let Some(inner) = inner.as_mut() else { return };
        if inner.subscribers.is_empty() {
            return;
        }
        let snapshot = serde_json::to_value(snapshot(chat, settings, window))
            .expect("a snapshot always serializes");
        inner.seq += 1;
        let line = json!({ "event": "update", "seq": inner.seq, "snapshot": snapshot });
        inner
            .subscribers
            .retain(|_, out| out.try_send(line.to_string()).is_ok());
    }

    fn subscribe(&self, connection: u64, out: Outbound) -> u64 {
        let mut inner = self.0.borrow_mut();
        let Some(inner) = inner.as_mut() else {
            return 0;
        };
        inner.subscribers.insert(connection, out);
        inner.seq
    }

    fn unsubscribe(&self, connection: u64) {
        let mut inner = self.0.borrow_mut();
        if let Some(inner) = inner.as_mut() {
            inner.subscribers.remove(&connection);
        }
    }
}
