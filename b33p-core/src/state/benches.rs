//! Timings of what the UI asks of `ChatState` on a large account. In release:
//! `cargo test -p b33p-core --release -- --ignored bench_ --nocapture`.
use super::*;
use crate::{ChatFolder, ConversationKind, Delivery, Story, StoryContent};
use std::hint::black_box;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CONTACTS: usize = 1_000;
const GROUPS: usize = 50;
const HISTORY: usize = 5_000;
/// People writing in the open group.
const SENDERS: usize = 12;
const MINUTE: u64 = 60_000;

/// The benches run one at a time, so they don't slow each other down.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Deterministic, so every run measures the same account.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

fn sender_id(i: usize) -> String {
    format!("person-{i}")
}

fn contact(i: usize) -> ConversationId {
    ConversationId::contact(&sender_id(i))
}

fn group(i: usize) -> ConversationId {
    ConversationId(format!("g:{i:04}"))
}

/// The open chat: a busy group.
fn open() -> ConversationId {
    group(0)
}

fn picture() -> Arc<Picture> {
    Arc::new(Picture {
        width: 1,
        height: 1,
        rgba: vec![0; 4],
    })
}

fn attachment(id: String, kind: AttachmentKind) -> Attachment {
    Attachment {
        id: AttachmentId(id),
        kind,
        content_type: String::new(),
        file_name: Some("file.bin".into()),
        size: 1_000,
        width: 640,
        height: 480,
        blurhash: None,
        caption: None,
        gif: false,
        pointer: Arc::from(Vec::new()),
    }
}

/// `n` messages of the open group, oldest first, ending an hour ago and reaching back
/// over a year: runs of quick replies, quiet hours, days without any.
fn group_history(n: usize, seed: u64) -> Vec<ChatMessage> {
    let mut rng = Rng(seed);
    let gaps: Vec<u64> = (0..n)
        .map(|_| match rng.below(100) {
            0..70 => (1 + rng.below(4)) * MINUTE,
            70..95 => (10 + rng.below(170)) * MINUTE,
            _ => (1440 + rng.below(2880)) * MINUTE,
        })
        .collect();
    let mut at = now_millis() - 60 * MINUTE - gaps.iter().sum::<u64>();
    gaps.iter()
        .enumerate()
        .map(|(i, gap)| {
            at += gap;
            group_message(i, at, &mut rng)
        })
        .collect()
}

fn group_message(i: usize, timestamp: u64, rng: &mut Rng) -> ChatMessage {
    let sender = rng.below(SENDERS as u64 + 3) as usize;
    let outgoing = sender >= SENDERS;
    let mut message = ChatMessage {
        conversation: open(),
        sender_id: if outgoing {
            "me".into()
        } else {
            sender_id(sender)
        },
        sender: if outgoing {
            "You".into()
        } else {
            format!("Person {sender}")
        },
        body: match i % 4 {
            0 => format!("Message {i}"),
            1 => format!("Message {i}, about https://example.com/{i} and more"),
            2 => format!("Message {i}\nwith a second line\nand a third"),
            _ => "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do".into(),
        },
        timestamp,
        outgoing,
        delivery: if outgoing {
            Delivery::Read
        } else {
            Delivery::Sent
        },
        ..ChatMessage::default()
    };
    if i % 10 == 3 {
        message.body.clear();
        message.attachments = vec![attachment(format!("photo-{i}"), AttachmentKind::Image)];
    }
    if i % 97 == 5 {
        message.attachments = vec![attachment(format!("long-{i}"), AttachmentKind::LongText)];
    }
    if i.is_multiple_of(7) {
        message.reactions = (0..2)
            .map(|r| Reaction {
                emoji: "👍".into(),
                sender_id: sender_id(r),
                sender: format!("Person {r}"),
                outgoing: false,
            })
            .collect();
    }
    if i % 250 == 1 {
        message.notice = true;
        message.outgoing = false;
        message.body = "Person 3's safety number changed.".into();
    }
    message
}

/// 1,050 chats, folders, stories, avatars; the big group open with `history` loaded and
/// read, in a focused window.
fn account(history: Vec<ChatMessage>, older: Option<u64>) -> ChatState {
    let mut rng = Rng(1);
    let now = now_millis();
    let mut chat = ChatState::default();
    chat.apply(Event::Linked {
        account: "me".into(),
    });
    chat.apply(Event::Synced);
    chat.apply(Event::StorageRead);
    chat.set_window_focused(true);
    let members: Vec<Person> = (0..300)
        .map(|i| Person {
            service_id: sender_id(i),
            name: format!("Person {i}"),
        })
        .collect();
    let contacts = (0..CONTACTS).map(|i| Conversation {
        id: contact(i),
        title: if i % 10 == 9 {
            format!("+49 151 {i:07}")
        } else {
            format!("Person {i}")
        },
        settings: ChatSettings {
            accepted: i % 40 != 39,
            archived: i % 50 == 49,
            muted_until: (i % 30 == 29).then_some(u64::MAX),
            pinned: (i < 3).then_some(i as u32),
            ..ChatSettings::default()
        },
        ..Conversation::default()
    });
    let read_up_to = history
        .len()
        .checked_sub(50)
        .map_or(0, |i| history[i].timestamp);
    let groups = (0..GROUPS).map(|i| Conversation {
        id: group(i),
        title: format!("Group {i}"),
        kind: ConversationKind::Group,
        settings: ChatSettings {
            accepted: true,
            last_read: if i == 0 { read_up_to } else { 0 },
            ..ChatSettings::default()
        },
        members: members[..if i == 0 { SENDERS } else { 20 + i * 5 }].to_vec(),
        ..Conversation::default()
    });
    chat.apply(Event::Conversations(contacts.chain(groups).collect()));
    // The newest message of each chat, as at start-up; most are old.
    let latest: Vec<Event> = (0..CONTACTS)
        .map(|i| (contact(i), sender_id(i), i % 3 == 0))
        .chain((1..GROUPS).map(|i| (group(i), sender_id(i), false)))
        .map(|(conversation, sender_id, outgoing)| {
            Event::Latest(ChatMessage {
                conversation,
                sender: sender_id.clone(),
                sender_id,
                body: "Hello there\nsecond line".into(),
                timestamp: now - rng.below(400 * 1440) * MINUTE,
                outgoing,
                ..ChatMessage::default()
            })
        })
        .collect();
    chat.apply_all(latest);
    chat.apply(Event::Unread(
        (0..CONTACTS)
            .step_by(7)
            .map(|i| (contact(i), 1 + (i % 5) as u32))
            .collect(),
    ));
    chat.apply(Event::Folders(
        (0..6)
            .map(|f| ChatFolder {
                name: format!("Folder {f}"),
                only_unread: f == 1,
                show_muted: f % 2 == 0,
                all_individual: f == 2,
                all_groups: f == 3,
                included: (0..CONTACTS)
                    .filter(|i| i % 6 == f)
                    .take(150)
                    .map(contact)
                    .collect(),
                excluded: (0..CONTACTS)
                    .filter(|i| i % 11 == f)
                    .take(20)
                    .map(contact)
                    .collect(),
            })
            .collect(),
    ));
    let avatars: Vec<Event> = (0..CONTACTS)
        .filter(|i| *i < SENDERS || i % 3 == 0)
        .map(contact)
        .chain((0..GROUPS).map(group))
        .map(|conversation| Event::Avatar {
            conversation,
            avatar: Some(picture()),
        })
        .collect();
    chat.apply_all(avatars);
    chat.apply(Event::Stories(
        (0..300)
            .map(|s| Story {
                author_id: sender_id(s * 3 % CONTACTS),
                author: format!("Person {}", s * 3 % CONTACTS),
                conversation: contact(s * 3 % CONTACTS),
                timestamp: now - (s as u64) * MINUTE,
                content: StoryContent::Text {
                    text: "A story".into(),
                    foreground: 0,
                    background: 0,
                },
                viewed: s % 2 == 0,
                outgoing: false,
            })
            .collect(),
    ));
    let long_texts: Vec<AttachmentId> = history
        .iter()
        .flat_map(|m| &m.attachments)
        .filter(|a| a.kind == AttachmentKind::LongText)
        .map(|a| a.id.clone())
        .collect();
    chat.select(&open());
    chat.apply(Event::History {
        conversation: open(),
        messages: history,
        reactions: Vec::new(),
        older,
    });
    for id in long_texts {
        chat.apply(Event::Attachment {
            id,
            content: Ok(AttachmentContent::Text("The whole story. ".repeat(40))),
        });
    }
    chat.apply(Event::Typing {
        conversation: open(),
        sender_id: sender_id(1),
        sender: "Person 1".into(),
        typing: true,
    });
    chat.take_commands();
    chat.take_notices();
    chat
}

/// What ui.rs `render` asks for after every turn of the event loop that changed anything,
/// e.g. a keystroke in the composer, whatever the update says.
fn render_always(chat: &ChatState, draft: &str) -> usize {
    let mut n = chat.staged().len();
    if let Some(Some(media)) = chat.media() {
        n += media.media.len() + media.files.len() + media.links.len();
    }
    n += chat.sticker_packs().flatten().map_or(0, <[_]>::len);
    n += usize::from(chat.details().is_some());
    n += chat.unviewed_stories();
    n += chat.story_rings().len();
    n += chat.story_groups().len();
    n += usize::from(chat.current_story().is_some());
    if let Some((query, Some(results))) = chat.chat_search() {
        n += results
            .iter()
            .map(|m| snippet(&m.body, query).0.len())
            .sum::<usize>();
    }
    if chat.group_draft().is_some() {
        n += chat.group_candidates("").len();
    }
    if chat.forwarding() {
        n += chat.forward_targets("").len();
    }
    let (archived, unread) = chat.archived();
    n += archived + unread;
    n += (0..chat.folders().len())
        .map(|at| chat.folder_unread(at) as usize)
        .sum::<usize>();
    n += chat
        .selected()
        .map_or(0, |c| typing_label(&chat.typing(&c.id)).len());
    n += usize::from(chat.editing().is_some());
    // The picked messages' text only when copied, not at every redraw.
    n += chat.selection_len();
    n += usize::from(chat.can_forward_selection());
    n += usize::from(chat.can_delete_selection_for_everyone());
    n += usize::from(chat.selected().is_some_and(|c| c.is_group()));
    n += usize::from(chat.can_compose(draft));
    n += chat.mention_candidates(draft).len();
    n += chat.unread_total() as usize;
    n += usize::from(chat.can_load_older());
    n += usize::from(chat.connection() == Connection::Online);
    n += chat.error().map_or(0, str::len);
    n
}

/// What ui.rs `render` asks for to redraw the chat list, with `list_time` formatting times.
fn render_chat_list(chat: &ChatState, list_time: impl Fn(u64) -> String) -> usize {
    let pinned = chat.pinned().len();
    let rows: usize = chat
        .conversations()
        .map(|view| {
            let c = view.conversation;
            view.preview().len()
                + view
                    .last_message
                    .map_or(0, |m| list_time(m.timestamp).len())
                + initials(&c.title).len()
                + typing_label(&view.typing).len()
                + usize::from(view.muted())
                + usize::from(view.marked_unread())
                + usize::from(view.pinned())
                + usize::from(chat.is_request(c))
        })
        .sum();
    rows + pinned
        + chat.selected_index().unwrap_or_default()
        + chat.selected_view().map_or(0, |view| view.preview().len())
}

/// What a row of the open chat costs to read, roughly as the UI reads it.
fn read_row(view: &MessageView) -> usize {
    view.body.len()
        + view.styles.len()
        + view.day.as_ref().map_or(0, String::len)
        + usize::from(view.first_of_run)
        + usize::from(view.last_of_run)
        + usize::from(view.show_sender)
        + usize::from(view.sender_avatar.is_some())
        + usize::from(view.first_unread)
        + usize::from(view.revealed)
        + usize::from(view.highlighted)
        + usize::from(view.selected)
}

/// Times `run` on a fresh `setup()` each time, after a warm-up, and prints the median
/// and the fastest. What `run` returns is dropped outside the timing.
fn bench_with<S, T>(
    name: &str,
    runs: usize,
    mut setup: impl FnMut() -> S,
    mut run: impl FnMut(S) -> T,
) {
    black_box(run(setup()));
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let input = setup();
            let start = Instant::now();
            let output = run(input);
            let elapsed = start.elapsed();
            black_box(output);
            elapsed
        })
        .collect();
    times.sort();
    println!(
        "bench {name:<52} median {:>11.3?}   min {:>11.3?}   ({runs} runs)",
        times[runs / 2],
        times[0]
    );
}

fn bench<T>(name: &str, runs: usize, mut run: impl FnMut() -> T) {
    bench_with(name, runs, || (), |()| run());
}

const DRAFT: &str = "Sounds good, see you then @Pers";

#[test]
#[ignore]
fn bench_render() {
    let _one = ONE_AT_A_TIME.lock();
    let mut chat = account(group_history(HISTORY, 3), None);
    bench("a. keystroke: render's calls, nothing picked", 200, || {
        chat.draft_changed(DRAFT);
        render_always(&chat, DRAFT)
    });

    chat.toggle_selected(0);
    chat.select_through(HISTORY - 1);
    assert!(chat.selection().len() > HISTORY - 30);
    bench("b. keystroke: render's calls, all 5,000 picked", 20, || {
        chat.draft_changed(DRAFT);
        render_always(&chat, DRAFT)
    });
    chat.clear_selection();

    // The side panels open: media (2,000), choosing people for a group, stories.
    chat.open_media();
    chat.apply(Event::Media {
        conversation: open(),
        messages: group_history(2_000, 5).into_iter().rev().collect(),
    });
    chat.open_group_draft(Some(group(GROUPS - 1)));
    bench("a'. keystroke, media + group draft open", 100, || {
        chat.draft_changed(DRAFT);
        render_always(&chat, DRAFT)
    });
    bench("   media()", 100, || {
        chat.media().map(|m| m.map(|m| m.links.len()))
    });
    bench("   group_candidates(\"\")", 100, || {
        chat.group_candidates("").len()
    });
    bench("   story_rings()", 100, || chat.story_rings().len());
}

#[test]
#[ignore]
fn bench_messages() {
    let _one = ONE_AT_A_TIME.lock();
    let mut chat = account(group_history(HISTORY, 3), None);
    let last = chat.messages().last().unwrap().message.timestamp;
    let mut rng = Rng(9);
    let mut at = last;
    let mut i = 0;
    bench(
        "   one incoming message: apply + last_messages(1)",
        200,
        || {
            at += MINUTE;
            i += 1;
            let mut message = group_message(i, at, &mut rng);
            message.notice = false;
            let update = chat.apply(Event::Message(message));
            assert_eq!(update.messages, MessagesUpdate::Appended(1));
            chat.last_messages(1).map(|v| read_row(&v)).sum::<usize>()
        },
    );
    bench("c. last_messages(1)", 200, || {
        chat.last_messages(1).map(|v| read_row(&v)).sum::<usize>()
    });
    bench("d. messages(): every row", 50, || {
        chat.messages().map(|v| read_row(&v)).sum::<usize>()
    });
    let near_end = chat.messages().len() - 2;
    bench("   react(near the end)", 200, || chat.react(near_end, "👍"));
}

/// ui.rs `rows_around`: the rows to redraw for the changed messages.
fn rows_around(chat: &ChatState, changed: &[MessageRef]) -> Vec<usize> {
    let mut rows: Vec<usize> = changed
        .iter()
        .filter_map(|message| chat.row_of(message))
        .flat_map(|row| row.saturating_sub(1)..=row + 1)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// A reaction comes in, and the rows are redrawn as ui.rs `render` does: every one, as
/// before the changed ones were named, or theirs.
#[test]
#[ignore]
fn bench_reaction() {
    let _one = ONE_AT_A_TIME.lock();
    let mut chat = account(group_history(HISTORY, 3), None);
    let near_end = chat.messages().len() - 2;
    let target = chat.message_view(near_end).unwrap().message.reference();
    let mut remove = true;
    let mut react = |chat: &mut ChatState| {
        remove = !remove;
        let update = chat.apply(Event::Reaction {
            conversation: open(),
            target: target.clone(),
            reaction: Reaction {
                emoji: "👍".into(),
                sender_id: sender_id(5),
                sender: "Person 5".into(),
                outgoing: false,
            },
            remove,
        });
        assert!(update.rows);
    };
    bench("h. a reaction, then every row (5,000)", 50, || {
        react(&mut chat);
        chat.take_changed_rows();
        chat.messages().map(|v| read_row(&v)).sum::<usize>()
    });
    bench(
        "h'. a reaction, then its row and those beside it",
        200,
        || {
            react(&mut chat);
            let changed = chat.take_changed_rows().expect("named");
            rows_around(&chat, &changed)
                .into_iter()
                .filter_map(|row| chat.message_view(row))
                .map(|v| read_row(&v))
                .sum::<usize>()
        },
    );
}

#[test]
#[ignore]
fn bench_prepend() {
    let _one = ONE_AT_A_TIME.lock();
    let all = group_history(100 * 101, 4);
    bench_with(
        "e. 100 older pages of 100, one by one",
        5,
        || {
            let newest = all[100 * 100..].to_vec();
            let older = Some(newest[0].timestamp);
            account(newest, older)
        },
        |mut chat| {
            for page in all[..100 * 100].chunks(100).rev() {
                chat.load_older();
                let update = chat.apply_all([Event::History {
                    conversation: open(),
                    messages: page.to_vec(),
                    reactions: Vec::new(),
                    older: Some(page[0].timestamp),
                }]);
                assert_eq!(update.messages, MessagesUpdate::Prepended(100));
            }
            chat
        },
    );
}

#[test]
#[ignore]
fn bench_catch_up() {
    let _one = ONE_AT_A_TIME.lock();
    let history = group_history(HISTORY, 3);
    let last = history.last().unwrap().timestamp;
    let mut rng = Rng(11);
    let arriving: Vec<Event> = (0..3_000)
        .map(|i| {
            let mut message = group_message(i, last + 1 + i as u64 * 1_000, &mut rng);
            message.notice = false;
            Event::Message(message)
        })
        .collect();
    bench_with(
        "f. 3,000 messages in one apply_all, chat open",
        3,
        || (account(history.clone(), None), arriving.clone()),
        |(mut chat, arriving)| {
            chat.apply_all(arriving);
            let commands = chat.take_commands();
            (chat, commands)
        },
    );
}

#[test]
#[ignore]
fn bench_chat_list() {
    let _one = ONE_AT_A_TIME.lock();
    let mut chat = account(group_history(HISTORY, 3), None);
    bench("g. chat list: conversations() as rows", 100, || {
        render_chat_list(&chat, format_list_time)
    });
    bench("g'. the same, with list_time_formatter()", 100, || {
        render_chat_list(&chat, list_time_formatter())
    });
    // What ui.rs `render` reads when an event names the one chat it changed.
    let one = group(GROUPS - 1);
    bench("g''. one chat named: the order, and its row", 100, || {
        let list_time = list_time_formatter();
        let order = chat.listed_ids().count();
        let view = chat.conversation_view(&one).unwrap();
        order
            + view.preview().len()
            + view
                .last_message
                .map_or(0, |m| list_time(m.timestamp).len())
    });

    let mut n = 0;
    bench(
        "   300 one-chat Event::Conversations, one batch",
        20,
        || {
            let events: Vec<Event> = (0..300)
                .map(|_| {
                    n += 1;
                    Event::Conversations(vec![Conversation {
                        id: contact(n % CONTACTS),
                        title: format!("Renamed {n}"),
                        ..Conversation::default()
                    }])
                })
                .collect();
            chat.apply_all(events)
        },
    );
    let picked = chat.messages().len() - 200;
    bench_with(
        "   delete 200 picked of 5,000",
        5,
        || {
            let mut chat = account(group_history(HISTORY, 3), None);
            chat.toggle_selected(picked);
            chat.select_through(picked + 199);
            chat
        },
        |mut chat| {
            chat.delete_selection(false);
            chat
        },
    );
}
