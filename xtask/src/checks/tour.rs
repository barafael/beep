//! The tour (pending/stress.md): every feature b33p has, between the b33ps of a mesh, in an order
//! drawn from a seed (`TESTBOX_SEED`, printed at the start), each step checked on every b33p it
//! reaches against a model of what each should show. Each kind of step comes up
//! `TESTBOX_TOUR_REPEAT` times (two by default), when what it needs is there: an edit once there
//! is a message to edit, a removal once a group has a member to remove. What the steps took, and
//! what the b33ps used, is in the record's `tour.json`.
//!
//! The model is of what each b33p shows of each chat: its messages (not the notices), each with
//! who sent it, its text, whether it was edited or deleted, its reactions, its quote and its
//! attachments' kinds; a group's members and admins; what each b33p deleted for itself. A b33p
//! that joined a group again is held to what came after.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use b33p_harness::{Action, Answer, AttachmentKind, Delivery, InviteLinkAccess, Snapshot};
use serde::Serialize;
use strum::VariantArray;

use super::b33p::{edit, select};
use super::rng::{self, Xorshift};
use super::setup::{MeshB33p, MeshBox, started};
use super::stress::{Sample, growing, no_panic, sample};
use super::{Checks, PATIENCE};
use crate::Sockets;
use crate::error::{Context, Error, Result};
use crate::harness::Harness;
use crate::pod::{Pod, Service};
use crate::process;

/// How long a message with a disappearing timer may take to go, past its timer.
const EXPIRY_PATIENCE: Duration = Duration::from_secs(90);
/// The disappearing timer the tour sets, in seconds.
const TIMER: u32 = 10;
/// How often the b33ps' memory, open files and answering time are sampled.
const SAMPLE_EVERY: Duration = Duration::from_secs(30);
/// How long the details of a group are waited for before they are opened again.
const DETAILS_AGAIN: Duration = Duration::from_secs(3);
/// How many steps in a row may be drawn to let the rest go before the tour gives up on it.
const FILLERS: usize = 8;
/// The most groups the tour makes.
const GROUPS: usize = 2;
const EMOJI: [&str; 5] = ["👍", "❤️", "😂", "😮", "🎉"];

/// What a step does: each comes up [`repeat`] times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, strum::VariantArray, strum::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum Kind {
    Text,
    Reply,
    React,
    Edit,
    DeleteForEveryone,
    DeleteForMe,
    Forward,
    Picture,
    File,
    Audio,
    Typing,
    ReadReceipt,
    Disappearing,
    ChatState,
    Search,
    Block,
    CreateGroup,
    Rename,
    Describe,
    GroupPicture,
    AddMember,
    RemoveMember,
    MakeAdmin,
    Leave,
    JoinByLink,
    Story,
    Restart,
    /// A link sent with link previews off (b33p's default): nothing may look it up.
    LinkWithoutPreview,
    /// A reply to one's own message, the message edited after, and a restart: the quote stays
    /// what it quoted.
    QuoteThenEdit,
}

/// `TESTBOX_TOUR_REPEAT`, or 2.
fn repeat() -> usize {
    std::env::var("TESTBOX_TOUR_REPEAT")
        .ok()
        .and_then(|repeat| repeat.parse().ok())
        .unwrap_or(2)
}

/// One chat as the model has it: between two b33ps, or a group.
struct Thread {
    /// The two b33ps of a chat between two, by their place in the mesh.
    pair: Option<(usize, usize)>,
    /// A group's id, the same on every b33p.
    group: Option<String>,
    title: String,
    members: BTreeSet<usize>,
    admins: BTreeSet<usize>,
    /// Who an admin removed: they can't join by the link again, only be added.
    removed: BTreeSet<usize>,
    /// Who left: they may ask to join again by the link.
    left: BTreeSet<usize>,
    /// When each member last joined, in milliseconds since the epoch: what came before isn't held
    /// against it.
    joined: BTreeMap<usize, u64>,
    description: Option<String>,
    picture: bool,
    /// When the picture last changed, in milliseconds since the epoch.
    picture_at: u64,
    messages: Vec<Expected>,
}

impl Thread {
    /// The id `k`'s b33p knows it by.
    fn id_on(&self, k: usize, mesh: &[MeshB33p<'_>]) -> String {
        match (&self.group, self.pair) {
            (Some(group), _) => group.clone(),
            (None, Some((a, b))) => mesh[if k == a { b } else { a }].chat(),
            (None, None) => String::new(),
        }
    }

    fn name(&self) -> String {
        match self.pair {
            Some((a, b)) => format!("the chat of {a} and {b}"),
            None => format!("the group {:?}", self.title),
        }
    }

    /// The messages `k` sees, ours to act on: not deleted for it, since it last joined.
    fn visible(&self, k: usize) -> impl Iterator<Item = &Expected> {
        let since = self.joined.get(&k).copied().unwrap_or_default();
        self.messages
            .iter()
            .filter(move |m| m.audience.contains(&k) && !m.hidden.contains(&k) && m.ts >= since)
    }

    /// What `k`'s b33p should show of it.
    fn expected(&self, k: usize) -> Vec<Seen> {
        let mut seen: Vec<Seen> = self
            .visible(k)
            .map(|message| {
                if message.deleted {
                    Seen::deleted(message.ts, message.from)
                } else {
                    let mut reactions: Vec<(String, bool)> = message
                        .reactions
                        .iter()
                        .map(|(reactor, emoji)| (emoji.clone(), *reactor == k))
                        .collect();
                    reactions.sort();
                    Seen {
                        ts: message.ts,
                        from: message.from,
                        deleted: false,
                        text: message.text.clone(),
                        edited: message.edited,
                        reactions,
                        quote: message.quote.clone(),
                        attachments: message.attachments.clone(),
                    }
                }
            })
            .collect();
        seen.sort_by_key(|seen| seen.ts);
        seen
    }
}

/// A message as the model has it.
#[derive(Clone)]
struct Expected {
    /// Its sender's timestamp, the same on every b33p, and kept through edits.
    ts: u64,
    from: usize,
    text: String,
    edited: bool,
    deleted: bool,
    /// Each reactor's emoji.
    reactions: BTreeMap<usize, String>,
    quote: Option<String>,
    attachments: Vec<AttachmentKind>,
    /// Who deleted it for themselves, or blocked its sender when it came.
    hidden: BTreeSet<usize>,
    /// Who it went to, its sender too.
    audience: BTreeSet<usize>,
}

/// A message as a b33p shows it, or should: what the model and a snapshot are compared by.
#[derive(Debug, PartialEq)]
struct Seen {
    ts: u64,
    from: usize,
    deleted: bool,
    text: String,
    edited: bool,
    reactions: Vec<(String, bool)>,
    quote: Option<String>,
    attachments: Vec<AttachmentKind>,
}

impl Seen {
    /// A deleted message shows that it was, and nothing of what it was.
    fn deleted(ts: u64, from: usize) -> Self {
        Self {
            ts,
            from,
            deleted: true,
            text: String::new(),
            edited: false,
            reactions: Vec::new(),
            quote: None,
            attachments: Vec::new(),
        }
    }
}

/// What `k`'s b33p shows in its open chat, from `since` on, as the model has it; a sender that
/// isn't of the mesh is `usize::MAX`.
fn seen(snapshot: &Snapshot, k: usize, acis: &[String], since: u64) -> Vec<Seen> {
    let mut seen: Vec<Seen> = snapshot
        .messages
        .iter()
        .filter(|message| !message.notice && message.timestamp >= since)
        .map(|message| {
            let from = if message.outgoing {
                k
            } else {
                acis.iter()
                    .position(|aci| message.sender_id.contains(aci.as_str()))
                    .unwrap_or(usize::MAX)
            };
            if message.deleted {
                return Seen::deleted(message.timestamp, from);
            }
            let mut reactions: Vec<(String, bool)> = message
                .reactions
                .iter()
                .map(|reaction| (reaction.emoji.clone(), reaction.outgoing))
                .collect();
            reactions.sort();
            Seen {
                ts: message.timestamp,
                from,
                deleted: false,
                text: message.text.clone(),
                edited: message.edited,
                reactions,
                quote: message.quote.as_ref().map(|quote| quote.text.clone()),
                attachments: message
                    .attachments
                    .iter()
                    .map(|attachment| attachment.kind)
                    .collect(),
            }
        })
        .collect();
    seen.sort_by_key(|seen| seen.ts);
    seen
}

/// A step drawn, with what it acts on.
#[derive(Clone, Debug)]
enum Step {
    Text {
        by: usize,
        thread: usize,
        text: String,
    },
    Reply {
        by: usize,
        thread: usize,
        to: u64,
        text: String,
    },
    React {
        by: usize,
        thread: usize,
        to: u64,
        emoji: String,
    },
    Edit {
        by: usize,
        thread: usize,
        message: u64,
        text: String,
    },
    DeleteForEveryone {
        by: usize,
        thread: usize,
        message: u64,
    },
    DeleteForMe {
        by: usize,
        thread: usize,
        message: u64,
    },
    Forward {
        by: usize,
        from: usize,
        message: u64,
        to: usize,
    },
    Attachment {
        by: usize,
        thread: usize,
        kind: AttachmentKind,
        file: &'static str,
        text: String,
    },
    Typing {
        by: usize,
        thread: usize,
    },
    ReadReceipt {
        by: usize,
        thread: usize,
        message: u64,
    },
    Disappearing {
        by: usize,
        thread: usize,
        text: String,
    },
    ChatState {
        by: usize,
        thread: usize,
    },
    Search {
        by: usize,
        thread: usize,
    },
    Block {
        by: usize,
        thread: usize,
        blocked_text: String,
        text: String,
    },
    CreateGroup {
        by: usize,
        members: Vec<usize>,
        title: String,
    },
    Rename {
        by: usize,
        thread: usize,
        title: String,
    },
    Describe {
        by: usize,
        thread: usize,
        description: String,
    },
    GroupPicture {
        by: usize,
        thread: usize,
        picture: bool,
    },
    AddMember {
        by: usize,
        thread: usize,
        member: usize,
    },
    RemoveMember {
        by: usize,
        thread: usize,
        member: usize,
    },
    MakeAdmin {
        by: usize,
        thread: usize,
        member: usize,
    },
    Leave {
        by: usize,
        thread: usize,
    },
    JoinByLink {
        by: usize,
        thread: usize,
        admin: usize,
        /// Asked for, as by one who left: told to the members they knew. One who was never in
        /// joins by a link that needs no approval, as a first request reaches no admin until
        /// they fetch the group (pending/tour.md).
        approval: bool,
    },
    Story {
        by: usize,
        thread: usize,
        text: String,
    },
    Restart {
        by: usize,
    },
    LinkWithoutPreview {
        by: usize,
        thread: usize,
        text: String,
    },
    QuoteThenEdit {
        by: usize,
        thread: usize,
        quoted: u64,
        reply: String,
        edit: String,
    },
}

/// The test files the steps send, in `/files` (pod::FILES on the host).
const PICTURE: &str = "tour.png";
const FILE: &str = "tour.txt";
const AUDIO: &str = "tour.wav";

pub(super) fn tour(
    checks: &mut Checks,
    mesh: &mut MeshBox<'_>,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
    picture: &Path,
) -> Result<()> {
    std::fs::copy(picture, files.join(PICTURE)).context(picture.display().to_string())?;
    testbox_tools::write(&files.join(FILE), "testbox tour: a file\n")?;
    testbox_tools::write(&files.join(AUDIO), b33p_core::mock_voice_message())?;
    let seed = rng::seed();
    let repeat = repeat();
    let b33ps = mesh.b33ps.len();
    println!(
        "  seed {seed}, each step {repeat} times, {b33ps} b33ps (TESTBOX_SEED, TESTBOX_TOUR_REPEAT)"
    );
    checks.kept.write("seed", &format!("{seed}\n"));
    let mut tour = Tour::new(&mut mesh.b33ps, containers, sockets, seed);
    // Read receipts go once a chat shows, with the window focused.
    checks.require("every b33p's window counts as focused", || {
        tour.b33ps.iter_mut().try_for_each(|each| {
            each.b33p
                .act(Action::SetWindowFocused { focused: true })
                .map(drop)
        })
    })?;
    tour.sample_all()?;

    let mut left: Vec<Kind> = Kind::VARIANTS
        .iter()
        .flat_map(|&kind| std::iter::repeat_n(kind, repeat))
        .collect();
    let mut number = 0;
    let mut fillers = 0;
    while !left.is_empty() {
        number += 1;
        let drawn = tour.world.draw(&mut left);
        let (kind, step) = match drawn {
            Some(drawn) => {
                fillers = 0;
                drawn
            }
            // Nothing left can go yet: a step that may let it (a member leaving lets one ask to
            // join again), which counts as none of them; and no end of those.
            None => {
                fillers += 1;
                if fillers > FILLERS {
                    let stuck: Vec<&'static str> = left.iter().map(|&kind| kind.into()).collect();
                    return checks.require("every kind of step came up", || {
                        Err(Error::Failed(format!(
                            "{stuck:?} could never go; again with TESTBOX_SEED={seed}"
                        )))
                    });
                }
                tour.world.filler()?
            }
        };
        let name = format!("step {number}: {}", tour.world.describe(&step));
        let started = Instant::now();
        let outcome = checks.require(&name, || tour.perform(&step));
        let took = started.elapsed();
        tour.steps.push(StepRecord {
            number,
            kind: kind.into(),
            step: format!("{step:?}"),
            seconds: took.as_secs_f64(),
            passed: outcome.is_ok(),
        });
        tour.durations
            .entry(kind.into())
            .or_default()
            .push(took.as_secs_f64());
        if let Some((by, tried)) = tour.looked_up.take() {
            checks.record(
                &format!("step {number}: nothing looked the link up"),
                || {
                    if tried == 0 {
                        Ok(())
                    } else {
                        Err(Error::Failed(format!(
                            "b33p {by} logged {tried} tries at a preview, with link previews off"
                        )))
                    }
                },
            );
        }
        let stale = std::mem::take(&mut tour.stale_pictures);
        if !stale.is_empty() {
            checks.record(
                &format!("step {number}: who came back shows the group's picture"),
                || Err(Error::Failed(stale.join("\n"))),
            );
        }
        if outcome.is_err() {
            tour.write(checks, seed);
            tour.keep_snapshots(checks);
            println!("  the tour ended at step {number}; again with TESTBOX_SEED={seed}");
            return outcome;
        }
        if tour.sampled.elapsed() >= SAMPLE_EVERY {
            tour.sample_all()?;
        }
    }

    checks.record(
        "every b33p shows every chat it is in as the model has it",
        || (0..tour.world.threads.len()).try_for_each(|thread| tour.converge_all(thread)),
    );
    tour.sample_all()?;
    for k in 0..b33ps {
        let service = Service::B33p(tour.b33ps[k].n);
        checks.record(&format!("b33p {k} logged no panic"), || {
            no_panic(containers, service)
        });
        checks.record(
            &format!("b33p {k}'s memory and open files didn't keep growing"),
            || growing(&tour.samples[k]),
        );
    }
    tour.write(checks, seed);
    tour.summary();
    Ok(())
}

/// The model of every chat of the mesh, and what draws the steps on it.
struct World {
    /// How many b33ps the mesh has.
    count: usize,
    acis: Vec<String>,
    threads: Vec<Thread>,
    groups: usize,
    texts: usize,
    random: Xorshift,
}

impl World {
    /// The chats of two of `count` b33ps, nothing in them yet.
    fn new(acis: Vec<String>, seed: u64) -> Self {
        let count = acis.len();
        let threads = (0..count)
            .flat_map(|a| (a + 1..count).map(move |b| (a, b)))
            .map(|(a, b)| Thread {
                pair: Some((a, b)),
                group: None,
                title: String::new(),
                members: BTreeSet::from([a, b]),
                admins: BTreeSet::new(),
                removed: BTreeSet::new(),
                left: BTreeSet::new(),
                joined: BTreeMap::new(),
                description: None,
                picture: false,
                picture_at: 0,
                messages: Vec::new(),
            })
            .collect();
        Self {
            count,
            acis,
            threads,
            groups: 0,
            texts: 0,
            random: Xorshift(seed),
        }
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.random.below(bound as u64)).unwrap_or_default()
    }

    /// One of `items`, at random.
    fn pick<T: Clone>(&mut self, items: &[T]) -> Option<T> {
        self.random.pick(items).cloned()
    }

    /// A fresh text, of a kind drawn at random: plain, emoji, right to left, CJK, combining marks.
    fn new_text(&mut self, what: &str) -> String {
        self.texts += 1;
        let n = self.texts;
        let body = match self.below(5) {
            0 => "plain words",
            1 => "emoji 🎉🚀👩‍👩‍👧",
            2 => "שלום עולם, right to left",
            3 => "你好，世界",
            _ => "combining e\u{301} and a\u{308}",
        };
        format!("tour {n}: {what}, {body}")
    }

    /// Draws a kind of step from `left` that can go now, takes it out, and plans it.
    fn draw(&mut self, left: &mut Vec<Kind>) -> Option<(Kind, Step)> {
        let mut order: Vec<usize> = (0..left.len()).collect();
        // Shuffled, then the first that can go.
        for i in (1..order.len()).rev() {
            let j = self.below(i + 1);
            order.swap(i, j);
        }
        for at in order {
            let kind = left[at];
            if let Some(step) = self.plan(kind) {
                left.remove(at);
                return Some((kind, step));
            }
        }
        None
    }

    /// A step that may let what is left go, drawn from those that can go now: someone leaves
    /// (to join again), is added (to be removed, made an admin, post a story), a group is made,
    /// or, failing all of that, a text.
    fn filler(&mut self) -> Result<(Kind, Step)> {
        let mut kinds = [Kind::Leave, Kind::AddMember, Kind::CreateGroup];
        for i in (1..kinds.len()).rev() {
            let j = self.below(i + 1);
            kinds.swap(i, j);
        }
        kinds
            .into_iter()
            .chain([Kind::Text])
            .find_map(|kind| Some((kind, self.plan(kind)?)))
            .ok_or_else(|| Error::Failed("no chat to write in".to_owned()))
    }

    /// The threads `k` is a member of; chats of two only, or groups only.
    fn threads_of(&self, k: usize, pairs: bool, groups: bool) -> Vec<usize> {
        self.threads
            .iter()
            .enumerate()
            .filter(|(_, thread)| {
                thread.members.contains(&k)
                    && (pairs && thread.pair.is_some() || groups && thread.group.is_some())
            })
            .map(|(at, _)| at)
            .collect()
    }

    /// A b33p, and a thread it is in, at random.
    fn someone_in(&mut self, pairs: bool, groups: bool) -> Option<(usize, usize)> {
        let mut choices = Vec::new();
        for k in 0..self.count {
            for thread in self.threads_of(k, pairs, groups) {
                choices.push((k, thread));
            }
        }
        self.pick(&choices)
    }

    /// The messages `k` sees in `thread` that `wanted` takes, by timestamp.
    fn messages(&self, k: usize, thread: usize, wanted: impl Fn(&Expected) -> bool) -> Vec<u64> {
        self.threads[thread]
            .visible(k)
            .filter(|message| wanted(message))
            .map(|message| message.ts)
            .collect()
    }

    /// A b33p, a thread it is in and a message there that `wanted` takes, at random.
    fn some_message(
        &mut self,
        pairs: bool,
        groups: bool,
        wanted: impl Fn(usize, &Expected) -> bool,
    ) -> Option<(usize, usize, u64)> {
        let mut choices = Vec::new();
        for k in 0..self.count {
            for thread in self.threads_of(k, pairs, groups) {
                for ts in self.messages(k, thread, |message| wanted(k, message)) {
                    choices.push((k, thread, ts));
                }
            }
        }
        self.pick(&choices)
    }

    /// A group, an admin of it, and one of `candidates(group)`, at random.
    fn admin_and(
        &mut self,
        candidates: impl Fn(&Thread, usize) -> Vec<usize>,
    ) -> Option<(usize, usize, usize)> {
        let mut choices = Vec::new();
        for (at, thread) in self.threads.iter().enumerate() {
            if thread.group.is_none() {
                continue;
            }
            for &admin in &thread.admins {
                for other in candidates(thread, admin) {
                    choices.push((admin, at, other));
                }
            }
        }
        self.pick(&choices)
    }

    /// A group and an admin of it, at random.
    fn some_admin(&mut self) -> Option<(usize, usize)> {
        self.admin_and(|_, admin| vec![admin])
            .map(|(admin, thread, _)| (admin, thread))
    }

    /// The step `kind` with what it acts on drawn, if anything can be.
    fn plan(&mut self, kind: Kind) -> Option<Step> {
        let count = self.count;
        let step = match kind {
            Kind::Text => {
                let (by, thread) = self.someone_in(true, true)?;
                Step::Text {
                    by,
                    thread,
                    text: self.new_text(&format!("from {by}")),
                }
            }
            Kind::Reply => {
                let (by, thread, to) = self.some_message(true, true, |_, m| !m.deleted)?;
                Step::Reply {
                    by,
                    thread,
                    to,
                    text: self.new_text(&format!("a reply from {by}")),
                }
            }
            Kind::React => {
                let (by, thread, to) = self.some_message(true, true, |_, m| !m.deleted)?;
                let had = self.threads[thread]
                    .messages
                    .iter()
                    .find(|m| m.ts == to)
                    .and_then(|m| m.reactions.get(&by).cloned());
                let emoji: Vec<&str> = EMOJI
                    .into_iter()
                    .filter(|emoji| had.as_deref() != Some(emoji))
                    .collect();
                Step::React {
                    by,
                    thread,
                    to,
                    emoji: self.pick(&emoji)?.to_owned(),
                }
            }
            Kind::Edit => {
                let (by, thread, message) = self.some_message(true, true, |k, m| {
                    m.from == k && !m.deleted && m.attachments.is_empty()
                })?;
                Step::Edit {
                    by,
                    thread,
                    message,
                    text: self.new_text(&format!("edited by {by}")),
                }
            }
            Kind::DeleteForEveryone => {
                let (by, thread, message) =
                    self.some_message(true, true, |k, m| m.from == k && !m.deleted)?;
                Step::DeleteForEveryone {
                    by,
                    thread,
                    message,
                }
            }
            Kind::DeleteForMe => {
                let (by, thread, message) = self.some_message(true, true, |_, _| true)?;
                Step::DeleteForMe {
                    by,
                    thread,
                    message,
                }
            }
            Kind::Forward => {
                let (by, from, message) = self.some_message(true, true, |_, m| !m.deleted)?;
                let to: Vec<usize> = self
                    .threads_of(by, true, true)
                    .into_iter()
                    .filter(|&to| to != from)
                    .collect();
                Step::Forward {
                    by,
                    from,
                    message,
                    to: self.pick(&to)?,
                }
            }
            Kind::Picture | Kind::File | Kind::Audio => {
                let (by, thread) = self.someone_in(true, true)?;
                let (kind, file) = match kind {
                    Kind::Picture => (AttachmentKind::Image, PICTURE),
                    Kind::File => (AttachmentKind::File, FILE),
                    _ => (AttachmentKind::Audio, AUDIO),
                };
                Step::Attachment {
                    by,
                    thread,
                    kind,
                    file,
                    text: self.new_text(&format!("{kind:?} from {by}")),
                }
            }
            Kind::Typing => {
                let (by, thread) = self.someone_in(true, false)?;
                Step::Typing { by, thread }
            }
            Kind::ReadReceipt => {
                // The newest message of one of a chat of two, to the other: once the other
                // showed it, it was read.
                let mut choices = Vec::new();
                for (at, thread) in self.threads.iter().enumerate() {
                    if thread.pair.is_none() {
                        continue;
                    }
                    if let Some(newest) = thread.messages.iter().max_by_key(|m| m.ts)
                        && !newest.deleted
                        && newest.hidden.is_empty()
                    {
                        choices.push((newest.from, at, newest.ts));
                    }
                }
                let (by, thread, message) = self.pick(&choices)?;
                Step::ReadReceipt {
                    by,
                    thread,
                    message,
                }
            }
            Kind::Disappearing => {
                let (by, thread) = self.someone_in(true, false)?;
                Step::Disappearing {
                    by,
                    thread,
                    text: self.new_text(&format!("gone in {TIMER} s, from {by}")),
                }
            }
            Kind::ChatState => {
                let (by, thread) = self.someone_in(true, true)?;
                Step::ChatState { by, thread }
            }
            Kind::Search => {
                let (by, thread) = self.someone_in(true, true)?;
                Step::Search { by, thread }
            }
            Kind::Block => {
                // A third b33p's message shows when the blocked one has been dealt with.
                if count < 3 {
                    return None;
                }
                let (by, thread) = self.someone_in(true, false)?;
                Step::Block {
                    by,
                    thread,
                    blocked_text: self.new_text("sent while blocked"),
                    text: self.new_text("sent once unblocked"),
                }
            }
            Kind::CreateGroup => {
                if self.groups >= GROUPS || count < 2 {
                    return None;
                }
                let by = self.below(count);
                let mut others: Vec<usize> = (0..count).filter(|&k| k != by).collect();
                // At least one other.
                let keep = 1 + self.below(others.len());
                for i in (1..others.len()).rev() {
                    let j = self.below(i + 1);
                    others.swap(i, j);
                }
                others.truncate(keep);
                others.sort_unstable();
                self.groups += 1;
                Step::CreateGroup {
                    by,
                    members: others,
                    title: format!("tour group {}", self.groups),
                }
            }
            Kind::Rename => {
                let (by, thread) = self.some_admin()?;
                Step::Rename {
                    by,
                    thread,
                    title: format!("{}, renamed by {by}", self.new_text("title")),
                }
            }
            Kind::Describe => {
                let (by, thread) = self.some_admin()?;
                Step::Describe {
                    by,
                    thread,
                    description: self.new_text(&format!("described by {by}")),
                }
            }
            Kind::GroupPicture => {
                let (by, thread) = self.some_admin()?;
                Step::GroupPicture {
                    by,
                    thread,
                    picture: !self.threads[thread].picture,
                }
            }
            Kind::AddMember => {
                let (by, thread, member) = self.admin_and(|thread, _| {
                    (0..count).filter(|k| !thread.members.contains(k)).collect()
                })?;
                Step::AddMember { by, thread, member }
            }
            Kind::RemoveMember => {
                let (by, thread, member) = self.admin_and(|thread, admin| {
                    thread
                        .members
                        .iter()
                        .copied()
                        .filter(|&k| k != admin)
                        .collect()
                })?;
                Step::RemoveMember { by, thread, member }
            }
            Kind::MakeAdmin => {
                let (by, thread, member) = self.admin_and(|thread, _| {
                    thread
                        .members
                        .iter()
                        .copied()
                        .filter(|k| !thread.admins.contains(k))
                        .collect()
                })?;
                Step::MakeAdmin { by, thread, member }
            }
            Kind::Leave => {
                // Not the last admin, nor the last member.
                let mut choices = Vec::new();
                for (at, thread) in self.threads.iter().enumerate() {
                    if thread.group.is_none() || thread.members.len() < 2 {
                        continue;
                    }
                    for &k in &thread.members {
                        if thread.admins.iter().any(|&admin| admin != k) {
                            choices.push((k, at));
                        }
                    }
                }
                let (by, thread) = self.pick(&choices)?;
                Step::Leave { by, thread }
            }
            Kind::JoinByLink => {
                let (admin, thread, by) = self.admin_and(|thread, _| {
                    (0..count)
                        .filter(|k| !thread.members.contains(k) && !thread.removed.contains(k))
                        .collect()
                })?;
                Step::JoinByLink {
                    by,
                    thread,
                    admin,
                    approval: self.threads[thread].left.contains(&by),
                }
            }
            Kind::Story => {
                let (by, thread) = self.someone_in(false, true)?;
                if self.threads[thread].members.len() < 2 {
                    return None;
                }
                Step::Story {
                    by,
                    thread,
                    text: self.new_text(&format!("a story by {by}")),
                }
            }
            Kind::Restart => Step::Restart {
                by: self.below(count),
            },
            Kind::QuoteThenEdit => {
                let (by, thread, quoted) = self.some_message(true, true, |k, m| {
                    m.from == k && !m.deleted && m.attachments.is_empty()
                })?;
                Step::QuoteThenEdit {
                    by,
                    thread,
                    quoted,
                    reply: self.new_text(&format!("a reply from {by}, its quote edited after")),
                    edit: self.new_text(&format!("edited by {by} after its reply")),
                }
            }
            Kind::LinkWithoutPreview => {
                let (by, thread) = self.someone_in(true, true)?;
                let text = self.new_text(&format!("a link from {by}"));
                Step::LinkWithoutPreview {
                    by,
                    thread,
                    text: format!("{text} https://b33p-tour.org/link"),
                }
            }
        };
        Some(step)
    }

    fn describe(&self, step: &Step) -> String {
        let name = |thread: &usize| self.threads[*thread].name();
        match step {
            Step::Text { by, thread, .. } => format!("b33p {by} writes in {}", name(thread)),
            Step::Reply { by, thread, .. } => format!("b33p {by} replies in {}", name(thread)),
            Step::React {
                by, thread, emoji, ..
            } => format!("b33p {by} reacts {emoji} in {}", name(thread)),
            Step::Edit { by, thread, .. } => format!("b33p {by} edits in {}", name(thread)),
            Step::DeleteForEveryone { by, thread, .. } => {
                format!("b33p {by} deletes for everyone in {}", name(thread))
            }
            Step::DeleteForMe { by, thread, .. } => {
                format!("b33p {by} deletes for itself in {}", name(thread))
            }
            Step::Forward { by, from, to, .. } => {
                format!("b33p {by} forwards from {} to {}", name(from), name(to))
            }
            Step::Attachment {
                by, thread, kind, ..
            } => format!("b33p {by} sends a {kind:?} in {}", name(thread)),
            Step::Typing { by, thread } => format!("b33p {by} types in {}", name(thread)),
            Step::ReadReceipt { by, thread, .. } => {
                format!("b33p {by} sees its message read in {}", name(thread))
            }
            Step::Disappearing { by, thread, .. } => {
                format!("b33p {by} sends a disappearing message in {}", name(thread))
            }
            Step::ChatState { by, thread } => {
                format!(
                    "b33p {by} pins, mutes, marks unread and archives {}",
                    name(thread)
                )
            }
            Step::Search { by, thread } => {
                format!("b33p {by} finds {} in its chat list", name(thread))
            }
            Step::Block { by, thread, .. } => {
                format!(
                    "b33p {by} blocks and unblocks the other of {}",
                    name(thread)
                )
            }
            Step::CreateGroup { by, members, title } => {
                format!("b33p {by} makes {title:?} with {members:?}")
            }
            Step::Rename { by, thread, .. } => format!("b33p {by} renames {}", name(thread)),
            Step::Describe { by, thread, .. } => format!("b33p {by} describes {}", name(thread)),
            Step::GroupPicture {
                by,
                thread,
                picture,
            } => {
                let what = if *picture { "sets" } else { "takes away" };
                format!("b33p {by} {what} the picture of {}", name(thread))
            }
            Step::AddMember { by, thread, member } => {
                format!("b33p {by} adds {member} to {}", name(thread))
            }
            Step::RemoveMember { by, thread, member } => {
                format!("b33p {by} removes {member} from {}", name(thread))
            }
            Step::MakeAdmin { by, thread, member } => {
                format!("b33p {by} makes {member} an admin of {}", name(thread))
            }
            Step::Leave { by, thread } => format!("b33p {by} leaves {}", name(thread)),
            Step::JoinByLink {
                by,
                thread,
                admin,
                approval: true,
            } => format!(
                "b33p {by} asks to join {} again by its link, and {admin} approves",
                name(thread)
            ),
            Step::JoinByLink {
                by,
                thread,
                admin,
                approval: false,
            } => format!(
                "b33p {by} joins {} by the link {admin} turns on",
                name(thread)
            ),
            Step::Story { by, thread, .. } => {
                format!("b33p {by} posts a story to {}", name(thread))
            }
            Step::Restart { by } => format!("b33p {by} is killed and starts again"),
            Step::QuoteThenEdit { by, thread, .. } => format!(
                "b33p {by} replies to its message in {}, edits that, and starts again",
                name(thread)
            ),
            Step::LinkWithoutPreview { by, thread, .. } => format!(
                "b33p {by} sends a link in {}, with link previews off",
                name(thread)
            ),
        }
    }
}

struct Tour<'run, 'pod> {
    b33ps: &'run mut [MeshB33p<'pod>],
    containers: &'run Pod,
    sockets: &'run Sockets,
    world: World,
    /// What happened, for `tour.json`.
    steps: Vec<StepRecord>,
    /// Each kind of step's durations, in seconds.
    durations: BTreeMap<&'static str, Vec<f64>>,
    /// From a message sent to it showing on each other b33p of its chat, in seconds.
    delivered: Vec<f64>,
    /// From a b33p killed to its chats shown again, in seconds.
    restarts: Vec<f64>,
    /// Each b33p's samples, with when they were taken (seconds into the tour).
    samples: Vec<Vec<Sample>>,
    sampled_at: Vec<Vec<f64>>,
    /// Who sent a link at the last step, and how often it was looked up.
    looked_up: Option<(usize, usize)>,
    /// The groups whose picture a member that came back after it changed doesn't show.
    stale_pictures: Vec<String>,
    started: Instant,
    sampled: Instant,
}

impl<'run, 'pod> Tour<'run, 'pod> {
    fn new(
        b33ps: &'run mut [MeshB33p<'pod>],
        containers: &'run Pod,
        sockets: &'run Sockets,
        seed: u64,
    ) -> Self {
        let count = b33ps.len();
        let acis = b33ps.iter().map(|each| each.aci.clone()).collect();
        let mut world = World::new(acis, seed);
        // The chats' first messages, the primaries' (setup.rs `mesh_hello`), are before the tour.
        let start = now_ms();
        for thread in &mut world.threads {
            thread.joined = thread.members.iter().map(|&k| (k, start)).collect();
        }
        Self {
            b33ps,
            containers,
            sockets,
            world,
            steps: Vec::new(),
            durations: BTreeMap::new(),
            delivered: Vec::new(),
            restarts: Vec::new(),
            samples: (0..count).map(|_| Vec::new()).collect(),
            sampled_at: (0..count).map(|_| Vec::new()).collect(),
            looked_up: None,
            stale_pictures: Vec::new(),
            started: Instant::now(),
            sampled: Instant::now(),
        }
    }

    fn harness(&mut self, k: usize) -> &mut Harness {
        &mut self.b33ps[k].b33p
    }

    /// Opens `thread` in `k`'s b33p.
    fn open(&mut self, k: usize, thread: usize) -> Result<()> {
        let id = self.world.threads[thread].id_on(k, self.b33ps);
        select(self.harness(k), &id)
    }

    /// The index `k`'s b33p shows the message `ts` at, in the open chat.
    fn index(&mut self, k: usize, ts: u64) -> Result<u32> {
        self.harness(k)
            .wait_for(&format!("the message of {ts}"), PATIENCE, |snapshot| {
                snapshot
                    .messages
                    .iter()
                    .find(|message| message.timestamp == ts && !message.notice)
                    .map(|message| message.index)
            })
    }

    /// Sends what is in `k`'s composer to its open chat, and waits until the server took it; its
    /// timestamp.
    fn send(&mut self, k: usize, text: &str) -> Result<u64> {
        let before = now_ms();
        let b33p = self.harness(k);
        b33p.act(Action::Type {
            text: text.to_owned(),
        })?;
        if !b33p.act(Action::Send)? {
            // What b33p-core's `can_compose` goes by.
            let snapshot = b33p.snapshot()?;
            let chat = snapshot
                .conversations
                .iter()
                .find(|chat| Some(&chat.id) == snapshot.selected.as_ref());
            return Err(Error::Failed(format!(
                "b33p {k} sent nothing for {text:?}: synced {}, staged {:?}, draft {:?}, the \
                 open chat {:?}",
                snapshot.synced,
                snapshot.staged,
                snapshot.draft,
                chat.map(|chat| (
                    &chat.id,
                    chat.request,
                    chat.blocked,
                    chat.left,
                    chat.accepted
                )),
            )));
        }
        sent_since(b33p, k, before)
    }

    /// Adds the message `ts` from `by` to `thread`, to its members now.
    fn add(&mut self, thread: usize, by: usize, ts: u64, text: &str) -> &mut Expected {
        let audience = self.world.threads[thread].members.clone();
        let messages = &mut self.world.threads[thread].messages;
        messages.push(Expected {
            ts,
            from: by,
            text: text.to_owned(),
            edited: false,
            deleted: false,
            reactions: BTreeMap::new(),
            quote: None,
            attachments: Vec::new(),
            hidden: BTreeSet::new(),
            audience,
        });
        let last = messages.len() - 1;
        &mut messages[last]
    }

    fn message(&mut self, thread: usize, ts: u64) -> Result<&mut Expected> {
        self.world.threads[thread]
            .messages
            .iter_mut()
            .find(|message| message.ts == ts)
            .ok_or_else(|| Error::Failed(format!("no message of {ts} in the model")))
    }

    fn perform(&mut self, step: &Step) -> Result<()> {
        match step.clone() {
            Step::Text { by, thread, text } => {
                self.open(by, thread)?;
                let started = Instant::now();
                let ts = self.send(by, &text)?;
                self.add(thread, by, ts, &text);
                self.converge_timed(thread, by, started)
            }
            Step::Reply {
                by,
                thread,
                to,
                text,
            } => {
                self.open(by, thread)?;
                let index = self.index(by, to)?;
                let quoted = self.message(thread, to)?.text.clone();
                self.harness(by).act(Action::ReplyTo { index })?;
                let started = Instant::now();
                let ts = self.send(by, &text)?;
                self.add(thread, by, ts, &text).quote = Some(quoted);
                self.converge_timed(thread, by, started)
            }
            Step::React {
                by,
                thread,
                to,
                emoji,
            } => {
                self.open(by, thread)?;
                let index = self.index(by, to)?;
                self.harness(by).act(Action::React {
                    index,
                    emoji: emoji.clone(),
                })?;
                self.message(thread, to)?.reactions.insert(by, emoji);
                self.converge(thread)
            }
            Step::Edit {
                by,
                thread,
                message,
                text,
            } => {
                self.open(by, thread)?;
                let index = self.index(by, message)?;
                edit(self.harness(by), index, &text)?;
                let edited = self.message(thread, message)?;
                edited.text = text;
                edited.edited = true;
                self.converge(thread)
            }
            Step::DeleteForEveryone {
                by,
                thread,
                message,
            } => {
                self.open(by, thread)?;
                let index = self.index(by, message)?;
                self.harness(by).act(Action::Delete {
                    index,
                    everyone: true,
                })?;
                self.message(thread, message)?.deleted = true;
                self.converge(thread)
            }
            Step::DeleteForMe {
                by,
                thread,
                message,
            } => {
                self.open(by, thread)?;
                let index = self.index(by, message)?;
                self.harness(by).act(Action::Delete {
                    index,
                    everyone: false,
                })?;
                self.message(thread, message)?.hidden.insert(by);
                self.converge(thread)
            }
            Step::Forward {
                by,
                from,
                message,
                to,
            } => {
                self.open(by, from)?;
                let index = self.index(by, message)?;
                let original = self.message(from, message)?.clone();
                let target = self.world.threads[to].id_on(by, self.b33ps);
                let before = now_ms();
                let started = Instant::now();
                let b33p = self.harness(by);
                b33p.act(Action::Forward { index })?;
                b33p.act(Action::ForwardTo {
                    conversation: target.clone(),
                })?;
                select(b33p, &target)?;
                let ts = sent_since(b33p, by, before)?;
                self.add(to, by, ts, &original.text).attachments = original.attachments;
                self.converge_timed(to, by, started)
            }
            Step::Attachment {
                by,
                thread,
                kind,
                file,
                text,
            } => {
                self.open(by, thread)?;
                self.harness(by).act(Action::StageFiles {
                    paths: vec![format!("/files/{file}")],
                })?;
                let started = Instant::now();
                let ts = self.send(by, &text)?;
                self.add(thread, by, ts, &text).attachments = vec![kind];
                self.converge_timed(thread, by, started)
            }
            Step::Typing { by, thread } => {
                let other = self.other(thread, by)?;
                let id = self.world.threads[thread].id_on(other, self.b33ps);
                self.open(by, thread)?;
                self.harness(by).act(Action::Type {
                    text: "tour: typing".to_owned(),
                })?;
                let shown = typing(self.harness(other), &id, true);
                self.harness(by).act(Action::Type {
                    text: String::new(),
                })?;
                shown?;
                typing(self.harness(other), &id, false)
            }
            Step::ReadReceipt {
                by,
                thread,
                message,
            } => {
                let other = self.other(thread, by)?;
                self.open(other, thread)?;
                self.open(by, thread)?;
                self.harness(by)
                    .wait_for("the message read", PATIENCE, |snapshot| {
                        snapshot
                            .messages
                            .iter()
                            .find(|m| m.timestamp == message)
                            .filter(|m| m.delivery == Some(Delivery::Read))
                            .map(drop)
                    })
            }
            Step::Disappearing { by, thread, text } => self.disappearing(by, thread, &text),
            Step::ChatState { by, thread } => self.chat_state(by, thread),
            Step::Search { by, thread } => self.search(by, thread),
            Step::Block {
                by,
                thread,
                blocked_text,
                text,
            } => self.block(by, thread, &blocked_text, &text),
            Step::CreateGroup { by, members, title } => self.create_group(by, &members, &title),
            Step::Rename { by, thread, title } => {
                self.open(by, thread)?;
                self.harness(by).act(Action::RenameGroup {
                    title: title.clone(),
                })?;
                self.world.threads[thread].title = title;
                self.converge_group(thread)
            }
            Step::Describe {
                by,
                thread,
                description,
            } => {
                self.open(by, thread)?;
                self.harness(by).act(Action::DescribeGroup {
                    description: description.clone(),
                })?;
                self.world.threads[thread].description = Some(description);
                self.converge_group(thread)?;
                self.converge_details(thread)
            }
            Step::GroupPicture {
                by,
                thread,
                picture,
            } => {
                self.open(by, thread)?;
                self.harness(by).act(Action::SetGroupPicture {
                    path: picture.then(|| format!("/files/{PICTURE}")),
                })?;
                self.world.threads[thread].picture = picture;
                self.world.threads[thread].picture_at = now_ms();
                self.converge_group(thread)
            }
            Step::AddMember { by, thread, member } => {
                let candidate = self.b33ps[member].chat();
                self.open(by, thread)?;
                let b33p = self.harness(by);
                b33p.act(Action::OpenAddMembers)?;
                b33p.act(Action::ToggleCandidate {
                    conversation: candidate,
                })?;
                b33p.act(Action::FinishGroupDraft {
                    title: String::new(),
                })?;
                self.joins(thread, member);
                self.world.threads[thread].removed.remove(&member);
                self.converge_group(thread)?;
                self.converge_details(thread)
            }
            Step::RemoveMember { by, thread, member } => {
                let aci = self.world.acis[member].clone();
                self.open(by, thread)?;
                self.harness(by).act(Action::RemoveMember { member: aci })?;
                self.leaves(thread, member)?;
                self.world.threads[thread].removed.insert(member);
                self.converge_group(thread)?;
                self.converge_details(thread)
            }
            Step::MakeAdmin { by, thread, member } => {
                let aci = self.world.acis[member].clone();
                self.open(by, thread)?;
                self.harness(by).act(Action::SetMemberAdmin {
                    member: aci,
                    admin: true,
                })?;
                self.world.threads[thread].admins.insert(member);
                self.converge_details(thread)
            }
            Step::Leave { by, thread } => {
                self.open(by, thread)?;
                self.harness(by).act(Action::LeaveGroup)?;
                self.leaves(thread, by)?;
                self.world.threads[thread].left.insert(by);
                self.converge_group(thread)?;
                self.converge_details(thread)
            }
            Step::JoinByLink {
                by,
                thread,
                admin,
                approval,
            } => self.join_by_link(by, thread, admin, approval),
            Step::Story { by, thread, text } => self.story(by, thread, &text),
            Step::Restart { by } => self.restart(by),
            Step::QuoteThenEdit {
                by,
                thread,
                quoted,
                reply,
                edit,
            } => {
                self.perform(&Step::Reply {
                    by,
                    thread,
                    to: quoted,
                    text: reply,
                })?;
                self.perform(&Step::Edit {
                    by,
                    thread,
                    message: quoted,
                    text: edit,
                })?;
                // Shown from the store now: the quote must still be what it quoted.
                self.restart(by)
            }
            Step::LinkWithoutPreview { by, thread, text } => {
                // presage's line for a preview it tried to build (presage
                // `fetch_link_previews`); without egress every try fails, and says so.
                const TRIED: &str = "failed to build link preview";
                let service = Service::B33p(self.b33ps[by].n);
                let tries = |tour: &Self| -> Result<usize> {
                    Ok(tour.containers.log(service)?.matches(TRIED).count())
                };
                let before = tries(self)?;
                self.open(by, thread)?;
                let started = Instant::now();
                let ts = self.send(by, &text)?;
                self.add(thread, by, ts, &text);
                self.converge_timed(thread, by, started)?;
                let tried = tries(self)? - before;
                // Checked apart (`tour`), as a failure here leaves the chats as the model has them.
                self.looked_up = Some((by, tried));
                Ok(())
            }
        }
    }

    /// The other b33p of a chat of two.
    fn other(&self, thread: usize, k: usize) -> Result<usize> {
        match self.world.threads[thread].pair {
            Some((a, b)) => Ok(if k == a { b } else { a }),
            None => Err("not a chat of two".into()),
        }
    }

    /// `k` joins `thread` now.
    fn joins(&mut self, thread: usize, k: usize) {
        let thread = &mut self.world.threads[thread];
        thread.members.insert(k);
        thread.left.remove(&k);
        thread.joined.insert(k, now_ms());
    }

    /// `k` is no longer in `thread`: its b33p says it left.
    fn leaves(&mut self, thread: usize, k: usize) -> Result<()> {
        let group = &mut self.world.threads[thread];
        group.members.remove(&k);
        group.admins.remove(&k);
        let id = group.id_on(k, self.b33ps);
        self.harness(k)
            .wait_for("the group left", PATIENCE, |snapshot| {
                snapshot
                    .conversations
                    .iter()
                    .any(|chat| chat.id == id && chat.left)
                    .then_some(())
            })
    }

    /// [`Self::converge`], with how long the message from `by` took to show on each of the others,
    /// from `started`, noted.
    fn converge_timed(&mut self, thread: usize, by: usize, started: Instant) -> Result<()> {
        let members: Vec<usize> = self.world.threads[thread].members.iter().copied().collect();
        for k in members.iter().copied().filter(|&k| k != by) {
            self.converge_one(thread, k)?;
            self.delivered.push(started.elapsed().as_secs_f64());
        }
        self.converge_one(thread, by)
    }

    /// Waits until every member of `thread` shows it as the model has it.
    fn converge(&mut self, thread: usize) -> Result<()> {
        let members: Vec<usize> = self.world.threads[thread].members.iter().copied().collect();
        members
            .into_iter()
            .try_for_each(|k| self.converge_one(thread, k))
    }

    /// [`Self::converge`], and a group's title, picture and members as the model has them.
    fn converge_all(&mut self, thread: usize) -> Result<()> {
        self.converge(thread)?;
        if self.world.threads[thread].group.is_some() {
            self.converge_group(thread)?;
            self.converge_details(thread)?;
        }
        Ok(())
    }

    fn converge_one(&mut self, thread: usize, k: usize) -> Result<()> {
        self.open(k, thread)?;
        let expected = self.world.threads[thread].expected(k);
        let since = self.world.threads[thread]
            .joined
            .get(&k)
            .copied()
            .unwrap_or_default();
        let acis = self.world.acis.clone();
        let name = self.world.threads[thread].name();
        let mut last = Vec::new();
        let outcome = self.harness(k).wait_for(
            &format!("{name} on b33p {k} as the model has it"),
            PATIENCE,
            |snapshot| {
                last = seen(snapshot, k, &acis, since);
                (last == expected).then_some(())
            },
        );
        outcome.map_err(|error| Error::Failed(format!("{error}\n{}", difference(&expected, &last))))
    }

    /// Waits until every member of the group `thread` lists it with its title and picture,
    /// answering a request to join it as a member would.
    fn converge_group(&mut self, thread: usize) -> Result<()> {
        let members: Vec<usize> = self.world.threads[thread].members.iter().copied().collect();
        for k in members {
            let group = &self.world.threads[thread];
            let id = group.id_on(k, self.b33ps);
            let (title, picture) = (group.title.clone(), group.picture);
            let b33p = self.harness(k);
            let request = b33p.wait_for(
                &format!("the group {title:?} on b33p {k}"),
                PATIENCE,
                |snapshot| {
                    snapshot
                        .conversations
                        .iter()
                        .find(|chat| chat.id == id && !chat.left)
                        .map(|chat| chat.request)
                },
            )?;
            if request {
                select(b33p, &id)?;
                b33p.act(Action::Respond {
                    answer: Answer::Accept,
                })?;
            }
            // One that joined after the picture last changed isn't shown it until b33p restarts
            // (pending/tour.md, finding 11): told apart, the tour going on.
            let group = &self.world.threads[thread];
            let came_after = group
                .joined
                .get(&k)
                .is_some_and(|&at| at > group.picture_at);
            let shown = |chat: &b33p_harness::ConversationSnapshot, with_picture: bool| {
                chat.title == title && !chat.request && (!with_picture || chat.avatar == picture)
            };
            let b33p = self.harness(k);
            b33p.wait_for(
                &format!("the group {title:?} with its picture {picture} on b33p {k}"),
                PATIENCE,
                |snapshot| {
                    snapshot
                        .conversations
                        .iter()
                        .find(|chat| chat.id == id)
                        .filter(|chat| shown(chat, !came_after))
                        .map(drop)
                },
            )?;
            if came_after {
                let pictured = b33p.wait_for("the picture", DETAILS_AGAIN, |snapshot| {
                    snapshot
                        .conversations
                        .iter()
                        .find(|chat| chat.id == id)
                        .filter(|chat| chat.avatar == picture)
                        .map(drop)
                });
                if pictured.is_err() {
                    self.stale_pictures.push(format!(
                        "b33p {k} shows {title:?} with the picture {}, not {picture}",
                        !picture
                    ));
                }
            }
        }
        Ok(())
    }

    /// Waits until every member of the group `thread` shows its members, admins and description
    /// as the model has them, in its details.
    fn converge_details(&mut self, thread: usize) -> Result<()> {
        let group = &self.world.threads[thread];
        let wanted: BTreeMap<String, bool> = group
            .members
            .iter()
            .map(|&k| (self.world.acis[k].clone(), group.admins.contains(&k)))
            .collect();
        let description = group.description.clone();
        let members: Vec<usize> = group.members.iter().copied().collect();
        for k in members {
            self.open(k, thread)?;
            let b33p = self.harness(k);
            let mut last = None;
            // The details are read as they open, and not again while open (b33p-core
            // `Command::LoadDetails`): opened again until they show the change, as a person
            // would, since it may reach this b33p after they opened.
            let deadline = Instant::now() + PATIENCE;
            let shown = loop {
                b33p.act(Action::OpenDetails)?;
                let shown = b33p.wait_for(
                    &format!("the group's members and description on b33p {k}"),
                    DETAILS_AGAIN,
                    |snapshot| {
                        let details = snapshot.details.as_ref()?;
                        let members: BTreeMap<String, bool> = details
                            .members
                            .iter()
                            .map(|member| (member.service_id.clone(), member.admin))
                            .collect();
                        let matches = members == wanted && details.description == description;
                        last = Some((members, details.description.clone()));
                        matches.then_some(())
                    },
                );
                b33p.act(Action::CloseDetails)?;
                if shown.is_ok() || Instant::now() >= deadline {
                    break shown;
                }
            };
            shown.map_err(|error| {
                Error::Failed(format!(
                    "{error}\nwanted {wanted:?} and {description:?}, shown {last:?}"
                ))
            })?;
        }
        Ok(())
    }

    /// A timer set in a chat of two, a message sent with it gone from both sides, the timer off.
    fn disappearing(&mut self, by: usize, thread: usize, text: &str) -> Result<()> {
        let other = self.other(thread, by)?;
        let timer = |tour: &mut Self, seconds: u32| -> Result<()> {
            let id = tour.world.threads[thread].id_on(by, tour.b33ps);
            tour.harness(by).act(Action::SetTimer {
                conversation: id,
                seconds,
            })?;
            for k in [by, other] {
                let id = tour.world.threads[thread].id_on(k, tour.b33ps);
                tour.harness(k).wait_for(
                    &format!("the timer at {seconds} s on b33p {k}"),
                    PATIENCE,
                    |snapshot| {
                        snapshot
                            .conversations
                            .iter()
                            .any(|chat| chat.id == id && chat.expire_timer == seconds)
                            .then_some(())
                    },
                )?;
            }
            Ok(())
        };
        timer(self, TIMER)?;
        self.open(by, thread)?;
        let ts = self.send(by, text)?;
        self.open(other, thread)?;
        for k in [other, by] {
            self.open(k, thread)?;
            self.harness(k).wait_for(
                &format!("the message gone on b33p {k}"),
                EXPIRY_PATIENCE,
                |snapshot| (!snapshot.messages.iter().any(|m| m.timestamp == ts)).then_some(()),
            )?;
        }
        timer(self, 0)
    }

    /// Pins, mutes and marks unread `thread` on `by`'s b33p, then archives it, and undoes all.
    fn chat_state(&mut self, by: usize, thread: usize) -> Result<()> {
        let id = self.world.threads[thread].id_on(by, self.b33ps);
        let b33p = self.harness(by);
        let chat =
            |b33p: &mut Harness,
             what: &str,
             wanted: &dyn Fn(&b33p_harness::ConversationSnapshot) -> bool| {
                b33p.wait_for(&format!("the chat {what}"), PATIENCE, |snapshot| {
                    snapshot
                        .conversations
                        .iter()
                        .any(|chat| chat.id == id && wanted(chat))
                        .then_some(())
                })
            };
        b33p.act(Action::Back)?;
        b33p.act(Action::TogglePin {
            conversation: id.clone(),
        })?;
        b33p.act(Action::Mute {
            conversation: id.clone(),
            minutes: 60,
        })?;
        b33p.act(Action::MarkRead {
            conversation: id.clone(),
            read: false,
        })?;
        chat(b33p, "pinned, muted and marked unread", &|chat| {
            chat.pinned && chat.muted && chat.marked_unread
        })?;
        b33p.act(Action::TogglePin {
            conversation: id.clone(),
        })?;
        b33p.act(Action::Archive {
            conversation: id.clone(),
            archived: true,
        })?;
        b33p.act(Action::ShowArchived { show: true })?;
        let archived = chat(b33p, "archived", &|chat| chat.archived && !chat.pinned);
        b33p.act(Action::Archive {
            conversation: id.clone(),
            archived: false,
        })?;
        b33p.act(Action::ShowArchived { show: false })?;
        archived?;
        b33p.act(Action::Mute {
            conversation: id.clone(),
            minutes: 0,
        })?;
        b33p.act(Action::MarkRead {
            conversation: id.clone(),
            read: true,
        })?;
        chat(b33p, "as it was", &|chat| {
            !chat.pinned && !chat.muted && !chat.marked_unread && !chat.archived
        })
    }

    /// Searches `by`'s chat list for `thread`'s title as it shows it: it is among what is found,
    /// and all of that has it.
    fn search(&mut self, by: usize, thread: usize) -> Result<()> {
        let id = self.world.threads[thread].id_on(by, self.b33ps);
        let b33p = self.harness(by);
        let title = b33p
            .snapshot()?
            .conversations
            .into_iter()
            .find(|chat| chat.id == id)
            .map(|chat| chat.title)
            .ok_or_else(|| Error::Failed(format!("no {id} in the chat list")))?;
        b33p.act(Action::Search {
            text: title.clone(),
        })?;
        let found = b33p.wait_for(&format!("{title:?} found"), PATIENCE, |snapshot| {
            (snapshot.conversations.iter().any(|chat| chat.id == id)
                && snapshot
                    .conversations
                    .iter()
                    .all(|chat| chat.title.to_lowercase().contains(&title.to_lowercase())))
            .then_some(())
        });
        b33p.act(Action::Search {
            text: String::new(),
        })?;
        found
    }

    /// `by` blocks the other of `thread`, which writes `blocked_text`; `by` unblocks it, and it
    /// writes `text`: `by` shows only the second.
    fn block(&mut self, by: usize, thread: usize, blocked_text: &str, text: &str) -> Result<()> {
        let other = self.other(thread, by)?;
        let id = self.world.threads[thread].id_on(by, self.b33ps);
        let blocked = |tour: &mut Self, blocked: bool| -> Result<()> {
            let b33p = tour.harness(by);
            b33p.act(Action::Block {
                conversation: id.clone(),
                blocked,
            })?;
            b33p.wait_for(&format!("blocked {blocked}"), PATIENCE, |snapshot| {
                snapshot
                    .conversations
                    .iter()
                    .any(|chat| chat.id == id && chat.blocked == blocked)
                    .then_some(())
            })?;
            // The chat list shows the block at once, but what receiving goes by is written
            // when the backend takes the command, after those before it (b33p-core
            // `Command::Block`): the details, which it loads in the same turn, come after.
            b33p.act(Action::Select {
                conversation: id.clone(),
            })?;
            b33p.act(Action::OpenDetails)?;
            let loaded = b33p.wait_for("the details, after the block", PATIENCE, |snapshot| {
                snapshot.details.as_ref().map(drop)
            });
            b33p.act(Action::CloseDetails)?;
            loaded
        };
        blocked(self, true)?;
        self.open(other, thread)?;
        let ts = self.send(other, blocked_text)?;
        self.add(thread, other, ts, blocked_text).hidden.insert(by);
        // The server took it, but `by` may not have yet, and taken after the unblock it would
        // show, rightly; nor does `by` say it got it (no receipt to someone blocked). A third
        // b33p writes to `by` after it: the server hands a device its messages in turn, so once
        // `by` shows that, it has dealt with the blocked one.
        let third = (0..self.b33ps.len())
            .find(|&k| k != by && k != other)
            .ok_or("a third b33p")?;
        let beside = self
            .world
            .threads
            .iter()
            .position(|t| t.pair == Some((by.min(third), by.max(third))))
            .ok_or("the chat of the third b33p")?;
        // Not drawn: a seed then replays what came after as it did before the step had it.
        self.world.texts += 1;
        let marker = format!(
            "tour {}: after the blocked one, from {third}",
            self.world.texts
        );
        self.open(third, beside)?;
        let ts = self.send(third, &marker)?;
        self.add(beside, third, ts, &marker);
        self.converge_one(beside, by)?;
        blocked(self, false)?;
        let started = Instant::now();
        let ts = self.send(other, text)?;
        self.add(thread, other, ts, text);
        self.converge_timed(thread, other, started)
    }

    fn create_group(&mut self, by: usize, members: &[usize], title: &str) -> Result<()> {
        let chats: Vec<String> = members.iter().map(|&k| self.b33ps[k].chat()).collect();
        let b33p = self.harness(by);
        b33p.act(Action::CreateGroup {
            title: title.to_owned(),
            members: chats,
        })?;
        let id = b33p.wait_for(&format!("the group {title:?}"), PATIENCE, |snapshot| {
            snapshot
                .conversations
                .iter()
                .find(|chat| chat.group && chat.title == title)
                .map(|chat| chat.id.clone())
        })?;
        self.world.threads.push(Thread {
            pair: None,
            group: Some(id),
            title: title.to_owned(),
            members: members.iter().copied().chain([by]).collect(),
            admins: BTreeSet::from([by]),
            removed: BTreeSet::new(),
            left: BTreeSet::new(),
            joined: BTreeMap::new(),
            description: None,
            picture: false,
            picture_at: 0,
            messages: Vec::new(),
        });
        let thread = self.world.threads.len() - 1;
        self.converge_group(thread)?;
        self.converge_details(thread)
    }

    /// `admin` turns `thread`'s link on, with `approval` or for anyone; `by` joins by it. With
    /// approval `by`, who left, asks again; the request reaches `admin` without a fetch, as `by`
    /// tells the members it knew, and `admin` approves. The link goes off after.
    fn join_by_link(
        &mut self,
        by: usize,
        thread: usize,
        admin: usize,
        approval: bool,
    ) -> Result<()> {
        let aci = self.world.acis[by].clone();
        let title = self.world.threads[thread].title.clone();
        let access = if approval {
            InviteLinkAccess::Approval
        } else {
            InviteLinkAccess::Anyone
        };
        self.open(admin, thread)?;
        let b33p = self.harness(admin);
        b33p.act(Action::SetInviteLink { access })?;
        b33p.act(Action::OpenDetails)?;
        let link = b33p.wait_for("the invite link", PATIENCE, |snapshot| {
            let details = snapshot.details.as_ref()?;
            (details.invite_link_access == access)
                .then(|| details.invite_link.clone())
                .flatten()
        });
        b33p.act(Action::CloseDetails)?;
        let link = link?;

        let joining = self.harness(by);
        joining.act(Action::OpenJoinGroup {
            link: Some(link.clone()),
        })?;
        joining.wait_for("the group behind the link", PATIENCE, |snapshot| {
            let join = snapshot.join.as_ref()?;
            (join.title.as_deref() == Some(title.as_str())
                && join.approval == approval
                && !join.busy)
                .then_some(())
        })?;
        joining.act(Action::JoinGroup)?;
        let id = self.world.threads[thread].id_on(by, self.b33ps);
        let joining = self.harness(by);
        joining.wait_for(
            if approval {
                "the request sent"
            } else {
                "the group joined"
            },
            PATIENCE,
            |snapshot| {
                snapshot
                    .conversations
                    .iter()
                    .any(|chat| {
                        chat.id == id
                            && if approval {
                                chat.requested
                            } else {
                                !chat.left && !chat.requested
                            }
                    })
                    .then_some(())
            },
        )?;
        joining.act(Action::CloseJoinGroup)?;

        if approval {
            let b33p = self.harness(admin);
            let deadline = Instant::now() + PATIENCE;
            let asked = loop {
                b33p.act(Action::OpenDetails)?;
                let asked =
                    b33p.wait_for("the request in the details", DETAILS_AGAIN, |snapshot| {
                        snapshot
                            .details
                            .as_ref()?
                            .requests
                            .iter()
                            .any(|request| request.contains(aci.as_str()))
                            .then_some(())
                    });
                b33p.act(Action::CloseDetails)?;
                if asked.is_ok() || Instant::now() >= deadline {
                    break asked;
                }
            };
            asked?;
            b33p.act(Action::AnswerJoinRequest {
                member: aci,
                approve: true,
            })?;
        }
        self.joins(thread, by);
        self.converge_group(thread)?;
        self.converge_details(thread)?;
        self.open(admin, thread)?;
        self.harness(admin).act(Action::SetInviteLink {
            access: InviteLinkAccess::Off,
        })?;
        Ok(())
    }

    /// `by` posts a text story to the group `thread`; every other member shows it.
    fn story(&mut self, by: usize, thread: usize, text: &str) -> Result<()> {
        let group = self.world.threads[thread]
            .group
            .clone()
            .ok_or("a story to a chat of two")?;
        self.harness(by).act(Action::PostStoryText {
            group,
            text: text.to_owned(),
        })?;
        // b33p files a group's stories under the group (b33p-core `stories::on_story`).
        let author = self.world.threads[thread]
            .group
            .clone()
            .ok_or("a story to a chat of two")?;
        let others: Vec<usize> = self.world.threads[thread]
            .members
            .iter()
            .copied()
            .filter(|&k| k != by)
            .collect();
        for k in others {
            let b33p = self.harness(k);
            // Opened again until it shows: before the story came, there was none to open.
            let deadline = Instant::now() + PATIENCE;
            let shown = loop {
                b33p.act(Action::OpenStory {
                    conversation: author.clone(),
                })?;
                let shown = b33p.wait_for(
                    &format!("the story on b33p {k}"),
                    Duration::from_secs(5),
                    |snapshot| {
                        snapshot
                            .story
                            .as_ref()
                            .is_some_and(|story| story.text.as_deref() == Some(text))
                            .then_some(())
                    },
                );
                if shown.is_ok() || Instant::now() >= deadline {
                    break shown;
                }
            };
            b33p.act(Action::CloseStory)?;
            shown?;
        }
        Ok(())
    }

    /// Kills `by`'s b33p, starts it again, and checks every chat it is in.
    fn restart(&mut self, by: usize) -> Result<()> {
        let n = self.b33ps[by].n;
        let service = Service::B33p(n);
        let killed = Instant::now();
        let since = process::now()?;
        self.containers.kill(service)?;
        // The pod may have started it again already (`restartPolicy: Always`).
        self.containers.start(service)?;
        let mut b33p = started(self.containers, n, &self.sockets.agent(n), since)?;
        self.restarts.push(killed.elapsed().as_secs_f64());
        b33p.act(Action::SetWindowFocused { focused: true })?;
        self.b33ps[by].b33p = b33p;
        let threads = self.world.threads_of(by, true, true);
        threads
            .into_iter()
            .try_for_each(|thread| self.converge_one(thread, by))
    }

    fn sample_all(&mut self) -> Result<()> {
        let at = self.started.elapsed().as_secs_f64();
        for k in 0..self.b33ps.len() {
            let service = Service::B33p(self.b33ps[k].n);
            let taken = sample(self.containers, service, &mut self.b33ps[k].b33p)?;
            self.samples[k].push(taken);
            self.sampled_at[k].push(at);
        }
        self.sampled = Instant::now();
        Ok(())
    }

    /// What each b33p shows now, as `b33p-<k>.json` in the record: what a failed step left.
    fn keep_snapshots(&mut self, checks: &mut Checks) {
        for k in 0..self.b33ps.len() {
            let snapshot = self.harness(k).snapshot().and_then(|snapshot| {
                serde_json::to_string_pretty(&snapshot).context("b33p's snapshot")
            });
            checks.kept.write(
                &format!("b33p-{k}.json"),
                &snapshot.unwrap_or_else(|error| error.to_string()),
            );
        }
    }

    /// `tour.json` into the record.
    fn write(&self, checks: &mut Checks, seed: u64) {
        let samples = self
            .samples
            .iter()
            .zip(&self.sampled_at)
            .map(|(samples, at)| {
                samples
                    .iter()
                    .zip(at)
                    .map(|(sample, &at)| SampleRecord {
                        at,
                        memory_mib: sample.memory as f64 / f64::from(1 << 20),
                        files: sample.files,
                        answer_ms: sample.answer.as_secs_f64() * 1000.0,
                    })
                    .collect()
            })
            .collect();
        let report = Report {
            seed,
            b33ps: self.b33ps.len(),
            link_seconds: self
                .b33ps
                .iter()
                .map(|each| each.linked_in.as_secs_f64())
                .collect(),
            steps: &self.steps,
            kinds: self
                .durations
                .iter()
                .map(|(kind, seconds)| (*kind, Spread::of(seconds)))
                .collect(),
            delivered_seconds: Spread::of(&self.delivered),
            restart_seconds: Spread::of(&self.restarts),
            samples,
        };
        checks.kept.write(
            "tour.json",
            &serde_json::to_string_pretty(&report).unwrap_or_else(|error| error.to_string()),
        );
    }

    fn summary(&self) {
        println!("  each kind of step: median, 95th percentile, most (s)");
        for (kind, seconds) in &self.durations {
            let (median, p95, most) = percentiles(seconds);
            println!(
                "    {kind}: {median:.1}, {p95:.1}, {most:.1} ({} times)",
                seconds.len()
            );
        }
        let (median, p95, most) = percentiles(&self.delivered);
        println!(
            "  a message showing on another b33p: {median:.1}, {p95:.1}, {most:.1} s ({} times)",
            self.delivered.len()
        );
        for (k, samples) in self.samples.iter().enumerate() {
            if let (Some(first), Some(last)) = (samples.first(), samples.last()) {
                println!("  b33p {k}: from {first} to {last}");
            }
        }
    }
}

/// Waits for `k`'s newest message of its own in its open chat, sent at `before` or later, to
/// leave "sending"; its timestamp. Fails if it failed.
fn sent_since(b33p: &mut Harness, k: usize, before: u64) -> Result<u64> {
    let (ts, delivery) = b33p.wait_for(
        &format!("the server taking b33p {k}'s message"),
        PATIENCE,
        |snapshot| {
            snapshot
                .messages
                .iter()
                .filter(|m| m.outgoing && !m.notice && m.timestamp >= before)
                .max_by_key(|m| m.timestamp)
                .and_then(|m| Some((m.timestamp, m.delivery?)))
                .filter(|(_, delivery)| *delivery != Delivery::Sending)
        },
    )?;
    match delivery {
        Delivery::Failed | Delivery::Refused => Err(Error::Failed(format!(
            "b33p {k}'s message of {ts} {delivery:?}"
        ))),
        _ => Ok(ts),
    }
}

/// Waits until `b33p` shows someone typing in the chat `id`, or no one.
fn typing(b33p: &mut Harness, id: &str, someone: bool) -> Result<()> {
    b33p.wait_for(
        &format!("typing {someone} in the chat"),
        PATIENCE,
        |snapshot| {
            snapshot
                .conversations
                .iter()
                .any(|chat| chat.id == id && chat.typing.is_empty() != someone)
                .then_some(())
        },
    )
}

/// How `expected` and `shown` differ, message by message.
fn difference(expected: &[Seen], shown: &[Seen]) -> String {
    let mut lines = Vec::new();
    let mut timestamps: Vec<u64> = expected.iter().chain(shown).map(|seen| seen.ts).collect();
    timestamps.sort_unstable();
    timestamps.dedup();
    for ts in timestamps {
        let wanted = expected.iter().find(|seen| seen.ts == ts);
        let have = shown.iter().find(|seen| seen.ts == ts);
        if wanted != have {
            lines.push(format!(
                "  {ts}: wanted {wanted:?}\n  {ts}: shown  {have:?}"
            ));
        }
    }
    if lines.is_empty() {
        "the same messages".to_owned()
    } else {
        lines.join("\n")
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// The median, the 95th percentile and the most of `values`; zeros of none.
fn percentiles(values: &[f64]) -> (f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |fraction: f64| {
        let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    };
    (at(0.5), at(0.95), sorted[sorted.len() - 1])
}

/// What the record's `tour.json` holds.
#[derive(Serialize)]
struct Report<'tour> {
    seed: u64,
    b33ps: usize,
    /// Each b33p's, from its harness listening to its chats.
    link_seconds: Vec<f64>,
    steps: &'tour [StepRecord],
    /// How long each kind of step took.
    kinds: BTreeMap<&'static str, Spread>,
    /// From a message sent to it showing on each other b33p of its chat.
    delivered_seconds: Spread,
    /// From a b33p killed to its chats shown again.
    restart_seconds: Spread,
    /// Each b33p's.
    samples: Vec<Vec<SampleRecord>>,
}

#[derive(Serialize)]
struct StepRecord {
    number: usize,
    kind: &'static str,
    step: String,
    seconds: f64,
    passed: bool,
}

#[derive(Serialize)]
struct SampleRecord {
    /// Seconds into the tour.
    at: f64,
    memory_mib: f64,
    files: u64,
    answer_ms: f64,
}

#[derive(Serialize)]
struct Spread {
    count: usize,
    median: f64,
    p95: f64,
    max: f64,
}

impl Spread {
    fn of(values: &[f64]) -> Self {
        let (median, p95, max) = percentiles(values);
        Self {
            count: values.len(),
            median,
            p95,
            max,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One message's reactions are compared as the viewer sees them: its own, or someone's.
    #[test]
    fn a_reaction_is_ours_only_to_its_reactor() {
        let thread = Thread {
            pair: Some((0, 1)),
            group: None,
            title: String::new(),
            members: BTreeSet::from([0, 1]),
            admins: BTreeSet::new(),
            removed: BTreeSet::new(),
            left: BTreeSet::new(),
            joined: BTreeMap::new(),
            description: None,
            picture: false,
            picture_at: 0,
            messages: vec![Expected {
                ts: 5,
                from: 0,
                text: "hi".to_owned(),
                edited: false,
                deleted: false,
                reactions: BTreeMap::from([(1, "👍".to_owned())]),
                quote: None,
                attachments: Vec::new(),
                hidden: BTreeSet::from([0]),
                audience: BTreeSet::from([0, 1]),
            }],
        };
        assert!(thread.expected(0).is_empty(), "hidden for its sender");
        assert_eq!(
            thread.expected(1)[0].reactions,
            vec![("👍".to_owned(), true)]
        );
    }

    /// Three b33ps, a message in the chat of 0 and 1, and a group of 0 (its admin) and 1 that 2
    /// left.
    fn world(seed: u64) -> World {
        let mut world = World::new(vec!["a".into(), "b".into(), "c".into()], seed);
        world.threads[0].messages.push(Expected {
            ts: 1,
            from: 0,
            text: "tour 0: hi".to_owned(),
            edited: false,
            deleted: false,
            reactions: BTreeMap::new(),
            quote: None,
            attachments: Vec::new(),
            hidden: BTreeSet::new(),
            audience: BTreeSet::from([0, 1]),
        });
        world.threads.push(Thread {
            pair: None,
            group: Some("g".to_owned()),
            title: "tour group 1".to_owned(),
            members: BTreeSet::from([0, 1]),
            admins: BTreeSet::from([0]),
            removed: BTreeSet::new(),
            left: BTreeSet::from([2]),
            joined: BTreeMap::new(),
            description: None,
            picture: false,
            picture_at: 0,
            messages: Vec::new(),
        });
        world
    }

    /// With a message and a group there, every kind of step can be drawn.
    #[test]
    fn every_kind_of_step_can_go() {
        for &kind in Kind::VARIANTS {
            assert!(world(7).plan(kind).is_some(), "{kind:?} can't go");
        }
    }

    /// A seed draws the same steps again; another, others.
    #[test]
    fn a_seed_draws_the_same_tour() {
        let tour = |seed| {
            let mut world = world(seed);
            let mut left: Vec<Kind> = Kind::VARIANTS.repeat(2);
            let mut drawn = Vec::new();
            while let Some((kind, step)) = world.draw(&mut left) {
                drawn.push(format!("{kind:?} {step:?}"));
            }
            assert!(left.is_empty(), "{left:?} never drawn");
            drawn
        };
        assert_eq!(tour(11), tour(11));
        assert_ne!(tour(11), tour(13));
    }

    /// When only a join by link is left and nobody may join (2 was removed), the step drawn
    /// instead lets someone leave or is another that may help, never nothing.
    #[test]
    fn a_stuck_tour_draws_what_may_unstick_it() {
        let mut world = world(5);
        world.threads[3].left.clear();
        world.threads[3].removed.insert(2);
        assert!(world.plan(Kind::JoinByLink).is_none());
        let mut left = vec![Kind::JoinByLink];
        assert!(world.draw(&mut left).is_none());
        let (kind, _) = world.filler().expect("a filler");
        assert!(
            [Kind::Leave, Kind::AddMember, Kind::CreateGroup].contains(&kind),
            "{kind:?}"
        );
    }

    /// Nothing that needs a message or a group is drawn before there is one.
    #[test]
    fn a_step_waits_for_what_it_needs() {
        let mut world = World::new(vec!["a".into(), "b".into()], 3);
        for kind in [
            Kind::Edit,
            Kind::React,
            Kind::Rename,
            Kind::Leave,
            Kind::Story,
        ] {
            assert!(
                world.plan(kind).is_none(),
                "{kind:?} with nothing to act on"
            );
        }
    }

    #[test]
    fn percentiles_of_a_few() {
        assert_eq!(percentiles(&[]), (0.0, 0.0, 0.0));
        assert_eq!(percentiles(&[3.0, 1.0, 2.0]), (2.0, 3.0, 3.0));
    }
}
