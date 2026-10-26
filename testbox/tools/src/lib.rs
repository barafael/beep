//! What the testbox's programs share: failures typed, with what was being done; running commands
//! ([`process`]); the drivers' protocol ([`drivers`]) and the keys ([`keys`]), which the xtask uses
//! too.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::Path;

use derive_more::{Display, Error};

pub mod agent;
#[cfg(feature = "android")]
pub mod avd;
pub mod drivers;
pub mod keys;
pub mod process;
#[cfg(feature = "android")]
pub mod verification;

/// What `provision` logs once the server serves with its remote config written: the box's
/// server is ready for every client then.
pub const SERVER_READY: &str = "provision: the server serves, its remote config written";

/// What Jetty logs once a Dropwizard service serves: Signal-Server's, storage-service's.
pub const JETTY_STARTED: &str = "org.eclipse.jetty.server.Server: Started";

/// The captcha Signal-Server's test mode accepts, and the verification code: any does
/// (Signal-Server TESTING.md:23, :24).
pub const CAPTCHA: &str = "signalcaptcha://noop.noop.registration.noop";
pub const VERIFICATION_CODE: &str = "123456";

/// The display's size (`<width>x<height>`) as `with-display` tells its program.
pub const DISPLAY_SIZE: &str = "WITH_DISPLAY_SIZE";

/// How many stickers testbox/stickers seeds.
pub const STICKERS: u32 = 2;

/// The line the `cdn0` tool logs for each request it forwards, which the checks wait on.
pub fn cdn0_request_line(method: &str, path: &str, status: u16) -> String {
    format!("cdn0: {method} {path} {status}")
}

/// What the `storage` tool logs once storage-service serves on its tables.
pub const STORAGE_READY: &str = "storage: storage-service serves";

#[derive(Debug, Display, Error)]
pub enum Error {
    /// I/O while doing `what`.
    #[display("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    /// JSON that didn't parse or serialize: a line of a harness or driver, a file of the keys.
    #[display("{what}: {source}")]
    Json {
        what: String,
        source: serde_json::Error,
    },
    /// Another format's failure, or a client's; their error types differ, so their message is
    /// what is kept.
    #[display("{what}: {source}")]
    Format {
        what: String,
        #[error(not(source))]
        source: String,
    },
    /// An environment variable the program needs, unset or not Unicode.
    #[display("{name}: {source}")]
    Env {
        name: String,
        source: std::env::VarError,
    },
    /// A command that failed, with what it printed.
    #[display("{command} failed{output}")]
    Command {
        command: String,
        #[error(not(source))]
        output: String,
    },
    /// A check others depend on failed, and was reported as it failed (the xtask's
    /// `Checks::require`).
    #[display("{_0} failed")]
    Unmet(#[error(not(source))] String),
    /// The rest: a refused request, a wait that timed out, a state that isn't as expected.
    #[display("{_0}")]
    Failed(#[error(not(source))] String),
}

impl From<&str> for Error {
    fn from(failure: &str) -> Self {
        Self::Failed(failure.to_owned())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Says what was being done when an I/O or JSON error came up.
pub trait Context<T> {
    fn context(self, what: impl Into<String>) -> Result<T>;
}

impl<T> Context<T> for std::io::Result<T> {
    fn context(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Io {
            what: what.into(),
            source,
        })
    }
}

impl<T> Context<T> for serde_json::Result<T> {
    fn context(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Json {
            what: what.into(),
            source,
        })
    }
}

/// [`Context`] with the description built only on failure.
pub trait WithContext<T> {
    fn with_context(self, what: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E> WithContext<T> for std::result::Result<T, E>
where
    std::result::Result<T, E>: Context<T>,
{
    fn with_context(self, what: impl FnOnce() -> String) -> Result<T> {
        self.or_else(|error| Err(error).context(what()))
    }
}

/// Says what was being done when another parser, a serializer or a client failed, with the
/// error's causes after it (a client's error often names only the step: "error sending
/// request").
pub trait Describe<T> {
    fn describe(self, what: impl Into<String>) -> Result<T>;
}

impl<T, E: std::error::Error> Describe<T> for std::result::Result<T, E> {
    fn describe(self, what: impl Into<String>) -> Result<T> {
        self.map_err(|error| Error::Format {
            what: what.into(),
            source: std::iter::successors(error.source(), |cause| cause.source())
                .fold(error.to_string(), |message, cause| {
                    format!("{message}: {cause}")
                }),
        })
    }
}

/// A tool's exit: its failure on stderr, under its name.
pub fn exit(tool: &str, outcome: Result<()>) -> std::process::ExitCode {
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{tool}: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The tool's one argument, if any; more is a usage error.
pub fn argument(usage: &str) -> Result<Option<String>> {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next(), arguments.next()) {
        (argument, None) => Ok(argument),
        (Some(_), Some(_)) | (None, Some(_)) => Err(Error::Failed(format!("usage: {usage}"))),
    }
}

/// A display's VNC sockets (testbox/tools `with-display`), by their file names: one that only
/// shows, one that takes the viewer's input.
#[derive(Clone, Copy, PartialEq, Eq, strum::IntoStaticStr, strum::VariantArray)]
#[strum(serialize_all = "lowercase")]
pub enum View {
    Watch,
    Interact,
}

pub fn env_var(name: &str) -> Result<String> {
    std::env::var(name).map_err(|source| Error::Env {
        name: name.to_owned(),
        source,
    })
}

pub fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).context(path.display().to_string())
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, contents).context(path.display().to_string())
}

/// Fails unless `socket` is a socket owned by its directory's owner, where the box's clients put
/// theirs: the directory is theirs to write too, and a link planted there would lead a client of
/// it to any socket of the host's.
pub fn box_socket(socket: &Path) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let directory = socket
        .parent()
        .ok_or_else(|| Error::Failed(format!("{} has no directory", socket.display())))?;
    let owner = std::fs::metadata(directory)
        .context(directory.display().to_string())?
        .uid();
    let found = std::fs::symlink_metadata(socket).context(socket.display().to_string())?;
    if found.file_type().is_socket() && found.uid() == owner {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{} is not a socket of the box's (a {:?} of uid {})",
            socket.display(),
            found.file_type(),
            found.uid()
        )))
    }
}

pub fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context(path.display().to_string()),
    }
}
