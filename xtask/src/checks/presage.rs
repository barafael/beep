use crate::error::{Error, Result};
use crate::pod::{Pod, Service};
use testbox_tools::keys::KeyFile;

/// What presage-cli prints for a message it failed to decrypt (presage-cli/src/main.rs).
const LOST: &str = "failed to decrypt a message";

/// A presage-cli account of the box's.
#[derive(Clone, Copy)]
pub(super) enum Account {
    /// The account b33p links to.
    Primary,
    /// The account b33p writes to, and Desktop links to.
    Peer,
    /// A group's member besides the peer (`groups`).
    Member,
}

impl Account {
    fn db(self) -> &'static str {
        match self {
            Self::Primary => "/presage/primary.db",
            Self::Peer => "/presage/peer.db",
            Self::Member => "/presage/member.db",
        }
    }

    fn phone_number(self) -> &'static str {
        match self {
            Self::Primary => "+15555550101",
            Self::Peer => "+15555550102",
            Self::Member => "+15555550104",
        }
    }

    /// The profile's given name, which the clients show it by.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Primary => "Primary",
            Self::Peer => "Peer",
            Self::Member => "Member",
        }
    }
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

    fn cli(&self, arguments: &[&str], stdin: &str) -> Result<String> {
        let db = self.account.db();
        let argv = [&["presage-cli", "--sqlite-db-path", db][..], arguments].concat();
        self.containers.exec(Service::Presage, &argv, stdin)
    }

    /// `output`, after noting each message it says presage-cli lost (its
    /// `Received::DecryptionError`), for the check every phase ends with. Not only `sync`
    /// receives: a `send` does so in the background while it sends, and exits wherever that
    /// got to.
    fn noting_lost(&self, output: String) -> String {
        for line in output.lines().filter(|line| line.contains(LOST)) {
            let account = self.account.name();
            self.containers
                .note_lost(format!("presage-cli ({account}): {}", line.trim()));
        }
        output
    }

    /// Registers afresh, with the account's name as the profile's given name, and returns its ACI.
    /// The test server accepts any verification code and the captcha
    /// `noop.noop.registration.noop` (Signal-Server TESTING.md). A name, since Desktop won't
    /// write a profile without one ("writeProfile: Cannot set an empty profile name").
    pub(super) fn register(&self) -> Result<String> {
        let servers = KeyFile::Servers.path();
        let servers = servers.to_string_lossy();
        let register = [
            "register",
            "--force",
            // The box's servers, from the keys (testbox/tools `provision`).
            "--servers-file",
            &servers,
            "--phone-number",
            self.account.phone_number(),
            "--captcha",
            testbox_tools::CAPTCHA,
        ];
        // presage-cli reads the verification code from stdin.
        self.cli(
            &register,
            &format!("{}\n", testbox_tools::VERIFICATION_CODE),
        )?;
        self.cli(&["update-profile", "--given-name", self.account.name()], "")?;
        let whoami = self.cli(&["whoami"], "")?;
        // presage-cli prints `WhoAmIResponse { aci: <uuid>, ... }`.
        whoami
            .split_once("aci: ")
            .and_then(|(_, rest)| rest.get(..36))
            .map(str::to_owned)
            .ok_or_else(|| Error::Failed(format!("no aci in whoami: {whoami}")))
    }

    pub(super) fn add_device(&self, url: &str) -> Result<()> {
        self.cli(&["add-device", "--url", url], "").map(drop)
    }

    /// Seeds the box's sticker pack and installs it on this account (testbox/stickers); its id.
    pub(super) fn seed_stickers(&self) -> Result<String> {
        self.containers
            .exec(
                Service::Presage,
                &["testbox-stickers", self.account.db()],
                "",
            )
            .map(|id| id.trim().to_owned())
    }

    pub(super) fn send(&self, recipient_aci: &str, text: &str) -> Result<()> {
        self.cli(&["send", "--uuid", recipient_aci, "--message", text], "")
            .map(drop)
    }

    /// Sends `texts` to `recipient_aci` one after another, in one `podman exec`: as fast as
    /// presage-cli sends. Stops at the first that fails.
    pub(super) fn send_all(&self, recipient_aci: &str, texts: &[String]) -> Result<()> {
        let script = r#"db=$1; to=$2; shift 2
for text; do presage-cli --sqlite-db-path "$db" send --uuid "$to" --message "$text" || exit 1; done"#;
        let mut argv = vec!["sh", "-c", script, "sh", self.account.db(), recipient_aci];
        argv.extend(texts.iter().map(String::as_str));
        self.containers
            .exec(Service::Presage, &argv, "")
            .map(|output| drop(self.noting_lost(output)))
    }

    /// Sends `text` with the file at `path`, as presage-cli names it: by its file name, its
    /// type guessed from the extension.
    pub(super) fn send_file(&self, recipient_aci: &str, text: &str, path: &str) -> Result<()> {
        self.send_files(recipient_aci, text, &[path])
    }

    /// [`Self::send_file`] with each of `paths`, in one message.
    pub(super) fn send_files(&self, recipient_aci: &str, text: &str, paths: &[&str]) -> Result<()> {
        let mut argv = vec!["send", "--uuid", recipient_aci, "--message", text];
        for path in paths {
            argv.extend(["--attach", path]);
        }
        self.cli(&argv, "").map(drop)
    }

    /// Receives what is queued, answering what asks for an answer; what presage-cli printed.
    pub(super) fn sync(&self) -> Result<String> {
        self.cli(&["sync", "--stop-after-empty-queue"], "")
            .map(|received| self.noting_lost(received))
    }

    /// Receives what is queued; fails unless presage-cli printed `expected`.
    pub(super) fn sync_printing(&self, expected: &str) -> Result<()> {
        let received = self.sync()?;
        if received.contains(expected) {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "the peer's sync printed no {expected:?}:\n{received}"
            )))
        }
    }
}
