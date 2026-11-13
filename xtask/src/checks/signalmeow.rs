//! signalmeow, mautrix-signal's Go implementation of Signal's client protocol, as a client of the
//! box: neither presage nor libsignal-service (Rust or Java) is in it, only libsignal's
//! cryptography through its FFI. Chat, REST, gRPC and the zkgroup calls are its own code, so
//! what it agrees with here, b33p and Signal-Server agree on without a shared implementation
//! (testbox/signalmeow, docs/onboarding-signalmeow.md).
//!
//! It is driven like presage-cli, by `podman exec` of testbox/signalmeow/driver, whose lines for
//! what it received are presage-cli's ("From <aci> @ <timestamp>: <text>").

use std::io::{BufRead, BufReader, Read};

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

use super::attachments::{incoming_attachment, saved};
use super::b33p::{accept_request, b33p_shows, chat_previewing, delivered, select, type_and_send};
use super::presage::{Account, Presage};
use super::setup::{PlainBox, plain_box};
use super::{Checks, Failure, PATIENCE};
use crate::Sockets;
use crate::pod::{Pod, PodError, Service};
use b33p_harness::{Action, AttachmentKind};

/// Where the driver keeps the account (the `signalmeow` volume).
const DB: &str = "/signalmeow/meow.db";
/// The peer's first message to b33p's account, which b33p accepts as a message request.
const PEER_FIRST: &str = "testbox signalmeow: the peer's first message";

/// What failed driving signalmeow in its container.
#[derive(Debug, thiserror::Error)]
pub(super) enum SignalmeowError {
    /// A driver command: `command` is its subcommand, as "send".
    #[error("signalmeow-driver {command}")]
    Command {
        command: &'static str,
        #[source]
        source: PodError,
    },
    /// `link` ended without printing the URL a primary adds, or the account it linked.
    #[error("signalmeow-driver link {what}:\n{stderr}")]
    Link { what: &'static str, stderr: String },
    /// The driver printed nothing that says whose account it is.
    #[error("no aci in the driver's whoami: {whoami:?}")]
    NoAci { whoami: String },
    /// A sync that should have received `expected` didn't print it.
    #[error("signalmeow's sync printed no {expected:?}:\n{received}")]
    NotPrinted { expected: String, received: String },
    /// The `serve` process refused or didn't answer a command.
    #[error("{0}")]
    Serve(String),
    /// The thread that drained the driver's log panicked.
    #[error("the thread reading signalmeow's log panicked")]
    LogThread,
}

/// signalmeow's one account of the box, in the `signalmeow` container.
pub(super) struct Signalmeow<'pod> {
    containers: &'pod Pod,
    /// The driver's `serve`, when the account stays connected (see [`Self::serve`]).
    daemon: Option<Daemon>,
}

/// The driver's `serve` in the container: one process that stays connected and takes its commands
/// as JSON lines, as a client of its own does. Receiving goes on all the while, so that nothing
/// arrives as a short-lived process ends (a pre-key message handled as the process exits leaves
/// its one-time key spent and its session unsaved: the sender's whole burst then fails with
/// "invalid Kyber prekey identifier", and what was written down before it is written down again).
struct Daemon {
    child: std::sync::Mutex<std::process::Child>,
    stdin: std::sync::Mutex<std::process::ChildStdin>,
    next: std::sync::atomic::AtomicU64,
    shared: std::sync::Arc<DaemonShared>,
}

/// A command line to the driver's `serve`: its `id` comes back in the answer.
#[derive(Serialize)]
struct Line<'a> {
    id: u64,
    #[serde(flatten)]
    command: Command<'a>,
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum Command<'a> {
    Send {
        to: &'a str,
        text: &'a str,
    },
    SendFile {
        to: &'a str,
        path: &'a str,
        text: &'a str,
    },
    SendGroup {
        master_key: &'a str,
        text: &'a str,
    },
    React {
        to: &'a str,
        author: &'a str,
        target: u64,
        emoji: &'a str,
    },
    Edit {
        to: &'a str,
        target: u64,
        text: &'a str,
    },
    Quit,
}

/// A line the driver's `serve` writes (`emit` in its main.go): the event, and what the checks
/// read of it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Event {
    event: String,
    /// An answer's: the command's id, whether it worked, why not, and the time it was sent at.
    id: Option<u64>,
    ok: bool,
    error: Option<String>,
    timestamp: Option<u64>,
    /// A message's, or a decryption failure's: the sender, and for a message its text and whether
    /// this account's own other device wrote it.
    from: Option<String>,
    own: bool,
    text: Option<String>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        use std::io::Write as _;
        if let Ok(mut stdin) = self.stdin.lock() {
            if let Ok(quit) = serde_json::to_string(&Line {
                id: 0,
                command: Command::Quit,
            }) {
                drop(writeln!(stdin, "{quit}"));
            }
            drop(stdin.flush());
        }
        if let Ok(mut child) = self.child.lock() {
            let until = std::time::Instant::now() + Duration::from_secs(15);
            while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(100));
            }
            if matches!(child.try_wait(), Ok(None)) {
                drop(child.kill());
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        let log = self
            .shared
            .log
            .lock()
            .map(|log| log.clone())
            .unwrap_or_default();
        if !log.trim().is_empty() {
            eprintln!("  signalmeow-driver serve's log:\n{log}");
        }
    }
}

#[derive(Default)]
struct DaemonShared {
    answers: std::sync::Mutex<std::collections::HashMap<u64, Event>>,
    answered: std::sync::Condvar,
    /// What the driver said it could not decrypt, for [`Signalmeow::noting_lost`].
    lost: std::sync::Mutex<Vec<String>>,
    /// The texts it received since they were last taken, a line each as `sync` prints them.
    received: std::sync::Mutex<Vec<String>>,
    log: std::sync::Mutex<String>,
}

impl<'pod> Signalmeow<'pod> {
    pub(super) fn new(containers: &'pod Pod) -> Self {
        Self {
            containers,
            daemon: None,
        }
    }

    /// Keeps the account connected from now on, in one `serve` process: sends go to it, and receiving
    /// is its own. Once the account is linked.
    pub(super) fn serve(&mut self) -> Result<(), Failure> {
        use std::io::BufRead as _;
        let mut child = self.containers.spawn_exec_interactive(
            Service::Signalmeow,
            &["signalmeow-driver", "--db", DB, "serve"],
        )?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let shared = std::sync::Arc::new(DaemonShared::default());
        let ready = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        {
            let (shared, ready) = (shared.clone(), ready.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let Ok(event) = serde_json::from_str::<Event>(&line) else {
                        continue;
                    };
                    match event.event.as_str() {
                        "ready" => {
                            *ready
                                .0
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                            ready.1.notify_all();
                        }
                        "answer" => {
                            if let Some(id) = event.id {
                                shared
                                    .answers
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .insert(id, event);
                                shared.answered.notify_all();
                            }
                        }
                        "message" if !event.own => shared
                            .received
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "From {} @ {}: {}",
                                event.from.as_deref().unwrap_or("?"),
                                event.timestamp.unwrap_or_default(),
                                event.text.as_deref().unwrap_or("")
                            )),
                        "decryption-error" => shared
                            .lost
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!(
                                "failed to decrypt a message from <ACI:{}> of {}: {}",
                                event.from.as_deref().unwrap_or("?"),
                                event.timestamp.unwrap_or_default(),
                                event.error.as_deref().unwrap_or("?")
                            )),
                        _ => {}
                    }
                }
                // The driver ended: whoever waits for an answer must not wait for ever.
                shared.answered.notify_all();
            });
        }
        {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let mut log = String::new();
                drop(stderr.read_to_string(&mut log));
                *shared
                    .log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = log;
            });
        }
        let (flag, signal) = (&ready.0, &ready.1);
        let guard = flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (guard, timeout) = signal
            .wait_timeout_while(guard, Duration::from_secs(60), |ready| !*ready)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if timeout.timed_out() && !*guard {
            return Err("signalmeow-driver serve was not ready in 60 s".into());
        }
        drop(guard);
        self.daemon = Some(Daemon {
            child: std::sync::Mutex::new(child),
            stdin: std::sync::Mutex::new(stdin),
            next: std::sync::atomic::AtomicU64::new(1),
            shared,
        });
        Ok(())
    }

    /// One command to the `serve` process, and the time it says it sent the message at, if it did.
    fn ask(&self, daemon: &Daemon, command: Command<'_>) -> Result<Option<u64>, Failure> {
        use std::io::Write as _;
        let id = daemon
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut stdin = daemon
                .stdin
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            writeln!(stdin, "{}", serde_json::to_string(&Line { id, command })?)?;
            stdin.flush()?;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        let mut answers = daemon
            .shared
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(answer) = answers.remove(&id) {
                return if answer.ok {
                    Ok(answer.timestamp)
                } else {
                    Err(format!(
                        "signalmeow-driver serve: {}",
                        answer.error.unwrap_or_default()
                    )
                    .into())
                };
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err("signalmeow-driver serve gave no answer in 90 s".into());
            }
            answers = daemon
                .shared
                .answered
                .wait_timeout(answers, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// What the `serve` process could not decrypt, noted as lost ([`Pod::note_lost`]).
    pub(super) fn flush_lost(&self) {
        if let Some(daemon) = &self.daemon {
            let lost = std::mem::take(
                &mut *daemon
                    .shared
                    .lost
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            for line in lost {
                self.containers.note_lost(format!("signalmeow: {line}"));
            }
        }
    }

    /// Runs the driver's `command` with `arguments`; what it printed.
    fn driver(
        &self,
        command: &'static str,
        arguments: &[&str],
        stdin: &str,
    ) -> Result<String, SignalmeowError> {
        let argv = [&["signalmeow-driver", "--db", DB, command][..], arguments].concat();
        self.containers
            .exec(Service::Signalmeow, &argv, stdin)
            .map_err(|source| SignalmeowError::Command { command, source })
    }

    /// Links as a secondary device of `primary`'s account, and returns that account's ACI: the
    /// driver prints the URL its provisioning offers, the primary adds the device, and the driver
    /// prints the account once the server has confirmed it.
    pub(super) fn link(&self, primary: &Presage<'_>) -> Result<String, Failure> {
        let mut child = self.containers.spawn_exec(
            Service::Signalmeow,
            &["signalmeow-driver", "--db", DB, "link", "signalmeow"],
        )?;
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        // The log goes on while the primary works; unread, a full pipe would hold the driver up.
        let log = std::thread::spawn(move || {
            let mut log = String::new();
            if let Err(error) = stderr.read_to_string(&mut log) {
                log.push_str(&format!("\n(reading the log failed: {error})"));
            }
            log
        });
        let outcome = (|| -> Result<String, Failure> {
            let mut lines = BufReader::new(stdout).lines();
            let mut url = None;
            for line in lines.by_ref() {
                if let Some(found) = line?.strip_prefix("URL ") {
                    url = Some(found.to_owned());
                    break;
                }
            }
            let url = url.ok_or(SignalmeowError::Link {
                what: "printed no URL",
                stderr: String::new(),
            })?;
            primary.add_device(&url)?;
            for line in lines {
                if let Some(aci) = line?.strip_prefix("linked ") {
                    return Ok(aci.trim().to_owned());
                }
            }
            Err(SignalmeowError::Link {
                what: "did not link",
                stderr: String::new(),
            }
            .into())
        })();
        let status = child.wait()?;
        let log = log.join().map_err(|_| SignalmeowError::LogThread)?;
        match outcome {
            Ok(aci) if status.success() => Ok(aci),
            Ok(_) => Err(SignalmeowError::Link {
                what: "failed after it linked",
                stderr: log,
            }
            .into()),
            Err(error) => {
                // The driver's own account of it beats the pipe closing.
                eprintln!("  signalmeow-driver link: {error}\n{log}");
                Err(error)
            }
        }
    }

    pub(super) fn aci(&self) -> Result<String, SignalmeowError> {
        let whoami = self.driver("whoami", &[], "")?;
        let aci = whoami.trim();
        if aci.len() == 36 {
            Ok(aci.to_owned())
        } else {
            Err(SignalmeowError::NoAci { whoami })
        }
    }

    /// Receiving, and notes what was lost ([`Pod::note_lost`]): the driver's wording is
    /// presage-cli's.
    fn noting_lost(&self, output: String) -> String {
        for line in output.lines().filter(|line| {
            line.contains("failed to decrypt a message")
                || line.contains("failed to download an attachment")
        }) {
            self.containers
                .note_lost(format!("signalmeow: {}", line.trim()));
        }
        output
    }

    /// `command` to the `serve` process, and what it answers.
    fn daemon_ask(&self, command: Command<'_>) -> Result<Option<u64>, SignalmeowError> {
        let daemon = self
            .daemon
            .as_ref()
            .ok_or_else(|| SignalmeowError::Serve("the serve process isn't running".to_owned()))?;
        self.ask(daemon, command)
            .map_err(|error| SignalmeowError::Serve(error.to_string()))
    }

    pub(super) fn send(&self, recipient_aci: &str, text: &str) -> Result<(), SignalmeowError> {
        if self.daemon.is_some() {
            return self.send_stamped(recipient_aci, text).map(drop);
        }
        self.driver("send", &[recipient_aci, text], "")
            .map(|printed| drop(self.noting_lost(printed)))
    }

    /// [`Self::send`], through the `serve` process; the time the text was sent at.
    pub(super) fn send_stamped(
        &self,
        recipient_aci: &str,
        text: &str,
    ) -> Result<u64, SignalmeowError> {
        self.daemon_ask(Command::Send {
            to: recipient_aci,
            text,
        })?
        .ok_or_else(|| SignalmeowError::Serve("the answer to a send has no time".to_owned()))
    }

    /// Reacts with `emoji` to the message `author` sent at `target`, in the chat with `recipient_aci`.
    pub(super) fn react(
        &self,
        recipient_aci: &str,
        author: &str,
        target: u64,
        emoji: &str,
    ) -> Result<(), SignalmeowError> {
        self.daemon_ask(Command::React {
            to: recipient_aci,
            author,
            target,
            emoji,
        })
        .map(drop)
    }

    /// Changes this account's message sent at `target`, in the chat with `recipient_aci`, to `text`.
    pub(super) fn edit(
        &self,
        recipient_aci: &str,
        target: u64,
        text: &str,
    ) -> Result<(), SignalmeowError> {
        self.daemon_ask(Command::Edit {
            to: recipient_aci,
            target,
            text,
        })
        .map(drop)
    }

    /// Sends `text` with the file at `path` (in the container: `/files/…`) to `recipient_aci`.
    pub(super) fn send_file(
        &self,
        recipient_aci: &str,
        path: &str,
        text: &str,
    ) -> Result<(), SignalmeowError> {
        if self.daemon.is_some() {
            return self
                .daemon_ask(Command::SendFile {
                    to: recipient_aci,
                    path,
                    text,
                })
                .map(drop);
        }
        self.driver("send-file", &[recipient_aci, path, text], "")
            .map(|printed| drop(self.noting_lost(printed)))
    }

    /// Sends `text` to the group with `master_key` (hex or base64).
    pub(super) fn send_to_group(
        &self,
        master_key: &str,
        text: &str,
    ) -> Result<(), SignalmeowError> {
        if self.daemon.is_some() {
            return self
                .daemon_ask(Command::SendGroup { master_key, text })
                .map(drop);
        }
        self.driver("send-group", &[master_key, text], "")
            .map(|printed| drop(self.noting_lost(printed)))
    }

    /// What the driver's store holds of its chat with `who` (an ACI, or a group's identifier).
    pub(super) fn messages_with(&self, who: &str) -> Result<String, SignalmeowError> {
        self.driver("list-messages", &[who], "")
    }

    /// Receives what is queued; what it printed.
    pub(super) fn sync(&self) -> Result<String, SignalmeowError> {
        if let Some(daemon) = &self.daemon {
            // Receiving is the `serve` process's, all the time: what it received since.
            self.flush_lost();
            let lines = std::mem::take(
                &mut *daemon
                    .shared
                    .received
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            return Ok(lines.iter().map(|line| format!("{line}\n")).collect());
        }
        self.driver("sync", &[], "")
            .map(|received| self.noting_lost(received))
    }

    /// Receives what is queued; fails unless the driver printed `expected`.
    pub(super) fn sync_printing(&self, expected: &str) -> Result<(), SignalmeowError> {
        let received = self.sync()?;
        if received.contains(expected) {
            Ok(())
        } else {
            Err(SignalmeowError::NotPrinted {
                expected: expected.to_owned(),
                received,
            })
        }
    }
}

/// The lowercase hex of `bytes`' SHA-256, as the driver prints a received attachment's.
pub(super) fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `len` bytes that don't compress: a file the CDN's padding and the encryption can't hide a
/// wrong byte in. xorshift from a fixed seed.
fn blob(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// signalmeow as a second account beside b33p's: text each way with a presage-cli account and with
/// b33p, files each way through the CDNs, and a group b33p makes of the three, in which each writes.
pub(super) fn signalmeow(
    checks: &mut Checks,
    containers: &Pod,
    sockets: &Sockets,
    files: &Path,
) -> Result<(), Failure> {
    let PlainBox {
        primary: _primary,
        peer,
        primary_aci,
        mut b33p,
    } = plain_box(checks, containers, sockets)?;
    let account = Presage::new(containers, Account::Meow);
    let meow = Signalmeow::new(containers);
    let peer_aci = peer.aci()?;
    checks.require("a presage-cli account registers for signalmeow", || {
        Ok(account.register()?)
    })?;
    let meow_aci = checks.require("signalmeow links to it", || meow.link(&account))?;
    checks.require("signalmeow knows its account", || {
        let aci = meow.aci()?;
        if aci == meow_aci {
            Ok(())
        } else {
            Err(format!("whoami says {aci}, linking said {meow_aci}").into())
        }
    })?;

    // --- with a presage-cli account.
    let to_peer = "testbox signalmeow: to the peer";
    let from_peer = "testbox signalmeow: from the peer";
    checks.require("text from signalmeow reaches the peer", || {
        meow.send(&peer_aci, to_peer)?;
        Ok(peer.sync_printing(to_peer)?)
    })?;
    checks.require("text from the peer reaches signalmeow", || {
        peer.send(&meow_aci, from_peer)?;
        Ok(meow.sync_printing(from_peer)?)
    })?;
    checks.record("signalmeow keeps what it sent and received", || {
        let kept = meow.messages_with(&peer_aci)?;
        for text in [to_peer, from_peer] {
            if !kept.contains(text) {
                return Err(format!("signalmeow's store has no {text:?}:\n{kept}").into());
            }
        }
        Ok(())
    });
    // presage-cli sends no receipts (checks/core.rs), so the receipt that matters is signalmeow's:
    // it sent one for the peer's text when it received it.
    checks.record(
        "signalmeow sends a delivery receipt for the peer's text",
        || Ok(peer.sync_printing("got Delivery receipt")?),
    );

    // --- with b33p.
    let to_b33p = "testbox signalmeow: to b33p";
    let from_b33p = "testbox signalmeow: from b33p";
    let meow_chat = checks.require("text from signalmeow reaches b33p", || {
        meow.send(&primary_aci, to_b33p)?;
        Ok(chat_previewing(&mut b33p, to_b33p)?)
    })?;
    checks.require(
        "signalmeow's first message is a message request in b33p, accepted",
        || Ok(accept_request(&mut b33p, &meow_chat)?),
    )?;
    checks.require("text from b33p reaches signalmeow", || {
        select(&mut b33p, &meow_chat)?;
        type_and_send(&mut b33p, from_b33p)?;
        Ok(meow.sync_printing(from_b33p)?)
    })?;
    checks.record("b33p shows signalmeow's delivery receipt", || {
        Ok(delivered(&mut b33p, from_b33p)?)
    });

    // --- files, through the CDNs: encrypted by one implementation, decrypted by another.
    let bytes = blob(300 * 1024 + 17);
    let digest = sha256(&bytes);
    testbox_tools::write(&files.join("meow-blob.bin"), &bytes)?;
    let blob_path = "/files/meow-blob.bin";
    let file_to_b33p = "testbox signalmeow: a file to b33p";
    checks.record(
        "a file from signalmeow reaches b33p, downloaded, decrypted and saved",
        || {
            meow.send_file(&primary_aci, blob_path, file_to_b33p)?;
            select(&mut b33p, &meow_chat)?;
            let id = incoming_attachment(&mut b33p, Some(file_to_b33p), AttachmentKind::File)?;
            // A file is downloaded when saved: b33p fetches only what it shows inline (a picture;
            // state/attachments.rs `fetch_attachment`).
            b33p.act(Action::SaveAttachment { id: id.clone() })?;
            Ok(saved(&mut b33p, &id)?)
        },
    );
    let file_to_meow = "testbox signalmeow: a file to signalmeow";
    checks.record("a file from the peer reaches signalmeow intact", || {
        peer.send_file(&meow_aci, file_to_meow, blob_path)?;
        let received = meow.sync()?;
        if received.contains(&format!("{} bytes sha256:{digest}", bytes.len())) {
            Ok(())
        } else {
            Err(format!(
                "no attachment of {} bytes, sha256 {digest}, in:\n{received}",
                bytes.len()
            )
            .into())
        }
    });
    checks.record("a file from b33p reaches signalmeow intact", || {
        b33p.act(Action::StageFiles {
            paths: vec![blob_path.to_owned()],
        })?;
        select(&mut b33p, &meow_chat)?;
        type_and_send(&mut b33p, "testbox signalmeow: a file from b33p")?;
        let received = meow.sync()?;
        if received.contains(&format!("{} bytes sha256:{digest}", bytes.len())) {
            Ok(())
        } else {
            Err(format!(
                "no attachment of {} bytes, sha256 {digest}, in:\n{received}",
                bytes.len()
            )
            .into())
        }
    });

    // --- a group of the three, made by b33p.
    let peer_chat = checks.require("the peer's first message reaches b33p, accepted", || {
        peer.send(&primary_aci, PEER_FIRST)?;
        let chat = chat_previewing(&mut b33p, PEER_FIRST)?;
        accept_request(&mut b33p, &chat)?;
        Ok(chat)
    })?;
    let title = "testbox signalmeow group";
    let group = checks.require("b33p makes a group of the peer and signalmeow", || {
        b33p.act(Action::CreateGroup {
            title: title.to_owned(),
            members: vec![peer_chat.clone(), meow_chat.clone()],
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
    let hello = "testbox signalmeow group: hello from b33p";
    checks.require("text b33p sends to the group reaches signalmeow", || {
        select(&mut b33p, &group)?;
        type_and_send(&mut b33p, hello)?;
        Ok(meow.sync_printing(hello)?)
    })?;
    let answer = "testbox signalmeow group: signalmeow answers";
    checks.require(
        "text signalmeow sends to the group reaches b33p and the peer",
        || {
            meow.send_to_group(&master_key, answer)?;
            b33p_shows(&mut b33p, &format!("{answer:?}"), |message| {
                !message.outgoing && message.text == answer
            })?;
            Ok(peer.sync_printing(answer)?)
        },
    )?;
    Ok(())
}
