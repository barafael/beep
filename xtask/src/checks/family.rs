//! The presage family (testbox/family): clients that run on the presage and libsignal-service
//! they ship with, not on b33p's fork's. Each phase is on a box of its own: b33p linked to a
//! presage-cli primary, a presage-cli peer, and the client, which writes to both and to b33p.
//!
//! What a pass shows: that the box's server and b33p agree with clients whose protocol code is
//! an older revision's than b33p's fork, which presage-cli at b33p's pin can't show, as it is
//! b33p's own code.

use std::time::{Duration, Instant};

use b33p_harness::Action;

use super::b33p::{accept_request, chat_previewing, select, type_and_send};
use super::presage::{Account, Presage};
use super::setup::{PlainBox, plain_box};
use super::{Checks, Failure, PATIENCE};
use crate::Sockets;
use crate::pod::{Pod, PodError, Service};
use testbox_tools::keys::KeyFile;

/// What the family's checks find wrong of their own, besides the steps' errors.
#[derive(Debug, thiserror::Error)]
pub(super) enum FamilyError {
    #[error("flare-box printed no ACI: {printed}")]
    NoAci { printed: String },
    /// A sync or screen that should have shown `expected`.
    #[error("{client} never showed {expected:?} (last seen):\n{seen}")]
    NotShown {
        client: &'static str,
        expected: String,
        seen: String,
    },
    /// A text of a burst that isn't in what the client shows exactly once, after the one before.
    #[error("{client} shows {text:?} {found} times, or out of order; it shows:\n{shown}")]
    NotOnceInOrder {
        client: &'static str,
        text: String,
        found: usize,
        shown: String,
    },
    #[error("no group named {title:?} among {groups}")]
    NoGroup { title: String, groups: String },
    /// What a wait for gurk's screen wanted never showed on it.
    #[error("not on gurk's screen within the patience:\n{screen}")]
    NoScreen { screen: String },
    #[error("gurk has stopped: its tmux session is gone")]
    Gone,
    #[error("{what}")]
    Stuck { what: String },
    #[error("gurk logged:\n{lines}")]
    Logged { lines: String },
    /// A flare-box command that exited with a failure: what it printed.
    #[error("{what} failed ({status}):\n{output}")]
    Exited {
        what: String,
        status: std::process::ExitStatus,
        output: String,
    },
    #[error("{what}")]
    Step {
        what: String,
        #[source]
        source: PodError,
    },
}

/// How many messages the bursts send, one after another as fast as presage-cli sends: what a
/// terminal's pane shows at once (gurk's, 50 rows).
const BURST: usize = 20;

/// `prefix 001` to `prefix <count>`: in order as text, and told apart as one text from another.
fn numbered(prefix: &str, count: usize) -> Vec<String> {
    (1..=count).map(|n| format!("{prefix} {n:03}")).collect()
}

/// A store's listing of a thread (presage-cli's `list-messages`, flare-box's `messages`) is
/// newest first: the lines the other way round.
fn oldest_first(listed: &str) -> String {
    let mut lines: Vec<&str> = listed.lines().collect();
    lines.reverse();
    lines.join("\n")
}

/// Fails unless each of `texts` is in `shown` exactly once, each after the one before.
fn once_in_order(client: &'static str, shown: &str, texts: &[String]) -> Result<(), FamilyError> {
    let mut after = 0;
    for text in texts {
        let found: Vec<usize> = shown
            .match_indices(text.as_str())
            .map(|(at, _)| at)
            .collect();
        match found[..] {
            [at] if at >= after => after = at,
            _ => {
                return Err(FamilyError::NotOnceInOrder {
                    client,
                    text: text.clone(),
                    found: found.len(),
                    shown: shown.to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Flare's backend, one command at a time: `flare-box` on the account's store in the `family`
/// container. Registers as an account of its own, as Flare does when it isn't linked.
pub(super) struct FlareBox<'pod> {
    containers: &'pod Pod,
    /// What the commands that receive (`send`, `sync`) printed since [`Self::take_heard`].
    heard: std::sync::Mutex<String>,
}

/// What presage logs (at ERROR) of an envelope it could not open.
const OPENING_FAILED: &str = "error opening envelope";
const FLARE_DB: &str = "/presage/family-flare.db";
const FLARE_PHONE: &str = "+15555550450";

impl<'pod> FlareBox<'pod> {
    pub(super) fn new(containers: &'pod Pod) -> Self {
        Self {
            containers,
            heard: std::sync::Mutex::default(),
        }
    }

    /// What the commands that received printed since this was last called.
    pub(super) fn take_heard(&self) -> String {
        std::mem::take(
            &mut *self
                .heard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn cli(&self, arguments: &[&str]) -> Result<String, FamilyError> {
        let argv = [&["flare-box", FLARE_DB][..], arguments].concat();
        let what = format!("flare-box {}", arguments.first().copied().unwrap_or(""));
        let output = self
            .containers
            .exec_status(Service::Family, &argv)
            .map_err(|source| FamilyError::Step {
                what: what.clone(),
                source,
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(FamilyError::Exited {
                what,
                status: output.status,
                output: format!("{stdout}{stderr}"),
            });
        }
        if what.starts_with("flare-box send")
            || what == "flare-box sync"
            || what == "flare-box react"
        {
            self.heard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_str(&stdout);
        }
        // presage logs a message it can't open and goes on, so the command succeeds: what it
        // couldn't open is a message lost, which the check every phase ends with reports.
        for line in stderr.lines().filter(|line| line.contains(OPENING_FAILED)) {
            self.containers
                .note_lost(format!("flare-box ({what}): {}", line.trim()));
        }
        Ok(stdout)
    }

    /// Registers afresh against the box's servers; the account's ACI.
    pub(super) fn register(&self) -> Result<String, FamilyError> {
        let servers = KeyFile::Servers.path();
        let printed = self.cli(&[
            "register",
            &servers.to_string_lossy(),
            FLARE_PHONE,
            testbox_tools::CAPTCHA,
            testbox_tools::VERIFICATION_CODE,
            "Flare",
        ])?;
        let aci = aci_printed(&printed)?;
        // A name, as presage-cli's accounts get one: an account without a profile has no profile
        // key credential, and is invited to a group, not added to it.
        self.cli(&["set-profile", "Flare"])?;
        Ok(aci)
    }

    /// Receives what is queued and a second more; what it printed.
    pub(super) fn sync(&self) -> Result<String, FamilyError> {
        self.cli(&["sync"])
    }

    fn sync_printing(&self, expected: &str) -> Result<(), FamilyError> {
        let printed = self.sync()?;
        if printed.contains(expected) {
            Ok(())
        } else {
            Err(FamilyError::NotShown {
                client: "flare-box",
                expected: expected.to_owned(),
                seen: printed,
            })
        }
    }

    pub(super) fn send(&self, aci: &str, text: &str) -> Result<(), FamilyError> {
        self.cli(&["send", aci, text]).map(drop)
    }

    /// Sends `text` to `aci`; the time it was sent at (the "To <thread> @ <ms>: <text>" line the
    /// command prints for it).
    pub(super) fn send_stamped(&self, aci: &str, text: &str) -> Result<u64, FamilyError> {
        let printed = self.cli(&["send", aci, text])?;
        printed
            .lines()
            .filter(|line| line.starts_with("To "))
            .find_map(|line| {
                let (head, said) = line.split_once(": ")?;
                (said == text).then_some(head)?;
                head.rsplit_once(" @ ")?.1.parse().ok()
            })
            .ok_or(FamilyError::NotShown {
                client: "flare-box",
                expected: format!("To … @ <ms>: {text}"),
                seen: printed,
            })
    }

    /// Sends `text` with the file at `path` (in the container: `/files/…`).
    pub(super) fn send_file(&self, aci: &str, path: &str, text: &str) -> Result<(), FamilyError> {
        self.cli(&["send-file", aci, path, text]).map(drop)
    }

    /// Reacts with `emoji` to the message `author` sent at `at` (milliseconds), in the chat with `aci`.
    pub(super) fn react(
        &self,
        aci: &str,
        author: &str,
        at: u64,
        emoji: &str,
    ) -> Result<(), FamilyError> {
        self.cli(&["react", aci, author, &at.to_string(), emoji])
            .map(drop)
    }

    /// `texts` one after another in one process, as presage-cli's `send_all` does: one process
    /// has the connection open all through, where one a text has only its own few milliseconds.
    pub(super) fn send_many(&self, aci: &str, texts: &[String]) -> Result<(), FamilyError> {
        let mut arguments = vec!["send-many", aci];
        arguments.extend(texts.iter().map(String::as_str));
        self.cli(&arguments).map(drop)
    }

    /// What the store holds of the thread with `aci`, as `Stored <ms> by <aci> ...: <text>` lines.
    pub(super) fn messages(&self, aci: &str) -> Result<String, FamilyError> {
        Ok(oldest_first(&self.cli(&["messages", aci])?))
    }

    /// `sync`, killed (SIGKILL) after `seconds`, wherever that gets it: a crash, in the middle of
    /// receiving. Its output is whatever came before.
    fn sync_killed(&self, seconds: &str) -> Result<String, FamilyError> {
        let argv = [
            "timeout",
            "-s",
            "KILL",
            seconds,
            "flare-box",
            FLARE_DB,
            "sync",
            "20000",
        ];
        self.containers
            .exec_status(Service::Family, &argv)
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .map_err(|source| FamilyError::Step {
                what: "a killed flare-box sync".to_owned(),
                source,
            })
    }

    pub(super) fn send_group(&self, group: &str, text: &str) -> Result<(), FamilyError> {
        self.cli(&["send-group", group, text]).map(drop)
    }

    /// The groups it knows: "Group <id hex> "<name>" members <n>" a line.
    fn groups(&self) -> Result<String, FamilyError> {
        self.cli(&["groups"])
    }

    /// The id of the group named `title`, once flare-box knows it.
    pub(super) fn group_titled(
        &self,
        title: &str,
        patience: Duration,
    ) -> Result<String, FamilyError> {
        let started = Instant::now();
        loop {
            let listed = self.groups()?;
            let found = listed.lines().find_map(|line| {
                let rest = line.strip_prefix("Group ")?;
                let (id, rest) = rest.split_once(' ')?;
                rest.starts_with(&format!("{title:?}"))
                    .then(|| id.to_owned())
            });
            if let Some(id) = found {
                return Ok(id);
            }
            if started.elapsed() > patience {
                return Err(FamilyError::NoGroup {
                    title: title.to_owned(),
                    groups: listed,
                });
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

/// "ACI <uuid>" in what flare-box printed.
fn aci_printed(printed: &str) -> Result<String, FamilyError> {
    printed
        .lines()
        .find_map(|line| line.strip_prefix("ACI "))
        .map(|aci| aci.trim().to_owned())
        .ok_or_else(|| FamilyError::NoAci {
            printed: printed.to_owned(),
        })
}

/// b33p, the peer and the client write to each other: text each way, and a group of b33p's.
pub(super) fn flare(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
) -> Result<(), Failure> {
    let PlainBox {
        primary: _,
        peer,
        primary_aci,
        mut b33p,
    } = plain_box(checks, containers, sockets)?;
    let flare = FlareBox::new(containers);
    let peer_aci = peer.aci()?;
    let flare_aci = checks.require("Flare's backend registers as an account of its own", || {
        Ok(flare.register()?)
    })?;

    checks.record("text from the peer reaches Flare", || {
        let text = "testbox flare: from the peer";
        peer.send(&flare_aci, text)?;
        Ok(flare.sync_printing(text)?)
    });
    checks.record("text from Flare reaches the peer", || {
        let text = "testbox flare: from Flare to the peer";
        flare.send(&peer_aci, text)?;
        Ok(peer.sync_printing(text)?)
    });

    // b33p can't start a conversation by phone number (contact discovery isn't in the box), so
    // Flare writes first; the message is a request, which b33p accepts.
    let flare_chat = checks.require("text from Flare reaches b33p", || {
        let text = "testbox flare: from Flare to b33p";
        flare.send(&primary_aci, text)?;
        Ok(chat_previewing(&mut b33p, text)?)
    })?;
    checks.require(
        "Flare's first message is a message request in b33p, accepted",
        || Ok(accept_request(&mut b33p, &flare_chat)?),
    )?;
    checks.record("text from b33p reaches Flare", || {
        let text = "testbox flare: from b33p to Flare";
        select(&mut b33p, &flare_chat)?;
        type_and_send(&mut b33p, text)?;
        Ok(flare.sync_printing(text)?)
    });

    // The peer's too, for a group of the three.
    let peer_chat = checks.require("text from the peer reaches b33p", || {
        let text = "testbox flare: from the peer to b33p";
        peer.send(&primary_aci, text)?;
        Ok(chat_previewing(&mut b33p, text)?)
    })?;
    checks.require("the peer's message is a request in b33p, accepted", || {
        Ok(accept_request(&mut b33p, &peer_chat)?)
    })?;
    let title = "testbox flare group";
    let group = checks.require("b33p makes a group of the peer and Flare", || {
        b33p.act(Action::CreateGroup {
            title: title.to_owned(),
            members: vec![peer_chat.clone(), flare_chat.clone()],
        })?;
        Ok(b33p.wait_for("the group in b33p", PATIENCE, |snapshot| {
            snapshot
                .conversations
                .iter()
                .find(|chat| chat.group && chat.title == title && !chat.request)
                .map(|chat| chat.id.clone())
        })?)
    })?;
    let group_id = checks.require("Flare learns of the group", || {
        // The group's change reaches it as a message to it, or not until it is written to.
        select(&mut b33p, &group)?;
        type_and_send(&mut b33p, "testbox flare: hello, group")?;
        Ok(flare.group_titled(title, PATIENCE)?)
    })?;
    checks.record("text b33p sends to the group reaches Flare", || {
        let text = "testbox flare: b33p to the group";
        select(&mut b33p, &group)?;
        type_and_send(&mut b33p, text)?;
        Ok(flare.sync_printing(text)?)
    });
    checks.record(
        "text Flare sends to the group reaches b33p and the peer",
        || {
            let text = "testbox flare: Flare to the group";
            flare.send_group(&group_id, text)?;
            chat_previewing(&mut b33p, text)?;
            Ok(peer.sync_printing(text)?)
        },
    );
    checks.record(
        &format!("{BURST} messages in a burst reach Flare once each, in order"),
        || {
            let texts = numbered("testbox flare: burst", BURST);
            peer.send_all(&flare_aci, &texts)?;
            let printed = flare.sync()?;
            Ok(once_in_order("flare-box", &printed, &texts)?)
        },
    );
    // The peer's and b33p's at once, to one account: two senders' sessions, one receiver.
    checks.record(
        "texts the peer and b33p send at once both reach Flare",
        || {
            let (from_peer, from_b33p) = (
                "testbox flare: at once, from the peer",
                "testbox flare: at once, from b33p",
            );
            select(&mut b33p, &flare_chat)?;
            std::thread::scope(|scope| -> Result<(), Failure> {
                let sending = scope.spawn(|| peer.send(&flare_aci, from_peer));
                type_and_send(&mut b33p, from_b33p)?;
                sending.join().map_err(|_| FamilyError::Stuck {
                    what: "the peer's send panicked".to_owned(),
                })??;
                Ok(())
            })?;
            let printed = flare.sync()?;
            for text in [from_peer, from_b33p] {
                once_in_order("flare-box", &printed, &[text.to_owned()])?;
            }
            Ok(())
        },
    );
    // Each side's send while it receives what the other sends: a one-shot flare-box receives as
    // it sends, as the receive loop of Flare's manager does, and so does the peer's presage-cli.
    checks.record(
        "texts Flare and the peer send each other at once arrive, once each",
        || {
            let rounds = numbered("testbox flare: race", 6);
            for round in &rounds {
                let (to_flare, to_peer) =
                    (format!("{round} to Flare"), format!("{round} to the peer"));
                std::thread::scope(|scope| -> Result<(), Failure> {
                    let sending = scope.spawn(|| peer.send(&flare_aci, &to_flare));
                    flare.send(&peer_aci, &to_peer)?;
                    sending.join().map_err(|_| FamilyError::Stuck {
                        what: "the peer's send panicked".to_owned(),
                    })??;
                    Ok(())
                })?;
            }
            // What each one's store holds: a send receives too, and prints what it received, so
            // what a sync printed is only some of it.
            flare.sync()?;
            peer.sync()?;
            let to_flare: Vec<String> = rounds.iter().map(|r| format!("{r} to Flare")).collect();
            let to_peer: Vec<String> = rounds.iter().map(|r| format!("{r} to the peer")).collect();
            let (at_flare, at_peer) = (
                flare.messages(&peer_aci)?,
                oldest_first(&peer.messages_with(&flare_aci)?),
            );
            for (client, stored) in [
                ("flare-box's store", &at_flare),
                ("presage-cli's store", &at_peer),
            ] {
                once_in_order(client, stored, &to_flare)?;
                once_in_order(client, stored, &to_peer)?;
            }
            Ok(())
        },
    );
    // A crash in the middle of receiving a burst: what was read and not acknowledged comes again,
    // and the store has each message once, in order.
    checks.record(
        "Flare killed while it receives a burst loses and repeats nothing",
        || {
            let texts = numbered("testbox flare: kill", BURST);
            std::thread::scope(|scope| -> Result<(), Failure> {
                let sending = scope.spawn(|| peer.send_all(&flare_aci, &texts));
                flare.sync_killed("4")?;
                sending.join().map_err(|_| FamilyError::Stuck {
                    what: "the peer's burst panicked".to_owned(),
                })??;
                Ok(())
            })?;
            flare.sync()?;
            let stored = flare.messages(&peer_aci)?;
            Ok(once_in_order("flare-box's store", &stored, &texts)?)
        },
    );
    Ok(())
}

/// gurk-rs, the terminal client, in tmux in the `family` container: its keys typed, its screen
/// read, as a person at a terminal would.
struct Gurk<'pod> {
    containers: &'pod Pod,
}

const GURK_DIR: &str = "/presage/family-gurk";
const GURK_SESSION: &str = "gurk";
/// What the terminal is as wide and high as (tmux `-x`, `-y`): wide, as messages wrap at it.
const GURK_SIZE: [&str; 4] = ["-x", "200", "-y", "50"];

impl<'pod> Gurk<'pod> {
    fn step(&self, what: &str, argv: &[&str], stdin: &str) -> Result<String, FamilyError> {
        self.containers
            .exec(Service::Family, argv, stdin)
            .map_err(|source| FamilyError::Step {
                what: what.to_owned(),
                source,
            })
    }

    fn tmux(&self, arguments: &[&str]) -> Result<String, FamilyError> {
        let argv = [&["tmux"][..], arguments].concat();
        self.step(
            &format!("tmux {}", arguments.first().copied().unwrap_or("")),
            &argv,
            "",
        )
    }

    /// Writes gurk's configuration and starts it in a tmux session; `relink` links it as a new
    /// device (the first start), which it does against the box's servers (testbox/family's
    /// patch: `GURK_SIGNAL_SERVERS_FILE`).
    fn start(&self, relink: bool) -> Result<(), FamilyError> {
        let config = format!(
            "data_dir = \"{GURK_DIR}/data\"\npassphrase = \"testbox gurk passphrase\"\n\
             [user]\ndisplay_name = \"gurk\"\n[notifications]\nenabled = false\n"
        );
        let script = format!("mkdir -p {GURK_DIR} && cat > {GURK_DIR}/config.toml");
        self.step("gurk's configuration", &["sh", "-c", &script], &config)?;
        let servers = KeyFile::Servers.path();
        let servers = format!("GURK_SIGNAL_SERVERS_FILE={}", servers.to_string_lossy());
        let config_path = format!("{GURK_DIR}/config.toml");
        let mut argv = vec!["new-session", "-d", "-s", GURK_SESSION];
        argv.extend(GURK_SIZE);
        argv.extend(["-c", GURK_DIR, "-e", &servers, "-e", "RUST_LOG=info"]);
        argv.extend(["gurk", "--config", &config_path, "--verbose"]);
        if relink {
            argv.push("--relink");
        }
        self.tmux(&argv).map(drop)
    }

    /// The terminal's text as it is, wrapped lines joined.
    fn screen(&self) -> Result<String, FamilyError> {
        self.tmux(&["capture-pane", "-p", "-J", "-t", GURK_SESSION])
    }

    /// Whether gurk still runs: its session ends with it.
    fn alive(&self) -> bool {
        self.tmux(&["has-session", "-t", GURK_SESSION]).is_ok()
    }

    /// `screen` until `found` takes something from it; the last screen if it never does.
    fn wait<Found>(
        &self,
        patience: Duration,
        found: impl Fn(&str) -> Option<Found>,
    ) -> Result<Found, FamilyError> {
        let started = Instant::now();
        loop {
            let screen = self.screen()?;
            if let Some(found) = found(&screen) {
                return Ok(found);
            }
            if started.elapsed() > patience {
                return Err(FamilyError::NoScreen { screen });
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// The link URL gurk prints (testbox/family's patch) while it waits to be linked.
    fn link_url(&self) -> Result<String, FamilyError> {
        self.wait(Duration::from_secs(60), |screen| {
            screen
                .lines()
                .find_map(|line| line.strip_prefix("LINK-URL "))
                .map(|url| url.trim().to_owned())
        })
    }

    /// Once the terminal shows gurk's panes: it has linked and started.
    fn await_ui(&self) -> Result<(), FamilyError> {
        self.wait(Duration::from_secs(60), |screen| {
            (screen.contains("Channels") && screen.contains("Messages")).then_some(())
        })
    }

    fn keys(&self, keys: &[&str]) -> Result<(), FamilyError> {
        let argv = [&["send-keys", "-t", GURK_SESSION][..], keys].concat();
        self.tmux(&argv).map(drop)
    }

    fn type_text(&self, text: &str) -> Result<(), FamilyError> {
        self.tmux(&["send-keys", "-t", GURK_SESSION, "-l", text])
            .map(drop)
    }

    /// Opens the chat `name` shows as in the channel pop-up (ctrl+p, the name, enter).
    fn open(&self, name: &str) -> Result<(), FamilyError> {
        self.keys(&["Escape"])?;
        self.keys(&["C-p"])?;
        self.type_text(name)?;
        self.keys(&["Enter"])
    }

    /// Fails unless the open chat shows `expected` within `patience`.
    fn shows(&self, expected: &str, patience: Duration) -> Result<(), FamilyError> {
        self.wait(patience, |screen| screen.contains(expected).then_some(()))
            .map_err(|error| match error {
                FamilyError::NoScreen { screen } => FamilyError::NotShown {
                    client: "gurk",
                    expected: expected.to_owned(),
                    seen: screen,
                },
                other => other,
            })
    }

    /// Types `text` into the open chat and sends it.
    fn send(&self, text: &str) -> Result<(), FamilyError> {
        self.type_text(text)?;
        self.keys(&["Enter"])
    }

    /// Ends gurk's session, and waits for the process to go: a terminal closed on it.
    fn stop(&self) -> Result<(), FamilyError> {
        self.tmux(&["kill-session", "-t", GURK_SESSION])?;
        // By /proc: the image has no ps. A process's `comm` is its executable's name. A zombie
        // is gone: tmux's server ends with its last session, and gurk, its child, is then the
        // container's first process's (`sleep infinity`, which reaps nothing).
        let gone = "for p in /proc/[0-9]*; do \
            [ \"$(cat $p/comm 2>/dev/null)\" = gurk ] || continue; \
            case \"$(grep State: $p/status)\" in *zombie*) ;; *) exit 1 ;; esac; done; exit 0";
        let started = Instant::now();
        while self
            .containers
            .exec_status(Service::Family, &["sh", "-c", gone])
            .map_err(|source| FamilyError::Step {
                what: "looking for gurk".to_owned(),
                source,
            })?
            .status
            .code()
            != Some(0)
        {
            if started.elapsed() > Duration::from_secs(30) {
                return Err(FamilyError::Stuck {
                    what: "gurk didn't end within 30 s of its terminal closing".to_owned(),
                });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(())
    }

    /// gurk's own log (`--verbose`): its lines of `level` ("ERROR", "WARN").
    fn logged(&self, level: &str) -> Result<String, FamilyError> {
        let script = format!("grep -a ' {level} ' {GURK_DIR}/gurk.log || true");
        self.step("gurk's log", &["sh", "-c", &script], "")
    }
}

/// gurk-rs is linked to a presage-cli primary, as a secondary device, and run in tmux.
pub(super) fn gurk(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
) -> Result<(), Failure> {
    let PlainBox {
        peer,
        primary_aci,
        mut b33p,
        ..
    } = plain_box(checks, containers, sockets)?;
    let host = Presage::new(containers, Account::Family(0));
    let gurk = Gurk { containers };
    let host_aci = checks.require("gurk's primary registers", || Ok(host.register()?))?;
    checks.require("gurk links to its primary", || {
        gurk.start(true)?;
        let url = gurk.link_url()?;
        host.add_device(&url)?;
        Ok(gurk.await_ui()?)
    })?;

    checks.record("text from the peer reaches gurk", || {
        let text = "testbox gurk: from the peer";
        peer.send(&host_aci, text)?;
        // The chat is listed once its first message has come, by the peer's profile name (or,
        // until gurk has it, otherwise): the channel pop-up filters by what it types.
        let name = peer.name().to_string();
        gurk.wait(PATIENCE, |screen| screen.contains(&name).then_some(()))?;
        gurk.open(&name)?;
        Ok(gurk.shows(text, PATIENCE)?)
    });
    checks.record("text from gurk reaches the peer", || {
        let text = "testbox gurk: from gurk to the peer";
        gurk.open(&peer.name())?;
        gurk.send(text)?;
        Ok(peer.sync_printing(text)?)
    });

    // b33p can't start a conversation by phone number: the account gurk is a device of writes
    // first (it is a message request, which b33p accepts).
    let host_chat = checks.require("text from gurk's account reaches b33p", || {
        let text = "testbox gurk: from gurk's account to b33p";
        host.send(&primary_aci, text)?;
        Ok(chat_previewing(&mut b33p, text)?)
    })?;
    checks.require(
        "gurk's account's first message is a request in b33p, accepted",
        || Ok(accept_request(&mut b33p, &host_chat)?),
    )?;
    checks.record("text from b33p reaches gurk", || {
        let text = "testbox gurk: from b33p to gurk";
        select(&mut b33p, &host_chat)?;
        type_and_send(&mut b33p, text)?;
        gurk.open(&Account::Primary.name())?;
        Ok(gurk.shows(text, PATIENCE)?)
    });
    checks.record("text from gurk reaches b33p", || {
        let text = "testbox gurk: from gurk to b33p";
        gurk.open(&Account::Primary.name())?;
        gurk.send(text)?;
        chat_previewing(&mut b33p, text)?;
        Ok(())
    });
    checks.record(
        &format!("{BURST} messages in a burst reach gurk once each, in order"),
        || {
            let texts = numbered("testbox gurk: burst", BURST);
            let name = peer.name().to_string();
            peer.send_all(&host_aci, &texts)?;
            gurk.open(&name)?;
            let last = texts.last().map(String::as_str).unwrap_or_default();
            gurk.shows(last, PATIENCE)?;
            let screen = gurk.screen()?;
            Ok(once_in_order("gurk", &screen, &texts)?)
        },
    );
    checks.record(
        "messages sent while gurk is stopped reach it once it starts again",
        || {
            let texts = numbered("testbox gurk: while stopped", 5);
            let name = peer.name().to_string();
            gurk.stop()?;
            peer.send_all(&host_aci, &texts)?;
            gurk.start(false)?;
            gurk.await_ui()?;
            gurk.wait(PATIENCE, |screen| screen.contains(&name).then_some(()))?;
            gurk.open(&name)?;
            let last = texts.last().map(String::as_str).unwrap_or_default();
            gurk.shows(last, PATIENCE)?;
            let screen = gurk.screen()?;
            Ok(once_in_order("gurk", &screen, &texts)?)
        },
    );
    checks.record(
        "texts gurk and the peer send each other at once arrive, once each",
        || {
            let from_peer = numbered("testbox gurk: race, peer", 10);
            let from_gurk = numbered("testbox gurk: race, gurk", 10);
            let name = peer.name().to_string();
            gurk.open(&name)?;
            std::thread::scope(|scope| -> Result<(), Failure> {
                let sending = scope.spawn(|| peer.send_all(&host_aci, &from_peer));
                for text in &from_gurk {
                    gurk.send(text)?;
                }
                sending.join().map_err(|_| FamilyError::Stuck {
                    what: "the peer's burst panicked".to_owned(),
                })??;
                Ok(())
            })?;
            let last = from_peer.last().map(String::as_str).unwrap_or_default();
            gurk.shows(last, PATIENCE)?;
            once_in_order("gurk", &gurk.screen()?, &from_peer)?;
            once_in_order("gurk", &gurk.screen()?, &from_gurk)?;
            // The peer's store, not what it printed: its sends receive too, and print it there.
            peer.sync()?;
            let stored = oldest_first(&peer.messages_with(&host_aci)?);
            Ok(once_in_order("presage-cli's store", &stored, &from_gurk)?)
        },
    );
    checks.record(
        "gurk killed while it receives a burst loses and repeats nothing",
        || {
            let texts = numbered("testbox gurk: kill", BURST);
            let name = peer.name().to_string();
            gurk.open(&name)?;
            std::thread::scope(|scope| -> Result<(), Failure> {
                let sending = scope.spawn(|| peer.send_all(&host_aci, &texts));
                // In the middle of it: a few messages in.
                std::thread::sleep(Duration::from_secs(4));
                gurk.stop()?;
                sending.join().map_err(|_| FamilyError::Stuck {
                    what: "the peer's burst panicked".to_owned(),
                })??;
                Ok(())
            })?;
            gurk.start(false)?;
            gurk.await_ui()?;
            gurk.wait(PATIENCE, |screen| screen.contains(&name).then_some(()))?;
            gurk.open(&name)?;
            let last = texts.last().map(String::as_str).unwrap_or_default();
            gurk.shows(last, PATIENCE)?;
            Ok(once_in_order("gurk", &gurk.screen()?, &texts)?)
        },
    );
    checks.record("gurk is still running and logged no error", || {
        if !gurk.alive() {
            return Err(FamilyError::Gone.into());
        }
        let errors = gurk.logged("ERROR")?;
        if errors.trim().is_empty() {
            Ok(())
        } else {
            Err(FamilyError::Logged { lines: errors }.into())
        }
    });
    Ok(())
}
