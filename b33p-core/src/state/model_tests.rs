//! The Slint UI keeps its message rows in sync only through the `Update`s that `apply` and the
//! actions return (b33p-slint/src/ui.rs `render`), merged per batch of events (main.rs). This
//! replays that bookkeeping on a plain Vec and checks it against `messages()`.
use super::*;
use crate::{AfterFinding, ConversationKind, Delivery, Reaction};
use proptest::prelude::*;
use std::sync::OnceLock;

const CONVS: [&str; 2] = ["a", "g"];
const SENDERS: [&str; 3] = ["alice", "bob", "me"];

fn base() -> u64 {
    static BASE: OnceLock<u64> = OnceLock::new();
    // An hour ago, on the minute: recent enough to edit and delete for everyone.
    *BASE.get_or_init(|| (now_millis() - 3_600_000) / 60_000 * 60_000)
}

fn ts(t: u8) -> u64 {
    base() + u64::from(t) * 1000
}

fn cid(c: usize) -> ConversationId {
    ConversationId(CONVS[c].into())
}

fn msg(c: usize, sender: usize, t: u8) -> ChatMessage {
    ChatMessage {
        conversation: cid(c),
        sender_id: SENDERS[sender].into(),
        sender: SENDERS[sender].into(),
        body: format!("{}@{t}", SENDERS[sender]),
        timestamp: ts(t),
        outgoing: sender == 2,
        // Some with a photo, to download and save.
        attachments: if t.is_multiple_of(3) {
            vec![photo(c, sender, t)]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

fn photo(c: usize, sender: usize, t: u8) -> Attachment {
    Attachment {
        id: AttachmentId(format!("{c}/{}@{t}", SENDERS[sender])),
        kind: AttachmentKind::Image,
        content_type: "image/png".into(),
        file_name: None,
        size: 1_000,
        width: 1,
        height: 1,
        blurhash: None,
        caption: None,
        gif: false,
        pointer: Arc::from(Vec::new()),
    }
}

fn target(sender: usize, t: u8) -> MessageRef {
    MessageRef {
        author_id: SENDERS[sender].into(),
        timestamp: ts(t),
    }
}

#[derive(Clone, Debug)]
enum Ev {
    Msg(usize, usize, u8),
    Latest(usize, usize, u8),
    History {
        conv: usize,
        msgs: Vec<(usize, u8)>,
        older: Option<u8>,
    },
    Edited(usize, usize, u8),
    Deleted(usize, usize, u8),
    Expiring(usize, usize, u8),
    Expired(usize, Vec<(usize, u8)>),
    Cleared(usize),
    Delivered(usize, Vec<u8>),
    Reaction {
        conv: usize,
        sender: usize,
        t: u8,
        from: usize,
        remove: bool,
    },
    Sent(usize),
    SendFailed(usize),
    Found(usize),
    /// A picture of alice or bob, or none.
    Avatar(usize, bool),
    /// A photo downloaded, or not.
    Attachment(usize, usize, u8, bool),
    /// A photo saved, or not.
    Saved(usize, usize, u8, bool),
    /// Something for a message of the open chat, by its row (wrapping around): the
    /// others mostly miss.
    AtRow(usize, RowEv),
}

#[derive(Clone, Debug)]
enum RowEv {
    Reaction(usize, bool),
    Edited,
    Deleted,
    Expiring,
    Delivered,
    Attachment(bool),
    Saved(bool),
}

#[derive(Clone, Debug)]
enum Op {
    Batch(Vec<Ev>),
    Select(usize),
    Deselect,
    Compose,
    LoadOlder,
    Delete(usize, bool),
    Edit(usize),
    ReplyTo(usize),
    Reveal(usize),
    NotificationReply(usize),
    Archive,
    Pick(usize),
    PickThrough(usize),
    ClearPicks,
    /// Forwards the picked messages to a chat.
    ForwardPicked(usize),
    /// Finds a message by searching the open chat, and shows it.
    ShowResult(usize, u8),
    /// Downloads the photo of a row, as one coming into view does.
    Fetch(usize),
    Save(usize),
    /// Sends a message that failed to go again.
    RetrySend,
    /// Several, drawn once, as `Shared::refresh` merges what a turn of the event loop
    /// brings.
    Turn(Vec<Op>),
}

fn ev() -> impl Strategy<Value = Ev> {
    let conv = 0usize..2;
    let sender = 0usize..3;
    let t = 0u8..12;
    prop_oneof![
        6 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Msg(c, s, t)),
        1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Latest(c, s, t)),
        4 => (conv.clone(), prop::collection::vec((sender.clone(), t.clone()), 0..5), prop::option::of(t.clone()))
            .prop_map(|(conv, msgs, older)| Ev::History { conv, msgs, older }),
        1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Edited(c, s, t)),
        1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Deleted(c, s, t)),
        1 => (conv.clone(), sender.clone(), t.clone()).prop_map(|(c, s, t)| Ev::Expiring(c, s, t)),
        1 => (conv.clone(), prop::collection::vec((sender.clone(), t.clone()), 0..3)).prop_map(|(c, v)| Ev::Expired(c, v)),
        1 => conv.clone().prop_map(Ev::Cleared),
        1 => (conv.clone(), prop::collection::vec(t.clone(), 0..3)).prop_map(|(c, v)| Ev::Delivered(c, v)),
        1 => (conv.clone(), sender.clone(), t.clone(), sender.clone(), any::<bool>())
            .prop_map(|(conv, sender, t, from, remove)| Ev::Reaction { conv, sender, t, from, remove }),
        2 => (0usize..4).prop_map(Ev::Sent),
        2 => (0usize..4).prop_map(Ev::SendFailed),
        1 => conv.clone().prop_map(Ev::Found),
        1 => (0usize..2, any::<bool>()).prop_map(|(s, some)| Ev::Avatar(s, some)),
        1 => (conv.clone(), sender.clone(), t.clone(), any::<bool>())
            .prop_map(|(c, s, t, ok)| Ev::Attachment(c, s, t, ok)),
        1 => (conv, sender.clone(), t, any::<bool>()).prop_map(|(c, s, t, ok)| Ev::Saved(c, s, t, ok)),
        6 => (0usize..8, row_ev(sender)).prop_map(|(row, ev)| Ev::AtRow(row, ev)),
    ]
}

fn row_ev(sender: std::ops::Range<usize>) -> impl Strategy<Value = RowEv> {
    prop_oneof![
        (sender, any::<bool>()).prop_map(|(from, remove)| RowEv::Reaction(from, remove)),
        Just(RowEv::Edited),
        Just(RowEv::Deleted),
        Just(RowEv::Expiring),
        Just(RowEv::Delivered),
        any::<bool>().prop_map(RowEv::Attachment),
        any::<bool>().prop_map(RowEv::Saved),
    ]
}

fn one_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => prop::collection::vec(ev(), 1..5).prop_map(Op::Batch),
        3 => (0usize..2).prop_map(Op::Select),
        1 => Just(Op::Deselect),
        2 => Just(Op::Compose),
        2 => Just(Op::LoadOlder),
        2 => (0usize..8, any::<bool>()).prop_map(|(i, e)| Op::Delete(i, e)),
        1 => (0usize..8).prop_map(Op::Edit),
        1 => (0usize..8).prop_map(Op::ReplyTo),
        1 => (0usize..8).prop_map(Op::Reveal),
        1 => (0usize..2).prop_map(Op::NotificationReply),
        1 => Just(Op::Archive),
        1 => (0usize..8).prop_map(Op::Pick),
        1 => (0usize..8).prop_map(Op::PickThrough),
        1 => Just(Op::ClearPicks),
        1 => (0usize..2).prop_map(Op::ForwardPicked),
        1 => (0usize..3, 0u8..12).prop_map(|(s, t)| Op::ShowResult(s, t)),
        2 => (0usize..8).prop_map(Op::Fetch),
        1 => (0usize..8).prop_map(Op::Save),
        2 => Just(Op::RetrySend),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => one_op(),
        1 => prop::collection::vec(one_op(), 2..5).prop_map(Op::Turn),
    ]
}

/// What a row of the UI's model shows: everything of a message that can change in place.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    timestamp: u64,
    sender_id: String,
    body: String,
    edited: bool,
    unsent: Option<Unsent>,
    delivery: Delivery,
    deleted: bool,
    /// Who reacted with what, in order.
    reactions: Vec<(String, String)>,
    expires_at: Option<u64>,
    /// Where each attachment's download and saving stand.
    attachments: Vec<(Option<&'static str>, Option<&'static str>)>,
    revealed: bool,
    highlighted: bool,
    selected: bool,
    first_unread: bool,
    /// Which picture, by address.
    sender_avatar: Option<usize>,
    // Which depend on the neighbours.
    day: Option<String>,
    first_of_run: bool,
    last_of_run: bool,
    show_sender: bool,
}

fn row(chat: &ChatState, view: &MessageView) -> Row {
    let m = view.message;
    let download = |id| {
        chat.attachment(id).map(|state| match state {
            AttachmentState::Loading => "loading",
            AttachmentState::Ready(_) => "ready",
            AttachmentState::Failed(_) => "failed",
        })
    };
    let save = |id| {
        chat.saved(id).map(|state| match state {
            SaveState::Saving => "saving",
            SaveState::Saved(_) => "saved",
            SaveState::Failed(_) => "failed",
        })
    };
    Row {
        timestamp: m.timestamp,
        sender_id: m.sender_id.clone(),
        body: view.body.to_owned(),
        edited: m.edited_at.is_some(),
        unsent: m.unsent,
        delivery: m.delivery,
        deleted: m.deleted,
        reactions: m
            .reactions
            .iter()
            .map(|r| (r.sender_id.clone(), r.emoji.clone()))
            .collect(),
        expires_at: m.expires_at,
        attachments: m
            .attachments
            .iter()
            .map(|a| (download(&a.id), save(&a.id)))
            .collect(),
        revealed: view.revealed,
        highlighted: view.highlighted,
        selected: view.selected,
        first_unread: view.first_unread,
        sender_avatar: view.sender_avatar.map(|p| Arc::as_ptr(p) as usize),
        day: view.day.clone(),
        first_of_run: view.first_of_run,
        last_of_run: view.last_of_run,
        show_sender: view.show_sender,
    }
}

/// ui.rs `render`, for the message rows: a Slint `VecModel` ignores `set_row_data` past its
/// end and panics on `insert` past it.
fn render(model: &mut Vec<Row>, chat: &ChatState, update: Update) {
    match update.messages {
        MessagesUpdate::Unchanged => {}
        MessagesUpdate::Appended(n) => {
            // The one before them may no longer end its run.
            if let Some(before) = model.len().checked_sub(1)
                && let Some(view) = chat.message_view(before)
            {
                model[before] = row(chat, &view);
            }
            model.extend(chat.last_messages(n).map(|m| row(chat, &m)));
        }
        MessagesUpdate::Prepended(n) => {
            let items: Vec<Row> = chat.messages().take(n + 1).map(|m| row(chat, &m)).collect();
            for (r, item) in items.into_iter().enumerate() {
                if r < n {
                    model.insert(r, item);
                } else if r < model.len() {
                    model[r] = item;
                }
            }
        }
        MessagesUpdate::Reset => *model = chat.messages().map(|m| row(chat, &m)).collect(),
    }
    let changed = chat.take_changed_rows();
    if update.messages != MessagesUpdate::Reset {
        let rows: Vec<usize> = match changed {
            None if update.rows => (0..model.len()).collect(),
            None => Vec::new(),
            // ui.rs `rows_around`.
            Some(changed) => {
                let mut rows: Vec<usize> = changed
                    .iter()
                    .filter_map(|message| chat.row_of(message))
                    .flat_map(|row| row.saturating_sub(1)..=row + 1)
                    .collect();
                rows.sort_unstable();
                rows.dedup();
                rows
            }
        };
        for r in rows {
            if r < model.len()
                && let Some(view) = chat.message_view(r)
            {
                model[r] = row(chat, &view);
            }
        }
    }
}

struct Sim {
    chat: ChatState,
    model: Vec<Row>,
    /// Sends shown while sending: conversation and timestamp.
    sending: Vec<(ConversationId, u64)>,
    log: Vec<String>,
}

impl Sim {
    fn new() -> Self {
        let mut chat = ChatState::default();
        chat.apply(Event::Linked {
            account: "me".into(),
        });
        chat.apply(Event::Synced);
        chat.apply(Event::Conversations(vec![
            Conversation {
                id: cid(0),
                title: "Alice".into(),
                settings: ChatSettings {
                    accepted: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            Conversation {
                id: cid(1),
                title: "Group".into(),
                kind: ConversationKind::Group,
                settings: ChatSettings {
                    accepted: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        ]));
        chat.set_window_focused(true);
        Sim {
            chat,
            model: Vec::new(),
            sending: Vec::new(),
            log: Vec::new(),
        }
    }

    fn event(&mut self, e: &Ev) -> Option<Event> {
        Some(match e.clone() {
            Ev::Msg(c, s, t) => Event::Message(msg(c, s, t)),
            Ev::Latest(c, s, t) => Event::Latest(msg(c, s, t)),
            Ev::History { conv, msgs, older } => Event::History {
                conversation: cid(conv),
                messages: {
                    let mut m: Vec<ChatMessage> =
                        msgs.iter().map(|&(s, t)| msg(conv, s, t)).collect();
                    m.sort_by_key(|m| (m.timestamp, m.sender_id.clone()));
                    m
                },
                reactions: Vec::new(),
                older: older.map(ts),
            },
            Ev::Edited(c, s, t) => Event::Edited {
                conversation: cid(c),
                target: target(s, t),
                message: ChatMessage {
                    body: "edited".into(),
                    ..msg(c, s, t)
                },
            },
            Ev::Deleted(c, s, t) => Event::Deleted {
                conversation: cid(c),
                target: target(s, t),
            },
            Ev::Expiring(c, s, t) => Event::Expiring {
                conversation: cid(c),
                expiries: vec![(target(s, t), now_millis() + 600_000)],
            },
            Ev::Expired(c, v) => Event::Expired {
                conversation: cid(c),
                targets: v.iter().map(|&(s, t)| target(s, t)).collect(),
            },
            Ev::Cleared(c) => Event::Cleared(cid(c)),
            Ev::Delivered(c, v) => Event::Delivered {
                conversation: cid(c),
                timestamps: v.iter().map(|&t| ts(t)).collect(),
                delivery: Delivery::Read,
            },
            Ev::Reaction {
                conv,
                sender,
                t,
                from,
                remove,
            } => Event::Reaction {
                conversation: cid(conv),
                target: target(sender, t),
                reaction: Reaction {
                    emoji: "👍".into(),
                    sender_id: SENDERS[from].into(),
                    sender: SENDERS[from].into(),
                    outgoing: from == 2,
                },
                remove,
            },
            Ev::Sent(i) => {
                if self.sending.is_empty() {
                    return None;
                }
                let (conversation, timestamp) = self.sending.remove(i % self.sending.len());
                Event::Message(ChatMessage {
                    conversation,
                    sender_id: "me".into(),
                    sender: "me".into(),
                    body: "sent".into(),
                    timestamp,
                    outgoing: true,
                    ..Default::default()
                })
            }
            Ev::SendFailed(i) => {
                if self.sending.is_empty() {
                    return None;
                }
                let (conversation, timestamp) = self.sending[i % self.sending.len()].clone();
                Event::SendFailed {
                    conversation,
                    timestamp,
                    retryable: true,
                }
            }
            Ev::Found(c) => Event::Found {
                conversation: cid(c),
                then: AfterFinding::Open,
            },
            Ev::Avatar(s, some) => Event::Avatar {
                conversation: ConversationId::contact(SENDERS[s]),
                avatar: some.then(|| {
                    Arc::new(Picture {
                        width: 1,
                        height: 1,
                        rgba: vec![0; 4],
                    })
                }),
            },
            Ev::Attachment(c, s, t, ok) => Event::Attachment {
                id: photo(c, s, t).id,
                content: match ok {
                    true => Ok(AttachmentContent::Picture(Arc::new(Picture {
                        width: 1,
                        height: 1,
                        rgba: vec![0; 4],
                    }))),
                    false => Err(crate::Error::unreachable("gone")),
                },
            },
            Ev::Saved(c, s, t, ok) => Event::AttachmentSaved {
                id: photo(c, s, t).id,
                path: match ok {
                    true => Ok("/tmp/photo.png".into()),
                    false => Err(crate::Error::unreachable("full")),
                },
            },
            Ev::AtRow(row, what) => {
                let rows = self.chat.messages().len();
                let m = self.chat.message_view(row % rows.max(1))?.message.clone();
                let (conversation, target) = (m.conversation.clone(), m.reference());
                let photo = m.attachments.first().map(|a| a.id.clone());
                match what {
                    RowEv::Reaction(from, remove) => Event::Reaction {
                        conversation,
                        target,
                        reaction: Reaction {
                            emoji: "❤".into(),
                            sender_id: SENDERS[from].into(),
                            sender: SENDERS[from].into(),
                            outgoing: from == 2,
                        },
                        remove,
                    },
                    RowEv::Edited => Event::Edited {
                        conversation,
                        target,
                        message: ChatMessage {
                            body: "edited".into(),
                            edited_at: None,
                            ..m
                        },
                    },
                    RowEv::Deleted => Event::Deleted {
                        conversation,
                        target,
                    },
                    RowEv::Expiring => Event::Expiring {
                        conversation,
                        expiries: vec![(target, now_millis() + 300_000)],
                    },
                    RowEv::Delivered => Event::Delivered {
                        conversation,
                        timestamps: vec![m.timestamp],
                        delivery: Delivery::Delivered,
                    },
                    RowEv::Attachment(ok) => Event::Attachment {
                        id: photo?,
                        content: match ok {
                            true => Ok(AttachmentContent::Text("photo".into())),
                            false => Err(crate::Error::unreachable("gone")),
                        },
                    },
                    RowEv::Saved(ok) => Event::AttachmentSaved {
                        id: photo?,
                        path: match ok {
                            true => Ok("/tmp/photo.png".into()),
                            false => Err(crate::Error::unreachable("full")),
                        },
                    },
                }
            }
        })
    }

    /// The selected chat's place in `CONVS`.
    fn selected(&self) -> Option<usize> {
        let id = self.chat.selected.as_ref()?;
        CONVS.iter().position(|c| *c == id.0)
    }

    /// The photo of the `i`th row, if it has one.
    fn photo_at(&self, i: usize) -> Option<AttachmentId> {
        let view = self.chat.message_view(i)?;
        Some(view.message.attachments.first()?.id.clone())
    }

    fn note_send(&mut self, command: &Option<Command>) {
        if let Some(Command::Send {
            conversation,
            timestamp,
            ..
        }) = command
        {
            self.sending.push((conversation.clone(), *timestamp));
        }
    }

    /// Does `op` and draws what it changed.
    fn run(&mut self, op: &Op) {
        let update = self.step(op);
        self.chat.take_commands();
        self.log.push(format!("{op:?} -> {update:?}"));
        render(&mut self.model, &self.chat, update);
        self.chat.assert_indexed();
    }

    /// Does `op`, saying what to draw as the UI's callbacks do.
    fn step(&mut self, op: &Op) -> Update {
        match op {
            Op::Batch(events) => {
                self.chat.set_window_focused(true);
                let mut update = Update::default();
                for e in events {
                    if let Some(event) = self.event(e) {
                        let noted = self.chat.changed_rows_noted();
                        let u = self.chat.apply(event.clone());
                        self.log
                            .push(format!("  apply {event:?} -> {:?}", u.messages));
                        // An event that changes rows in place names them, or says it
                        // is all of them: one that didn't would go unseen whenever
                        // another names its own in the same redraw.
                        assert!(
                            !u.rows
                                || u.messages == MessagesUpdate::Reset
                                || noted.1
                                || self.chat.changed_rows_noted() != noted,
                            "{event:?} redraws rows without naming them"
                        );
                        update = update.merge(u);
                    }
                }
                update
            }
            Op::Select(c) => {
                self.chat.select(&cid(*c));
                Update::reset()
            }
            Op::Deselect => self.chat.deselect(),
            Op::Compose => {
                let (command, update) = self.chat.compose("hello");
                self.note_send(&command);
                update
            }
            Op::LoadOlder => {
                self.chat.load_older();
                Update::default()
            }
            Op::Delete(i, everyone) => self.chat.delete(*i, *everyone).1,
            Op::Edit(i) => {
                self.chat.edit(*i);
                let (command, update) = self.chat.compose("changed");
                self.note_send(&command);
                update
            }
            Op::ReplyTo(i) => self.chat.reply_to(*i),
            Op::Reveal(i) => self.chat.reveal(*i),
            Op::NotificationReply(c) => {
                let (command, update) = self.chat.reply(&cid(*c), "thanks");
                self.note_send(&command);
                update.merge(Update {
                    conversations: true,
                    ..Update::default()
                })
            }
            Op::Archive => match self.chat.selected.clone() {
                Some(id) => {
                    self.chat.archive(&id, true);
                    // Unarchive right away so the chat can be selected again.
                    self.chat.archive(&id, false);
                    Update::reset()
                }
                None => Update::default(),
            },
            Op::Pick(i) => self.chat.toggle_selected(*i),
            Op::PickThrough(i) => self.chat.select_through(*i),
            Op::ClearPicks => self.chat.clear_selection(),
            Op::ForwardPicked(c) => {
                self.chat.start_forward_selection();
                self.chat.forward_to(&cid(*c));
                self.chat.cancel_forward();
                Update::default()
            }
            Op::ShowResult(s, t) => {
                let Some(c) = self.selected() else {
                    return Update::default();
                };
                self.chat.open_chat_search();
                self.chat.search_chat("found");
                self.chat.apply(Event::SearchResults {
                    conversation: cid(c),
                    query: "found".into(),
                    messages: vec![msg(c, *s, *t)],
                });
                self.chat.show_result(0)
            }
            Op::Fetch(i) => {
                let command = self
                    .photo_at(*i)
                    .and_then(|id| self.chat.fetch_attachment(&id));
                Update {
                    rows: command.is_some(),
                    ..Update::default()
                }
            }
            Op::Save(i) => {
                if let Some(id) = self.photo_at(*i) {
                    self.chat.save_attachment(&id);
                }
                Update {
                    rows: true,
                    ..Update::default()
                }
            }
            Op::RetrySend => {
                let failed = self
                    .chat
                    .messages()
                    .position(|m| m.message.unsent == Some(Unsent::Failed));
                if let Some(row) = failed {
                    self.chat.retry_send(row);
                }
                Update {
                    rows: true,
                    ..Update::default()
                }
            }
            Op::Turn(ops) => ops
                .iter()
                .fold(Update::default(), |update, op| update.merge(self.step(op))),
        }
    }

    fn shown(&self) -> Vec<(u64, String)> {
        self.model
            .iter()
            .map(|r| (r.timestamp, r.sender_id.clone()))
            .collect()
    }

    fn truth(&self) -> Vec<(u64, String)> {
        self.chat
            .messages()
            .map(|m| (m.message.timestamp, m.message.sender_id.clone()))
            .collect()
    }
}

fn rel(rows: &[(u64, String)]) -> Vec<String> {
    rows.iter()
        .map(|(t, s)| format!("{s}@{}", (t.saturating_sub(base())) / 1000))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, max_shrink_iters: 20000, failure_persistence: None, ..ProptestConfig::default() })]

    /// After every batch of events and every action, the UI's rows are the messages of
    /// `messages()`, in order: same count, same message in each row.
    #[test]
    fn ui_rows_follow_messages(ops in prop::collection::vec(op(), 1..30)) {
        let mut sim = Sim::new();
        for op in &ops {
            sim.run(op);
            let (shown, truth) = (sim.shown(), sim.truth());
            prop_assert!(
                shown == truth,
                "after {:?}\nUI rows:  {:?}\nmessages: {:?}\nlog:\n{}",
                op, rel(&shown), rel(&truth), sim.log.join("\n")
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, max_shrink_iters: 20000, failure_persistence: None, ..ProptestConfig::default() })]

    /// Weaker: rows hold the right messages *and* show their current state (body, delivery,
    /// deletion, reactions, expiry).
    #[test]
    fn ui_rows_show_current_state(ops in prop::collection::vec(op(), 1..30)) {
        let mut sim = Sim::new();
        for op in &ops {
            sim.run(op);
            if sim.shown() != sim.truth() {
                // Covered by `ui_rows_follow_messages`.
                return Ok(());
            }
            let truth: Vec<Row> = sim.chat.messages().map(|m| row(&sim.chat, &m)).collect();
            prop_assert!(
                sim.model == truth,
                "after {:?}\nUI rows:  {:?}\nmessages: {:?}\nlog:\n{}",
                op, sim.model, truth, sim.log.join("\n")
            );
        }
    }
}

/// The realistic shape of what `ui_rows_follow_messages` finds: two messages arrive late
/// (sent before the oldest loaded one), then the older page that has a message between
/// them is loaded.
#[test]
fn late_messages_then_an_older_page() {
    let mut sim = Sim::new();
    let a = cid(0);
    sim.run(&Op::Select(0));
    // The newest page: messages at 10 and 11; older ones before 10 are still stored.
    sim.run(&Op::Batch(vec![Ev::History {
        conv: 0,
        msgs: vec![(0, 10), (2, 11)],
        older: Some(10),
    }]));
    // Two messages Alice sent earlier arrive only now (her phone was offline).
    sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 3)]));
    sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 5)]));
    // Scrolled to the top: the page before 10 comes, with what was stored between them.
    sim.run(&Op::LoadOlder);
    let update = sim.chat.apply(Event::History {
        conversation: a.clone(),
        messages: vec![
            msg(0, 1, 2),
            msg(0, 0, 3),
            msg(0, 1, 4),
            msg(0, 0, 5),
            msg(0, 1, 6),
        ],
        reactions: Vec::new(),
        older: None,
    });
    render(&mut sim.model, &sim.chat, update);
    println!(
        "update {update:?}\nUI rows:  {:?}\nmessages: {:?}",
        rel(&sim.shown()),
        rel(&sim.truth())
    );
    assert_eq!(rel(&sim.shown()), rel(&sim.truth()));
}

/// The same, reached by picking a search result in the older page: the UI scrolls to the
/// result's row, which shows another message, and the result itself isn't in the list.
#[test]
fn search_result_in_an_older_page() {
    let mut sim = Sim::new();
    let a = cid(0);
    sim.run(&Op::Select(0));
    sim.run(&Op::Batch(vec![Ev::History {
        conv: 0,
        msgs: vec![(0, 10), (2, 11)],
        older: Some(10),
    }]));
    sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 3)]));
    sim.run(&Op::Batch(vec![Ev::Msg(0, 0, 5)]));
    sim.chat.open_chat_search();
    sim.chat.search_chat("bob");
    sim.chat.apply(Event::SearchResults {
        conversation: a.clone(),
        query: "bob".into(),
        messages: vec![msg(0, 1, 6)],
    });
    let update = sim.chat.show_result(0);
    render(&mut sim.model, &sim.chat, update);
    let commands = sim.chat.take_commands();
    assert!(
        matches!(
            commands.as_slice(),
            [Command::LoadHistory {
                before: Some(_),
                ..
            }]
        ),
        "{commands:?}"
    );
    let update = sim.chat.apply(Event::History {
        conversation: a.clone(),
        messages: vec![
            msg(0, 1, 2),
            msg(0, 0, 3),
            msg(0, 1, 4),
            msg(0, 0, 5),
            msg(0, 1, 6),
        ],
        reactions: Vec::new(),
        older: None,
    });
    render(&mut sim.model, &sim.chat, update);
    let (row, _) = sim.chat.jump().expect("scrolls to the result");
    let at_row = &sim.model[row];
    println!(
        "jump row {row}: UI shows {}@{} there; result bob@6 shown at all: {}",
        at_row.sender_id,
        (at_row.timestamp - base()) / 1000,
        sim.model
            .iter()
            .any(|r| r.sender_id == "bob" && r.timestamp == ts(6))
    );
    assert_eq!(
        (at_row.sender_id.as_str(), at_row.timestamp),
        ("bob", ts(6))
    );
}
