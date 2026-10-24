//! Running the programs the tools and the xtask drive: to success, or failing with what they
//! printed.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::{Context, Error, Result, WithContext};

/// `program args…`, as an error names a command.
pub fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs with the terminal attached; fails on a non-zero exit.
pub fn run(mut command: Command) -> Result<()> {
    let status = command.status().context(describe(&command))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Command {
            command: describe(&command),
            output: format!(" ({status})"),
        })
    }
}

/// Runs with `stdin` as input; returns stdout, and fails with stdout and stderr on a non-zero exit.
pub fn output(mut command: Command, stdin: &str) -> Result<String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(describe(&command))?;
    child
        .stdin
        .take()
        .ok_or_else(|| Error::Failed(format!("{}: no stdin", describe(&command))))?
        .write_all(stdin.as_bytes())
        .with_context(|| format!("writing to {}", describe(&command)))?;
    let output = child.wait_with_output().context(describe(&command))?;
    checked(&command, output)
}

fn checked(command: &Command, output: std::process::Output) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(Error::Command {
            command: describe(command),
            output: format!(
                " ({})\n{stdout}{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        })
    }
}

#[cfg(feature = "async")]
pub use running::{output_async, spawn_logged, supervise, wait_for};

#[cfg(feature = "async")]
mod running {
    use std::process::Stdio;

    use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;

    use std::time::Duration;

    use super::{checked, describe};
    use crate::{Context, Error, Result, WithContext};

    /// Waits up to `patience` until every one of `needles` has shown in a line from `heard`, as
    /// [`spawn_logged`] passes them on; the receiver is dropped after, the log goes on to ours.
    pub async fn wait_for(
        mut heard: mpsc::UnboundedReceiver<String>,
        needles: &[&str],
        patience: Duration,
    ) -> Result<()> {
        let mut waiting = needles.to_vec();
        let heard_all = async {
            while !waiting.is_empty() {
                let line = heard
                    .recv()
                    .await
                    .ok_or_else(|| Error::Failed(format!("the log ended without {waiting:?}")))?;
                waiting.retain(|needle| !line.contains(needle));
            }
            Ok(())
        };
        let outcome = tokio::time::timeout(patience, heard_all).await;
        outcome.unwrap_or_else(|_elapsed| {
            Err(Error::Failed(format!(
                "no {waiting:?} within {} s",
                patience.as_secs()
            )))
        })
    }

    /// [`super::output`] for a tool's runtime, without input.
    pub async fn output_async(command: &mut Command) -> Result<String> {
        let output = command.output().await.context(describe(command.as_std()))?;
        checked(command.as_std(), output)
    }

    /// Starts `command` with its output going on to the tool's stderr, line by line, and to the
    /// receiver for as long as it is kept: what a tool waits for its program to say.
    pub fn spawn_logged(command: &mut Command) -> Result<(Child, mpsc::UnboundedReceiver<String>)> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context(describe(command.as_std()))?;
        let (lines, heard) = mpsc::unbounded_channel();
        // Piped just above.
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(pass_on(stdout, lines.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(pass_on(stderr, lines));
        }
        Ok((child, heard))
    }

    async fn pass_on(stream: impl AsyncRead + Unpin, lines: mpsc::UnboundedSender<String>) {
        let mut listener = Some(lines);
        let mut reader = BufReader::new(stream).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    eprintln!("{line}");
                    // Once the receiver is dropped, the lines only go to the log.
                    if let Some(sender) = &listener
                        && let Err(mpsc::error::SendError(_)) = sender.send(line)
                    {
                        listener = None;
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    eprintln!("reading a program's output: {error}");
                    return;
                }
            }
        }
    }

    /// Waits until `child` exits, which is a failure unless it succeeded, or until the tool is
    /// told to stop (SIGTERM, as a container's first process is), which stops `child` too.
    pub async fn supervise(child: &mut Child, name: &str) -> Result<()> {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("waiting for SIGTERM")?;
        tokio::select! {
            exited = child.wait() => {
                let status = exited.context(name)?;
                if status.success() {
                    Ok(())
                } else {
                    Err(Error::Failed(format!("{name} exited: {status}")))
                }
            }
            signal = terminate.recv() => match signal {
                Some(()) => {
                    child.start_kill().with_context(|| format!("stopping {name}"))?;
                    child.wait().await.context(name).map(drop)
                }
                None => Err(Error::Failed(format!(
                    "SIGTERM can no longer be heard while {name} runs"
                ))),
            }
        }
    }
}
