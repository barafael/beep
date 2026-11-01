use std::borrow::Cow;
use std::fmt;

use crate::pod::{Pod, PodError, Service};
use testbox_tools::keys::KeyFile;

/// What presage-cli prints for a message it failed to decrypt (presage-cli/src/main.rs).
const LOST: &str = "failed to decrypt a message";

/// A presage-cli account of the box's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Account {
    /// The account b33p links to.
    Primary,
    /// The account b33p writes to, and Desktop links to.
    Peer,
    /// A group's member besides the peer (`groups`).
    Member,
    /// The `n`th of the large group's members (`large-group`).
    Crowd(u8),
    /// The account the `n`th b33p of a mesh links to (`tour`).
    Mesh(u8),
}

impl Account {
    fn db(self) -> Cow<'static, str> {
        match self {
            Self::Primary => "/presage/primary.db".into(),
            Self::Peer => "/presage/peer.db".into(),
            Self::Member => "/presage/member.db".into(),
            Self::Crowd(n) => format!("/presage/crowd-{n}.db").into(),
            Self::Mesh(n) => format!("/presage/mesh-{n}.db").into(),
        }
    }

    fn phone_number(self) -> Cow<'static, str> {
        match self {
            Self::Primary => "+15555550101".into(),
            Self::Peer => "+15555550102".into(),
            Self::Member => "+15555550104".into(),
            Self::Crowd(n) => format!("+1555555{:04}", 300 + u16::from(n)).into(),
            Self::Mesh(n) => format!("+1555555{:04}", 200 + u16::from(n)).into(),
        }
    }

    /// The profile's given name, which the clients show it by.
    pub(super) fn name(self) -> Cow<'static, str> {
        match self {
            Self::Primary => "Primary".into(),
            Self::Peer => "Peer".into(),
            Self::Member => "Member".into(),
            Self::Crowd(n) => format!("Crowd {n}").into(),
            Self::Mesh(n) => format!("Mesh {n}").into(),
        }
    }
}

/// The account by its name, as the clients show it.
impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// What failed driving a presage-cli account in the `presage` container.
#[derive(Debug, thiserror::Error)]
pub(super) enum PresageError {
    /// A presage-cli command of the account's: `command` is its subcommand, as "send".
    #[error("presage-cli {command} for {account}")]
    Command {
        account: Account,
        command: &'static str,
        #[source]
        source: PodError,
    },
    /// [`Presage::send_all`]'s `count` sends, in one `podman exec`.
    #[error("{account} sending {count} messages with presage-cli")]
    SendAll {
        account: Account,
        count: usize,
        #[source]
        source: PodError,
    },
    #[error("seeding the sticker pack for {account}")]
    SeedStickers {
        account: Account,
        #[source]
        source: PodError,
    },
    /// presage-cli's `whoami` printed no ACI where it prints one.
    #[error("no aci in {account}'s whoami: {whoami}")]
    NoAci { account: Account, whoami: String },
    /// A sync that should have received `expected` didn't print it.
    #[error("{account}'s sync printed no {expected:?}:\n{received}")]
    NotPrinted {
        account: Account,
        expected: String,
        received: String,
    },
}

/// An account of the `presage` service's.
pub(super) struct Presage<'pod> {
    pub(super) containers: &'pod Pod,
    account: Account,
}

impl<'pod> Presage<'pod> {
    pub(super) fn new(containers: &'pod Pod, account: Account) -> Self {
        Self {
            containers,
            account,
        }
    }

    /// The account's name, as the clients show it.
    pub(super) fn name(&self) -> Cow<'static, str> {
        self.account.name()
    }

    /// Runs presage-cli's `command` with `arguments` on the account's store; what it printed.
    fn cli(
        &self,
        command: &'static str,
        arguments: &[&str],
        stdin: &str,
    ) -> Result<String, PresageError> {
        let db = self.account.db();
        let argv = [
            &["presage-cli", "--sqlite-db-path", &db, command][..],
            arguments,
        ]
        .concat();
        self.containers
            .exec(Service::Presage, &argv, stdin)
            .map_err(|source| PresageError::Command {
                account: self.account,
                command,
                source,
            })
    }

    /// `output`, after noting each message it says presage-cli lost (its
    /// `Received::DecryptionError`), for the check every phase ends with. Not only `sync`
    /// receives: a `send` does so in the background while it sends, and exits wherever that
    /// got to. One that is stored is no loss but a redelivery: presage-cli exited after storing
    /// it and before acknowledging it.
    fn noting_lost(&self, output: String) -> String {
        for line in output.lines().filter(|line| line.contains(LOST)) {
            if self.stored(line) {
                continue;
            }
            let account = self.account.name();
            self.containers
                .note_lost(format!("presage-cli ({account}): {}", line.trim()));
        }
        output
    }

    /// Whether the message that `line` (presage-cli's "failed to decrypt a message from
    /// <ACI:…>.2 of 1793028555172: …") says it failed to decrypt is in its store: in the
    /// sender's thread, or, for a sent transcript from the account's own other device (Desktop
    /// linked to the peer), in the thread of whichever other account of the box it went to.
    fn stored(&self, line: &str) -> bool {
        let Some((aci, timestamp)) = line
            .split_once("<ACI:")
            .and_then(|(_, rest)| rest.split_once('>'))
            .and_then(|(aci, rest)| Some((aci, rest.split_once(" of ")?.1.split_once(':')?.0)))
        else {
            return false;
        };
        let listed = |thread: &str| {
            self.cli("list-messages", &["-u", thread, "--from", timestamp], "")
                .is_ok_and(|listed| listed.contains(&format!(" @ {timestamp}: ")))
        };
        if listed(aci) {
            return true;
        }
        if !self.aci().is_ok_and(|me| me == aci) {
            return false;
        }
        [Account::Primary, Account::Peer, Account::Member]
            .into_iter()
            .filter(|&other| other != self.account)
            .filter_map(|other| Self::new(self.containers, other).aci().ok())
            .any(|thread| listed(&thread))
    }

    /// The account's ACI, from presage-cli's `whoami` (`WhoAmIResponse { aci: <uuid>, ... }`).
    fn aci(&self) -> Result<String, PresageError> {
        let whoami = self.cli("whoami", &[], "")?;
        whoami
            .split_once("aci: ")
            .and_then(|(_, rest)| rest.get(..36))
            .map(str::to_owned)
            .ok_or_else(|| PresageError::NoAci {
                account: self.account,
                whoami: whoami.clone(),
            })
    }

    /// Registers afresh, with the account's name as the profile's given name, and returns its ACI.
    /// The test server accepts any verification code and the captcha
    /// `noop.noop.registration.noop` (Signal-Server TESTING.md). A name, since Desktop won't
    /// write a profile without one ("writeProfile: Cannot set an empty profile name").
    pub(super) fn register(&self) -> Result<String, PresageError> {
        let servers = KeyFile::Servers.path();
        let servers = servers.to_string_lossy();
        let register = [
            "--force",
            // The box's servers, from the keys (testbox/tools `provision`).
            "--servers-file",
            &servers,
            "--phone-number",
            &self.account.phone_number(),
            "--captcha",
            testbox_tools::CAPTCHA,
        ];
        // presage-cli reads the verification code from stdin.
        self.cli(
            "register",
            &register,
            &format!("{}\n", testbox_tools::VERIFICATION_CODE),
        )?;
        self.cli(
            "update-profile",
            &["--given-name", &self.account.name()],
            "",
        )?;
        self.aci()
    }

    pub(super) fn add_device(&self, url: &str) -> Result<(), PresageError> {
        self.cli("add-device", &["--url", url], "").map(drop)
    }

    /// Seeds the box's sticker pack and installs it on this account (testbox/stickers); its id.
    pub(super) fn seed_stickers(&self) -> Result<String, PresageError> {
        self.containers
            .exec(
                Service::Presage,
                &["testbox-stickers", &self.account.db()],
                "",
            )
            .map(|id| id.trim().to_owned())
            .map_err(|source| PresageError::SeedStickers {
                account: self.account,
                source,
            })
    }

    pub(super) fn send(&self, recipient_aci: &str, text: &str) -> Result<(), PresageError> {
        self.cli("send", &["--uuid", recipient_aci, "--message", text], "")
            .map(drop)
    }

    /// Sends `text` to the group whose master key is `master_key` (hex), which this account
    /// knows from a message it received in it.
    pub(super) fn send_to_group(&self, master_key: &str, text: &str) -> Result<(), PresageError> {
        self.cli(
            "send-to-group",
            &["--master-key", master_key, "--message", text],
            "",
        )
        .map(drop)
    }

    /// Sends `texts` to `recipient_aci` one after another, in one `podman exec`: as fast as
    /// presage-cli sends. Stops at the first that fails.
    pub(super) fn send_all(
        &self,
        recipient_aci: &str,
        texts: &[String],
    ) -> Result<(), PresageError> {
        let script = r#"db=$1; to=$2; shift 2
for text; do presage-cli --sqlite-db-path "$db" send --uuid "$to" --message "$text" || exit 1; done"#;
        let db = self.account.db();
        let mut argv = vec!["sh", "-c", script, "sh", &db, recipient_aci];
        argv.extend(texts.iter().map(String::as_str));
        self.containers
            .exec(Service::Presage, &argv, "")
            .map(|output| drop(self.noting_lost(output)))
            .map_err(|source| PresageError::SendAll {
                account: self.account,
                count: texts.len(),
                source,
            })
    }

    /// Sends `text` with the file at `path`, as presage-cli names it: by its file name, its
    /// type guessed from the extension.
    pub(super) fn send_file(
        &self,
        recipient_aci: &str,
        text: &str,
        path: &str,
    ) -> Result<(), PresageError> {
        self.send_files(recipient_aci, text, &[path])
    }

    /// [`Self::send_file`] with each of `paths`, in one message.
    pub(super) fn send_files(
        &self,
        recipient_aci: &str,
        text: &str,
        paths: &[&str],
    ) -> Result<(), PresageError> {
        let mut argv = vec!["--uuid", recipient_aci, "--message", text];
        for path in paths {
            argv.extend(["--attach", path]);
        }
        self.cli("send", &argv, "").map(drop)
    }

    /// Receives what is queued, answering what asks for an answer; what presage-cli printed.
    /// What presage-cli's store holds of its chat with `aci`: one line per message, ending in
    /// " / " and its text.
    pub(super) fn messages_with(&self, aci: &str) -> Result<String, PresageError> {
        self.cli("list-messages", &["-u", aci], "")
    }

    pub(super) fn sync(&self) -> Result<String, PresageError> {
        self.cli("sync", &["--stop-after-empty-queue"], "")
            .map(|received| self.noting_lost(received))
    }

    /// Receives what is queued; fails unless presage-cli printed `expected`.
    pub(super) fn sync_printing(&self, expected: &str) -> Result<(), PresageError> {
        let received = self.sync()?;
        if received.contains(expected) {
            Ok(())
        } else {
            Err(PresageError::NotPrinted {
                account: self.account,
                expected: expected.to_owned(),
                received,
            })
        }
    }
}
