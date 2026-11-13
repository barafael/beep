//! Clients of every implementation at once (docs/onboarding-*.md): b33p, a presage-cli account,
//! signal-cli, signalmeow and Flare's backend write to one another in bursts that overlap, and each side is held to
//! what it must hold: every message once, from each sender in the order it was sent. Seen from the
//! clients themselves, not only from b33p: each client's own record of what it received is read
//! back and checked too, so a message the others lost or doubled shows on the side that kept it.
//!
//! `TESTBOX_MIXED_BURST` is how many messages each sender sends to each recipient in a burst
//! (twenty by default), `TESTBOX_MIXED_ROUNDS` how many times the bursts repeat (two), and
//! `TESTBOX_MIXED_ONLY` (`presage-cli`, `signal-cli`, `signalmeow`, `flare`, comma-separated) which
//! of them play at all.

use std::thread;
use std::time::{Duration, Instant};

use b33p_harness::{Action, Delivery};

use super::b33p::{accept_request, chat_previewing, select};
use super::family::FlareBox;
use super::presage::{Account, Presage};
use super::setup::{PlainBox, plain_box};
use super::signal_cli::{Account as CliAccount, SignalCli};
use super::signalmeow::Signalmeow;
use super::stress::{all_texts, in_order_once, kill_and_start, numbered, settled};
use super::{Checks, Failure, PATIENCE, env_number};
use crate::Sockets;
use crate::pod::Pod;

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
            Kind::Presage(client) => self.note_printed(&client.sync()?),
            Kind::SignalCli(client) => drop(client.sync()?),
            Kind::Signalmeow(client) => drop(client.sync()?),
            Kind::Flare(client) => self.note_printed(&client.sync()?),
        }
        Ok(())
    }

    fn note_printed(&self, output: &str) {
        self.printed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(texts_in(output));
    }

    /// What `held` is held against: `expected` without the texts that it lacks only because the
    /// client's store keeps one message of two sent at the same millisecond (see
    /// [`Self::printed`]): a text the client printed exactly once, and the store doesn't hold. A
    /// text it never printed stays: that one was lost. Then the texts left out.
    fn owed(&self, held: &[String], expected: &[String]) -> (Vec<String>, Vec<String>) {
        let printed = self
            .printed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        expected.iter().cloned().partition(|text| {
            held.contains(text)
                || !matches!(self.kind, Kind::Presage(_) | Kind::Flare(_))
                || printed.iter().filter(|seen| *seen == text).count() != 1
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
    let meow = Signalmeow::new(containers);
    checks.require("a presage-cli account registers for signalmeow", || {
        Ok(meow_account.register()?)
    })?;
    let meow_aci = checks.require("signalmeow links to it", || meow.link(&meow_account))?;
    let flare = FlareBox::new(containers);
    let flare_aci = checks.require("Flare's backend registers", || Ok(flare.register()?))?;
    let mut peers = vec![
        Peer {
            label: "presage-cli",
            aci: peer_aci,
            kind: Kind::Presage(peer),
            printed: Default::default(),
        },
        Peer {
            label: "signal-cli",
            aci: cli_aci,
            kind: Kind::SignalCli(cli),
            printed: Default::default(),
        },
        Peer {
            label: "signalmeow",
            aci: meow_aci,
            kind: Kind::Signalmeow(meow),
            printed: Default::default(),
        },
        Peer {
            label: "flare",
            aci: flare_aci,
            kind: Kind::Flare(flare),
            printed: Default::default(),
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
    Ok(())
}
