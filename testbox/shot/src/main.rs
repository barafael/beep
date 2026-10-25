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

use testbox_tools::{Context, Describe, Error, Result};
use testbox_vnc::Frame;

/// How long a display may take to send all of itself.
const PATIENCE: Duration = Duration::from_secs(10);

fn main() -> std::process::ExitCode {
    testbox_tools::exit("testbox-shot", run())
}

fn run() -> Result<()> {
    let usage =
        || Error::Failed("usage: testbox-shot <directory> <label> <name>=<socket>...".into());
    let mut arguments = std::env::args().skip(1);
    let directory = PathBuf::from(arguments.next().ok_or_else(usage)?);
    let label = arguments.next().ok_or_else(usage)?;
    let mut displays = Vec::new();
    for argument in arguments {
        let (name, socket) = argument.split_once('=').ok_or_else(usage)?;
        displays.push((name.to_owned(), PathBuf::from(socket)));
    }
    if displays.is_empty() {
        return Err(usage());
    }
    std::fs::create_dir_all(&directory).context(directory.display().to_string())?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .describe("a runtime")?
        .block_on(take(&directory, &label, displays))
}

async fn take(directory: &Path, label: &str, displays: Vec<(String, PathBuf)>) -> Result<()> {
    let mut skipped = Vec::new();
    let mut connected = Vec::new();
    for (name, socket) in displays {
        match testbox_vnc::connect(&socket).await {
            Ok(client) => connected.push((name, client)),
            Err(error) => skipped.push(format!("{name}: {error}")),
        }
    }
    testbox_vnc::sandbox_writing(directory.to_owned())?;
    for (name, client) in connected {
        let path = directory.join(format!("{label}-{name}.png"));
        let written = match testbox_vnc::snapshot(client, PATIENCE).await {
            Ok(frame) => write(&path, &frame),
            Err(error) => Err(error),
        };
        if let Err(error) = written {
            skipped.push(format!("{name}: {error}"));
        }
    }
    if skipped.is_empty() {
        Ok(())
    } else {
        Err(Error::Failed(format!("not taken: {}", skipped.join("; "))))
    }
}

fn write(path: &Path, frame: &Frame) -> Result<()> {
    let file = File::create(path).context(path.display().to_string())?;
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
        .describe(path.display().to_string())
}
