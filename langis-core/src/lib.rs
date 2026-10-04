//! UI-agnostic Signal backend.
//!
//! A [`Backend`] runs on its own thread. The UI sends [`Command`]s and receives [`Event`]s,
//! either by awaiting them in its event loop or by blocking on a worker thread; neither side
//! ever blocks the other. All types here are plain data so any toolkit can
//! render them.

mod mock;
mod qr;
pub mod settings;
pub mod state;
#[cfg(feature = "signal")]
mod signal;
#[cfg(feature = "signal")]
mod import;
#[cfg(feature = "signal")]
mod transfer;
#[cfg(feature = "signal")]
mod vault;

pub use qr::QrMatrix;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

/// Opaque, stable identifier of a 1:1 chat or group.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConversationId(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub id: ConversationId,
    pub title: String,
    pub is_group: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub conversation: ConversationId,
    /// Stable identity of the author (a Signal service ID); `sender` is only a display name.
    pub sender_id: String,
    pub sender: String,
    pub body: String,
    /// Milliseconds since the Unix epoch, as used by Signal.
    pub timestamp: u64,
    pub outgoing: bool,
}

/// A square profile or group picture, decoded to RGBA (row-major, 4 bytes per pixel).
#[derive(Clone, PartialEq, Eq)]
pub struct Avatar {
    pub side: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Avatar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Avatar({0}x{0})", self.side)
    }
}

/// Stages of the message history transfer; see [`Event::HistoryTransfer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryTransfer {
    /// Waiting for the phone to upload the history.
    Waiting,
    Downloading,
    /// This many messages imported so far.
    Importing { messages: usize },
    /// Done, failed or skipped; errors arrive separately.
    Finished,
}

/// A user-entered password. Wiped from memory on drop and never printed.
#[derive(Clone)]
pub struct Password(zeroize::Zeroizing<String>);

impl Password {
    pub fn new(password: impl Into<String>) -> Self {
        Self(zeroize::Zeroizing::new(password.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(***)")
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// No system keyring is available: the local database is protected by a password.
    /// `new` means none has been chosen yet; `error` explains why the last attempt failed.
    PasswordRequired { new: bool, error: Option<String> },
    /// Not linked yet: show this QR code to be scanned from the phone.
    LinkQr(QrMatrix),
    /// Linked (or loaded an existing registration) as `account`.
    Linked { account: String },
    /// Lost the connection to Signal and retrying; [`Event::Synced`] follows once back.
    Reconnecting,
    /// Progress of the message history transfer right after linking.
    HistoryTransfer(HistoryTransfer),
    /// Initial message queue drained; it is now safe to send.
    Synced,
    /// New or changed conversations, merged by id.
    Conversations(Vec<Conversation>),
    /// Profile or group picture of a conversation, replacing any earlier one.
    Avatar { conversation: ConversationId, avatar: Arc<Avatar> },
    /// The complete stored history of a conversation, answering [`Command::LoadHistory`].
    History { conversation: ConversationId, messages: Vec<ChatMessage> },
    /// A conversation's newest stored message, so it can be sorted and previewed before its
    /// history is loaded. Unlike [`Event::Message`], it is not new and not unread.
    Latest(ChatMessage),
    Message(ChatMessage),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum Command {
    /// Answer to [`Event::PasswordRequired`].
    Unlock(Password),
    LoadHistory(ConversationId),
    Send { conversation: ConversationId, body: String },
}

pub struct Config {
    /// Name shown in the phone's "Linked devices" list.
    pub device_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { device_name: "langis".into() }
    }
}

/// Backend side of the event channel. It is unbounded, so sending never blocks or awaits.
#[derive(Clone)]
pub(crate) struct Events(async_channel::Sender<Event>);

impl Events {
    /// Returns `false` once the UI has gone away.
    pub(crate) fn send(&self, event: Event) -> bool {
        self.0.try_send(event).is_ok()
    }
}

/// What travels to the backend: UI commands, then possibly the order to stop.
enum Request {
    Command(Command),
    Shutdown,
}

/// Backend side of the command channel.
pub(crate) struct Inbox(Receiver<Request>);

impl Inbox {
    /// The next command; `None` once the backend should stop.
    pub(crate) fn recv(&self) -> Option<Command> {
        match self.0.recv() {
            Ok(Request::Command(command)) => Some(command),
            Ok(Request::Shutdown) | Err(_) => None,
        }
    }

    /// Like [`Inbox::recv`], but gives up after `timeout`.
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Result<Command, RecvTimeoutError> {
        match self.0.recv_timeout(timeout)? {
            Request::Command(command) => Ok(command),
            Request::Shutdown => Err(RecvTimeoutError::Disconnected),
        }
    }

    /// Commands until the backend should stop.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Command> + '_ {
        std::iter::from_fn(|| self.recv())
    }
}

/// Cheap, cloneable handle for sending commands, e.g. from UI callbacks. It doesn't keep the
/// backend alive; the [`Backend`] does.
#[derive(Clone)]
pub struct Commands(Sender<Request>);

impl Commands {
    pub fn send(&self, command: Command) {
        // The backend only goes away when it has failed fatally, which it reports as an event,
        // or when it was shut down.
        let _ = self.0.send(Request::Command(command));
    }
}

/// Owner of a running backend. Dropping it stops the backend and waits until it has closed its
/// database, so drop it before the process exits: library teardown in `exit()` crashes threads
/// still using the database.
pub struct Backend {
    commands: Commands,
    events: async_channel::Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Backend {
    /// Starts the real Signal backend.
    #[cfg(feature = "signal")]
    pub fn start(config: Config) -> Self {
        Self::spawn(move |inbox, events| signal::run(config, inbox, events))
    }

    /// Starts an offline backend with canned data, for UI work.
    pub fn mock() -> Self {
        Self::spawn(mock::run)
    }

    /// Starts the real backend, or the mock one when `LANGIS_MOCK` is set or the
    /// `signal` feature is disabled.
    pub fn from_env(config: Config) -> Self {
        #[cfg(feature = "signal")]
        if std::env::var_os("LANGIS_MOCK").is_none() {
            return Self::start(config);
        }
        let _ = config;
        Self::mock()
    }

    fn spawn(run: impl FnOnce(Inbox, Events) + Send + 'static) -> Self {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = async_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("langis-backend".into())
            // presage/libsignal futures are huge and the runtime is current-thread, so they all
            // live on this stack; the 2 MiB default overflows in debug builds.
            .stack_size(64 * 1024 * 1024)
            .spawn(move || run(Inbox(command_rx), Events(event_tx)))
            .expect("failed to spawn backend thread");
        Self { commands: Commands(command_tx), events: event_rx, thread: Some(thread) }
    }

    pub fn send(&self, command: Command) {
        self.commands.send(command);
    }

    pub fn commands(&self) -> Commands {
        self.commands.clone()
    }

    /// Cloneable receiver: `.recv().await` it from an async event loop, or `.recv_blocking()`
    /// on a worker thread.
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.commands.0.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Current time in Signal's timestamp format.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
