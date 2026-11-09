//! What the testbox's programs share: reporting a failure with its causes; running commands
//! ([`process`]); the drivers' protocol ([`drivers`]), the keys ([`keys`]) and the form of a
//! decoded connection ([`connections`]), which the xtask uses too.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::path::{Path, PathBuf};

pub mod agent;
#[cfg(feature = "android")]
pub mod avd;
pub mod connections;
pub mod drivers;
pub mod keys;
pub mod process;
#[cfg(feature = "tls")]
pub mod tls;
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

/// What the `preview` tool logs once it serves.
pub const PREVIEW_READY: &str = "preview: serving";

/// What `front` says once it listens, the CDN3 front.
pub const FRONT_READY: &str = "front: serving";

/// What tcpdump says once it captures (testbox/capture/pod.yaml): its own wording, at the
/// version Debian trixie ships in the cdn0 image.
pub const CAPTURE_READY: &str = "listening on lo";

/// What `with-display` says when its application failed on its own: what waits for the
/// application to be ready stops waiting, rather than for its restarts.
pub const APPLICATION_FAILED: &str = "the application failed";

/// The line the `preview` tool logs for each request, which the checks can wait on.
pub fn preview_request_line(path: &str, status: u16) -> String {
    format!("preview: GET {path} {status}")
}

/// The title of the preview server's page `n` (`/page/<n>`), which a preview of it shows.
pub fn preview_title(n: u32) -> String {
    format!("b33p tour page {n}")
}

/// What the `storage` tool logs once storage-service serves on its tables.
pub const STORAGE_READY: &str = "storage: storage-service serves";

/// An error with its causes after it, each after a colon: what a tool or the xtask prints when
/// it reports a failure, as an error's own message names only its step.
pub struct Report<'error>(pub &'error dyn std::error::Error);

impl std::fmt::Display for Report<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)?;
        let mut cause = self.0.source();
        while let Some(error) = cause {
            write!(formatter, ": {error}")?;
            cause = error.source();
        }
        Ok(())
    }
}

/// The tool was given more than its one argument.
#[derive(Debug, thiserror::Error)]
#[error("usage: {usage}")]
pub struct UsageError {
    pub usage: String,
}

/// An environment variable the program needs, unset or not Unicode.
#[derive(Debug, thiserror::Error)]
#[error("the environment variable {name}")]
pub struct EnvError {
    pub name: String,
    #[source]
    pub source: std::env::VarError,
}

/// A file the tools read, write or remove whole ([`read`], [`write`], [`remove_if_present`]).
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("removing {}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A socket of the box's that [`box_socket`] refuses to connect to.
#[derive(Debug, thiserror::Error)]
pub enum SocketError {
    #[error("{} has no directory", socket.display())]
    NoDirectory { socket: PathBuf },
    /// Its directory's owner or the socket's own kind and owner, unreadable.
    #[error("looking at {}", path.display())]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Something else than a socket, or one another user owns: maybe a link planted there.
    #[error("{} is not a socket of the box's (a {file_type} of uid {uid}, the directory's {owner})", socket.display())]
    NotTheBoxs {
        socket: PathBuf,
        file_type: String,
        uid: u32,
        owner: u32,
    },
}

/// A tool's exit: its failure on stderr, under its name.
pub fn exit<E: Into<Box<dyn std::error::Error + Send + Sync>>>(
    tool: &str,
    outcome: Result<(), E>,
) -> std::process::ExitCode {
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{tool}: {}", Report(&*error.into()));
            std::process::ExitCode::FAILURE
        }
    }
}

/// The tool's one argument, if any; more is a usage error.
pub fn argument(usage: &str) -> Result<Option<String>, UsageError> {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next(), arguments.next()) {
        (argument, None) => Ok(argument),
        (Some(_), Some(_)) | (None, Some(_)) => Err(UsageError {
            usage: usage.to_owned(),
        }),
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

/// The environment variable `name`, which must be set.
pub fn env_var(name: &str) -> Result<String, EnvError> {
    std::env::var(name).map_err(|source| EnvError {
        name: name.to_owned(),
        source,
    })
}

/// The file at `path`, whole, as text.
pub fn read(path: &Path) -> Result<String, FileError> {
    std::fs::read_to_string(path).map_err(|source| FileError::Read {
        path: path.to_owned(),
        source,
    })
}

/// Writes `contents` to the file at `path`, replacing what it had.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<(), FileError> {
    std::fs::write(path, contents).map_err(|source| FileError::Write {
        path: path.to_owned(),
        source,
    })
}

/// Fails unless `socket` is a socket owned by its directory's owner, where the box's clients put
/// theirs: the directory is theirs to write too, and a link planted there would lead a client of
/// it to any socket of the host's.
pub fn box_socket(socket: &Path) -> Result<(), SocketError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let directory = socket.parent().ok_or_else(|| SocketError::NoDirectory {
        socket: socket.to_owned(),
    })?;
    let metadata = |path: &Path, of: fn(&Path) -> std::io::Result<std::fs::Metadata>| {
        of(path).map_err(|source| SocketError::Metadata {
            path: path.to_owned(),
            source,
        })
    };
    let owner = metadata(directory, |path| std::fs::metadata(path))?.uid();
    let found = metadata(socket, |path| std::fs::symlink_metadata(path))?;
    if found.file_type().is_socket() && found.uid() == owner {
        Ok(())
    } else {
        Err(SocketError::NotTheBoxs {
            socket: socket.to_owned(),
            file_type: format!("{:?}", found.file_type()),
            uid: found.uid(),
            owner,
        })
    }
}

/// Removes the file at `path`, if there is one.
pub fn remove_if_present(path: &Path) -> Result<(), FileError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(FileError::Remove {
            path: path.to_owned(),
            source,
        }),
    }
}
