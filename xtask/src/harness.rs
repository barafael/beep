//! Clients of the box's JSON-line sockets: b33p's agent harness (its protocol is the b33p-harness
//! crate) and the Desktop and Android drivers (`testbox_tools::drivers`), which answer the same way.

use std::io::{BufRead, BufReader, Write};
use std::marker::PhantomData;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use b33p_harness::{Acted, Action, Call, Pushed, Reply, Snapshot, Subscribed};
use serde::de::DeserializeOwned;

use crate::error::{Context, Error, Result, WithContext};
use serde::Serialize;
use testbox_tools::drivers::{Id, Numbered};

struct Connection {
    stream: UnixStream,
    lines: BufReader<UnixStream>,
    next_id: u64,
}

impl Connection {
    /// Connects to a listening socket of the box's (`testbox_tools::box_socket`): the caller waits
    /// for its service to say it listens.
    fn connect(socket: &Path) -> Result<Self> {
        testbox_tools::box_socket(socket)?;
        let stream = UnixStream::connect(socket).context(socket.display().to_string())?;
        let lines = BufReader::new(
            stream
                .try_clone()
                .context("a second handle to the socket")?,
        );
        Ok(Self {
            stream,
            lines,
            next_id: 1,
        })
    }

    fn request<C: Serialize, T: DeserializeOwned>(&mut self, call: &C) -> Result<T> {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&Numbered { id, call }).context("a request")?;
        writeln!(self.stream, "{line}").context("sending a request")?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.lines.read_line(&mut line).context("reading a reply")? == 0 {
                return Err("the socket closed the connection".into());
            }
            // Pushed updates have no id, and other replies another.
            let reply: Id = serde_json::from_str(&line).with_context(|| line.clone())?;
            if reply.id != Some(id) {
                continue;
            }
            let reply: Reply<T> = serde_json::from_str(&line).with_context(|| line.clone())?;
            return Result::from(reply.outcome).map_err(Error::Failed);
        }
    }

    /// The next pushed update, or an error once `deadline` passes: a read that blocks, bounded
    /// by the time left.
    fn next_pushed(&mut self, deadline: Instant) -> Result<Pushed> {
        let mut line = String::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("timed out".into());
            }
            self.stream
                .set_read_timeout(Some(left))
                .context("setting a read timeout")?;
            line.clear();
            let read = self.lines.read_line(&mut line);
            self.stream
                .set_read_timeout(None)
                .context("clearing the read timeout")?;
            match read {
                Ok(0) => return Err("the socket closed the connection".into()),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err("timed out".into());
                }
                Err(error) => return Err(error).context("reading an update"),
            }
            // A reply to a request nobody waits for any more isn't an update.
            if serde_json::from_str::<Id>(&line)
                .with_context(|| line.clone())?
                .id
                .is_none()
            {
                return serde_json::from_str(&line).with_context(|| line.clone());
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
    pub fn connect(socket: &Path) -> Result<Self> {
        Ok(Self {
            connection: Connection::connect(socket)?,
            subscribed: false,
        })
    }

    pub fn snapshot(&mut self) -> Result<Snapshot> {
        self.connection.request(&Call::Snapshot)
    }

    /// Returns whether a command went to the backend.
    pub fn act(&mut self, action: Action) -> Result<bool> {
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
    ) -> Result<Found> {
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
                Err(error) => {
                    return Err(Error::Failed(format!(
                        "{error} waiting for {waiting_for}; screen {:?}, error {:?}",
                        snapshot.screen, snapshot.error
                    )));
                }
            };
        }
    }
}

/// A driver of a Signal client, taking calls of type `C`.
pub struct Driver<C>(Connection, PhantomData<C>);

impl<C: Serialize> Driver<C> {
    pub fn connect(socket: &Path) -> Result<Self> {
        Connection::connect(socket).map(|connection| Self(connection, PhantomData))
    }

    /// A call answered with nothing but success.
    pub fn call(&mut self, call: C) -> Result<()> {
        self.0.request(&call)
    }

    /// A call answered with a `T`.
    pub fn ask<T: DeserializeOwned>(&mut self, call: C) -> Result<T> {
        self.0.request(&call)
    }
}
