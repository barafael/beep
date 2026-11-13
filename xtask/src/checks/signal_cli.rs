//! signal-cli (testbox/signal-cli), a client that is neither presage nor libsignal-service-rs:
//! Java on Turasa's libsignal-service-java and libsignal-net, driven the way `presage.rs` drives
//! presage-cli, by `podman exec` in its container. Its methods mirror [`super::presage::Presage`]'s,
//! with what signal-cli differs in: it stores no messages, so what `receive` printed is kept here,
//! and a group is its base64 id, not a master key.

use std::borrow::Cow;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use std::path::Path;

use super::attachments::{downloaded, incoming_attachment, stage_picture};
use super::b33p::{chat_previewing, select, type_and_send};
use super::presage::{Presage, PresageError};
use super::setup::InteropBox;
use super::{Checks, Failure, PATIENCE};
use crate::pod::{Pod, PodError, Service};
use b33p_harness::{Action, AttachmentKind};

/// How long `link` waits for signal-cli to print its URI, and then to have been linked.
const LINKING: Duration = Duration::from_secs(120);

/// Prints each signal-cli account's log (`Account::log`), for the record.
pub(super) const LOG_FILES: &str =
    "for f in /signal-cli/*/log.txt; do echo \"== $f\"; cat \"$f\"; done";

/// A signal-cli account of the box's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Account {
    /// Registered as a primary device of its own.
    Cli,
    /// Linked as a secondary device to the presage-cli primary ([`SignalCli::link`]).
    Linked,
}

impl Account {
    /// Where its data is: a directory of its own under the volume, so that each is alone in its
    /// `--config`.
    fn config(self) -> &'static str {
        match self {
            Self::Cli => "/signal-cli/cli",
            Self::Linked => "/signal-cli/linked",
        }
    }

    /// Where its commands' own log goes (`--log-file`), every command's appended: signal-cli
    /// writes to the terminal what it fails at, and the log has the rest (the record keeps it,
    /// [`LOG_FILES`]).
    fn log(self) -> String {
        format!("{}/log.txt", self.config())
    }

    /// The number a registered account is made with; a linked one has its primary's
    /// (`presage.rs`: +15555550101).
    fn phone_number(self) -> &'static str {
        match self {
            Self::Cli => "+15555550110",
            Self::Linked => "+15555550101",
        }
    }

    /// The profile's given name, which the clients show it by.
    pub(super) fn name(self) -> Cow<'static, str> {
        match self {
            Self::Cli => "Cli".into(),
            Self::Linked => "Cli (linked)".into(),
        }
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// What failed driving a signal-cli account in the `signal-cli` container.
#[derive(Debug, thiserror::Error)]
pub(super) enum SignalCliError {
    /// A signal-cli command of the account's: `command` is its subcommand, as "send".
    #[error("signal-cli {command} for {account}")]
    Command {
        account: Account,
        command: &'static str,
        #[source]
        source: PodError,
    },
    /// Starting or reading the process that links ([`SignalCli::link`]).
    #[error("linking {account}")]
    Link {
        account: Account,
        #[source]
        source: PodError,
    },
    /// The primary couldn't add the device `link` printed the URI of.
    #[error("{account}'s primary adding it")]
    AddDevice {
        account: Account,
        #[source]
        source: PresageError,
    },
    /// `link` printed no URI, or the primary didn't have it linked, within [`LINKING`].
    #[error("{account} {stage} within {} s; its output:\n{output}", LINKING.as_secs())]
    LinkTimeout {
        account: Account,
        stage: &'static str,
        output: String,
    },
    /// `receive --output=json` printed a line that isn't JSON.
    #[error("{account}'s receive printed {line:?}")]
    NotJson {
        account: Account,
        line: String,
        #[source]
        source: serde_json::Error,
    },
    /// `send` printed no time for the message.
    #[error("{account}'s send printed no timestamp: {printed:?}")]
    NoTimestamp { account: Account, printed: String },
    /// An attachment read out of the container isn't base64.
    #[error("{account}'s attachment {id} isn't base64")]
    NotBase64 {
        account: Account,
        id: String,
        #[source]
        source: base64::DecodeError,
    },
    /// `listAccounts` has no account with the number.
    #[error("no aci for {account} in {listed}")]
    NoAci { account: Account, listed: String },
    /// A receive that should have got `expected` didn't print it.
    #[error("{account} received no {expected:?}:\n{received}")]
    NotReceived {
        account: Account,
        expected: String,
        received: String,
    },
}

/// What one line of `receive --output=json` says arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Received {
    /// The sender's ACI.
    pub(super) from: Option<String>,
    pub(super) timestamp: u64,
    /// A data message's text, or a sent transcript's (what the account's own other device wrote).
    pub(super) text: Option<String>,
    /// For a group's message, its base64 id.
    pub(super) group: Option<String>,
    /// A receipt's kind (`DELIVERY`, `READ`, `VIEWED`), for a receipt message, and the times of
    /// the messages it is for.
    pub(super) receipt: Option<String>,
    pub(super) receipt_for: Vec<u64>,
    /// What a data message carries, downloaded and decrypted by `receive`.
    pub(super) attachments: Vec<ReceivedAttachment>,
    /// Why signal-cli couldn't handle it: its `exception`'s message.
    pub(super) exception: Option<String>,
}

/// An attachment of a received data message, as `receive` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReceivedAttachment {
    /// Its file's name under the account's `attachments` directory, where signal-cli saved it.
    pub(super) id: Option<String>,
    pub(super) content_type: String,
    pub(super) file_name: Option<String>,
    pub(super) size: Option<u64>,
}

/// What `signal-cli --output=json` writes, as far as the checks read it (signal-cli's
/// `JsonReceiveMessageHandler`, `JsonMessageEnvelope`, `JsonDataMessage`, `JsonAttachment`).
mod json {
    use serde::Deserialize;

    /// A line of `receive`: an envelope, or why one couldn't be handled, or both.
    #[derive(Deserialize)]
    pub(super) struct Line {
        pub(super) envelope: Option<Envelope>,
        pub(super) exception: Option<Exception>,
    }

    #[derive(Deserialize)]
    pub(super) struct Exception {
        pub(super) message: Option<String>,
        #[serde(rename = "type")]
        pub(super) kind: Option<String>,
    }

    #[derive(Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    pub(super) struct Envelope {
        pub(super) source_uuid: Option<String>,
        pub(super) timestamp: u64,
        pub(super) data_message: Option<Data>,
        pub(super) sync_message: Option<Sync>,
        pub(super) receipt_message: Option<Receipt>,
    }

    #[derive(Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    pub(super) struct Data {
        pub(super) message: Option<String>,
        pub(super) group_info: Option<GroupInfo>,
        pub(super) attachments: Vec<Attachment>,
    }

    #[derive(Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    pub(super) struct Sync {
        pub(super) sent_message: Option<Data>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct GroupInfo {
        pub(super) group_id: Option<String>,
    }

    #[derive(Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    pub(super) struct Receipt {
        pub(super) is_delivery: bool,
        pub(super) is_read: bool,
        pub(super) is_viewed: bool,
        /// The times of the messages it is for.
        pub(super) timestamps: Vec<u64>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Attachment {
        pub(super) id: Option<String>,
        pub(super) content_type: Option<String>,
        pub(super) filename: Option<String>,
        pub(super) size: Option<u64>,
    }

    /// An entry of `listAccounts`.
    #[derive(Deserialize)]
    pub(super) struct Account {
        pub(super) number: Option<String>,
        pub(super) aci: Option<String>,
    }

    /// An entry of `listGroups`.
    #[derive(Deserialize)]
    pub(super) struct Group {
        pub(super) id: Option<String>,
        pub(super) name: Option<String>,
    }
}

impl Received {
    /// `line`, as `signal-cli --output=json receive` writes it.
    fn parse(line: &str) -> Result<Self, serde_json::Error> {
        let line: json::Line = serde_json::from_str(line)?;
        let envelope = line.envelope.unwrap_or_default();
        let data = envelope.data_message.unwrap_or_default();
        // What the account's own other device wrote shows as a sent transcript.
        let sent = envelope
            .sync_message
            .and_then(|sync| sync.sent_message)
            .unwrap_or_default();
        let group = |data: &json::Data| data.group_info.as_ref()?.group_id.clone();
        let receipt = envelope.receipt_message;
        Ok(Self {
            from: envelope.source_uuid,
            timestamp: envelope.timestamp,
            group: group(&data).or_else(|| group(&sent)),
            text: data.message.or(sent.message),
            receipt: receipt.as_ref().and_then(|receipt| {
                [
                    (receipt.is_delivery, "DELIVERY"),
                    (receipt.is_read, "READ"),
                    (receipt.is_viewed, "VIEWED"),
                ]
                .into_iter()
                .find_map(|(is, kind)| is.then(|| kind.to_owned()))
            }),
            receipt_for: receipt
                .map(|receipt| receipt.timestamps)
                .unwrap_or_default(),
            attachments: data
                .attachments
                .into_iter()
                .map(|attachment| ReceivedAttachment {
                    id: attachment.id,
                    content_type: attachment.content_type.unwrap_or_default(),
                    file_name: attachment.filename,
                    size: attachment.size,
                })
                .collect(),
            exception: line.exception.map(|exception| {
                format!(
                    "{} ({})",
                    exception.message.unwrap_or_default(),
                    exception.kind.unwrap_or_default()
                )
            }),
        })
    }
}

/// An account of the `signal-cli` service's.
pub(super) struct SignalCli<'pod> {
    pub(super) containers: &'pod Pod,
    account: Account,
    /// Everything `receive` printed so far, which signal-cli itself doesn't keep.
    received: Mutex<Vec<Received>>,
}

impl<'pod> SignalCli<'pod> {
    pub(super) fn new(containers: &'pod Pod, account: Account) -> Self {
        Self {
            containers,
            account,
            received: Mutex::new(Vec::new()),
        }
    }

    /// Runs signal-cli's `command` with `arguments`, for the account; what it printed. `global`
    /// goes before the account: signal-cli's own options (`--output=json`).
    fn cli(
        &self,
        global: &[&str],
        command: &'static str,
        arguments: &[&str],
    ) -> Result<String, SignalCliError> {
        let log = self.account.log();
        let argv = [
            &[
                "signal-cli",
                "--config",
                self.account.config(),
                "--log-file",
                &log,
            ][..],
            global,
            &["-a", self.account.phone_number(), command],
            arguments,
        ]
        .concat();
        self.containers
            .exec(Service::SignalCli, &argv, "")
            .map_err(|source| SignalCliError::Command {
                account: self.account,
                command,
                source,
            })
    }

    /// The account's ACI, from `listAccounts` (`[{"number":…,"aci":…}]`).
    fn aci(&self) -> Result<String, SignalCliError> {
        let log = self.account.log();
        let argv = [
            "signal-cli",
            "--config",
            self.account.config(),
            "--log-file",
            &log,
            "--output=json",
            "listAccounts",
        ];
        let listed = self
            .containers
            .exec(Service::SignalCli, &argv, "")
            .map_err(|source| SignalCliError::Command {
                account: self.account,
                command: "listAccounts",
                source,
            })?;
        serde_json::from_str::<Vec<json::Account>>(&listed)
            .ok()
            .and_then(|accounts| {
                accounts.into_iter().find_map(|account| {
                    (account.number.as_deref() == Some(self.account.phone_number()))
                        .then_some(account.aci)
                        .flatten()
                })
            })
            .ok_or(SignalCliError::NoAci {
                account: self.account,
                listed,
            })
    }

    /// Registers afresh as a primary device, with the account's name as the profile's given name,
    /// and returns its ACI. The test server accepts any verification code and the captcha
    /// `signalcaptcha://noop.noop.registration.noop` (Signal-Server TESTING.md).
    pub(super) fn register(&self) -> Result<String, SignalCliError> {
        self.cli(
            &[],
            "register",
            &["--captcha", testbox_tools::CAPTCHA, "--reregister"],
        )?;
        self.cli(&[], "verify", &[testbox_tools::VERIFICATION_CODE])?;
        self.cli(
            &[],
            "updateProfile",
            &["--given-name", &self.account.name()],
        )?;
        self.aci()
    }

    /// Links as a secondary device to `primary`: `signal-cli link` prints a URI and waits, the
    /// primary adds the device with it, and `link` then ends. Returns the account's ACI, the
    /// primary's.
    pub(super) fn link(&self, primary: &Presage<'_>) -> Result<String, SignalCliError> {
        let out = "/tmp/link.out";
        let config = self.account.config();
        let name = self.account.name();
        // In the background, its output in a file: `podman exec` ends when the command does, and
        // this one waits for the primary.
        let log = self.account.log();
        let start = format!(
            "rm -f {out}; (signal-cli --config {config} --log-file {log} link -n {name:?} > {out} 2>&1 &)"
        );
        let exec = |script: &str| {
            self.containers
                .exec(Service::SignalCli, &["sh", "-c", script], "")
                .map_err(|source| SignalCliError::Link {
                    account: self.account,
                    source,
                })
        };
        exec(&start)?;
        let deadline = Instant::now() + LINKING;
        let waiting = |stage: &'static str, output: String| SignalCliError::LinkTimeout {
            account: self.account,
            stage,
            output,
        };
        let uri = loop {
            let output = exec(&format!("cat {out}"))?;
            if let Some(uri) = output.lines().find(|line| line.starts_with("sgnl://")) {
                break uri.to_owned();
            }
            if Instant::now() > deadline {
                return Err(waiting("printed no link URI", output));
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        primary
            .add_device(&uri)
            .map_err(|source| SignalCliError::AddDevice {
                account: self.account,
                source,
            })?;
        loop {
            let output = exec(&format!("cat {out}"))?;
            if output.contains("Associated with:") {
                return self.aci();
            }
            if Instant::now() > deadline {
                return Err(waiting("wasn't linked", output));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Sends `text` to `recipient_aci`; the time it was sent at, which `send` prints.
    pub(super) fn send(&self, recipient_aci: &str, text: &str) -> Result<u64, SignalCliError> {
        self.sending(&[recipient_aci, "-m", text])
    }

    /// Sends `text` to the group `group_id` (base64: the id `listGroups` and a received group
    /// message print), which this account is a member of.
    pub(super) fn send_to_group(&self, group_id: &str, text: &str) -> Result<u64, SignalCliError> {
        self.sending(&["-g", group_id, "-m", text])
    }

    /// `send` with `arguments`, which end with the attachments, if any: `-a` takes every argument
    /// after it. Prints the time of the message.
    fn sending(&self, arguments: &[&str]) -> Result<u64, SignalCliError> {
        let printed = self.cli(&[], "send", arguments)?;
        printed
            .lines()
            .find_map(|line| line.trim().parse().ok())
            .ok_or(SignalCliError::NoTimestamp {
                account: self.account,
                printed,
            })
    }

    /// Sends `texts` to `recipient_aci` one after another, in one `podman exec`: as fast as
    /// signal-cli sends, each a JVM of its own. Stops at the first that fails.
    pub(super) fn send_all(
        &self,
        recipient_aci: &str,
        texts: &[String],
    ) -> Result<(), SignalCliError> {
        let script = r#"config=$1; number=$2; to=$3; shift 3
for text; do signal-cli --config "$config" --log-file "$config/log.txt" -a "$number" send -m "$text" "$to" || exit 1; done"#;
        let mut argv = vec![
            "sh",
            "-c",
            script,
            "sh",
            self.account.config(),
            self.account.phone_number(),
            recipient_aci,
        ];
        argv.extend(texts.iter().map(String::as_str));
        self.containers
            .exec(Service::SignalCli, &argv, "")
            .map(drop)
            .map_err(|source| SignalCliError::Command {
                account: self.account,
                command: "send",
                source,
            })
    }

    /// Sends `text` with the file at `path`.
    pub(super) fn send_file(
        &self,
        recipient_aci: &str,
        text: &str,
        path: &str,
    ) -> Result<u64, SignalCliError> {
        self.send_files(recipient_aci, text, &[path])
    }

    /// [`Self::send_file`] with each of `paths`, in one message.
    pub(super) fn send_files(
        &self,
        recipient_aci: &str,
        text: &str,
        paths: &[&str],
    ) -> Result<u64, SignalCliError> {
        let mut argv = vec![recipient_aci, "-m", text];
        if !paths.is_empty() {
            argv.push("-a");
            argv.extend(paths);
        }
        self.sending(&argv)
    }

    /// Everything the account has received so far through [`Self::sync`], in the chat with `aci`
    /// (or, for a group message, with the group: its id), one line a message: its time and text.
    /// signal-cli keeps no history, so this is the record of the checks' own receives.
    pub(super) fn messages_with(&self, aci: &str) -> String {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|message| {
                message.from.as_deref() == Some(aci) || message.group.as_deref() == Some(aci)
            })
            .filter_map(|message| {
                Some(format!(
                    "{} @ {}: {}\n",
                    aci,
                    message.timestamp,
                    message.text.as_ref()?
                ))
            })
            .collect()
    }

    /// The attachment `id` as `receive` saved it (decrypted), read out of the container as base64:
    /// the exec's output is text.
    pub(super) fn attachment(&self, id: &str) -> Result<Vec<u8>, SignalCliError> {
        use base64::Engine as _;
        let saved = format!("{}/attachments/{id}", self.account.config());
        let encoded = self
            .containers
            .exec(Service::SignalCli, &["base64", "-w0", &saved], "")
            .map_err(|source| SignalCliError::Command {
                account: self.account,
                command: "base64",
                source,
            })?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|source| SignalCliError::NotBase64 {
                account: self.account,
                id: id.to_owned(),
                source,
            })
    }

    /// Receives what is queued, `timeout` seconds without a message ending it (signal-cli's
    /// `receive --timeout`, which has no "until the queue is empty"); what it printed, parsed.
    /// Each message it failed to handle is noted as lost, for the check every phase ends with.
    pub(super) fn sync_for(&self, timeout: u32) -> Result<Vec<Received>, SignalCliError> {
        let timeout = timeout.to_string();
        let output = self.cli(&["--output=json"], "receive", &["--timeout", &timeout])?;
        let mut printed = Vec::new();
        for line in output.lines().filter(|line| !line.trim().is_empty()) {
            let message = Received::parse(line).map_err(|source| SignalCliError::NotJson {
                account: self.account,
                line: line.to_owned(),
                source,
            })?;
            if let Some(exception) = &message.exception {
                self.containers.note_lost(format!(
                    "signal-cli ({}): {exception} (envelope {} from {:?})",
                    self.account, message.timestamp, message.from
                ));
            }
            if message.text.is_some() {
                self.containers
                    .note_printed(&self.account.name(), message.timestamp);
            }
            printed.push(message);
        }
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(printed.iter().cloned());
        Ok(printed)
    }

    /// [`Self::sync_for`] a few seconds.
    pub(super) fn sync(&self) -> Result<Vec<Received>, SignalCliError> {
        self.sync_for(5)
    }

    /// Receives what is queued; fails unless a message with `expected` as its text arrived,
    /// which it returns.
    pub(super) fn sync_printing(&self, expected: &str) -> Result<Received, SignalCliError> {
        let received = self.sync()?;
        received
            .iter()
            .find(|message| message.text.as_deref() == Some(expected))
            .cloned()
            .ok_or_else(|| SignalCliError::NotReceived {
                account: self.account,
                expected: expected.to_owned(),
                received: format!("{received:#?}"),
            })
    }

    /// The groups the account is in: each one's base64 id and name (`listGroups`).
    pub(super) fn groups(&self) -> Result<Vec<(String, String)>, SignalCliError> {
        let listed = self.cli(&["--output=json"], "listGroups", &[])?;
        Ok(serde_json::from_str::<Vec<json::Group>>(&listed)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|group| Some((group.id?, group.name.unwrap_or_default())))
            .collect())
    }
}

/// Receives until a message `wanted` says is the one arrives, for [`PATIENCE`]; it, or what was
/// received instead.
fn await_received(
    cli: &SignalCli<'_>,
    expected: &str,
    wanted: impl Fn(&Received) -> bool,
) -> Result<Received, Failure> {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        let received = cli.sync()?;
        if let Some(found) = received.iter().find(|message| wanted(message)) {
            return Ok(found.clone());
        }
        seen.extend(received);
        if Instant::now() > deadline {
            return Err(SignalCliError::NotReceived {
                account: cli.account,
                expected: expected.to_owned(),
                received: format!("{seen:#?}"),
            }
            .into());
        }
    }
}

/// Whether two encoded pictures show the same: the same size and pixels.
fn same_pixels(one: &[u8], other: &[u8]) -> bool {
    match (image::load_from_memory(one), image::load_from_memory(other)) {
        (Ok(one), Ok(other)) => one.to_rgba8() == other.to_rgba8(),
        _ => false,
    }
}

/// What the signal-cli checks find wrong of their own.
#[derive(Debug, thiserror::Error)]
enum InteropError {
    /// A message that should have come in a group came without its id.
    #[error("{text:?} arrived without a group id: {message:?}")]
    NoGroup { text: String, message: Received },
    /// A conversation id that isn't a group's.
    #[error("{id:?} is no group")]
    NotAGroup { id: String },
    /// signal-cli doesn't list the group it was told of.
    #[error("signal-cli lists no group {id:?}: {listed:?}")]
    NotListed {
        id: String,
        listed: Vec<(String, String)>,
    },
    /// A text of the group that signal-cli's own record of what it received lacks.
    #[error("{text:?} isn't in what signal-cli received in the group: {kept:?}")]
    NotKept { text: String, kept: String },
    /// An attachment `receive` printed without the id of the file it saved.
    #[error("no attachment id in {message:?}")]
    NoAttachmentId { message: Received },
    /// What signal-cli saved of an attachment isn't the picture that was sent.
    #[error("attachment {id} isn't the picture sent")]
    Differs { id: String },
    #[error("reading {}", path.display())]
    ReadPicture {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// b33p shows the texts signal-cli sent in a row in another order.
    #[error("b33p shows the burst as {seen:?}")]
    OutOfOrder { seen: Vec<String> },
    /// Linked to a primary, signal-cli is the primary's account, with its ACI.
    #[error("linked as {linked}, not as the primary {primary}")]
    OtherAccount { linked: String, primary: String },
}

/// signal-cli with b33p and presage-cli: text both ways, receipts, a group b33p makes and both
/// write in, and signal-cli as a linked device of the primary b33p is linked to.
pub(super) fn signal_cli(
    checks: &mut Checks,
    fixture: &mut InteropBox<'_>,
    containers: &Pod,
    files: &Path,
    picture: &Path,
) -> Result<(), Failure> {
    let InteropBox {
        primary,
        peer,
        primary_aci,
        peer_aci,
        b33p,
        cli,
        cli_aci,
        cli_chat,
        peer_chat,
    } = fixture;

    let text = "testbox signal-cli: from b33p to signal-cli";
    checks.record("text from b33p reaches signal-cli", || {
        select(b33p, cli_chat)?;
        type_and_send(b33p, text)?;
        cli.sync_printing(text)?;
        Ok(())
    });

    // signal-cli sends delivery receipts itself, as a data message arrives (its `receive`);
    // what comes back for its own text is b33p's.
    let to_be_receipted = "testbox signal-cli: to be receipted by b33p";
    checks.record("b33p sends signal-cli a delivery receipt", || {
        let sent = cli.send(primary_aci, to_be_receipted)?;
        chat_previewing(b33p, to_be_receipted)?;
        await_received(cli, "a delivery receipt from b33p's account", |message| {
            message.receipt.as_deref() == Some("DELIVERY")
                && message.receipt_for.contains(&sent)
                && message.from.as_deref() == Some(primary_aci.as_str())
        })?;
        Ok(())
    });

    // Neither is b33p: presage-cli's side is presage, signal-cli's is not.
    let (to_cli, to_peer) = (
        "testbox signal-cli: from the peer to signal-cli",
        "testbox signal-cli: from signal-cli to the peer",
    );
    checks.record(
        "text goes both ways between signal-cli and a presage-cli account",
        || {
            peer.send(cli_aci, to_cli)?;
            cli.sync_printing(to_cli)?;
            cli.send(peer_aci, to_peer)?;
            peer.sync_printing(to_peer)?;
            Ok(())
        },
    );

    // Through CDN3 both ways: signal-cli's own upload and download, and the bytes compared.
    stage_picture(picture, files)?;
    let in_box = "/files/picture.png";
    let picture_to_cli = "testbox signal-cli: a picture from b33p";
    checks.record("a picture from b33p reaches signal-cli, intact", || {
        select(b33p, cli_chat)?;
        b33p.act(Action::StageFiles {
            paths: vec![in_box.to_owned()],
        })?;
        type_and_send(b33p, picture_to_cli)?;
        let message = await_received(cli, picture_to_cli, |message| {
            message.text.as_deref() == Some(picture_to_cli) && !message.attachments.is_empty()
        })?;
        let id = message.attachments[0]
            .id
            .clone()
            .ok_or_else(|| InteropError::NoAttachmentId {
                message: message.clone(),
            })?;
        // Not byte for byte: b33p sends a picture without its metadata (b33p-core
        // `without_metadata`), re-encoded. The pixels are the picture's.
        let received = cli.attachment(&id)?;
        let sent = std::fs::read(picture).map_err(|source| InteropError::ReadPicture {
            path: picture.to_owned(),
            source,
        })?;
        if !same_pixels(&received, &sent) {
            return Err(InteropError::Differs { id }.into());
        }
        Ok(())
    });
    let picture_from_cli = "testbox signal-cli: a picture from signal-cli";
    checks.record(
        "a picture from signal-cli reaches b33p, downloaded and decrypted",
        || {
            select(b33p, cli_chat)?;
            cli.send_file(primary_aci, picture_from_cli, in_box)?;
            let id = incoming_attachment(b33p, Some(picture_from_cli), AttachmentKind::Image)?;
            b33p.act(Action::FetchAttachment { id: id.clone() })?;
            Ok(downloaded(b33p, &id)?)
        },
    );

    // signal-cli sends each as a JVM of its own, a second or two apiece: the order they were
    // sent in is the order b33p shows them in.
    let burst: Vec<String> = (0..10)
        .map(|n| format!("testbox signal-cli: burst {n}"))
        .collect();
    checks.record(
        "ten texts from signal-cli in a row reach b33p, in order",
        || {
            select(b33p, cli_chat)?;
            cli.send_all(primary_aci, &burst)?;
            let shown = |snapshot: &b33p_harness::Snapshot| -> Vec<String> {
                snapshot
                    .messages
                    .iter()
                    .filter(|message| !message.outgoing && burst.contains(&message.text))
                    .map(|message| message.text.clone())
                    .collect()
            };
            let seen = b33p.wait_for("all ten in b33p", PATIENCE, |snapshot| {
                let seen = shown(snapshot);
                (seen.len() >= burst.len()).then_some(seen)
            })?;
            if seen != burst {
                return Err(InteropError::OutOfOrder { seen }.into());
            }
            Ok(())
        },
    );

    // The group: b33p makes it of signal-cli and the peer, so that signal-cli learns of it from
    // the group's message, and hears what each of the others writes in it.
    let title = "testbox signal-cli group";
    let hello = "testbox signal-cli: hello, group";
    let group = checks.require("b33p makes a group of signal-cli and the peer", || {
        b33p.act(Action::CreateGroup {
            title: title.to_owned(),
            members: vec![cli_chat.clone(), peer_chat.clone()],
        })?;
        Ok(b33p.wait_for("the group in b33p", PATIENCE, |snapshot| {
            snapshot
                .conversations
                .iter()
                .find(|chat| chat.group && chat.title == title && !chat.request)
                .map(|chat| chat.id.clone())
        })?)
    })?;
    let group_key = b33p_core::ConversationId(group.clone())
        .group_key()
        .ok_or_else(|| InteropError::NotAGroup { id: group.clone() })?
        .to_owned();
    let group_id = checks.require("text b33p sends to the group reaches signal-cli", || {
        select(b33p, &group)?;
        type_and_send(b33p, hello)?;
        let message = await_received(cli, hello, |message| message.text.as_deref() == Some(hello))?;
        let id = message.group.clone().ok_or_else(|| InteropError::NoGroup {
            text: hello.to_owned(),
            message,
        })?;
        let listed = cli.groups()?;
        if !listed.iter().any(|(listed_id, _)| *listed_id == id) {
            return Err(InteropError::NotListed { id, listed }.into());
        }
        Ok(id)
    })?;
    // presage-cli knows the group from b33p's greeting.
    let from_cli = "testbox signal-cli: signal-cli writes to the group";
    checks.record(
        "text signal-cli sends to the group reaches b33p and the peer",
        || {
            peer.sync_printing(hello)?;
            cli.send_to_group(&group_id, from_cli)?;
            b33p.wait_for("signal-cli's text in the group", PATIENCE, |snapshot| {
                snapshot
                    .conversations
                    .iter()
                    .any(|chat| chat.id == group && chat.preview.contains(from_cli))
                    .then_some(())
            })?;
            peer.sync_printing(from_cli)?;
            Ok(())
        },
    );
    let from_peer = "testbox signal-cli: the peer writes to the group";
    checks.record(
        "text the peer sends to the group reaches signal-cli",
        || {
            peer.send_to_group(&group_key, from_peer)?;
            let message = await_received(cli, from_peer, |message| {
                message.text.as_deref() == Some(from_peer)
            })?;
            if message.group.is_none() {
                return Err(InteropError::NoGroup {
                    text: from_peer.to_owned(),
                    message,
                }
                .into());
            }
            Ok(())
        },
    );

    checks.record(
        "signal-cli's record of the group holds what was written in it",
        || {
            let kept = cli.messages_with(&group_id);
            for text in [hello, from_peer] {
                if !kept.contains(text) {
                    return Err(InteropError::NotKept {
                        text: text.to_owned(),
                        kept,
                    }
                    .into());
                }
            }
            Ok(())
        },
    );

    // signal-cli as a linked device: of the account b33p is linked to, which then has two.
    let linked = SignalCli::new(containers, Account::Linked);
    checks.record("signal-cli links to the primary b33p is linked to", || {
        let aci = linked.link(primary)?;
        if aci != *primary_aci {
            return Err(InteropError::OtherAccount {
                linked: aci,
                primary: primary_aci.clone(),
            }
            .into());
        }
        Ok(())
    });
    let to_both = "testbox signal-cli: to the primary, for both devices";
    checks.record(
        "a text to the primary reaches b33p and linked signal-cli",
        || {
            peer.send(primary_aci, to_both)?;
            chat_previewing(b33p, to_both)?;
            linked.sync_printing(to_both)?;
            Ok(())
        },
    );
    let from_linked = "testbox signal-cli: written on the linked signal-cli";
    checks.record(
        "what linked signal-cli sends reaches the peer and b33p",
        || {
            linked.send(peer_aci, from_linked)?;
            peer.sync_printing(from_linked)?;
            // b33p, the account's other device, lists it as sent, from the sync transcript.
            chat_previewing(b33p, &format!("You: {from_linked}"))?;
            Ok(())
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_receive_printed_is_read() {
        let text = r#"{"account":"+15555550110","envelope":{"sourceUuid":"0ab6a4b0","timestamp":1794005612019,"dataMessage":{"message":"hello","groupInfo":{"groupId":"YWJj"}}}}"#;
        assert_eq!(
            Received::parse(text).unwrap(),
            Received {
                from: Some("0ab6a4b0".to_owned()),
                timestamp: 1794005612019,
                text: Some("hello".to_owned()),
                group: Some("YWJj".to_owned()),
                receipt: None,
                receipt_for: Vec::new(),
                exception: None,
                attachments: Vec::new(),
            }
        );
    }

    #[test]
    fn an_attachment_is_read() {
        let text = r#"{"envelope":{"timestamp":6,"dataMessage":{"message":"a picture","attachments":[{"contentType":"image/png","filename":"picture.png","id":"abc","size":12}]}}}"#;
        assert_eq!(
            Received::parse(text).unwrap().attachments,
            vec![ReceivedAttachment {
                id: Some("abc".to_owned()),
                content_type: "image/png".to_owned(),
                file_name: Some("picture.png".to_owned()),
                size: Some(12),
            }]
        );
    }

    #[test]
    fn a_receipt_is_read() {
        let text = r#"{"envelope":{"sourceUuid":"x","timestamp":5,"receiptMessage":{"when":5,"isDelivery":true,"isRead":false,"isViewed":false,"timestamps":[3,4]}}}"#;
        let read = Received::parse(text).unwrap();
        assert_eq!(read.receipt.as_deref(), Some("DELIVERY"));
        assert_eq!(read.receipt_for, vec![3, 4]);
        let text = r#"{"envelope":{"timestamp":5,"receiptMessage":{"isDelivery":false,"isRead":true,"isViewed":false,"timestamps":[]}}}"#;
        assert_eq!(
            Received::parse(text).unwrap().receipt.as_deref(),
            Some("READ")
        );
    }

    #[test]
    fn what_the_accounts_own_device_wrote_is_read_as_sent() {
        let text = r#"{"envelope":{"timestamp":7,"syncMessage":{"sentMessage":{"message":"mine","groupInfo":{"groupId":"Zw=="}}}}}"#;
        let read = Received::parse(text).unwrap();
        assert_eq!(read.text.as_deref(), Some("mine"));
        assert_eq!(read.group.as_deref(), Some("Zw=="));
    }

    #[test]
    fn a_failure_is_read() {
        let text = r#"{"exception":{"message":"No session","type":"ProtocolNoSessionException"}}"#;
        assert_eq!(
            Received::parse(text).unwrap().exception.as_deref(),
            Some("No session (ProtocolNoSessionException)")
        );
    }
}
