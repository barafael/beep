//! `testbox-shot <directory> <label> <name>=<socket>...` writes each display, as it is now, to
//! `<directory>/<label>-<name>.png`. It connects to every display first, then restricts itself
//! with Landlock (`testbox_vnc::sandbox_writing`) to writing beneath the directory, and only then
//! takes the frames. A display it can't take is skipped and said so; it fails if any was.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Duration;

use testbox_tools::Report;
use testbox_vnc::{ConnectError, Frame, SandboxError, SnapshotError};

/// How long a display may take to send all of itself.
const PATIENCE: Duration = Duration::from_secs(10);

const USAGE: &str = "usage: testbox-shot <directory> <label> <name>=<socket>...";

fn main() -> std::process::ExitCode {
    testbox_tools::exit("testbox-shot", run())
}

/// What keeps testbox-shot from its screenshots.
#[derive(Debug, thiserror::Error)]
enum ShotError {
    #[error("no directory given ({USAGE})")]
    NoDirectory,
    #[error("no label given ({USAGE})")]
    NoLabel,
    /// An argument after the label that is not a display.
    #[error("{argument:?} is no <name>=<socket> ({USAGE})")]
    NotADisplay { argument: String },
    #[error("no display given ({USAGE})")]
    NoDisplay,
    #[error("making the directory {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("starting a tokio runtime")]
    Runtime {
        #[source]
        source: std::io::Error,
    },
    #[error("restricting the process to writing beneath {}", directory.display())]
    Sandbox {
        directory: PathBuf,
        #[source]
        source: SandboxError,
    },
    /// Each display skipped and why: several causes, so each with its own chain, none as the
    /// source.
    #[error("not taken: {}", skipped.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    NotTaken { skipped: Vec<Skipped> },
}

/// A display not taken, by name, and why, with the reason's causes.
#[derive(Debug)]
struct Skipped {
    name: String,
    reason: TakeError,
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.name, Report(&self.reason))
    }
}

/// Why one display's screenshot wasn't taken.
#[derive(Debug, thiserror::Error)]
enum TakeError {
    #[error("connecting to the display")]
    Connect {
        #[source]
        source: ConnectError,
    },
    #[error("taking the display's frame")]
    Snapshot {
        #[source]
        source: SnapshotError,
    },
    #[error("creating {}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {} as a PNG", path.display())]
    Encode {
        path: PathBuf,
        #[source]
        source: png::EncodingError,
    },
}

fn run() -> Result<(), ShotError> {
    let mut arguments = std::env::args().skip(1);
    let directory = PathBuf::from(arguments.next().ok_or(ShotError::NoDirectory)?);
    let label = arguments.next().ok_or(ShotError::NoLabel)?;
    let mut displays = Vec::new();
    for argument in arguments {
        let Some((name, socket)) = argument.split_once('=') else {
            return Err(ShotError::NotADisplay { argument });
        };
        displays.push((name.to_owned(), PathBuf::from(socket)));
    }
    if displays.is_empty() {
        return Err(ShotError::NoDisplay);
    }
    std::fs::create_dir_all(&directory).map_err(|source| ShotError::CreateDirectory {
        path: directory.clone(),
        source,
    })?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| ShotError::Runtime { source })?
        .block_on(take(&directory, &label, displays))
}

async fn take(
    directory: &Path,
    label: &str,
    displays: Vec<(String, PathBuf)>,
) -> Result<(), ShotError> {
    let mut skipped = Vec::new();
    let mut connected = Vec::new();
    for (name, socket) in displays {
        match testbox_vnc::connect(&socket).await {
            Ok(client) => connected.push((name, client)),
            Err(source) => skipped.push(Skipped {
                name,
                reason: TakeError::Connect { source },
            }),
        }
    }
    testbox_vnc::sandbox_writing(directory.to_owned()).map_err(|source| ShotError::Sandbox {
        directory: directory.to_owned(),
        source,
    })?;
    for (name, client) in connected {
        let path = directory.join(format!("{label}-{name}.png"));
        let written = match testbox_vnc::snapshot(client, PATIENCE).await {
            Ok(frame) => write(path, &frame),
            Err(source) => Err(TakeError::Snapshot { source }),
        };
        if let Err(reason) = written {
            skipped.push(Skipped { name, reason });
        }
    }
    if skipped.is_empty() {
        Ok(())
    } else {
        Err(ShotError::NotTaken { skipped })
    }
}

fn write(path: PathBuf, frame: &Frame) -> Result<(), TakeError> {
    let file = match File::create(&path) {
        Ok(file) => file,
        Err(source) => return Err(TakeError::Create { path, source }),
    };
    let mut encoder = png::Encoder::new(
        BufWriter::new(file),
        u32::from(frame.width()),
        u32::from(frame.height()),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(frame.rgba()))
        .map_err(|source| TakeError::Encode { path, source })
}
