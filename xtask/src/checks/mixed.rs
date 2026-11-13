//! Clients of every implementation at once (docs/onboarding-*.md): b33p, a presage-cli account,
//! signal-cli, signalmeow and Flare's backend write to one another in bursts that overlap, and each side is held to
//! what it must hold: every message once, from each sender in the order it was sent. Seen from the
//! clients themselves, not only from b33p: each client's own record of what it received is read
//! back and checked too, so a message the others lost or doubled shows on the side that kept it.
//!
//! `TESTBOX_MIXED_BURST` is how many messages each sender sends to each recipient in a burst
//! (twenty by default), `TESTBOX_MIXED_ROUNDS` how many times the bursts repeat (two), and
//! `TESTBOX_MIXED_ONLY` (`presage-cli`, `signal-cli`, `signalmeow`, `flare`, comma-separated) which
//! of them play at all, `TESTBOX_MIXED_FILES` and `TESTBOX_MIXED_AMEND` (1 by default; 0 for none)
//! whether each round also sends files, and reactions and edits (`mixed/amend.rs`).
//! `TESTBOX_MIXED_WARM` (1 by default; 0 for none) lets each pair of clients write to one another
//! once, in turn, before the bursts.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use b33p_harness::{Action, Delivery};

use super::b33p::{accept_request, b33p_shows, chat_previewing, select, type_and_send};
use super::family::FlareBox;
use super::presage::{Account, Presage};
use super::setup::{PlainBox, plain_box};
use super::signal_cli::{Account as CliAccount, SignalCli};
use super::signalmeow::Signalmeow;
use super::stress::{
    Sample, all_texts, growing, in_order_once, kill_and_start, numbered, sample, settled,
};
use super::{Checks, Failure, PATIENCE, env_number};
use crate::Sockets;
use crate::pod::{B33P, Pod};

mod amend;

use amend::Learned;

/// What every text of the phase starts with: the listings the clients keep are searched for it.
const MARK: &str = "testbox mixed";

/// One client of the box, as the phase writes to it and reads back from it.
enum Kind<'pod> {
    Presage(Presage<'pod>),
    SignalCli(SignalCli<'pod>),
    Signalmeow(Signalmeow<'pod>),
    Flare(FlareBox<'pod>),
}

struct Peer<'pod> {
    label: &'static str,
    aci: String,
    kind: Kind<'pod>,
    /// What the client printed of the texts it received, as it received them (presage-cli's and
    /// Flare's `sync` print each message before the store keeps it). Their stores key a message by
    /// thread and time alone, so two messages of one conversation sent in the same millisecond, one
    /// by each side, take one place and the store holds one: the message is then received, and
    /// not held.
    printed: std::sync::Mutex<Vec<String>>,
    /// What it reported of reactions, edits and files (`amend.rs`).
    learned: std::sync::Mutex<Learned>,
}

impl Peer<'_> {
    /// Sends `texts` to `to`, one after another, as fast as the client does.
    fn send_many(&self, to: &str, texts: &[String]) -> Result<(), Failure> {
        match &self.kind {
            Kind::Presage(client) => Ok(client.send_all(to, texts)?),
            Kind::SignalCli(client) => Ok(client.send_all(to, texts)?),
            Kind::Signalmeow(client) => {
                for text in texts {
                    client.send(to, text)?;
                }
                Ok(())
            }
            Kind::Flare(client) => Ok(client.send_many(to, texts)?),
        }
    }

    /// Receives what the server has queued for it.
    fn pull(&self) -> Result<(), Failure> {
        match &self.kind {
            Kind::Presage(client) => {
                client.sync()?;
                // The sync's, and what the sends since received (see `Presage::heard`).
                self.note_printed(&client.take_heard());
            }
            Kind::SignalCli(client) => {
                let received = client.sync()?;
                // An edit's text is printed as a text, as the other clients print it.
                self.note_printed(
                    &received
                        .iter()
                        .filter_map(|message| {
                            message
                                .text
                                .as_deref()
                                .or_else(|| message.edit.as_ref().map(|(_, text)| text.as_str()))
                        })
                        .map(|text| format!("{text}\n"))
                        .collect::<String>(),
                );
                self.learn_received(&received)?;
            }
            Kind::Signalmeow(client) => self.note_printed(&client.sync()?),
            Kind::Flare(client) => {
                client.sync()?;
                self.note_printed(&client.take_heard());
            }
        }
        Ok(())
    }

    /// The texts the client reported receiving, each once, in the order it first did: a
    /// redelivery (a message acknowledged late, after the process ended) is no second message.
    fn texts_seen(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for text in self
            .printed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            if !seen.contains(text) {
                seen.push(text.clone());
            }
        }
        seen
    }

    /// What this client calls the group `title` (b33p's, with `master_key`), once it has received
    /// `hello` in it: presage-cli and signalmeow take the master key, signal-cli its base64 id
    /// (listed by title), Flare its hex id.
    fn group_ref(&self, title: &str, master_key: &str, hello: &str) -> Result<String, Failure> {
        let deadline = Instant::now() + PATIENCE;
        while !self.texts_seen().iter().any(|text| text == hello) {
            if Instant::now() > deadline {
                return Err(format!("{} never received {hello:?}", self.label).into());
            }
            self.pull()?;
            thread::sleep(Duration::from_secs(1));
        }
        match &self.kind {
            Kind::Presage(_) | Kind::Signalmeow(_) => Ok(master_key.to_owned()),
            Kind::SignalCli(client) => client
                .groups()?
                .into_iter()
                .find(|(_, name)| name == title)
                .map(|(id, _)| id)
                .ok_or_else(|| format!("signal-cli lists no group {title:?}").into()),
            Kind::Flare(client) => Ok(client.group_titled(title, PATIENCE)?),
        }
    }

    fn send_group(&self, group: &str, text: &str) -> Result<(), Failure> {
        match &self.kind {
            Kind::Presage(client) => Ok(client.send_to_group(group, text)?),
            Kind::SignalCli(client) => client
                .send_to_group(group, text)
                .map(drop)
                .map_err(Into::into),
            Kind::Signalmeow(client) => Ok(client.send_to_group(group, text)?),
            Kind::Flare(client) => Ok(client.send_group(group, text)?),
        }
    }

    fn note_printed(&self, output: &str) {
        self.learn(output);
        self.printed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(texts_in(output));
    }

    /// What `held` is held against: `expected` without the texts that it lacks only because the
    /// client's store keeps one message of two sent at the same millisecond (see
    /// [`Self::printed`]): a text the client printed (received), and the store doesn't hold. A
    /// text it never printed stays: that one was lost. Then the texts left out.
    fn owed(&self, held: &[String], expected: &[String]) -> (Vec<String>, Vec<String>) {
        let printed = self
            .printed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expected.iter().cloned().partition(|text| {
            held.contains(text)
                || !matches!(self.kind, Kind::Presage(_) | Kind::Flare(_))
                || !printed.contains(text)
        })
    }

    /// Tells of the texts [`Self::owed`] left out.
    fn say_collided(&self, collided: &[String]) {
        for text in collided {
            println!(
                "  {}'s store lacks {text:?}, which it received: one place for two messages of one millisecond",
                self.label
            );
        }
    }

    /// The texts of the phase the client holds from `from`, in the order it holds them.
    fn held_from(&self, from: &str) -> Result<Vec<String>, Failure> {
        let (listing, newest_first) = match &self.kind {
            // presage-cli's `list-messages` lists the newest first.
            Kind::Presage(client) => (client.messages_with(from)?, true),
            Kind::SignalCli(client) => (client.messages_with(from), false),
            Kind::Signalmeow(client) => (client.messages_with(from)?, false),
            Kind::Flare(client) => (client.messages(from)?, false),
        };
        let mut texts = texts_in(&listing);
        if newest_first {
            texts.reverse();
        }
        Ok(texts)
    }
}

/// From each line of `listing` the text that starts at [`MARK`], to the end of the line: the
/// clients list a message with its time and sender in front, each in its own way.
fn texts_in(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.find(MARK).map(|at| line[at..].trim_end().to_owned()))
        .collect()
}

fn setting(name: &str, default: usize) -> Result<usize, Failure> {
    Ok(env_number::<usize>(name)?.unwrap_or(default))
}

/// Runs `receive` on every peer at once (each a container's `podman exec`), and fails with the
/// first that failed.
fn together(
    peers: &[Peer<'_>],
    work: impl Fn(&Peer<'_>) -> Result<(), Failure> + Sync,
) -> Result<(), Failure> {
    let work = &work;
    thread::scope(|scope| {
        let running: Vec<_> = peers
            .iter()
            .map(|peer| (peer.label, scope.spawn(move || work(peer))))
            .collect();
        let mut first = None;
        for (label, handle) in running {
            let outcome = handle
                .join()
                .unwrap_or_else(|_| Err(format!("the thread of {label} panicked").into()));
            if let Err(error) = outcome {
                first.get_or_insert_with(|| format!("{label}: {}", chain(error.as_ref())));
            }
        }
        first.map_or(Ok(()), |error| Err(error.into()))
    })
}

/// An error and what caused it, one after the other: a step's own text says which step, its
/// source says what the container printed.
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// The texts `sender` sends in `round`, to `to`: told apart by sender, recipient and round, in
/// order when compared as text.
fn burst(sender: &str, to: &str, round: usize, count: usize) -> Vec<String> {
    numbered(&format!("{MARK}: r{round} {sender} to {to}"), count)
}

pub(super) fn mixed(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<(), Failure> {
    let count = setting("TESTBOX_MIXED_BURST", 20)?;
    let rounds = setting("TESTBOX_MIXED_ROUNDS", 2)?;
    let PlainBox {
        primary: _primary,
        peer,
        primary_aci,
        mut b33p,
    } = plain_box(checks, containers, sockets)?;
    let peer_aci = peer.aci()?;

    let cli = SignalCli::new(containers, CliAccount::Cli);
    let cli_aci = checks.require("signal-cli registers", || Ok(cli.register()?))?;
    let meow_account = Presage::new(containers, Account::Meow);
    let mut meow = Signalmeow::new(containers);
    checks.require("a presage-cli account registers for signalmeow", || {
        Ok(meow_account.register()?)
    })?;
    let meow_aci = checks.require("signalmeow links to it", || meow.link(&meow_account))?;
    // Connected all the time, as a client of its own is: a process per command ends as a message
    // arrives, now and then, and what it was handling is spent (see signalmeow.rs `Daemon`).
    checks.require("signalmeow stays connected", || meow.serve())?;
    let flare = FlareBox::new(containers);
    let flare_aci = checks.require("Flare's backend registers", || Ok(flare.register()?))?;
    let mut peers = vec![
        Peer {
            label: "presage-cli",
            aci: peer_aci,
            kind: Kind::Presage(peer),
            printed: Default::default(),
            learned: Default::default(),
        },
        Peer {
            label: "signal-cli",
            aci: cli_aci,
            kind: Kind::SignalCli(cli),
            printed: Default::default(),
            learned: Default::default(),
        },
        Peer {
            label: "signalmeow",
            aci: meow_aci,
            kind: Kind::Signalmeow(meow),
            printed: Default::default(),
            learned: Default::default(),
        },
        Peer {
            label: "flare",
            aci: flare_aci,
            kind: Kind::Flare(flare),
            printed: Default::default(),
            learned: Default::default(),
        },
    ];
    // `TESTBOX_MIXED_ONLY=flare,signal-cli`: only those play, to find which pairing a failure needs.
    // The others are registered all the same, so that the box is as it is for all of them.
    if let Some(only) = std::env::var("TESTBOX_MIXED_ONLY")
        .ok()
        .filter(|only| !only.is_empty())
    {
        let wanted: Vec<&str> = only.split(',').map(str::trim).collect();
        if let Some(unknown) = wanted
            .iter()
            .find(|name| !peers.iter().any(|peer| peer.label == **name))
        {
            return Err(format!("TESTBOX_MIXED_ONLY names {unknown:?}, which is no peer").into());
        }
        peers.retain(|peer| wanted.contains(&peer.label));
    }

    // Each writes to b33p's account first: b33p takes it as a message request and accepts it.
    let mut chats = Vec::new();
    for peer in &peers {
        let first = format!("{MARK}: the first message of {}", peer.label);
        let chat = checks.require(&format!("text from {} reaches b33p", peer.label), || {
            peer.send_many(&primary_aci, std::slice::from_ref(&first))?;
            Ok(chat_previewing(&mut b33p, &first)?)
        })?;
        checks.require(
            &format!("{}'s first message is a request, accepted", peer.label),
            || Ok(accept_request(&mut b33p, &chat)?),
        )?;
        chats.push(chat);
    }

    // Sessions between the clients before the bursts, each pair in turn: the clients' first
    // contact is a moment of its own, and two sides making it at once is where signalmeow and
    // signal-cli part ways (docs/testbox-clients-run-2026-11-13.md). `TESTBOX_MIXED_WARM=0` leaves
    // it to the bursts.
    if setting("TESTBOX_MIXED_WARM", 1)? != 0 {
        checks.require(
            "each pair of clients has written to one another once",
            || {
                for (at, first) in peers.iter().enumerate() {
                    for second in &peers[at + 1..] {
                        for (from, to) in [(first, second), (second, first)] {
                            let text = format!("{MARK}: warm-up, {} to {}", from.label, to.label);
                            from.send_many(&to.aci, std::slice::from_ref(&text))?;
                            let deadline = Instant::now() + PATIENCE;
                            while !to.texts_seen().contains(&text) {
                                if Instant::now() > deadline {
                                    return Err(
                                        format!("{} never received {text:?}", to.label).into()
                                    );
                                }
                                to.pull()?;
                                thread::sleep(Duration::from_millis(500));
                            }
                        }
                    }
                }
                Ok(())
            },
        )?;
    }

    // A group of b33p's, of all of them: what each client calls it differs (a master key, a base64
    // id, a hex id), and each learns it from what b33p writes in it.
    let title = "everyone in the mix"; // not starting with MARK: the listings are searched for it
    let group = checks.require("b33p makes a group of all the clients", || {
        b33p.act(Action::CreateGroup {
            title: title.to_owned(),
            members: chats.clone(),
        })?;
        Ok(b33p.wait_for("the group in b33p", PATIENCE, |snapshot| {
            snapshot
                .conversations
                .iter()
                .find(|chat| chat.group && chat.title == title && !chat.request)
                .map(|chat| chat.id.clone())
        })?)
    })?;
    let master_key = b33p_core::ConversationId(group.clone())
        .group_key()
        .ok_or_else(|| format!("{group} isn't a group's id"))?
        .to_owned();
    let group_refs: Vec<String> = checks.require("every client learns of the group", || {
        let hello = format!("{MARK}: hello, group");
        select(&mut b33p, &group)?;
        type_and_send(&mut b33p, &hello)?;
        let learned: Vec<Result<String, Failure>> = thread::scope(|scope| {
            let running: Vec<_> = peers
                .iter()
                .map(|peer| {
                    let (title, master_key, hello) = (title, &master_key, &hello);
                    scope.spawn(move || peer.group_ref(title, master_key, hello))
                })
                .collect();
            running
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("a thread panicked".into()))
                })
                .collect()
        });
        learned.into_iter().collect()
    })?;
    let group_of = |peer: &Peer<'_>| -> &str {
        peers
            .iter()
            .position(|other| other.label == peer.label)
            .map_or("", |at| group_refs[at].as_str())
    };

    let mix = amend::Mix {
        peers: &peers,
        chats: &chats,
        primary_aci: &primary_aci,
        containers,
        files,
    };
    let mut kept = Vec::new();
    let with_files = setting("TESTBOX_MIXED_FILES", 1)? != 0;
    let with_amendments = setting("TESTBOX_MIXED_AMEND", 1)? != 0;

    // b33p's memory, open files and answering time after each round, for the check at the end.
    let mut samples: Vec<Sample> = Vec::new();
    for round in 1..=rounds {
        let tag = format!("round {round} of {rounds}");

        checks.record(
            &format!("{tag}: every client writes to b33p at once, each in order, once"),
            || {
                let started = Instant::now();
                together(&peers, |peer| {
                    peer.send_many(&primary_aci, &burst(peer.label, "b33p", round, count))
                })?;
                let sent = started.elapsed();
                for (peer, chat) in peers.iter().zip(&chats) {
                    let texts = burst(peer.label, "b33p", round, count);
                    select(&mut b33p, chat)?;
                    let last = texts.last().cloned().unwrap_or_default();
                    super::b33p::b33p_shows(&mut b33p, &format!("{last:?}"), |message| {
                        message.text == last
                    })?;
                    settled(&mut b33p)?;
                    let prefix = format!("{MARK}: r{round} {} to b33p ", peer.label);
                    in_order_once(&all_texts(&mut b33p, &prefix)?, &texts)?;
                }
                println!(
                    "  {} clients sent {count} each in {:.1} s; all shown after {:.1} s",
                    peers.len(),
                    sent.as_secs_f64(),
                    started.elapsed().as_secs_f64()
                );
                Ok(())
            },
        );

        checks.record(
            &format!("{tag}: b33p writes to every client while they write to it, each gets it in order, once"),
            || {
                let started = Instant::now();
                let ours = thread::scope(|scope| -> Result<(), Failure> {
                    let others = scope.spawn(|| {
                        together(&peers, |peer| {
                            peer.send_many(
                                &primary_aci,
                                &burst(peer.label, "b33p-too", round, count),
                            )
                        })
                    });
                    let mine = (|| -> Result<(), Failure> {
                        for (peer, chat) in peers.iter().zip(&chats) {
                            select(&mut b33p, chat)?;
                            for text in burst("b33p", peer.label, round, count) {
                                b33p.act(Action::Type { text })?;
                                b33p.act(Action::Send)?;
                            }
                        }
                        Ok(())
                    })();
                    others
                        .join()
                        .unwrap_or_else(|_| Err("the clients' thread panicked".into()))?;
                    mine
                });
                ours?;
                b33p.wait_for("b33p's own sent", PATIENCE, |snapshot| {
                    snapshot
                        .messages
                        .iter()
                        .filter(|message| message.text.starts_with(MARK))
                        .all(|message| message.delivery != Some(Delivery::Sending))
                        .then_some(())
                })
                .map_err(|error| -> Failure { Box::new(error) })?;
                for (peer, chat) in peers.iter().zip(&chats) {
                    let texts = burst(peer.label, "b33p-too", round, count);
                    select(&mut b33p, chat)?;
                    let last = texts.last().cloned().unwrap_or_default();
                    super::b33p::b33p_shows(&mut b33p, &format!("{last:?}"), |message| {
                        message.text == last
                    })?;
                    settled(&mut b33p)?;
                    let prefix = format!("{MARK}: r{round} {} to b33p-too ", peer.label);
                    in_order_once(&all_texts(&mut b33p, &prefix)?, &texts)?;
                }
                // Each client receives what is queued, and keeps it: what it holds from b33p's
                // account (the primary's: b33p is its linked device) is what b33p sent, in order,
                // once. A retry after a receive is the clients' own, so pull until it holds all.
                let deadline = Instant::now() + PATIENCE;
                together(&peers, |peer| {
                    let expected = burst("b33p", peer.label, round, count);
                    loop {
                        peer.pull()?;
                        let held: Vec<String> = peer
                            .held_from(&primary_aci)?
                            .into_iter()
                            .filter(|text| text.starts_with(&format!("{MARK}: r{round} b33p to ")))
                            .collect();
                        let (owed, collided) = peer.owed(&held, &expected);
                        if held.len() >= owed.len() || Instant::now() > deadline {
                            peer.say_collided(&collided);
                            let held: Vec<String> =
                                held.into_iter().filter(|text| owed.contains(text)).collect();
                            return in_order_once(&held, &owed).map_err(Into::into);
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                })?;
                println!(
                    "  b33p sent {count} to each of {} clients; all held after {:.1} s",
                    peers.len(),
                    started.elapsed().as_secs_f64()
                );
                Ok(())
            },
        );

        checks.record(
            &format!("{tag}: every client writes to every other at once, each in order, once"),
            || {
                let started = Instant::now();
                together(&peers, |peer| {
                    for other in peers.iter().filter(|other| other.label != peer.label) {
                        peer.send_many(&other.aci, &burst(peer.label, other.label, round, count))?;
                    }
                    Ok(())
                })?;
                let deadline = Instant::now() + PATIENCE;
                together(&peers, |peer| {
                    for sender in peers.iter().filter(|other| other.label != peer.label) {
                        let expected = burst(sender.label, peer.label, round, count);
                        loop {
                            peer.pull()?;
                            let held: Vec<String> = peer
                                .held_from(&sender.aci)?
                                .into_iter()
                                .filter(|text| {
                                    text.starts_with(&format!(
                                        "{MARK}: r{round} {} to {} ",
                                        sender.label, peer.label
                                    ))
                                })
                                .collect();
                            let (owed, collided) = peer.owed(&held, &expected);
                            if held.len() >= owed.len() || Instant::now() > deadline {
                                peer.say_collided(&collided);
                                in_order_once(&held, &owed).map_err(|error| -> Failure {
                                    format!("{} from {}: {error}", peer.label, sender.label).into()
                                })?;
                                break;
                            }
                            thread::sleep(Duration::from_secs(1));
                        }
                    }
                    Ok(())
                })?;
                println!(
                    "  {} clients each wrote {count} to each other client; all held after {:.1} s",
                    peers.len(),
                    started.elapsed().as_secs_f64()
                );
                Ok(())
            },
        );

        checks.record(
            &format!("{tag}: every client writes to the group at once, each holds every burst once, in order"),
            || {
                let started = Instant::now();
                together(&peers, |peer| {
                    for text in burst(peer.label, "group", round, count) {
                        peer.send_group(group_of(peer), &text)?;
                    }
                    Ok(())
                })?;
                select(&mut b33p, &group)?;
                for peer in &peers {
                    let texts = burst(peer.label, "group", round, count);
                    let last = texts.last().cloned().unwrap_or_default();
                    b33p_shows(&mut b33p, &format!("{last:?}"), |message| message.text == last)?;
                }
                settled(&mut b33p)?;
                for peer in &peers {
                    let prefix = format!("{MARK}: r{round} {} to group ", peer.label);
                    in_order_once(
                        &all_texts(&mut b33p, &prefix)?,
                        &burst(peer.label, "group", round, count),
                    )?;
                }
                let deadline = Instant::now() + PATIENCE;
                together(&peers, |peer| {
                    loop {
                        peer.pull()?;
                        let seen = peer.texts_seen();
                        let mut missing = None;
                        for sender in peers.iter().filter(|other| other.label != peer.label) {
                            let expected = burst(sender.label, "group", round, count);
                            let got: Vec<String> = seen
                                .iter()
                                .filter(|text| {
                                    text.starts_with(&format!(
                                        "{MARK}: r{round} {} to group ",
                                        sender.label
                                    ))
                                })
                                .cloned()
                                .collect();
                            if got != expected {
                                missing = Some((sender.label, got, expected));
                                break;
                            }
                        }
                        let Some((from, got, expected)) = missing else {
                            return Ok(());
                        };
                        if Instant::now() > deadline {
                            return in_order_once(&got, &expected).map_err(|error| -> Failure {
                                format!("{} from {from} in the group: {error}", peer.label).into()
                            });
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                })?;
                println!(
                    "  {} clients wrote {count} each to the group; all held after {:.1} s",
                    peers.len(),
                    started.elapsed().as_secs_f64()
                );
                Ok(())
            },
        );

        checks.record(
            &format!("{tag}: b33p writes to the group, each client holds it once, in order"),
            || {
                select(&mut b33p, &group)?;
                let texts = burst("b33p", "group", round, count);
                for text in &texts {
                    type_and_send(&mut b33p, text)?;
                }
                let deadline = Instant::now() + PATIENCE;
                together(&peers, |peer| {
                    loop {
                        peer.pull()?;
                        let got: Vec<String> = peer
                            .texts_seen()
                            .into_iter()
                            .filter(|text| {
                                text.starts_with(&format!("{MARK}: r{round} b33p to group "))
                            })
                            .collect();
                        if got.len() >= texts.len() || Instant::now() > deadline {
                            return in_order_once(&got, &texts).map_err(|error| -> Failure {
                                format!("{} in the group: {error}", peer.label).into()
                            });
                        }
                        thread::sleep(Duration::from_secs(1));
                    }
                })?;
                Ok(())
            },
        );

        if with_files {
            amend::files(checks, &mix, &mut b33p, round, &tag);
        }
        if with_amendments {
            amend::amendments(checks, &mix, &mut b33p, round, &tag, count, &mut kept);
        }

        match sample(containers, B33P, &mut b33p) {
            Ok(taken) => {
                println!("  b33p after round {round}: {taken}");
                samples.push(taken);
            }
            Err(error) => println!("  b33p after round {round}: not sampled: {error}"),
        }
    }

    if samples.len() >= 6 {
        checks.record(
            "b33p's memory, files and answering time don't keep growing",
            || Ok(growing(&samples)?),
        );
    }

    checks.record(
        "b33p killed, started again: it still shows every burst, each in order, once",
        || {
            b33p = kill_and_start(containers, sockets)?;
            for (peer, chat) in peers.iter().zip(&chats) {
                select(&mut b33p, chat)?;
                settled(&mut b33p)?;
                for round in 1..=rounds {
                    let prefix = format!("{MARK}: r{round} {} to b33p ", peer.label);
                    in_order_once(
                        &all_texts(&mut b33p, &prefix)?,
                        &burst(peer.label, "b33p", round, count),
                    )?;
                    let prefix = format!("{MARK}: r{round} {} to b33p-too ", peer.label);
                    in_order_once(
                        &all_texts(&mut b33p, &prefix)?,
                        &burst(peer.label, "b33p-too", round, count),
                    )?;
                }
            }
            Ok(())
        },
    );
    if !kept.is_empty() {
        checks.record(
            "b33p killed, started again: it still shows every edit and reaction, on their messages",
            || {
                for amended in &kept {
                    amend::b33p_shows_amended(&mut b33p, amended)?;
                }
                Ok(())
            },
        );
    }
    for peer in &peers {
        if let Kind::Signalmeow(meow) = &peer.kind {
            meow.flush_lost();
        }
    }
    Ok(())
}
