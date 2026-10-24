use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use testbox_tools::process::describe;
pub use testbox_tools::process::{output, run};

use crate::error::{Context, Error, Result, WithContext};

/// What following a log came to, short of running out of time.
pub enum Heard {
    Found,
    /// The log ended first: its container stopped.
    Ended,
}

/// The time in seconds since the epoch, as `since` for [`crate::pod::Pod::await_log`].
pub fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|error| Error::Failed(format!("the clock is before 1970: {error}")))
}

/// Reads `command`'s stdout and stderr until `times` lines have contained `needle` or the output
/// ends, for up to `patience`.
pub fn await_lines(
    mut command: Command,
    needle: &str,
    times: usize,
    patience: Duration,
) -> Result<Heard> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(describe(&command))?;
    let (found, seen) = mpsc::channel();
    let readers = [
        child
            .stdout
            .take()
            .map(|out| watch(out, needle, found.clone())),
        child
            .stderr
            .take()
            .map(|err| watch(err, needle, found.clone())),
    ];
    // Only the readers hold senders now: both done without a match is a disconnect.
    drop(found);
    let deadline = Instant::now() + patience;
    let mut heard = 0;
    let outcome = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match seen.recv_timeout(left) {
            Ok(Ok(())) => {
                heard += 1;
                if heard == times {
                    break Ok(Heard::Found);
                }
            }
            Ok(Err(source)) => {
                break Err(Error::Io {
                    what: format!("reading the output of {}", describe(&command)),
                    source,
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                break Err(Error::Failed(format!(
                    "{}: {heard} of {times} {needle:?} within {}s",
                    describe(&command),
                    patience.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(Heard::Ended),
        }
    };
    match child.kill() {
        // Already exited, when its output ended.
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
        stopped => stopped.with_context(|| format!("stopping {}", describe(&command)))?,
    }
    child.wait().context(describe(&command))?;
    for reader in readers.into_iter().flatten() {
        reader
            .join()
            .map_err(|_| Error::Failed(format!("a reader of {} panicked", describe(&command))))?;
    }
    outcome
}

/// Reads `stream` by line on a thread of its own, sending each time a line contains `needle`, and
/// the error that ends the reading, if one does.
fn watch<R: Read + Send + 'static>(
    stream: R,
    needle: &str,
    found: mpsc::Sender<std::io::Result<()>>,
) -> std::thread::JoinHandle<()> {
    let needle = needle.to_owned();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            match line {
                Ok(line) if line.contains(&needle) => {
                    // The receiver may have given up already; then nobody needs the news.
                    if found.send(Ok(())).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                // The reading ends here; the receiver hears why, if it still listens.
                Err(error) => match found.send(Err(error)) {
                    Ok(()) | Err(mpsc::SendError(_)) => return,
                },
            }
        }
    })
}
