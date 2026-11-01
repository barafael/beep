//! Running the programs the tools and the xtask drive: to success, or failing with what they
//! printed.

use std::io::Write;
use std::process::{Command, ExitStatus, Stdio};

/// A command that didn't run to success, named as [`describe`] names it.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("starting {command}")]
    Start {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// Its stdin, piped, wasn't there to write to.
    #[error("{command} has no stdin")]
    NoStdin { command: String },
    #[error("writing to {command}")]
    Input {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("waiting for {command}")]
    Wait {
        command: String,
        #[source]
        source: std::io::Error,
    },
    /// It exited with a failure, having printed `output` (empty when it printed to the terminal).
    #[error("{command} failed ({status}){}", if output.is_empty() { String::new() } else { format!("\n{output}") })]
    Exit {
        command: String,
        status: ExitStatus,
        output: String,
    },
}

/// `program args…`, as an error names a command.
pub fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs with the terminal attached; fails on a non-zero exit.
pub fn run(mut command: Command) -> Result<(), CommandError> {
    let status = command.status().map_err(|source| CommandError::Start {
        command: describe(&command),
        source,
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CommandError::Exit {
            command: describe(&command),
            status,
            output: String::new(),
        })
    }
}

/// Runs with `stdin` as input; returns stdout, and fails with stdout and stderr on a non-zero exit.
pub fn output(mut command: Command, stdin: &str) -> Result<String, CommandError> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| CommandError::Start {
            command: describe(&command),
            source,
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| CommandError::NoStdin {
            command: describe(&command),
        })?
        .write_all(stdin.as_bytes())
        .map_err(|source| CommandError::Input {
            command: describe(&command),
            source,
        })?;
    let output = child
        .wait_with_output()
        .map_err(|source| CommandError::Wait {
            command: describe(&command),
            source,
        })?;
    checked(&command, output)
}

/// What `command` printed to stdout, if it exited successfully; else what it printed to both.
fn checked(command: &Command, output: std::process::Output) -> Result<String, CommandError> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(CommandError::Exit {
            command: describe(command),
            status: output.status,
            output: format!("{stdout}{}", String::from_utf8_lossy(&output.stderr)),
        })
    }
}

#[cfg(feature = "async")]
pub use running::{LogError, SuperviseError, output_async, spawn_logged, supervise, wait_for};

/// The JVM's arguments for `--capture`'s key log: jSSLKeyLog (testbox/server/Containerfile),
/// writing the TLS sessions' secrets to the file `SSLKEYLOGFILE` names, as the other servers
/// do themselves; none without the variable. The JDK writes no key log of its own.
pub fn java_key_log_arguments() -> Vec<String> {
    match std::env::var("SSLKEYLOGFILE") {
        Ok(path) if !path.is_empty() => {
            eprintln!("the JVM's TLS sessions' secrets go to {path}");
            // The agent instruments the JDK's TLS engine: Jetty's connectors. Signal-Server's
            // omnibus port is Netty's, on BoringSSL unless its configuration says JDK, which
            // `provision` sets under `--capture`.
            vec![format!("-javaagent:{JSSLKEYLOG_JAR}={path}:quiet")]
        }
        _ => {
            eprintln!("SSLKEYLOGFILE unset, the JVM's TLS sessions' secrets go nowhere");
            Vec::new()
        }
    }
}

/// Where the server and storage images put jSSLKeyLog's agent for Java 24 and later.
pub const JSSLKEYLOG_JAR: &str = "/opt/testbox/jSSLKeyLog-java24+.jar";

#[cfg(feature = "async")]
mod running {
    use std::process::Stdio;

    use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;

    use std::time::Duration;

    use super::{CommandError, checked, describe};

    /// A program's log that didn't say what [`wait_for`] waited for.
    #[derive(Debug, thiserror::Error)]
    pub enum LogError {
        #[error("the log ended without {waiting:?}")]
        Ended { waiting: Vec<String> },
        #[error("no {waiting:?} within {} s", patience.as_secs())]
        Timeout {
            waiting: Vec<String>,
            patience: Duration,
        },
    }

    /// A supervised program that failed, or that couldn't be kept or stopped ([`supervise`]).
    #[derive(Debug, thiserror::Error)]
    pub enum SuperviseError {
        #[error("listening for SIGTERM")]
        Listen(#[source] std::io::Error),
        #[error("waiting for {name}")]
        Wait {
            name: String,
            #[source]
            source: std::io::Error,
        },
        #[error("{name} exited: {status}")]
        Exited {
            name: String,
            status: std::process::ExitStatus,
        },
        #[error("stopping {name}")]
        Stop {
            name: String,
            #[source]
            source: std::io::Error,
        },
        #[error("SIGTERM can no longer be heard while {name} runs")]
        Deaf { name: String },
    }

    /// Waits up to `patience` until every one of `needles` has shown in a line from `heard`, as
    /// [`spawn_logged`] passes them on; the receiver is dropped after, the log goes on to ours.
    pub async fn wait_for(
        mut heard: mpsc::UnboundedReceiver<String>,
        needles: &[&str],
        patience: Duration,
    ) -> Result<(), LogError> {
        let mut waiting = needles.to_vec();
        let still = |waiting: &[&str]| waiting.iter().map(|&needle| needle.to_owned()).collect();
        let heard_all = async {
            while !waiting.is_empty() {
                let line = heard.recv().await.ok_or_else(|| LogError::Ended {
                    waiting: still(&waiting),
                })?;
                waiting.retain(|needle| !line.contains(needle));
            }
            Ok(())
        };
        let outcome = tokio::time::timeout(patience, heard_all).await;
        outcome.unwrap_or_else(|_elapsed| {
            Err(LogError::Timeout {
                waiting: still(&waiting),
                patience,
            })
        })
    }

    /// [`super::output`] for a tool's runtime, without input.
    pub async fn output_async(command: &mut Command) -> Result<String, CommandError> {
        let output = command
            .output()
            .await
            .map_err(|source| CommandError::Start {
                command: describe(command.as_std()),
                source,
            })?;
        checked(command.as_std(), output)
    }

    /// Starts `command` with its output going on to the tool's stderr, line by line, and to the
    /// receiver for as long as it is kept: what a tool waits for its program to say.
    pub fn spawn_logged(
        command: &mut Command,
    ) -> Result<(Child, mpsc::UnboundedReceiver<String>), CommandError> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| CommandError::Start {
                command: describe(command.as_std()),
                source,
            })?;
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
    pub async fn supervise(child: &mut Child, name: &str) -> Result<(), SuperviseError> {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(SuperviseError::Listen)?;
        let waited = |source| SuperviseError::Wait {
            name: name.to_owned(),
            source,
        };
        tokio::select! {
            exited = child.wait() => {
                let status = exited.map_err(waited)?;
                if status.success() {
                    Ok(())
                } else {
                    Err(SuperviseError::Exited { name: name.to_owned(), status })
                }
            }
            signal = terminate.recv() => match signal {
                Some(()) => {
                    child.start_kill().map_err(|source| SuperviseError::Stop {
                        name: name.to_owned(),
                        source,
                    })?;
                    child.wait().await.map_err(waited).map(drop)
                }
                None => Err(SuperviseError::Deaf { name: name.to_owned() }),
            }
        }
    }
}
