//! UI-agnostic Signal backend.
//!
//! A [`Backend`] runs on its own thread. The UI sends [`Command`]s and receives [`Event`]s,
//! either by awaiting them in its event loop or by blocking on a worker thread; neither side
//! ever blocks the other. All types here are plain data so any toolkit can
//! render them.

mod mock;
mod qr;
pub mod state;
#[cfg(feature = "signal")]
mod signal;
#[cfg(feature = "signal")]
mod vault;

pub use qr::QrMatrix;

use crossbeam_channel::{Receiver, Sender};

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
    /// Initial message queue drained; it is now safe to send.
    Synced,
    /// New or changed conversations, merged by id.
    Conversations(Vec<Conversation>),
    History { conversation: ConversationId, messages: Vec<ChatMessage> },
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

/// Handle to a running backend. Dropping it shuts the backend down.
pub struct Backend {
    commands: Sender<Command>,
    events: async_channel::Receiver<Event>,
}

impl Backend {
    /// Starts the real Signal backend.
    #[cfg(feature = "signal")]
    pub fn start(config: Config) -> Self {
        Self::spawn(move |commands, events| signal::run(config, commands, events))
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

    fn spawn(run: impl FnOnce(Receiver<Command>, Events) + Send + 'static) -> Self {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (event_tx, event_rx) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("langis-backend".into())
            .spawn(move || run(command_rx, Events(event_tx)))
            .expect("failed to spawn backend thread");
        Self { commands: command_tx, events: event_rx }
    }

    pub fn send(&self, command: Command) {
        // The backend only goes away when it has failed fatally, which it reports as an event.
        let _ = self.commands.send(command);
    }

    /// Cloneable receiver: `.recv().await` it from an async event loop, or `.recv_blocking()`
    /// on a worker thread.
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }
}

/// Current time in Signal's timestamp format.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
