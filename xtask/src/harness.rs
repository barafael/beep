//! Clients of the box's JSON-line sockets: b33p's agent harness (its protocol is the b33p-harness
//! crate) and the Desktop and Android drivers (`testbox_tools::drivers`), which answer the same way.

use std::io::{BufRead, BufReader, Write};
use std::marker::PhantomData;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use b33p_harness::{Acted, Action, Call, Pushed, Reply, Shot, Snapshot, Subscribed};
use serde::de::DeserializeOwned;

use serde::Serialize;
use testbox_tools::SocketError;
use testbox_tools::drivers::{Id, Numbered};

/// What went wrong talking to one of the box's sockets: b33p's harness or a driver, named by its
/// socket.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The socket isn't one of the box's, to be trusted.
    #[error("refusing {}", socket.display())]
    Untrusted {
        socket: PathBuf,
        #[source]
        source: SocketError,
    },
    #[error("connecting to {}", socket.display())]
    Connect {
        socket: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The socket's I/O: `doing` is the step, as "sending a request" or "reading a reply".
    #[error("{doing} on {}", socket.display())]
    Io {
        socket: PathBuf,
        doing: Step,
        #[source]
        source: std::io::Error,
    },
    #[error("encoding a request for {}", socket.display())]
    Encode {
        socket: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// A line from the socket that isn't a reply or an update.
    #[error("{} sent {line:?}", socket.display())]
    Decode {
        socket: PathBuf,
        line: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{} closed the connection", socket.display())]
    Closed { socket: PathBuf },
    /// The other side refused `request`, saying `reason`.
    #[error("{} refused {request}: {reason}", socket.display())]
    Refused {
        socket: PathBuf,
        request: String,
        reason: String,
    },
    /// No update came in time.
    #[error("{} sent no update in time", socket.display())]
    TimedOut { socket: PathBuf },
    /// What b33p showed never came within the patience.
    #[error("{waited}")]
    Wait {
        /// Boxed, as every check's error carries a `HarnessError`.
        waited: Box<Waited>,
        #[source]
        source: Box<HarnessError>,
    },
}

/// A wait that ran out ([`HarnessError::Wait`]): `screen` and `error` are the last state's.
#[derive(Debug)]
pub struct Waited {
    pub waiting_for: String,
    pub patience: Duration,
    pub screen: String,
    pub error: Option<String>,
}

impl std::fmt::Display for Waited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "waiting {} s for {}; screen {}, error {:?}",
            self.patience.as_secs(),
            self.waiting_for,
            self.screen,
            self.error
        )
    }
}

/// A step of talking to a socket, as [`HarnessError::Io`] names it.
#[derive(Debug, Clone, Copy, strum::Display)]
pub enum Step {
    #[strum(serialize = "a second handle to the socket")]
    Clone,
    #[strum(serialize = "sending a request")]
    Send,
    #[strum(serialize = "reading a reply")]
    ReadReply,
    #[strum(serialize = "reading an update")]
    ReadUpdate,
    #[strum(serialize = "setting a read timeout")]
    SetTimeout,
}

struct Connection {
    socket: PathBuf,
    stream: UnixStream,
    lines: BufReader<UnixStream>,
    next_id: u64,
}

impl Connection {
    /// Connects to a listening socket of the box's (`testbox_tools::box_socket`): the caller waits
    /// for its service to say it listens.
    fn connect(socket: &Path) -> Result<Self, HarnessError> {
        testbox_tools::box_socket(socket).map_err(|source| HarnessError::Untrusted {
            socket: socket.to_owned(),
            source,
        })?;
        let stream = UnixStream::connect(socket).map_err(|source| HarnessError::Connect {
            socket: socket.to_owned(),
            source,
        })?;
        let lines = BufReader::new(stream.try_clone().map_err(|source| HarnessError::Io {
            socket: socket.to_owned(),
            doing: Step::Clone,
            source,
        })?);
        Ok(Self {
            socket: socket.to_owned(),
            stream,
            lines,
            next_id: 1,
        })
    }

    /// [`HarnessError::Io`] on this socket, `doing` the step.
    fn io(&self, doing: Step) -> impl FnOnce(std::io::Error) -> HarnessError {
        let socket = self.socket.clone();
        move |source| HarnessError::Io {
            socket,
            doing,
            source,
        }
    }

    /// `line` as a `T`, or [`HarnessError::Decode`].
    fn decode<T: DeserializeOwned>(&self, line: &str) -> Result<T, HarnessError> {
        serde_json::from_str(line).map_err(|source| HarnessError::Decode {
            socket: self.socket.clone(),
            line: line.trim_end().to_owned(),
            source,
        })
    }

    fn request<C: Serialize, T: DeserializeOwned>(&mut self, call: &C) -> Result<T, HarnessError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::to_string(&Numbered { id, call }).map_err(|source| {
            HarnessError::Encode {
                socket: self.socket.clone(),
                source,
            }
        })?;
        writeln!(self.stream, "{request}").map_err(self.io(Step::Send))?;
        let mut line = String::new();
        loop {
            line.clear();
            if self
                .lines
                .read_line(&mut line)
                .map_err(self.io(Step::ReadReply))?
                == 0
            {
                return Err(HarnessError::Closed {
                    socket: self.socket.clone(),
                });
            }
            // Pushed updates have no id, and other replies another.
            let reply: Id = self.decode(&line)?;
            if reply.id != Some(id) {
                continue;
            }
            let reply: Reply<T> = self.decode(&line)?;
            return Result::from(reply.outcome).map_err(|reason| HarnessError::Refused {
                socket: self.socket.clone(),
                request,
                reason,
            });
        }
    }

    /// The next pushed update, or an error once `deadline` passes: a read that blocks, bounded
    /// by the time left.
    fn next_pushed(&mut self, deadline: Instant) -> Result<Pushed, HarnessError> {
        let timed_out = || HarnessError::TimedOut {
            socket: self.socket.clone(),
        };
        let mut line = String::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(timed_out());
            }
            self.stream
                .set_read_timeout(Some(left))
                .map_err(self.io(Step::SetTimeout))?;
            line.clear();
            let read = self.lines.read_line(&mut line);
            self.stream
                .set_read_timeout(None)
                .map_err(self.io(Step::SetTimeout))?;
            match read {
                Ok(0) => {
                    return Err(HarnessError::Closed {
                        socket: self.socket.clone(),
                    });
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(timed_out());
                }
                Err(error) => return Err(self.io(Step::ReadUpdate)(error)),
            }
            // A reply to a request nobody waits for any more isn't an update.
            if self.decode::<Id>(&line)?.id.is_none() {
                return self.decode(&line);
            }
        }
    }
}

/// b33p's agent harness.
pub struct Harness {
    connection: Connection,
    /// Whether b33p pushes an update after every redraw to this connection yet.
    subscribed: bool,
}

impl Harness {
    pub fn connect(socket: &Path) -> Result<Self, HarnessError> {
        Ok(Self {
            connection: Connection::connect(socket)?,
            subscribed: false,
        })
    }

    /// What b33p shows now.
    pub fn snapshot(&mut self) -> Result<Snapshot, HarnessError> {
        self.connection.request(&Call::Snapshot)
    }

    /// Writes the window, drawn from the state as it is now, to `path` as a PNG; the path is
    /// b33p's, in its container. A b33p that doesn't answer within `patience` fails it.
    pub fn screenshot(&mut self, path: &str, patience: Duration) -> Result<Shot, HarnessError> {
        self.connection
            .stream
            .set_read_timeout(Some(patience))
            .map_err(self.connection.io(Step::SetTimeout))?;
        self.connection.request(&Call::Screenshot {
            path: path.to_owned(),
        })
    }

    /// Returns whether a command went to the backend.
    pub fn act(&mut self, action: Action) -> Result<bool, HarnessError> {
        let acted: Acted = self.connection.request(&Call::Act { action })?;
        Ok(acted.command_sent)
    }

    /// Waits until `accept` returns a value for what b33p shows, or fails after `patience` with
    /// `waiting_for` and the last state's screen. Nothing polls: b33p pushes the state after
    /// every redraw, and every change redraws.
    pub fn wait_for<Found>(
        &mut self,
        waiting_for: &str,
        patience: Duration,
        mut accept: impl FnMut(&Snapshot) -> Option<Found>,
    ) -> Result<Found, HarnessError> {
        let deadline = Instant::now() + patience;
        // Subscribed before the first look, so no change between the two goes unseen.
        if !self.subscribed {
            self.connection
                .request::<_, Subscribed>(&Call::Subscribe)
                .map(drop)?;
            self.subscribed = true;
        }
        let mut snapshot = self.snapshot()?;
        loop {
            if let Some(found) = accept(&snapshot) {
                return Ok(found);
            }
            snapshot = match self.connection.next_pushed(deadline) {
                Ok(pushed) => pushed.snapshot,
                Err(source) => {
                    return Err(HarnessError::Wait {
                        waited: Box::new(Waited {
                            waiting_for: waiting_for.to_owned(),
                            patience,
                            screen: format!("{:?}", snapshot.screen),
                            error: snapshot.error,
                        }),
                        source: Box::new(source),
                    });
                }
            };
        }
    }
}

/// A driver of a Signal client, taking calls of type `C`.
pub struct Driver<C>(Connection, PhantomData<C>);

impl<C: Serialize> Driver<C> {
    pub fn connect(socket: &Path) -> Result<Self, HarnessError> {
        Connection::connect(socket).map(|connection| Self(connection, PhantomData))
    }

    /// A call answered with nothing but success.
    pub fn call(&mut self, call: C) -> Result<(), HarnessError> {
        self.0.request(&call)
    }

    /// A call answered with a `T`.
    pub fn ask<T: DeserializeOwned>(&mut self, call: C) -> Result<T, HarnessError> {
        self.0.request(&call)
    }
}
