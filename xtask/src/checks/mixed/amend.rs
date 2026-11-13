//! What the mixed phase adds to its texts: files, reactions and edits, each sent by one client to
//! the others while the others write (docs/testbox-clients-run-2026-11-13.md).
//!
//! A file is sent from every client to b33p and to every other client at once, and by b33p to
//! each; a receiver that can tell what it received (b33p, signal-cli, signalmeow, Flare print or
//! keep it; presage-cli saves it in a directory it deletes at exit) holds the file's hash to the
//! one the sender's bytes have. Reactions and edits name their target by the time it was sent at
//! and its author, so what is held to account is that each lands on the message it is for while
//! bursts of other messages arrive: b33p's reaction on, and edit of, a message of each client's;
//! the clients that can (signal-cli, signalmeow, Flare) react to b33p's message and edit their
//! own.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use b33p_harness::{Action, AttachmentKind, MessageSnapshot};

use super::{Kind, MARK, Peer, burst, together};
use crate::checks::attachments::incoming_attachment;
use crate::checks::b33p::{b33p_shows, edit, select, type_and_send};
use crate::checks::signal_cli::Received;
use crate::checks::signalmeow::sha256;
use crate::checks::stress::{all_texts, in_order_once, save, settled};
use crate::checks::{Checks, Failure, PATIENCE};
use crate::harness::Harness;
use crate::pod::{B33P, Pod};

/// A reaction or an edit a client reported receiving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Amend {
    /// `emoji`, to the message sent at `target`, or (for presage-cli, which prints the text of
    /// the message, not its time) the one whose text was `body`.
    Reaction {
        emoji: String,
        target: Option<u64>,
        body: Option<String>,
    },
    /// `text` in place of the message sent at `target`.
    Edit { target: Option<u64>, text: String },
}

/// What a client reported of reactions, edits and files, besides the texts it printed.
#[derive(Default)]
pub(super) struct Learned {
    amends: Vec<Amend>,
    /// `<bytes>:<sha256>` of each file received, downloaded and decrypted.
    files: Vec<String>,
}

/// What a client's output says of reactions, edits and files, in the words of each of them: the
/// drivers print `From <aci> @ <ms>: <text> [reaction <emoji> to <ms>]` (signalmeow),
/// `Reaction <emoji> from <aci> to <ms>` (Flare), `Reacted with <emoji> to message: "<text>"`
/// (presage-cli), `Edit from <aci> of <ms>: <text>` (signalmeow and Flare) and `Attachment of
/// <ms>: <name> <type> <n> bytes sha256:<hex>` (both).
fn parse(output: &str) -> Learned {
    let mut learned = Learned::default();
    for line in output.lines() {
        if let Some(at) = line.find(" bytes sha256:") {
            let size = line[..at].rsplit(' ').next().unwrap_or_default();
            let hex = line[at + " bytes sha256:".len()..].trim();
            learned.files.push(format!("{size}:{hex}"));
        }
        if let Some(at) = line.rfind(" [reaction ") {
            let inner = line[at + " [reaction ".len()..]
                .trim_end()
                .trim_end_matches(']');
            if let Some((emoji, target)) = inner.split_once(" to ") {
                learned.amends.push(Amend::Reaction {
                    emoji: emoji.to_owned(),
                    target: target.trim().parse().ok(),
                    body: None,
                });
            }
        }
        if let Some(rest) = line.strip_prefix("Reaction ")
            && let Some((emoji, rest)) = rest.split_once(" from ")
        {
            learned.amends.push(Amend::Reaction {
                emoji: emoji.to_owned(),
                target: rest
                    .rsplit_once(" to ")
                    .and_then(|(_, target)| target.trim().parse().ok()),
                body: None,
            });
        }
        if let Some(at) = line.find("Reacted with ") {
            let rest = &line[at + "Reacted with ".len()..];
            if let Some((emoji, body)) = rest.split_once(" to message: \"") {
                learned.amends.push(Amend::Reaction {
                    emoji: emoji.to_owned(),
                    target: None,
                    body: Some(body.trim_end().trim_end_matches('"').to_owned()),
                });
            }
        }
        if let Some(rest) = line.strip_prefix("Edit from ")
            && let Some((_, rest)) = rest.split_once(" of ")
            && let Some((target, text)) = rest.split_once(": ")
        {
            learned.amends.push(Amend::Edit {
                target: target.trim().parse().ok(),
                text: text.trim_end().to_owned(),
            });
        }
    }
    learned
}

impl Peer<'_> {
    /// Notes what `output` (what a client printed) says of reactions, edits and files.
    pub(super) fn learn(&self, output: &str) {
        let learned = parse(output);
        let mut mine = self
            .learned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        mine.amends.extend(learned.amends);
        mine.files.extend(learned.files);
    }

    /// [`Self::learn`] for what signal-cli received: its attachments saved and decrypted, which it
    /// reads out of the container.
    pub(super) fn learn_received(&self, received: &[Received]) -> Result<(), Failure> {
        let Kind::SignalCli(client) = &self.kind else {
            return Ok(());
        };
        let mut learned = Learned::default();
        for message in received {
            if let Some((emoji, target)) = &message.reaction {
                learned.amends.push(Amend::Reaction {
                    emoji: emoji.clone(),
                    target: Some(*target),
                    body: None,
                });
            }
            if let Some((target, text)) = &message.edit {
                learned.amends.push(Amend::Edit {
                    target: Some(*target),
                    text: text.clone(),
                });
            }
            for attachment in &message.attachments {
                if let Some(id) = &attachment.id {
                    let bytes = client.attachment(id)?;
                    learned
                        .files
                        .push(format!("{}:{}", bytes.len(), sha256(&bytes)));
                }
            }
        }
        let mut mine = self
            .learned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        mine.amends.extend(learned.amends);
        mine.files.extend(learned.files);
        Ok(())
    }

    /// Whether the client's output says what it received of a file: presage-cli's doesn't.
    fn tells_files(&self) -> bool {
        !matches!(self.kind, Kind::Presage(_))
    }

    fn has_file(&self, key: &str) -> bool {
        self.learned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .files
            .iter()
            .any(|file| file == key)
    }

    /// Whether the driver of this client can react and edit (presage-cli's can't).
    fn can_amend(&self) -> bool {
        !matches!(self.kind, Kind::Presage(_))
    }

    fn send_file(&self, to: &str, text: &str, path: &str) -> Result<(), Failure> {
        match &self.kind {
            Kind::Presage(client) => Ok(client.send_file(to, text, path)?),
            Kind::SignalCli(client) => client
                .send_file(to, text, path)
                .map(drop)
                .map_err(Into::into),
            Kind::Signalmeow(client) => Ok(client.send_file(to, path, text)?),
            Kind::Flare(client) => Ok(client.send_file(to, path, text)?),
        }
    }

    /// Sends `text` to `to`; the time it was sent at, which a reaction or an edit names.
    fn send_stamped(&self, to: &str, text: &str) -> Result<u64, Failure> {
        match &self.kind {
            Kind::Presage(_) => Err("presage-cli's send prints no time that a test uses".into()),
            Kind::SignalCli(client) => Ok(client.send(to, text)?),
            Kind::Signalmeow(client) => Ok(client.send_stamped(to, text)?),
            Kind::Flare(client) => Ok(client.send_stamped(to, text)?),
        }
    }

    fn react(&self, to: &str, author: &str, target: u64, emoji: &str) -> Result<(), Failure> {
        match &self.kind {
            Kind::Presage(_) => Err("presage-cli doesn't react".into()),
            Kind::SignalCli(client) => Ok(client.react(to, author, target, emoji)?),
            Kind::Signalmeow(client) => Ok(client.react(to, author, target, emoji)?),
            Kind::Flare(client) => Ok(client.react(to, author, target, emoji)?),
        }
    }

    /// Whether this client sends edits: presage-cli's driver has no command for it, and Flare's
    /// backend panics on one (`todo!()` in flare-types `convert/to_presage.rs`: it converts
    /// only texts, reactions and deletions to send). Both receive them.
    fn can_edit(&self) -> bool {
        matches!(self.kind, Kind::SignalCli(_) | Kind::Signalmeow(_))
    }

    fn edit(&self, to: &str, target: u64, text: &str) -> Result<(), Failure> {
        match &self.kind {
            Kind::SignalCli(client) => Ok(client.edit(to, target, text)?),
            Kind::Signalmeow(client) => Ok(client.edit(to, target, text)?),
            Kind::Presage(_) | Kind::Flare(_) => {
                Err(format!("{} sends no edits", self.label).into())
            }
        }
    }

    /// Whether the client reported a reaction with `emoji` to the message sent at `target`, whose
    /// text was one of `bodies` (what presage-cli names it by).
    fn saw_reaction(&self, emoji: &str, target: u64, bodies: &[&str]) -> bool {
        self.learned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .amends
            .iter()
            .any(|amend| match amend {
                Amend::Reaction {
                    emoji: got,
                    target: at,
                    body,
                } => {
                    got == emoji
                        && (*at == Some(target)
                            || body.as_deref().is_some_and(|body| bodies.contains(&body)))
                }
                Amend::Edit { .. } => false,
            })
    }

    /// Whether the client reported `text` in place of the message sent at `target`: presage-cli
    /// prints an edit as a message of its own, so the text printed is all it says.
    fn saw_edit(&self, text: &str, target: u64) -> bool {
        let named = self
            .learned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .amends
            .iter()
            .any(|amend| {
                matches!(amend, Amend::Edit { target: at, text: got }
                    if got == text && at.is_none_or(|at| at == target))
            });
        named
            || (matches!(self.kind, Kind::Presage(_))
                && self.texts_seen().iter().any(|seen| seen == text))
    }

    /// Receives until `done` holds of what the client has reported, and fails with `what` if it
    /// doesn't within the patience.
    fn pull_until(&self, what: &str, done: impl Fn(&Self) -> bool) -> Result<(), Failure> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.pull()?;
            if done(self) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("{} never received {what}", self.label).into());
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}

/// What the stages know of the box.
pub(super) struct Mix<'a, 'pod> {
    pub(super) peers: &'a [Peer<'pod>],
    /// b33p's chat with each peer, in the order of `peers`.
    pub(super) chats: &'a [String],
    pub(super) primary_aci: &'a str,
    pub(super) containers: &'a Pod,
    /// The host's directory the containers see as `/files`.
    pub(super) files: &'a Path,
}

/// The name of the file `from` sends to `to` in `round`.
fn file_name(round: usize, from: &str, to: &str) -> String {
    format!("mixed-r{round}-{from}-{to}.bin")
}

/// Its text, which each side finds the message by.
fn file_text(round: usize, from: &str, to: &str) -> String {
    format!("{MARK}: r{round} file {from} to {to}")
}

/// The file's bytes: not compressible, and told apart from every other's by its name, which
/// seeds them; its size varies from 3 KB to 150 KB, off the padding's and the chunks' edges.
fn file_bytes(name: &str) -> Vec<u8> {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        state = (state ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    let len = 3_000 + (state % 147_000) as usize;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// `<bytes>:<sha256>` of the file's bytes, as the clients' outputs are keyed.
fn file_key(name: &str) -> String {
    let bytes = file_bytes(name);
    format!("{}:{}", bytes.len(), sha256(&bytes))
}

/// Every client sends a file to b33p and to every other client at once, b33p one to each; each
/// arrives intact.
pub(super) fn files(
    checks: &mut Checks,
    mix: &Mix<'_, '_>,
    b33p: &mut Harness,
    round: usize,
    tag: &str,
) {
    checks.record(
        &format!("{tag}: every client sends a file to b33p and to every other at once, each arrives intact"),
        || {
            let started = Instant::now();
            let labels: Vec<&str> = mix
                .peers
                .iter()
                .map(|peer| peer.label)
                .chain(["b33p"])
                .collect();
            for from in &labels {
                for to in labels.iter().filter(|to| to != &from) {
                    let name = file_name(round, from, to);
                    testbox_tools::write(&mix.files.join(&name), file_bytes(&name))?;
                }
            }
            let in_box = |name: &str| format!("/files/{name}");
            thread::scope(|scope| -> Result<(), Failure> {
                let others = scope.spawn(|| {
                    together(mix.peers, |peer| {
                        for other in mix.peers.iter().filter(|other| other.label != peer.label) {
                            peer.send_file(
                                &other.aci,
                                &file_text(round, peer.label, other.label),
                                &in_box(&file_name(round, peer.label, other.label)),
                            )?;
                        }
                        peer.send_file(
                            mix.primary_aci,
                            &file_text(round, peer.label, "b33p"),
                            &in_box(&file_name(round, peer.label, "b33p")),
                        )
                    })
                });
                let mine = (|| -> Result<(), Failure> {
                    for (peer, chat) in mix.peers.iter().zip(mix.chats) {
                        select(b33p, chat)?;
                        b33p.act(Action::StageFiles {
                            paths: vec![in_box(&file_name(round, "b33p", peer.label))],
                        })?;
                        type_and_send(b33p, &file_text(round, "b33p", peer.label))?;
                    }
                    Ok(())
                })();
                others
                    .join()
                    .unwrap_or_else(|_| Err("the clients' thread panicked".into()))?;
                mine
            })?;
            let sent = started.elapsed();
            // b33p: each file shown, saved, and the saved file the sender's bytes.
            for (peer, chat) in mix.peers.iter().zip(mix.chats) {
                select(b33p, chat)?;
                let text = file_text(round, peer.label, "b33p");
                let id = incoming_attachment(b33p, Some(&text), AttachmentKind::File)?;
                let path = save(b33p, &id)??;
                let hashed = mix.containers.exec(B33P, &["sha256sum", &path], "")?;
                let got = hashed.split_whitespace().next().unwrap_or_default();
                let bytes = file_bytes(&file_name(round, peer.label, "b33p"));
                if got != sha256(&bytes) {
                    return Err(format!(
                        "b33p saved {path} of {}'s file with sha256 {got}, not {} ({} bytes sent)",
                        peer.label,
                        sha256(&bytes),
                        bytes.len()
                    )
                    .into());
                }
            }
            // Each client: b33p's file and each other's, the hash the sender's where it tells it.
            together(mix.peers, |peer| {
                let senders = mix
                    .peers
                    .iter()
                    .map(|sender| sender.label)
                    .filter(|sender| *sender != peer.label)
                    .chain(["b33p"]);
                for sender in senders {
                    let text = file_text(round, sender, peer.label);
                    let key = file_key(&file_name(round, sender, peer.label));
                    peer.pull_until(&format!("{text:?}, with its file"), |peer| {
                        peer.texts_seen().contains(&text) && (!peer.tells_files() || peer.has_file(&key))
                    })?;
                }
                Ok(())
            })?;
            println!(
                "  {} clients sent files to every other and to b33p in {:.1} s; all intact after {:.1} s",
                mix.peers.len(),
                sent.as_secs_f64(),
                started.elapsed().as_secs_f64()
            );
            Ok(())
        },
    );
}

/// The emoji each client that can react reacts with; b33p's own.
fn emoji_of(label: &str) -> Option<&'static str> {
    match label {
        "signal-cli" => Some("👍"),
        "signalmeow" => Some("😂"),
        "flare" => Some("🎉"),
        _ => None,
    }
}
const B33P_EMOJI: &str = "🔥";

/// What b33p must show of one chat's targets once they are reacted to and edited.
pub(super) struct Amended {
    chat: String,
    label: &'static str,
    /// The client's message, and what it was edited to (presage-cli can't).
    their_old: String,
    their_new: Option<String>,
    /// b33p's message to the client, and what it was edited to.
    my_old: String,
    my_new: String,
}

/// Every message of the open conversation, older pages loaded until no more come.
fn all_messages(b33p: &mut Harness) -> Result<Vec<MessageSnapshot>, Failure> {
    let (mut quiet, mut seen) = (0, 0);
    loop {
        let messages = b33p.snapshot()?.messages;
        if messages.len() == seen {
            quiet += 1;
            if quiet == 3 {
                return Ok(messages);
            }
        } else {
            seen = messages.len();
            quiet = 0;
        }
        b33p.act(Action::LoadOlder)?;
        thread::sleep(Duration::from_millis(500));
    }
}

/// What b33p shows of `amended`, or why not.
fn judge(messages: &[MessageSnapshot], amended: &Amended) -> Result<(), String> {
    let named = |text: &str, outgoing: bool| -> Vec<&MessageSnapshot> {
        messages
            .iter()
            .filter(|message| {
                message.outgoing == outgoing && !message.notice && message.text == text
            })
            .collect()
    };
    let who = amended.label;
    // The client's message: once, as edited if it could be, with b33p's reaction on it, once.
    let theirs = amended.their_new.as_deref().unwrap_or(&amended.their_old);
    let found = named(theirs, false);
    if found.len() != 1 {
        return Err(format!(
            "{who}'s {theirs:?} is shown {} times, not once",
            found.len()
        ));
    }
    if amended.their_new.is_some() {
        if !found[0].edited {
            return Err(format!("{who}'s {theirs:?} is not marked as edited"));
        }
        let old = named(&amended.their_old, false).len();
        if old != 0 {
            return Err(format!(
                "{who}'s message is still shown as {:?} ({old} times) after its edit",
                amended.their_old
            ));
        }
    }
    let reactions: Vec<_> = found[0]
        .reactions
        .iter()
        .map(|reaction| (reaction.emoji.as_str(), reaction.outgoing))
        .collect();
    if reactions != [(B33P_EMOJI, true)] {
        return Err(format!(
            "{who}'s {theirs:?} carries the reactions {reactions:?}, not b33p's {B33P_EMOJI}"
        ));
    }
    // b33p's message to the client: once, as edited, and the client's reaction on it, if it can.
    let found = named(&amended.my_new, true);
    if found.len() != 1 {
        return Err(format!(
            "b33p's {:?} to {who} is shown {} times, not once",
            amended.my_new,
            found.len()
        ));
    }
    if !found[0].edited {
        return Err(format!(
            "b33p's {:?} to {who} is not marked as edited",
            amended.my_new
        ));
    }
    let old = named(&amended.my_old, true).len();
    if old != 0 {
        return Err(format!(
            "b33p's message to {who} is still shown as {:?} ({old} times) after its edit",
            amended.my_old
        ));
    }
    let reactions: Vec<_> = found[0]
        .reactions
        .iter()
        .map(|reaction| (reaction.emoji.as_str(), reaction.outgoing))
        .collect();
    let want: Vec<(&str, bool)> = emoji_of(who)
        .map(|emoji| (emoji, false))
        .into_iter()
        .collect();
    if reactions != want {
        return Err(format!(
            "b33p's {:?} to {who} carries the reactions {reactions:?}, not {want:?}",
            amended.my_new
        ));
    }
    Ok(())
}

/// b33p shows `amended`, loading the chat's history to look for it; waits for it to.
pub(super) fn b33p_shows_amended(b33p: &mut Harness, amended: &Amended) -> Result<(), Failure> {
    select(b33p, &amended.chat)?;
    let deadline = Instant::now() + PATIENCE;
    loop {
        match judge(&all_messages(b33p)?, amended) {
            Ok(()) => return Ok(()),
            Err(why) if Instant::now() > deadline => return Err(why.into()),
            Err(_) => thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// The index of the message in the open conversation sent at `time`, by b33p or not.
fn index_at(b33p: &mut Harness, outgoing: bool, time: u64) -> Result<u32, Failure> {
    Ok(b33p.wait_for(
        &format!("the message sent at {time}"),
        PATIENCE,
        |snapshot| {
            snapshot
                .messages
                .iter()
                .find(|message| {
                    message.outgoing == outgoing && !message.notice && message.timestamp == time
                })
                .map(|message| message.index)
        },
    )?)
}

/// The time of the message in the open conversation with `text`.
fn time_of(b33p: &mut Harness, outgoing: bool, text: &str) -> Result<u64, Failure> {
    Ok(
        b33p.wait_for(&format!("{text:?}'s time"), PATIENCE, |snapshot| {
            snapshot
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.outgoing == outgoing && !message.notice && message.text == text
                })
                .map(|message| message.timestamp)
        })?,
    )
}

/// Reactions and edits, from b33p to each client and from the clients that can to b33p, all at
/// once and amid a burst of texts from every client: each lands on the message it names.
/// What b33p must keep showing is added to `kept`.
pub(super) fn amendments(
    checks: &mut Checks,
    mix: &Mix<'_, '_>,
    b33p: &mut Harness,
    round: usize,
    tag: &str,
    count: usize,
    kept: &mut Vec<Amended>,
) {
    checks.record(
        &format!("{tag}: reactions and edits each way land on their messages, amid a burst"),
        || {
            let started = Instant::now();
            // Each side's message, which the other reacts to and which its writer edits.
            let their_old = |peer: &Peer<'_>| format!("{MARK}: r{round} target of {}", peer.label);
            let their_new = |peer: &Peer<'_>| format!("{MARK}: r{round} target of {}, edited", peer.label);
            let my_old = |peer: &Peer<'_>| format!("{MARK}: r{round} target of b33p to {}", peer.label);
            let my_new = |peer: &Peer<'_>| format!("{MARK}: r{round} target of b33p to {}, edited", peer.label);

            // The messages: each client's to b33p (its time kept, where it can name it), b33p's to each.
            let stamps: Mutex<HashMap<&str, u64>> = Mutex::new(HashMap::new());
            together(mix.peers, |peer| {
                let time = if peer.can_amend() {
                    peer.send_stamped(mix.primary_aci, &their_old(peer))?
                } else {
                    peer.send_many(mix.primary_aci, std::slice::from_ref(&their_old(peer)))?;
                    0
                };
                stamps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(peer.label, time);
                Ok(())
            })?;
            let (mut theirs, mut mine) = (HashMap::new(), HashMap::new());
            for (peer, chat) in mix.peers.iter().zip(mix.chats) {
                select(b33p, chat)?;
                b33p_shows(b33p, &their_old(peer), |message| message.text == their_old(peer))?;
                type_and_send(b33p, &my_old(peer))?;
                // The time b33p shows is the time the sender sent it at.
                let seen = time_of(b33p, false, &their_old(peer))?;
                let sent = stamps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)[peer.label];
                if sent != 0 && sent != seen {
                    return Err(format!("{} sent a message at {sent}, which b33p shows at {seen}", peer.label).into());
                }
                theirs.insert(peer.label, seen);
                mine.insert(peer.label, time_of(b33p, true, &my_old(peer))?);
            }
            together(mix.peers, |peer| {
                peer.pull_until(&format!("{:?}", my_old(peer)), |peer| {
                    peer.texts_seen().contains(&my_old(peer))
                })
            })?;

            // At once, with a burst from every client under them: the clients react to b33p's
            // message and edit their own; b33p reacts to each client's and edits its own.
            let amid = |peer: &Peer<'_>| burst(peer.label, "b33p-amid", round, count);
            thread::scope(|scope| -> Result<(), Failure> {
                let load = scope.spawn(|| {
                    together(mix.peers, |peer| peer.send_many(mix.primary_aci, &amid(peer)))
                });
                let amenders = scope.spawn(|| {
                    together(mix.peers, |peer| {
                        let Some(emoji) = emoji_of(peer.label) else {
                            return Ok(());
                        };
                        // b33p's account is the author: b33p is the primary's linked device.
                        peer.react(mix.primary_aci, mix.primary_aci, mine[peer.label], emoji)?;
                        if peer.can_edit() {
                            peer.edit(mix.primary_aci, theirs[peer.label], &their_new(peer))?;
                        }
                        Ok(())
                    })
                });
                let ours = (|| -> Result<(), Failure> {
                    for (peer, chat) in mix.peers.iter().zip(mix.chats) {
                        select(b33p, chat)?;
                        let index = index_at(b33p, false, theirs[peer.label])?;
                        b33p.act(Action::React {
                            index,
                            emoji: B33P_EMOJI.to_owned(),
                        })?;
                        let index = index_at(b33p, true, mine[peer.label])?;
                        edit(b33p, index, &my_new(peer))?;
                    }
                    Ok(())
                })();
                load.join()
                    .unwrap_or_else(|_| Err("the clients' thread panicked".into()))?;
                amenders
                    .join()
                    .unwrap_or_else(|_| Err("the clients' thread panicked".into()))?;
                ours
            })?;

            // b33p shows each target as it must, and the burst under them in order, once.
            for (peer, chat) in mix.peers.iter().zip(mix.chats) {
                let amended = Amended {
                    chat: chat.clone(),
                    label: peer.label,
                    their_old: their_old(peer),
                    their_new: peer.can_edit().then(|| their_new(peer)),
                    my_old: my_old(peer),
                    my_new: my_new(peer),
                };
                b33p_shows_amended(b33p, &amended)?;
                let texts = amid(peer);
                let last = texts.last().cloned().unwrap_or_default();
                b33p_shows(b33p, &format!("{last:?}"), |message| message.text == last)?;
                settled(b33p)?;
                let prefix = format!("{MARK}: r{round} {} to b33p-amid ", peer.label);
                in_order_once(&all_texts(b33p, &prefix)?, &texts)?;
                kept.push(amended);
            }
            // Each client: b33p's edit of its message, on the message it names, and b33p's
            // reaction to the client's, on the one it names.
            together(mix.peers, |peer| {
                let edited = my_new(peer);
                let reaction_to = [their_old(peer), their_new(peer)];
                peer.pull_until(
                    &format!("b33p's edit {edited:?} of {} and its reaction {B33P_EMOJI}", mine[peer.label]),
                    |peer| {
                        peer.saw_edit(&edited, mine[peer.label])
                            && peer.saw_reaction(
                                B33P_EMOJI,
                                theirs[peer.label],
                                &[reaction_to[0].as_str(), reaction_to[1].as_str()],
                            )
                    },
                )
            })?;
            println!(
                "  b33p reacted and edited in {} chats, {} clients under a burst of {count} each; all landed after {:.1} s",
                mix.peers.len(),
                mix.peers.len(),
                started.elapsed().as_secs_f64()
            );
            Ok(())
        },
    );
}
